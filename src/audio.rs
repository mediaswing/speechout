//! Decoding speech audio, playing it, and saving it as WAV or MP3.

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::num::NonZero;
use std::path::Path;

/// Mono audio as floating-point samples.
#[derive(Clone, Debug, Default)]
pub struct Pcm {
    pub samples: Vec<f32>,
    pub rate: u32,
}

impl Pcm {
    pub fn duration_secs(&self) -> f32 {
        if self.rate == 0 { 0.0 } else { self.samples.len() as f32 / self.rate as f32 }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AudioFormat {
    #[default]
    Mp3,
    Wav,
}

impl AudioFormat {
    pub fn label(self) -> String {
        match self {
            AudioFormat::Mp3 => crate::i18n::t("format.mp3"),
            AudioFormat::Wav => crate::i18n::t("format.wav"),
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::Wav => "wav",
        }
    }
}

/// Decodes WAV or MP3 bytes into mono samples.
pub fn decode(bytes: Vec<u8>) -> anyhow::Result<Pcm> {
    use rodio::Source;
    let decoder = rodio::Decoder::new(Cursor::new(bytes)).context("the speech audio could not be decoded")?;
    let channels = usize::from(decoder.channels().get());
    let rate = decoder.sample_rate().get();
    let interleaved: Vec<f32> = decoder.collect();
    let samples = if channels <= 1 {
        interleaved
    } else {
        interleaved
            .chunks(channels)
            .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
            .collect()
    };
    Ok(Pcm { samples, rate })
}

/// Linear-interpolation resampling; good enough for speech.
fn resample(pcm: &Pcm, rate: u32) -> Vec<f32> {
    if pcm.rate == rate || pcm.samples.is_empty() {
        return pcm.samples.clone();
    }
    let ratio = f64::from(pcm.rate) / f64::from(rate);
    let out_len = (pcm.samples.len() as f64 / ratio) as usize;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            let idx = pos as usize;
            let frac = (pos - idx as f64) as f32;
            let a = pcm.samples[idx.min(pcm.samples.len() - 1)];
            let b = pcm.samples[(idx + 1).min(pcm.samples.len() - 1)];
            a + (b - a) * frac
        })
        .collect()
}

/// Joins pieces of speech into one recording at the first piece's sample rate,
/// with a short pause between pieces.
pub fn concat(pieces: &[Pcm]) -> Pcm {
    let rate = pieces.first().map(|p| p.rate).unwrap_or(22_050);
    let gap = vec![0.0; rate as usize / 5];
    let mut samples = Vec::new();
    for (i, piece) in pieces.iter().enumerate() {
        if i > 0 {
            samples.extend_from_slice(&gap);
        }
        samples.extend(resample(piece, rate));
    }
    Pcm { samples, rate }
}

fn to_i16(samples: &[f32]) -> Vec<i16> {
    samples.iter().map(|s| (s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16).collect()
}

/// What a saved recording says about itself, so music players and file
/// managers can show more than the file name. Written into MP3 files as an
/// ID3 tag; WAV files have no tag. Empty fields are left out.
#[derive(Clone, Debug, Default)]
pub struct Tags {
    /// Shown as the track's title: the saved file's name.
    pub title: String,
    /// Shown as the artist: the voice that read it.
    pub artist: String,
    /// Shown as the album: the document it was read from, so recordings of
    /// the same document are grouped together.
    pub album: String,
}

pub fn save(pcm: &Pcm, path: &Path, format: AudioFormat, tags: &Tags) -> anyhow::Result<()> {
    if pcm.samples.is_empty() {
        bail!("there is no audio to save");
    }
    let bytes = match format {
        AudioFormat::Wav => encode_wav(pcm)?,
        AudioFormat::Mp3 => [id3_tag(tags), encode_mp3(pcm)?].concat(),
    };
    std::fs::write(path, bytes).with_context(|| format!("could not write {}", path.display()))
}

fn encode_wav(pcm: &Pcm) -> anyhow::Result<Vec<u8>> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: pcm.rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut out = Cursor::new(Vec::new());
    let mut writer = hound::WavWriter::new(&mut out, spec)?;
    for s in to_i16(&pcm.samples) {
        writer.write_sample(s)?;
    }
    writer.finalize()?;
    Ok(out.into_inner())
}

