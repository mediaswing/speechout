//! Turns speech in an audio file into text with Whisper, an AI model that
//! runs on this computer through whisper.cpp.
//!
//! The audio is never sent anywhere. The only network request is the one-off
//! download of the Whisper model from Hugging Face, which the user is asked
//! about first.

use anyhow::{Context, bail};
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

/// A download that sends nothing for this long is given up.
const STALL_LIMIT: Duration = Duration::from_secs(60);

/// A segment ending this long before the next one starts begins a new
/// paragraph, in hundredths of a second.
const PARAGRAPH_GAP: i64 = 150;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum WhisperModel {
    /// Quick, and good with clear speech.
    #[default]
    Base,
    /// Slower, but better with accents, noise and languages other than English.
    Small,
}

impl WhisperModel {
    pub const ALL: [WhisperModel; 2] = [WhisperModel::Base, WhisperModel::Small];

    /// The model's name as people know it.
    pub fn name(self) -> &'static str {
        match self {
            WhisperModel::Base => "Whisper base",
            WhisperModel::Small => "Whisper small",
        }
    }

    /// The download size, for "about 150 MB".
    pub fn size(self) -> &'static str {
        match self {
            WhisperModel::Base => "150 MB",
            WhisperModel::Small => "490 MB",
        }
    }

    fn file_name(self) -> &'static str {
        match self {
            WhisperModel::Base => "ggml-base.bin",
            WhisperModel::Small => "ggml-small.bin",
        }
    }

    pub fn path(self) -> PathBuf {
        crate::paths::whisper_dir().join(self.file_name())
    }

    pub fn is_downloaded(self) -> bool {
        self.path().is_file()
    }

    fn url(self) -> String {
        format!("https://huggingface.co/ggerganov/whisper.cpp/resolve/main/{}", self.file_name())
    }
}

/// Downloads `model` from Hugging Face, calling `progress` with the fraction
/// done. Returns false if `stopped` said to stop, when the part downloaded so
/// far is thrown away.
pub fn download(
    model: WhisperModel,
    mut progress: impl FnMut(f32),
    stopped: impl Fn() -> bool,
) -> anyhow::Result<bool> {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    let agent = AGENT.get_or_init(|| {
        ureq::Agent::config_builder()
            .https_only(true)
            .timeout_connect(Some(Duration::from_secs(20)))
            .timeout_recv_response(Some(Duration::from_secs(60)))
            .user_agent(concat!("speechout/", env!("CARGO_PKG_VERSION")))
            .build()
            .into()
    });
    let path = model.path();
    let dir = path.parent().context("the model folder could not be found")?;
    std::fs::create_dir_all(dir).with_context(|| format!("could not create {}", dir.display()))?;
    log::info!("downloading {} from {}", model.name(), model.url());
    let response = agent.get(&model.url()).call().context("could not reach Hugging Face to download the model")?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    let body = response.into_body().into_reader();
    // Written beside the model and renamed when complete, so a model file
    // that exists is always whole.
    let mut part = tempfile::NamedTempFile::new_in(dir).context("could not save the model")?;
    let report = |done| {
        if let Some(total) = total.filter(|t| *t > 0) {
            progress(done as f32 / total as f32);
        }
    };
    let Some(done) = copy_body(body, &mut part, report, stopped, STALL_LIMIT)? else {
        log::info!("Whisper model download stopped");
        return Ok(false);
    };
    if total.is_some_and(|t| t != done) || done == 0 {
        bail!("the download ended before it finished");
    }
    part.persist(&path).map_err(|e| e.error).context("could not save the model")?;
    log::info!("downloaded {} ({done} bytes)", model.name());
    Ok(true)
}

