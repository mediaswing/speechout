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

pub fn save(pcm: &Pcm, path: &Path, format: AudioFormat) -> anyhow::Result<()> {
    if pcm.samples.is_empty() {
        bail!("there is no audio to save");
    }
    let bytes = match format {
        AudioFormat::Wav => encode_wav(pcm)?,
        AudioFormat::Mp3 => encode_mp3(pcm)?,
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
    Ok(out)
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
        pcm,
    }))
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
