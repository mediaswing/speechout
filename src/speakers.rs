//! Works out who is speaking when in a recording, so a transcript can say
//! "Speaker 1:" and "Speaker 2:". This is done on this computer by
//! sherpa-onnx with two small AI models: one finds where people speak and
//! where the voice changes, and the other describes how each stretch of
//! speech sounds, so stretches that sound alike can be grouped as one person.
//!
//! As with Whisper, the audio is never sent anywhere. The only network
//! requests are the one-off downloads of the two models, which the user is
//! asked about first.

use crate::audio::Listened;
use crate::transcribe::{Piece, join_segments};
use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use sherpa_onnx_sys::offline_speaker_diarization as sys;
use std::ffi::{CString, c_void};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::Duration;

/// The two models together, for "about 46 MB".
pub const MODELS_SIZE: &str = "46 MB";

/// How far apart two voices must sound to be taken as different people when
/// the number of speakers isn't given. Higher joins people into fewer
/// speakers. With TitaNet, 0.75 tells apart the four people in sherpa-onnx's
/// test recording (0-four-speakers-zh.wav) and the two in
/// `labels_a_conversation`; 0.7 hears a fifth person in the first, and 0.8
/// hears only one in the second.
const SAME_SPEAKER_THRESHOLD: f32 = 0.75;

/// The most speakers that can be chosen on the Audio Player tab.
const MAX_SPEAKERS: usize = 8;

/// Whether, and how, to label who is speaking in a transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SpeakerLabels {
    #[default]
    Off,
    /// Label them, working out how many people there are.
    Auto,
    /// Label them, knowing how many people there are.
    Count(usize),
}

impl SpeakerLabels {
    /// The choices on the Audio Player tab, in order.
    pub fn choices() -> Vec<SpeakerLabels> {
        let counts = (2..=MAX_SPEAKERS).map(SpeakerLabels::Count);
        [SpeakerLabels::Off, SpeakerLabels::Auto].into_iter().chain(counts).collect()
    }
}

struct ModelFile {
    file_name: &'static str,
    url: &'static str,
    /// The website, for "could not reach GitHub".
    source: &'static str,
    bytes: u64,
}

impl ModelFile {
    fn path(&self) -> PathBuf {
        crate::paths::speakers_dir().join(self.file_name)
    }
}

/// pyannote segmentation 3.0, which finds speech and changes of speaker
/// (MIT licence).
const SEGMENTATION: ModelFile = ModelFile {
    file_name: "pyannote-segmentation-3.0.onnx",
    url: "https://huggingface.co/csukuangfj/sherpa-onnx-pyannote-segmentation-3-0/resolve/main/model.onnx",
    source: "Hugging Face",
    bytes: 5_992_913,
};

/// NVIDIA NeMo TitaNet small, which describes how someone sounds (CC BY 4.0
/// licence). Of the models sherpa-onnx offers, it told voices apart best in
/// testing; the CAM++ ones confused even a man and a woman.
const EMBEDDING: ModelFile = ModelFile {
    file_name: "nemo-titanet-small.onnx",
    url: "https://github.com/k2-fsa/sherpa-onnx/releases/download/speaker-recongition-models/nemo_en_titanet_small.onnx",
    source: "GitHub",
    bytes: 40_257_283,
};

pub fn models_downloaded() -> bool {
    [SEGMENTATION, EMBEDDING].iter().all(|m| m.path().is_file())
}

/// Downloads whichever of the two models is missing, calling `progress` with
/// the fraction done. Returns false if `stopped` said to stop.
pub fn download(mut progress: impl FnMut(f32), stopped: impl Fn() -> bool) -> anyhow::Result<bool> {
    let models = [SEGMENTATION, EMBEDDING];
    let total: u64 = models.iter().map(|m| m.bytes).sum();
    let mut before = 0;
    for model in &models {
        if !model.path().is_file() {
            let share = |f: f32| (before as f32 + f * model.bytes as f32) / total as f32;
            let report = |f| progress(share(f));
            if !crate::transcribe::download_file(model.url, &model.path(), model.source, report, &stopped)? {
                return Ok(false);
            }
        }
        before += model.bytes;
    }
    Ok(true)
}

