//! The window: tabs down the left, the selected pane on the right, and a status
//! bar along the bottom.
//!
//! Accessibility comes first here:
//! * egui publishes the interface to screen readers through AccessKit (UI
//!   Automation on Windows, NSAccessibility on macOS, AT-SPI on Linux).
//! * Every field and dropdown is linked to its visible label, tabs have the tab
//!   role and selected state, and the status bar is a live region, so progress
//!   and errors are announced without moving the focus.
//! * Everything works from the keyboard: Tab and Shift+Tab move between
//!   controls, Space or Enter activates them, and the shortcuts in
//!   `shortcuts.rs` work from anywhere. Tests in `tests.rs` hold this in place.
//! * When a control the focus is on goes away, the focus moves somewhere
//!   sensible, so nobody is left stranded.

mod appearance;
mod audio_tab;
mod general;
mod settings;
mod shortcuts;
#[cfg(test)]
mod tests;
mod updates;
mod widgets;
mod wordlist_tab;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};

use egui::accesskit::{Live, Role};
use egui::{RichText, Ui};

use crate::speech::{Provider, Voice};
use crate::store::{Installed, Settings, Store};
use crate::worker::{self, Control, Msg, Reporter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    General,
    Settings,
    Wordlist,
    AudioPlayer,
}

impl Tab {
    const ALL: [Tab; 4] = [Tab::General, Tab::Settings, Tab::Wordlist, Tab::AudioPlayer];

    fn name(self) -> &'static str {
        match self {
            Tab::General => "General",
            Tab::Settings => "Settings",
            Tab::Wordlist => "Wordlist",
            Tab::AudioPlayer => "Audio Player",
        }
    }

    /// With an icon, as on the window. Screen readers get `name` instead.
    fn label(self) -> &'static str {
        match self {
            Tab::General => "🏠  General",
            Tab::Settings => "⚙  Settings",
            Tab::Wordlist => "🏷  Wordlist",
            Tab::AudioPlayer => "🎵  Audio Player",
        }
    }
}

/// The native dialogs, behind a trait so the tests can stand in for them.
pub trait Dialogs {
    fn pick_file(&self, title: &str, kinds: &str, extensions: &[&str]) -> Option<PathBuf>;
    fn pick_folder(&self, title: &str) -> Option<PathBuf>;
    fn save_file(&self, title: &str, name: &str, kinds: &str, extensions: &[&str]) -> Option<PathBuf>;
    fn confirm(&self, title: &str, question: &str) -> bool;
}

/// The operating system's own dialogs, which screen readers already know.
pub struct NativeDialogs;

impl Dialogs for NativeDialogs {
    fn pick_file(&self, title: &str, kinds: &str, extensions: &[&str]) -> Option<PathBuf> {
        let dialog = rfd::FileDialog::new().set_title(title);
        // Programs on macOS and Linux have no extension to filter by.
        if extensions.is_empty() { dialog } else { dialog.add_filter(kinds, extensions) }.pick_file()
    }

    fn pick_folder(&self, title: &str) -> Option<PathBuf> {
        rfd::FileDialog::new().set_title(title).pick_folder()
    }

    fn save_file(&self, title: &str, name: &str, kinds: &str, extensions: &[&str]) -> Option<PathBuf> {
        rfd::FileDialog::new().set_title(title).set_file_name(name).add_filter(kinds, extensions).save_file()
    }

    fn confirm(&self, title: &str, question: &str) -> bool {
        rfd::MessageDialog::new()
            .set_title(title)
            .set_description(question)
            .set_buttons(rfd::MessageButtons::YesNo)
            .show()
            == rfd::MessageDialogResult::Yes
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
    Saving,
}

/// A file that has been opened.
struct Opened {
    /// The whole text, or an image's description.
    text: Arc<String>,
    /// What the text box shows: a long document would make it too slow.
    preview: String,
    name: String,
}

struct Status {
    text: String,
    error: bool,
}

pub struct App {
    store: Store,
    settings: Settings,
    rep: Reporter,
    rx: Receiver<Msg>,
    dialogs: Box<dyn Dialogs>,
    tab: Tab,
    status: Status,
    /// A control waiting to be given the keyboard focus, by name.
    focus_wanted: Option<&'static str>,
    /// Escape was pressed, and hasn't been acted on yet.
    escape_pressed: bool,
    /// F1 has replaced the selected tab's controls with the keyboard shortcuts.
    shortcuts_open: bool,
    focus_before_shortcuts: Option<egui::Id>,

    // General
    /// Whether to read a file or text typed into the window.
    source: general::Source,
    free_text: String,
    path_input: String,
    opened: Option<Opened>,
    /// Which request to open a file is the latest. Answers to others are ignored.
    load_id: u64,
    loading: Option<String>,
    job: Option<(JobKind, Arc<Control>)>,
    paused: bool,
    progress: f32,
    voices: HashMap<Provider, Loadable<Vec<Voice>>>,

