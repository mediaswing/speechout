//! Background jobs. Anything slow (network, synthesis, file parsing) runs on
//! its own thread and reports back to the UI through a channel, so the
//! interface never freezes and the screen reader is kept informed.

use crate::audio::{self, AudioFormat, Playback};
use crate::document::DocumentReading;
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
    Loaded(PathBuf, Result<LoadedFile, String>),
    /// Downloading a model finished: true if complete, false if stopped.
    ModelDownloaded(String, Result<bool, String>),
    /// Installing and/or starting Ollama finished.
    OllamaReady(Result<(), String>),
    /// Translating the interface finished or was stopped.
    Translated(Result<Translated, String>),
    /// An audio file for the Audio Player tab was decoded and checked:
    /// `None` if opening it was stopped.
    AudioLoaded(PathBuf, Result<Option<Arc<audio::Listened>>, String>),
    /// A zip file for the Audio Player tab was unpacked: `None` if opening it
    /// was stopped.
    PlaylistOpened(PathBuf, Result<Option<crate::playlist::Playlist>, String>),
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
    /// Installing ffmpeg finished, with where it now is.
    FfmpegReady(Result<PathBuf, String>),
    /// A picture from the video being described is about to be described.
    VideoFrame(VideoFrame),
    /// Describing a video finished: `None` if it was stopped.
    VideoDescribed(PathBuf, Result<Option<VideoDescription>, String>),
    /// How long the video being played is. Each video message starts with
    /// the number of the play it comes from.
    VideoLength(u64, Duration),
    /// How far into the video playback is, with the picture to show then
    /// if there is a new one.
    VideoPlaying(u64, Duration, Option<egui::ColorImage>),
    /// Playing the video ended: true if it reached the end.
    VideoPlayEnded(u64, Result<bool, String>),
    /// Result of an update check; `bool` is true when the user asked for it.
    Update(Result<Option<crate::update::Release>, String>, bool),
    /// Progress worth announcing.
    Status(String),
    /// How far through the current job we are, from 0.0 to 1.0.
    Progress(f32),
    /// A read-aloud or save job ended normally: true if it got to the end,
    /// false if it was stopped.
    Done(String, bool),
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

