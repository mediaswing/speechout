//! The Settings tab. Everything here is stored in the SQLite database as soon as
//! it is changed, apart from API keys, which have a button, because typing one
//! a character at a time shouldn't save each partial key.

use std::path::{Path, PathBuf};

use egui::{Key, Ui};

use super::widgets::{dropdown, full_button, heading, password_field, path_field, text_field};
use super::{App, Loadable, appearance, shortcuts};
use crate::logging;
use crate::speech::Provider;
use crate::store::{AiKind, Appearance};
use crate::vision;
use crate::worker;

const AUTOMATIC_MODEL: &str = "Choose a model automatically";
const GPS_OFF: &str = "No, don't look up where photos were taken";
const GPS_ON: &str = "Yes, send a photo's rounded location to OpenStreetMap to find the place name";

impl App {
    pub(super) fn save_api_key(&mut self) {
        let provider = self.key_service;
        let key = self.key_input.trim().to_owned();
        if key.is_empty() {
            return self.set_info(format!("Type the {} API key first.", provider.label()));
        }
        if key.chars().any(|c| c.is_control() || c.is_whitespace()) {
            return self.set_error(format!("The {} API key has spaces or other characters a key can't have.", provider.label()));
        }
        if let Err(e) = self.store.set_api_key(provider, &key) {
            log::error!("Could not save an API key: {e:#}");
            return self.set_error(format!("The {} API key could not be saved.", provider.label()));
        }
        self.key_input.clear();
        self.saved_keys.insert(provider, true);
        // A new key can see different voices.
        self.voices.remove(&provider);
        self.set_info(format!("Saved the API key for {}, without encryption. Its voices are on the General tab.", provider.label()));
    }

    fn remove_api_key(&mut self, provider: Provider) {
        if let Err(e) = self.store.remove_api_key(provider) {
            log::error!("Could not remove an API key: {e:#}");
            return self.set_error(format!("The {} API key could not be removed.", provider.label()));
        }
        self.saved_keys.insert(provider, false);
        self.voices.remove(&provider);
        if self.settings.provider == provider {
            self.settings.provider = Provider::System;
            self.ensure_voices(Provider::System);
            self.save_settings();
        }
        // The button just pressed has gone, so the focus needs somewhere to go.
        self.focus_wanted = Some("save_key");
        self.set_info(format!("Removed the API key for {}.", provider.label()));
    }

    fn api_keys_section(&mut self, ui: &mut Ui) {
        heading(ui, "Speech Services", 3);
        let where_stored = match self.store.path() {
            Some(path) => format!("API keys are stored without encryption in {}. Anyone who can read that file can use them.", path.display()),
            None => "API keys won't be kept after the app closes, because the settings database could not be opened.".to_owned(),
        };
        ui.label(where_stored);
        let has_key = |app: &Self, provider| app.saved_keys.get(&provider).copied().unwrap_or(false);
        // With one box on show, this is where to see which services have a key.
        let names: Vec<String> = Provider::CLOUD
            .iter()
            .map(|p| if has_key(self, *p) { format!("{} (key saved)", p.label()) } else { p.label().to_owned() })
            .collect();
        let mut index = Provider::CLOUD.iter().position(|p| *p == self.key_service).unwrap_or(0);
        if dropdown(ui, "key_service", "Speech Service", &names, &mut index) {
            self.key_service = Provider::CLOUD[index];
            // What was typed was meant for the other service, so it must never
            // be saved for this one.
            if !self.key_input.is_empty() {
                self.key_input.clear();
                self.set_info("Changing the speech service cleared the API key you had typed, which wasn't saved.");
            }
        }
        let provider = self.key_service;
        let saved = has_key(self, provider);
        let label = if saved {
            format!("{} API key (one is saved; type a new one to replace it)", provider.label())
        } else {
            format!("{} API key", provider.label())
        };
        // A box per service, as far as egui can tell, so that undo can't bring
        // back a key typed for another service.
        password_field(ui, &label, provider.key(), &mut self.key_input);
        if let Some(url) = provider.signup_url() {
            // Opens in the web browser. Hovering shows where it goes.
            ui.vertical_centered(|ui| ui.hyperlink_to(format!("Sign up for {}", provider.label()), url));
        }
        let save = full_button(ui, "save_key", "Save API key", Some(&shortcuts::SAVE));
        if self.wants_focus("save_key") {
            save.request_focus();
        }
        if save.clicked() {
            self.save_api_key();
        }
        if saved && full_button(ui, "remove_key", &format!("Remove the {} API key", provider.label()), None).clicked() {
            self.remove_api_key(provider);
        }
    }

    fn appearance_section(&mut self, ui: &mut Ui) {
        heading(ui, "Appearance", 3);
        let names: Vec<String> = Appearance::ALL.iter().map(|a| format!("{}: {}", a.label(), a.description())).collect();
        let mut index = Appearance::ALL.iter().position(|a| *a == self.settings.appearance).unwrap_or(0);
        if dropdown(ui, "appearance", "Colours and text", &names, &mut index) {
            self.settings.appearance = Appearance::ALL[index];
            appearance::apply(ui.ctx(), self.settings.appearance);
            self.save_settings();
            self.set_info(format!("Using the {} appearance.", self.settings.appearance.label()));
        }
    }

    pub(super) fn models_listed(&mut self, result: Result<Vec<String>, String>, requested: bool) {
        match result {
            Ok(models) => {
                if requested {
                    self.set_info(format!("Found {} models.", models.len()));
                }
                self.models = Loadable::Ready(models);
            }
            Err(e) => {
                self.models = Loadable::Failed;
                // At startup, not having a local AI system isn't a problem.
                if requested {
                    self.set_error(e);
                }
            }
        }
    }

