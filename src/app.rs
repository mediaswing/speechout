//! The window: four tabs (General, Settings, Wordlists, Audio Player) with
//! every control on its own full-width line.
//!
//! Accessibility notes:
//! * egui publishes the interface to screen readers through AccessKit
//!   (UI Automation on Windows, NSAccessibility on macOS, AT-SPI on Linux).
//! * Every text field and dropdown is linked to its visible label, the tabs
//!   are exposed with the tab role and selected state, and the status line is a
//!   polite live region so progress and errors are announced.
//! * Everything works from the keyboard: Tab and Shift+Tab move between
//!   controls, Space or Enter activates them, arrow keys change a focused
//!   dropdown, and the shortcuts below work from anywhere.
//! * A thick focus ring is drawn around whichever control has focus.

use crate::audio::{AudioFormat, Cue, Listened, SoundKind};
use crate::i18n::{self, Language, Translation, t, tf};
use crate::playlist::Playlist;
use crate::settings::Settings;
use crate::speech::{self, Provider, Voice};
use crate::speakers::SpeakerLabels;
use crate::transcribe::WhisperModel;
use crate::wordlist::{self, Installed, Substitutions};
use crate::worker::{self, Control, Msg, Reporter, SpeechJob};
use egui::accesskit::{Live, Role};
use egui::{Button, Color32, EventFilter, Id, Key, KeyboardShortcut, Modifiers, RichText, Ui};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;

pub const APP_TITLE: &str = "Speech Output Engine";

const CONTROL_HEIGHT: f32 = 36.0;

/// Spoken by the Preview voice button. Kept short because cloud services
/// charge by the character.
const PREVIEW_TEXT: &str = "This is a preview of the selected voice.";

/// The command key as printed on buttons and as spoken in status messages.
#[cfg(target_os = "macos")]
const MOD_KEY: (&str, &str) = ("Cmd", "Command");
#[cfg(not(target_os = "macos"))]
const MOD_KEY: (&str, &str) = ("Ctrl", "Control");

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    General,
    Settings,
    Wordlists,
    Player,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::General, Tab::Settings, Tab::Wordlists, Tab::Player];

    fn label(self) -> String {
        match self {
            Tab::General => t("tab.general"),
            Tab::Settings => t("tab.settings"),
            Tab::Wordlists => t("tab.wordlists"),
            Tab::Player => t("tab.player"),
        }
    }

    /// Stays the same whatever the interface language, so focus can find it.
    fn id(self) -> Id {
        Id::new(("tab", self as u8))
    }
}

/// Where the text to read aloud comes from, chosen on the General tab.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Source {
    File,
    Paste,
}

impl Source {
    const ALL: [Source; 2] = [Source::File, Source::Paste];

    fn label(self) -> String {
        match self {
            Source::File => t("general.source_file"),
            Source::Paste => t("general.source_paste"),
        }
    }
}

/// A zip file of audio open on the Audio Player tab.
struct OpenPlaylist {
    zip: PathBuf,
    list: Playlist,
    /// The track chosen, counting from 0.
    current: usize,
    /// The first track has been opened, and asked to download the models for
    /// transcribing if they were missing. Later tracks don't ask again.
    offered_models: bool,
}

enum Loadable<T> {
    Loading,
    Ready(T),
    Failed,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobKind {
    Speaking,
    Previewing,
    Saving,
    /// Downloading an AI model into Ollama.
    Downloading,
    /// Translating the interface with an AI model.
    Translating,
    /// Turning the speech in an audio file into text with Whisper.
    Transcribing,
}

pub struct SpeechApp {
    settings: Settings,
    tab: Tab,
    rep: Reporter,
    rx: Receiver<Msg>,
    api_keys: HashMap<Provider, String>,
    key_inputs: HashMap<Provider, String>,
    voices: HashMap<Provider, Loadable<Vec<Voice>>>,
    models: Option<Loadable<Vec<String>>>,
    file: Option<PathBuf>,
    /// Text read from the chosen file.
    text: String,
    source: Source,
    /// The text box as typed or pasted, and the cleaned text read aloud.
    pasted: String,
    pasted_text: String,
    loading_file: bool,
    /// A photo chosen before a local AI model was found, to describe once
    /// the check for Ollama finishes.
    waiting_photo: Option<PathBuf>,
    /// Ollama is being installed or started for a waiting photo.
    setting_up_ollama: bool,
    /// The window has been checked against the screen size.
    fitted_to_screen: bool,
    job: Option<(JobKind, Arc<Control>)>,
    paused: bool,
    checking_update: bool,
    /// Progress of the current read-aloud or save job, from 0.0 to 1.0.
    progress: f32,
    status: String,
    log_dir_input: String,
    wordlists: Vec<Installed>,
    wordlist_to_remove: usize,
    /// Languages offered on the Settings tab.
    languages: Vec<Language>,
    focus: Option<Id>,
    /// The file on the Audio Player tab.
    audio: Option<(PathBuf, Arc<Listened>)>,
    /// Stops the audio file being opened.
    audio_loader: Option<Arc<Control>>,
    transcript: String,
    /// Controls the audio file while it is playing or paused.
    audio_player: Option<Arc<Control>>,
    audio_paused: bool,
    audio_position: Duration,
    /// The playlist the audio file is a track of.
    playlist: Option<OpenPlaylist>,
    /// Play the audio file as soon as it has opened, as when one track of a
    /// playlist follows another.
    play_when_loaded: bool,
    /// Transcribe the audio file once the track before it, in a playlist,
    /// has stopped being transcribed.
    transcribe_when_free: bool,
    /// Transcribe the audio file once the Whisper model has downloaded.
    transcribe_after_download: bool,
    cues: crate::audio::Cues,
}

impl SpeechApp {
    pub fn new(cc: &eframe::CreationContext<'_>, mut settings: Settings, log_status: String) -> Self {
        setup_style(&cc.egui_ctx);
        let (tx, rx) = channel();
        let rep = Reporter::new(tx, cc.egui_ctx.clone());

        let wordlist_dir = crate::paths::wordlist_dir();
        if !settings.examples_installed {
            match wordlist::install_examples(&wordlist_dir) {
                Ok(()) => {
                    settings.examples_installed = true;
                    settings.save();
                }
                Err(e) => log::warn!("could not install example wordlists: {e}"),
            }
        }

        let api_keys = Provider::ALL
            .iter()
            .filter_map(|p| Some((*p, crate::secrets::get(p.key_name()?)?)))
            .collect();

        let mut app = Self {
            log_dir_input: settings.log_dir().display().to_string(),
            settings,
            tab: Tab::General,
            rep,
            rx,
            api_keys,
            key_inputs: HashMap::new(),
            voices: HashMap::new(),
            models: None,
            file: None,
            text: String::new(),
            source: Source::File,
            pasted: String::new(),
            pasted_text: String::new(),
            loading_file: false,
            waiting_photo: None,
            setting_up_ollama: false,
            fitted_to_screen: false,
            job: None,
            paused: false,
            checking_update: false,
            progress: 0.0,
            status: format!("{} {log_status}", tf("status.ready", &[("key_name", &MOD_KEY.1)])),
            wordlists: Vec::new(),
            wordlist_to_remove: 0,
            languages: i18n::available(&crate::paths::languages_dir()),
            focus: Some(Tab::General.id()),
            audio: None,
            audio_loader: None,
            transcript: String::new(),
            audio_player: None,
            audio_paused: false,
            audio_position: Duration::ZERO,
            playlist: None,
            play_when_loaded: false,
            transcribe_when_free: false,
            transcribe_after_download: false,
            cues: crate::audio::Cues::default(),
        };
        app.reload_wordlists();
        // Ask Ollama for its models now (a quick local request), so photos can
        // be described without visiting Settings first.
        app.ensure_models();
        if app.settings.check_updates {
            app.rep.spawn(|rep| worker::check_for_update(rep, false));
        }
        app
    }

    // ----- state helpers -------------------------------------------------

    fn announce(&mut self, text: impl Into<String>) {
        let mut text = text.into();
        log::info!("status: {text}");
        // Screen readers ignore a live region whose text has not changed, so
        // a repeated message (pressing F7 twice while paused, say) would be
        // silent. A trailing no-break space makes the text differ.
        if text == self.status.trim_end_matches('\u{a0}') && !self.status.ends_with('\u{a0}') {
            text.push('\u{a0}');
        }
        self.status = text;
    }

    /// Reports something that went wrong: in the status line, like any other
    /// message, and in an error dialog so it cannot be missed. Screen readers
    /// read the dialog out when it opens.
    fn show_error(&mut self, text: impl Into<String>) {
        let text = text.into();
        self.announce(text.clone());
        self.cue(Cue::Failure);
        rfd::MessageDialog::new()
            .set_title(APP_TITLE)
            .set_description(text)
            .set_buttons(rfd::MessageButtons::Ok)
            .set_level(rfd::MessageLevel::Error)
            .show();
    }

    /// Plays the success or failure sound, unless sounds are turned off or
    /// speech or an audio file is playing, which it would talk over.
    fn cue(&mut self, cue: Cue) {
        let speaking = matches!(self.job, Some((JobKind::Speaking | JobKind::Previewing, _))) && !self.paused;
        let playing = self.audio_player.is_some() && !self.audio_paused;
        if self.settings.sounds && !speaking && !playing {
            self.cues.play(cue);
        }
    }

    /// Speaks how far through reading or saving we are (F7).
    fn announce_progress(&mut self) {
        let percent = (self.progress * 100.0).round();
        let key = match &self.job {
            Some((JobKind::Speaking, _)) if self.paused => "progress.paused",
            Some((JobKind::Speaking, _)) => "progress.reading",
            Some((JobKind::Previewing, _)) => "progress.previewing",
            Some((JobKind::Saving, _)) => "progress.saving",
            Some((JobKind::Downloading, _)) => "progress.downloading",
            Some((JobKind::Translating, _)) => "progress.translating",
            Some((JobKind::Transcribing, _)) => "progress.transcribing",
            None if self.setting_up_ollama => "progress.setting_up",
            None if self.loading_file || self.audio_loader.is_some() => "progress.opening",
            None => "progress.nothing",
        };
        let msg = tf(key, &[("percent", &percent)]);
        self.announce(msg);
    }

    /// The text that will be read aloud: from the file, or from the text box.
    fn current_text(&self) -> &str {
        match self.source {
            Source::File => &self.text,
            Source::Paste => &self.pasted_text,
        }
    }

    fn copy_text(&mut self, ctx: &egui::Context) {
        let text = self.current_text().to_owned();
        if text.is_empty() {
            self.announce(t("copy.nothing"));
            return;
        }
        let words = text.split_whitespace().count();
        ctx.copy_text(text);
        self.announce(tf("copy.done", &[("count", &words)]));
    }

    fn available_providers(&self) -> Vec<Provider> {
        Provider::ALL
            .into_iter()
            .filter(|p| p.key_name().is_none() || self.api_keys.contains_key(p))
            .collect()
    }

    fn active_provider(&self) -> Provider {
        let p = self.settings.provider;
        if self.available_providers().contains(&p) { p } else { Provider::System }
    }

    fn ensure_voices(&mut self, provider: Provider) {
        if self.voices.contains_key(&provider) {
            return;
        }
        self.voices.insert(provider, Loadable::Loading);
        let key = self.api_keys.get(&provider).cloned();
        self.rep.spawn(move |rep| worker::load_voices(rep, provider, key));
    }

    fn ensure_models(&mut self) {
        if self.models.is_none() {
            self.models = Some(Loadable::Loading);
            self.rep.spawn(worker::load_models);
        }
    }

    fn voice_list(&self, provider: Provider) -> &[Voice] {
        match self.voices.get(&provider) {
            Some(Loadable::Ready(v)) => v,
            _ => &[],
        }
    }

