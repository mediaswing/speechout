//! The window: five tabs (General, Settings, Wordlists, Audio Player,
//! Describe Video) with
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

mod general_tab;
mod keys;
mod labels;
mod models;
mod player_tab;
mod settings_tab;
mod video_tab;
mod widgets;
mod wordlists_tab;

use crate::APP_TITLE;
use crate::audio::{AudioFormat, Cue, Listened, SoundKind};
use crate::document::{DocumentReading, FileKind};
use crate::i18n::{self, Language, Translation, short_and_full, short_key, t, tf};
use crate::playlist::Playlist;
use crate::settings::Settings;
use crate::shortcuts::{self, Action, Command, KeyContext, Keymap, Platform, Shortcut};
use crate::speech::{self, Provider, Voice};
use crate::speakers::SpeakerLabels;
use crate::transcribe::WhisperModel;
use crate::wordlist::{self, Installed};
use crate::worker::{self, Control, Msg, Reporter, SpeechJob};
use egui::accesskit::{Live, Role};
use egui::{Button, Color32, EventFilter, Id, Key, Modifiers, RichText, Ui};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, channel};
use std::time::Duration;
use general_tab::Source;
use labels::*;
use models::Waiting;
use player_tab::PlayerTab;
use video_tab::VideoTab;
use widgets::*;

const CONTROL_HEIGHT: f32 = 36.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    General,
    Settings,
    Wordlists,
    Player,
    Video,
}

impl Tab {
    const ALL: [Tab; 5] = [Tab::General, Tab::Settings, Tab::Wordlists, Tab::Player, Tab::Video];

    fn label(self) -> String {
        match self {
            Tab::General => t("tab.general"),
            Tab::Settings => t("tab.settings"),
            Tab::Wordlists => t("tab.wordlists"),
            Tab::Player => t("tab.player"),
            Tab::Video => t("tab.video"),
        }
    }

    /// Stays the same whatever the interface language, so focus can find it.
    fn id(self) -> Id {
        Id::new(("tab", self as u8))
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
    /// Downloading an AI model into Ollama.
    Downloading,
    /// Translating the interface with an AI model.
    Translating,
    /// Turning the speech in an audio file into text with Whisper.
    Transcribing,
    /// Taking the pictures out of a video and describing them.
    DescribingVideo,
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
    /// Whether `file`, such as a PNG screenshot, must be read with the
    /// system voices. Worked out when the file is loaded, from what it holds.
    file_system_voices_only: bool,
    /// Text read from the chosen file.
    text: String,
    source: Source,
    /// The text box as typed or pasted, and the cleaned text read aloud.
    pasted: String,
    pasted_text: String,
    /// The Web Reader's address box, and the page last read from it.
    web_address: String,
    web_title: String,
    web_text: String,
    loading_page: bool,
    loading_file: bool,
    /// A photo or video chosen before a local AI model was found, to
    /// describe once the check for Ollama finishes.
    waiting: Option<Waiting>,
    /// Ollama is being installed or started for a waiting photo or video.
    setting_up_ollama: bool,
    /// ffmpeg is being installed for a waiting video or WMA file.
    setting_up_ffmpeg: bool,
    /// The video or WMA file to open once ffmpeg is installed. Kept apart
    /// from `waiting`, so finding or downloading an AI model meanwhile
    /// doesn't take it.
    waiting_for_ffmpeg: Option<Waiting>,
    /// The Describe Video tab.
    video: VideoTab,
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
    /// The Audio Player tab.
    player: PlayerTab,
    cues: crate::audio::Cues,
    keymap: Keymap,
    /// Shortcuts pressed since the last frame, taken from the input before
    /// egui saw it.
    pending: Vec<Command>,
    /// The tab buttons' ids last frame, to tell when one has focus.
    tab_buttons: Vec<Id>,
    /// Read aloud is on the right-click menu in File Explorer, Finder or the
    /// file manager.
    context_menu: bool,
    /// The Windows Installer installed the app, and manages that menu item.
    #[cfg(windows)]
    context_menu_managed: bool,
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

        let screen_reader = crate::platform::screen_reader_running();
        log::info!("screen reader running: {screen_reader}");
        let keymap = Keymap::new(settings.shortcuts_enabled(), &settings.shortcuts, Platform::CURRENT);
        i18n::set_hints(keymap.hints());
        let mut status = format!("{} {log_status}", t("status.ready"));
        if screen_reader && settings.shortcuts_on.is_none() {
            status = format!("{} {}", status.trim_end(), t("status.screen_reader"));
        }
        crate::platform::context_menu::refresh();
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
            file_system_voices_only: false,
            text: String::new(),
            source: Source::File,
            pasted: String::new(),
            pasted_text: String::new(),
            web_address: String::new(),
            web_title: String::new(),
            web_text: String::new(),
            loading_page: false,
            loading_file: false,
            waiting: None,
            setting_up_ollama: false,
            setting_up_ffmpeg: false,
            waiting_for_ffmpeg: None,
            video: VideoTab::default(),
            fitted_to_screen: false,
            job: None,
            paused: false,
            checking_update: false,
            progress: 0.0,
            status,
            wordlists: Vec::new(),
            wordlist_to_remove: 0,
            languages: i18n::available(&crate::paths::languages_dir()),
            focus: Some(Tab::General.id()),
            player: PlayerTab::default(),
            cues: crate::audio::Cues::default(),
            keymap,
            pending: Vec::new(),
            tab_buttons: Vec::new(),
            context_menu: crate::platform::context_menu::is_installed(),
            #[cfg(windows)]
            context_menu_managed: crate::platform::context_menu::managed_by_installer(),
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
        let playing = self.player.control.is_some() && !self.player.paused;
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
            Some((JobKind::DescribingVideo, _)) => "progress.describing_video",
            None if self.setting_up_ollama => "progress.setting_up",
            None if self.setting_up_ffmpeg => "progress.setting_up_ffmpeg",
            None if self.loading_file || self.player.loader.is_some() => "progress.opening",
            None if self.loading_page => "progress.getting_page",
            None => "progress.nothing",
        };
        let msg = tf(key, &[("percent", &percent)]);
        self.announce(msg);
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
                Msg::Voices(provider, result) => self.voices_loaded(provider, result),
                Msg::Models(result) => self.models_listed(result),
                Msg::ModelDownloaded(model, result) => self.model_downloaded(model, result),
                Msg::OllamaReady(result) => self.ollama_ready(result),
                Msg::Loaded(path, result) => self.file_loaded(path, result),
                Msg::PageLoaded(result) => self.page_loaded(result),
                Msg::Translated(result) => self.translated(result),
                Msg::AudioLoaded(path, result) => self.audio_loaded(path, result),
                Msg::PlaylistOpened(path, result) => self.playlist_opened(path, result),
                Msg::AudioPosition(position) => self.audio_position_changed(position),
                Msg::AudioEnded(result) => self.audio_ended(result),
                Msg::WhisperDownloaded(model, result) => self.whisper_downloaded(model, result),
                Msg::SpeakerModelsDownloaded(result) => self.speaker_models_downloaded(result),
                Msg::Transcribed(path, result) => self.transcribed(path, result),
                Msg::FfmpegReady(result) => self.ffmpeg_ready(result),
                Msg::VideoFrame(frame) => self.video_frame_arrived(frame),
                Msg::VideoDescribed(path, result) => self.video_described(path, result),
                Msg::VideoLength(play, length) => self.video_length_found(play, length),
                Msg::VideoPlaying(play, position, picture) => self.video_playing(play, position, picture),
                Msg::VideoPlayEnded(play, result) => self.video_play_ended(play, result),
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

