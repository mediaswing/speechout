//! Background jobs. Anything slow (network, synthesis, file parsing) runs on
//! its own thread and reports back to the UI through a channel, so the
//! interface never freezes and the screen reader is kept informed.

use crate::audio::{self, AudioFormat, Playback};
use crate::i18n::{self, Language, Translation, t, tf};
use crate::speech::retry::{self, Kind};
use crate::speech::{self, Provider, Voice};
use crate::speakers::SpeakerLabels;
use crate::transcribe::WhisperModel;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub enum Msg {
    Voices(Provider, Result<Vec<Voice>, String>),
    Models(Result<Vec<String>, String>),
    Loaded(PathBuf, Result<String, String>),
    /// Downloading a model finished: true if complete, false if stopped.
    ModelDownloaded(String, Result<bool, String>),
    /// Installing and/or starting Ollama finished.
    OllamaReady(Result<(), String>),
    /// Translating the interface finished or was stopped.
    Translated(Result<Translated, String>),
    /// An audio file for the Audio Player tab was decoded and checked:
    /// `None` if opening it was stopped.
    AudioLoaded(PathBuf, Result<Option<Arc<audio::Listened>>, String>),
    /// How far into the audio file playback is.
    AudioPosition(Duration),
    /// Playing an audio file ended: true if it reached the end.
    AudioEnded(Result<bool, String>),
    /// Downloading a Whisper model finished: true if complete, false if stopped.
    WhisperDownloaded(WhisperModel, Result<bool, String>),
    /// Downloading the speaker models finished: true if complete, false if
    /// stopped.
    SpeakerModelsDownloaded(Result<bool, String>),
    /// Transcribing an audio file finished: `None` if it was stopped.
    Transcribed(PathBuf, Result<Option<Transcript>, String>),
    /// Result of an update check; `bool` is true when the user asked for it.
    Update(Result<Option<crate::update::Release>, String>, bool),
    /// Progress worth announcing.
    Status(String),
    /// How far through the current job we are, from 0.0 to 1.0.
    Progress(f32),
    /// A read-aloud or save job ended normally.
    Done(String),
    /// A read-aloud or save job ended with an error.
    Failed(String),
}

#[derive(Clone)]
pub struct Reporter {
    tx: Sender<Msg>,
    ctx: egui::Context,
}

impl Reporter {
    pub fn new(tx: Sender<Msg>, ctx: egui::Context) -> Self {
        Self { tx, ctx }
    }

    pub fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }

    pub fn spawn(&self, job: impl FnOnce(Reporter) + Send + 'static) {
        let rep = self.clone();
        std::thread::spawn(move || job(rep));
    }
}

/// The user sees the friendly top-level message; the technical cause chain
/// goes to the debug log.
fn err(e: anyhow::Error) -> String {
    log::warn!("{e:#}");
    let mut text = e.to_string();
    if let Some(first) = text.chars().next() {
        text.replace_range(..first.len_utf8(), &first.to_uppercase().to_string());
    }
    if !text.ends_with(['.', '!', '?']) {
        text.push('.');
    }
    text
}

/// Shared between the UI and a running read-aloud or save job.
#[derive(Default)]
pub struct Control {
    stop: AtomicBool,
    paused: AtomicBool,
    player: Mutex<Option<Arc<rodio::Player>>>,
    /// Where to move to in an audio file being played.
    seek: Mutex<Option<Duration>>,
}

impl Control {
    pub fn stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(p) = self.player.lock().ok().and_then(|g| g.clone()) {
            p.stop();
        }
    }

    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
        if let Some(p) = self.player.lock().ok().and_then(|g| g.clone()) {
            if paused { p.pause() } else { p.play() }
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }

    /// Asks the audio file player to move to `position`.
    pub fn seek(&self, position: Duration) {
        if let Ok(mut seek) = self.seek.lock() {
            *seek = Some(position);
        }
    }

    fn take_seek(&self) -> Option<Duration> {
        self.seek.lock().ok().and_then(|mut s| s.take())
    }

    /// Waits for `delay`, calling `tick` every tenth of a second. Returns
    /// false as soon as the job is stopped.
    fn wait(&self, delay: Duration, mut tick: impl FnMut()) -> bool {
        let end = Instant::now() + delay;
        while Instant::now() < end {
            if self.is_stopped() {
                return false;
            }
            tick();
            std::thread::sleep(Duration::from_millis(100).min(end.saturating_duration_since(Instant::now())));
        }
        !self.is_stopped()
    }
}