    /// The chosen voice, falling back to the first one available.
    fn current_voice(&self, provider: Provider) -> Option<&Voice> {
        let voices = self.voice_list(provider);
        let saved = self.settings.voice_for(provider);
        voices.iter().find(|v| Some(v.id.as_str()) == saved).or_else(|| voices.first())
    }

    fn reload_wordlists(&mut self) {
        self.wordlists = wordlist::list_installed(&crate::paths::wordlist_dir());
        if self.wordlist_to_remove >= self.wordlists.len() {
            self.wordlist_to_remove = 0;
        }
    }

    /// The window opens at its usual size, which can be taller than the room
    /// above the taskbar on a small or zoomed screen, hiding the progress bar
    /// and status line. Once the screen size is known, shrink the window to
    /// fit and move it up if its bottom would be covered.
    fn fit_to_screen(&mut self, ctx: &egui::Context) {
        /// Room left for a taskbar, dock or panel.
        const RESERVED: f32 = 56.0;
        if self.fitted_to_screen {
            return;
        }
        let (monitor, inner, outer) = ctx.input(|i| {
            let v = i.viewport();
            (v.monitor_size, v.inner_rect, v.outer_rect)
        });
        // Some systems (Wayland) never report the window's position.
        let (Some(monitor), Some(inner), Some(outer)) = (monitor, inner, outer) else { return };
        self.fitted_to_screen = true;
        let border = outer.size() - inner.size();
        let largest = (monitor - border - egui::vec2(0.0, RESERVED)).max(egui::vec2(360.0, 480.0));
        let size = inner.size().min(largest);
        if size != inner.size() {
            log::info!("shrinking the window from {:?} to {size:?} to fit the screen", inner.size());
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
        }
        // Only move a window on the main screen, whose top-left corner is 0, 0.
        let bottom = outer.top() + border.y + size.y;
        if outer.top() >= 0.0 && outer.top() < monitor.y && bottom > monitor.y - RESERVED {
            let top = (monitor.y - RESERVED - border.y - size.y).max(0.0);
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(egui::pos2(outer.left(), top)));
        }
    }

    fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    fn handle_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Voices(provider, result) => {
                    if let Err(e) = &result {
                        self.show_error(tf("voices.failed", &[("service", &provider.name()), ("error", e)]));
                    }
                    self.voices.insert(
                        provider,
                        match result {
                            Ok(v) => Loadable::Ready(v),
                            Err(_) => Loadable::Failed,
                        },
                    );
                }
                Msg::Models(result) => {
                    let waiting = self.waiting_photo.is_some();
                    match &result {
                        // A waiting photo leads to an offer to download a model instead.
                        Ok(models) if models.is_empty() && !waiting => self.announce(t("models.none")),
                        Ok(models) if models.is_empty() => {}
                        Ok(models) => {
                            if !models.contains(&self.settings.vision_model)
                                && let Some(model) = crate::vision::preferred_model(models) {
                                    self.settings.vision_model = model.clone();
                                    self.settings.save();
                                }
                            if self.tab == Tab::Settings {
                                self.announce(tf("models.found", &[("count", &models.len())]));
                            }
                        }
                        // A waiting photo gets the offer to install or start
                        // Ollama instead, so it is not reported twice.
                        Err(e) => {
                            if self.tab == Tab::Settings && !waiting {
                                self.show_error(e.clone());
                            }
                        }
                    }
                    if let Some(path) = self.waiting_photo.take() {
                        self.loading_file = false;
                        match &result {
                            Err(_) => self.offer_ollama(path),
                            Ok(models) if !crate::vision::has_vision_model(models) => {
                                self.offer_model_download(Some(path));
                            }
                            Ok(_) => self.open_file(path, true),
                        }
                    }
                    self.models = Some(match result {
                        Ok(m) => Loadable::Ready(m),
                        Err(_) => Loadable::Failed,
                    });
                }
                Msg::ModelDownloaded(model, result) => {
                    self.job = None;
                    let photo = self.waiting_photo.take();
                    match result {
                        Ok(true) => {
                            self.settings.vision_model = model.clone();
                            self.settings.save();
                            self.models = None;
                            self.ensure_models();
                            match photo {
                                Some(path) => self.open_file(path, true),
                                None => {
                                    self.announce(tf("model.downloaded", &[("model", &model)]));
                                    self.cue(Cue::Success);
                                }
                            }
                        }
                        Ok(false) => self.announce(t("model.download_stopped")),
                        Err(e) => self.show_error(tf("model.download_failed", &[("error", &e)])),
                    }
                }
                Msg::OllamaReady(result) => {
                    self.setting_up_ollama = false;
                    match result {
                        Ok(()) => {
                            // Look for models, then describe the waiting photo.
                            self.announce(t("ollama.running"));
                            self.models = None;
                            self.ensure_models();
                        }
                        Err(e) => {
                            self.loading_file = false;
                            self.waiting_photo = None;
                            self.show_error(tf("ollama.setup_failed", &[("error", &e)]));
                        }
                    }
                }
                Msg::Loaded(path, result) => {
                    self.loading_file = false;
                    let name = file_name(&path);
                    match result {
                        Ok(text) => {
                            let words = text.split_whitespace().count();
                            self.text = text;
                            self.file = Some(path);
                            self.source = Source::File;
                            self.announce(tf(
                                "file.loaded",
                                &[("name", &name), ("count", &words), ("key_name", &MOD_KEY.1)],
                            ));
                            self.cue(Cue::Success);
                        }
                        Err(e) => self.show_error(tf("file.load_failed", &[("name", &name), ("error", &e)])),
                    }
                }
                Msg::Translated(result) => self.translated(result),
                Msg::AudioLoaded(path, result) => self.audio_loaded(path, result),
                Msg::PlaylistOpened(path, result) => self.playlist_opened(path, result),
                Msg::AudioPosition(position) => {
                    if self.audio_player.is_some() {
                        self.audio_position = position;
                    }
                }
                Msg::AudioEnded(result) => {
                    self.audio_player = None;
                    self.audio_paused = false;
                    self.audio_position = Duration::ZERO;
                    match result {
                        // Stopped because another file or track was chosen:
                        // that is being announced instead.
                        Ok(_) if self.audio_loader.is_some() => {}
                        Ok(true) => match &self.playlist {
                            Some(p) if p.current + 1 < p.list.tracks.len() => {
                                let next = p.current + 1;
                                self.open_track(next, true, String::new());
                            }
                            Some(_) => self.announce(t("playlist.finished")),
                            None => self.announce(t("player.finished")),
                        },
                        Ok(false) => self.announce(t("player.stopped")),
                        Err(e) => self.show_error(tf("player.play_failed", &[("error", &e)])),
                    }
                }
                Msg::WhisperDownloaded(model, result) => {
                    self.job = None;
                    let transcribe = std::mem::take(&mut self.transcribe_after_download);
                    match result {
                        Ok(true) => {
                            let done = tf("whisper.downloaded", &[("model", &model.name())]);
                            if transcribe && self.audio.is_some() {
                                self.transcribe_audio(done);
                            } else {
                                self.announce(done);
                                self.cue(Cue::Success);
                            }
                        }
                        Ok(false) => self.announce(t("whisper.download_stopped")),
                        Err(e) => self.show_error(tf("whisper.download_failed", &[("error", &e)])),
                    }
                }
                Msg::SpeakerModelsDownloaded(result) => {
                    self.job = None;
                    let transcribe = std::mem::take(&mut self.transcribe_after_download);
                    match result {
                        Ok(true) => {
                            let done = t("speakers.downloaded");
                            if transcribe && self.audio.is_some() {
                                self.transcribe_audio(done);
                            } else {
                                self.announce(done);
                                self.cue(Cue::Success);
                            }
                        }
                        Ok(false) => self.announce(t("speakers.download_stopped")),
                        Err(e) => self.show_error(tf("speakers.download_failed", &[("error", &e)])),
                    }
                }
                Msg::Transcribed(path, result) => self.transcribed(path, result),
                Msg::Update(result, requested) => self.update_checked(result, requested),
                Msg::Status(s) => self.announce(s),
                Msg::Progress(p) => self.progress = p.clamp(0.0, 1.0),
                Msg::Done(s, finished) => {
                    // A preview ending is not worth a sound.
                    let job = self.job.take().map(|(kind, _)| kind);
                    self.paused = false;
                    self.announce(s);
                    if finished && matches!(job, Some(JobKind::Speaking | JobKind::Saving)) {
                        self.cue(Cue::Success);
                    }
                }
                Msg::Failed(e) => {
                    self.job = None;
                    self.paused = false;
                    self.show_error(e);
                }
            }
        }
    }

    // ----- actions ------------------------------------------------------

    fn update_checked(&mut self, result: Result<Option<crate::update::Release>, String>, requested: bool) {
        self.checking_update = false;
        let release = match result {
            Ok(Some(release)) => release,
            Ok(None) => {
                if requested {
                    self.announce(tf("update.latest", &[("version", &env!("CARGO_PKG_VERSION"))]));
                }
                return;
            }
            Err(e) => {
                // An automatic check fails quietly (the error is in the log).
                if requested {
                    self.show_error(e);
                }
                return;
            }
        };
        log::info!("update available: {}", release.version);
        let open = rfd::MessageDialog::new()
            .set_title(t("update.title"))
            .set_description(tf(
                "update.question",
                &[("version", &release.version), ("app", &APP_TITLE), ("current", &env!("CARGO_PKG_VERSION"))],
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .set_level(rfd::MessageLevel::Info)
            .show()
            == rfd::MessageDialogResult::Yes;
        if open {
            match crate::platform::open_url(&release.url) {
                Ok(()) => self.announce(tf("update.opened", &[("version", &release.version)])),
                Err(e) => self.show_error(tf("update.open_failed", &[("error", &e), ("url", &release.url)])),
            }
        } else {
            self.announce(tf("update.later", &[("version", &release.version)]));
        }
    }

    fn check_for_update(&mut self) {
        if self.checking_update {
            return;
        }
        self.checking_update = true;
        self.announce(t("update.checking"));
        self.rep.spawn(|rep| worker::check_for_update(rep, true));
    }

    fn choose_file(&mut self) {
        if self.loading_file {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title(t("file.dialog_title"))
            .add_filter(t("file.filter_all"), &["pdf", "txt", "docx", "odt", "csv", "ods", "pptx", "ppt", "jpg", "jpeg", "heic", "heif"])
            .add_filter(t("file.filter_documents"), &["pdf", "txt", "docx", "odt", "csv", "ods", "pptx", "ppt"])
            .add_filter(t("file.filter_photos"), &["jpg", "jpeg", "heic", "heif"])
            .pick_file()
        else {
            return;
        };
        let Some(kind) = crate::document::FileKind::from_path(&path) else {
            self.show_error(t("file.unsupported"));
            return;
        };
        let is_image = kind == crate::document::FileKind::Image;
        if is_image && matches!(self.job, Some((JobKind::Downloading, _))) {
            self.announce(t("file.model_downloading"));
            return;
        }
        // Check Ollama first when no model is chosen, when Ollama was not
        // running at the last check, or when it has no model for photos, so
        // the user is offered a fix instead of a failed description.
        let needs_check = self.settings.vision_model.is_empty()
            || match &self.models {
                Some(Loadable::Failed) => true,
                Some(Loadable::Ready(m)) => !crate::vision::has_vision_model(m),
                _ => false,
            };
        if is_image && needs_check {
            // Look for Ollama again, in case it has been installed or started
            // since the last check, and carry on when the answer arrives.
            if !matches!(self.models, Some(Loadable::Loading)) {
                self.models = None;
            }
            self.ensure_models();
            self.loading_file = true;
            self.announce(tf("file.looking_for_model", &[("name", &file_name(&path))]));
            self.waiting_photo = Some(path);
            return;
        }
        self.open_file(path, is_image);
    }

    /// Ollama could not be reached when a photo was chosen. Offers to start
    /// it, install it with the package manager, or open its download page.
    fn offer_ollama(&mut self, path: PathBuf) {
        let name = file_name(&path);
        let installed = crate::platform::ollama_installed();
        let manager = crate::platform::package_manager();
        let intro = t("ollama.intro");
        let (title, question) = match (installed, manager) {
            (true, _) => (t("ollama.start_title"), t("ollama.start_question")),
            (false, Some(manager)) => {
                let mut question = tf("ollama.install_question", &[("manager", &manager)]);
                if cfg!(target_os = "linux") {
                    question = format!("{question} {}", t("ollama.password"));
                }
                (t("ollama.install_title"), question)
            }
            (false, None) => (t("ollama.install_title"), t("ollama.download_question")),
        };
        let question = format!("{intro}\n\n{question}");
        let yes = rfd::MessageDialog::new()
            .set_title(title)
            .set_description(question)
            .set_buttons(rfd::MessageButtons::YesNo)
            .set_level(rfd::MessageLevel::Info)
            .show()
            == rfd::MessageDialogResult::Yes;
        if !yes {
            self.announce(tf("ollama.declined", &[("name", &name)]));
            return;
        }
        if !installed && manager.is_none() {
            const DOWNLOAD_PAGE: &str = "https://ollama.com/download";
            match crate::platform::open_url(DOWNLOAD_PAGE) {
                Ok(()) => self.announce(t("ollama.page_opened")),
                Err(e) => self.show_error(tf("ollama.page_failed", &[("error", &e), ("url", &DOWNLOAD_PAGE)])),
            }
            return;
        }
        self.setting_up_ollama = true;
        self.loading_file = true;
        self.waiting_photo = Some(path);
        self.announce(match manager {
            Some(manager) if !installed => tf("ollama.installing", &[("manager", &manager)]),
            _ => t("ollama.starting"),
        });
        self.rep.spawn(move |rep| worker::set_up_ollama(rep, !installed));
    }

    /// Ollama has no model that understands images. Asks whether to download
    /// one, then describes `photo` if one is waiting.
    fn offer_model_download(&mut self, photo: Option<PathBuf>) {
        let model = crate::vision::SUGGESTED_MODEL;
        let yes = rfd::MessageDialog::new()
            .set_title(t("model.download_title"))
            .set_description(tf("model.download_question", &[("model", &model), ("size", &model_size())]))
            .set_buttons(rfd::MessageButtons::YesNo)
            .set_level(rfd::MessageLevel::Info)
            .show()
            == rfd::MessageDialogResult::Yes;
        if !yes {
            self.announce(t("model.not_downloaded"));
            return;
        }
        let control = Arc::new(Control::default());
        self.job = Some((JobKind::Downloading, control.clone()));
        self.progress = 0.0;
        self.waiting_photo = photo;
        self.announce(tf("model.downloading", &[("model", &model), ("size", &model_size())]));
        self.rep.spawn(move |rep| worker::download_model(rep, control, model.to_owned()));
    }

    fn open_file(&mut self, path: PathBuf, is_image: bool) {
        self.loading_file = true;
        let key = if is_image { "file.describing" } else { "file.opening" };
        self.announce(tf(key, &[("name", &file_name(&path))]));
        let model = self.settings.vision_model.clone();
        let resolve = self.settings.resolve_location;
        self.rep.spawn(move |rep| worker::load_file(rep, path, model, resolve));
    }

    /// A job for the loaded document, with the wordlists applied and then
    /// email addresses, dates, numbers and so on put the way they are said.
    fn build_job(&mut self) -> Option<SpeechJob> {
        if self.is_busy() {
            return None;
        }
        if self.current_text().trim().is_empty() {
            let msg = match self.source {
                Source::File => tf("read.nothing", &[("key_name", &MOD_KEY.1)]),
                Source::Paste => t("read.nothing_pasted"),
            };
            self.announce(msg);
            return None;
        }
        let subs = Substitutions::new(&self.wordlists, &self.settings.disabled_wordlists);
        let text = crate::spoken::apply(&subs.apply(self.current_text()));
        self.job_for(text)
    }

    /// A job that speaks `text` with the chosen service, voice and speed.
    fn job_for(&mut self, text: String) -> Option<SpeechJob> {
        let provider = self.active_provider();
        let voice = match self.current_voice(provider) {
            Some(v) => v.id.clone(),
            None if provider == Provider::System => String::new(),
            None => {
                self.announce(t("voice.choose_first"));
                return None;
            }
        };
        Some(SpeechJob {
            provider,
            key: self.api_keys.get(&provider).cloned(),
            voice,
            speed: self.settings.speed_for(provider),
            text,
            announce_parts: self.settings.announce_parts,
            preview: false,
        })
    }

    /// For cloud voices, how much text the job will send, such as
    /// " 48,250 characters will be sent to ElevenLabs." With `up_to`, says
    /// "Up to", because reading aloud stops sending when the user stops it.
    fn usage_note(job: &SpeechJob, up_to: bool) -> String {
        if job.provider == Provider::System {
            return String::new();
        }
        let count = speech::billable_chars(job.provider, &job.text, job.announce_parts);
        let key = match (up_to, count == 1) {
            (true, true) => "usage.up_to.one",
            (true, false) => "usage.up_to.other",
            (false, true) => "usage.exact.one",
            (false, false) => "usage.exact.other",
        };
        let note = tf(key, &[("count", &speech::format_count(count)), ("service", &job.provider.short_name())]);
        format!(" {note}")
    }

    fn read_aloud(&mut self) {
        let Some(job) = self.build_job() else { return };
        // Reading aloud stops sending text when the user presses Stop.
        let usage = Self::usage_note(&job, true);
        self.start_speaking(job, format!("{}{usage}", t("read.started")));
    }

    /// Speaks a short sentence with the chosen voice and speed, through the
    /// same job as reading aloud, so Stop, Pause and error handling all work.
    fn preview_voice(&mut self) {
        if self.is_busy() {
            return;
        }
        let Some(mut job) = self.job_for(PREVIEW_TEXT.to_owned()) else { return };
        job.preview = true;
        self.start_speaking(job, t("progress.previewing"));
    }

    fn start_speaking(&mut self, job: SpeechJob, message: String) {
        // One sound at a time, so the voice can be heard.
        if self.audio_player.is_some() {
            self.announce(t("read.audio_playing"));
            return;
        }
        self.cues.stop();
        let kind = if job.preview { JobKind::Previewing } else { JobKind::Speaking };
        let control = Arc::new(Control::default());
        self.job = Some((kind, control.clone()));
        self.progress = 0.0;
        self.paused = false;
        self.announce(message);
        self.rep.spawn(move |rep| worker::speak(rep, control, job));
    }

    fn toggle_pause(&mut self) {
        if let Some((JobKind::Speaking, control)) = &self.job {
            self.paused = !self.paused;
            control.set_paused(self.paused);
            if !self.paused {
                self.cues.stop();
            }
            let msg = if self.paused { t("read.paused") } else { t("read.resumed") };
            self.announce(msg);
        }
    }

    fn stop(&mut self) {
        if let Some((_, control)) = &self.job {
            control.stop();
            self.announce(t("read.stopping"));
        }
    }

    fn save_audio(&mut self) {
        if self.is_busy() {
            return;
        }
        let format = self.settings.audio_format;
        let file = if self.source == Source::File { self.file.as_ref() } else { None };
        let stem = file
            .and_then(|f| f.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| t("save.default_name"));
        let Some(mut path) = rfd::FileDialog::new()
            .set_title(t("save.dialog_title"))
            .set_file_name(format!("{stem}.{}", format.extension()))
            .add_filter(format.label(), &[format.extension()])
            .save_file()
        else {
            return;
        };
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case(format.extension())) {
            path.as_mut_os_string().push(format!(".{}", format.extension()));
        }
        let Some(mut job) = self.build_job() else { return };
        // A saved photo description ends by saying how it was made, since
        // the listener cannot tell from the audio alone.
        let is_image = self
            .file
            .as_deref()
            .and_then(crate::document::FileKind::from_path)
            .is_some_and(|k| k == crate::document::FileKind::Image);
        if is_image {
            job.text = format!("{}\n\n{}", job.text.trim_end(), job.provider.image_description_note());
        }
        let control = Arc::new(Control::default());
        self.job = Some((JobKind::Saving, control.clone()));
        self.progress = 0.0;
        let usage = Self::usage_note(&job, false);
        let preparing = tf("save.preparing", &[("name", &file_name(&path))]);
        self.announce(format!("{preparing}{usage} {}", t("save.cancel_hint")));
        self.rep.spawn(move |rep| worker::save(rep, control, job, path, format));
    }

    fn save_keys(&mut self) {
        let mut saved = Vec::new();
        let mut where_ = "";
        for provider in Provider::ALL {
            let Some(name) = provider.key_name() else { continue };
            let Some(input) = self.key_inputs.get(&provider).map(|s| s.trim().to_owned()) else { continue };
            if input.is_empty() {
                continue;
            }
            match crate::secrets::set(name, &input) {
                Ok(location) => {
                    where_ = location;
                    if let Some(stored) = crate::secrets::get(name) {
                        self.api_keys.insert(provider, stored);
                    }
                    self.voices.remove(&provider);
                    saved.push(provider.name());
                }
                Err(e) => {
                    self.show_error(tf("keys.save_failed", &[("service", &provider.name()), ("error", &e)]));
                    return;
                }
            }
        }
        self.key_inputs.clear();
        if saved.is_empty() {
            self.announce(t("keys.type_first"));
        } else {
            let services = saved.join(&format!(" {} ", t("keys.and")));
            self.announce(tf("keys.saved", &[("services", &services), ("place", &where_)]));
        }
    }

    fn remove_keys(&mut self) {
        if self.api_keys.is_empty() {
            self.announce(t("keys.none"));
            return;
        }
        let confirmed = rfd::MessageDialog::new()
            .set_title(t("keys.remove_title"))
            .set_description(t("keys.remove_question"))
            .set_buttons(rfd::MessageButtons::YesNo)
            .set_level(rfd::MessageLevel::Warning)
            .show()
            == rfd::MessageDialogResult::Yes;
        if !confirmed {
            return;
        }
        for provider in Provider::ALL {
            if let Some(name) = provider.key_name()
                && let Err(e) = crate::secrets::delete(name) {
                    log::warn!("could not delete key: {e}");
                }
        }
        self.api_keys.clear();
        self.voices.retain(|p, _| *p == Provider::System);
        self.announce(t("keys.removed"));
    }

    fn apply_log_dir(&mut self, dir: PathBuf) {
        match crate::logging::set_directory(&dir) {
            Ok(file) => {
                self.log_dir_input = dir.display().to_string();
                self.settings.log_dir = Some(dir);
                self.settings.save();
                self.announce(tf("settings.log_saved", &[("file", &file.display())]));
            }
            Err(e) => self.show_error(tf("settings.log_failed", &[("error", &e)])),
        }
    }

    fn import_wordlist(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title(t("wordlists.import_title"))
            .add_filter(t("wordlists.filter"), &["xml"])
            .pick_file()
        else {
            return;
        };
        match wordlist::import(&path, &crate::paths::wordlist_dir()) {
            Ok(name) => {
                self.reload_wordlists();
                self.announce(tf("wordlists.imported", &[("name", &name)]));
                self.cue(Cue::Success);
            }
            Err(e) => {
                let error = format!("{e:#}");
                self.show_error(tf("wordlists.import_failed", &[("name", &file_name(&path)), ("error", &error)]));
            }
        }
    }

    fn remove_wordlist(&mut self) {
        let Some(item) = self.wordlists.get(self.wordlist_to_remove) else { return };
        let file = item.file_name.clone();
        let title = display_name(item);
        let confirmed = rfd::MessageDialog::new()
            .set_title(t("wordlists.remove_title"))
            .set_description(tf("wordlists.remove_question", &[("name", &title)]))
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            == rfd::MessageDialogResult::Yes;
        if !confirmed {
            return;
        }
        match wordlist::remove(&crate::paths::wordlist_dir(), &file) {
            Ok(()) => {
                self.settings.disabled_wordlists.remove(&file);
                self.settings.save();
                self.reload_wordlists();
                self.announce(tf("wordlists.removed", &[("name", &title)]));
            }
            Err(e) => self.show_error(format!("{e:#}")),
        }
    }

    // ----- language -----------------------------------------------------

    fn current_language(&self) -> Language {
        self.languages
            .iter()
            .find(|l| l.code == self.settings.language)
            .unwrap_or(&self.languages[0])
            .clone()
    }

    /// The Ollama model to translate with: the one chosen for translating if
    /// Ollama has it, otherwise the image description model, otherwise the
    /// first model.
    fn translation_model(&self) -> Option<String> {
        let Some(Loadable::Ready(models)) = &self.models else { return None };
        [&self.settings.translation_model, &self.settings.vision_model]
            .into_iter()
            .find(|m| models.contains(m))
            .or_else(|| models.first())
            .cloned()
    }

    /// Switches the interface to the language at `index` in the list, if it
    /// has been translated. The arrow keys change the list straight away, so
    /// this never opens a dialog of its own.
    fn choose_language(&mut self, index: usize) {
        let Some(language) = self.languages.get(index).cloned() else { return };
        self.settings.language = language.code.clone();
        self.settings.save();
        let label = language.label();
        if language.code == i18n::ENGLISH_CODE {
            i18n::activate(None);
            self.announce(tf("language.chosen", &[("language", &label)]));
            return;
        }
        let dir = crate::paths::languages_dir();
        if !i18n::file_for(&dir, &language.code).exists() {
            i18n::activate(None);
            let button = tf("language.translate", &[("language", &label)]);
            self.announce(tf("language.not_yet", &[("language", &label), ("button", &button)]));
            return;
        }
        match i18n::load(&dir, &language.code) {
            Ok(translation) => {
                // Switch first, so the message is in the new language.
                i18n::activate(Some(&translation));
                let missing = translation.missing().len();
                self.announce(if missing == 0 {
                    tf("language.chosen", &[("language", &label)])
                } else {
                    tf("language.partly", &[("language", &label), ("count", &missing)])
                });
            }
            Err(e) => {
                i18n::activate(None);
                let error = format!("{e:#}");
                self.announce(tf("language.load_failed", &[("language", &label), ("error", &error)]));
            }
        }
    }

    /// Translates the interface into the chosen language with the local AI
    /// model. Only the text not yet translated is sent, unless everything
    /// is, when the user is asked whether to start again.
    fn translate_app(&mut self) {
        if self.is_busy() {
            return;
        }
        let language = self.current_language();
        if language.code == i18n::ENGLISH_CODE {
            return;
        }
        let Some(model) = self.translation_model() else {
            // Ollama may have been started since the last look.
            if !matches!(self.models, Some(Loadable::Loading)) {
                self.models = None;
                self.ensure_models();
            }
            self.show_error(t("language.no_model"));
            return;
        };
        let dir = crate::paths::languages_dir();
        let file = i18n::file_for(&dir, &language.code);
        let mut translation = match i18n::load(&dir, &language.code) {
            Ok(translation) => translation,
            Err(e) => {
                // Keep a damaged file to one side rather than lose someone's
                // corrections, and start again.
                if file.exists() {
                    log::warn!("translation {} is damaged, starting again: {e:#}", language.code);
                    if let Err(e) = std::fs::rename(&file, file.with_extension("json.bak")) {
                        let error = format!("{e:#}");
                        self.show_error(tf("language.load_failed", &[("language", &language.label()), ("error", &error)]));
                        return;
                    }
                }
                Translation::new(&language)
            }
        };
        if translation.missing().is_empty() {
            let again = rfd::MessageDialog::new()
                .set_title(t("language.again_title"))
                .set_description(tf("language.again_question", &[("language", &language.label())]))
                .set_buttons(rfd::MessageButtons::YesNo)
                .set_level(rfd::MessageLevel::Info)
                .show()
                == rfd::MessageDialogResult::Yes;
            if !again {
                return;
            }
            translation.strings.clear();
        }
        let control = Arc::new(Control::default());
        self.job = Some((JobKind::Translating, control.clone()));
        self.progress = 0.0;
        self.announce(tf("language.translating", &[("language", &language.label()), ("model", &model)]));
        self.rep.spawn(move |rep| worker::translate(rep, control, language, model, translation));
    }

    fn translated(&mut self, result: Result<worker::Translated, String>) {
        self.job = None;
        match result {
            Ok(done) => {
                // Use what was translated, even if the job was stopped part
                // way, then report in the new language.
                if self.settings.language == done.language.code {
                    i18n::activate(Some(&done.translation));
                }
                let label = done.language.label();
                if !done.stopped {
                    self.cue(Cue::Success);
                }
                self.announce(if done.stopped {
                    t("language.stopped")
                } else if done.left == 0 {
                    tf("language.done", &[("language", &label)])
                } else {
                    tf("language.done_partly", &[("language", &label), ("count", &done.left)])
                });
            }
            Err(e) => self.show_error(tf("language.failed", &[("error", &e)])),
        }
    }

    // ----- audio player -------------------------------------------------

    fn choose_audio(&mut self) {
        if self.audio_loader.is_some() {
            return;
        }
        if matches!(self.job, Some((JobKind::Transcribing, _))) {
            self.announce(t("player.wait_transcribing"));
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title(t("player.dialog_title"))
            .add_filter(t("player.filter"), &["wav", "mp3", "zip"])
            .pick_file()
        else {
            return;
        };
        if let Some(control) = &self.audio_player {
            control.stop();
        }
        let control = Arc::new(Control::default());
        self.audio_loader = Some(control.clone());
        self.play_when_loaded = false;
        let name = file_name(&path);
        if crate::playlist::is_zip(&path) {
            self.announce(tf("playlist.unpacking", &[("name", &name)]));
            self.rep.spawn(move |rep| worker::open_playlist(rep, control, path));
        } else {
            self.announce(tf("player.opening", &[("name", &name)]));
            self.rep.spawn(move |rep| worker::load_audio(rep, control, path));
        }
    }

    fn playlist_opened(&mut self, zip: PathBuf, result: Result<Option<Playlist>, String>) {
        self.audio_loader = None;
        let name = file_name(&zip);
        let list = match result {
            Ok(Some(list)) => list,
            Ok(None) => return self.announce(tf("player.open_stopped", &[("name", &name)])),
            Err(e) => return self.show_error(tf("file.load_failed", &[("name", &name), ("error", &e)])),
        };
        let count = list.tracks.len();
        let mut intro = if count == 1 {
            tf("playlist.opened_one", &[("name", &name)])
        } else {
            tf("playlist.opened", &[("name", &name), ("count", &count)])
        };
        match list.missing.len() {
            0 => {}
            1 => intro = format!("{intro} {}", t("playlist.missing_one")),
            n => intro = format!("{intro} {}", tf("playlist.missing", &[("count", &n)])),
        }
        if !list.missing.is_empty() {
            log::warn!("listing.txt names files that are not WAV or MP3 files in the zip: {:?}", list.missing);
        }
        self.audio = None;
        self.audio_position = Duration::ZERO;
        self.transcript.clear();
        self.playlist = Some(OpenPlaylist { zip, list, current: 0, offered_models: false });
        self.open_track(0, false, intro);
    }

    /// Opens track `index` of the playlist (counting from 0), stopping the
    /// track before it, and plays it once it has opened if `play` is true.
    /// `intro` is said first.
    fn open_track(&mut self, index: usize, play: bool, intro: String) {
        if self.audio_loader.is_some() {
            return;
        }
        let Some(playlist) = &mut self.playlist else { return };
        let Some(path) = playlist.list.tracks.get(index).cloned() else { return };
        playlist.current = index;
        let count = playlist.list.tracks.len();
        if let Some(control) = &self.audio_player {
            control.stop();
        }
        // Its transcript would be of the track before.
        if let Some((JobKind::Transcribing, control)) = &self.job {
            control.stop();
        }
        let control = Arc::new(Control::default());
        self.audio_loader = Some(control.clone());
        self.play_when_loaded = play;
        let opening = tf("playlist.opening", &[("number", &(index + 1)), ("count", &count), ("name", &file_name(&path))]);
        self.announce(format!("{intro} {opening}").trim().to_owned());
        self.rep.spawn(move |rep| worker::load_audio(rep, control, path));
    }

    /// Moves to the next track of the playlist, or the one before. It plays
    /// if the track it replaces was playing.
    fn change_track(&mut self, forward: bool) {
        let Some(playlist) = &self.playlist else { return };
        let index = if forward { playlist.current + 1 } else { playlist.current.wrapping_sub(1) };
        if index >= playlist.list.tracks.len() {
            self.announce(if forward { t("playlist.at_last") } else { t("playlist.at_first") });
            return;
        }
        let play = self.audio_player.is_some() && !self.audio_paused;
        self.open_track(index, play, String::new());
    }

    fn audio_loaded(&mut self, path: PathBuf, result: Result<Option<Arc<Listened>>, String>) {
        self.audio_loader = None;
        let play = std::mem::take(&mut self.play_when_loaded);
        let name = file_name(&path);
        let audio = match result {
            Ok(Some(audio)) => audio,
            Ok(None) => return self.announce(tf("player.open_stopped", &[("name", &name)])),
            Err(e) => return self.show_error(tf("file.load_failed", &[("name", &name), ("error", &e)])),
        };
        // A file chosen on its own replaces the playlist.
        if self.playlist.as_ref().is_some_and(|p| !p.list.tracks.contains(&path)) {
            self.playlist = None;
        }
        self.audio_position = Duration::ZERO;
        self.transcript.clear();
        self.audio = Some((path, audio.clone()));
        let playing = play && self.start_playing();
        let length = spoken_duration(audio.duration);
        let mut intro = match &self.playlist {
            Some(p) => {
                let key = if playing { "playlist.playing" } else { "playlist.loaded" };
                let count = p.list.tracks.len();
                tf(key, &[("number", &(p.current + 1)), ("count", &count), ("name", &name), ("length", &length)])
            }
            None => tf("player.loaded", &[("name", &name), ("length", &length)]),
        };
        if audio.sound == SoundKind::Speech {
            if audio.cut_short {
                intro = format!("{intro} {}", t("player.cut_short"));
            }
            // In a playlist, only the first track offers to download the
            // models for transcribing, so the questions don't keep coming up.
            let may_ask = match &mut self.playlist {
                Some(p) => !std::mem::replace(&mut p.offered_models, true),
                None => true,
            };
            if may_ask || self.transcribing_models_ready() {
                let intro = format!("{intro} {}", t("player.speech"));
                // The track before is still being transcribed, and has been
                // told to stop.
                if matches!(self.job, Some((JobKind::Transcribing, _))) {
                    self.transcribe_when_free = true;
                    return self.announce(intro);
                }
                return self.transcribe_audio(intro);
            }
        }
        let note = match audio.sound {
            SoundKind::Silent => t("player.silent"),
            SoundKind::Speech => tf("playlist.not_transcribed", &[("button", &t("player.transcribe"))]),
            SoundKind::Other if playing => String::new(),
            SoundKind::Other => tf("player.not_speech", &[("button", &t("player.transcribe"))]),
        };
        self.announce(format!("{intro} {note}").trim().to_owned());
        // Speech is transcribed next, and that gets the sound when it's done.
        self.cue(Cue::Success);
    }

    /// The models for transcribing, with the speakers labelled if that is
    /// chosen, are on the computer.
    fn transcribing_models_ready(&self) -> bool {
        self.settings.whisper_model.is_downloaded()
            && (self.settings.speaker_labels == SpeakerLabels::Off || crate::speakers::models_downloaded())
    }

    /// Plays the audio file from where it was left, or resumes it if paused.
    fn play_audio(&mut self) {
        if self.audio_player.is_some() {
            if self.audio_paused {
                self.toggle_audio_pause();
            }
            return;
        }
        if self.audio.is_none() {
            // A playlist whose track didn't open, such as when Escape was
            // pressed while it was opening.
            if let Some(p) = &self.playlist {
                return self.open_track(p.current, true, String::new());
            }
            self.announce(tf("player.nothing", &[("key_name", &MOD_KEY.1)]));
            return;
        }
        if self.start_playing() {
            self.announce(t("player.playing"));
        }
    }

    /// Starts the audio file playing from where it was left. Returns false,
    /// having said why, if it can't.
    fn start_playing(&mut self) -> bool {
        let Some((path, audio)) = &self.audio else { return false };
        if matches!(self.job, Some((JobKind::Speaking | JobKind::Previewing, _))) {
            self.announce(t("player.reading_aloud"));
            return false;
        }
        // Start again from the beginning if the last play reached the end.
        let start = if self.audio_position + Duration::from_millis(500) >= audio.duration {
            Duration::ZERO
        } else {
            self.audio_position
        };
        self.cues.stop();
        let path = path.clone();
        let control = Arc::new(Control::default());
        self.audio_player = Some(control.clone());
        self.audio_paused = false;
        self.audio_position = start;
        self.rep.spawn(move |rep| worker::play_audio(rep, control, path, start));
        true
    }

    fn toggle_audio_pause(&mut self) {
        let Some(control) = &self.audio_player else { return };
        self.audio_paused = !self.audio_paused;
        control.set_paused(self.audio_paused);
        if !self.audio_paused {
            self.cues.stop();
        }
        let msg = if self.audio_paused {
            tf("player.paused", &[("position", &spoken_duration(self.audio_position))])
        } else {
            t("read.resumed")
        };
        self.announce(msg);
    }

    /// Stops the audio file playing, or being opened.
    fn stop_audio(&mut self) {
        for control in [&self.audio_player, &self.audio_loader].into_iter().flatten() {
            control.stop();
        }
    }

    /// Moves `seconds` forward (or back, if negative) in the audio file.
    fn skip_audio(&mut self, seconds: f64) {
        let Some((_, audio)) = &self.audio else { return };
        let to = (self.audio_position.as_secs_f64() + seconds).clamp(0.0, audio.duration.as_secs_f64());
        self.audio_position = Duration::from_secs_f64(to);
        if let Some(control) = &self.audio_player {
            control.seek(self.audio_position);
        }
        self.announce_audio_position();
    }

    /// Says where in the audio file playback is (F7 while it plays).
    fn announce_audio_position(&mut self) {
        let Some((_, audio)) = &self.audio else { return };
        let length = spoken_duration(audio.duration);
        let position = spoken_duration(self.audio_position);
        let key = if self.audio_paused { "player.position_paused" } else { "player.position" };
        self.announce(tf(key, &[("position", &position), ("length", &length)]));
    }

    /// Transcribes the audio file with Whisper, labelling the speakers if
    /// that is chosen, first offering to download the models that aren't on
    /// the computer yet. `intro` is said first.
    fn transcribe_audio(&mut self, intro: String) {
        self.transcribe_audio_labelled(intro, self.settings.speaker_labels);
    }

    fn transcribe_audio_labelled(&mut self, intro: String, labels: SpeakerLabels) {
        let Some((path, audio)) = self.audio.clone() else { return };
        if self.is_busy() {
            self.announce(format!("{intro} {}", t("transcript.busy")).trim().to_owned());
            return;
        }
        let model = self.settings.whisper_model;
        if !model.is_downloaded() {
            self.offer_whisper_download(intro);
            return;
        }
        if labels != SpeakerLabels::Off && !crate::speakers::models_downloaded() {
            self.offer_speaker_download(intro);
            return;
        }
        let control = Arc::new(Control::default());
        self.job = Some((JobKind::Transcribing, control.clone()));
        self.progress = 0.0;
        self.transcript.clear();
        let started = tf("transcript.started", &[("model", &model.name())]);
        self.announce(format!("{intro} {started}").trim().to_owned());
        self.rep.spawn(move |rep| worker::transcribe(rep, control, model, path, audio, labels));
    }

    /// The models that tell speakers apart aren't on the computer. Asks
    /// whether to download them, and transcribes the audio file when they
    /// arrive. If not, it is transcribed without labelling the speakers.
    fn offer_speaker_download(&mut self, intro: String) {
        let size = tf("model.size", &[("size", &crate::speakers::MODELS_SIZE)]);
        if !intro.is_empty() {
            self.announce(intro);
        }
        let yes = rfd::MessageDialog::new()
            .set_title(t("speakers.download_title"))
            .set_description(tf("speakers.download_question", &[("size", &size)]))
            .set_buttons(rfd::MessageButtons::YesNo)
            .set_level(rfd::MessageLevel::Info)
            .show()
            == rfd::MessageDialogResult::Yes;
        if !yes {
            self.transcribe_audio_labelled(t("speakers.not_downloaded"), SpeakerLabels::Off);
            return;
        }
        let control = Arc::new(Control::default());
        self.job = Some((JobKind::Downloading, control.clone()));
        self.progress = 0.0;
        self.transcribe_after_download = true;
        self.announce(tf("speakers.downloading", &[("size", &size)]));
        self.rep.spawn(move |rep| worker::download_speaker_models(rep, control));
    }

    /// The Whisper model isn't on the computer. Asks whether to download it,
    /// and transcribes the audio file when it arrives.
    fn offer_whisper_download(&mut self, intro: String) {
        let model = self.settings.whisper_model;
        let size = tf("model.size", &[("size", &model.size())]);
        if !intro.is_empty() {
            self.announce(intro);
        }
        let yes = rfd::MessageDialog::new()
            .set_title(t("whisper.download_title"))
            .set_description(tf("whisper.download_question", &[("model", &model.name()), ("size", &size)]))
            .set_buttons(rfd::MessageButtons::YesNo)
            .set_level(rfd::MessageLevel::Info)
            .show()
            == rfd::MessageDialogResult::Yes;
        if !yes {
            self.announce(tf("whisper.not_downloaded", &[("button", &t("player.transcribe"))]));
            return;
        }
        let control = Arc::new(Control::default());
        self.job = Some((JobKind::Downloading, control.clone()));
        self.progress = 0.0;
        self.transcribe_after_download = true;
        self.announce(tf("whisper.downloading", &[("model", &model.name()), ("size", &size)]));
        self.rep.spawn(move |rep| worker::download_whisper(rep, control, model));
    }

    fn transcribed(&mut self, path: PathBuf, result: Result<Option<worker::Transcript>, String>) {
        self.job = None;
        // A track of a playlist that has since been left, which was told to
        // stop. The track now chosen may be waiting its turn.
        let current = self.audio_loader.is_none() && self.audio.as_ref().is_some_and(|(p, _)| *p == path);
        if self.playlist.is_some() && !current {
            if let Err(e) = result {
                log::warn!("transcribing the track before failed: {e}");
            }
            if std::mem::take(&mut self.transcribe_when_free) {
                self.transcribe_audio(String::new());
            }
            return;
        }
        let name = file_name(&path);
        match result {
            Ok(None) => self.announce(t("transcript.stopped")),
            Ok(Some(done)) if done.text.trim().is_empty() => {
                self.announce(tf("transcript.no_speech", &[("name", &name)]))
            }
            Ok(Some(done)) => {
                // Keep it only if the same file is still chosen.
                if self.audio.as_ref().is_some_and(|(p, _)| *p == path) {
                    self.transcript = done.text;
                    let mut msg =
                        tf("transcript.done", &[("name", &name), ("count", &done.words), ("key_name", &MOD_KEY.1)]);
                    match done.speakers {
                        Some(1) => msg = format!("{msg} {}", t("speakers.found_one")),
                        Some(n) => msg = format!("{msg} {}", tf("speakers.found", &[("count", &n)])),
                        None => {}
                    }
                    self.announce(msg);
                    self.cue(Cue::Success);
                }
            }
            Err(e) => self.show_error(tf("transcript.failed", &[("error", &e)])),
        }
    }

    fn copy_transcript(&mut self, ctx: &egui::Context) {
        if self.transcript.is_empty() {
            self.announce(t("copy.nothing"));
            return;
        }
        ctx.copy_text(self.transcript.clone());
        let words = self.transcript.split_whitespace().count();
        self.announce(tf("copy.done", &[("count", &words)]));
    }

    fn save_transcript(&mut self) {
        if self.transcript.is_empty() {
            self.announce(t("transcript.nothing"));
            return;
        }
        let stem = self
            .audio
            .as_ref()
            .and_then(|(p, _)| p.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| t("transcript.default_name"));
        let Some(mut path) = rfd::FileDialog::new()
            .set_title(t("transcript.save_title"))
            .set_file_name(format!("{stem}.txt"))
            .add_filter(t("transcript.filter"), &["txt"])
            .save_file()
        else {
            return;
        };
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("txt")) {
            path.as_mut_os_string().push(".txt");
        }
        let name = file_name(&path);
        match std::fs::write(&path, &self.transcript) {
            Ok(()) => {
                self.announce(tf("transcript.saved", &[("name", &name)]));
                self.cue(Cue::Success);
            }
            Err(e) => self.show_error(tf("transcript.save_failed", &[("name", &name), ("error", &e)])),
        }
    }

    // ----- keyboard -----------------------------------------------------

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let pressed = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));
        let popup_open = ctx.any_popup_open();

        for (key, tab) in [Key::Num1, Key::Num2, Key::Num3, Key::Num4].into_iter().zip(Tab::ALL) {
            if pressed(cmd(key)) {
                self.switch_tab(tab);
            }
        }
        let count = Tab::ALL.len();
        if pressed(KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab)) {
            let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
            self.switch_tab(Tab::ALL[(i + count - 1) % count]);
        } else if pressed(KeyboardShortcut::new(Modifiers::CTRL, Key::Tab)) {
            let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
            self.switch_tab(Tab::ALL[(i + 1) % count]);
        }
        // On the Audio Player tab, the usual keys work on the audio file.
        let on_player = self.tab == Tab::Player;
        if pressed(cmd(Key::O)) {
            if on_player { self.choose_audio() } else { self.choose_file() }
        }
        if pressed(cmd(Key::S)) {
            if on_player { self.save_transcript() } else { self.save_audio() }
        }
        if pressed(KeyboardShortcut::new(Modifiers::NONE, Key::F5)) {
            if on_player { self.play_audio() } else { self.read_aloud() }
        }
        // F6, F7 and Escape work on the audio file wherever it is playing.
        let audio_playing = self.audio_player.is_some();
        if pressed(KeyboardShortcut::new(Modifiers::NONE, Key::F6)) {
            if audio_playing { self.toggle_audio_pause() } else { self.toggle_pause() }
        }
        if pressed(KeyboardShortcut::new(Modifiers::NONE, Key::F7)) {
            if audio_playing { self.announce_audio_position() } else { self.announce_progress() }
        }
        // Escape also stops an audio file being opened.
        let audio_active = audio_playing || self.audio_loader.is_some();
        if !popup_open
            && (audio_active || self.is_busy())
            && pressed(KeyboardShortcut::new(Modifiers::NONE, Key::Escape))
        {
            if audio_active { self.stop_audio() } else { self.stop() }
        }
    }

    fn switch_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.focus = Some(tab.id());
    }

    // ----- drawing ------------------------------------------------------

    fn tab_bar(&mut self, ui: &mut Ui) {
        ui.columns(Tab::ALL.len(), |cols| {
            for (col, tab) in cols.iter_mut().zip(Tab::ALL) {
                let selected = self.tab == tab;
                let text = RichText::new(tab.label()).family(tab_font()).size(18.0);
                let text = if selected { text.strong() } else { text };
                let resp = col
                    .push_id(tab.id(), |ui| {
                        ui.add_sized([ui.available_width(), 40.0], Button::selectable(selected, text))
                    })
                    .inner;
                col.ctx().accesskit_node_builder(resp.id, |node| {
                    node.set_role(Role::Tab);
                    node.set_selected(selected);
                });
                if self.focus == Some(tab.id()) {
                    resp.request_focus();
                    self.focus = None;
                }
                if resp.clicked() && !selected {
                    self.switch_tab(tab);
                    self.announce(tf("tab.chosen", &[("tab", &tab.label())]));
                }
            }
        });
    }

    fn general_tab(&mut self, ui: &mut Ui) {
        heading(ui, &t("general.heading"));

        let labels: Vec<String> = Source::ALL.iter().map(|s| s.label()).collect();
        let mut sindex = Source::ALL.iter().position(|s| *s == self.source).unwrap_or(0);
        if dropdown(ui, "source", &t("general.source"), &labels, &mut sindex, !self.is_busy()) {
            self.source = Source::ALL[sindex];
            self.announce(match self.source {
                Source::File => tf("general.source_chosen_file", &[("key_name", &MOD_KEY.1)]),
                Source::Paste => t("general.source_chosen_paste"),
            });
        }

        match self.source {
            Source::File => {
                let mut file_text = match (&self.file, self.loading_file) {
                    (_, true) => t("general.loading"),
                    (Some(p), _) => p.display().to_string(),
                    (None, _) => t("general.no_file"),
                };
                text_field(ui, &t("general.current_file"), &mut file_text, false);

                let label = tf("general.choose_file", &[("key", &MOD_KEY.0)]);
                if full_button(ui, &label, !self.loading_file).clicked() {
                    self.choose_file();
                }
            }
            Source::Paste => {
                let label = ui.label(t("general.pasted"));
                let enabled = !self.is_busy();
                let edit = egui::TextEdit::multiline(&mut self.pasted)
                    .id_salt("pasted_text")
                    .char_limit(crate::document::MAX_PASTED_CHARS)
                    .desired_width(f32::INFINITY)
                    .desired_rows(10);
                let resp = ui.add_enabled(enabled, edit).labelled_by(label.id);
                if resp.changed() {
                    self.pasted_text = crate::document::clean_pasted(&self.pasted);
                    if self.pasted.chars().count() >= crate::document::MAX_PASTED_CHARS {
                        let count = speech::format_count(crate::document::MAX_PASTED_CHARS);
                        self.announce(tf("general.pasted_full", &[("count", &count)]));
                    }
                }
            }
        }

        // Speech service.
        let providers = self.available_providers();
        let mut index = providers.iter().position(|p| *p == self.active_provider()).unwrap_or(0);
        let labels: Vec<String> = providers.iter().map(|p| p.name()).collect();
        if dropdown(ui, "provider", &t("general.service"), &labels, &mut index, !self.is_busy()) {
            let provider = providers[index];
            self.settings.provider = provider;
            self.settings.save();
            self.announce(tf(
                "general.service_chosen",
                &[("service", &provider.name()), ("note", &provider.privacy_note())],
            ));
        }
        // Where the speech is made, so it is clear when text leaves the computer.
        ui.label(self.active_provider().privacy_note());

        // Voice.
        let provider = self.active_provider();
        self.ensure_voices(provider);
        let voices = self.voice_list(provider).to_vec();
        let (names, empty_text) = match self.voices.get(&provider) {
            Some(Loadable::Ready(v)) if v.is_empty() => (vec![], "general.no_voices"),
            Some(Loadable::Ready(v)) => (v.iter().map(voice_name).collect(), ""),
            Some(Loadable::Failed) => (vec![], "general.voices_failed"),
            _ => (vec![], "general.loading_voices"),
        };
        let names = if names.is_empty() { vec![t(empty_text)] } else { names };
        let current = self.current_voice(provider).map(|v| v.id.clone());
        let mut vindex = voices.iter().position(|v| Some(&v.id) == current.as_ref()).unwrap_or(0);
        if dropdown(ui, "voice", &t("general.voice"), &names, &mut vindex, !voices.is_empty() && !self.is_busy())
            && let Some(v) = voices.get(vindex) {
                self.settings.set_voice(provider, v.id.clone());
                self.settings.save();
            }
        if matches!(self.voices.get(&provider), Some(Loadable::Failed))
            && full_button(ui, &t("general.retry_voices"), true).clicked()
        {
            self.voices.remove(&provider);
        }

        // Speaking speed, only for services that have a speed setting.
        let speeds = provider.speed_choices();
        if !speeds.is_empty() {
            let labels: Vec<String> = speeds.iter().map(|s| speech::speed_label(*s)).collect();
            let current = self.settings.speed_for(provider);
            let mut sindex = speeds.iter().position(|s| *s == current).unwrap_or(0);
            if dropdown(ui, "speed", &t("general.speed"), &labels, &mut sindex, !self.is_busy()) {
                self.settings.set_speed(provider, speeds[sindex]);
                self.settings.save();
            }
        }
        let can_preview = !self.is_busy() && (provider == Provider::System || !voices.is_empty());
        if full_button(ui, &t("general.preview"), can_preview).clicked() {
            self.preview_voice();
        }

        let has_text = !self.current_text().is_empty();
        let speaking = matches!(self.job, Some((JobKind::Speaking, _)));
        if full_button(ui, &t("general.read_aloud"), has_text && !self.is_busy()).clicked() {
            self.read_aloud();
        }
        let pause_label = if self.paused { t("general.resume") } else { t("general.pause") };
        if full_button(ui, &pause_label, speaking).clicked() {
            self.toggle_pause();
        }
        let stop_label = match self.job {
            Some((JobKind::Saving, _)) => t("general.cancel_saving"),
            Some((JobKind::Downloading, _)) => t("general.stop_downloading"),
            Some((JobKind::Translating, _)) => t("general.stop_translating"),
            Some((JobKind::Transcribing, _)) => self.stop_transcribing_label(),
            _ => t("general.stop"),
        };
        if full_button(ui, &stop_label, self.is_busy()).clicked() {
            self.stop();
        }

        let formats = [AudioFormat::Mp3, AudioFormat::Wav];
        let labels: Vec<String> = formats.iter().map(|f| f.label()).collect();
        let mut findex = formats.iter().position(|f| *f == self.settings.audio_format).unwrap_or(0);
        if dropdown(ui, "format", &t("general.format"), &labels, &mut findex, !self.is_busy()) {
            self.settings.audio_format = formats[findex];
            self.settings.save();
        }
        if full_button(ui, &tf("general.save", &[("key", &MOD_KEY.0)]), has_text && !self.is_busy()).clicked() {
            self.save_audio();
        }
        if full_button(ui, &t("general.copy"), has_text).clicked() {
            let ctx = ui.ctx().clone();
            self.copy_text(&ctx);
        }
    }

    fn settings_tab(&mut self, ui: &mut Ui) {
        heading(ui, &t("settings.heading"));
        self.language_section(ui);

        // Image description model.
        let (models, shown) = self.model_choices();
        let mut mindex = models.iter().position(|m| *m == self.settings.vision_model).unwrap_or(0);
        if dropdown(ui, "model", &t("settings.model"), &shown, &mut mindex, !models.is_empty()) {
            self.settings.vision_model = models[mindex].clone();
            self.settings.save();
        }
        // Offered while Ollama is running without a model that describes photos.
        if let Some(Loadable::Ready(m)) = &self.models
            && !crate::vision::has_vision_model(m)
        {
            let label =
                tf("settings.download_model", &[("model", &crate::vision::SUGGESTED_MODEL), ("size", &model_size())]);
            if full_button(ui, &label, !self.is_busy()).clicked() {
                self.offer_model_download(None);
            }
        }
        if full_button(ui, &t("settings.refresh_models"), !matches!(self.models, Some(Loadable::Loading))).clicked() {
            self.models = None;
            self.ensure_models();
            self.announce(t("models.looking"));
        }

        let location_options = vec![t("settings.location_off"), t("settings.location_on")];
        let mut lindex = usize::from(self.settings.resolve_location);
        if dropdown(ui, "location", &t("settings.location"), &location_options, &mut lindex, true) {
            self.settings.resolve_location = lindex == 1;
            self.settings.save();
        }

        // Speech recognition model for the Audio Player tab.
        let whisper_models = WhisperModel::ALL;
        let labels: Vec<String> = whisper_models.iter().map(|m| whisper_label(*m)).collect();
        let mut windex = whisper_models.iter().position(|m| *m == self.settings.whisper_model).unwrap_or(0);
        let whisper_busy = matches!(self.job, Some((JobKind::Transcribing | JobKind::Downloading, _)));
        if dropdown(ui, "whisper", &t("settings.whisper"), &labels, &mut windex, !whisper_busy) {
            self.settings.whisper_model = whisper_models[windex];
            self.settings.save();
        }
        ui.label(tf("settings.whisper_note", &[("folder", &crate::paths::whisper_dir().display())]));

        let part_options = vec![t("settings.parts_run_on"), t("settings.parts_announce")];
        let mut pindex = usize::from(self.settings.announce_parts);
        let label = tf("settings.parts", &[("count", &speech::format_count(speech::PART_CHARS))]);
        if dropdown(ui, "parts", &label, &part_options, &mut pindex, true) {
            self.settings.announce_parts = pindex == 1;
            self.settings.save();
        }

        let sound_options = vec![t("settings.sounds_on"), t("settings.sounds_off")];
        let mut sindex = usize::from(!self.settings.sounds);
        if dropdown(ui, "sounds", &t("settings.sounds"), &sound_options, &mut sindex, true) {
            self.settings.sounds = sindex == 0;
            self.settings.save();
        }

        let update_options = vec![t("settings.updates_on"), t("settings.updates_off")];
        let mut uindex = usize::from(!self.settings.check_updates);
        if dropdown(ui, "updates", &t("settings.updates"), &update_options, &mut uindex, true) {
            self.settings.check_updates = uindex == 0;
            self.settings.save();
        }
        if full_button(ui, &t("settings.check_now"), !self.checking_update).clicked() {
            self.check_for_update();
        }

        let mut dir = self.log_dir_input.clone();
        if text_field(ui, &t("settings.log_folder"), &mut dir, true).changed() {
            self.log_dir_input = dir;
        }
        if full_button(ui, &t("settings.choose_log"), true).clicked()
            && let Some(dir) = rfd::FileDialog::new()
                .set_title(t("settings.log_dialog"))
                .set_directory(self.settings.log_dir())
                .pick_folder()
            {
                self.apply_log_dir(dir);
            }
        if full_button(ui, &t("settings.use_typed_log"), true).clicked() {
            let typed = PathBuf::from(self.log_dir_input.trim());
            if typed.is_absolute() {
                self.apply_log_dir(typed);
            } else {
                self.show_error(t("settings.full_path"));
            }
        }

        ui.add_space(8.0);
        heading(ui, &t("keys.heading"));
        ui.label(tf("keys.stored", &[("place", &crate::platform::secret_store_description())]));
        for provider in Provider::ALL {
            if provider.key_name().is_none() {
                continue;
            }
            let key = if self.api_keys.contains_key(&provider) { "keys.label_saved" } else { "keys.label_none" };
            let label = tf(key, &[("service", &provider.name())]);
            let input = self.key_inputs.entry(provider).or_default();
            password_field(ui, &label, input);
        }
        if full_button(ui, &t("keys.save"), true).clicked() {
            self.save_keys();
        }
        if full_button(ui, &t("keys.remove"), !self.api_keys.is_empty()).clicked() {
            self.remove_keys();
        }
    }

    /// The Ollama models, and what to show in a list of them: the models, or
    /// a line saying why there are none.
    fn model_choices(&mut self) -> (Vec<String>, Vec<String>) {
        self.ensure_models();
        let (models, placeholder) = match &self.models {
            Some(Loadable::Ready(m)) if !m.is_empty() => (m.clone(), String::new()),
            Some(Loadable::Ready(_)) => (vec![], t("settings.no_models")),
            Some(Loadable::Failed) => (vec![], t("settings.ollama_off")),
            _ => (vec![], t("settings.looking")),
        };
        let shown = if models.is_empty() { vec![placeholder] } else { models.clone() };
        (models, shown)
    }

    fn language_section(&mut self, ui: &mut Ui) {
        let translating = matches!(self.job, Some((JobKind::Translating, _)));
        let labels: Vec<String> = self.languages.iter().map(Language::label).collect();
        let mut index = self.languages.iter().position(|l| l.code == self.settings.language).unwrap_or(0);
        // In another language, the label also says "Language" in English, so
        // anyone who chose a language by mistake can find their way back.
        let mut label = t("language.label");
        if self.settings.language != i18n::ENGLISH_CODE {
            label = format!("{label} (Language)");
        }
        if dropdown(ui, "language", &label, &labels, &mut index, !translating) {
            self.choose_language(index);
        }
        let language = self.current_language();
        if language.code == i18n::ENGLISH_CODE {
            return;
        }
        let (models, shown) = self.model_choices();
        let chosen = self.translation_model();
        let mut mindex = models.iter().position(|m| Some(m) == chosen.as_ref()).unwrap_or(0);
        if dropdown(ui, "translation_model", &t("language.model"), &shown, &mut mindex, !models.is_empty() && !translating) {
            self.settings.translation_model = models[mindex].clone();
            self.settings.save();
        }
        let button = tf("language.translate", &[("language", &language.label())]);
        if full_button(ui, &button, !self.is_busy()).clicked() {
            self.translate_app();
        }
        ui.label(tf("language.note", &[("folder", &crate::paths::languages_dir().display())]));
        ui.add_space(8.0);
    }

    fn wordlists_tab(&mut self, ui: &mut Ui) {
        heading(ui, &t("wordlists.heading"));
        ui.label(t("wordlists.intro"));
        if full_button(ui, &t("wordlists.import"), true).clicked() {
            self.import_wordlist();
        }
        if full_button(ui, &t("wordlists.reload"), true).clicked() {
            self.reload_wordlists();
            self.announce(tf("wordlists.count", &[("count", &self.wordlists.len())]));
        }

        ui.add_space(8.0);
        heading(ui, &t("wordlists.installed"));
        if self.wordlists.is_empty() {
            ui.label(t("wordlists.none"));
        }
        let mut toggled = None;
        for item in &self.wordlists {
            match &item.list {
                Ok(list) => {
                    let mut enabled = !self.settings.disabled_wordlists.contains(&item.file_name);
                    let description = if list.description.is_empty() { &item.file_name } else { &list.description };
                    let text = tf(
                        "wordlists.item",
                        &[("name", &list.name), ("description", description), ("count", &list.entries.len())],
                    );
                    let resp = ui.add_sized(
                        [ui.available_width(), CONTROL_HEIGHT],
                        egui::Checkbox::new(&mut enabled, text),
                    );
                    if resp.changed() {
                        toggled = Some((item.file_name.clone(), enabled, list.name.clone()));
                    }
                }
                Err(e) => {
                    let text = tf("wordlists.unreadable", &[("file", &item.file_name), ("error", e)]);
                    ui.label(RichText::new(text).color(error_color(ui)));
                }
            }
        }
        if let Some((file, enabled, name)) = toggled {
            if enabled {
                self.settings.disabled_wordlists.remove(&file);
            } else {
                self.settings.disabled_wordlists.insert(file);
            }
            self.settings.save();
            let key = if enabled { "wordlists.enabled" } else { "wordlists.disabled" };
            self.announce(tf(key, &[("name", &name)]));
        }

        if !self.wordlists.is_empty() {
            ui.add_space(8.0);
            let names: Vec<String> = self.wordlists.iter().map(display_name).collect();
            let mut index = self.wordlist_to_remove;
            if dropdown(ui, "remove_wordlist", &t("wordlists.to_remove"), &names, &mut index, true) {
                self.wordlist_to_remove = index;
            }
            if full_button(ui, &t("wordlists.remove"), true).clicked() {
                self.remove_wordlist();
            }
        }
    }

    fn player_tab(&mut self, ui: &mut Ui) {
        heading(ui, &t("player.heading"));
        let transcribing = matches!(self.job, Some((JobKind::Transcribing, _)));

        let mut file_text = match (&self.audio, &self.playlist, self.audio_loader.is_some()) {
            (_, _, true) => t("general.loading"),
            (_, Some(p), _) => tf(
                "playlist.current_file",
                &[
                    ("zip", &p.zip.display()),
                    ("number", &(p.current + 1)),
                    ("count", &p.list.tracks.len()),
                    ("name", &file_name(&p.list.tracks[p.current])),
                ],
            ),
            (Some((p, _)), _, _) => p.display().to_string(),
            (None, _, _) => t("general.no_file"),
        };
        text_field(ui, &t("general.current_file"), &mut file_text, false);
        let label = tf("player.choose", &[("key", &MOD_KEY.0)]);
        if full_button(ui, &label, self.audio_loader.is_none() && !transcribing).clicked() {
            self.choose_audio();
        }
        if let Some(p) = &self.playlist {
            let tracks: Vec<String> = p
                .list
                .tracks
                .iter()
                .enumerate()
                .map(|(i, track)| tf("playlist.track", &[("number", &(i + 1)), ("name", &file_name(track))]))
                .collect();
            let current = p.current;
            let mut index = current;
            if dropdown(ui, "track", &t("playlist.label"), &tracks, &mut index, self.audio_loader.is_none())
                && index != current
            {
                let play = self.audio_player.is_some() && !self.audio_paused;
                self.open_track(index, play, String::new());
            }
        }

        if let Some((_, audio)) = &self.audio {
            let sound = match audio.sound {
                SoundKind::Speech => t("player.kind_speech"),
                SoundKind::Other => t("player.kind_other"),
                SoundKind::Silent => t("player.kind_silent"),
            };
            let summary = tf("player.summary", &[("length", &spoken_duration(audio.duration)), ("sound", &sound)]);
            ui.label(&summary);
            waveform(ui, &audio.waveform, self.audio_fraction(), &tf("player.waveform", &[("summary", &summary)]));
        }

        let has_audio = self.audio.is_some();
        let playing = self.audio_player.is_some();
        let speaking = matches!(self.job, Some((JobKind::Speaking | JobKind::Previewing, _)));
        if full_button(ui, &t("player.play"), has_audio && !playing && !speaking).clicked() {
            self.play_audio();
        }
        let pause_label = if self.audio_paused { t("general.resume") } else { t("general.pause") };
        if full_button(ui, &pause_label, playing).clicked() {
            self.toggle_audio_pause();
        }
        if full_button(ui, &t("general.stop"), playing || self.audio_loader.is_some()).clicked() {
            self.stop_audio();
        }
        if full_button(ui, &t("player.back"), has_audio).clicked() {
            self.skip_audio(-10.0);
        }
        if full_button(ui, &t("player.forward"), has_audio).clicked() {
            self.skip_audio(10.0);
        }
        if self.playlist.is_some() {
            let can_change = self.audio_loader.is_none();
            if full_button(ui, &t("playlist.previous"), can_change).clicked() {
                self.change_track(false);
            }
            if full_button(ui, &t("playlist.next"), can_change).clicked() {
                self.change_track(true);
            }
        }

        ui.add_space(8.0);
        heading(ui, &t("transcript.heading"));
        let choices = SpeakerLabels::choices();
        let labels: Vec<String> = choices.iter().map(|c| speaker_choice_label(*c)).collect();
        let mut sindex = choices.iter().position(|c| *c == self.settings.speaker_labels).unwrap_or(0);
        let downloading = matches!(self.job, Some((JobKind::Downloading, _)));
        if dropdown(ui, "speakers", &t("speakers.label"), &labels, &mut sindex, !transcribing && !downloading) {
            self.settings.speaker_labels = choices[sindex];
            self.settings.save();
        }
        if self.settings.speaker_labels == SpeakerLabels::Auto {
            ui.label(t("speakers.auto_note"));
        }
        if transcribing {
            if full_button(ui, &self.stop_transcribing_label(), true).clicked() {
                self.stop();
            }
        } else if full_button(ui, &t("player.transcribe"), has_audio && !self.is_busy()).clicked() {
            self.transcribe_audio(String::new());
        }
        let text = if self.transcript.is_empty() { t("transcript.none") } else { self.transcript.clone() };
        text_area(ui, &t("transcript.label"), &text);
        let has_transcript = !self.transcript.is_empty();
        if full_button(ui, &t("transcript.copy"), has_transcript).clicked() {
            let ctx = ui.ctx().clone();
            self.copy_transcript(&ctx);
        }
        if full_button(ui, &tf("transcript.save", &[("key", &MOD_KEY.0)]), has_transcript).clicked() {
            self.save_transcript();
        }
    }

    /// While the audio file plays, Escape stops it rather than transcribing,
    /// so the button doesn't offer Escape then.
    fn stop_transcribing_label(&self) -> String {
        if self.audio_player.is_some() { t("transcript.stop") } else { t("general.stop_transcribing") }
    }

    /// How far through the audio file playback is, from 0.0 to 1.0.
    fn audio_fraction(&self) -> f32 {
        match &self.audio {
            Some((_, audio)) if !audio.duration.is_zero() => {
                (self.audio_position.as_secs_f32() / audio.duration.as_secs_f32()).min(1.0)
            }
            _ => 0.0,
        }
    }

    /// A full-width progress bar for reading aloud, saving audio and loading
    /// files. Screen readers get it as a progress indicator named "Progress",
    /// with a percentage value.
    fn progress_bar(&self, ui: &mut Ui) {
        let (fraction, key) = match &self.job {
            Some((JobKind::Speaking, _)) if self.paused => (Some(self.progress), "bar.paused"),
            Some((JobKind::Speaking, _)) => (Some(self.progress), "bar.reading"),
            Some((JobKind::Previewing, _)) => (Some(self.progress), "bar.previewing"),
            Some((JobKind::Saving, _)) => (Some(self.progress), "bar.saving"),
            Some((JobKind::Downloading, _)) => (Some(self.progress), "bar.downloading"),
            Some((JobKind::Translating, _)) => (Some(self.progress), "bar.translating"),
            Some((JobKind::Transcribing, _)) => (Some(self.progress), "bar.transcribing"),
            None if self.setting_up_ollama => (None, "bar.setting_up"),
            None if self.loading_file || self.audio_loader.is_some() => (None, "bar.opening"),
            None if self.audio_player.is_some() => {
                let key = if self.audio_paused { "bar.audio_paused" } else { "bar.audio_playing" };
                (Some(self.audio_fraction()), key)
            }
            None => (Some(0.0), "bar.nothing"),
        };
        let percent = format!("{:.0}", fraction.unwrap_or(0.0) * 100.0);
        let text = tf(key, &[("percent", &percent)]);
        let bar = egui::ProgressBar::new(fraction.unwrap_or(0.0))
            .desired_width(ui.available_width())
            .desired_height(24.0)
            // An unknown amount of work (opening a file) shows as an animation.
            .animate(fraction.is_none());
        let resp = ui.add(bar);
        // egui draws a bar's own text at the left edge, so the text is drawn
        // here instead, centred, in the font and colour the bar would use.
        let galley = egui::WidgetText::from(text.as_str()).into_galley(
            ui,
            Some(egui::TextWrapMode::Truncate),
            resp.rect.width(),
            egui::TextStyle::Button,
        );
        let colour = ui.visuals().override_text_color.unwrap_or(ui.visuals().selection.stroke.color);
        let pos = resp.rect.center() - galley.size() / 2.0;
        ui.painter().with_clip_rect(resp.rect).galley(pos, galley, colour);
        ui.ctx().accesskit_node_builder(resp.id, |node| {
            node.set_label(t("bar.label"));
            node.set_value(text);
            if let Some(f) = fraction {
                node.set_min_numeric_value(0.0);
                node.set_max_numeric_value(100.0);
                node.set_numeric_value(f64::from((f * 100.0).round()));
            }
        });
    }

    fn status_bar(&self, ui: &mut Ui) {
        let resp = ui.add(egui::Label::new(RichText::new(&self.status).size(16.0)).wrap());
        ui.ctx().accesskit_node_builder(resp.id, |node| {
            node.set_live(Live::Polite);
        });
    }
}

