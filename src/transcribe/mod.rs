//! Listening to audio files: deciding from the waveform whether they are mostly
//! speech, and if so, writing down what was said, with each voice named
//! "Speaker 1", "Speaker 2" and so on.
//!
//! Everything happens on this computer. whisper.cpp does the transcribing, and
//! its model is the only thing downloaded, once, after the user agrees.

pub mod detect;
pub mod speakers;
pub mod whisper;

use std::io::{BufWriter, Write};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::{Context, bail};

pub use detect::Verdict;

use crate::worker::Control;

/// whisper.cpp hears audio at 16 kHz, so the analysis does too.
pub const RATE: u32 = 16_000;
/// How much of a recording is analysed and transcribed. Two hours of 16 kHz
/// audio is about 460 MB in memory.
pub const MAX_SECONDS: usize = 2 * 60 * 60;

/// An audio file, heard.
#[derive(Debug)]
pub struct Heard {
    /// 16 kHz mono.
    pub samples: Vec<f32>,
    pub verdict: Verdict,
    /// Only the first [`MAX_SECONDS`] were kept.
    pub shortened: bool,
}

impl Heard {
    pub fn seconds(&self) -> f32 {
        self.samples.len() as f32 / RATE as f32
    }
}

/// Decodes an audio file and judges whether it is speech.
pub fn listen(path: &Path) -> anyhow::Result<Heard> {
    let file = std::fs::File::open(path).context("the file could not be opened")?;
    let decoder = rodio::Decoder::try_from(file).context("this isn't audio that can be played, or it is damaged")?;
    let (samples, shortened) = decode_16k_mono(decoder);
    if samples.is_empty() {
        bail!("no sound could be read from it");
    }
    let verdict = detect::judge(&samples);
    Ok(Heard { samples, verdict, shortened })
}

/// Mixes down to mono and resamples to 16 kHz as it decodes, so a long file
/// never has to be in memory at its own rate.
fn decode_16k_mono<S: rodio::Source>(source: S) -> (Vec<f32>, bool) {
    let channels = usize::from(source.channels().get());
    let mut down = Downsampler::new(source.sample_rate().get());
    let limit = MAX_SECONDS * RATE as usize;
    let mut out = Vec::new();
    let mut frame = Vec::with_capacity(channels);
    for sample in source {
        frame.push(sample);
        if frame.len() == channels {
            down.push(frame.iter().sum::<f32>() / channels as f32, &mut out);
            frame.clear();
            if out.len() >= limit {
                out.truncate(limit);
                return (out, true);
            }
        }
    }
    (out, false)
}

/// Decoded speech, as from a voice, at 16 kHz. Only the macOS voice test needs it.
#[cfg(all(test, target_os = "macos"))]
pub fn to_16k_mono(pcm: crate::audio::Pcm) -> Vec<f32> {
    use std::num::NonZero;
    let rate = NonZero::new(pcm.rate).expect("a sample rate");
    decode_16k_mono(rodio::buffer::SamplesBuffer::new(NonZero::<u16>::MIN, rate, pcm.samples)).0
}

/// Turns a stream of samples at any rate into 16 kHz. Two passes of a moving
/// average first take out most of what is too high to keep, so it doesn't fold
/// back down as noise, then the output is interpolated. Plenty for speech.
struct Downsampler {
    /// Input samples per output sample.
    step: f64,
    /// Where the next output sample falls, in input samples.
    next: f64,
    seen: u64,
    previous: f32,
    smoothing: [MovingAverage; 2],
}

impl Downsampler {
    fn new(rate: u32) -> Self {
        let step = f64::from(rate.max(1)) / f64::from(RATE);
        let width = step.round().max(1.0) as usize;
        Self { step, next: 0.0, seen: 0, previous: 0.0, smoothing: [MovingAverage::new(width), MovingAverage::new(width)] }
    }

    fn push(&mut self, sample: f32, out: &mut Vec<f32>) {
        let y = self.smoothing.iter_mut().fold(sample, |s, m| m.push(s));
        let here = self.seen as f64;
        while self.next <= here {
            let back = here - self.next;
            out.push(if self.seen == 0 { y } else { y + (self.previous - y) * back as f32 });
            self.next += self.step;
        }
        self.previous = y;
        self.seen += 1;
    }
}

struct MovingAverage {
    values: Vec<f32>,
    at: usize,
    sum: f32,
}

impl MovingAverage {
    fn new(width: usize) -> Self {
        Self { values: vec![0.0; width], at: 0, sum: 0.0 }
    }

    fn push(&mut self, sample: f32) -> f32 {
        if self.values.len() == 1 {
            return sample;
        }
        self.sum += sample - self.values[self.at];
        self.values[self.at] = sample;
        self.at = (self.at + 1) % self.values.len();
        self.sum / self.values.len() as f32
    }
}