    /// Checks the address box and starts listing that server's models.
    fn refresh_models(&mut self) {
        match vision::check_local_address(&self.ai_address_input) {
            Ok(address) => {
                self.ai_address_input = address.clone();
                self.settings.ai_address = address.clone();
                self.save_settings();
                self.models = Loadable::Loading;
                self.set_info("Looking for models.");
                worker::list_models(&self.rep, self.settings.ai_kind, address, true);
            }
            Err(e) => self.set_error(worker::sentence(&e)),
        }
    }

    fn images_section(&mut self, ui: &mut Ui) {
        heading(ui, "Describing Images", 3);
        let names: Vec<String> = AiKind::ALL.iter().map(|k| k.label().to_owned()).collect();
        let mut index = AiKind::ALL.iter().position(|k| *k == self.settings.ai_kind).unwrap_or(0);
        if dropdown(ui, "ai_kind", "Local AI system", &names, &mut index) {
            let old = self.settings.ai_kind;
            self.settings.ai_kind = AiKind::ALL[index];
            if self.ai_address_input == old.default_address() {
                self.ai_address_input = self.settings.ai_kind.default_address().to_owned();
            }
            self.settings.ai_model.clear();
            self.refresh_models();
        }
        let field = text_field(ui, "Local AI address, which must be on this computer", "ai_address", &mut self.ai_address_input);
        if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            field.request_focus();
            self.refresh_models();
        }

        let mut models = vec![AUTOMATIC_MODEL.to_owned()];
        if let Loadable::Ready(found) = &self.models {
            models.extend(found.iter().cloned());
        }
        // A saved model that the server isn't offering now stays visible.
        if !self.settings.ai_model.is_empty() && !models.contains(&self.settings.ai_model) {
            models.push(self.settings.ai_model.clone());
        }
        let mut index = models.iter().position(|m| *m == self.settings.ai_model).unwrap_or(0);
        if dropdown(ui, "ai_model", "Image description model", &models, &mut index) {
            self.settings.ai_model = if index == 0 { String::new() } else { models[index].clone() };
            self.save_settings();
        }
        if full_button(ui, "refresh_models", "Refresh the list of models", None).clicked() {
            self.refresh_models();
        }

        let options = [GPS_OFF.to_owned(), GPS_ON.to_owned()];
        let mut index = usize::from(self.settings.resolve_gps);
        if dropdown(ui, "resolve_gps", "Say where a geotagged photo was taken, at the end of its description", &options, &mut index) {
            self.settings.resolve_gps = index == 1;
            self.save_settings();
        }
    }

    fn set_whisper_program(&mut self, typed: &str) {
        let typed = typed.trim();
        self.whisper_input = typed.to_owned();
        self.settings.whisper_program = (!typed.is_empty()).then(|| PathBuf::from(typed));
        self.save_settings();
        match &self.settings.whisper_program {
            None => self.set_info("whisper.cpp will be looked for in the usual places."),
            Some(path) if path.is_file() => self.set_info(format!("Using whisper.cpp at {}.", path.display())),
            Some(path) => self.set_error(format!("There's no program at {}. Choose whisper-cli instead.", path.display())),
        }
    }

    fn transcription_section(&mut self, ui: &mut Ui) {
        heading(ui, "Transcribing Audio", 3);
        ui.label(super::audio_tab::whisper_status(self.settings.whisper_program.as_deref()));
        let picker = path_field(
            ui,
            "whisper.cpp program (whisper-cli). Leave it empty to look in the usual places. Type its path and press \
             Enter, or use the button to choose it.",
            "whisper_program",
            &mut self.whisper_input,
            "Choose the whisper.cpp program",
            None,
        );
        let field = picker.field;
        if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            field.request_focus();
            let typed = self.whisper_input.clone();
            self.set_whisper_program(&typed);
        }
        let extensions: &[&str] = if cfg!(windows) { &["exe"] } else { &[] };
        if picker.choose
            && let Some(path) = self.dialogs.pick_file("Choose the whisper.cpp program", "Programs", extensions)
        {
            self.set_whisper_program(&path.display().to_string());
        }
    }

    fn set_log_dir(&mut self, dir: &Path) {
        match logging::set_dir(dir) {
            Ok(path) => {
                self.settings.log_dir = Some(dir.to_owned());
                self.save_settings();
                self.set_info(format!("The debug log is now {}.", path.display()));
            }
            Err(e) => {
                log::warn!("Could not move the log: {e:#}");
                self.set_error(format!("The debug log was not moved: {}", worker::sentence(&e)));
            }
        }
    }

    fn log_section(&mut self, ui: &mut Ui) {
        let picker = path_field(
            ui,
            "Folder for debug logs. Type its path and press Enter, or use the button to choose it.",
            "log_dir",
            &mut self.log_dir_input,
            "Choose the folder for debug logs",
            None,
        );
        let field = picker.field;
        if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            field.request_focus();
            let typed = PathBuf::from(self.log_dir_input.trim());
            self.set_log_dir(&typed);
        }
        if picker.choose
            && let Some(dir) = self.dialogs.pick_folder("Choose the folder for debug logs")
        {
            self.log_dir_input = dir.display().to_string();
            self.set_log_dir(&dir);
        }
    }

    pub(super) fn settings_tab(&mut self, ui: &mut Ui) {
        heading(ui, "Settings", 2);
        self.api_keys_section(ui);
        ui.add_space(8.0);
        self.appearance_section(ui);
        ui.add_space(8.0);
        self.images_section(ui);
        self.log_section(ui);
        ui.add_space(8.0);
        self.transcription_section(ui);
        ui.add_space(8.0);
        self.updates_section(ui);
    }
}
