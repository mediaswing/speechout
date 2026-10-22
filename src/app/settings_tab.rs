//! The Settings tab: API keys, the debug log, the interface language and its
//! translation, the right-click menu, keyboard shortcuts and update checks.

use super::*;

impl SpeechApp {
    pub(super) fn update_checked(&mut self, result: Result<Option<crate::update::Release>, String>, requested: bool) {
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

    pub(super) fn translated(&mut self, result: Result<worker::Translated, String>) {
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

    pub(super) fn settings_tab(&mut self, ui: &mut Ui) {
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
        let camera_options = vec![t("settings.camera_off"), t("settings.camera_on")];
        let mut cindex = usize::from(self.settings.read_camera);
        if dropdown(ui, "camera", &t("settings.camera"), &camera_options, &mut cindex, true) {
            self.settings.read_camera = cindex == 1;
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

        #[cfg(windows)]
        self.context_menu_section(ui);
        self.shortcuts_section(ui);
    }

    /// Says whether Read aloud is on the right-click menu in File Explorer.
    /// The installer adds and removes it, so this says how to change that.
    /// Without the installer, a menu item left by an earlier version can be
    /// removed here.
    #[cfg(windows)]
    fn context_menu_section(&mut self, ui: &mut Ui) {
        use crate::platform::context_menu;
        ui.add_space(8.0);
        heading(ui, &t("context_menu.heading"));
        ui.label(t("context_menu.note"));
        if self.context_menu_managed {
            ui.label(t(if self.context_menu { "context_menu.on" } else { "context_menu.off" }));
        } else if self.context_menu {
            ui.label(t("context_menu.leftover"));
            if full_button(ui, &t("context_menu.remove"), true).clicked() {
                let result = context_menu::remove();
                self.context_menu = context_menu::is_installed();
                match result {
                    Ok(()) => self.announce(t("context_menu.removed")),
                    Err(e) => self.show_error(tf("context_menu.failed", &[("error", &format!("{e:#}"))])),
                }
            }
        } else {
            ui.label(t("context_menu.needs_installer"));
        }
    }

    /// Turns shortcuts on or off, and lets each be changed or turned off.
    /// Each list offers only shortcuts that pass the conflict checks in
    /// `shortcuts.rs` and that no other action uses, so picking one can
    /// never take a key from somewhere else.
    fn shortcuts_section(&mut self, ui: &mut Ui) {
        ui.add_space(8.0);
        heading(ui, &t("shortcuts.heading"));
        let options = vec![t("shortcuts.use_on"), t("shortcuts.use_off")];
        let mut index = usize::from(!self.settings.shortcuts_enabled());
        if dropdown(ui, "shortcuts", &t("shortcuts.use"), &options, &mut index, true) {
            self.settings.shortcuts_on = Some(index == 0);
            self.set_shortcuts();
            self.announce(t(if index == 0 { "shortcuts.turned_on" } else { "shortcuts.turned_off" }));
        }
        let platform = self.keymap.platform();
        for action in Action::ALL {
            let choices: Vec<Shortcut> = action
                .choices(platform)
                .into_iter()
                .filter(|s| self.keymap.used_by(*s, action).is_none())
                .collect();
            let mut labels = vec![t("shortcuts.none")];
            labels.extend(choices.iter().map(|s| self.keymap.describe(action, *s)));
            let current = self.keymap.binding(action);
            let mut index = current.and_then(|c| choices.iter().position(|s| *s == c)).map_or(0, |i| i + 1);
            if dropdown(ui, &format!("shortcut_{}", action.id()), &action.label(), &labels, &mut index, true) {
                let chosen = index.checked_sub(1).and_then(|i| choices.get(i).copied());
                if chosen == action.default_shortcut(platform) {
                    self.settings.shortcuts.remove(action.id());
                } else {
                    let text = chosen.map_or_else(|| shortcuts::OFF.to_owned(), |s| s.to_text(platform));
                    self.settings.shortcuts.insert(action.id().to_owned(), text);
                }
                self.set_shortcuts();
            }
        }
        ui.label(t("shortcuts.note"));
        if full_button(ui, &self.keyed("shortcuts.show", Action::Help), true).clicked() {
            self.show_shortcuts();
        }
        let changed = !self.settings.shortcuts.is_empty() || self.settings.shortcuts_on.is_some();
        if full_button(ui, &t("shortcuts.reset"), changed).clicked() {
            self.settings.shortcuts.clear();
            self.settings.shortcuts_on = None;
            self.set_shortcuts();
            self.announce(t("shortcuts.reset_done"));
        }
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
}