/// Writes 16 kHz mono 16-bit WAV, which every build of whisper.cpp can read.
fn write_wav(samples: &[f32], path: &Path) -> anyhow::Result<()> {
    let data_bytes = u32::try_from(samples.len() * 2).context("the audio is too long")?;
    let mut w = BufWriter::new(std::fs::File::create(path).context("could not create a temporary audio file")?);
    w.write_all(b"RIFF")?;
    w.write_all(&(36 + data_bytes).to_le_bytes())?;
    w.write_all(b"WAVEfmt ")?;
    w.write_all(&16u32.to_le_bytes())?;
    w.write_all(&1u16.to_le_bytes())?; // PCM
    w.write_all(&1u16.to_le_bytes())?; // mono
    w.write_all(&RATE.to_le_bytes())?;
    w.write_all(&(RATE * 2).to_le_bytes())?;
    w.write_all(&2u16.to_le_bytes())?;
    w.write_all(&16u16.to_le_bytes())?;
    w.write_all(b"data")?;
    w.write_all(&data_bytes.to_le_bytes())?;
    for s in samples {
        w.write_all(&((s.clamp(-1.0, 1.0) * f32::from(i16::MAX)) as i16).to_le_bytes())?;
    }
    w.flush().context("could not write a temporary audio file")?;
    Ok(())
}

/// Writes down what was said. Returns `None` if stopped.
pub fn transcribe(
    program: &Path,
    model: &Path,
    samples: &[f32],
    control: &Control,
    progress: &dyn Fn(f32),
) -> anyhow::Result<Option<String>> {
    let dir = tempfile::tempdir().context("could not create a temporary folder")?;
    let wav = dir.path().join("audio.wav");
    write_wav(samples, &wav)?;
    let Some(segments) = whisper::run(program, model, &wav, control, progress)? else {
        return Ok(None);
    };
    if segments.is_empty() {
        bail!("whisper.cpp listened to the whole thing and didn't hear a single word");
    }
    let at = |ms: u64| (ms as usize).saturating_mul(RATE as usize / 1000);
    let stretches: Vec<_> = segments.iter().map(|s| at(s.from_ms)..at(s.to_ms)).collect();
    let mut speakers = speakers::label(samples, &stretches);
    continue_sentences(&segments, &mut speakers);
    Ok(Some(layout(&segments, &speakers)))
}

/// whisper.cpp sometimes splits a sentence between stretches. Someone else
/// rarely finishes another's sentence, so a stretch that carries on from an
/// unfinished one is given to the same speaker.
fn continue_sentences(segments: &[whisper::Segment], speakers: &mut [usize]) {
    for i in 1..segments.len().min(speakers.len()) {
        let finished = segments[i - 1].text.trim_end().ends_with(['.', '?', '!', '"', '\u{201d}', ')', ']']);
        let carries_on = segments[i].text.chars().next().is_some_and(|c| c.is_lowercase());
        if !finished && carries_on {
            speakers[i] = speakers[i - 1];
        }
    }
    // Numbering by first appearance may have changed.
    let mut order: Vec<usize> = Vec::new();
    for s in speakers.iter_mut() {
        *s = match order.iter().position(|o| o == s) {
            Some(n) => n,
            None => {
                order.push(*s);
                order.len() - 1
            }
        };
    }
}

/// One paragraph each time the speaker changes, starting with who it is and
/// when, such as `Speaker 2 (1:05): Thanks for having me.`
fn layout(segments: &[whisper::Segment], speakers: &[usize]) -> String {
    let mut out = String::new();
    let mut current = None;
    for (segment, &speaker) in segments.iter().zip(speakers) {
        if current == Some(speaker) {
            out.push(' ');
        } else {
            if current.is_some() {
                out.push_str("\n\n");
            }
            out.push_str(&format!("Speaker {} ({}): ", speaker + 1, clock(segment.from_ms)));
            current = Some(speaker);
        }
        out.push_str(&segment.text);
    }
    out
}

/// A time such as 4:05, or 1:02:03 an hour or more in.
pub fn clock(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 { format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60) } else { format!("{}:{:02}", s / 60, s % 60) }
}

/// Transcription failures are reported with some feeling, as asked for, but
/// always followed by what actually went wrong and what to do about it.
const OPENERS: [&str; 6] = [
    "Well, that went brilliantly.",
    "Oh, marvellous.",
    "Splendid. Truly a triumph.",
    "Ten out of ten for effort, nought for results.",
    "And the award for most useless transcript goes to this one.",
    "I tried. I really did. Well, a bit.",
];

/// A sarcastic sentence saying why there is no transcript. `reason` should be
/// a clause starting in lower case, as errors here are.
pub fn sarcastic(reason: &str) -> String {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let opener = OPENERS[NEXT.fetch_add(1, Ordering::Relaxed) % OPENERS.len()];
    let mut reason = reason.trim().to_owned();
    if !reason.ends_with(['.', '!', '?']) {
        reason.push('.');
    }
    format!("{opener} No transcript, because {reason}")
}

#[cfg(test)]
mod tests {
    use std::num::NonZero;

    use super::*;