/// Runs `work`, which reads a file the user chose, turning a crash in it into
/// an error. Otherwise a damaged file that crashed a reader would leave the
/// window waiting for an answer that never comes.
fn contain<T>(work: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<T> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or_else(|payload| {
        let detail = payload
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_default();
        log::error!("a background job crashed: {detail}");
        Err(anyhow::anyhow!("the file could not be read. It may be damaged."))
    })
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
    /// The playing video's pictures aren't on screen, so needn't be sent.
    pictures_hidden: AtomicBool,
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

    /// Says whether the playing video's pictures are off screen.
    pub fn set_pictures_hidden(&self, hidden: bool) {
        self.pictures_hidden.store(hidden, Ordering::SeqCst);
    }

    fn pictures_hidden(&self) -> bool {
        self.pictures_hidden.load(Ordering::SeqCst)
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
    rep.send(Msg::Models(crate::ollama::list_models().map_err(err)));
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
        if !crate::ollama::wait_until_running(Duration::from_secs(2)) {
            crate::platform::start_ollama()?;
            if !crate::ollama::wait_until_running(Duration::from_secs(60)) {
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
    let mut quarters = Quarters::new(&rep, "model.download_progress");
    let result = crate::ollama::pull_model(&model, |f| quarters.report(&rep, f), || control.is_stopped());
    rep.send(Msg::ModelDownloaded(model, result.map_err(err)));
}

pub fn check_for_update(rep: Reporter, requested: bool) {
    let result = crate::update::check().map_err(err);
    rep.send(Msg::Update(result, requested));
}

pub fn load_file(
    rep: Reporter,
    path: PathBuf,
    model: String,
    resolve_location: bool,
    read_camera: bool,
    reading: DocumentReading,
) {
    let result = read_file(&path, &model, resolve_location, read_camera, reading);
    rep.send(Msg::Loaded(path, result));
}

/// What was read from a file.
pub struct LoadedFile {
    /// The text of a document, or the description of a photo.
    pub text: String,
    /// Whether the text must be read with the system voices, as for a PNG
    /// screenshot. Worked out from what the file held, not just its name.
    pub system_voices_only: bool,
    /// The model that described a photo, which is another one if the one
    /// chosen couldn't be used.
    pub vision_model: Option<String>,
}

/// The text of a document, or the description of a photo. `reading` says
/// whether a DOCX, ODT or Markdown file's formatting is read out too.
pub fn read_file(
    path: &std::path::Path,
    model: &str,
    resolve_location: bool,
    read_camera: bool,
    reading: DocumentReading,
) -> Result<LoadedFile, String> {
    use crate::document::FileKind;
    let result = contain(|| match FileKind::from_path(path) {
        Some(FileKind::Image) => crate::vision::describe(path, model, resolve_location, read_camera).map(|d| LoadedFile {
            text: d.text,
            system_voices_only: d.was_png || crate::document::system_voices_only(path),
            vision_model: Some(d.model),
        }),
        _ => crate::document::extract_text(path, reading)
            .map(|text| LoadedFile { text, system_voices_only: false, vision_model: None }),
    });
    result.map_err(err)
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
            return rep.send(Msg::Done(stopped, false));
        }
        match job.render(&rep, &control, chunk, || progress.report(&rep, &playback)) {
            Ok(None) => return rep.send(Msg::Done(stopped, false)),
            Ok(Some(pcm)) => {
                if control.is_stopped() {
                    return rep.send(Msg::Done(stopped, false));
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
        rep.send(Msg::Done(stopped, false));
    } else {
        rep.send(Msg::Done(finished, true));
    }
}

pub fn save(rep: Reporter, control: Arc<Control>, job: SpeechJob, path: PathBuf, format: AudioFormat, tags: audio::Tags) {
    let chunks = job.pieces();
    let total = chunks.len();
    let mut pieces = Vec::with_capacity(total);
    let mut announced = 0;
    rep.send(Msg::Progress(0.0));
    for (i, chunk) in chunks.iter().enumerate() {
        if control.is_stopped() {
            return rep.send(Msg::Done(t("save.cancelled"), false));
        }
        match job.render(&rep, &control, chunk, || ()) {
            Ok(None) => return rep.send(Msg::Done(t("save.cancelled"), false)),
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
    match audio::save(&joined, &path, format, &tags) {
        Ok(()) => {
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            log::info!("saved audio ({} samples)", joined.samples.len());
            rep.send(Msg::Done(tf("save.saved", &[("name", &name)]), true));
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
    let result = contain(|| audio::listen(&path, || control.is_stopped())).map(|a| a.map(Arc::new)).map_err(err);
    rep.send(Msg::AudioLoaded(path, result));
}

/// Unpacks a zip file of audio to play as a playlist. Escape stops it.
pub fn open_playlist(rep: Reporter, control: Arc<Control>, path: PathBuf) {
    let result = contain(|| crate::playlist::open(&path, || control.is_stopped())).map_err(err);
    rep.send(Msg::PlaylistOpened(path, result));
}

/// Plays an audio file from `start`, reporting the position as it goes.
/// The control pauses, moves and stops it. Holding `audio` keeps the WAV
/// that ffmpeg made from a WMA file until playing it has stopped.
pub fn play_audio(rep: Reporter, control: Arc<Control>, path: PathBuf, audio: Arc<audio::Listened>, start: Duration) {
    use anyhow::Context;
    let result = contain(|| {
        let playback = Playback::open()?;
        let playable = audio.playable(&path);
        let file = std::fs::File::open(playable).with_context(|| format!("could not open {}", path.display()))?;
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
    });
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
    let result = contain(|| {
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
    });
    rep.send(Msg::Transcribed(path, result.map_err(err)));
}

/// Installs ffmpeg with the package manager, and checks it can be found.
pub fn install_ffmpeg(rep: Reporter) {
    let result = crate::platform::install_ffmpeg().and_then(|()| {
        log::info!("installed ffmpeg");
        crate::platform::ffmpeg().ok_or_else(|| anyhow::anyhow!("ffmpeg was installed but could not be found"))
    });
    rep.send(Msg::FfmpegReady(result.map_err(err)));
}

/// A picture from a video, for the preview.
pub struct VideoFrame {
    pub video: PathBuf,
    /// Counting from 1.
    pub number: usize,
    pub count: usize,
    pub time: Duration,
    /// `None` if it couldn't be shown.
    pub image: Option<egui::ColorImage>,
}

/// What describing a video made.
pub struct VideoDescription {
    pub text: String,
    /// How many pictures were described.
    pub frames: usize,
    /// How long the video is, if ffmpeg could tell.
    pub length: Option<Duration>,
    /// The model that described the pictures, which is another one if the
    /// one chosen couldn't be used.
    pub model: String,
}

/// Share of the progress bar that taking out the pictures takes. Describing
/// them is far slower.
const KEYFRAMES_SHARE: f32 = 0.1;
/// The preview is drawn no larger than this.
const PREVIEW_EDGE: u32 = 800;

/// Takes the keyframes out of a video into `dir` with ffmpeg, then describes
/// each with the image description model `model`. Escape stops it, between
/// pictures once they are being described.
pub fn describe_video(rep: Reporter, control: Arc<Control>, ffmpeg: PathBuf, video: PathBuf, dir: PathBuf, mut model: String) {
    let result = contain(|| {
        let mut quarters = Quarters::new(&rep, "video.progress");
        let keyframes = crate::video::keyframes(
            &ffmpeg,
            &video,
            &dir,
            |f| quarters.report(&rep, f * KEYFRAMES_SHARE),
            || control.is_stopped(),
        )?;
        let Some(crate::video::Keyframes { frames, length }) = keyframes else { return Ok(None) };
        if frames.is_empty() {
            anyhow::bail!("no pictures could be taken from this video");
        }
        let count = frames.len();
        rep.send(Msg::Status(if count == 1 { t("video.found_one") } else { tf("video.found", &[("count", &count)]) }));
        let mut described = Vec::with_capacity(count);
        for (i, frame) in frames.iter().enumerate() {
            if control.is_stopped() {
                return Ok(None);
            }
            let image = preview_image(&frame.path)
                .inspect_err(|e| log::warn!("could not show picture {}: {e:#}", i + 1))
                .ok();
            rep.send(Msg::VideoFrame(VideoFrame { video: video.clone(), number: i + 1, count, time: frame.time, image }));
            let description = crate::vision::describe(&frame.path, &model, false, false)?;
            // The rest go straight to a model that works.
            model = description.model;
            described.push((frame.time, description.text));
            quarters.report(&rep, KEYFRAMES_SHARE + (i + 1) as f32 / count as f32 * (1.0 - KEYFRAMES_SHARE));
        }
        Ok(Some(VideoDescription { text: crate::video::join_descriptions(&described), frames: count, length, model }))
    });
    rep.send(Msg::VideoDescribed(video, result.map_err(err)));
}

/// How a stretch of playing a video ended.
enum Played {
    End,
    Stopped,
    /// The user moved to another place in it.
    Moved(Duration),
}

/// Plays a video, pictures and sound, from `start`, while its description is
/// edited. Moving to another place starts ffmpeg again from there. `play`
/// numbers this play in the messages it sends. The video's length is only
/// found out if `length` doesn't already give it.
pub fn play_video(
    rep: Reporter,
    control: Arc<Control>,
    play: u64,
    ffmpeg: PathBuf,
    video: PathBuf,
    start: Duration,
    length: Option<Duration>,
) {
    let result = contain(|| {
        if length.is_none()
            && let Some(length) = crate::video::probe(&ffmpeg, &video)?
        {
            rep.send(Msg::VideoLength(play, length));
        }
        let mut from = start;
        loop {
            match play_video_from(&rep, &control, play, &ffmpeg, &video, from)? {
                Played::End => return Ok(true),
                Played::Stopped => return Ok(false),
                Played::Moved(to) => from = to,
            }
        }
    });
    rep.send(Msg::VideoPlayEnded(play, result.map_err(err)));
}

/// Sets a flag when dropped, however the function holding it returns.
struct SetOnDrop(Arc<AtomicBool>);

impl Drop for SetOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// Plays the video from `from` until it ends, is stopped or is moved.
fn play_video_from(
    rep: &Reporter,
    control: &Control,
    play: u64,
    ffmpeg: &std::path::Path,
    video: &std::path::Path,
    from: Duration,
) -> anyhow::Result<Played> {
    use crate::video::{PLAY_FPS, PLAY_HEIGHT, PLAY_WIDTH};
    use anyhow::Context;
    use std::io::Read;
    use std::sync::mpsc::{TryRecvError, sync_channel};

    // The pictures are read on a thread of their own, a couple ahead, so a
    // slow ffmpeg never holds up Stop. Dropping `pictures` stops ffmpeg,
    // which ends the thread. Each picture's memory comes back through
    // `spare` once used, to be read into again.
    let mut pictures = crate::video::play_pictures(ffmpeg, video, from)?;
    let mut output = pictures.output().context("could not run ffmpeg")?;
    let (tx, rx) = sync_channel::<Vec<u8>>(2);
    let (spare, spares) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        loop {
            let mut picture = spares.try_recv().unwrap_or_else(|_| vec![0; PLAY_WIDTH * PLAY_HEIGHT * 4]);
            if output.read_exact(&mut picture).is_err() || tx.send(picture).is_err() {
                break;
            }
        }
    });

    // The sound, if there is a device to play it on. The video can still be
    // watched without one.
    let playback = Playback::open().inspect_err(|e| log::warn!("the video will play without sound: {e:#}")).ok();
    let player = playback.as_ref().map(Playback::handle);
    // Set once the sound has started arriving, and once it has all arrived.
    let sound_ready = Arc::new(AtomicBool::new(player.is_none()));
    let sound_done = Arc::new(AtomicBool::new(player.is_none()));
    let finished = Arc::new(AtomicBool::new(false));
    let _finish = SetOnDrop(finished.clone());
    if let Some(player) = &player {
        // Held until the picture is ready too, so they start together.
        player.pause();
        let mut sound = crate::video::play_sound(ffmpeg, video, from)?;
        let (player, ready, done) = (player.clone(), sound_ready.clone(), sound_done.clone());
        std::thread::spawn(move || {
            let _done = SetOnDrop(done);
            let _ready = SetOnDrop(ready.clone());
            crate::video::feed_sound(&mut sound, &player, &ready, &finished);
        });
    }
    // Set even when there is no sound, so Pause and Stop never reach the
    // last stretch's player.
    if let Ok(mut slot) = control.player.lock() {
        *slot = player.clone();
    }

    let picture_time = |n: u32| from + Duration::from_secs_f64(f64::from(n) / PLAY_FPS);
    let late = Duration::from_secs_f64(2.0 / PLAY_FPS);
    let to_image = |rgba: &[u8]| egui::ColorImage::from_rgba_unmultiplied([PLAY_WIDTH, PLAY_HEIGHT], rgba);

    // Wait for the first picture and the start of the sound.
    let waiting = Instant::now();
    let mut first = None;
    let mut pictures_done = false;
    while waiting.elapsed() < Duration::from_secs(10) {
        if control.is_stopped() {
            return Ok(Played::Stopped);
        }
        if let Some(to) = control.take_seek() {
            return Ok(Played::Moved(to));
        }
        if first.is_none() && !pictures_done {
            match rx.try_recv() {
                Ok(picture) => first = Some(picture),
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => pictures_done = true,
            }
        }
        if (first.is_some() || pictures_done) && sound_ready.load(Ordering::SeqCst) {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    if let Some(picture) = &first
        && !control.pictures_hidden()
    {
        rep.send(Msg::VideoPlaying(play, from, Some(to_image(picture))));
    }
    // If the first picture never came in time, the next to arrive is still
    // the first, and is timed as such.
    let mut shown = u32::from(first.is_some());
    let mut clock = crate::video::Clock::new(from, control.is_paused());
    control.set_paused(control.is_paused());

    let mut last_sent = from;
    loop {
        if control.is_stopped() {
            return Ok(Played::Stopped);
        }
        if let Some(to) = control.take_seek() {
            return Ok(Played::Moved(to));
        }
        clock.set_paused(control.is_paused());
        let now = clock.now();
        if !pictures_done && now >= picture_time(shown) {
            match rx.try_recv() {
                Ok(picture) => {
                    let time = picture_time(shown);
                    shown += 1;
                    // A picture already too late is skipped, so playback
                    // catches up rather than falling further behind. One
                    // that can't be seen isn't sent at all.
                    if now <= time + late && !control.pictures_hidden() {
                        last_sent = now;
                        rep.send(Msg::VideoPlaying(play, now, Some(to_image(&picture))));
                    }
                    let _ = spare.send(picture);
                    continue;
                }
                Err(TryRecvError::Empty) => {}
                Err(TryRecvError::Disconnected) => pictures_done = true,
            }
        }
        if pictures_done && sound_done.load(Ordering::SeqCst) && player.as_ref().is_none_or(|p| p.empty()) {
            return Ok(Played::End);
        }
        // The position is sent even without a new picture, as when the
        // pictures have ended before the sound.
        if now.abs_diff(last_sent) >= Duration::from_millis(250) {
            last_sent = now;
            rep.send(Msg::VideoPlaying(play, now, None));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A picture made small enough to draw quickly.
fn preview_image(path: &std::path::Path) -> anyhow::Result<egui::ColorImage> {
    let mut img = image::open(path)?;
    if img.width() > PREVIEW_EDGE || img.height() > PREVIEW_EDGE {
        img = img.resize(PREVIEW_EDGE, PREVIEW_EDGE, image::imageops::FilterType::Triangle);
    }
    let rgba = img.into_rgba8();
    let size = [rgba.width() as usize, rgba.height() as usize];
    Ok(egui::ColorImage::from_rgba_unmultiplied(size, rgba.as_raw()))
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

    /// What `Quarters` sends for each fraction in turn: the progress bar's
    /// position, or the percentage announced.
    fn quarters_sent(fractions: &[f32]) -> Vec<String> {
        let (tx, rx) = std::sync::mpsc::channel();
        let rep = Reporter::new(tx, egui::Context::default());
        let mut quarters = Quarters::new(&rep, "model.download_progress");
        for fraction in fractions {
            quarters.report(&rep, *fraction);
        }
        drop((rep, quarters));
        rx.into_iter()
            .map(|msg| match msg {
                Msg::Progress(f) => format!("bar {f}"),
                // Only the number, which is the same in any language.
                Msg::Status(s) => format!("said {}", s.split(|c: char| !c.is_ascii_digit()).find(|n| !n.is_empty()).unwrap()),
                _ => panic!("unexpected message"),
            })
            .collect()
    }

    #[test]
    fn long_jobs_announce_each_quarter_once() {
        assert_eq!(
            quarters_sent(&[0.001, 0.1, 0.26, 0.26, 0.9, 1.0]),
            [
                // The bar starts at nothing; less than half a percent more
                // isn't worth moving it for.
                "bar 0",
                "bar 0.1",
                "bar 0.26",
                "said 25",
                // A quarter skipped over is not announced late.
                "bar 0.9",
                "said 75",
                // Done is said by the job itself, not as "100% done".
                "bar 1",
            ]
        );
    }

    /// Plays a three-second video made with ffmpeg, which must be installed:
    /// from a second in to the end, then moved forward and stopped.
    #[test]
    #[ignore]
    fn plays_a_video() {
        let ffmpeg = crate::platform::ffmpeg().expect("ffmpeg is installed");
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("test.mp4");
        // Silent sound, so nothing is heard, but there is sound to play.
        let made = std::process::Command::new(&ffmpeg)
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=480x854:rate=30:duration=3"])
            .args(["-f", "lavfi", "-i", "anullsrc=r=44100:cl=mono", "-t", "3", "-pix_fmt", "yuv420p"])
            .arg(&video)
            .status()
            .unwrap();
        assert!(made.success());

        let play = |start: Duration, act: &dyn Fn(&Control, Duration)| {
            let (tx, rx) = std::sync::mpsc::channel();
            let control = Arc::new(Control::default());
            let rep = Reporter::new(tx, egui::Context::default());
            let (job_control, ffmpeg, video) = (control.clone(), ffmpeg.clone(), video.clone());
            let started = Instant::now();
            std::thread::spawn(move || play_video(rep, job_control, 1, ffmpeg, video, start, None));
            let (mut length, mut pictures, mut positions) = (None, 0, Vec::new());
            loop {
                match rx.recv_timeout(Duration::from_secs(20)).expect("the video ends") {
                    Msg::VideoLength(_, l) => length = Some(l),
                    Msg::VideoPlaying(_, position, picture) => {
                        pictures += usize::from(picture.is_some());
                        positions.push(position);
                        act(&control, position);
                    }
                    Msg::VideoPlayEnded(_, result) => {
                        return (result.unwrap(), length, pictures, positions, started.elapsed());
                    }
                    _ => {}
                }
            }
        };

        let (ended, length, pictures, positions, took) = play(Duration::from_secs(1), &|_, _| {});
        assert!(ended, "it plays to the end");
        let length = length.unwrap().as_secs_f64();
        assert!((length - 3.0).abs() < 0.1, "{length}");
        // Two seconds at 15 pictures a second, less any skipped to keep up.
        assert!((20..=31).contains(&pictures), "{pictures}");
        assert!(positions.windows(2).all(|p| p[0] <= p[1]), "{positions:?}");
        assert_eq!(positions[0], Duration::from_secs(1));
        assert!(took > Duration::from_millis(1800) && took < Duration::from_secs(5), "{took:?}");

        // Moved to 2.5 seconds as soon as it starts, then stopped there.
        let (ended, _, _, positions, _) = play(Duration::ZERO, &|control, position| {
            if position < Duration::from_secs(2) {
                control.seek(Duration::from_millis(2500));
            } else {
                control.stop();
            }
        });
        assert!(!ended, "it was stopped");
        assert!(positions.iter().any(|p| *p >= Duration::from_millis(2500)), "{positions:?}");
    }

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
    fn a_crash_becomes_an_error() {
        let result: anyhow::Result<()> = contain(|| panic!("bad file"));
        assert_eq!(err(result.unwrap_err()), "The file could not be read. It may be damaged.");
        assert_eq!(contain(|| Ok(5)).unwrap(), 5);
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