impl eframe::App for SpeechApp {
    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.fit_to_screen(&ctx);
        self.handle_messages();
        self.handle_shortcuts(&ctx);

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.add_space(4.0);
            self.tab_bar(ui);
            ui.add_space(4.0);
        });
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(6.0);
            self.progress_bar(ui);
            ui.add_space(4.0);
            self.status_bar(ui);
            ui.add_space(4.0);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 10.0;
                match self.tab {
                    Tab::General => self.general_tab(ui),
                    Tab::Settings => self.settings_tab(ui),
                    Tab::Wordlists => self.wordlists_tab(ui),
                    Tab::Player => self.player_tab(ui),
                }
            });
        });

        draw_focus_ring(&ctx);
    }
}

// ----- widgets -----------------------------------------------------------

/// Font family used for the tab labels (Google Sans Bold).
fn tab_font() -> egui::FontFamily {
    egui::FontFamily::Name("tab".into())
}

/// Google Sans (SIL Open Font License, see assets/fonts/OFL.txt): Medium for
/// all body text, Bold for the tabs. egui's built-in fonts stay behind them
/// as fallbacks for symbols and scripts Google Sans does not cover.
fn setup_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily};
    use std::sync::Arc;

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "GoogleSans-Medium".into(),
        Arc::new(FontData::from_static(include_bytes!("../assets/fonts/GoogleSans-Medium.ttf"))),
    );
    fonts.font_data.insert(
        "GoogleSans-Bold".into(),
        Arc::new(FontData::from_static(include_bytes!("../assets/fonts/GoogleSans-Bold.ttf"))),
    );
    let fallbacks = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "GoogleSans-Medium".into());
    let mut tab = vec!["GoogleSans-Bold".to_owned()];
    tab.extend(fallbacks);
    fonts.families.insert(tab_font(), tab);
    ctx.set_fonts(fonts);
}