    #[test]
    fn downsampling_keeps_the_length_and_a_low_tone() {
        for rate in [8000, 16000, 22050, 44100, 48000] {
            let tone: Vec<f32> =
                (0..rate).map(|i| (i as f32 / rate as f32 * 300.0 * std::f32::consts::TAU).sin() * 0.5).collect();
            let rate = NonZero::new(rate).unwrap();
            let (out, shortened) = decode_16k_mono(rodio::buffer::SamplesBuffer::new(NonZero::<u16>::MIN, rate, tone));
            assert!(!shortened);
            assert!(out.len().abs_diff(16000) <= 2, "{rate}: {}", out.len());
            let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
            assert!((0.4..=0.55).contains(&peak), "{rate}: a 300 Hz tone survives, {peak}");
        }
    }

    #[test]
    fn downsampling_quietens_what_is_too_high_to_keep() {
        let rate = 48000;
        let hiss: Vec<f32> =
            (0..rate).map(|i| (i as f32 / rate as f32 * 15000.0 * std::f32::consts::TAU).sin() * 0.5).collect();
        let (out, _) = decode_16k_mono(rodio::buffer::SamplesBuffer::new(NonZero::<u16>::MIN, NonZero::new(rate).unwrap(), hiss));
        let peak = out.iter().skip(10).fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak < 0.1, "{peak}");
    }

    #[test]
    fn stereo_is_mixed_to_mono() {
        let samples = [0.5f32, -0.5].repeat(16000);
        let (out, _) = decode_16k_mono(rodio::buffer::SamplesBuffer::new(
            NonZero::new(2).unwrap(),
            NonZero::new(16000).unwrap(),
            samples,
        ));
        assert_eq!(out.len(), 16000);
        assert!(out.iter().all(|s| s.abs() < 1e-6));
    }

    #[test]
    fn wav_files_can_be_read_back() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.wav");
        let samples = detect::tests::chord(0.5);
        write_wav(&samples, &path).unwrap();
        let pcm = crate::audio::decode(std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(pcm.rate, RATE);
        assert_eq!(pcm.samples.len(), samples.len());
    }

    #[test]
    fn listening_to_files() {
        let dir = tempfile::tempdir().unwrap();
        let mp3 = dir.path().join("chord.mp3");
        let pcm = crate::audio::Pcm { samples: detect::tests::chord(3.0), rate: RATE };
        crate::audio::save_mp3(&pcm, &mp3).unwrap();
        let heard = listen(&mp3).unwrap();
        assert!((heard.seconds() - 3.0).abs() < 0.2, "{}", heard.seconds());
        assert!(!heard.verdict.is_speech());
        assert!(!heard.shortened);

        let junk = dir.path().join("junk.mp3");
        std::fs::write(&junk, b"this is not audio at all").unwrap();
        assert!(listen(&junk).is_err());
        assert!(listen(&dir.path().join("missing.mp3")).is_err());
    }

    #[test]
    fn transcripts_have_a_paragraph_per_turn() {
        let seg = |from_ms, text: &str| whisper::Segment { from_ms, to_ms: from_ms + 1000, text: text.to_owned() };
        let segments = [seg(0, "Hello."), seg(1000, "Welcome."), seg(65_000, "Thanks."), seg(3_723_000, "Bye.")];
        assert_eq!(
            layout(&segments, &[0, 0, 1, 0]),
            "Speaker 1 (0:00): Hello. Welcome.\n\nSpeaker 2 (1:05): Thanks.\n\nSpeaker 1 (1:02:03): Bye."
        );
    }

    #[test]
    fn an_unfinished_sentence_keeps_its_speaker() {
        let seg = |text: &str| whisper::Segment { from_ms: 0, to_ms: 1000, text: text.to_owned() };
        let segments = [seg("I was thinking, at one"), seg("point during this event."), seg("Right."), seg("Yes")];
        let mut speakers = [1, 0, 2, 0];
        continue_sentences(&segments, &mut speakers);
        // The second goes with the first, and then all are numbered afresh.
        assert_eq!(speakers, [0, 0, 1, 2]);
    }

    #[test]
    fn sarcasm_still_says_what_went_wrong() {
        let first = sarcastic("whisper.cpp isn't installed");
        assert!(first.ends_with("No transcript, because whisper.cpp isn't installed."), "{first}");
        let second = sarcastic("the model is missing.");
        assert_ne!(first.split(" No transcript").next(), second.split(" No transcript").next(), "it varies");
        assert!(second.ends_with("because the model is missing."));
    }

    /// Transcribes a real recording with a real whisper.cpp and model:
    /// `E2E_AUDIO=talk.mp3 cargo test real_transcription -- --ignored --nocapture`.
    /// Uses the model in the app's folder, or `E2E_MODEL`.
    #[test]
    #[ignore]
    fn real_transcription() {
        let path = std::env::var("E2E_AUDIO").expect("set E2E_AUDIO to an audio file");
        let model = std::env::var("E2E_MODEL").map(std::path::PathBuf::from).unwrap_or_else(|_| whisper::model_path());
        let heard = listen(Path::new(&path)).unwrap();
        println!("{:.0} seconds, {:?}", heard.seconds(), heard.verdict);
        let program = whisper::find_program(None).expect("whisper.cpp is installed");
        let text = transcribe(&program, &model, &heard.samples, &Control::default(), &|_| {}).unwrap().unwrap();
        println!("{text}");
        assert!(text.starts_with("Speaker 1 (0:"));
    }
}