/// Copies `body` into `out`, calling `progress` with the bytes so far.
/// Returns how many bytes were copied, or `None` if `stopped` said to stop.
///
/// Reading happens on a thread of its own, because a stalled connection can
/// leave a read waiting for ever. That way Escape still works, and the
/// download is given up if nothing arrives for `stall_limit`. A thread left
/// waiting ends when the connection finally closes.
fn copy_body(
    mut body: impl Read + Send + 'static,
    out: &mut impl Write,
    mut progress: impl FnMut(u64),
    stopped: impl Fn() -> bool,
    stall_limit: Duration,
) -> anyhow::Result<Option<u64>> {
    use std::sync::mpsc::RecvTimeoutError;
    let (tx, rx) = std::sync::mpsc::sync_channel::<std::io::Result<Vec<u8>>>(4);
    std::thread::spawn(move || {
        let mut buffer = vec![0; 256 * 1024];
        loop {
            let chunk = body.read(&mut buffer).map(|n| buffer[..n].to_vec());
            let last = !matches!(&chunk, Ok(c) if !c.is_empty());
            // The receiver is gone once the download is stopped or given up.
            if tx.send(chunk).is_err() || last {
                break;
            }
        }
    });
    let mut done = 0u64;
    let mut last_data = Instant::now();
    loop {
        if stopped() {
            return Ok(None);
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(Ok(chunk)) if chunk.is_empty() => return Ok(Some(done)),
            Ok(Ok(chunk)) => {
                out.write_all(&chunk).context("could not save the model")?;
                done += chunk.len() as u64;
                last_data = Instant::now();
                progress(done);
            }
            Ok(Err(e)) => return Err(e).context("the download was interrupted"),
            Err(RecvTimeoutError::Timeout) if last_data.elapsed() >= stall_limit => {
                bail!("the download stopped making progress. Check the internet connection and try again")
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => bail!("the download was interrupted"),
        }
    }
}

/// Transcribes mono audio at 16 kHz with `model`, which must have been
/// downloaded. `progress` is called with the percentage done. Returns `None`
/// if `stopped` said to stop.
pub fn transcribe(
    model: WhisperModel,
    samples: &[f32],
    mut progress: impl FnMut(i32),
    mut stopped: impl FnMut() -> bool,
) -> anyhow::Result<Option<String>> {
    use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};
    // Send whisper.cpp's own messages to the debug log, not the terminal.
    whisper_rs::install_logging_hooks();

    let path = model.path();
    log::info!("transcribing {:.0} seconds of audio with {}", samples.len() as f32 / 16_000.0, model.name());
    let context = WhisperContext::new_with_params(&path, WhisperContextParameters::default())
        .with_context(|| format!("could not load the {} model. Try downloading it again", model.name()))?;
    let mut state = context.create_state().context("could not start the speech recognition")?;

    let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 5 });
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).min(8);
    params.set_n_threads(threads as i32);
    // Work out the language from the speech.
    params.set_language(Some("auto"));
    params.set_print_special(false);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_suppress_blank(true);
    // The callbacks are set up by hand, because whisper-rs 0.16's safe
    // versions go wrong: set_abort_callback_safe reads its closure back as
    // the wrong type, and set_progress_callback_safe never frees its closure.
    // `report` and `abort` outlive `state.full`, the only time whisper.cpp
    // calls them.
    let mut report: &mut dyn FnMut(i32) = &mut progress;
    let mut abort: &mut dyn FnMut() -> bool = &mut stopped;
    // SAFETY: each user data pointer points to `report` or `abort`, which
    // `report_progress` and `should_abort` read as the same types, and which
    // live until `state.full` has returned.
    unsafe {
        params.set_progress_callback(Some(report_progress));
        params.set_progress_callback_user_data((&raw mut report).cast());
        params.set_abort_callback(Some(should_abort));
        params.set_abort_callback_user_data((&raw mut abort).cast());
    }

    let result = state.full(params, samples);
    // whisper.cpp fails when it gives up, so a transcript that finished just
    // as Stop was pressed is still kept.
    if result.is_err() && stopped() {
        log::info!("transcribing stopped");
        return Ok(None);
    }
    result.context("the speech could not be recognised")?;

    if let Some(language) = whisper_rs::get_lang_str_full(state.full_lang_id_from_state()) {
        log::info!("detected language: {language}");
    }
    let segments: Vec<(i64, i64, String)> = state
        .as_iter()
        .filter_map(|s| Some((s.start_timestamp(), s.end_timestamp(), s.to_str_lossy().ok()?.into_owned())))
        .collect();
    Ok(Some(join_segments(&segments)))
}

/// Told by whisper.cpp how far it has got, as a percentage. `data` points to
/// the `&mut dyn FnMut(i32)` set up in `transcribe`.
unsafe extern "C" fn report_progress(
    _: *mut whisper_rs::whisper_rs_sys::whisper_context,
    _: *mut whisper_rs::whisper_rs_sys::whisper_state,
    percent: std::ffi::c_int,
    data: *mut std::ffi::c_void,
) {
    // SAFETY: see `transcribe`.
    let report = unsafe { &mut *data.cast::<&mut dyn FnMut(i32)>() };
    report(percent);
}

