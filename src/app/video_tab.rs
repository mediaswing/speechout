//! The Describe Video tab: describing a video's pictures, and playing the video
//! while the description is edited.

use super::*;

/// What the Describe Video tab is showing and doing.
#[derive(Default)]
pub(super) struct VideoTab {
    /// The video on the Describe Video tab.
    pub(super) file: Option<PathBuf>,
    /// Where the pictures taken from the video are kept while it is
    /// described. Deleted when dropped.
    frames: Option<tempfile::TempDir>,
    /// The picture being described, and what screen readers call it.
    pub(super) preview: Option<(egui::TextureHandle, String)>,
    /// A picture that arrived since the last frame, to make into a texture.
    pub(super) pending_preview: Option<(egui::ColorImage, String)>,
    /// The description of the video.
    pub(super) text: String,
    /// The description is being edited, with the video beside it.
    pub(super) editing: bool,
    /// The description box takes the keyboard focus on the next frame.
    focus_text: bool,
    /// Controls the video while it is playing or paused.
    pub(super) control: Option<Arc<Control>>,
    /// Numbers each play of the video, so messages from one that has been
    /// left behind, such as when another video is chosen, are ignored.
    play: u64,
    pub(super) paused: bool,
    position: Duration,
    /// How long the video is, once it has been played.
    length: Option<Duration>,
    /// The picture of the video now playing.
    pub(super) picture: Option<egui::TextureHandle>,
    /// A picture of the playing video that arrived since the last frame.
    pub(super) pending_picture: Option<egui::ColorImage>,
}

impl SpeechApp {
    /// Installing ffmpeg finished.
    pub(super) fn ffmpeg_ready(&mut self, result: Result<PathBuf, String>) {
        self.setting_up_ffmpeg = false;
        let waiting = self.waiting_for_ffmpeg.take();
        match result {
            Ok(_) => match waiting {
                Some(Waiting::Video(path)) => self.describe_video(path, t("ffmpeg.installed")),
                Some(Waiting::Audio(path)) => self.open_audio(path, t("ffmpeg.installed")),
                _ => self.announce(t("ffmpeg.installed")),
            },
            Err(e) => self.show_error(tf("ffmpeg.setup_failed", &[("error", &e)])),
        }
    }

    /// A picture from the video being described is about to be described.
    pub(super) fn video_frame_arrived(&mut self, frame: worker::VideoFrame) {
        // Only the video still chosen; one left behind was told to stop.
        if self.video.file.as_ref() == Some(&frame.video)
            && let Some(image) = frame.image
        {
            let label = tf(
                "video.preview",
                &[
                    ("number", &frame.number),
                    ("count", &frame.count),
                    ("time", &spoken_duration(frame.time)),
                ],
            );
            self.video.pending_preview = Some((image, label));
        }
    }

    /// How long the video being played is.
    pub(super) fn video_length_found(&mut self, play: u64, length: Duration) {
        if play == self.video.play {
            self.video.length = Some(length);
        }
    }

    /// How far into the video playback is, with the picture to show then if there is a new one.
    pub(super) fn video_playing(&mut self, play: u64, position: Duration, picture: Option<egui::ColorImage>) {
        if self.video.control.is_some() && play == self.video.play {
            self.video.position = position;
            if picture.is_some() {
                self.video.pending_picture = picture;
            }
        }
    }

    /// A video is waiting for Ollama to be checked or set up, or for ffmpeg
    /// to be installed.
    pub(super) fn preparing_video(&self) -> bool {
        self.setting_up_ffmpeg || matches!(self.waiting, Some(Waiting::Video(_)))
    }

