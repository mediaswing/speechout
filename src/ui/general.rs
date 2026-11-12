//! The General tab: choose a file, hear it, save it as MP3.

use std::path::PathBuf;
use std::sync::Arc;

use egui::{Key, Ui};

use super::widgets::{RowButton, button_row, dropdown, editable_text_area, heading, path_field, progress_bar, text_area};
use super::{App, JobKind, Loadable, Opened, shortcuts};
use crate::document::{self, FileKind};
use crate::speech::{Provider, Voice};
use crate::vision::LocalAi;
use crate::worker::{self, Control, SpeechJob, display_name};

/// How much of the text the text box shows. egui lays out the whole of a text
/// box every time it changes, and screen readers are given all of it.
const PREVIEW_CHARS: usize = 20_000;

/// Where the text to read comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Source {
    /// A document or image, chosen with the file picker.
    File,
    /// Text typed or pasted into the window.
    FreeText,
}

impl Source {
    const ALL: [Source; 2] = [Source::File, Source::FreeText];

    fn label(self) -> &'static str {
        match self {
            Source::File => "Read a File",
            Source::FreeText => "Free Text",
        }
    }
}

/// The buttons on the line that controls reading.
#[derive(Clone, Copy)]
enum Action {
    Read,
    Pause,
    Stop,
    Save,
}

impl App {
    /// The System voices, and each cloud service with a saved key.
    pub(super) fn available_providers(&self) -> Vec<Provider> {
        Provider::ALL
            .into_iter()
            .filter(|p| *p == Provider::System || self.saved_keys.get(p).copied().unwrap_or(false))
            .collect()
    }

    /// Starts finding a service's voices, unless they are known or on the way.
    pub(super) fn ensure_voices(&mut self, provider: Provider) {
        if matches!(self.voices.get(&provider), Some(Loadable::Ready(_) | Loadable::Loading)) {
            return;
        }
        let key = match provider {
            Provider::System => String::new(),
            _ => self.store.api_key(provider).ok().flatten().unwrap_or_default(),
        };
        self.voices.insert(provider, Loadable::Loading);
        worker::load_voices(&self.rep, provider, key);
    }

    pub(super) fn voices_loaded(&mut self, provider: Provider, result: Result<Vec<Voice>, String>) {
        match result {
            Ok(voices) => {
                self.voices.insert(provider, Loadable::Ready(voices));
            }
            Err(e) => {
                self.voices.insert(provider, Loadable::Failed);
                if self.settings.provider == provider {
                    self.set_error(e);
                }
            }
        }
    }

    /// The voice to use with the current service. System voices can use the
    /// computer's default; the cloud services need a particular one.
    fn current_voice(&self) -> Option<String> {
        let provider = self.settings.provider;
        let saved = self.settings.voices.get(provider.key());
        let Some(Loadable::Ready(voices)) = self.voices.get(&provider) else {
            return (provider == Provider::System).then(String::new);
        };
        let known = saved.filter(|id| voices.iter().any(|v| &v.id == *id)).cloned();
        match provider {
            Provider::System => Some(known.unwrap_or_default()),
            _ => known.or_else(|| voices.first().map(|v| v.id.clone())),
        }
    }

    pub(super) fn choose_file(&mut self) {
        let chosen = self.dialogs.pick_file(
            "Choose a file to read or an image to describe",
            "Documents and images",
            document::EXTENSIONS,
        );
        if let Some(path) = chosen {
            self.open_file(path);
        }
    }

    pub(super) fn open_file(&mut self, path: PathBuf) {
        // A typed name starting with "-" must not look like an option to the
        // programs that convert photos.
        let path = std::path::absolute(&path).unwrap_or(path);
        // Choosing or dropping a file means reading a file.
        self.source = Source::File;
        self.path_input = path.display().to_string();
        let Some(kind) = FileKind::of(&path) else {
            self.set_error("This kind of file can't be read. Choose a PDF, TXT, DOCX, CSV, JPEG, HEIF or HEIC file.");
            return;
        };
        let name = display_name(&path);
        self.load_id += 1;
        self.opened = None;
        let verb = if kind.is_image() { "Describing" } else { "Opening" };
        self.set_info(format!("{verb} {name}."));
        self.loading = Some(name);
        let ai = LocalAi {
            kind: self.settings.ai_kind,
            address: self.settings.ai_address.clone(),
            model: self.settings.ai_model.clone(),
        };
        worker::load_file(&self.rep, self.load_id, path, kind, ai, self.settings.resolve_gps);
    }

    pub(super) fn file_loaded(&mut self, id: u64, result: Result<String, String>) {
        if id != self.load_id {
            return;
        }
        let name = self.loading.take().unwrap_or_default();
        match result {
            Ok(text) => {
                let words = text.split_whitespace().count();
                let mut preview: String = text.chars().take(PREVIEW_CHARS).collect();
                if preview.len() < text.len() {
                    preview.push_str("\n\nThis box shows only the start of the text. All of it is read aloud and saved.");
                }
                self.opened = Some(Opened { text: Arc::new(text), preview, name: name.clone() });
                self.set_info(format!("{name} is ready to read aloud: {words} words."));
            }
            Err(e) => self.set_error(format!("{name}: {e}")),
        }
    }