fn encode_mp3(pcm: &Pcm) -> anyhow::Result<Vec<u8>> {
    use mp3lame_encoder::{Bitrate, Builder, FlushNoGap, MonoPcm, Quality};
    let lame_err = |e: &dyn std::fmt::Display| anyhow::anyhow!("MP3 encoder error: {e}");
    let mut builder = Builder::new().context("could not start the MP3 encoder")?;
    builder.set_num_channels(1).map_err(|e| lame_err(&e))?;
    builder.set_sample_rate(pcm.rate).map_err(|e| lame_err(&e))?;
    builder.set_brate(Bitrate::Kbps128).map_err(|e| lame_err(&e))?;
    builder.set_quality(Quality::Good).map_err(|e| lame_err(&e))?;
    let mut encoder = builder.build().map_err(|e| lame_err(&e))?;

    let samples = to_i16(&pcm.samples);
    let mut out = Vec::new();
    for block in samples.chunks(1152 * 64) {
        out.reserve(mp3lame_encoder::max_required_buffer_size(block.len()));
        encoder.encode_to_vec(MonoPcm(block), &mut out).map_err(|e| lame_err(&e))?;
    }
    out.reserve(7200);
    encoder.flush_to_vec::<FlushNoGap>(&mut out).map_err(|e| lame_err(&e))?;
    // LAME starts the audio with an empty frame, to be filled in once the
    // whole recording is encoded with its Info header: how long it is, and
    // the silence the encoder added at each end. Without it, players have to
    // guess the length from the file size.
    let mut info = Vec::with_capacity(encoder.lame_tag_size());
    if encoder.lame_tag_encode_to_vec(&mut info).is_some() && info.len() <= out.len() {
        out[..info.len()].copy_from_slice(&info);
    }
    Ok(out)
}

/// An ID3v2.3 tag, to go at the start of an MP3 file, naming `tags` and the
/// app that made it. Version 2.3 with UTF-16 text is what Windows File
/// Explorer, Apple Music and other players all read; LAME's own tag writer
/// would only take Latin-1, losing names such as "Café" or "日本語".
fn id3_tag(tags: &Tags) -> Vec<u8> {
    // The Audio Player tab looks for this to tell the app's own speech,
    // which has no need to be transcribed; see `made_by_this_app`.
    let software = format!("{} {}", crate::APP_TITLE, env!("CARGO_PKG_VERSION"));
    let fields = [(b"TIT2", &tags.title), (b"TPE1", &tags.artist), (b"TALB", &tags.album), (b"TSSE", &software)];
    let mut frames = Vec::new();
    for (id, text) in fields {
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        let text = text.trim();
        if text.is_empty() {
            continue;
        }
        // Encoding 1 is UTF-16 with a byte order mark.
        let mut body = vec![1, 0xFF, 0xFE];
        body.extend(text.encode_utf16().flat_map(u16::to_le_bytes));
        frames.extend_from_slice(id);
        frames.extend_from_slice(&(body.len() as u32).to_be_bytes());
        frames.extend_from_slice(&[0, 0]);
        frames.extend(body);
    }
    // The tag's size is written seven bits to a byte, as ID3 requires.
    let size = frames.len() as u32;
    let mut tag = b"ID3\x03\x00\x00".to_vec();
    tag.extend([21, 14, 7, 0].map(|shift| (size >> shift & 0x7F) as u8));
    tag.extend(frames);
    tag
}

/// Plays decoded speech through the default output device.
pub struct Playback {
    _device: rodio::MixerDeviceSink,
    player: std::sync::Arc<rodio::Player>,
}

impl Playback {
    pub fn open() -> anyhow::Result<Self> {
        let mut device = rodio::DeviceSinkBuilder::open_default_sink()
            .context("no audio output device is available")?;
        device.log_on_drop(false);
        let player = std::sync::Arc::new(rodio::Player::connect_new(device.mixer()));
        Ok(Self { _device: device, player })
    }

    /// A handle that other threads can use to pause or stop playback.
    pub fn handle(&self) -> std::sync::Arc<rodio::Player> {
        self.player.clone()
    }

    pub fn append(&self, pcm: Pcm) {
        let Some(rate) = NonZero::new(pcm.rate) else { return };
        self.player
            .append(rodio::buffer::SamplesBuffer::new(NonZero::<u16>::MIN, rate, pcm.samples));
    }

    /// How far into the piece now playing.
    pub fn position(&self) -> std::time::Duration {
        self.player.get_pos()
    }

    /// Number of pieces queued or playing.
    pub fn queued(&self) -> usize {
        self.player.len()
    }

}

// ----- success and failure sounds -----------------------------------------

/// A short sound marking how something went.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cue {
    Success,
    Failure,
}