/// A stretch of the recording in which one person speaks. Times are in
/// seconds, and speakers are numbered from 0 in the order they first speak.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub start: f32,
    pub end: f32,
    pub speaker: usize,
}

/// Works out who speaks when in `audio`. `speakers` is how many people there
/// are, if the user knows; otherwise it is worked out from how alike the
/// voices sound. `progress` is called with the fraction done.
///
/// sherpa-onnx can't be stopped part-way, so it runs on a thread of its own
/// and is left to finish if `stopped` says to stop, when this returns `None`
/// at once and its result is thrown away.
pub fn find(
    audio: Arc<Listened>,
    speakers: Option<usize>,
    mut progress: impl FnMut(f32),
    stopped: impl Fn() -> bool,
) -> anyhow::Result<Option<Vec<Turn>>> {
    if !models_downloaded() {
        bail!("the speaker models are not downloaded");
    }
    let diarizer = Diarizer::new(speakers)?;
    if diarizer.sample_rate() != crate::audio::LISTEN_RATE as i32 {
        bail!("the speaker models expect a different sample rate");
    }
    log::info!("finding speakers ({})", speakers.map_or("number not given".to_owned(), |n| format!("{n} given")));

    enum Event {
        Progress(f32),
        Done(anyhow::Result<Vec<Turn>>),
    }
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let progress_tx = tx.clone();
        let result = diarizer.process(&audio.pcm.samples, move |f| {
            let _ = progress_tx.send(Event::Progress(f));
        });
        // Nobody is listening any more if the job was stopped.
        let _ = tx.send(Event::Done(result));
    });
    loop {
        if stopped() {
            log::info!("finding speakers stopped");
            return Ok(None);
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Event::Progress(f)) => progress(f),
            Ok(Event::Done(result)) => {
                let turns = result?;
                log::info!(
                    "found {} speakers in {} turns",
                    turns.iter().map(|t| t.speaker + 1).max().unwrap_or(0),
                    turns.len()
                );
                return Ok(Some(turns));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => bail!("finding the speakers failed"),
        }
    }
}

/// Owns a sherpa-onnx speaker diarizer. The C API is used directly, as the
/// Rust wrapper can't report progress.
struct Diarizer(*const sys::OfflineSpeakerDiarization);

// SAFETY: the diarizer is used by one thread at a time, which sherpa-onnx
// supports.
unsafe impl Send for Diarizer {}

unsafe extern "C" {
    /// Missing from sherpa-onnx-sys, though in the C API it is built from.
    fn SherpaOnnxOfflineSpeakerDiarizationProcessWithCallback(
        sd: *const sys::OfflineSpeakerDiarization,
        samples: *const f32,
        n: i32,
        callback: Option<unsafe extern "C" fn(i32, i32, *mut c_void) -> i32>,
        arg: *mut c_void,
    ) -> *const sys::OfflineSpeakerDiarizationResult;
}

