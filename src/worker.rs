//! Background jobs. Anything slow (network, synthesis, file parsing) runs on
//! its own thread and reports back to the UI through a channel, so the
//! interface never freezes and the screen reader is kept informed.

use crate::audio::{self, AudioFormat, Playback};
use crate::speech::retry::{self, Kind};
use crate::speech::{self, Provider, Voice};
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
            rep.send(Msg::Status("Ollama is installed. Starting it.".into()));
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
                rep.send(Msg::Status(format!("Downloading the AI model, {}% done.", quarter * 25)));
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
        Kind::RateLimited { .. } => format!("{name} is limiting how fast requests can be made"),
        Kind::Unavailable { .. } => format!("{name} is busy"),
        _ => format!("Could not reach {name}"),
    };
    let secs = delay.as_secs().max(1);
    let unit = if secs == 1 { "second" } else { "seconds" };
    format!("{problem}. Trying again in {secs} {unit}. Press Escape to stop.")
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
        ("Stopped the preview.", "Finished the preview.")
    } else {
        ("Stopped reading.", "Finished reading.")
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
            return rep.send(Msg::Done(stopped.into()));
        }
        match job.render(&rep, &control, chunk, || progress.report(&rep, &playback)) {
            Ok(None) => return rep.send(Msg::Done(stopped.into())),
            Ok(Some(pcm)) => {
                if control.is_stopped() {
                    return rep.send(Msg::Done(stopped.into()));
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
        rep.send(Msg::Done(stopped.into()));
    } else {
        rep.send(Msg::Done(finished.into()));
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
            return rep.send(Msg::Done("Cancelled saving audio.".into()));
        }
        match job.render(&rep, &control, chunk, || ()) {
            Ok(None) => return rep.send(Msg::Done("Cancelled saving audio.".into())),
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
            rep.send(Msg::Status(format!("Preparing audio, {}% done.", announced * 25)));
        }
    }
    let joined = audio::concat(&pieces);
    match audio::save(&joined, &path, format) {
        Ok(()) => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            log::info!("saved audio ({} samples)", joined.samples.len());
            rep.send(Msg::Done(format!("Saved the audio as {name}.")));
        }
        Err(e) => rep.send(Msg::Failed(err(e))),
    }
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