impl Cue {
    fn bytes(self) -> &'static [u8] {
        match self {
            Cue::Success => include_bytes!("../assets/sounds/success.mp3"),
            Cue::Failure => include_bytes!("../assets/sounds/failure.wav"),
        }
    }
}

/// Plays cues, one at a time, each on its own thread so the window never
/// waits for the audio device.
#[derive(Default)]
pub struct Cues {
    /// How many cues have been started or stopped, and the one now playing.
    /// A cue whose number is out of date by the time its audio device opens
    /// is not played, so `stop` works even before then.
    current: std::sync::Arc<std::sync::Mutex<(u64, Option<std::sync::Arc<rodio::Player>>)>>,
}

impl Cues {
    pub fn play(&self, cue: Cue) {
        let Some(number) = self.stop() else { return };
        let current = self.current.clone();
        std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                let mut device = rodio::DeviceSinkBuilder::open_default_sink()
                    .context("no audio output device is available")?;
                device.log_on_drop(false);
                let player = std::sync::Arc::new(rodio::Player::connect_new(device.mixer()));
                let decoder = rodio::Decoder::try_from(Cursor::new(cue.bytes()))?;
                {
                    let Ok(mut slot) = current.lock() else { return Ok(()) };
                    if slot.0 != number {
                        return Ok(());
                    }
                    slot.1 = Some(player.clone());
                    player.append(decoder);
                }
                player.sleep_until_end();
                if let Ok(mut slot) = current.lock()
                    && slot.0 == number
                {
                    slot.1 = None;
                }
                Ok(())
            })();
            if let Err(e) = result {
                log::warn!("could not play the {cue:?} sound: {e:#}");
            }
        });
    }

    /// Cuts off the cue now playing or about to play, so it doesn't play
    /// over speech. Returns the number the next cue should have.
    pub fn stop(&self) -> Option<u64> {
        let mut slot = self.current.lock().ok()?;
        slot.0 += 1;
        if let Some(player) = slot.1.take() {
            player.stop();
        }
        Some(slot.0)
    }
}

// ----- audio files on the Audio Player tab ---------------------------------

/// The sample rate audio files are reduced to for the waveform, the speech
/// check and transcribing, which is the rate Whisper expects.
pub const LISTEN_RATE: u32 = 16_000;

/// Only this much of a file is kept for transcribing: three hours at 16 kHz
/// is about 700 MB of memory. Longer files still play in full.
const MAX_LISTEN_SECS: u64 = 3 * 60 * 60;

/// How many bars the waveform is drawn with.
const WAVEFORM_BARS: usize = 600;

/// An audio file chosen on the Audio Player tab.
pub struct Listened {
    /// The whole file, played or not.
    pub duration: std::time::Duration,
    /// Mono audio at `LISTEN_RATE`, for transcribing.
    pub pcm: Pcm,
    /// The loudest sample in each slice of the file, from 0.0 to 1.0.
    pub waveform: Vec<f32>,
    pub sound: SoundKind,
    /// The file is longer than `MAX_LISTEN_SECS`, so only the start of it
    /// can be transcribed.
    pub cut_short: bool,
    /// This app saved the file, reading text aloud, so the words are known
    /// already and transcribing it would only give them back.
    pub made_here: bool,
}

/// What the waveform suggests a recording is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SoundKind {
    /// Mostly spoken word.
    Speech,
    /// Mostly something else, such as music.
    Other,
    /// Silent, or nearly.
    Silent,
}