impl Diarizer {
    fn new(speakers: Option<usize>) -> anyhow::Result<Self> {
        let path = |m: &ModelFile| CString::new(m.path().to_string_lossy().into_owned()).context("bad model path");
        let segmentation = path(&SEGMENTATION)?;
        let embedding = path(&EMBEDDING)?;
        let cpu = c"cpu";
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8) as i32;
        let config = sys::OfflineSpeakerDiarizationConfig {
            segmentation: sys::OfflineSpeakerSegmentationModelConfig {
                pyannote: sys::OfflineSpeakerSegmentationPyannoteModelConfig {
                    model: segmentation.as_ptr(),
                    window_shift_ratio: 0.1,
                },
                num_threads: threads,
                debug: 0,
                provider: cpu.as_ptr(),
            },
            embedding: sherpa_onnx_sys::speaker_embedding::SpeakerEmbeddingExtractorConfig {
                model: embedding.as_ptr(),
                num_threads: threads,
                debug: 0,
                provider: cpu.as_ptr(),
            },
            clustering: sys::FastClusteringConfig {
                num_clusters: speakers.map_or(-1, |n| n as i32),
                threshold: SAME_SPEAKER_THRESHOLD,
                compute_confidence: 0,
            },
            // The sherpa-onnx defaults: ignore speech shorter than 0.3
            // seconds, and join pauses shorter than 0.5 seconds.
            min_duration_on: 0.3,
            min_duration_off: 0.5,
        };
        // SAFETY: the config and the strings it points to outlive the call,
        // which copies what it needs.
        let ptr = unsafe { sys::SherpaOnnxCreateOfflineSpeakerDiarization(&config) };
        if ptr.is_null() {
            bail!("could not load the speaker models. Try downloading them again");
        }
        Ok(Self(ptr))
    }

    fn sample_rate(&self) -> i32 {
        // SAFETY: the pointer is a live diarizer.
        unsafe { sys::SherpaOnnxOfflineSpeakerDiarizationGetSampleRate(self.0) }
    }

    /// Finds the turns in mono `samples` at the model's sample rate, calling
    /// `progress` with the fraction done.
    fn process(&self, samples: &[f32], progress: impl FnMut(f32)) -> anyhow::Result<Vec<Turn>> {
        unsafe extern "C" fn report(done: i32, total: i32, data: *mut c_void) -> i32 {
            // SAFETY: `data` is the `&mut dyn FnMut(f32)` below, which lives
            // until the call that calls this returns.
            let progress = unsafe { &mut *data.cast::<&mut dyn FnMut(f32)>() };
            if total > 0 {
                progress(done as f32 / total as f32);
            }
            // sherpa-onnx ignores this.
            0
        }
        let n = i32::try_from(samples.len()).context("the recording is too long to find the speakers in")?;
        let mut progress = progress;
        let mut progress: &mut dyn FnMut(f32) = &mut progress;
        // SAFETY: the diarizer is live, `samples` has `n` samples, and the
        // callback's data points to `progress`, as `report` expects.
        let result = unsafe {
            SherpaOnnxOfflineSpeakerDiarizationProcessWithCallback(
                self.0,
                samples.as_ptr(),
                n,
                Some(report),
                (&raw mut progress).cast(),
            )
        };
        if result.is_null() {
            bail!("the speakers could not be told apart");
        }
        // SAFETY: `result` is a live result, its segments array has the
        // length it gives, and both are destroyed once copied.
        let segments = unsafe {
            let count = sys::SherpaOnnxOfflineSpeakerDiarizationResultGetNumSegments(result).max(0) as usize;
            let array = sys::SherpaOnnxOfflineSpeakerDiarizationResultSortByStartTime(result);
            let segments = if array.is_null() || count == 0 {
                Vec::new()
            } else {
                std::slice::from_raw_parts(array, count).iter().map(|s| (s.start, s.end, s.speaker)).collect()
            };
            if !array.is_null() {
                sys::SherpaOnnxOfflineSpeakerDiarizationDestroySegment(array);
            }
            sys::SherpaOnnxOfflineSpeakerDiarizationDestroyResult(result);
            segments
        };
        Ok(number_speakers(segments))
    }
}

impl Drop for Diarizer {
    fn drop(&mut self) {
        // SAFETY: the pointer is a live diarizer, destroyed only here.
        unsafe { sys::SherpaOnnxDestroyOfflineSpeakerDiarization(self.0) }
    }
}