    // Settings
    /// The service the API key box is for.
    key_service: Provider,
    key_input: String,
    saved_keys: HashMap<Provider, bool>,
    ai_address_input: String,
    models: Loadable<Vec<String>>,
    log_dir_input: String,

    whisper_input: String,
    /// A newer version that GitHub has, once a check has found one.
    newer_version: Option<crate::update::Version>,
    checking_for_updates: bool,

    // Wordlist
    wordlists: Vec<Installed>,
    wordlist_to_remove: usize,

    // Audio Player
    audio: audio_tab::AudioState,
}

impl App {
    pub fn new(ctx: &egui::Context, store: Store, dialogs: Box<dyn Dialogs>, problem: String) -> Self {
        let (tx, rx) = channel();
        let rep = Reporter::new(tx, ctx.clone());
        let mut problems = vec![problem];
        let settings = store.settings().unwrap_or_else(|e| {
            problems.push(format!("Your settings could not be read: {e:#}."));
            Settings::default()
        });
        appearance::apply(ctx, settings.appearance);
        ctx.all_styles_mut(|style| {
            style.spacing.interact_size.y = widgets::CONTROL_HEIGHT;
            style.spacing.button_padding = egui::vec2(10.0, 6.0);
            style.text_styles.insert(egui::TextStyle::Body, egui::FontId::proportional(16.0));
            style.text_styles.insert(egui::TextStyle::Button, egui::FontId::proportional(16.0));
            style.text_styles.insert(egui::TextStyle::Heading, egui::FontId::proportional(24.0));
        });
        if let Err(e) = store.install_examples() {
            log::warn!("Could not install the example wordlists: {e:#}");
        }
        let wordlists = store.wordlists().unwrap_or_else(|e| {
            problems.push(format!("The wordlists could not be read: {e:#}."));
            Vec::new()
        });
        let saved_keys = Provider::CLOUD.iter().map(|p| (*p, store.api_key(*p).is_ok_and(|k| k.is_some()))).collect();
        let problem = problems.into_iter().filter(|p| !p.is_empty()).collect::<Vec<_>>().join(" ");
        let log_dir_input = settings
            .log_dir
            .clone()
            .unwrap_or_else(|| crate::store::data_dir().join("logs"))
            .display()
            .to_string();
        let whisper_input = settings.whisper_program.as_deref().map(|p| p.display().to_string()).unwrap_or_default();
        let mut app = Self {
            whisper_input,
            audio: audio_tab::AudioState::default(),
            ai_address_input: settings.ai_address.clone(),
            log_dir_input,
            settings,
            store,
            rep,
            rx,
            dialogs,
            tab: Tab::General,
            status: Status { text: "Ready.".to_owned(), error: false },
            focus_wanted: Some(Tab::General.name()),
            escape_pressed: false,
            shortcuts_open: false,
            focus_before_shortcuts: None,
            source: general::Source::File,
            free_text: String::new(),
            path_input: String::new(),
            opened: None,
            load_id: 0,
            loading: None,
            job: None,
            paused: false,
            progress: 0.0,
            voices: HashMap::new(),
            key_service: Provider::CLOUD[0],
            key_input: String::new(),
            saved_keys,
            models: Loadable::Loading,
            wordlists,
            wordlist_to_remove: 0,
            newer_version: None,
            checking_for_updates: false,
        };
        if !problem.is_empty() {
            app.set_error(problem);
        }
        // A saved key can have been removed by hand.
        if !app.available_providers().contains(&app.settings.provider) {
            app.settings.provider = Provider::System;
        }
        app.ensure_voices(app.settings.provider);
        worker::list_models(&app.rep, app.settings.ai_kind, app.settings.ai_address.clone(), false);
        app
    }

    /// Puts a message in the status bar, which screen readers announce. They
    /// ignore a live region whose text hasn't changed, so a repeated message
    /// gets a no-break space added to make it differ.
    fn announce(&mut self, text: String, error: bool) {
        log::debug!("Status: {text}");
        let mut text = text;
        if text == self.status.text.trim_end_matches('\u{a0}') && !self.status.text.ends_with('\u{a0}') {
            text.push('\u{a0}');
        }
        self.status = Status { text, error };
    }

    fn set_info(&mut self, text: impl Into<String>) {
        self.announce(text.into(), false);
    }

    fn set_error(&mut self, text: impl Into<String>) {
        self.announce(text.into(), true);
    }

    /// Saves the settings, reporting any failure.
    fn save_settings(&mut self) {
        if let Err(e) = self.store.save_settings(&self.settings) {
            log::error!("Could not save the settings: {e:#}");
            self.set_error(format!("Your settings could not be saved: {e:#}."));
        }
    }

