//! The slow work, on background threads, so the window keeps answering the
//! keyboard and the screen reader: opening files, describing images, finding
//! voices, speaking and saving, playing audio files and transcribing them. Each
//! job reports back with a [`Msg`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;

use crate::audio::{self, Playback};
use crate::document::{self, FileKind};
use crate::speech::{self, Provider, Voice};
use crate::store::AiKind;
use crate::transcribe::{self, Heard, whisper};
use crate::update::{self, Version};
use crate::vision::{self, LocalAi};
use crate::wordlist::{Substitutions, Wordlist};

pub enum Msg {
    Voices(Provider, Result<Vec<Voice>, String>),
    /// The models on the local AI server, and whether the user asked for them.
    Models(Result<Vec<String>, String>, bool),
    /// A file's text or description. The number says which request it answers.
    Loaded(u64, Result<String, String>),
    /// What is happening with a file being opened, numbered in the same way.
    LoadStatus(u64, String),
    Progress(f32),
    /// Reading aloud or saving finished. The text is for the status bar.
    Done(String),
    /// Reading aloud or saving failed. Other work reports its own failures,
    /// so that one can never be taken for another.
    Failed(String),
    /// An audio file, heard and judged, numbered like `Loaded`.
    Heard(u64, Result<Arc<Heard>, String>),
    /// The audio player finished: whether it got to the end, rather than being
    /// stopped. The number says which time Play was pressed.
    Played(u64, Result<bool, String>),
    /// What the transcription is doing now, for the status bar. Each
    /// transcription's messages carry its number, so that one stopped when a
    /// new file was opened can't be taken for the new file's.
    TranscribeStage(u64, String),
    TranscribeProgress(u64, f32),
    /// A transcript, or `None` if stopped. An error is ready for the status bar.
    Transcribed(u64, Result<Option<String>, String>),
    /// A newer version, if there is one, and whether the user asked.
    Update(Result<Option<Version>, String>, bool),
}

/// Sends messages to the window and wakes it up to read them.
#[derive(Clone)]
pub struct Reporter {
    tx: Sender<Msg>,
    ctx: egui::Context,
}

impl Reporter {
    pub fn new(tx: Sender<Msg>, ctx: egui::Context) -> Self {
        Self { tx, ctx }
    }

    fn send(&self, msg: Msg) {
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }

    /// Runs `job` on a new thread. If it panics, `on_panic` makes the answer
    /// the window is waiting for, so it isn't left waiting forever.
    fn spawn_or(&self, job: impl FnOnce(&Reporter) + Send + 'static, on_panic: impl FnOnce() -> Msg + Send + 'static) {
        let rep = self.clone();
        std::thread::spawn(move || {
            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&rep))).is_err() {
                rep.send(on_panic());
            }
        });
    }
}

const PANIC: &str = "Something went wrong. The debug log has the details.";

/// An error as a sentence for the status bar and screen readers.
pub fn sentence(error: &anyhow::Error) -> String {
    let text = format!("{error:#}");
    let mut chars = text.chars();
    let mut out: String = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => "Something went wrong".to_owned(),
    };
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

fn logged(what: &str, result: anyhow::Result<String>) -> Result<String, String> {
    result.map_err(|e| {
        log::warn!("{what} failed: {e:#}");
        sentence(&e)
    })
}

/// Lets the window pause or stop a job while it runs. Pausing acts on the
/// player directly, so it works at once, even while the next piece of speech
/// is still being made.
#[derive(Default)]
pub struct Control {
    stop: AtomicBool,
    paused: AtomicBool,
    player: Mutex<Option<Arc<rodio::Player>>>,
}

impl Control {
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Ok(player) = self.player.lock()
            && let Some(player) = player.as_ref()
        {
            player.stop();
        }
    }

    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
        if let Ok(player) = self.player.lock()
            && let Some(player) = player.as_ref()
        {
            if paused { player.pause() } else { player.play() }
        }
    }

    /// How far through the player is.
    pub fn position(&self) -> Option<Duration> {
        self.player.lock().ok()?.as_ref().map(|p| p.get_pos())
    }

    fn attach(&self, player: Arc<rodio::Player>) {
        if self.paused.load(Ordering::SeqCst) {
            player.pause();
        }
        if let Ok(mut slot) = self.player.lock() {
            *slot = Some(player);
        }
    }
}