/// Turns sherpa-onnx's segments, sorted by start time, into turns, with the
/// speakers numbered in the order they first speak.
fn number_speakers(segments: Vec<(f32, f32, i32)>) -> Vec<Turn> {
    let mut order: Vec<i32> = Vec::new();
    segments
        .into_iter()
        .map(|(start, end, id)| {
            let speaker = order.iter().position(|o| *o == id).unwrap_or_else(|| {
                order.push(id);
                order.len() - 1
            });
            Turn { start, end, speaker }
        })
        .collect()
}

/// Joins the timed words of a transcript into paragraphs, one for each turn,
/// each starting "Speaker 1:" and so on. Each word goes to the turn it
/// overlaps most, or the nearest one if it overlaps none. Returns the text
/// and how many people speak in it. When that is only one, the text has no
/// labels, as they would say nothing.
pub fn label(words: &[Piece], turns: &[Turn]) -> (String, usize) {
    if turns.is_empty() {
        return (join_segments(words), 1);
    }
    let speaker_of = |(start, end, _): &Piece| {
        let (start, end) = (*start as f32 / 100.0, *end as f32 / 100.0);
        let overlap = |t: &Turn| t.end.min(end) - t.start.max(start);
        let best = turns.iter().max_by(|a, b| overlap(a).total_cmp(&overlap(b))).map(|t| t.speaker);
        best.unwrap_or(0)
    };

    // Runs of words by the same speaker, renumbered in the order they first
    // speak, since a speaker may have no words of their own.
    let mut runs: Vec<(usize, Vec<Piece>)> = Vec::new();
    let mut order: Vec<usize> = Vec::new();
    for word in words.iter().filter(|w| !w.2.trim().is_empty()) {
        let found = speaker_of(word);
        let speaker = order.iter().position(|s| *s == found).unwrap_or_else(|| {
            order.push(found);
            order.len() - 1
        });
        match runs.last_mut() {
            Some((s, run)) if *s == speaker => run.push(word.clone()),
            _ => runs.push((speaker, vec![word.clone()])),
        }
    }
    if order.len() <= 1 {
        return (join_segments(words), order.len().max(1));
    }
    let paragraphs: Vec<String> =
        runs.iter().map(|(speaker, run)| format!("Speaker {}: {}", speaker + 1, join_segments(run))).collect();
    (paragraphs.join("\n\n"), order.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(list: &[(i64, i64, &str)]) -> Vec<Piece> {
        list.iter().map(|(s, e, w)| (*s, *e, w.to_string())).collect()
    }

    fn turn(start: f32, end: f32, speaker: usize) -> Turn {
        Turn { start, end, speaker }
    }

    #[test]
    fn speakers_are_numbered_in_the_order_they_speak() {
        let turns = number_speakers(vec![(0.0, 1.0, 3), (1.0, 2.0, 0), (2.0, 3.0, 3)]);
        assert_eq!(turns, vec![turn(0.0, 1.0, 0), turn(1.0, 2.0, 1), turn(2.0, 3.0, 0)]);
    }

    #[test]
    fn words_are_labelled_by_turn() {
        let said = words(&[
            (0, 40, " Good"),
            (40, 90, " morning."),
            (110, 150, " Hi"),
            (150, 200, " there."),
            (210, 260, " How"),
            (260, 300, " are"),
            (300, 340, " you?"),
            (900, 950, " Fine."),
        ]);
        // The turn times are off a little from the word times, as they are
        // in practice. "Fine." overlaps no turn, so goes to the nearest.
        let turns = [turn(0.0, 1.05, 4), turn(1.0, 2.05, 2), turn(2.1, 3.5, 4), turn(9.6, 10.0, 2)];
        let (text, speakers) = label(&said, &turns);
        assert_eq!(speakers, 2);
        assert_eq!(
            text,
            "Speaker 1: Good morning.\n\nSpeaker 2: Hi there.\n\nSpeaker 1: How are you?\n\nSpeaker 2: Fine."
        );
    }

    #[test]
    fn one_speaker_has_no_labels() {
        let said = words(&[(0, 40, " Just"), (40, 90, " me.")]);
        assert_eq!(label(&said, &[turn(0.0, 1.0, 0)]), ("Just me.".to_owned(), 1));
        assert_eq!(label(&said, &[]), ("Just me.".to_owned(), 1));
    }

    #[test]
    fn choices_offer_two_to_eight_speakers() {
        let choices = SpeakerLabels::choices();
        assert_eq!(choices.len(), 9);
        assert_eq!(choices[..3], [SpeakerLabels::Off, SpeakerLabels::Auto, SpeakerLabels::Count(2)]);
        assert_eq!(choices.last(), Some(&SpeakerLabels::Count(MAX_SPEAKERS)));
    }

    /// A conversation between system voices, transcribed and labelled.
    /// Downloads the Whisper base model and the speaker models (about 190 MB
    /// in all) if they aren't there already. The voices are macOS ones.
    #[test]
    #[ignore]
    #[cfg(target_os = "macos")]
    fn labels_a_conversation() {
        use crate::transcribe::{WhisperModel, transcribe};
        let model = WhisperModel::Base;
        if !model.is_downloaded() {
            assert!(crate::transcribe::download(model, |_| (), || false).unwrap());
        }
        if !models_downloaded() {
            assert!(download(|_| (), || false).unwrap());
        }
        let script = [
            ("Daniel", "Good morning, and welcome to the programme. Today we are talking about the weather."),
            ("Samantha", "Thanks for having me. It has been a very wet autumn across most of the country."),
            ("Daniel", "Will it get any better next week, do you think?"),
            ("Samantha", "I expect the rain to clear by Wednesday, with sunshine and cooler nights after that."),
            ("Daniel", "That is good news for everyone. Thank you for joining us."),
        ];
        let mut pcm: Option<crate::audio::Pcm> = None;
        for (voice, line) in script {
            let wav = crate::speech::synthesize(crate::speech::Provider::System, None, voice, line, 1.0).unwrap();
            let said = crate::audio::decode(wav).unwrap();
            let all = pcm.get_or_insert_with(|| crate::audio::Pcm { samples: Vec::new(), rate: said.rate });
            assert_eq!(all.rate, said.rate);
            all.samples.extend(&said.samples);
            all.samples.extend(std::iter::repeat_n(0.0, said.rate as usize * 7 / 10));
        }
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("conversation.wav");
        crate::audio::save(&pcm.unwrap(), &path, crate::audio::AudioFormat::Wav).unwrap();
        let audio = Arc::new(crate::audio::listen(&path, || false).unwrap().unwrap());

        let words = transcribe(model, &audio.pcm.samples, true, |_| (), || false).unwrap().unwrap();
        for count in [None, Some(2)] {
            let turns = find(audio.clone(), count, |_| (), || false).unwrap().unwrap();
            let (text, speakers) = label(&words, &turns);
            assert_eq!(speakers, 2, "{text}");
            // Each line, with any words Whisper heard differently, goes to
            // the right speaker.
            let paragraphs: Vec<&str> = text.split("\n\n").collect();
            assert_eq!(paragraphs.len(), script.len(), "{text}");
            for (paragraph, n) in paragraphs.iter().zip([1, 2, 1, 2, 1]) {
                assert!(paragraph.starts_with(&format!("Speaker {n}: ")), "{text}");
            }
            for (paragraph, end) in paragraphs.iter().zip(["weather.", "country.", "think?", "that.", "us."]) {
                assert!(paragraph.ends_with(end), "{text}");
            }
        }
        // Stopping returns at once.
        assert_eq!(find(audio, None, |_| (), || true).unwrap(), None);
    }

    #[test]
    fn models_live_in_the_speakers_folder() {
        for model in [SEGMENTATION, EMBEDDING] {
            assert!(model.path().starts_with(crate::paths::speakers_dir()));
            assert!(model.url.starts_with("https://"));
        }
    }
}