    fn handle_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Voices(provider, result) => self.voices_loaded(provider, result),
                Msg::Models(result, requested) => self.models_listed(result, requested),
                Msg::Loaded(id, result) => self.file_loaded(id, result),
                Msg::LoadStatus(id, text) => {
                    if id == self.load_id {
                        self.set_info(text);
                    }
                }
                Msg::Progress(p) => self.progress = p.clamp(0.0, 1.0),
                Msg::Done(text) => {
                    self.finish_job();
                    self.set_info(text);
                }
                Msg::Heard(id, result) => self.audio_heard(id, result),
                Msg::Played(id, result) => self.audio_played(id, result),
                Msg::TranscribeStage(id, text) => self.transcription_stage(id, text),
                Msg::TranscribeProgress(id, p) => self.transcription_progress(id, p),
                Msg::Transcribed(id, result) => self.transcribed(id, result),
                Msg::Update(result, requested) => self.update_checked(result, requested),
                Msg::Failed(text) => {
                    // Only reading aloud or saving fails this way. A file being
                    // opened, if any, carries on.
                    self.finish_job();
                    self.set_error(text);
                }
            }
        }
    }

    fn finish_job(&mut self) {
        self.job = None;
        self.paused = false;
        self.progress = 0.0;
    }

    /// Whether `name` is waiting for the focus. Asking takes the request away.
    fn wants_focus(&mut self, name: &str) -> bool {
        if self.focus_wanted == Some(name) {
            self.focus_wanted = None;
            true
        } else {
            false
        }
    }

    pub(crate) fn switch_tab(&mut self, tab: Tab) {
        if self.tab != tab {
            log::debug!("Switched to tab {tab:?}");
            self.tab = tab;
        }
        self.shortcuts_open = false;
        self.focus_before_shortcuts = None;
        self.focus_wanted = Some(tab.name());
    }

    fn tab_bar(&mut self, ui: &mut Ui) {
        // So screen readers can say "tab 1 of 4".
        ui.ctx().accesskit_node_builder(ui.unique_id(), |node| {
            node.set_role(Role::TabList);
            node.set_label("Tabs");
        });
        ui.add_space(8.0);
        for tab in Tab::ALL {
            let selected = self.tab == tab;
            let button = egui::Button::selectable(selected, RichText::new(tab.label()).size(15.0))
                .min_size(egui::vec2(ui.available_width(), 34.0));
            let resp = widgets::stable_scope(ui, tab.name(), |ui| ui.add(button));
            let shortcut = ui.ctx().format_shortcut(&shortcuts::tab(tab));
            ui.ctx().accesskit_node_builder(resp.id, |node| {
                node.set_role(Role::Tab);
                node.set_label(tab.name());
                node.set_selected(selected);
                node.set_keyboard_shortcut(shortcut.clone());
            });
            if self.wants_focus(tab.name()) {
                resp.request_focus();
            }
            if resp.on_hover_text(shortcut).clicked() {
                self.switch_tab(tab);
            }
        }
    }

    fn status_bar(&mut self, ui: &mut Ui) {
        ui.add_space(2.0);
        let colour = if self.status.error { ui.visuals().error_fg_color } else { ui.visuals().text_color() };
        let resp = ui.add(egui::Label::new(RichText::new(&self.status.text).color(colour)).wrap());
        // An error interrupts whatever the screen reader is saying.
        let live = if self.status.error { Live::Assertive } else { Live::Polite };
        ui.ctx().accesskit_node_builder(resp.id, |node| {
            node.set_live(live);
        });
        ui.add_space(2.0);
    }

    fn open_dropped_file(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(path) = dropped {
            let tab = if audio_tab::is_audio(&path) { Tab::AudioPlayer } else { Tab::General };
            if self.tab != tab {
                self.switch_tab(tab);
            }
            self.close_shortcuts(ctx);
            match tab {
                Tab::AudioPlayer => self.open_audio(path),
                _ => self.open_file(path),
            }
        }
    }

    /// Everything inside the window, once a frame.
    pub fn show(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        self.handle_messages();
        self.handle_shortcuts(&ctx);
        self.open_dropped_file(&ctx);
        egui::Panel::bottom("status_bar").show(ui, |ui| self.status_bar(ui));
        // The tabs give up some room when space is short, as when text is made
        // much larger, so the controls beside them stay usable.
        let tabs_width = (ui.available_width() * 0.34).clamp(110.0, 190.0);
        egui::Panel::left("tabs").resizable(false).exact_size(tabs_width).show(ui, |ui| self.tab_bar(ui));
        egui::CentralPanel::default().show(ui, |ui| {
            egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                // Keep clear of the scroll bar, so it doesn't cover the controls.
                ui.set_max_width(ui.available_width() - 14.0);
                ui.spacing_mut().item_spacing.y = 8.0;
                match self.tab {
                    _ if self.shortcuts_open => self.shortcuts_view(ui),
                    Tab::General => self.general(ui),
                    Tab::Settings => self.settings_tab(ui),
                    Tab::Wordlist => self.wordlist_tab(ui),
                    Tab::AudioPlayer => self.audio_tab(ui),
                }
            });
        });
        widgets::draw_focus_ring(&ctx);
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.take_escape(raw_input);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.show(ui);
    }
}