/// Decodes a WAV or MP3 file to mono at `LISTEN_RATE`, and works out its
/// waveform and whether it sounds like speech. Returns `None` if `stopped`
/// said to stop, which is checked for each second of audio.
pub fn listen(path: &Path, stopped: impl Fn() -> bool) -> anyhow::Result<Option<Listened>> {
    use rodio::Source;
    let file = std::fs::File::open(path).with_context(|| format!("could not open {}", path.display()))?;
    let decoder = rodio::Decoder::try_from(file).context("the audio could not be decoded")?;
    let channels = usize::from(decoder.channels().get());
    let rate = decoder.sample_rate().get();
    let keep = MAX_LISTEN_SECS as usize * rate as usize;
    // Room for the whole file at once where its length is known, rather than
    // growing in steps, which briefly needs half as much again.
    let expected = decoder.total_duration().map_or(0.0, |d| d.as_secs_f64().min(MAX_LISTEN_SECS as f64));
    let mut resampler = Resampler::new(rate, LISTEN_RATE, (expected * f64::from(LISTEN_RATE)) as usize + 1);
    let mut frames = 0usize;
    let mut frame = Vec::with_capacity(channels);
    for sample in decoder {
        frame.push(sample);
        if frame.len() < channels {
            continue;
        }
        if frames < keep {
            resampler.push(frame.iter().sum::<f32>() / channels as f32);
        }
        frames += 1;
        frame.clear();
        if frames.is_multiple_of(rate as usize) && stopped() {
            return Ok(None);
        }
    }
    if frames == 0 {
        bail!("the file has no audio in it");
    }
    let pcm = Pcm { samples: resampler.finish(), rate: LISTEN_RATE };
    Ok(Some(Listened {
        duration: std::time::Duration::from_secs_f64(frames as f64 / f64::from(rate)),
        waveform: waveform(&pcm.samples, WAVEFORM_BARS),
        sound: sound_kind(&pcm),
        cut_short: frames > keep,
        made_here: made_by_this_app(path).unwrap_or_else(|e| {
            log::warn!("could not read the audio file's tag: {e:#}");
            false
        }),
        pcm,
    }))
}

/// Whether the file's ID3 tag names this app as the software that made it,
/// as `id3_tag` writes. Other tag editors may have rewritten the tag since,
/// so versions 2.3 and 2.4 and any text encoding are read. Frames other than
/// the software one are skipped over unread, so cover art costs nothing.
pub fn made_by_this_app(path: &Path) -> anyhow::Result<bool> {
    use std::io::{BufReader, Read};
    let mut file = BufReader::new(std::fs::File::open(path)?);
    let mut header = [0u8; 10];
    if file.read_exact(&mut header).is_err() || &header[..3] != b"ID3" {
        return Ok(false);
    }
    let version = header[3];
    let flags = header[5];
    // Version 2.2's frames are different, and an unsynchronised tag would
    // have to be undone byte by byte: neither is written by this app, nor
    // by anything likely to keep its software frame.
    if !(3..=4).contains(&version) || flags & 0x80 != 0 {
        return Ok(false);
    }
    let synchsafe = |b: [u8; 4]| b.iter().fold(0u32, |n, b| n << 7 | u32::from(b & 0x7F));
    let mut left = i64::from(synchsafe(header[6..10].try_into()?));
    if flags & 0x40 != 0 {
        // An extended header, whose size counts itself in 2.4 but not 2.3.
        let mut size = [0u8; 4];
        file.read_exact(&mut size)?;
        let skip = if version == 4 { i64::from(synchsafe(size)) - 4 } else { i64::from(u32::from_be_bytes(size)) };
        file.seek_relative(skip)?;
        left -= skip + 4;
    }
    while left >= 10 {
        let mut frame = [0u8; 10];
        file.read_exact(&mut frame)?;
        // The rest of the tag is padding.
        if frame[0] == 0 {
            break;
        }
        let size_bytes = frame[4..8].try_into()?;
        let size = if version == 4 { synchsafe(size_bytes) } else { u32::from_be_bytes(size_bytes) };
        left -= 10 + i64::from(size);
        if &frame[..4] != b"TSSE" {
            file.seek_relative(i64::from(size))?;
            continue;
        }
        if size > 1024 {
            return Ok(false);
        }
        let mut body = vec![0u8; size as usize];
        file.read_exact(&mut body)?;
        let software = id3_text(&body);
        let software = software.trim_end_matches('\0').trim();
        return Ok(software == crate::APP_TITLE || software.starts_with(&format!("{} ", crate::APP_TITLE)));
    }
    Ok(false)
}

/// The text of an ID3 text frame: an encoding byte, then the text.
fn id3_text(body: &[u8]) -> String {
    let Some((&encoding, text)) = body.split_first() else { return String::new() };
    let utf16 = |text: &[u8], big_endian: bool| {
        let units = text.as_chunks::<2>().0.iter();
        let units = units.map(|&c| if big_endian { u16::from_be_bytes(c) } else { u16::from_le_bytes(c) });
        char::decode_utf16(units).map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER)).collect()
    };
    match encoding {
        0 => text.iter().map(|&b| char::from(b)).collect(),
        1 => match text {
            [0xFF, 0xFE, rest @ ..] => utf16(rest, false),
            [0xFE, 0xFF, rest @ ..] => utf16(rest, true),
            _ => utf16(text, false),
        },
        2 => utf16(text, true),
        _ => String::from_utf8_lossy(text).into_owned(),
    }
}