pub fn load_voices(rep: Reporter, provider: Provider, key: Option<String>) {
    let result = speech::list_voices(provider, key.as_deref()).map_err(err);
    rep.send(Msg::Voices(provider, result));
}

pub fn load_models(rep: Reporter) {
    rep.send(Msg::Models(crate::vision::list_models().map_err(err)));
}

/// Installs Ollama with the package manager if `install` is true, then starts
/// it and waits until it answers.
pub fn set_up_ollama(rep: Reporter, install: bool) {
    let result = (|| {
        if install {
            crate::platform::install_ollama()?;
            log::info!("installed Ollama");
            rep.send(Msg::Status(t("ollama.installed")));
        }
        // The installer may already have started it.
        if !crate::vision::wait_until_running(Duration::from_secs(2)) {
            crate::platform::start_ollama()?;
            if !crate::vision::wait_until_running(Duration::from_secs(60)) {
                anyhow::bail!("Ollama was started but is not answering. Try restarting the computer");
            }
        }
        Ok(())
    })();
    rep.send(Msg::OllamaReady(result.map_err(err)));
}

/// Downloads a model into Ollama, moving the progress bar and announcing each
/// quarter. Escape stops it.
pub fn download_model(rep: Reporter, control: Arc<Control>, model: String) {
    rep.send(Msg::Progress(0.0));
    let mut last_sent = -1.0;
    let mut announced = 0;
    let result = crate::vision::pull_model(
        &model,
        |fraction| {
            // Half a percent is enough to move the bar; avoid flooding the UI.
            if (fraction - last_sent).abs() >= 0.005 {
                last_sent = fraction;
                rep.send(Msg::Progress(fraction));
            }
            let quarter = (fraction * 4.0) as usize;
            if quarter > announced && quarter < 4 {
                announced = quarter;
                rep.send(Msg::Status(tf("model.download_progress", &[("percent", &(quarter * 25))])));
            }
        },
        || control.is_stopped(),
    );
    rep.send(Msg::ModelDownloaded(model, result.map_err(err)));
}

pub fn check_for_update(rep: Reporter, requested: bool) {
    let result = crate::update::check().map_err(err);
    rep.send(Msg::Update(result, requested));
}

pub fn load_file(rep: Reporter, path: PathBuf, model: String, resolve_location: bool) {
    use crate::document::FileKind;
    let result = match FileKind::from_path(&path) {
        Some(FileKind::Image) => crate::vision::describe(&path, &model, resolve_location),
        _ => crate::document::extract_text(&path),
    };
    rep.send(Msg::Loaded(path, result.map_err(err)));
}

pub struct SpeechJob {
    pub provider: Provider,
    pub key: Option<String>,
    pub voice: String,
    /// 1.0 for normal speed; ignored by providers without a speed setting.
    pub speed: f32,
    pub text: String,
    /// Say "This is part 2 of 3" at the start of each part of a long text;
    /// otherwise parts follow straight on from each other.
    pub announce_parts: bool,
    /// A short voice preview rather than the document.
    pub preview: bool,
}

impl SpeechJob {
    pub fn pieces(&self) -> Vec<String> {
        speech::pieces(self.provider, &self.text, self.announce_parts)
    }

    /// Renders one piece, trying again after a temporary failure such as a
    /// rate limit. Returns `None` if the job was stopped while waiting.
    /// `tick` is called during waits, to keep the progress bar moving.
    fn render(&self, rep: &Reporter, control: &Control, chunk: &str, mut tick: impl FnMut()) -> anyhow::Result<Option<audio::Pcm>> {
        let mut failures = 0;
        loop {
            let e = match speech::synthesize(self.provider, self.key.as_deref(), &self.voice, chunk, self.speed) {
                Ok(bytes) => return audio::decode(bytes).map(Some),
                Err(e) => e,
            };
            failures += 1;
            let kind = retry::kind_of(&e);
            let Some(delay) = retry::next_delay(kind, failures) else { return Err(e) };
            log::warn!("try {failures} of {} failed ({kind:?}), retrying in {delay:?}: {e:#}", retry::MAX_ATTEMPTS);
            // One announcement per retry, so the screen reader is not flooded.
            rep.send(Msg::Status(retry_message(self.provider, kind, delay)));
            if !control.wait(delay, &mut tick) {
                return Ok(None);
            }
        }
    }
}