    fn switch_tab(&mut self, tab: Tab) {
        self.tab = tab;
        self.focus = Some(tab.id());
    }

    fn tab_bar(&mut self, ui: &mut Ui) {
        self.tab_buttons.clear();
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
                self.tab_buttons.push(resp.id);
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
            Some((JobKind::DescribingVideo, _)) => (Some(self.progress), "bar.describing_video"),
            None if self.setting_up_ollama => (None, "bar.setting_up"),
            None if self.setting_up_ffmpeg => (None, "bar.setting_up_ffmpeg"),
            None if self.loading_file || self.player.loader.is_some() => (None, "bar.opening"),
            None if self.loading_page => (None, "bar.getting_page"),
            None if self.player.control.is_some() => {
                let key = if self.player.paused { "bar.audio_paused" } else { "bar.audio_playing" };
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
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.take_shortcuts(ctx, raw_input);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.fit_to_screen(&ctx);
        self.handle_messages();
        self.run_shortcuts();

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.add_space(4.0);
            self.tab_bar(ui);
            ui.add_space(4.0);
        });
        if let Some((image, label)) = self.video.pending_preview.take() {
            let texture = ctx.load_texture("video_preview", image, egui::TextureOptions::LINEAR);
            self.video.preview = Some((texture, label));
        }
        // The video's pictures are only seen beside its description.
        if let Some(control) = &self.video.control {
            control.set_pictures_hidden(!(self.tab == Tab::Video && self.video.editing));
        }
        if let Some(image) = self.video.pending_picture.take() {
            match &mut self.video.picture {
                Some(texture) => texture.set(image, egui::TextureOptions::LINEAR),
                None => self.video.picture = Some(ctx.load_texture("video_picture", image, egui::TextureOptions::LINEAR)),
            }
        }
        egui::Panel::bottom("status").show(ui, |ui| {
            ui.add_space(6.0);
            // The Describe Video tab has the progress bar under its preview.
            if self.tab != Tab::Video {
                self.progress_bar(ui);
                ui.add_space(4.0);
            }
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
                    Tab::Video => self.video_tab(ui),
                }
            });
        });

        draw_focus_ring(&ctx);
    }
}