/// Changes the sample rate of a stream of samples, one at a time, so a long
/// file never has to be held at its original rate. Going down, each output
/// sample is the average of the input samples it covers, which keeps out the
/// worst of the aliasing; going up, samples are joined with straight lines.
struct Resampler {
    /// Input samples per output sample.
    step: f64,
    out: Vec<f32>,
    /// Going down: the input samples so far for the next output sample.
    sum: f32,
    count: u32,
    /// How many input samples have been pushed.
    pushed: u64,
    previous: Option<f32>,
}

impl Resampler {
    /// `capacity` is how many output samples to make room for.
    fn new(from: u32, to: u32, capacity: usize) -> Self {
        let out = Vec::with_capacity(capacity);
        Self { step: f64::from(from) / f64::from(to), out, sum: 0.0, count: 0, pushed: 0, previous: None }
    }

    fn push(&mut self, sample: f32) {
        let index = self.pushed;
        self.pushed += 1;
        if self.step >= 1.0 {
            self.sum += sample;
            self.count += 1;
            // The output sample is complete once the next input sample
            // belongs to the one after it.
            let next_out = ((self.out.len() + 1) as f64 * self.step).ceil() as u64;
            if self.pushed >= next_out {
                self.out.push(self.sum / self.count as f32);
                self.sum = 0.0;
                self.count = 0;
            }
        } else {
            // Every output sample that falls between the previous input
            // sample and this one.
            let Some(previous) = self.previous.replace(sample) else {
                self.out.push(sample);
                return;
            };
            loop {
                let pos = self.out.len() as f64 * self.step;
                if pos > index as f64 {
                    break;
                }
                let frac = (pos - (index - 1) as f64) as f32;
                self.out.push(previous + (sample - previous) * frac);
            }
        }
    }

    fn finish(mut self) -> Vec<f32> {
        if self.count > 0 {
            self.out.push(self.sum / self.count as f32);
        }
        self.out
    }
}

/// The loudest sample in each of `bars` equal slices of `samples`.
fn waveform(samples: &[f32], bars: usize) -> Vec<f32> {
    if samples.is_empty() {
        return Vec::new();
    }
    let bars = bars.min(samples.len());
    (0..bars)
        .map(|i| {
            let slice = &samples[i * samples.len() / bars..(i + 1) * samples.len() / bars];
            slice.iter().fold(0.0f32, |peak, s| peak.max(s.abs())).min(1.0)
        })
        .collect()
}

/// Guesses from the waveform whether a recording is mostly spoken word.
///
/// The recording is cut into frames of 20 ms, and the frames into windows of
/// one second. Speech has two habits that music and most other sounds don't:
///
/// * Its loudness rises and falls with each syllable, with short gaps between
///   words, so a good share of the frames in each second are much quieter than
///   that second's average.
/// * Voiced sounds (vowels) alternate with hissy ones (s, f, sh), so a few
///   frames in each second cross zero far more often than the rest.
///
/// A window that shows both counts as speech. The recording is speech if most
/// of its windows that aren't silent are.
pub fn sound_kind(pcm: &Pcm) -> SoundKind {
    /// Share of windows with sound that must look like speech.
    const SPEECH_WINDOWS: f32 = 0.6;
    let (speech, sound) = speech_seconds(pcm);
    log::info!("speech check: {speech} of {sound} second(s) with sound look like speech");
    if sound == 0 {
        SoundKind::Silent
    } else if speech as f32 >= SPEECH_WINDOWS * sound as f32 {
        SoundKind::Speech
    } else {
        SoundKind::Other
    }
}