/// What to say, and who says it.
#[derive(Clone)]
pub struct SpeechJob {
    pub text: Arc<String>,
    pub provider: Provider,
    pub key: String,
    pub voice: String,
    /// The wordlists that are turned on.
    pub wordlists: Vec<Wordlist>,
}

impl SpeechJob {
    /// The text as it should be spoken, with the wordlists applied.
    fn spoken(&self) -> anyhow::Result<String> {
        Ok(Substitutions::new(&self.wordlists)?.apply(&self.text))
    }

    /// The text as it should be spoken, in pieces the service will accept.
    fn pieces(&self) -> anyhow::Result<Vec<String>> {
        Ok(speech::split_text(&self.spoken()?, self.provider))
    }

    /// The text as it should be spoken, in parts of at most
    /// [`speech::PART_CHARS`] characters, one for each MP3 file, and each part
    /// in pieces the service will accept.
    fn parts(&self) -> anyhow::Result<Vec<Vec<String>>> {
        Ok(speech::split_into_parts(&self.spoken()?).iter().map(|part| speech::split_text(part, self.provider)).collect())
    }

    /// The files that saving as `path` will write: `path` itself, or a
    /// numbered file for each part when the text is long and not stitched.
    pub fn output_paths(&self, path: &Path, stitch: bool) -> anyhow::Result<Vec<PathBuf>> {
        Ok(output_paths(path, self.parts()?.len(), stitch))
    }

    fn sound_of(&self, piece: &str) -> anyhow::Result<audio::Pcm> {
        let bytes = speech::synthesize(self.provider, &self.key, &self.voice, piece)?;
        audio::decode(bytes)
    }
}

pub fn load_voices(rep: &Reporter, provider: Provider, key: String) {
    rep.spawn_or(
        move |rep| {
            let voices = speech::list_voices(provider, &key).map_err(|e| {
                log::warn!("Listing {} voices failed: {e:#}", provider.label());
                sentence(&e)
            });
            rep.send(Msg::Voices(provider, voices));
        },
        move || Msg::Voices(provider, Err(PANIC.to_owned())),
    );
}

pub fn list_models(rep: &Reporter, kind: AiKind, address: String, requested: bool) {
    rep.spawn_or(
        move |rep| {
            let models = vision::list_models(kind, &address).map_err(|e| {
                log::info!("Listing local AI models failed: {e:#}");
                sentence(&e)
            });
            rep.send(Msg::Models(models, requested));
        },
        move || Msg::Models(Err(PANIC.to_owned()), requested),
    );
}

/// Asks GitHub whether there is a newer version. `requested` is false for the
/// check when the app starts.
pub fn check_for_update(rep: &Reporter, requested: bool) {
    rep.spawn_or(
        move |rep| {
            let result = update::latest().map(update::newer).map_err(|e| {
                log::info!("Checking for updates failed: {e:#}");
                sentence(&e)
            });
            rep.send(Msg::Update(result, requested));
        },
        move || Msg::Update(Err(PANIC.to_owned()), requested),
    );
}

/// Reads a document's text, or describes an image.
pub fn load_file(rep: &Reporter, id: u64, path: PathBuf, kind: FileKind, ai: LocalAi, resolve_gps: bool) {
    let job = move |rep: &Reporter| {
        log::info!("Opening a {kind:?} file");
        let started = AtomicBool::new(false);
        let on_starting = || {
            started.store(true, Ordering::SeqCst);
            rep.send(Msg::LoadStatus(id, "Starting Ollama. This can take a moment.".to_owned()));
        };
        let result = if kind.is_image() {
            vision::describe(&path, kind, &ai, resolve_gps, &on_starting)
        } else {
            document::read_text(&path, kind)
        };
        rep.send(Msg::Loaded(id, logged("Opening the file", result)));
        // Ollama wasn't running when the Settings tab asked for its models.
        if started.load(Ordering::SeqCst) {
            let models = vision::list_models(ai.kind, &ai.address).map_err(|e| sentence(&e));
            rep.send(Msg::Models(models, false));
        }
    };
    // A file that crashes the reader must not end a reading in progress.
    rep.spawn_or(job, move || Msg::Loaded(id, Err(PANIC.to_owned())));
}