    /// What's needed to speak, or a message saying what's missing.
    pub(super) fn speech_job(&mut self) -> Result<SpeechJob, String> {
        let text = match self.source {
            Source::File => {
                if let Some(name) = &self.loading {
                    return Err(format!("{name} is still being opened."));
                }
                let Some(opened) = &self.opened else {
                    return Err("Choose a file first.".to_owned());
                };
                opened.text.clone()
            }
            Source::FreeText => {
                if self.free_text.trim().is_empty() {
                    return Err("Type some text to read first.".to_owned());
                }
                Arc::new(self.free_text.clone())
            }
        };
        if self.job.is_some() {
            return Err("Something is already being read or saved. Stop it first.".to_owned());
        }
        let provider = self.settings.provider;
        let Some(voice) = self.current_voice() else {
            // If finding them failed earlier, this tries again.
            self.ensure_voices(provider);
            return Err(format!("The {} voices aren't available yet. Try again in a moment.", provider.label()));
        };
        let key = match provider {
            Provider::System => String::new(),
            _ => match self.store.api_key(provider) {
                Ok(Some(key)) => key,
                _ => return Err(format!("Save an API key for {} on the Settings tab first.", provider.label())),
            },
        };
        let wordlists = self.wordlists.iter().filter(|w| w.enabled).map(|w| w.list.clone()).collect();
        Ok(SpeechJob { text, provider, key, voice, wordlists })
    }

    pub(super) fn read_aloud(&mut self) {
        let job = match self.speech_job() {
            Ok(job) => job,
            Err(e) => return self.set_error(e),
        };
        let control = Arc::new(Control::default());
        self.job = Some((JobKind::Speaking, control.clone()));
        self.progress = 0.0;
        self.set_info("Reading aloud.");
        worker::speak(&self.rep, control, job);
    }