    pub(super) fn choose_video(&mut self) {
        if self.is_busy() || self.waiting.is_some() || self.setting_up_ffmpeg {
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title(t("video.dialog_title"))
            .add_filter(t("video.filter"), &crate::video::EXTENSIONS)
            .pick_file()
        else {
            return;
        };
        // Whatever the old video still sends is ignored from now on.
        self.stop_video();
        self.video.control = None;
        self.video.paused = false;
        self.video.file = Some(path.clone());
        self.video.text.clear();
        self.video.preview = None;
        self.video.pending_preview = None;
        self.video.editing = false;
        self.video.position = Duration::ZERO;
        self.video.length = None;
        self.video.picture = None;
        self.video.pending_picture = None;
        self.describe_video(path, String::new());
    }

    /// Describes the video's keyframes, first offering to install ffmpeg or
    /// to set up Ollama if they are missing. `intro` is said first.
    pub(super) fn describe_video(&mut self, path: PathBuf, intro: String) {
        if self.is_busy() {
            return self.announce(format!("{intro} {}", t("video.busy")).trim().to_owned());
        }
        let Some(ffmpeg) = crate::platform::ffmpeg() else {
            if !intro.is_empty() {
                self.announce(intro);
            }
            return self.offer_ffmpeg(Waiting::Video(path));
        };
        if self.needs_model_check() {
            return self.wait_for_model(Waiting::Video(path));
        }
        let frames = match tempfile::Builder::new().prefix("speechout-video-").tempdir() {
            Ok(dir) => dir,
            Err(e) => return self.show_error(tf("video.failed", &[("error", &e)])),
        };
        let dir = frames.path().to_owned();
        self.video.frames = Some(frames);
        let control = Arc::new(Control::default());
        self.job = Some((JobKind::DescribingVideo, control.clone()));
        self.progress = 0.0;
        let model = self.settings.vision_model.clone();
        let started = tf("video.started", &[("name", &file_name(&path)), ("model", &model)]);
        self.announce(format!("{intro} {started}").trim().to_owned());
        self.rep.spawn(move |rep| worker::describe_video(rep, control, ffmpeg, path, dir, model));
    }

    /// ffmpeg isn't installed. Offers to install it with the package
    /// manager, or to open its download page, then describes the video or
    /// opens the audio file that is `waiting`.
    pub(super) fn offer_ffmpeg(&mut self, waiting: Waiting) {
        let audio = matches!(waiting, Waiting::Audio(_));
        let key = |video: &'static str, audio_key: &'static str| if audio { audio_key } else { video };
        let name = file_name(waiting.path());
        let manager = crate::platform::ffmpeg_package_manager();
        let mut question = match manager {
            Some(manager) => tf("ffmpeg.install_question", &[("manager", &manager)]),
            None => t("ffmpeg.download_question"),
        };
        if manager.is_some() && cfg!(target_os = "linux") {
            question = format!("{question} {}", t("ollama.password"));
        }
        let yes = rfd::MessageDialog::new()
            .set_title(t("ffmpeg.install_title"))
            .set_description(format!("{}\n\n{question}", t(key("ffmpeg.intro", "ffmpeg.intro_audio"))))
            .set_buttons(rfd::MessageButtons::YesNo)
            .set_level(rfd::MessageLevel::Info)
            .show()
            == rfd::MessageDialogResult::Yes;
        if !yes {
            self.announce(tf(key("ffmpeg.declined", "ffmpeg.declined_audio"), &[("name", &name)]));
            return;
        }
        let Some(manager) = manager else {
            const DOWNLOAD_PAGE: &str = "https://ffmpeg.org/download.html";
            match crate::platform::open_url(DOWNLOAD_PAGE) {
                Ok(()) => self.announce(t(key("ffmpeg.page_opened", "ffmpeg.page_opened_audio"))),
                Err(e) => self.show_error(tf("ffmpeg.page_failed", &[("error", &e), ("url", &DOWNLOAD_PAGE)])),
            }
            return;
        };
        self.setting_up_ffmpeg = true;
        self.waiting_for_ffmpeg = Some(waiting);
        self.announce(tf("ffmpeg.installing", &[("manager", &manager)]));
        self.rep.spawn(worker::install_ffmpeg);
    }

    pub(super) fn video_described(&mut self, path: PathBuf, result: Result<Option<worker::VideoDescription>, String>) {
        self.job = None;
        // The pictures have been described, and the preview has its own copy.
        self.video.frames = None;
        let name = file_name(&path);
        match result {
            Ok(None) => self.announce(t("video.stopped")),
            Ok(Some(done)) => {
                let words = done.text.split_whitespace().count();
                let switched = self.switch_vision_model(done.model);
                self.video.text = done.text;
                // Known before the first play, so Forward can't go past the end.
                self.video.length = done.length;
                self.cue(Cue::Success);
                let edit = rfd::MessageDialog::new()
                    .set_title(t("video.edit_title"))
                    .set_description(tf("video.edit_question", &[("name", &name), ("words", &words)]))
                    .set_buttons(rfd::MessageButtons::YesNo)
                    .set_level(rfd::MessageLevel::Info)
                    .show()
                    == rfd::MessageDialogResult::Yes;
                if edit {
                    self.edit_video_text();
                } else {
                    let key = if done.frames == 1 { "video.done_one" } else { "video.done" };
                    let done = tf(key, &[("name", &name), ("count", &done.frames), ("words", &words)]);
                    self.announce(match switched {
                        Some(note) => format!("{done} {note}"),
                        None => done,
                    });
                }
            }
            Err(e) => self.show_error(tf("video.failed", &[("error", &e)])),
        }
    }

    /// Moves the picture over to the left with the video's controls under
    /// it, and puts the description beside it, ready to edit.
    fn edit_video_text(&mut self) {
        self.video.editing = true;
        self.video.focus_text = true;
        self.announce(t("video.editing"));
    }

    /// Puts the tab back as it was, keeping the changes to the description.
    fn finish_editing_video(&mut self) {
        self.stop_video();
        self.video.editing = false;
        let words = self.video.text.split_whitespace().count();
        self.announce(tf("video.edited", &[("words", &words)]));
    }

    /// Plays the video from where it was left, or carries on if paused.
    fn play_video(&mut self) {
        if self.video.control.is_some() {
            if self.video.paused {
                self.toggle_video_pause();
            }
            return;
        }
        let Some(path) = self.video.file.clone() else { return };
        if matches!(self.job, Some((JobKind::Speaking | JobKind::Previewing, _))) {
            return self.announce(t("video.reading_aloud"));
        }
        if self.player.control.is_some() {
            return self.announce(t("video.audio_playing"));
        }
        let Some(ffmpeg) = crate::platform::ffmpeg() else {
            return self.show_error(tf("video.play_failed", &[("error", &t("video.no_ffmpeg"))]));
        };
        // Start again from the beginning if the last play reached the end.
        if self.video.length.is_some_and(|length| self.video.position + Duration::from_millis(500) >= length) {
            self.video.position = Duration::ZERO;
        }
        self.cues.stop();
        let start = self.video.position;
        let control = Arc::new(Control::default());
        self.video.control = Some(control.clone());
        self.video.play += 1;
        let play = self.video.play;
        let length = self.video.length;
        self.video.paused = false;
        self.announce(t("video.playing"));
        self.rep.spawn(move |rep| worker::play_video(rep, control, play, ffmpeg, path, start, length));
    }

    pub(super) fn toggle_video_pause(&mut self) {
        let Some(control) = &self.video.control else { return };
        self.video.paused = !self.video.paused;
        control.set_paused(self.video.paused);
        let msg = if self.video.paused {
            tf("player.paused", &[("position", &spoken_duration(self.video.position))])
        } else {
            t("read.resumed")
        };
        self.announce(msg);
    }

    pub(super) fn stop_video(&mut self) {
        if let Some(control) = &self.video.control {
            control.stop();
        }
    }

    /// Moves `seconds` forward (or back, if negative) in the video.
    fn skip_video(&mut self, seconds: f64) {
        let end = self.video.length.map_or(f64::MAX, |l| l.as_secs_f64());
        let to = (self.video.position.as_secs_f64() + seconds).clamp(0.0, end);
        self.video.position = Duration::from_secs_f64(to);
        if let Some(control) = &self.video.control {
            control.seek(self.video.position);
        }
        self.announce_video_position();
    }

    /// Says where in the video playback is (F7 while it plays).
    pub(super) fn announce_video_position(&mut self) {
        let msg = self.video_place(self.video.paused);
        self.announce(msg);
    }

    /// Where in the video playback is, and how long it is if known, said as
    /// paused there if `paused`.
    fn video_place(&self, paused: bool) -> String {
        let position = spoken_duration(self.video.position);
        match (self.video.length, paused) {
            (Some(length), false) => tf("player.position", &[("position", &position), ("length", &spoken_duration(length))]),
            (Some(length), true) => {
                tf("player.position_paused", &[("position", &position), ("length", &spoken_duration(length))])
            }
            (None, _) => tf("video.position", &[("position", &position)]),
        }
    }

    /// Playing the video ended: true if it reached the end. A play left
    /// behind, such as when another video was chosen, is ignored.
    pub(super) fn video_play_ended(&mut self, play: u64, result: Result<bool, String>) {
        if play != self.video.play {
            return;
        }
        self.video.control = None;
        self.video.paused = false;
        // Stopped, it plays on from the same place next time.
        if result == Ok(true) {
            self.video.position = Duration::ZERO;
        }
        match result {
            Ok(true) => self.announce(t("video.play_finished")),
            Ok(false) => self.announce(t("video.play_stopped")),
            Err(e) => self.show_error(tf("video.play_failed", &[("error", &e)])),
        }
    }

    /// The file picker, with the picture being described under it and the
    /// progress bar under that, then buttons for the description.
    pub(super) fn video_tab(&mut self, ui: &mut Ui) {
        heading(ui, &t("video.heading"));
        let describing = matches!(self.job, Some((JobKind::DescribingVideo, _)));

        let mut file_text = match &self.video.file {
            Some(p) => p.display().to_string(),
            None => t("general.no_file"),
        };
        text_field(ui, &t("general.current_file"), &mut file_text, false);
        let label = self.keyed("video.choose", Action::Open);
        let can_choose = !self.is_busy() && self.waiting.is_none() && !self.setting_up_ffmpeg;
        if full_button(ui, &label, can_choose).clicked() {
            self.choose_video();
        }

        if self.video.editing {
            self.video_editor(ui);
        } else {
            let none = if describing { t("video.preview_waiting") } else { t("video.preview_none") };
            video_preview(ui, self.video.preview.as_ref(), &none);
        }
        self.progress_bar(ui);

        let has_text = !self.video.text.is_empty();
        if self.video.editing {
            if full_button(ui, &t("video.finish_editing"), true).clicked() {
                self.finish_editing_video();
            }
        } else if full_button(ui, &t("video.edit"), has_text && !describing).clicked() {
            self.edit_video_text();
        }
        if full_button(ui, &self.keyed("general.read_aloud", Action::Read), has_text && !self.is_busy()).clicked() {
            self.read_aloud();
        }
        let stop_label = match self.job {
            Some((JobKind::DescribingVideo, _)) => self.keyed("video.stop", Action::Stop),
            _ => self.keyed("general.stop", Action::Stop),
        };
        if full_button(ui, &stop_label, self.is_busy()).clicked() {
            self.stop();
        }
        let save_label = self.keyed("general.save", Action::Save);
        let can_save = has_text && !self.is_busy();
        let (mut save, mut copy) = (false, false);
        ui.columns(2, |cols| {
            save = full_button(&mut cols[0], &save_label, can_save).clicked();
            copy = full_button(&mut cols[1], &t("video.copy"), has_text).clicked();
        });
        if save {
            self.save_audio();
        }
        if copy {
            let ctx = ui.ctx().clone();
            self.copy_text(&ctx);
        }
    }

    /// The video on the left, with its controls under it, and the
    /// description on the right to edit. Read aloud, Save and Copy use the
    /// edited description.
    fn video_editor(&mut self, ui: &mut Ui) {
        let place = self.video_place(false);
        // Until it has been played, the last picture described is shown.
        let picture = match (&self.video.picture, &self.video.preview) {
            (Some(texture), _) => Some((texture.clone(), tf("video.picture", &[("place", &place)]))),
            (None, preview) => preview.clone(),
        };
        let playing = self.video.control.is_some();
        let speaking = matches!(self.job, Some((JobKind::Speaking | JobKind::Previewing, _)));
        let play_label = t("video.play");
        let pause_label = self.keyed(if self.video.paused { "general.resume" } else { "general.pause" }, Action::Pause);
        let stop_label = self.keyed("video.stop_playing", Action::Stop);
        let (mut play, mut pause, mut stop, mut back, mut forward) = (false, false, false, false, false);
        let focus = std::mem::take(&mut self.video.focus_text);
        let text = &mut self.video.text;
        ui.columns(2, |cols| {
            let left = &mut cols[0];
            video_preview(left, picture.as_ref(), &t("video.preview_none"));
            left.label(&place);
            play = full_button(left, &play_label, !playing && !speaking).clicked();
            pause = full_button(left, &pause_label, playing).clicked();
            stop = full_button(left, &stop_label, playing).clicked();
            back = full_button(left, &t("player.back"), true).clicked();
            forward = full_button(left, &t("player.forward"), true).clicked();

            let right = &mut cols[1];
            let label = right.label(t("video.text_label"));
            let edit = egui::TextEdit::multiline(text).desired_width(f32::INFINITY).desired_rows(18);
            let resp = right.add(edit).labelled_by(label.id);
            if focus {
                resp.request_focus();
            }
        });
        if play {
            self.play_video();
        }
        if pause {
            self.toggle_video_pause();
        }
        if stop {
            self.stop_video();
        }
        if back {
            self.skip_video(-10.0);
        }
        if forward {
            self.skip_video(10.0);
        }
    }
}