/// How many pieces of speech may wait to be played. More would waste requests
/// to a paid service if the user stops early.
const QUEUE_AHEAD: usize = 2;

pub fn speak(rep: &Reporter, control: Arc<Control>, job: SpeechJob) {
    rep.spawn_or(
        move |rep| match run_speech(rep, &control, &job) {
            Ok(true) => rep.send(Msg::Done("Finished reading aloud.".to_owned())),
            Ok(false) => rep.send(Msg::Done("Stopped reading aloud.".to_owned())),
            Err(e) => {
                log::warn!("Reading aloud failed: {e:#}");
                rep.send(Msg::Failed(sentence(&e)));
            }
        },
        || Msg::Failed(PANIC.to_owned()),
    );
}

/// Returns whether it got to the end, rather than being stopped.
fn run_speech(rep: &Reporter, control: &Control, job: &SpeechJob) -> anyhow::Result<bool> {
    let pieces = job.pieces()?;
    log::debug!("Reading aloud with {} in {} pieces", job.provider.label(), pieces.len());
    let playback = Playback::open()?;
    control.attach(playback.player());
    for (done, piece) in pieces.iter().enumerate() {
        while playback.queued() >= QUEUE_AHEAD && !control.stopped() {
            std::thread::sleep(Duration::from_millis(50));
        }
        if control.stopped() {
            return Ok(false);
        }
        let pcm = job.sound_of(piece)?;
        if control.stopped() {
            return Ok(false);
        }
        playback.append(pcm);
        rep.send(Msg::Progress(done as f32 / pieces.len() as f32));
    }
    while playback.queued() > 0 && !control.stopped() {
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(!control.stopped())
}

/// With `stitch`, text over [`speech::PART_CHARS`] characters is still saved
/// as one file; otherwise it is saved as a file for each part.
pub fn save_mp3(rep: &Reporter, control: Arc<Control>, job: SpeechJob, path: PathBuf, stitch: bool) {
    rep.spawn_or(
        move |rep| match run_save(rep, &control, &job, &path, stitch) {
            Ok(Some(saved)) => {
                // Saved in parts, the chosen file isn't written, so one already
                // there still has whatever it had before.
                let kept = (saved != [path.clone()] && path.exists()).then_some(path.as_path());
                rep.send(Msg::Done(saved_message(&saved, kept)));
            }
            Ok(None) => rep.send(Msg::Done("Stopped saving. No file was written.".to_owned())),
            Err(e) => {
                log::warn!("Saving the MP3 failed: {e:#}");
                rep.send(Msg::Failed(sentence(&e)));
            }
        },
        || Msg::Failed(PANIC.to_owned()),
    );
}

fn run_save(
    rep: &Reporter,
    control: &Control,
    job: &SpeechJob,
    path: &Path,
    stitch: bool,
) -> anyhow::Result<Option<Vec<PathBuf>>> {
    let parts = job.parts()?;
    log::debug!("Saving with {} in {} parts", job.provider.label(), parts.len());
    save_parts(rep, control, &parts, path, stitch, |piece| job.sound_of(piece))
}

/// Makes the sound of each piece with `sound_of` and saves it. Returns the
/// files written, or `None` if stopped. Each part is saved as its own numbered
/// file, unless there is only one or `stitch` puts them all in one file.
///
/// Each piece is encoded as soon as it is made, so only compressed sound is
/// kept, however long the text. The files go to temporary files first, and are
/// only moved into place once all of them are ready, so stopping or a failure
/// leaves nothing behind.
fn save_parts(
    rep: &Reporter,
    control: &Control,
    parts: &[Vec<String>],
    path: &Path,
    stitch: bool,
    mut sound_of: impl FnMut(&str) -> anyhow::Result<audio::Pcm>,
) -> anyhow::Result<Option<Vec<PathBuf>>> {
    let total: usize = parts.iter().map(Vec::len).sum();
    anyhow::ensure!(total > 0, "there was nothing to save");
    let outputs = output_paths(path, parts.len(), stitch);
    let mut staged = Vec::with_capacity(outputs.len());
    let mut encoder: Option<audio::Mp3Encoder> = None;
    let mut done = 0;
    for (i, part) in parts.iter().enumerate() {
        for piece in part {
            if control.stopped() {
                return Ok(None);
            }
            let pcm = sound_of(piece)?;
            let encoder = match &mut encoder {
                Some(encoder) => encoder,
                None => encoder.insert(audio::Mp3Encoder::new(pcm.rate)?),
            };
            encoder.push(&pcm)?;
            done += 1;
            rep.send(Msg::Progress(done as f32 / total as f32));
        }
        let file_ends = !stitch || i + 1 == parts.len();
        if file_ends && let Some(finished) = encoder.take() {
            let out = &outputs[staged.len()];
            staged.push((audio::stage(&finished.finish()?, out)?, out.clone()));
        }
    }
    anyhow::ensure!(staged.len() == outputs.len(), "there was nothing to save");
    let mut saved = Vec::with_capacity(staged.len());
    for (file, out) in staged {
        if let Err(e) = audio::persist(file, &out) {
            // Not a set with a part missing. Any files not yet moved into place
            // are deleted as they are dropped.
            for written in &saved {
                let _ = std::fs::remove_file(written);
            }
            return Err(e);
        }
        saved.push(out);
    }
    Ok(Some(saved))
}

fn output_paths(path: &Path, parts: usize, stitch: bool) -> Vec<PathBuf> {
    if stitch || parts <= 1 {
        vec![path.to_path_buf()]
    } else {
        (1..=parts).map(|number| part_path(path, number, parts)).collect()
    }
}

/// `speech.mp3` becomes `speech (part 2 of 3).mp3`, keeping the extension as
/// it was typed.
fn part_path(path: &Path, number: usize, count: usize) -> PathBuf {
    let stem = path.file_stem().map_or_else(|| "speech".into(), |s| s.to_string_lossy().into_owned());
    let extension = path.extension().map_or_else(|| "mp3".into(), |e| e.to_string_lossy().into_owned());
    path.with_file_name(format!("{stem} (part {number} of {count}).{extension}"))
}

/// What to say once saved. `kept` is the file that was chosen, if it wasn't
/// written because the text was saved in parts and a file of that name exists.
fn saved_message(saved: &[PathBuf], kept: Option<&Path>) -> String {
    let message = match saved {
        [one] => format!("Saved {}.", display_name(one)),
        [first, .., last] => format!(
            "The text was over {} characters, so it was saved in {} parts, {} to {}.",
            speech::PART_CHARS,
            saved.len(),
            display_name(first),
            display_name(last)
        ),
        [] => "Nothing was saved.".to_owned(),
    };
    match kept {
        Some(kept) => format!("{message} The existing {} was left as it was.", display_name(kept)),
        None => message,
    }
}

/// Decodes an audio file and judges whether it is speech.
pub fn hear(rep: &Reporter, id: u64, path: PathBuf) {
    rep.spawn_or(
        move |rep| {
            log::info!("Listening to an audio file");
            let heard = transcribe::listen(&path).map(Arc::new).map_err(|e| {
                log::warn!("Listening to an audio file failed: {e:#}");
                sentence(&e)
            });
            rep.send(Msg::Heard(id, heard));
        },
        move || Msg::Heard(id, Err(PANIC.to_owned())),
    );
}

/// Plays an audio file, decoding it as it goes.
pub fn play(rep: &Reporter, id: u64, control: Arc<Control>, path: PathBuf) {
    rep.spawn_or(
        move |rep| {
            let result = run_play(&control, &path).map_err(|e| {
                log::warn!("Playing an audio file failed: {e:#}");
                sentence(&e)
            });
            rep.send(Msg::Played(id, result));
        },
        move || Msg::Played(id, Err(PANIC.to_owned())),
    );
}

fn run_play(control: &Control, path: &Path) -> anyhow::Result<bool> {
    let file = std::fs::File::open(path).context("the file could not be opened")?;
    let decoder = rodio::Decoder::try_from(file).context("this isn't audio that can be played, or it is damaged")?;
    let playback = Playback::open()?;
    control.attach(playback.player());
    playback.append_source(decoder);
    while playback.queued() > 0 && !control.stopped() {
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(!control.stopped())
}

/// What transcribing needs, decided on with the user beforehand.
pub struct TranscribeJob {
    pub heard: Arc<Heard>,
    /// The whisper.cpp program, or `None` to install it first with `install`.
    pub program: Option<PathBuf>,
    pub install: Option<whisper::Installer>,
    pub model: PathBuf,
    /// The user agreed to download the model, which isn't there yet.
    pub download: bool,
}

pub fn transcribe(rep: &Reporter, id: u64, control: Arc<Control>, job: TranscribeJob) {
    rep.spawn_or(
        move |rep| {
            let result = run_transcribe(rep, id, &control, job).map_err(|e| {
                log::warn!("Transcribing failed: {e:#}");
                transcribe::sarcastic(&format!("{e:#}"))
            });
            rep.send(Msg::Transcribed(id, result));
        },
        move || Msg::Transcribed(id, Err(transcribe::sarcastic("something inside me broke. The debug log has the details"))),
    );
}

fn run_transcribe(rep: &Reporter, id: u64, control: &Control, job: TranscribeJob) -> anyhow::Result<Option<String>> {
    // Whole percentages only, so the window isn't woken thousands of times.
    let last = AtomicU32::new(u32::MAX);
    let progress = |p: f32| {
        let percent = (p.clamp(0.0, 1.0) * 100.0) as u32;
        if last.swap(percent, Ordering::Relaxed) != percent {
            rep.send(Msg::TranscribeProgress(id, p));
        }
    };
    let program = match job.program {
        Some(program) => program,
        None => {
            let installer = job.install.context("whisper.cpp isn't installed")?;
            rep.send(Msg::TranscribeStage(id, "Installing whisper.cpp. This can take a few minutes.".to_owned()));
            if !whisper::install(&installer, control)? {
                return Ok(None);
            }
            whisper::find_program(None)
                .context("whisper.cpp was installed, yet its whisper-cli program is nowhere to be found")?
        }
    };
    if job.download {
        rep.send(Msg::TranscribeStage(id, "Downloading the speech recognition model.".to_owned()));
        if !whisper::download_model(&job.model, control, &progress)? {
            return Ok(None);
        }
        last.store(u32::MAX, Ordering::Relaxed);
    }
    rep.send(Msg::TranscribeStage(id, "Transcribing. This takes a while for a long recording.".to_owned()));
    rep.send(Msg::TranscribeProgress(id, 0.0));
    transcribe::transcribe(&program, &job.model, &job.heard.samples, control, &progress)
}

pub fn display_name(path: &Path) -> String {
    path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_become_sentences() {
        assert_eq!(sentence(&anyhow::anyhow!("the file is big")), "The file is big.");
        assert_eq!(sentence(&anyhow::anyhow!("done already.")), "Done already.");
        assert_eq!(sentence(&anyhow::anyhow!("inner").context("outer")), "Outer: inner.");
    }

    #[test]
    fn wordlists_change_what_is_spoken_but_not_the_text() {
        let list = crate::wordlist::parse(r#"<wordlist name="t"><entry match="Leicester" replace="Lester"/></wordlist>"#).unwrap();
        let job = SpeechJob {
            text: Arc::new("Leicester city.".to_owned()),
            provider: Provider::System,
            key: String::new(),
            voice: String::new(),
            wordlists: vec![list],
        };
        assert_eq!(job.pieces().unwrap(), ["Lester city."]);
        assert_eq!(*job.text, "Leicester city.");
    }

    /// From a document on disk to an MP3 on disk, with the real system voice.
    /// CI installs eSpeak NG for this on Ubuntu.
    #[cfg(unix)]
    #[test]
    fn document_to_mp3_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let docx = dir.path().join("a.docx");
        crate::document::tests::docx(&docx, crate::document::tests::DOCX_XML);
        let text = document::read_text(&docx, FileKind::Docx).unwrap();
        let job = SpeechJob {
            text: Arc::new(text),
            provider: Provider::System,
            key: String::new(),
            voice: String::new(),
            wordlists: Vec::new(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        let rep = Reporter::new(tx, egui::Context::default());
        let out = dir.path().join("out.mp3");
        assert_eq!(run_save(&rep, &Control::default(), &job, &out, false).unwrap(), Some(vec![out.clone()]));
        let pcm = audio::decode(std::fs::read(&out).unwrap()).unwrap();
        assert!(pcm.samples.len() > pcm.rate as usize, "more than a second of speech");
        assert!(rx.try_iter().any(|m| matches!(m, Msg::Progress(p) if p == 1.0)));
    }

    /// Plays through the real speakers, so it only runs when asked for:
    /// `cargo test real_read_aloud -- --ignored`.
    #[test]
    #[ignore]
    fn real_read_aloud() {
        let job = |text: &str| SpeechJob {
            text: Arc::new(text.to_owned()),
            provider: Provider::System,
            key: String::new(),
            voice: String::new(),
            wordlists: Vec::new(),
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let rep = Reporter::new(tx, egui::Context::default());
        let control = Control::default();
        assert!(run_speech(&rep, &control, &job("Testing.")).unwrap(), "it reads to the end");

        // Pausing holds it up, and stopping ends it at once, even while paused.
        let control = Arc::new(Control::default());
        let (rep2, control2) = (rep.clone(), control.clone());
        let long = job(&"This sentence goes on for quite a while. ".repeat(20));
        let thread = std::thread::spawn(move || run_speech(&rep2, &control2, &long).unwrap());
        std::thread::sleep(Duration::from_millis(1500));
        control.set_paused(true);
        std::thread::sleep(Duration::from_millis(300));
        assert!(!thread.is_finished());
        let stopped_at = std::time::Instant::now();
        control.request_stop();
        assert!(!thread.join().unwrap(), "it reports being stopped");
        assert!(stopped_at.elapsed() < Duration::from_secs(5), "{:?}", stopped_at.elapsed());
    }

    #[test]
    fn long_text_is_split_into_parts() {
        let job = SpeechJob {
            text: Arc::new("A sentence of some length. ".repeat(400)),
            provider: Provider::Deepgram,
            key: String::new(),
            voice: String::new(),
            wordlists: Vec::new(),
        };
        let parts = job.parts().unwrap();
        assert_eq!(parts.len(), 3);
        assert!(parts.iter().all(|p| p.join(" ").chars().count() <= speech::PART_CHARS));
        assert!(parts.iter().flatten().all(|piece| piece.chars().count() <= 1900));
        let short = SpeechJob { text: Arc::new("Short.".to_owned()), ..job };
        assert_eq!(short.parts().unwrap(), [["Short."]]);
    }

    #[test]
    fn parts_are_numbered() {
        let path = Path::new("dir").join("my speech.mp3");
        assert_eq!(part_path(&path, 2, 3), Path::new("dir").join("my speech (part 2 of 3).mp3"));
        let saved = [part_path(&path, 1, 2), part_path(&path, 2, 2)];
        assert_eq!(
            saved_message(&saved, None),
            "The text was over 4500 characters, so it was saved in 2 parts, my speech (part 1 of 2).mp3 to my speech (part 2 of 2).mp3."
        );
        assert!(saved_message(&saved, Some(&path)).ends_with(" The existing my speech.mp3 was left as it was."));
        assert_eq!(saved_message(std::slice::from_ref(&path), None), "Saved my speech.mp3.");
        assert_eq!(part_path(Path::new("Talk.MP3"), 1, 2), Path::new("Talk (part 1 of 2).MP3"), "as it was typed");
        assert_eq!(output_paths(&path, 1, false), std::slice::from_ref(&path));
        assert_eq!(output_paths(&path, 3, true), std::slice::from_ref(&path));
        assert_eq!(output_paths(&path, 2, false), saved);
    }

    /// Uses the real system voice. CI installs eSpeak NG for this on Ubuntu.
    #[cfg(unix)]
    #[test]
    fn long_text_saves_several_files() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("long.mp3");
        let job = SpeechJob {
            text: Arc::new("Words. ".repeat(700)),
            provider: Provider::System,
            key: String::new(),
            voice: String::new(),
            wordlists: Vec::new(),
        };
        let (tx, _rx) = std::sync::mpsc::channel();
        let rep = Reporter::new(tx, egui::Context::default());
        let saved = run_save(&rep, &Control::default(), &job, &out, false).unwrap().unwrap();
        assert_eq!(saved, [part_path(&out, 1, 2), part_path(&out, 2, 2)]);
        assert!(saved.iter().all(|p| p.exists()));
        assert!(!out.exists());
    }

    /// Fixed tones rather than a voice, because a voice can take a different
    /// time to say the same words each time. Each piece's text is how many
    /// seconds of sound it makes.
    fn save_tones(parts: &[&[&str]], path: &Path, stitch: bool) -> anyhow::Result<Option<Vec<PathBuf>>> {
        let parts: Vec<Vec<String>> = parts.iter().map(|p| p.iter().map(|s| (*s).to_owned()).collect()).collect();
        let (tx, _rx) = std::sync::mpsc::channel();
        let rep = Reporter::new(tx, egui::Context::default());
        save_parts(&rep, &Control::default(), &parts, path, stitch, |piece| {
            let seconds: usize = piece.parse().map_err(|_| anyhow::anyhow!("the voice failed"))?;
            Ok(audio::Pcm { samples: (0..22050 * seconds).map(|i| (i as f32 * 0.05).sin() * 0.5).collect(), rate: 22050 })
        })
    }

    fn seconds(path: &Path) -> f64 {
        let pcm = audio::decode(std::fs::read(path).unwrap()).unwrap();
        pcm.samples.len() as f64 / f64::from(pcm.rate)
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> =
            std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    #[test]
    fn parts_are_saved_separately_or_stitched() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("apart.mp3");
        let saved = save_tones(&[&["1", "1"], &["3"]], &out, false).unwrap().unwrap();
        assert_eq!(saved, [part_path(&out, 1, 2), part_path(&out, 2, 2)]);
        assert!(!out.exists());
        assert!((seconds(&saved[0]) - 2.0).abs() < 0.1, "{}", seconds(&saved[0]));
        assert!((seconds(&saved[1]) - 3.0).abs() < 0.1, "{}", seconds(&saved[1]));

        let joined = dir.path().join("joined.mp3");
        let saved = save_tones(&[&["1", "1"], &["3"]], &joined, true).unwrap().unwrap();
        assert_eq!(saved, std::slice::from_ref(&joined), "stitched into the one file chosen");
        assert!((seconds(&joined) - 5.0).abs() < 0.1, "all the parts are in it: {}", seconds(&joined));

        let single = dir.path().join("single.mp3");
        assert_eq!(save_tones(&[&["1"]], &single, false).unwrap().unwrap(), std::slice::from_ref(&single));
        assert!(save_tones(&[], &single, true).is_err());
        assert_eq!(files_in(dir.path()).len(), 4, "no temporary files are left: {:?}", files_in(dir.path()));
    }

    #[test]
    fn a_failure_leaves_no_parts_behind() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.mp3");
        std::fs::write(part_path(&out, 1, 2), b"old").unwrap();
        assert!(save_tones(&[&["1"], &["broken"]], &out, false).is_err());
        assert_eq!(files_in(dir.path()), ["out (part 1 of 2).mp3"], "nothing new, not even temporary files");
        assert_eq!(std::fs::read(part_path(&out, 1, 2)).unwrap(), b"old", "the earlier file wasn't touched");
    }

    #[test]
    fn stopping_writes_no_file() {
        let dir = tempfile::tempdir().unwrap();
        let out = dir.path().join("out.mp3");
        let job = SpeechJob {
            text: Arc::new("Words.".to_owned()),
            provider: Provider::System,
            key: String::new(),
            voice: String::new(),
            wordlists: Vec::new(),
        };
        let control = Control::default();
        control.request_stop();
        let (tx, _rx) = std::sync::mpsc::channel();
        let rep = Reporter::new(tx, egui::Context::default());
        assert!(run_save(&rep, &control, &job, &out, true).unwrap().is_none());
        assert!(!out.exists());
    }
}