fn setup_style(ctx: &egui::Context) {
    use egui::{FontId, TextStyle};
    setup_fonts(ctx);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::proportional(24.0)),
            (TextStyle::Body, FontId::proportional(17.0)),
            (TextStyle::Button, FontId::proportional(17.0)),
            (TextStyle::Monospace, FontId::monospace(16.0)),
            (TextStyle::Small, FontId::proportional(14.0)),
        ]
        .into();
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
        style.spacing.interact_size.y = CONTROL_HEIGHT;
    });
}

fn error_color(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode { Color32::from_rgb(255, 140, 140) } else { Color32::from_rgb(170, 0, 0) }
}

/// A heading that screen readers list as a heading, so users can jump to it.
fn heading(ui: &mut Ui, text: &str) {
    let resp = ui.heading(text);
    ui.ctx().accesskit_node_builder(resp.id, |node| {
        node.set_role(Role::Heading);
        node.set_level(2);
        // egui stores a plain label's text as its value; headings need a name.
        node.set_label(text);
    });
}

fn full_button(ui: &mut Ui, text: &str, enabled: bool) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| ui.add_sized([ui.available_width(), CONTROL_HEIGHT], Button::new(text)))
        .inner
}

fn text_field(ui: &mut Ui, label: &str, value: &mut String, editable: bool) -> egui::Response {
    let label = ui.label(label);
    let resp = if editable {
        let edit = egui::TextEdit::singleline(value).desired_width(f32::INFINITY);
        ui.add_sized([ui.available_width(), CONTROL_HEIGHT], edit)
    } else {
        // Read-only, but still focusable so a screen reader can review it.
        let mut text = value.as_str();
        let edit = egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY);
        ui.add_sized([ui.available_width(), CONTROL_HEIGHT], edit)
    };
    let resp = resp.labelled_by(label.id);
    if !editable {
        ui.ctx().accesskit_node_builder(resp.id, |node| node.set_read_only());
    }
    resp
}

