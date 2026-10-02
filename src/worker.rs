//! Background jobs. Anything slow (network, synthesis, file parsing) runs on
//! its own thread and reports back to the UI through a channel, so the
//! interface never freezes and the screen reader is kept informed.

use crate::audio::{self, AudioFormat, Playback};
use crate::speech::{self, Provider, Voice};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub enum Msg {
    Voices(Provider, Result<Vec<Voice>, String>),
    Models(Result<Vec<String>, String>),
    Loaded(PathBuf, Result<String, String>),
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
}

pub fn load_voices(rep: Reporter, provider: Provider, key: Option<String>) {
    let result = speech::list_voices(provider, key.as_deref()).map_err(err);
    rep.send(Msg::Voices(provider, result));
}

pub fn load_models(rep: Reporter) {
    rep.send(Msg::Models(crate::vision::list_models().map_err(err)));
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
    pub text: String,
}

impl SpeechJob {
    fn render(&self, chunk: &str) -> anyhow::Result<audio::Pcm> {
        let bytes = speech::synthesize(self.provider, self.key.as_deref(), &self.voice, chunk)?;
        audio::decode(bytes)
    }
}

/// Reads the text aloud, synthesising at most two pieces ahead of playback so
/// that stopping early does not waste cloud credit.
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
    let chunks = speech::chunk_text(job.provider, &job.text);
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
            return rep.send(Msg::Done("Stopped reading.".into()));
        }
        match job.render(chunk) {
            Ok(pcm) => {
                if control.is_stopped() {
                    return rep.send(Msg::Done("Stopped reading.".into()));
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
        rep.send(Msg::Done("Stopped reading.".into()));
    } else {
        rep.send(Msg::Done("Finished reading.".into()));
    }
}

pub fn save(rep: Reporter, control: Arc<Control>, job: SpeechJob, path: PathBuf, format: AudioFormat) {
    let chunks = speech::chunk_text(job.provider, &job.text);
    let total = chunks.len();
    let mut pieces = Vec::with_capacity(total);
    let mut announced = 0;
    rep.send(Msg::Progress(0.0));
    for (i, chunk) in chunks.iter().enumerate() {
        if control.is_stopped() {
            return rep.send(Msg::Done("Cancelled saving audio.".into()));
        }
        match job.render(chunk) {
            Ok(pcm) => pieces.push(pcm),
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