fn retry_message(provider: Provider, kind: Kind, delay: Duration) -> String {
    let name = provider.short_name();
    let problem = match kind {
        Kind::RateLimited { .. } => tf("retry.rate_limited", &[("service", &name)]),
        Kind::Unavailable { .. } => tf("retry.busy", &[("service", &name)]),
        _ => tf("retry.unreachable", &[("service", &name)]),
    };
    let secs = delay.as_secs().max(1);
    let wait = if secs == 1 { t("retry.wait.one") } else { tf("retry.wait.other", &[("count", &secs)]) };
    format!("{problem} {wait}")
}

/// Works out how far through the text playback has got: whole pieces
/// already played, plus the position inside the piece playing now.
struct SpeakProgress {
    total: usize,
    /// Length in seconds of every piece handed to the player so far.
    durations: Vec<f32>,
    last_sent: f32,
}

impl SpeakProgress {
    fn report(&mut self, rep: &Reporter, playback: &Playback) {
        let finished = self.durations.len().saturating_sub(playback.queued());
        let within = self
            .durations
            .get(finished)
            .filter(|d| **d > 0.0)
            .map_or(0.0, |d| (playback.position().as_secs_f32() / d).min(1.0));
        let fraction = ((finished as f32 + within) / self.total.max(1) as f32).min(1.0);
        // Half a percent is enough to move the bar; avoid flooding the UI.
        if (fraction - self.last_sent).abs() >= 0.005 {
            self.last_sent = fraction;
            rep.send(Msg::Progress(fraction));
        }
    }