/// A read-only box of several lines that a screen reader can review.
fn text_area(ui: &mut Ui, label: &str, value: &str) {
    let label = ui.label(label);
    let mut text = value;
    let edit = egui::TextEdit::multiline(&mut text).desired_width(f32::INFINITY).desired_rows(10);
    let resp = ui.add(edit).labelled_by(label.id);
    ui.ctx().accesskit_node_builder(resp.id, |node| node.set_read_only());
}

/// Draws the waveform of an audio file, with the part already played in the
/// highlight colour and a line where playback is. Screen readers get it as an
/// image named `description`.
fn waveform(ui: &mut Ui, peaks: &[f32], played: f32, description: &str) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 72.0), egui::Sense::hover());
    let visuals = ui.visuals();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
    let bar_width = (rect.width() / peaks.len().max(1) as f32).max(1.0);
    let middle = rect.center().y;
    for (i, peak) in peaks.iter().enumerate() {
        let at = (i as f32 + 0.5) / peaks.len() as f32;
        let x = rect.left() + at * rect.width();
        let half = (peak * rect.height() / 2.0 * 0.9).max(0.5);
        let colour = if at <= played { visuals.selection.bg_fill } else { visuals.weak_text_color() };
        let bar = [egui::pos2(x, middle - half), egui::pos2(x, middle + half)];
        painter.line_segment(bar, egui::Stroke::new(bar_width, colour));
    }
    let x = rect.left() + played * rect.width();
    let playhead = [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())];
    painter.line_segment(playhead, egui::Stroke::new(2.0, visuals.text_color()));
    ui.ctx().accesskit_node_builder(resp.id, |node| {
        node.set_role(Role::Image);
        node.set_label(description);
    });
}