/// Asked by whisper.cpp after each step whether to give up. `data` points to
/// the `&mut dyn FnMut() -> bool` set up in `transcribe`.
unsafe extern "C" fn should_abort(data: *mut std::ffi::c_void) -> bool {
    // SAFETY: see `transcribe`.
    let abort = unsafe { &mut *data.cast::<&mut dyn FnMut() -> bool>() };
    abort()
}

/// Joins Whisper's segments (start and end in hundredths of a second, and
/// text) into paragraphs, starting a new one after a long pause.
fn join_segments(segments: &[(i64, i64, String)]) -> String {
    let mut text = String::new();
    let mut last_end = None;
    for (start, end, words) in segments {
        let words = words.trim();
        if words.is_empty() {
            continue;
        }
        match last_end {
            None => {}
            Some(last) if start - last >= PARAGRAPH_GAP => text.push_str("\n\n"),
            Some(_) => text.push(' '),
        }
        text.push_str(words);
        last_end = Some(*end);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_join_into_paragraphs() {
        let segments = [
            (0, 250, " Good morning.".to_owned()),
            (260, 500, " Here is the news. ".to_owned()),
            (480, 520, "   ".to_owned()),
            (700, 900, " The weather is next.".to_owned()),
        ];
        assert_eq!(join_segments(&segments), "Good morning. Here is the news.\n\nThe weather is next.");
        assert_eq!(join_segments(&[]), "");
    }

    /// Downloads the base model (about 150 MB) if it isn't there already.
    #[test]
    #[ignore]
    fn transcribes_the_system_voice() {
        let model = WhisperModel::Base;
        if !model.is_downloaded() {
            assert!(download(model, |_| (), || false).unwrap());
        }
        let said = "Please read this document aloud, then save it as an audio file.";
        let wav = crate::speech::synthesize(crate::speech::Provider::System, None, "", said, 1.0).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("said.wav");
        crate::audio::save(&crate::audio::decode(wav).unwrap(), &path, crate::audio::AudioFormat::Wav).unwrap();
        let audio = crate::audio::listen(&path, || false).unwrap().unwrap();
        assert_eq!(audio.sound, crate::audio::SoundKind::Speech);

        let mut reached = 0;
        let text = transcribe(model, &audio.pcm.samples, |p| reached = p, || false).unwrap().unwrap();
        assert_eq!(reached, 100);
        let words = |s: &str| {
            let lower = s.to_lowercase();
            lower.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_owned).collect::<Vec<_>>()
        };
        assert_eq!(words(&text), words(said), "heard: {text}");
        assert_eq!(transcribe(model, &audio.pcm.samples, |_| (), || true).unwrap(), None);
    }

    /// A download that never sends anything, like a stalled connection.
    struct Stalled;

    impl Read for Stalled {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            std::thread::sleep(Duration::from_secs(3600));
            Ok(0)
        }
    }

    #[test]
    fn downloads_copy_everything() {
        let data: Vec<u8> = (0..600_000u32).map(|i| i as u8).collect();
        let mut out = Vec::new();
        let mut reported = 0;
        let body = std::io::Cursor::new(data.clone());
        let done = copy_body(body, &mut out, |d| reported = d, || false, STALL_LIMIT);
        assert_eq!(done.unwrap(), Some(600_000));
        assert_eq!(reported, 600_000);
        assert_eq!(out, data);
    }

    #[test]
    fn a_stalled_download_can_be_stopped_or_given_up() {
        let start = Instant::now();
        let stop_soon = || start.elapsed() > Duration::from_millis(300);
        let stopped = copy_body(Stalled, &mut Vec::new(), |_| (), stop_soon, STALL_LIMIT);
        assert_eq!(stopped.unwrap(), None);
        assert!(start.elapsed() < Duration::from_secs(2));

        let start = Instant::now();
        let given_up = copy_body(Stalled, &mut Vec::new(), |_| (), || false, Duration::from_millis(300));
        assert!(given_up.unwrap_err().to_string().contains("stopped making progress"));
        assert!(start.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn models_live_in_the_whisper_folder() {
        for model in WhisperModel::ALL {
            assert!(model.path().starts_with(crate::paths::whisper_dir()));
            assert!(model.url().starts_with("https://huggingface.co/"));
        }
    }
}
