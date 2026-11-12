//! Telling speech from music and other sounds, from the waveform alone.
//!
//! Speech comes in syllables, a few a second, with short gaps between them, so
//! its loudness swings much more than music's does, and many of its frames are
//! far quieter than those around them. Its consonants also put short bursts of
//! hiss among the voiced sounds, so how often the waveform crosses zero jumps
//! about from frame to frame. Each second of sound is judged on these, and the
//! audio counts as speech if most of the seconds that aren't silent sound like it.

use super::RATE;

/// 20 ms frames.
const FRAME: usize = RATE as usize / 50;
/// Frames in a second.
const WINDOW: usize = 50;
/// A second quieter than this, about -50 dBFS, is silence and isn't judged.
const SILENCE: f32 = 0.003;
/// The share of the sound that has to be speech.
pub const SPEECH_SHARE_NEEDED: f32 = 0.5;

/// What the waveform says about the audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Verdict {
    /// The share of the seconds with sound in them that sound like speech.
    pub speech_share: f32,
    /// Seconds that weren't silent.
    pub sound_seconds: usize,
}

impl Verdict {
    /// Largely spoken word: at least a second of sound, mostly speech.
    pub fn is_speech(&self) -> bool {
        self.sound_seconds > 0 && self.speech_share >= SPEECH_SHARE_NEEDED
    }
}

/// Judges 16 kHz mono audio.
pub fn judge(samples: &[f32]) -> Verdict {
    let frames: Vec<(f32, f32)> = samples.as_chunks::<FRAME>().0.iter().map(|f| (rms(f), zero_crossings(f))).collect();
    let mut sound = 0;
    let mut speech = 0;
    for window in frames.as_chunks::<WINDOW>().0 {
        match judge_second(window) {
            None => {}
            Some(is_speech) => {
                sound += 1;
                speech += usize::from(is_speech);
            }
        }
    }
    let speech_share = if sound == 0 { 0.0 } else { speech as f32 / sound as f32 };
    Verdict { speech_share, sound_seconds: sound }
}

/// Whether a second of (loudness, zero-crossing rate) frames sounds like speech,
/// or `None` if it is silent.
fn judge_second(frames: &[(f32, f32)]) -> Option<bool> {
    let count = frames.len() as f32;
    let mean_rms = frames.iter().map(|f| f.0).sum::<f32>() / count;
    if mean_rms < SILENCE {
        return None;
    }
    let mean_zcr = frames.iter().map(|f| f.1).sum::<f32>() / count;
    // Frames much quieter than the second around them: the gaps between syllables.
    let low_energy = frames.iter().filter(|f| f.0 < 0.5 * mean_rms).count() as f32 / count;
    // Frames with far more zero crossings than usual: consonants such as "s".
    let hissy = frames.iter().filter(|f| f.1 > 1.5 * mean_zcr).count() as f32 / count;
    // How far the loudness swings, in decibels.
    let db: Vec<f32> = frames.iter().map(|f| 20.0 * f.0.max(1e-5).log10()).collect();
    let mean_db = db.iter().sum::<f32>() / count;
    let swing = (db.iter().map(|d| (d - mean_db).powi(2)).sum::<f32>() / count).sqrt();
    let votes = u8::from(low_energy >= 0.2) + u8::from(hissy >= 0.08) + u8::from(swing >= 7.0);
    Some(votes >= 2)
}

fn rms(frame: &[f32]) -> f32 {
    (frame.iter().map(|s| s * s).sum::<f32>() / frame.len() as f32).sqrt()
}

/// The share of neighbouring samples on opposite sides of zero.
fn zero_crossings(frame: &[f32]) -> f32 {
    let crossings = frame.windows(2).filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0)).count();
    crossings as f32 / frame.len() as f32
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A steady chord with a little vibrato, like a held note in music.
    pub fn chord(seconds: f32) -> Vec<f32> {
        let n = (seconds * RATE as f32) as usize;
        (0..n)
            .map(|i| {
                let t = i as f32 / RATE as f32;
                let wobble = 1.0 + 0.003 * (t * 5.0 * std::f32::consts::TAU).sin();
                [220.0, 277.2, 329.6, 440.0]
                    .iter()
                    .map(|f| (t * f * wobble * std::f32::consts::TAU).sin() * 0.1)
                    .sum::<f32>()
            })
            .collect()
    }

    #[test]
    fn silence_is_not_speech() {
        let verdict = judge(&vec![0.0; RATE as usize * 5]);
        assert_eq!(verdict.sound_seconds, 0);
        assert!(!verdict.is_speech());
        assert!(!judge(&[]).is_speech());
    }

    #[test]
    fn a_held_chord_is_not_speech() {
        let verdict = judge(&chord(10.0));
        assert_eq!(verdict.sound_seconds, 10);
        assert!(!verdict.is_speech(), "{verdict:?}");
    }

    /// The real system voice, so a real spoken sentence. macOS only, because
    /// eSpeak NG's robotic voice is a poor stand-in for a person.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_real_voice_is_speech() {
        let text = "Good morning, everyone. Today we are going to talk about the history of the \
                    railway in the West Midlands, and why the line to Birmingham mattered so much.";
        let wav = crate::platform::synthesize(text, "").unwrap();
        let samples = super::super::to_16k_mono(crate::audio::decode(wav).unwrap());
        let verdict = judge(&samples);
        assert!(verdict.is_speech(), "{verdict:?}");
    }

    /// Prints the verdict on real recordings, to check the thresholds:
    /// `E2E_AUDIO_LIST=a.mp3:b.mp3 cargo test real_speech_detection -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn real_speech_detection() {
        let list = std::env::var_os("E2E_AUDIO_LIST").expect("set E2E_AUDIO_LIST to audio files");
        for path in std::env::split_paths(&list) {
            match super::super::listen(&path) {
                Ok(heard) => println!("{:>5.0}s {:?} {}", heard.seconds(), heard.verdict, path.display()),
                Err(e) => println!("error {e:#}: {}", path.display()),
            }
        }
    }
}