fn password_field(ui: &mut Ui, label: &str, value: &mut String) {
    let label = ui.label(label);
    ui.add_sized(
        [ui.available_width(), CONTROL_HEIGHT],
        egui::TextEdit::singleline(value).password(true).desired_width(f32::INFINITY),
    )
    .labelled_by(label.id);
}

/// A labelled, full-width dropdown. While it has focus and is closed, the Up,
/// Down, Home and End keys change the choice directly, as in a native combo
/// box; Space or Enter opens the list.
fn dropdown(ui: &mut Ui, id_salt: &str, label: &str, options: &[String], selected: &mut usize, enabled: bool) -> bool {
    let label_resp = ui.label(label);
    let mut changed = false;
    let current = options.get(*selected).cloned().unwrap_or_default();
    let inner = ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(id_salt)
            .width(ui.available_width())
            .height(400.0)
            .selected_text(current.clone())
            .show_ui(ui, |ui| {
                for (i, option) in options.iter().enumerate() {
                    let resp = ui.add_sized(
                        [ui.available_width(), CONTROL_HEIGHT],
                        Button::selectable(i == *selected, option.as_str()),
                    );
                    if resp.clicked() {
                        *selected = i;
                        changed = true;
                    }
                }
            })
    });
    let combo = inner.inner;
    let resp = combo.response.labelled_by(label_resp.id);
    let popup_open = combo.inner.is_some();

    if enabled && resp.has_focus() && !popup_open && !options.is_empty() {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(resp.id, EventFilter { vertical_arrows: true, ..Default::default() })
        });
        let last = options.len() - 1;
        let new = ui.input_mut(|i| {
            if i.consume_key(Modifiers::NONE, Key::ArrowDown) {
                Some((*selected + 1).min(last))
            } else if i.consume_key(Modifiers::NONE, Key::ArrowUp) {
                Some(selected.saturating_sub(1))
            } else if i.consume_key(Modifiers::NONE, Key::Home) {
                Some(0)
            } else if i.consume_key(Modifiers::NONE, Key::End) {
                Some(last)
            } else {
                None
            }
        });
        if let Some(new) = new
            && new != *selected {
                *selected = new;
                changed = true;
                ui.ctx().request_repaint();
            }
    }
    let value = options.get(*selected).cloned().unwrap_or_default();
    ui.ctx().accesskit_node_builder(resp.id, |node| {
        node.set_role(Role::ComboBox);
        node.set_value(value);
    });
    changed
}

