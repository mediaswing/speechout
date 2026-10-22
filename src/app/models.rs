//! The local AI models that describe photos and videos: finding them in
//! Ollama, offering to install Ollama or download a model, and carrying on
//! with a photo or video that was waiting for one.

use super::*;

/// A photo or video chosen before the programs that describe it were found
/// or set up, to describe once they are.
pub(super) enum Waiting {
    Photo(PathBuf),
    Video(PathBuf),
}

impl Waiting {
    fn path(&self) -> &std::path::Path {
        match self {
            Waiting::Photo(p) | Waiting::Video(p) => p,
        }
    }
}

impl SpeechApp {
    pub(super) fn ensure_models(&mut self) {
        if self.models.is_none() {
            self.models = Some(Loadable::Loading);
            self.rep.spawn(worker::load_models);
        }
    }

    /// Ollama has listed its models, or couldn't be reached.
    pub(super) fn models_listed(&mut self, result: Result<Vec<String>, String>) {
        let waiting = self.waiting.is_some();
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
        if let Some(waiting) = self.waiting.take() {
            if matches!(waiting, Waiting::Photo(_)) {
                self.loading_file = false;
            }
            match &result {
                Err(_) => self.offer_ollama(waiting),
                Ok(models) if !crate::vision::has_vision_model(models) => {
                    self.offer_model_download(Some(waiting));
                }
                Ok(_) => self.resume(waiting),
            }
        }
        self.models = Some(match result {
            Ok(m) => Loadable::Ready(m),
            Err(_) => Loadable::Failed,
        });
    }

    /// Downloading a model into Ollama finished: true if complete, false if stopped.
    pub(super) fn model_downloaded(&mut self, model: String, result: Result<bool, String>) {
        self.job = None;
        let waiting = self.waiting.take();
        match result {
            Ok(true) => {
                self.settings.vision_model = model.clone();
                self.settings.save();
                self.models = None;
                self.ensure_models();
                match waiting {
                    Some(waiting) => self.resume(waiting),
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

    /// Installing and/or starting Ollama finished.
    pub(super) fn ollama_ready(&mut self, result: Result<(), String>) {
        self.setting_up_ollama = false;
        match result {
            Ok(()) => {
                // Look for models, then describe the waiting photo.
                self.announce(t("ollama.running"));
                self.models = None;
                self.ensure_models();
            }
            Err(e) => {
                if matches!(self.waiting.take(), Some(Waiting::Photo(_))) {
                    self.loading_file = false;
                }
                self.show_error(tf("ollama.setup_failed", &[("error", &e)]));
            }
        }
    }

    /// Ollama should be checked first: no model is chosen, Ollama was not
    /// running at the last check, or it has no model for photos. Then the
    /// user is offered a fix instead of a failed description.
    pub(super) fn needs_model_check(&self) -> bool {
        self.settings.vision_model.is_empty()
            || match &self.models {
                Some(Loadable::Failed) => true,
                Some(Loadable::Ready(m)) => !crate::vision::has_vision_model(m),
                _ => false,
            }
    }

    /// Looks for Ollama again, in case it has been installed or started
    /// since the last check, and describes `waiting` when the answer arrives.
    pub(super) fn wait_for_model(&mut self, waiting: Waiting) {
        if !matches!(self.models, Some(Loadable::Loading)) {
            self.models = None;
        }
        self.ensure_models();
        self.loading_file = matches!(waiting, Waiting::Photo(_));
        self.announce(tf("file.looking_for_model", &[("name", &file_name(waiting.path()))]));
        self.waiting = Some(waiting);
    }

    /// Carries on describing a photo or video once Ollama has a model.
    fn resume(&mut self, waiting: Waiting) {
        match waiting {
            Waiting::Photo(path) => self.open_file(path, true),
            Waiting::Video(path) => self.describe_video(path, String::new()),
        }
    }

    /// Ollama could not be reached when a photo or video was chosen. Offers
    /// to start it, install it with the package manager, or open its
    /// download page.
    fn offer_ollama(&mut self, waiting: Waiting) {
        let name = file_name(waiting.path());
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
        self.loading_file = matches!(waiting, Waiting::Photo(_));
        self.waiting = Some(waiting);
        self.announce(match manager {
            Some(manager) if !installed => tf("ollama.installing", &[("manager", &manager)]),
            _ => t("ollama.starting"),
        });
        self.rep.spawn(move |rep| worker::set_up_ollama(rep, !installed));
    }

    /// Ollama has no model that understands images. Asks whether to download
    /// one, then describes the photo or video that is waiting, if any.
    pub(super) fn offer_model_download(&mut self, waiting: Option<Waiting>) {
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
        self.waiting = waiting;
        self.announce(tf("model.downloading", &[("model", &model), ("size", &model_size())]));
        self.rep.spawn(move |rep| worker::download_model(rep, control, model.to_owned()));
    }

    /// The Ollama models, and what to show in a list of them: the models, or
    /// a line saying why there are none.
    pub(super) fn model_choices(&mut self) -> (Vec<String>, Vec<String>) {
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
}