    /// Sleeps while `busy` holds, keeping the progress bar moving.
    fn wait(&mut self, rep: &Reporter, playback: &Playback, control: &Control, busy: impl Fn() -> bool) {
        while busy() && !control.is_stopped() {
            self.report(rep, playback);
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

/// Reads the text aloud, synthesising at most two pieces ahead of playback so
/// that stopping early does not waste cloud credit.
pub fn speak(rep: Reporter, control: Arc<Control>, job: SpeechJob) {
    let (stopped, finished) = if job.preview {
        (t("preview.stopped"), t("preview.finished"))
    } else {
        (t("read.stopped"), t("read.finished"))
    };
    let chunks = job.pieces();
    let playback = match Playback::open() {
        Ok(p) => p,
        Err(e) => return rep.send(Msg::Failed(err(e))),
    };
    if let Ok(mut slot) = control.player.lock() {
        *slot = Some(playback.handle());
    }
    // Apply a pause pressed before the audio device was ready.
    control.set_paused(control.is_paused());
    log::info!("reading {} piece(s) with {:?}", chunks.len(), job.provider);
    let mut progress = SpeakProgress { total: chunks.len(), durations: Vec::new(), last_sent: -1.0 };
    rep.send(Msg::Progress(0.0));

    for (i, chunk) in chunks.iter().enumerate() {
        progress.wait(&rep, &playback, &control, || playback.queued() >= 2);
        if control.is_stopped() {
            return rep.send(Msg::Done(stopped));
        }
        match job.render(&rep, &control, chunk, || progress.report(&rep, &playback)) {
            Ok(None) => return rep.send(Msg::Done(stopped)),
            Ok(Some(pcm)) => {
                if control.is_stopped() {
                    return rep.send(Msg::Done(stopped));
                }
                progress.durations.push(pcm.duration_secs());
                playback.append(pcm);
            }
            Err(e) => {
                log::warn!("synthesis failed on piece {}: {e:#}", i + 1);
                // Let what is already queued finish, then report.
                progress.wait(&rep, &playback, &control, || playback.queued() > 0);
                return rep.send(Msg::Failed(err(e)));
            }
        }
    }
    progress.wait(&rep, &playback, &control, || playback.queued() > 0);
    if control.is_stopped() {
        rep.send(Msg::Done(stopped));
    } else {
        rep.send(Msg::Done(finished));
    }
}

pub fn save(rep: Reporter, control: Arc<Control>, job: SpeechJob, path: PathBuf, format: AudioFormat) {
    let chunks = job.pieces();
    let total = chunks.len();
    let mut pieces = Vec::with_capacity(total);
    let mut announced = 0;
    rep.send(Msg::Progress(0.0));
    for (i, chunk) in chunks.iter().enumerate() {
        if control.is_stopped() {
            return rep.send(Msg::Done(t("save.cancelled")));
        }
        match job.render(&rep, &control, chunk, || ()) {
            Ok(None) => return rep.send(Msg::Done(t("save.cancelled"))),
            Ok(Some(pcm)) => pieces.push(pcm),
            Err(e) => {
                log::warn!("synthesis failed on piece {}: {e:#}", i + 1);
                return rep.send(Msg::Failed(err(e)));
            }
        }
        rep.send(Msg::Progress((i + 1) as f32 / total as f32));
        // Announce progress in quarter steps rather than every piece, so the
        // screen reader is not flooded.
        let percent = (i + 1) * 100 / total;
        if total > 3 && percent / 25 > announced && percent < 100 {
            announced = percent / 25;
            rep.send(Msg::Status(tf("save.progress", &[("percent", &(announced * 25))])));
        }
    }
    let joined = audio::concat(&pieces);
    match audio::save(&joined, &path, format) {
        Ok(()) => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            log::info!("saved audio ({} samples)", joined.samples.len());
            rep.send(Msg::Done(tf("save.saved", &[("name", &name)])));
        }
        Err(e) => rep.send(Msg::Failed(err(e))),
    }
}

/// Announces a long job's progress in quarters ("25% done"), and moves the
/// progress bar in steps of half a percent, so neither the screen reader nor
/// the window is flooded.
struct Quarters {
    key: &'static str,
    last_sent: f32,
    announced: usize,
}

impl Quarters {
    fn new(rep: &Reporter, key: &'static str) -> Self {
        rep.send(Msg::Progress(0.0));
        Self { key, last_sent: 0.0, announced: 0 }
    }

    fn report(&mut self, rep: &Reporter, fraction: f32) {
        if (fraction - self.last_sent).abs() >= 0.005 {
            self.last_sent = fraction;
            rep.send(Msg::Progress(fraction));
        }
        let quarter = (fraction * 4.0) as usize;
        if quarter > self.announced && quarter < 4 {
            self.announced = quarter;
            rep.send(Msg::Status(tf(self.key, &[("percent", &(quarter * 25))])));
        }
    }
}

/// Decodes an audio file for the Audio Player tab and checks whether it
/// sounds like speech. Escape stops it.
pub fn load_audio(rep: Reporter, control: Arc<Control>, path: PathBuf) {
    let result = audio::listen(&path, || control.is_stopped()).map(|a| a.map(Arc::new)).map_err(err);
    rep.send(Msg::AudioLoaded(path, result));
}

/// Plays an audio file from `start`, reporting the position as it goes.
/// The control pauses, moves and stops it.
pub fn play_audio(rep: Reporter, control: Arc<Control>, path: PathBuf, start: Duration) {
    use anyhow::Context;
    let result = (|| {
        let playback = Playback::open()?;
        let file = std::fs::File::open(&path).with_context(|| format!("could not open {}", path.display()))?;
        let decoder = rodio::Decoder::try_from(file).context("the audio could not be decoded")?;
        let player = playback.handle();
        player.append(decoder);
        if let Ok(mut slot) = control.player.lock() {
            *slot = Some(player.clone());
        }
        // Apply a pause pressed before the audio device was ready.
        control.set_paused(control.is_paused());
        if !start.is_zero() {
            control.seek(start);
        }
        let mut last_sent = None;
        // A move waits until the sound system makes it, which is for ever if
        // the output device has gone, so it happens on a thread of its own and
        // Stop always works. One move at a time, so they happen in order.
        let mut moving: Option<std::thread::JoinHandle<()>> = None;
        loop {
            if control.is_stopped() {
                return Ok(false);
            }
            if player.empty() {
                return Ok(true);
            }
            if moving.as_ref().is_some_and(|m| m.is_finished()) {
                moving = None;
            }
            if moving.is_none()
                && let Some(position) = control.take_seek()
            {
                let player = player.clone();
                moving = Some(std::thread::spawn(move || {
                    if let Err(e) = player.try_seek(position) {
                        log::warn!("could not move to {position:?} in the audio: {e}");
                    }
                }));
            }
            // Until a move is made, the position is still the old one.
            let position = player.get_pos();
            if moving.is_none() && last_sent != Some(position) {
                last_sent = Some(position);
                rep.send(Msg::AudioPosition(position));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    })();
    rep.send(Msg::AudioEnded(result.map_err(err)));
}

/// Downloads a Whisper model, moving the progress bar and announcing each
/// quarter. Escape stops it.
pub fn download_whisper(rep: Reporter, control: Arc<Control>, model: WhisperModel) {
    let mut quarters = Quarters::new(&rep, "model.download_progress");
    let result = crate::transcribe::download(model, |f| quarters.report(&rep, f), || control.is_stopped());
    rep.send(Msg::WhisperDownloaded(model, result.map_err(err)));
}

/// Downloads the models that tell speakers apart, moving the progress bar
/// and announcing each quarter. Escape stops it.
pub fn download_speaker_models(rep: Reporter, control: Arc<Control>) {
    let mut quarters = Quarters::new(&rep, "model.download_progress");
    let result = crate::speakers::download(|f| quarters.report(&rep, f), || control.is_stopped());
    rep.send(Msg::SpeakerModelsDownloaded(result.map_err(err)));
}

/// What transcribing an audio file made.
pub struct Transcript {
    pub text: String,
    /// How many words were heard, not counting speaker labels.
    pub words: usize,
    /// How many people were heard, when speakers were labelled.
    pub speakers: Option<usize>,
}

/// Share of the progress bar that recognising the words takes when the
/// speakers are found afterwards.
const WORDS_SHARE: f32 = 0.75;

/// Transcribes the speech in an audio file, and then, if asked, works out who
/// is speaking. Escape stops it.
pub fn transcribe(
    rep: Reporter,
    control: Arc<Control>,
    model: WhisperModel,
    path: PathBuf,
    audio: Arc<audio::Listened>,
    labels: SpeakerLabels,
) {
    let result = (|| {
        let labelling = !matches!(labels, SpeakerLabels::Off);
        let share = if labelling { WORDS_SHARE } else { 1.0 };
        let mut quarters = Quarters::new(&rep, "transcript.progress");
        let pieces = crate::transcribe::transcribe(
            model,
            &audio.pcm.samples,
            labelling,
            |percent| quarters.report(&rep, percent as f32 / 100.0 * share),
            || control.is_stopped(),
        )?;
        let Some(pieces) = pieces else { return Ok(None) };
        let words = pieces.iter().map(|p| p.2.split_whitespace().count()).sum();
        let count = match labels {
            // Nothing to label if nothing was said.
            _ if words == 0 => return Ok(Some(Transcript { text: String::new(), words, speakers: None })),
            SpeakerLabels::Off => {
                let text = crate::transcribe::join_segments(&pieces);
                return Ok(Some(Transcript { text, words, speakers: None }));
            }
            SpeakerLabels::Auto => None,
            SpeakerLabels::Count(n) => Some(n),
        };
        rep.send(Msg::Status(t("transcript.finding_speakers")));
        let turns = crate::speakers::find(
            audio,
            count,
            |f| quarters.report(&rep, WORDS_SHARE + f * (1.0 - WORDS_SHARE)),
            || control.is_stopped(),
        )?;
        let Some(turns) = turns else { return Ok(None) };
        let (text, speakers) = crate::speakers::label(&pieces, &turns);
        Ok(Some(Transcript { text, words, speakers: Some(speakers) }))
    })();
    rep.send(Msg::Transcribed(path, result.map_err(err)));
}

/// What a translation job made.
pub struct Translated {
    pub language: Language,
    pub translation: Translation,
    /// How many pieces of text are still in English.
    pub left: usize,
    /// The job was stopped before it finished.
    pub stopped: bool,
}

/// Translates the pieces of interface text that `translation` is missing,
/// a batch at a time, with the Ollama model `model`. What has been translated
/// is saved even if the job is stopped, so the next run carries on from there.
/// Escape takes effect between batches.
pub fn translate(rep: Reporter, control: Arc<Control>, language: Language, model: String, mut translation: Translation) {
    let keys = translation.missing();
    let batches: Vec<&[&str]> = keys.chunks(i18n::BATCH_SIZE).collect();
    log::info!("translating {} piece(s) of text into {} with {model}", keys.len(), language.code);
    translation.made_with = model.clone();
    rep.send(Msg::Progress(0.0));
    let mut announced = 0;
    let mut failed_batches = 0;
    let mut last_error = None;
    let mut stopped = false;
    for (i, batch) in batches.iter().enumerate() {
        if control.is_stopped() {
            stopped = true;
            break;
        }
        // Small models sometimes reply with something that isn't JSON, so
        // each batch gets a second try.
        let result = i18n::translate_batch(&model, &language, batch).or_else(|e| {
            log::warn!("translation batch {} failed, trying again: {e:#}", i + 1);
            if control.is_stopped() { Err(e) } else { i18n::translate_batch(&model, &language, batch) }
        });
        match result {
            Ok(done) => translation.strings.extend(done),
            Err(e) => {
                failed_batches += 1;
                // Every batch so far has failed: Ollama is probably not
                // running, or the model can't do this. Stop wasting time.
                if failed_batches == i + 1 && failed_batches >= 2 {
                    return rep.send(Msg::Translated(Err(err(e))));
                }
                log::warn!("translation batch {} failed: {e:#}", i + 1);
                last_error = Some(e);
            }
        }
        let fraction = (i + 1) as f32 / batches.len() as f32;
        rep.send(Msg::Progress(fraction));
        let quarter = (fraction * 4.0) as usize;
        if quarter > announced && quarter < 4 {
            announced = quarter;
            rep.send(Msg::Status(tf("language.progress", &[("percent", &(quarter * 25))])));
        }
    }
    // Nothing worked at all (a single batch that failed twice): say why,
    // rather than report a translation with nothing in it.
    if !stopped && failed_batches > 0 && failed_batches == batches.len()
        && let Some(e) = last_error
    {
        return rep.send(Msg::Translated(Err(err(e))));
    }
    if let Err(e) = i18n::save(&crate::paths::languages_dir(), &language.code, &translation) {
        return rep.send(Msg::Translated(Err(err(e))));
    }
    let left = translation.missing().len();
    rep.send(Msg::Translated(Ok(Translated { language, translation, left, stopped })));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_messages_are_plain() {
        let secs = Duration::from_secs;
        assert_eq!(
            retry_message(Provider::ElevenLabs, Kind::Unavailable { retry_after: None }, secs(4)),
            "ElevenLabs is busy. Trying again in 4 seconds. Press Escape to stop."
        );
        assert_eq!(
            retry_message(Provider::OpenAi, Kind::RateLimited { retry_after: None }, secs(1)),
            "OpenAI is limiting how fast requests can be made. Trying again in 1 second. Press Escape to stop."
        );
        assert_eq!(
            retry_message(Provider::Deepgram, Kind::Unreachable, Duration::from_millis(300)),
            "Could not reach Deepgram. Trying again in 1 second. Press Escape to stop."
        );
    }

    #[test]
    fn waiting_ends_early_when_stopped() {
        let control = Control::default();
        control.stop();
        let start = Instant::now();
        assert!(!control.wait(Duration::from_secs(30), || ()));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn waiting_runs_its_course_otherwise() {
        let control = Control::default();
        let mut ticks = 0;
        assert!(control.wait(Duration::from_millis(250), || ticks += 1));
        assert!(ticks >= 2);
    }
}