/// How many seconds of `pcm` look like speech, and how many aren't silent.
fn speech_seconds(pcm: &Pcm) -> (usize, usize) {
    /// Below this average level (about -50 dBFS), a second counts as silent.
    const SILENT_RMS: f32 = 0.003;
    /// Share of frames under half the window's average loudness.
    const QUIET_FRAMES: f32 = 0.15;
    /// Share of frames that cross zero at least one and a half times as
    /// often as the window's average.
    const HISSY_FRAMES: f32 = 0.05;
    /// A window shorter than this many frames (a quarter of a second) is too
    /// short to tell from. The part-second at the end of a recording is left
    /// out if it is this short, unless it is all there is.
    const MIN_FRAMES: usize = 12;

    let frame_len = (pcm.rate as usize / 50).max(1);
    let frames: Vec<(f32, f32)> = pcm
        .samples
        .chunks_exact(frame_len)
        .map(|f| {
            let rms = (f.iter().map(|s| s * s).sum::<f32>() / f.len() as f32).sqrt();
            let crossings = f.windows(2).filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0)).count();
            (rms, crossings as f32 / f.len() as f32)
        })
        .collect();

    let (mut sound, mut speech) = (0, 0);
    let windows = frames.chunks(50).filter(|w| w.len() >= MIN_FRAMES || frames.len() < MIN_FRAMES);
    for window in windows {
        let n = window.len() as f32;
        let mean_rms = window.iter().map(|f| f.0).sum::<f32>() / n;
        if mean_rms < SILENT_RMS {
            continue;
        }
        sound += 1;
        let mean_zcr = window.iter().map(|f| f.1).sum::<f32>() / n;
        let quiet = window.iter().filter(|f| f.0 < 0.5 * mean_rms).count() as f32 / n;
        let hissy = window.iter().filter(|f| f.1 > 1.5 * mean_zcr).count() as f32 / n;
        if window.len() >= MIN_FRAMES && quiet >= QUIET_FRAMES && hissy >= HISSY_FRAMES {
            speech += 1;
        }
    }
    (speech, sound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cues_decode() {
        for cue in [Cue::Success, Cue::Failure] {
            let decoder = rodio::Decoder::try_from(Cursor::new(cue.bytes())).unwrap();
            assert!(decoder.count() > 0, "{cue:?} has no audio");
        }
    }

    fn tone() -> Pcm {
        Pcm { samples: (0..22_050).map(|i| (i as f32 * 0.05).sin() * 0.5).collect(), rate: 22_050 }
    }

    #[test]
    fn wav_round_trip() {
        let bytes = encode_wav(&tone()).unwrap();
        let back = decode(bytes).unwrap();
        assert_eq!(back.rate, 22_050);
        assert_eq!(back.samples.len(), 22_050);
    }

    #[test]
    fn mp3_encodes_and_decodes() {
        let bytes = encode_mp3(&tone()).unwrap();
        let back = decode(bytes).unwrap();
        assert!(back.samples.len() > 20_000);
    }

    #[test]
    fn mp3_has_an_info_header_with_its_length() {
        let pcm = tone();
        let bytes = encode_mp3(&pcm).unwrap();
        // A constant bit rate file's header says "Info"; it sits in the first
        // frame, after the frame header and side information.
        let at = bytes[..200].windows(4).position(|w| w == b"Info").expect("an Info header");
        let flags = u32::from_be_bytes(bytes[at + 4..at + 8].try_into().unwrap());
        assert_ne!(flags & 1, 0, "the header gives the number of frames");
        let frames = u32::from_be_bytes(bytes[at + 8..at + 12].try_into().unwrap()) as usize;
        // MPEG-2 layer III frames hold 576 samples each.
        let expected = pcm.samples.len().div_ceil(576);
        assert!(frames.abs_diff(expected) <= 3, "{frames} frames for {expected} frames of audio");
        assert!(bytes.windows(4).any(|w| w == b"LAME"));
    }

    /// The text frames of an ID3v2.3 tag, as (frame ID, text).
    fn read_id3(bytes: &[u8]) -> Vec<(String, String)> {
        assert_eq!(&bytes[..6], b"ID3\x03\x00\x00");
        let size = bytes[6..10].iter().fold(0, |n, b| n << 7 | usize::from(*b));
        let mut frames = &bytes[10..10 + size];
        let mut out = Vec::new();
        while !frames.is_empty() {
            let id = String::from_utf8(frames[..4].to_vec()).unwrap();
            let len = u32::from_be_bytes(frames[4..8].try_into().unwrap()) as usize;
            let body = &frames[10..10 + len];
            assert_eq!(&body[..3], &[1, 0xFF, 0xFE], "{id} is UTF-16 with a byte order mark");
            let units: Vec<u16> = body[3..].chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            out.push((id, String::from_utf16(&units).unwrap()));
            frames = &frames[10 + len..];
        }
        out
    }

    #[test]
    fn saved_mp3_has_id3_tags() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("speech.mp3");
        let tags = Tags {
            title: "Chapter 1: Café".into(),
            artist: "Draco (British English, masculine)".into(),
            album: "日本語のレポート".into(),
        };
        save(&tone(), &path, AudioFormat::Mp3, &tags).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let software = format!("Speech Output Engine {}", env!("CARGO_PKG_VERSION"));
        assert_eq!(
            read_id3(&bytes),
            [
                ("TIT2".into(), "Chapter 1: Café".into()),
                ("TPE1".into(), "Draco (British English, masculine)".into()),
                ("TALB".into(), "日本語のレポート".into()),
                ("TSSE".into(), software),
            ]
        );
        // The tag doesn't get in the way of playing it.
        assert!(decode(bytes).unwrap().samples.len() > 20_000);
    }

    #[test]
    fn empty_tags_are_left_out() {
        let tags = Tags { title: " \n ".into(), artist: String::new(), album: "Notes\u{0}".into() };
        let frames = read_id3(&id3_tag(&tags));
        let ids: Vec<&str> = frames.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(ids, ["TALB", "TSSE"]);
        assert_eq!(frames[0].1, "Notes");
    }

    #[test]
    fn knows_its_own_speech() {
        let dir = tempfile::tempdir().unwrap();
        let ours = dir.path().join("ours.mp3");
        save(&tone(), &ours, AudioFormat::Mp3, &Tags::default()).unwrap();
        assert!(made_by_this_app(&ours).unwrap());

        let untagged = dir.path().join("untagged.mp3");
        std::fs::write(&untagged, encode_mp3(&tone()).unwrap()).unwrap();
        assert!(!made_by_this_app(&untagged).unwrap());

        let wav = dir.path().join("speech.wav");
        save(&tone(), &wav, AudioFormat::Wav, &Tags::default()).unwrap();
        assert!(!made_by_this_app(&wav).unwrap());

        // Retagged by another editor as version 2.4 in UTF-8, with a large
        // frame first, as cover art would be.
        let synchsafe = |n: u32| [21, 14, 7, 0].map(|shift| (n >> shift & 0x7F) as u8);
        let frame = |id: &[u8], body: &[u8]| [id, &synchsafe(body.len() as u32), &[0, 0], body].concat();
        let retag = |software: &str| {
            let frames = [
                frame(b"APIC", &vec![7; 300_000]),
                frame(b"TSSE", &[&[3], format!("{software}\0").as_bytes()].concat()),
            ]
            .concat();
            [b"ID3\x04\x00\x00".as_slice(), &synchsafe(frames.len() as u32), &frames, &encode_mp3(&tone()).unwrap()]
                .concat()
        };
        let retagged = dir.path().join("retagged.mp3");
        std::fs::write(&retagged, retag("Speech Output Engine 3.4.0")).unwrap();
        assert!(made_by_this_app(&retagged).unwrap());
        std::fs::write(&retagged, retag("Lavf60.3.100")).unwrap();
        assert!(!made_by_this_app(&retagged).unwrap());
        std::fs::write(&retagged, retag("Speech Output Engineering Tools")).unwrap();
        assert!(!made_by_this_app(&retagged).unwrap());
    }

    #[test]
    fn wav_has_no_id3_tag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("speech.wav");
        let tags = Tags { title: "Title".into(), ..Tags::default() };
        save(&tone(), &path, AudioFormat::Wav, &tags).unwrap();
        assert_eq!(&std::fs::read(&path).unwrap()[..4], b"RIFF");
    }

    fn resampled(from: u32, to: u32, samples: &[f32]) -> Vec<f32> {
        let mut resampler = Resampler::new(from, to, 0);
        samples.iter().for_each(|s| resampler.push(*s));
        resampler.finish()
    }

    #[test]
    fn resampling_keeps_the_length_and_level() {
        let one_second = vec![0.25; 44_100];
        let down = resampled(44_100, LISTEN_RATE, &one_second);
        assert!((down.len() as i64 - 16_000).abs() <= 1, "{}", down.len());
        assert!(down.iter().all(|s| (s - 0.25).abs() < 1e-6));

        let ramp: Vec<f32> = (0..8_000).map(|i| i as f32).collect();
        let up = resampled(8_000, LISTEN_RATE, &ramp);
        assert!((up.len() as i64 - 16_000).abs() <= 2, "{}", up.len());
        assert_eq!(&up[..5], &[0.0, 0.5, 1.0, 1.5, 2.0]);

        assert_eq!(resampled(LISTEN_RATE, LISTEN_RATE, &ramp[..10]), &ramp[..10]);
    }

    #[test]
    fn waveform_has_the_peak_of_each_slice() {
        let samples = [0.1, -0.5, 0.2, 0.3, -0.05, 0.0];
        assert_eq!(waveform(&samples, 3), vec![0.5, 0.3, 0.05]);
        assert_eq!(waveform(&samples, 100).len(), 6);
        assert!(waveform(&[], 10).is_empty());
    }

    /// A made-up voice: syllables of a buzzing vowel, each followed by a
    /// hiss, with short gaps between them, about four syllables a second.
    fn speech_like(secs: usize) -> Pcm {
        let rate = LISTEN_RATE as usize;
        let mut noise = 12_345u32;
        let samples = (0..secs * rate)
            .map(|i| {
                let t = i as f32 / rate as f32;
                let in_syllable = (i % (rate / 4)) as f32 / (rate / 4) as f32;
                noise = noise.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                let hiss = (noise >> 16) as f32 / 32_768.0 - 1.0;
                if in_syllable < 0.55 {
                    let envelope = (in_syllable / 0.55 * std::f32::consts::PI).sin();
                    let tone = |hz: f32| (t * hz * std::f32::consts::TAU).sin();
                    envelope * 0.4 * (tone(130.0) + 0.5 * tone(700.0))
                } else if in_syllable < 0.75 {
                    0.08 * hiss
                } else {
                    0.0
                }
            })
            .collect();
        Pcm { samples, rate: LISTEN_RATE }
    }

    /// A made-up tune: a steady chord that changes every half second.
    fn music_like(secs: usize) -> Pcm {
        let rate = LISTEN_RATE as f32;
        let chords = [[261.6, 329.6, 392.0], [293.7, 370.0, 440.0], [329.6, 415.3, 493.9]];
        let samples = (0..secs * LISTEN_RATE as usize)
            .map(|i| {
                let t = i as f32 / rate;
                let chord = chords[(t * 2.0) as usize % chords.len()];
                chord.iter().map(|f| (t * f * std::f32::consts::TAU).sin()).sum::<f32>() * 0.2
            })
            .collect();
        Pcm { samples, rate: LISTEN_RATE }
    }

    #[test]
    fn tells_speech_from_music_and_silence() {
        assert_eq!(sound_kind(&speech_like(10)), SoundKind::Speech);
        assert_eq!(sound_kind(&music_like(10)), SoundKind::Other);
        assert_eq!(sound_kind(&Pcm { samples: vec![0.0; 160_000], rate: LISTEN_RATE }), SoundKind::Silent);
        assert_eq!(sound_kind(&Pcm { samples: vec![], rate: LISTEN_RATE }), SoundKind::Silent);
        // A short voice note, and a click too short to tell what it is.
        let clip = |secs: f32, pcm: Pcm| Pcm { samples: pcm.samples[..(secs * 16_000.0) as usize].to_vec(), ..pcm };
        assert_eq!(sound_kind(&clip(0.75, speech_like(1))), SoundKind::Speech);
        assert_eq!(sound_kind(&clip(1.1, speech_like(2))), SoundKind::Speech);
        assert_eq!(sound_kind(&clip(0.1, speech_like(1))), SoundKind::Other);
    }

    #[test]
    fn listens_to_a_file_and_can_seek_in_it() {
        use rodio::Source;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("talk.mp3");
        let mut speech = speech_like(12);
        speech.samples.iter_mut().for_each(|s| *s *= 0.8);
        std::fs::write(&path, encode_mp3(&Pcm { samples: resample(&speech, 22_050), rate: 22_050 }).unwrap()).unwrap();

        let listened = listen(&path, || false).unwrap().unwrap();
        assert!((listened.duration.as_secs_f32() - 12.0).abs() < 0.2, "{:?}", listened.duration);
        assert_eq!(listened.pcm.rate, LISTEN_RATE);
        assert_eq!(listened.waveform.len(), WAVEFORM_BARS);
        assert_eq!(listened.sound, SoundKind::Speech);
        assert!(!listened.cut_short);
        assert!(listened.pcm.samples.capacity() < listened.pcm.samples.len() * 11 / 10);
        assert!(listen(&path, || true).unwrap().is_none());

        // The Back and Forward buttons rely on seeking in the file.
        let mut decoder = rodio::Decoder::try_from(std::fs::File::open(&path).unwrap()).unwrap();
        decoder.try_seek(std::time::Duration::from_secs(8)).unwrap();
        let left = decoder.count() as f32 / 22_050.0;
        assert!((left - 4.0).abs() < 0.3, "{left} seconds left after seeking");
    }

    #[test]
    fn concat_resamples() {
        let a = tone();
        let b = Pcm { samples: vec![0.0; 44_100], rate: 44_100 };
        let joined = concat(&[a, b]);
        assert_eq!(joined.rate, 22_050);
        assert!((joined.samples.len() as i64 - (22_050 + 4_410 + 22_050)).abs() < 3);
    }
}
