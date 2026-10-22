//! The General tab: choosing a document or pasting text, the voice, and
//! reading it aloud or saving it as audio.

use super::*;

/// Spoken by the Preview voice button. Kept short because cloud services
/// charge by the character.
const PREVIEW_TEXT: &str = "This is a preview of the selected voice.";

/// Where the text to read aloud comes from, chosen on the General tab.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
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

impl SpeechApp {
    /// The text that will be read aloud: the video's description on the
    /// Describe Video tab, otherwise the file's or the text box's.
    fn current_text(&self) -> &str {
        if self.tab == Tab::Video {
            return &self.video.text;
        }
        match self.source {
            Source::File => &self.text,
            Source::Paste => &self.pasted_text,
        }
    }

    pub(super) fn copy_text(&mut self, ctx: &egui::Context) {
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

    /// The service speech is made with: the chosen one, unless it can't be
    /// used or the loaded file must be read with the system voices. The
    /// choice itself is kept for the next file.
    fn active_provider(&self) -> Provider {
        let p = self.settings.provider;
        p.or_system(self.api_keys.contains_key(&p), self.system_voices_only())
    }

    /// Whether the loaded file, such as a PNG screenshot, must be read with
    /// the system voices.
    fn system_voices_only(&self) -> bool {
        self.tab != Tab::Video && self.source == Source::File && self.file_system_voices_only
    }

    fn ensure_voices(&mut self, provider: Provider) {
        if self.voices.contains_key(&provider) {
            return;
        }
        self.voices.insert(provider, Loadable::Loading);
        let key = self.api_keys.get(&provider).cloned();
        self.rep.spawn(move |rep| worker::load_voices(rep, provider, key));
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

    /// A service has listed its voices, or failed to.
    pub(super) fn voices_loaded(&mut self, provider: Provider, result: Result<Vec<Voice>, String>) {
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

    /// The chosen file has been read, or couldn't be.
    pub(super) fn file_loaded(&mut self, path: PathBuf, result: Result<worker::LoadedFile, String>) {
        self.loading_file = false;
        let name = file_name(&path);
        match result {
            Ok(loaded) => {
                let words = loaded.text.split_whitespace().count();
                self.text = loaded.text;
                self.file_system_voices_only = loaded.system_voices_only;
                self.file = Some(path);
                self.source = Source::File;
                let mut message = tf("file.loaded", &[("name", &name), ("count", &words)]);
                // Say so when this file changes the service from the one chosen,
                // not when it couldn't be used anyway, such as without its API key.
                let chosen = self.settings.provider;
                if self.system_voices_only() && chosen != Provider::System && self.available_providers().contains(&chosen) {
                    message = format!("{message} {}", t("general.service_locked"));
                }
                self.announce(message);
                self.cue(Cue::Success);
            }
            Err(e) => self.show_error(tf("file.load_failed", &[("name", &name), ("error", &e)])),
        }
    }

    pub(super) fn choose_file(&mut self) {
        if self.loading_file || self.preparing_video() {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title(t("file.dialog_title"))
            .add_filter(t("file.filter_all"), &FileKind::extensions(|_| true))
            .add_filter(t("file.filter_documents"), &FileKind::extensions(|k| k != FileKind::Image))
            .add_filter(t("file.filter_photos"), &FileKind::extensions(|k| k == FileKind::Image))
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
        if is_image && self.needs_model_check() {
            self.wait_for_model(Waiting::Photo(path));
            return;
        }
        self.open_file(path, is_image);
    }

    pub(super) fn open_file(&mut self, path: PathBuf, is_image: bool) {
        self.loading_file = true;
        let key = if is_image { "file.describing" } else { "file.opening" };
        self.announce(tf(key, &[("name", &file_name(&path))]));
        let model = self.settings.vision_model.clone();
        let resolve = self.settings.resolve_location;
        let camera = self.settings.read_camera;
        let reading = self.settings.document_reading;
        self.rep.spawn(move |rep| worker::load_file(rep, path, model, resolve, camera, reading));
    }

    /// Opens the loaded DOCX, ODT, Markdown or EPUB file again, so a change to how
    /// such files are read shows straight away. While the app is busy, the change
    /// waits until the file is next opened.
    fn reread_document(&mut self) {
        let Some(path) = self.file.clone().filter(|_| self.source == Source::File) else { return };
        if !FileKind::from_path(&path).is_some_and(FileKind::reads_formatting) {
            return;
        }
        if self.loading_file || self.is_busy() {
            self.announce(t("settings.reading_next_time"));
            return;
        }
        self.open_file(path, false);
    }

    /// A job for the loaded document, with the wordlists applied and then
    /// email addresses, dates, numbers and so on put the way they are said.
    fn build_job(&mut self) -> Option<SpeechJob> {
        if self.is_busy() {
            return None;
        }
        if self.current_text().trim().is_empty() {
            let msg = match self.source {
                _ if self.tab == Tab::Video => t("video.nothing"),
                Source::File => t("read.nothing"),
                Source::Paste => t("read.nothing_pasted"),
            };
            self.announce(msg);
            return None;
        }
        let text = crate::spoken::prepare(self.current_text(), &self.wordlists, &self.settings.disabled_wordlists);
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

    pub(super) fn read_aloud(&mut self) {
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
        if self.player.control.is_some() {
            self.announce(t("read.audio_playing"));
            return;
        }
        // The video pauses, so its description can be heard and then the
        // video carried on from the same place.
        if let Some(control) = &self.video.control
            && !self.video.paused
        {
            self.video.paused = true;
            control.set_paused(true);
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

    pub(super) fn toggle_pause(&mut self) {
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

    pub(super) fn stop(&mut self) {
        if let Some((_, control)) = &self.job {
            control.stop();
            self.announce(t("read.stopping"));
        }
    }

    pub(super) fn save_audio(&mut self) {
        if self.is_busy() {
            return;
        }
        let format = self.settings.audio_format;
        let on_video = self.tab == Tab::Video;
        let file = match self.source {
            _ if on_video => self.video.file.as_ref(),
            Source::File => self.file.as_ref(),
            Source::Paste => None,
        };
        // The document's name, which the saved file is named after and which
        // its tags give as the album.
        let album = file
            .and_then(|f| f.file_stem())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = if album.is_empty() { t("save.default_name") } else { album.clone() };
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
        // A saved photo or video description ends by saying how it was
        // made, since the listener cannot tell from the audio alone.
        let is_image = !on_video
            && self.source == Source::File
            && self
                .file
                .as_deref()
                .and_then(crate::document::FileKind::from_path)
                .is_some_and(|k| k == crate::document::FileKind::Image);
        if is_image {
            job.text = format!("{}\n\n{}", job.text.trim_end(), job.provider.image_description_note());
        } else if on_video {
            job.text = format!("{}\n\n{}", job.text.trim_end(), job.provider.video_description_note());
        }
        let tags = crate::audio::Tags {
            title: path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
            // The default system voice has no name of its own.
            artist: self
                .current_voice(job.provider)
                .filter(|v| !v.id.is_empty())
                .map_or_else(|| job.provider.name(), |v| v.name.clone()),
            album,
        };
        let control = Arc::new(Control::default());
        self.job = Some((JobKind::Saving, control.clone()));
        self.progress = 0.0;
        let usage = Self::usage_note(&job, false);
        self.announce(tf("save.preparing", &[("name", &file_name(&path)), ("usage", &usage)]));
        self.rep.spawn(move |rep| worker::save(rep, control, job, path, format, tags));
    }

    pub(super) fn general_tab(&mut self, ui: &mut Ui) {
        heading(ui, &t("general.heading"));

        let labels: Vec<String> = Source::ALL.iter().map(|s| s.label()).collect();
        let mut sindex = Source::ALL.iter().position(|s| *s == self.source).unwrap_or(0);
        if dropdown(ui, "source", &t("general.source"), &labels, &mut sindex, !self.is_busy()) {
            self.source = Source::ALL[sindex];
            self.announce(match self.source {
                Source::File => t("general.source_chosen_file"),
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

                let label = self.keyed("general.choose_file", Action::Open);
                if full_button(ui, &label, !self.loading_file && !self.preparing_video()).clicked() {
                    self.choose_file();
                }

                let reading_options = vec![t("settings.reading_content"), t("settings.reading_format")];
                let mut rindex = usize::from(self.settings.document_reading == DocumentReading::Format);
                if dropdown(ui, "reading", &t("settings.reading"), &reading_options, &mut rindex, true) {
                    self.settings.document_reading =
                        if rindex == 1 { DocumentReading::Format } else { DocumentReading::Content };
                    self.settings.save();
                    self.reread_document();
                }
            }
            Source::Paste => {
                let enabled = !self.is_busy();
                let resp = paste_box(ui, &t("general.pasted"), &mut self.pasted, enabled);
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
        let locked = self.system_voices_only();
        if dropdown(ui, "provider", &t("general.service"), &labels, &mut index, !self.is_busy() && !locked) {
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
        if locked {
            ui.label(t("general.service_locked"));
        }

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
        if full_button(ui, &self.keyed("general.read_aloud", Action::Read), has_text && !self.is_busy()).clicked() {
            self.read_aloud();
        }
        let pause_label = self.keyed(if self.paused { "general.resume" } else { "general.pause" }, Action::Pause);
        if full_button(ui, &pause_label, speaking).clicked() {
            self.toggle_pause();
        }
        let stop_label = match self.job {
            Some((JobKind::Saving, _)) => self.keyed("general.cancel_saving", Action::Stop),
            Some((JobKind::Downloading, _)) => self.keyed("general.stop_downloading", Action::Stop),
            Some((JobKind::Translating, _)) => self.keyed("general.stop_translating", Action::Stop),
            Some((JobKind::Transcribing, _)) => self.stop_transcribing_label(),
            Some((JobKind::DescribingVideo, _)) => self.keyed("video.stop", Action::Stop),
            _ => self.keyed("general.stop", Action::Stop),
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
        if full_button(ui, &self.keyed("general.save", Action::Save), has_text && !self.is_busy()).clicked() {
            self.save_audio();
        }
    }
}