/// Draws a thick, high-contrast outline around the focused control.
fn draw_focus_ring(ctx: &egui::Context) {
    let Some(id) = ctx.memory(|m| m.focused()) else { return };
    let Some(resp) = ctx.read_response(id) else { return };
    let color = if ctx.global_style().visuals.dark_mode {
        Color32::from_rgb(255, 215, 0)
    } else {
        Color32::from_rgb(0, 70, 200)
    };
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("focus_ring")));
    painter.rect_stroke(
        resp.rect.expand(3.0),
        4.0,
        egui::Stroke::new(3.0, color),
        egui::StrokeKind::Outside,
    );
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| t("file.fallback_name"))
}

/// The suggested model's download size, such as "about 3.3 GB".
fn model_size() -> String {
    tf("model.size", &[("size", &crate::vision::SUGGESTED_MODEL_SIZE)])
}

/// A voice's name in the list. The default system voice is named by the app,
/// so it follows the interface language.
fn voice_name(voice: &Voice) -> String {
    if voice.id.is_empty() { t("voice.default") } else { voice.name.clone() }
}

/// A length of time as it is said, such as "1 minute 5 seconds".
fn spoken_duration(duration: Duration) -> String {
    let total = duration.as_secs_f64().round() as u64;
    let units = [
        (total / 3600, "time.hour", "time.hours"),
        (total / 60 % 60, "time.minute", "time.minutes"),
        (total % 60, "time.second", "time.seconds"),
    ];
    let parts: Vec<String> = units
        .into_iter()
        .filter(|(count, _, _)| *count > 0)
        .map(|(count, one, other)| if count == 1 { t(one) } else { tf(other, &[("count", &count)]) })
        .collect();
    if parts.is_empty() { tf("time.seconds", &[("count", &0)]) } else { parts.join(" ") }
}

/// A choice in the "Label who is speaking" list on the Audio Player tab.
fn speaker_choice_label(choice: SpeakerLabels) -> String {
    match choice {
        SpeakerLabels::Off => t("speakers.off"),
        SpeakerLabels::Auto => t("speakers.auto"),
        SpeakerLabels::Count(n) => tf("speakers.count", &[("count", &n)]),
    }
}

/// A Whisper model as it is listed on the Settings tab.
fn whisper_label(model: WhisperModel) -> String {
    let size = tf("model.size", &[("size", &model.size())]);
    let key = match model {
        WhisperModel::Base => "settings.whisper_base",
        WhisperModel::Small => "settings.whisper_small",
    };
    let label = tf(key, &[("model", &model.name()), ("size", &size)]);
    if model.is_downloaded() { format!("{label} {}", t("settings.whisper_downloaded")) } else { label }
}

fn display_name(item: &Installed) -> String {
    match &item.list {
        Ok(list) => format!("{} ({})", list.name, item.file_name),
        Err(_) => item.file_name.clone(),
    }
}