    pub(super) fn save_as_mp3(&mut self) {
        let job = match self.speech_job() {
            Ok(job) => job,
            Err(e) => return self.set_error(e),
        };
        let opened = self.opened.as_ref().filter(|_| self.source == Source::File);
        let stem = opened.map_or("speech", |o| o.name.rsplit_once('.').map_or(o.name.as_str(), |(s, _)| s));
        let suggested = format!("{stem}.mp3");
        let Some(mut path) = self.dialogs.save_file("Save the speech as an MP3 file", &suggested, "MP3 audio", &["mp3"]) else {
            return;
        };
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp3")) {
            path.as_mut_os_string().push(".mp3");
        }
        let control = Arc::new(Control::default());
        self.job = Some((JobKind::Saving, control.clone()));
        self.progress = 0.0;
        self.set_info(format!("Saving {}.", display_name(&path)));
        worker::save_mp3(&self.rep, control, job, path);
    }

    pub(super) fn toggle_pause(&mut self) {
        match &self.job {
            Some((JobKind::Speaking, control)) => {
                let paused = !self.paused;
                control.set_paused(paused);
                self.paused = paused;
                self.set_info(if paused { "Paused." } else { "Resumed." });
            }
            Some((JobKind::Saving, _)) => self.set_error("Saving can't be paused, but it can be stopped."),
            None => self.set_error("Nothing is being read aloud."),
        }
    }

    pub(super) fn stop(&mut self) {
        if let Some((_, control)) = &self.job {
            control.request_stop();
            self.paused = false;
            self.set_info("Stopping.");
        } else if let Some(name) = self.loading.take() {
            // The work can't be interrupted, but its answer will be ignored.
            self.load_id += 1;
            self.set_info(format!("Stopped opening {name}."));
        } else {
            self.set_info("Nothing is being read aloud.");
        }
    }

    /// A full-width progress bar. Screen readers get it as a progress indicator
    /// named "Progress", with a description of what is happening as its value.
    fn progress_bar(&self, ui: &mut Ui) {
        let (fraction, text) = match (&self.job, &self.loading) {
            (Some((JobKind::Speaking, _)), _) if self.paused => (Some(self.progress), "Paused".to_owned()),
            (Some((JobKind::Speaking, _)), _) => (Some(self.progress), format!("Reading aloud, {:.0}%", self.progress * 100.0)),
            (Some((JobKind::Saving, _)), _) => (Some(self.progress), format!("Saving, {:.0}%", self.progress * 100.0)),
            (None, Some(name)) => (None, format!("Opening {name}")),
            (None, None) => (Some(0.0), "Nothing is happening".to_owned()),
        };
        progress_bar(ui, "progress", "Progress", fraction, &text);
    }

    fn voice_dropdown(&mut self, ui: &mut Ui) {
        let provider = self.settings.provider;
        // Saving a new key forgets that service's voices.
        if !self.voices.contains_key(&provider) {
            self.ensure_voices(provider);
        }
        let voices: Option<Vec<Voice>> = match self.voices.get(&provider) {
            Some(Loadable::Ready(voices)) => Some(voices.clone()),
            _ => None,
        };
        let Some(voices) = voices else {
            let placeholder = match self.voices.get(&provider) {
                Some(Loadable::Failed) => "No voices could be found",
                _ => "Finding voices",
            };
            let mut none = 0;
            dropdown(ui, "voice", "Voice", &[placeholder.to_owned()], &mut none);
            return;
        };
        let system = provider == Provider::System;
        let mut names: Vec<String> = Vec::with_capacity(voices.len() + 1);
        if system {
            names.push("The computer's default voice".to_owned());
        }
        names.extend(voices.iter().map(|v| v.name.clone()));
        let offset = usize::from(system);
        let current = self.current_voice().unwrap_or_default();
        let mut index = voices.iter().position(|v| v.id == current).map_or(0, |i| i + offset);
        if dropdown(ui, "voice", "Voice", &names, &mut index) {
            let id = if system && index == 0 { String::new() } else { voices[index - offset].id.clone() };
            self.settings.voices.insert(provider.key().to_owned(), id);
            self.save_settings();
        }
    }

    /// The box for a file's path, with its Choose button.
    fn file_picker(&mut self, ui: &mut Ui) {
        let picker = path_field(
            ui,
            "File to read (PDF, TXT, DOCX or CSV) or image to describe (JPEG, HEIF or HEIC). Type its path and press Enter, or use the button to choose it.",
            "file_path",
            &mut self.path_input,
            "Choose a file",
            Some(&shortcuts::OPEN),
        );
        let field = picker.field;
        if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            field.request_focus();
            let typed = self.path_input.trim().to_owned();
            if typed.is_empty() {
                self.set_error("Type the path of a file first.");
            } else {
                self.open_file(PathBuf::from(typed));
            }
        }
        if picker.choose {
            self.choose_file();
        }
    }

    pub(super) fn general(&mut self, ui: &mut Ui) {
        heading(ui, "General", 2);

        let mut index = Source::ALL.iter().position(|s| *s == self.source).unwrap_or(0);
        let names: Vec<String> = Source::ALL.iter().map(|s| s.label().to_owned()).collect();
        if dropdown(ui, "source", "What to read", &names, &mut index) {
            self.source = Source::ALL[index];
        }
        match self.source {
            Source::File => self.file_picker(ui),
            Source::FreeText => {
                editable_text_area(ui, "Free Text: type or paste the text to read aloud", "free_text", &mut self.free_text);
            }
        }

        let providers = self.available_providers();
        let names: Vec<String> = providers.iter().map(|p| p.label().to_owned()).collect();
        let mut index = providers.iter().position(|p| *p == self.settings.provider).unwrap_or(0);
        if dropdown(ui, "provider", "Speech Service", &names, &mut index) {
            let provider = providers[index];
            self.settings.provider = provider;
            self.ensure_voices(provider);
            self.save_settings();
            // Said here too, because the Tab key doesn't stop at the note below.
            if provider == Provider::System {
                self.set_info("System voices stay on this computer.");
            } else {
                self.set_info(format!("{} is a cloud service: the text is sent to it over the internet.", provider.label()));
            }
        }
        if self.settings.provider != Provider::System {
            ui.label(format!(
                "{} is a cloud service: the text is sent to it over the internet. System voices stay on this computer.",
                self.settings.provider.label()
            ));
        }
        self.voice_dropdown(ui);

        let pause_label = if self.paused { "Resume" } else { "Pause" };
        let pressed = button_row(
            ui,
            &[
                RowButton { action: Action::Read, salt: "read", text: "Read Aloud", shortcut: Some(&shortcuts::READ) },
                RowButton { action: Action::Pause, salt: "pause", text: pause_label, shortcut: Some(&shortcuts::PAUSE) },
                RowButton { action: Action::Stop, salt: "stop", text: "Stop", shortcut: Some(&shortcuts::STOP) },
                RowButton { action: Action::Save, salt: "save", text: "Save as MP3", shortcut: Some(&shortcuts::SAVE) },
            ],
        );
        match pressed {
            Some(Action::Read) => self.read_aloud(),
            Some(Action::Pause) => self.toggle_pause(),
            Some(Action::Stop) => self.stop(),
            Some(Action::Save) => self.save_as_mp3(),
            None => {}
        }
        self.progress_bar(ui);

        // Typed text is already on show in its own box above.
        if self.source == Source::File {
            let shown = self.opened.as_ref().map_or("", |o| o.preview.as_str());
            text_area(ui, "Text that will be read aloud, before wordlists are applied", shown);
        }
    }
}
