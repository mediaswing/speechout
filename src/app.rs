//! The window: three tabs (General, Settings, Wordlists) with every control
//! on its own full-width line.
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

use crate::audio::AudioFormat;
use crate::settings::Settings;
use crate::speech::{self, Provider, Voice};
use crate::wordlist::{self, Installed, Substitutions};
use crate::worker::{self, Control, Msg, Reporter, SpeechJob};
use egui::accesskit::{Live, Role};
use egui::{Button, Color32, EventFilter, Id, Key, KeyboardShortcut, Modifiers, RichText, Ui};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

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
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::General, Tab::Settings, Tab::Wordlists];

    fn label(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Settings => "Settings",
            Tab::Wordlists => "Wordlists",
        }
    }

    fn id(self) -> Id {
        Id::new(("tab", self.label()))
    }
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
    text: String,
    loading_file: bool,
    job: Option<(JobKind, Arc<Control>)>,
    paused: bool,
    checking_update: bool,
    /// Progress of the current read-aloud or save job, from 0.0 to 1.0.
    progress: f32,
    status: String,
    log_dir_input: String,
    wordlists: Vec<Installed>,
    wordlist_to_remove: usize,
    focus: Option<Id>,
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
            loading_file: false,
            job: None,
            paused: false,
            checking_update: false,
            progress: 0.0,
            status: format!("Ready. Press {}+O to choose a file. {log_status}", MOD_KEY.1),
            wordlists: Vec::new(),
            wordlist_to_remove: 0,
            focus: Some(Tab::General.id()),
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

    /// Speaks how far through reading or saving we are (F7).
    fn announce_progress(&mut self) {
        let percent = (self.progress * 100.0).round();
        let msg = match &self.job {
            Some((JobKind::Speaking, _)) if self.paused => format!("Paused at {percent} percent."),
            Some((JobKind::Speaking, _)) => format!("Reading aloud, {percent} percent."),
            Some((JobKind::Previewing, _)) => "Previewing the voice.".to_owned(),
            Some((JobKind::Saving, _)) => format!("Saving audio, {percent} percent."),
            None if self.loading_file => "Still opening the file.".to_owned(),
            None => "Nothing is playing.".to_owned(),
        };
        self.announce(msg);
    }

    fn copy_text(&mut self, ctx: &egui::Context) {
        if self.text.is_empty() {
            self.announce("There is no text to copy yet.");
            return;
        }
        ctx.copy_text(self.text.clone());
        let words = self.text.split_whitespace().count();
        self.announce(format!("Copied {words} words to the clipboard."));
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

    fn is_busy(&self) -> bool {
        self.job.is_some()
    }

    fn handle_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Voices(provider, result) => {
                    if let Err(e) = &result {
                        self.announce(format!("Could not load voices for {}: {e}", provider.label()));
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
                    match &result {
                        Ok(models) if models.is_empty() => self.announce(
                            "Ollama is running but has no models. Install a vision model, for example: ollama pull llama3.2-vision",
                        ),
                        Ok(models) => {
                            if !models.contains(&self.settings.vision_model)
                                && let Some(model) = crate::vision::preferred_model(models) {
                                    self.settings.vision_model = model.clone();
                                    self.settings.save();
                                }
                            if self.tab == Tab::Settings {
                                self.announce(format!("Found {} local AI model(s).", models.len()));
                            }
                        }
                        Err(e) => {
                            if self.tab == Tab::Settings {
                                self.announce(e.clone());
                            }
                        }
                    }
                    self.models = Some(match result {
                        Ok(m) => Loadable::Ready(m),
                        Err(_) => Loadable::Failed,
                    });
                }
                Msg::Loaded(path, result) => {
                    self.loading_file = false;
                    let name = file_name(&path);
                    match result {
                        Ok(text) => {
                            let words = text.split_whitespace().count();
                            self.text = text;
                            self.file = Some(path);
                            self.announce(format!(
                                "Loaded {name}, {words} words. Press F5 to read it aloud or {}+S to save it as audio.",
                                MOD_KEY.1
                            ));
                        }
                        Err(e) => self.announce(format!("Could not load {name}. {e}")),
                    }
                }
                Msg::Update(result, requested) => self.update_checked(result, requested),
                Msg::Status(s) => self.announce(s),
                Msg::Progress(p) => self.progress = p.clamp(0.0, 1.0),
                Msg::Done(s) => {
                    self.job = None;
                    self.paused = false;
                    self.announce(s);
                }
                Msg::Failed(e) => {
                    self.job = None;
                    self.paused = false;
                    self.announce(e);
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
                    self.announce(format!(
                        "You have the latest version, {}.",
                        env!("CARGO_PKG_VERSION")
                    ));
                }
                return;
            }
            Err(e) => {
                // An automatic check fails quietly (the error is in the log).
                if requested {
                    self.announce(e);
                }
                return;
            }
        };
        log::info!("update available: {}", release.version);
        let open = rfd::MessageDialog::new()
            .set_title("Update available")
            .set_description(format!(
                "Version {} of the {APP_TITLE} is available. You have version {}.\n\n\
                 Open the download page in your web browser?",
                release.version,
                env!("CARGO_PKG_VERSION")
            ))
            .set_buttons(rfd::MessageButtons::YesNo)
            .set_level(rfd::MessageLevel::Info)
            .show()
            == rfd::MessageDialogResult::Yes;
        if open {
            match crate::platform::open_url(&release.url) {
                Ok(()) => self.announce(format!("Opened the download page for version {}.", release.version)),
                Err(e) => self.announce(format!("{e}. The download page is {}", release.url)),
            }
        } else {
            self.announce(format!(
                "Version {} is available. You can download it later from the Settings tab.",
                release.version
            ));
        }
    }

    fn check_for_update(&mut self) {
        if self.checking_update {
            return;
        }
        self.checking_update = true;
        self.announce("Checking for updates.");
        self.rep.spawn(|rep| worker::check_for_update(rep, true));
    }

    fn choose_file(&mut self) {
        if self.loading_file {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title("Choose a document or photo")
            .add_filter("Documents and photos", &["pdf", "txt", "docx", "jpg", "jpeg", "heic", "heif"])
            .add_filter("Documents", &["pdf", "txt", "docx"])
            .add_filter("Photos", &["jpg", "jpeg", "heic", "heif"])
            .pick_file()
        else {
            return;
        };
        let Some(kind) = crate::document::FileKind::from_path(&path) else {
            self.announce("That type of file is not supported. Choose a PDF, TXT, DOCX, JPEG or HEIC file.");
            return;
        };
        let is_image = kind == crate::document::FileKind::Image;
        if is_image && self.settings.vision_model.is_empty() {
            self.ensure_models();
            self.announce(
                "To describe photos, install Ollama with a vision model, then choose the model on the Settings tab.",
            );
            return;
        }
        self.loading_file = true;
        self.announce(if is_image {
            format!("Describing {}. This can take a minute.", file_name(&path))
        } else {
            format!("Opening {}.", file_name(&path))
        });
        let model = self.settings.vision_model.clone();
        let resolve = self.settings.resolve_location;
        self.rep.spawn(move |rep| worker::load_file(rep, path, model, resolve));
    }

    /// A job for the loaded document, with the wordlists applied.
    fn build_job(&mut self) -> Option<SpeechJob> {
        if self.is_busy() {
            return None;
        }
        if self.text.trim().is_empty() {
            self.announce(format!("There is nothing to read yet. Press {}+O to choose a file.", MOD_KEY.1));
            return None;
        }
        let subs = Substitutions::new(&self.wordlists, &self.settings.disabled_wordlists);
        let text = subs.apply(&self.text);
        self.job_for(text)
    }

    /// A job that speaks `text` with the chosen service, voice and speed.
    fn job_for(&mut self, text: String) -> Option<SpeechJob> {
        let provider = self.active_provider();
        let voice = match self.current_voice(provider) {
            Some(v) => v.id.clone(),
            None if provider == Provider::System => String::new(),
            None => {
                self.announce("Choose a voice first. The voice list may still be loading.");
                return None;
            }
        };
        Some(SpeechJob {
            provider,
            key: self.api_keys.get(&provider).cloned(),
            voice,
            speed: self.settings.speed_for(provider),
            text,
            preview: false,
        })
    }

    /// For cloud voices, how much text the job will send, such as
    /// " 48,250 characters will be sent to ElevenLabs."
    fn usage_note(job: &SpeechJob, prefix: &str) -> String {
        if job.provider == Provider::System {
            return String::new();
        }
        let count = speech::billable_chars(job.provider, &job.text);
        let noun = if count == 1 { "character" } else { "characters" };
        format!(" {prefix}{} {noun} will be sent to {}.", speech::format_count(count), job.provider.short_name())
    }

    fn read_aloud(&mut self) {
        let Some(job) = self.build_job() else { return };
        // Reading aloud stops sending text when the user presses Stop.
        let usage = Self::usage_note(&job, "Up to ");
        self.start_speaking(job, format!("Reading aloud. Press F6 to pause or Escape to stop.{usage}"));
    }

    /// Speaks a short sentence with the chosen voice and speed, through the
    /// same job as reading aloud, so Stop, Pause and error handling all work.
    fn preview_voice(&mut self) {
        if self.is_busy() {
            return;
        }
        let Some(mut job) = self.job_for(PREVIEW_TEXT.to_owned()) else { return };
        job.preview = true;
        self.start_speaking(job, "Previewing the voice.".to_owned());
    }

    fn start_speaking(&mut self, job: SpeechJob, message: String) {
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
            let msg = if self.paused { "Paused. Press F6 to resume." } else { "Resumed." };
            self.announce(msg);
        }
    }

    fn stop(&mut self) {
        if let Some((_, control)) = &self.job {
            control.stop();
            self.announce("Stopping.");
        }
    }

    fn save_audio(&mut self) {
        if self.is_busy() {
            return;
        }
        let format = self.settings.audio_format;
        let stem = self
            .file
            .as_ref()
            .and_then(|f| f.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "speech".into());
        let Some(mut path) = rfd::FileDialog::new()
            .set_title("Save spoken text as audio")
            .set_file_name(format!("{stem}.{}", format.extension()))
            .add_filter(format.label(), &[format.extension()])
            .save_file()
        else {
            return;
        };
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case(format.extension())) {
            path.as_mut_os_string().push(format!(".{}", format.extension()));
        }
        let Some(job) = self.build_job() else { return };
        let control = Arc::new(Control::default());
        self.job = Some((JobKind::Saving, control.clone()));
        self.progress = 0.0;
        let usage = Self::usage_note(&job, "");
        self.announce(format!("Preparing {}.{usage} Press Escape to cancel.", file_name(&path)));
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
                    saved.push(provider.label());
                }
                Err(e) => {
                    self.announce(format!("Could not save the {} key: {e}", provider.label()));
                    return;
                }
            }
        }
        self.key_inputs.clear();
        if saved.is_empty() {
            self.announce("Type a key into one of the API key boxes first.");
        } else {
            self.announce(format!("Saved the API key for {} in {where_}.", saved.join(" and ")));
        }
    }

    fn remove_keys(&mut self) {
        if self.api_keys.is_empty() {
            self.announce("No API keys are saved.");
            return;
        }
        let confirmed = rfd::MessageDialog::new()
            .set_title("Remove saved API keys")
            .set_description("Remove every saved API key? You will need to enter them again to use cloud voices.")
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
        self.announce("Removed all saved API keys.");
    }

    fn apply_log_dir(&mut self, dir: PathBuf) {
        match crate::logging::set_directory(&dir) {
            Ok(file) => {
                self.log_dir_input = dir.display().to_string();
                self.settings.log_dir = Some(dir);
                self.settings.save();
                self.announce(format!("Debug logs are now saved to {}.", file.display()));
            }
            Err(e) => self.announce(format!("Could not use that folder for logs: {e}")),
        }
    }

    fn import_wordlist(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Import a wordlist")
            .add_filter("XML wordlist", &["xml"])
            .pick_file()
        else {
            return;
        };
        match wordlist::import(&path, &crate::paths::wordlist_dir()) {
            Ok(name) => {
                self.reload_wordlists();
                self.announce(format!("Imported and enabled the wordlist \"{name}\"."));
            }
            Err(e) => self.announce(format!("Could not import {}: {e:#}", file_name(&path))),
        }
    }

    fn remove_wordlist(&mut self) {
        let Some(item) = self.wordlists.get(self.wordlist_to_remove) else { return };
        let file = item.file_name.clone();
        let title = display_name(item);
        let confirmed = rfd::MessageDialog::new()
            .set_title("Remove wordlist")
            .set_description(format!("Remove the wordlist \"{title}\"?"))
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
                self.announce(format!("Removed the wordlist \"{title}\"."));
            }
            Err(e) => self.announce(format!("{e:#}")),
        }
    }

    // ----- keyboard -----------------------------------------------------

    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        let cmd = |key| KeyboardShortcut::new(Modifiers::COMMAND, key);
        let pressed = |s: KeyboardShortcut| ctx.input_mut(|i| i.consume_shortcut(&s));
        let popup_open = ctx.any_popup_open();

        for (key, tab) in [(Key::Num1, Tab::General), (Key::Num2, Tab::Settings), (Key::Num3, Tab::Wordlists)] {
            if pressed(cmd(key)) {
                self.switch_tab(tab);
            }
        }
        if pressed(KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, Key::Tab)) {
            let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
            self.switch_tab(Tab::ALL[(i + 2) % 3]);
        } else if pressed(KeyboardShortcut::new(Modifiers::CTRL, Key::Tab)) {
            let i = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
            self.switch_tab(Tab::ALL[(i + 1) % 3]);
        }
        if pressed(cmd(Key::O)) {
            self.choose_file();
        }
        if pressed(cmd(Key::S)) {
            self.save_audio();
        }
        if pressed(KeyboardShortcut::new(Modifiers::NONE, Key::F5)) {
            self.read_aloud();
        }
        if pressed(KeyboardShortcut::new(Modifiers::NONE, Key::F6)) {
            self.toggle_pause();
        }
        if pressed(KeyboardShortcut::new(Modifiers::NONE, Key::F7)) {
            self.announce_progress();
        }
        if !popup_open && self.is_busy() && pressed(KeyboardShortcut::new(Modifiers::NONE, Key::Escape)) {
            self.stop();
        }
    }

    fn switch_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.focus = Some(tab.id());
    }

    // ----- drawing ------------------------------------------------------

    fn tab_bar(&mut self, ui: &mut Ui) {
        ui.columns(3, |cols| {
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
                    self.announce(format!("{} tab.", tab.label()));
                }
            }
        });
    }

    fn general_tab(&mut self, ui: &mut Ui) {
        heading(ui, "Read a document or describe a photo");

        let mut file_text = match (&self.file, self.loading_file) {
            (_, true) => "Loading…".to_owned(),
            (Some(p), _) => p.display().to_string(),
            (None, _) => "No file chosen".to_owned(),
        };
        text_field(ui, "Current file", &mut file_text, false);

        if full_button(ui, &format!("Choose a file… ({}+O)", MOD_KEY.0), !self.loading_file).clicked() {
            self.choose_file();
        }

        // Speech service.
        let providers = self.available_providers();
        let mut index = providers.iter().position(|p| *p == self.active_provider()).unwrap_or(0);
        let labels: Vec<String> = providers.iter().map(|p| p.label().to_owned()).collect();
        if dropdown(ui, "provider", "Speech service", &labels, &mut index, !self.is_busy()) {
            let provider = providers[index];
            self.settings.provider = provider;
            self.settings.save();
            self.announce(format!("Speech service: {}. {}", provider.label(), provider.privacy_note()));
        }
        // Where the speech is made, so it is clear when text leaves the computer.
        ui.label(self.active_provider().privacy_note());

        // Voice.
        let provider = self.active_provider();
        self.ensure_voices(provider);
        let voices = self.voice_list(provider).to_vec();
        let (names, empty_text) = match self.voices.get(&provider) {
            Some(Loadable::Ready(v)) if v.is_empty() => (vec![], "No voices found"),
            Some(Loadable::Ready(v)) => (v.iter().map(|v| v.name.clone()).collect(), ""),
            Some(Loadable::Failed) => (vec![], "Voices could not be loaded"),
            _ => (vec![], "Loading voices…"),
        };
        let names = if names.is_empty() { vec![empty_text.to_owned()] } else { names };
        let current = self.current_voice(provider).map(|v| v.id.clone());
        let mut vindex = voices.iter().position(|v| Some(&v.id) == current.as_ref()).unwrap_or(0);
        if dropdown(ui, "voice", "Voice", &names, &mut vindex, !voices.is_empty() && !self.is_busy())
            && let Some(v) = voices.get(vindex) {
                self.settings.set_voice(provider, v.id.clone());
                self.settings.save();
            }
        if matches!(self.voices.get(&provider), Some(Loadable::Failed))
            && full_button(ui, "Try loading voices again", true).clicked()
        {
            self.voices.remove(&provider);
        }

        // Speaking speed, only for services that have a speed setting.
        let speeds = provider.speed_choices();
        if !speeds.is_empty() {
            let labels: Vec<String> = speeds.iter().map(|s| speech::speed_label(*s)).collect();
            let current = self.settings.speed_for(provider);
            let mut sindex = speeds.iter().position(|s| *s == current).unwrap_or(0);
            if dropdown(ui, "speed", "Speaking speed", &labels, &mut sindex, !self.is_busy()) {
                self.settings.set_speed(provider, speeds[sindex]);
                self.settings.save();
            }
        }
        let can_preview = !self.is_busy() && (provider == Provider::System || !voices.is_empty());
        if full_button(ui, "Preview voice", can_preview).clicked() {
            self.preview_voice();
        }

        let has_text = !self.text.is_empty();
        let speaking = matches!(self.job, Some((JobKind::Speaking, _)));
        if full_button(ui, "Read aloud (F5)", has_text && !self.is_busy()).clicked() {
            self.read_aloud();
        }
        let pause_label = if self.paused { "Resume (F6)" } else { "Pause (F6)" };
        if full_button(ui, pause_label, speaking).clicked() {
            self.toggle_pause();
        }
        let stop_label = if matches!(self.job, Some((JobKind::Saving, _))) { "Cancel saving (Esc)" } else { "Stop (Esc)" };
        if full_button(ui, stop_label, self.is_busy()).clicked() {
            self.stop();
        }

        let formats = [AudioFormat::Mp3, AudioFormat::Wav];
        let labels: Vec<String> = formats.iter().map(|f| f.label().to_owned()).collect();
        let mut findex = formats.iter().position(|f| *f == self.settings.audio_format).unwrap_or(0);
        if dropdown(ui, "format", "Audio file format", &labels, &mut findex, !self.is_busy()) {
            self.settings.audio_format = formats[findex];
            self.settings.save();
        }
        if full_button(ui, &format!("Save spoken text as audio… ({}+S)", MOD_KEY.0), has_text && !self.is_busy()).clicked() {
            self.save_audio();
        }
        if full_button(ui, "Copy text to the clipboard", has_text).clicked() {
            let ctx = ui.ctx().clone();
            self.copy_text(&ctx);
        }
    }

    fn settings_tab(&mut self, ui: &mut Ui) {
        heading(ui, "Settings");

        // Image description model.
        self.ensure_models();
        let (models, placeholder) = match &self.models {
            Some(Loadable::Ready(m)) if !m.is_empty() => (m.clone(), String::new()),
            Some(Loadable::Ready(_)) => (vec![], "No models installed in Ollama".to_owned()),
            Some(Loadable::Failed) => (vec![], "Ollama is not running".to_owned()),
            _ => (vec![], "Looking for Ollama…".to_owned()),
        };
        let shown = if models.is_empty() { vec![placeholder] } else { models.clone() };
        let mut mindex = models.iter().position(|m| *m == self.settings.vision_model).unwrap_or(0);
        if dropdown(ui, "model", "Image description model (Ollama)", &shown, &mut mindex, !models.is_empty()) {
            self.settings.vision_model = models[mindex].clone();
            self.settings.save();
        }
        if full_button(ui, "Refresh the list of models", !matches!(self.models, Some(Loadable::Loading))).clicked() {
            self.models = None;
            self.ensure_models();
            self.announce("Looking for local AI models.");
        }

        let location_options = vec![
            "Do not read out where photos were taken".to_owned(),
            "Read out where geotagged photos were taken (looks up the place with OpenStreetMap)".to_owned(),
        ];
        let mut lindex = usize::from(self.settings.resolve_location);
        if dropdown(ui, "location", "Photo location", &location_options, &mut lindex, true) {
            self.settings.resolve_location = lindex == 1;
            self.settings.save();
        }

        let update_options = vec![
            "Check for updates when the app starts".to_owned(),
            "Do not check for updates automatically".to_owned(),
        ];
        let mut uindex = usize::from(!self.settings.check_updates);
        if dropdown(ui, "updates", "Updates", &update_options, &mut uindex, true) {
            self.settings.check_updates = uindex == 0;
            self.settings.save();
        }
        if full_button(ui, "Check for updates now", !self.checking_update).clicked() {
            self.check_for_update();
        }

        let mut dir = self.log_dir_input.clone();
        if text_field(ui, "Debug log folder", &mut dir, true).changed() {
            self.log_dir_input = dir;
        }
        if full_button(ui, "Choose the debug log folder…", true).clicked()
            && let Some(dir) = rfd::FileDialog::new()
                .set_title("Choose where to save debug logs")
                .set_directory(self.settings.log_dir())
                .pick_folder()
            {
                self.apply_log_dir(dir);
            }
        if full_button(ui, "Use the folder typed above for debug logs", true).clicked() {
            let typed = PathBuf::from(self.log_dir_input.trim());
            if typed.is_absolute() {
                self.apply_log_dir(typed);
            } else {
                self.announce("Type a full folder path, for example one starting with a drive letter or a slash.");
            }
        }

        ui.add_space(8.0);
        heading(ui, "Cloud voice API keys");
        ui.label(format!(
            "Keys are stored in {}. Leave a box empty to keep the key already saved.",
            crate::platform::secret_store_description()
        ));
        for provider in Provider::ALL {
            if provider.key_name().is_none() {
                continue;
            }
            let state = if self.api_keys.contains_key(&provider) { "a key is saved" } else { "no key saved" };
            let input = self.key_inputs.entry(provider).or_default();
            password_field(ui, &format!("{} API key ({state})", provider.label()), input);
        }
        if full_button(ui, "Save API keys", true).clicked() {
            self.save_keys();
        }
        if full_button(ui, "Remove all saved API keys…", !self.api_keys.is_empty()).clicked() {
            self.remove_keys();
        }
    }

    fn wordlists_tab(&mut self, ui: &mut Ui) {
        heading(ui, "Wordlists");
        ui.label(
            "Wordlists change how words are spoken, for example to fix pronunciation or to keep \
             reading classroom-safe. Ticked wordlists are used when reading aloud and saving audio.",
        );
        if full_button(ui, "Import a wordlist… (XML)", true).clicked() {
            self.import_wordlist();
        }
        if full_button(ui, "Reload wordlists", true).clicked() {
            self.reload_wordlists();
            self.announce(format!("{} wordlist(s) installed.", self.wordlists.len()));
        }

        ui.add_space(8.0);
        heading(ui, "Installed wordlists");
        if self.wordlists.is_empty() {
            ui.label("No wordlists are installed.");
        }
        let mut toggled = None;
        for item in &self.wordlists {
            match &item.list {
                Ok(list) => {
                    let mut enabled = !self.settings.disabled_wordlists.contains(&item.file_name);
                    let text = format!(
                        "{} – {} ({} entries)",
                        list.name,
                        if list.description.is_empty() { &item.file_name } else { &list.description },
                        list.entries.len()
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
                    ui.label(RichText::new(format!("{} could not be read: {e}", item.file_name)).color(error_color(ui)));
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
            self.announce(format!("{name} {}.", if enabled { "enabled" } else { "disabled" }));
        }

        if !self.wordlists.is_empty() {
            ui.add_space(8.0);
            let names: Vec<String> = self.wordlists.iter().map(display_name).collect();
            let mut index = self.wordlist_to_remove;
            if dropdown(ui, "remove_wordlist", "Wordlist to remove", &names, &mut index, true) {
                self.wordlist_to_remove = index;
            }
            if full_button(ui, "Remove the selected wordlist…", true).clicked() {
                self.remove_wordlist();
            }
        }
    }

    /// A full-width progress bar for reading aloud, saving audio and loading
    /// files. Screen readers get it as a progress indicator named "Progress",
    /// with a percentage value.
    fn progress_bar(&self, ui: &mut Ui) {
        let (fraction, text) = match &self.job {
            Some((JobKind::Speaking, _)) => {
                let state = if self.paused { "Paused" } else { "Reading aloud" };
                (Some(self.progress), format!("{state}: {:.0}%", self.progress * 100.0))
            }
            Some((JobKind::Previewing, _)) => (Some(self.progress), "Previewing the voice".to_owned()),
            Some((JobKind::Saving, _)) => {
                (Some(self.progress), format!("Saving audio: {:.0}%", self.progress * 100.0))
            }
            None if self.loading_file => (None, "Opening the file…".to_owned()),
            None => (Some(0.0), "Nothing playing".to_owned()),
        };
        let bar = egui::ProgressBar::new(fraction.unwrap_or(0.0))
            .desired_width(ui.available_width())
            .desired_height(24.0)
            .text(text.clone())
            // An unknown amount of work (opening a file) shows as an animation.
            .animate(fraction.is_none());
        let resp = ui.add(bar);
        ui.ctx().accesskit_node_builder(resp.id, |node| {
            node.set_label("Progress");
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
    path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "the file".into())
}

fn display_name(item: &Installed) -> String {
    match &item.list {
        Ok(list) => format!("{} ({})", list.name, item.file_name),
        Err(_) => item.file_name.clone(),
    }
}
