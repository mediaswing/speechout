//! The Audio Player tab: audio files and playlists, playing them, and
//! transcribing what is said in them.

use super::*;

/// A zip file of audio open on the Audio Player tab.
pub(super) struct OpenPlaylist {
    zip: PathBuf,
    list: Playlist,
    /// The track chosen, counting from 0.
    current: usize,
    /// The first track has been opened, and asked to download the models for
    /// transcribing if they were missing. Later tracks don't ask again.
    offered_models: bool,
}

/// What the Audio Player tab is showing and doing.
#[derive(Default)]
pub(super) struct PlayerTab {
    /// The file on the Audio Player tab.
    audio: Option<(PathBuf, Arc<Listened>)>,
    /// Stops the audio file being opened.
    pub(super) loader: Option<Arc<Control>>,
    transcript: String,
    /// The transcript is shown only once the user asks for it.
    show_transcript: bool,
    /// Controls the audio file while it is playing or paused.
    pub(super) control: Option<Arc<Control>>,
    pub(super) paused: bool,
    position: Duration,
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
}

impl SpeechApp {
    /// How far into the audio file playback is.
    pub(super) fn audio_position_changed(&mut self, position: Duration) {
        if self.player.control.is_some() {
            self.player.position = position;
        }
    }

    /// Playing the audio file ended: true if it reached the end.
    pub(super) fn audio_ended(&mut self, result: Result<bool, String>) {
        self.player.control = None;
        self.player.paused = false;
        self.player.position = Duration::ZERO;
        match result {
            // Stopped because another file or track was chosen:
            // that is being announced instead.
            Ok(_) if self.player.loader.is_some() => {}
            Ok(true) => match &self.player.playlist {
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

    /// Downloading a Whisper model finished: true if complete, false if stopped.
    pub(super) fn whisper_downloaded(&mut self, model: WhisperModel, result: Result<bool, String>) {
        self.job = None;
        let transcribe = std::mem::take(&mut self.player.transcribe_after_download);
        match result {
            Ok(true) => {
                let done = tf("whisper.downloaded", &[("model", &model.name())]);
                if transcribe && self.player.audio.is_some() {
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

    /// Downloading the speaker models finished: true if complete, false if stopped.
    pub(super) fn speaker_models_downloaded(&mut self, result: Result<bool, String>) {
        self.job = None;
        let transcribe = std::mem::take(&mut self.player.transcribe_after_download);
        match result {
            Ok(true) => {
                let done = t("speakers.downloaded");
                if transcribe && self.player.audio.is_some() {
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

    pub(super) fn choose_audio(&mut self) {
        if self.player.loader.is_some() {
            return;
        }
        if matches!(self.job, Some((JobKind::Transcribing, _))) {
            self.announce(t("player.wait_transcribing"));
            return;
        }
        let Some(path) = rfd::FileDialog::new()
            .set_title(t("player.dialog_title"))
            .add_filter(t("player.filter"), &["wav", "mp3", "wma", "zip"])
            .pick_file()
        else {
            return;
        };
        self.open_audio(path, String::new());
    }

    /// Opens an audio file or a zip file of them, first offering to install
    /// ffmpeg for a WMA file if it is missing. `intro` is said first.
    pub(super) fn open_audio(&mut self, path: PathBuf, intro: String) {
        if self.player.loader.is_some() {
            return;
        }
        if self.setting_up_ffmpeg {
            return self.announce(t("progress.setting_up_ffmpeg"));
        }
        if crate::audio::needs_ffmpeg(&path) && crate::platform::ffmpeg().is_none() {
            if !intro.is_empty() {
                self.announce(intro);
            }
            return self.offer_ffmpeg(Waiting::Audio(path));
        }
        if let Some(control) = &self.player.control {
            control.stop();
        }
        let control = Arc::new(Control::default());
        self.player.loader = Some(control.clone());
        self.player.play_when_loaded = false;
        let name = file_name(&path);
        if crate::playlist::is_zip(&path) {
            self.announce(format!("{intro} {}", tf("playlist.unpacking", &[("name", &name)])).trim().to_owned());
            self.rep.spawn(move |rep| worker::open_playlist(rep, control, path));
        } else {
            self.announce(format!("{intro} {}", tf("player.opening", &[("name", &name)])).trim().to_owned());
            self.rep.spawn(move |rep| worker::load_audio(rep, control, path));
        }
    }

    pub(super) fn playlist_opened(&mut self, zip: PathBuf, result: Result<Option<Playlist>, String>) {
        self.player.loader = None;
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
            log::warn!("listing.txt names files that are not WAV, MP3 or WMA files in the zip: {:?}", list.missing);
        }
        self.player.audio = None;
        self.player.position = Duration::ZERO;
        self.player.transcript.clear();
        self.player.show_transcript = false;
        self.player.playlist = Some(OpenPlaylist { zip, list, current: 0, offered_models: false });
        self.open_track(0, false, intro);
    }

    /// Opens track `index` of the playlist (counting from 0), stopping the
    /// track before it, and plays it once it has opened if `play` is true.
    /// `intro` is said first.
    fn open_track(&mut self, index: usize, play: bool, intro: String) {
        if self.player.loader.is_some() {
            return;
        }
        let Some(playlist) = &mut self.player.playlist else { return };
        let Some(path) = playlist.list.tracks.get(index).cloned() else { return };
        playlist.current = index;
        let count = playlist.list.tracks.len();
        if let Some(control) = &self.player.control {
            control.stop();
        }
        // Its transcript would be of the track before.
        if let Some((JobKind::Transcribing, control)) = &self.job {
            control.stop();
        }
        let control = Arc::new(Control::default());
        self.player.loader = Some(control.clone());
        self.player.play_when_loaded = play;
        let opening = tf("playlist.opening", &[("number", &(index + 1)), ("count", &count), ("name", &file_name(&path))]);
        self.announce(format!("{intro} {opening}").trim().to_owned());
        self.rep.spawn(move |rep| worker::load_audio(rep, control, path));
    }

    /// Moves to the next track of the playlist, or the one before. It plays
    /// if the track it replaces was playing.
    fn change_track(&mut self, forward: bool) {
        let Some(playlist) = &self.player.playlist else { return };
        let index = if forward { playlist.current + 1 } else { playlist.current.wrapping_sub(1) };
        if index >= playlist.list.tracks.len() {
            self.announce(if forward { t("playlist.at_last") } else { t("playlist.at_first") });
            return;
        }
        let play = self.player.control.is_some() && !self.player.paused;
        self.open_track(index, play, String::new());
    }

    pub(super) fn audio_loaded(&mut self, path: PathBuf, result: Result<Option<Arc<Listened>>, String>) {
        self.player.loader = None;
        let play = std::mem::take(&mut self.player.play_when_loaded);
        let name = file_name(&path);
        let audio = match result {
            Ok(Some(audio)) => audio,
            Ok(None) => return self.announce(tf("player.open_stopped", &[("name", &name)])),
            Err(e) => return self.show_error(tf("file.load_failed", &[("name", &name), ("error", &e)])),
        };
        // A file chosen on its own replaces the playlist.
        if self.player.playlist.as_ref().is_some_and(|p| !p.list.tracks.contains(&path)) {
            self.player.playlist = None;
        }
        self.player.position = Duration::ZERO;
        self.player.transcript.clear();
        self.player.show_transcript = false;
        self.player.audio = Some((path, audio.clone()));
        let playing = play && self.start_playing();
        let length = spoken_duration(audio.duration);
        let mut intro = match &self.player.playlist {
            Some(p) => {
                let key = if playing { "playlist.playing" } else { "playlist.loaded" };
                let count = p.list.tracks.len();
                tf(key, &[("number", &(p.current + 1)), ("count", &count), ("name", &name), ("length", &length)])
            }
            None => tf("player.loaded", &[("name", &name), ("length", &length)]),
        };
        if audio.sound == SoundKind::Speech && !audio.made_here {
            if audio.cut_short {
                intro = format!("{intro} {}", t("player.cut_short"));
            }
            // In a playlist, only the first track offers to download the
            // models for transcribing, so the questions don't keep coming up.
            let may_ask = match &mut self.player.playlist {
                Some(p) => !std::mem::replace(&mut p.offered_models, true),
                None => true,
            };
            if may_ask || self.transcribing_models_ready() {
                let intro = format!("{intro} {}", t("player.speech"));
                // The track before is still being transcribed, and has been
                // told to stop.
                if matches!(self.job, Some((JobKind::Transcribing, _))) {
                    self.player.transcribe_when_free = true;
                    return self.announce(intro);
                }
                return self.transcribe_audio(intro);
            }
        }
        let note = match audio.sound {
            SoundKind::Silent => t("player.silent"),
            SoundKind::Speech if audio.made_here => tf("player.made_here", &[("button", &t("player.transcribe"))]),
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
    pub(super) fn play_audio(&mut self) {
        if self.player.control.is_some() {
            if self.player.paused {
                self.toggle_audio_pause();
            }
            return;
        }
        if self.player.audio.is_none() {
            // A playlist whose track didn't open, such as when Escape was
            // pressed while it was opening.
            if let Some(p) = &self.player.playlist {
                return self.open_track(p.current, true, String::new());
            }
            self.announce(t("player.nothing"));
            return;
        }
        if self.start_playing() {
            self.announce(t("player.playing"));
        }
    }

    /// Starts the audio file playing from where it was left. Returns false,
    /// having said why, if it can't.
    fn start_playing(&mut self) -> bool {
        let Some((path, audio)) = &self.player.audio else { return false };
        if matches!(self.job, Some((JobKind::Speaking | JobKind::Previewing, _))) {
            self.announce(t("player.reading_aloud"));
            return false;
        }
        if self.video.control.is_some() {
            self.announce(t("player.video_playing"));
            return false;
        }
        // Start again from the beginning if the last play reached the end.
        let start = if self.player.position + Duration::from_millis(500) >= audio.duration {
            Duration::ZERO
        } else {
            self.player.position
        };
        self.cues.stop();
        let (path, audio) = (path.clone(), audio.clone());
        let control = Arc::new(Control::default());
        self.player.control = Some(control.clone());
        self.player.paused = false;
        self.player.position = start;
        self.rep.spawn(move |rep| worker::play_audio(rep, control, path, audio, start));
        true
    }

    pub(super) fn toggle_audio_pause(&mut self) {
        let Some(control) = &self.player.control else { return };
        self.player.paused = !self.player.paused;
        control.set_paused(self.player.paused);
        if !self.player.paused {
            self.cues.stop();
        }
        let msg = if self.player.paused {
            tf("player.paused", &[("position", &spoken_duration(self.player.position))])
        } else {
            t("read.resumed")
        };
        self.announce(msg);
    }

    /// Stops the audio file playing, or being opened.
    pub(super) fn stop_audio(&mut self) {
        for control in [&self.player.control, &self.player.loader].into_iter().flatten() {
            control.stop();
        }
    }

    /// Moves `seconds` forward (or back, if negative) in the audio file.
    fn skip_audio(&mut self, seconds: f64) {
        let Some((_, audio)) = &self.player.audio else { return };
        let to = (self.player.position.as_secs_f64() + seconds).clamp(0.0, audio.duration.as_secs_f64());
        self.player.position = Duration::from_secs_f64(to);
        if let Some(control) = &self.player.control {
            control.seek(self.player.position);
        }
        self.announce_audio_position();
    }

    /// Says where in the audio file playback is (F7 while it plays).
    pub(super) fn announce_audio_position(&mut self) {
        let Some((_, audio)) = &self.player.audio else { return };
        let length = spoken_duration(audio.duration);
        let position = spoken_duration(self.player.position);
        let key = if self.player.paused { "player.position_paused" } else { "player.position" };
        self.announce(tf(key, &[("position", &position), ("length", &length)]));
    }

    /// Transcribes the audio file with Whisper, labelling the speakers if
    /// that is chosen, first offering to download the models that aren't on
    /// the computer yet. `intro` is said first.
    fn transcribe_audio(&mut self, intro: String) {
        self.transcribe_audio_labelled(intro, self.settings.speaker_labels);
    }

    fn transcribe_audio_labelled(&mut self, intro: String, labels: SpeakerLabels) {
        let Some((path, audio)) = self.player.audio.clone() else { return };
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
        self.player.transcript.clear();
        self.player.show_transcript = false;
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
        self.player.transcribe_after_download = true;
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
        self.player.transcribe_after_download = true;
        self.announce(tf("whisper.downloading", &[("model", &model.name()), ("size", &size)]));
        self.rep.spawn(move |rep| worker::download_whisper(rep, control, model));
    }

    pub(super) fn transcribed(&mut self, path: PathBuf, result: Result<Option<worker::Transcript>, String>) {
        self.job = None;
        // A track of a playlist that has since been left, which was told to
        // stop. The track now chosen may be waiting its turn.
        let current = self.player.loader.is_none() && self.player.audio.as_ref().is_some_and(|(p, _)| *p == path);
        if self.player.playlist.is_some() && !current {
            if let Err(e) = result {
                log::warn!("transcribing the track before failed: {e}");
            }
            if std::mem::take(&mut self.player.transcribe_when_free) {
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
                if self.player.audio.as_ref().is_some_and(|(p, _)| *p == path) {
                    self.player.transcript = done.text;
                    let mut msg =
                        tf("transcript.done", &[("name", &name), ("count", &done.words)]);
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
        if self.player.transcript.is_empty() {
            self.announce(t("copy.nothing"));
            return;
        }
        ctx.copy_text(self.player.transcript.clone());
        let words = self.player.transcript.split_whitespace().count();
        self.announce(tf("copy.done", &[("count", &words)]));
    }

    pub(super) fn save_transcript(&mut self) {
        if self.player.transcript.is_empty() {
            self.announce(t("transcript.nothing"));
            return;
        }
        let stem = self
            .player
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
        match std::fs::write(&path, &self.player.transcript) {
            Ok(()) => {
                self.announce(tf("transcript.saved", &[("name", &name)]));
                self.cue(Cue::Success);
            }
            Err(e) => self.show_error(tf("transcript.save_failed", &[("name", &name), ("error", &e)])),
        }
    }

    pub(super) fn player_tab(&mut self, ui: &mut Ui) {
        heading(ui, &t("player.heading"));
        let transcribing = matches!(self.job, Some((JobKind::Transcribing, _)));

        let mut file_text = match (&self.player.audio, &self.player.playlist, self.player.loader.is_some()) {
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
        let label = self.keyed("player.choose", Action::Open);
        if full_button(ui, &label, self.player.loader.is_none() && !transcribing).clicked() {
            self.choose_audio();
        }
        if let Some(p) = &self.player.playlist {
            let tracks: Vec<String> = p
                .list
                .tracks
                .iter()
                .enumerate()
                .map(|(i, track)| tf("playlist.track", &[("number", &(i + 1)), ("name", &file_name(track))]))
                .collect();
            let current = p.current;
            let mut index = current;
            if dropdown(ui, "track", &t("playlist.label"), &tracks, &mut index, self.player.loader.is_none())
                && index != current
            {
                let play = self.player.control.is_some() && !self.player.paused;
                self.open_track(index, play, String::new());
            }
        }

        if let Some((_, audio)) = &self.player.audio {
            let sound = match audio.sound {
                SoundKind::Speech => t("player.kind_speech"),
                SoundKind::Other => t("player.kind_other"),
                SoundKind::Silent => t("player.kind_silent"),
            };
            let remaining = spoken_duration(audio.duration.saturating_sub(self.player.position));
            let summary = tf("player.summary", &[("remaining", &remaining), ("sound", &sound)]);
            ui.label(&summary);
            waveform(ui, &audio.waveform, self.audio_fraction(), &tf("player.waveform", &[("summary", &summary)]));
        }

        let has_audio = self.player.audio.is_some();
        let playing = self.player.control.is_some();
        let speaking = matches!(self.job, Some((JobKind::Speaking | JobKind::Previewing, _)));
        let play_label = self.keyed("player.play", Action::Read);
        let pause_label = self.keyed(if self.player.paused { "general.resume" } else { "general.pause" }, Action::Pause);
        let stop_label = self.keyed("general.stop", Action::Stop);
        let (back, back_name) = short_and_full("player.back_short", "player.back");
        let (forward, forward_name) = short_and_full("player.forward_short", "player.forward");
        let (previous, previous_name) = short_and_full("playlist.previous_short", "playlist.previous");
        let (next, next_name) = short_and_full("playlist.next_short", "playlist.next");
        let mut controls = vec![
            (play_label.as_str(), play_label.as_str(), has_audio && !playing && !speaking),
            (pause_label.as_str(), pause_label.as_str(), playing),
            (stop_label.as_str(), stop_label.as_str(), playing || self.player.loader.is_some()),
            (back.as_str(), back_name.as_str(), has_audio),
            (forward.as_str(), forward_name.as_str(), has_audio),
        ];
        if self.player.playlist.is_some() {
            let can_change = self.player.loader.is_none();
            controls.push((previous.as_str(), previous_name.as_str(), can_change));
            controls.push((next.as_str(), next_name.as_str(), can_change));
        }
        let clicked = button_row(ui, &controls);
        if clicked[0] {
            self.play_audio();
        }
        if clicked[1] {
            self.toggle_audio_pause();
        }
        if clicked[2] {
            self.stop_audio();
        }
        if clicked[3] {
            self.skip_audio(-10.0);
        }
        if clicked[4] {
            self.skip_audio(10.0);
        }
        if clicked.get(5) == Some(&true) {
            self.change_track(false);
        }
        if clicked.get(6) == Some(&true) {
            self.change_track(true);
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
        let has_transcript = !self.player.transcript.is_empty();
        let ((transcribe, transcribe_name), can_transcribe) = if transcribing {
            let label = self.stop_transcribing_label();
            ((label.clone(), label), true)
        } else {
            (short_and_full("player.transcribe_short", "player.transcribe"), has_audio && !self.is_busy())
        };
        let (toggle, toggle_name) = if self.player.show_transcript {
            short_and_full("transcript.hide_short", "transcript.hide")
        } else {
            short_and_full("transcript.show_short", "transcript.show")
        };
        let (copy, copy_name) = short_and_full("transcript.copy_short", "transcript.copy");
        let save = self.keyed(short_key("transcript.save_short", "transcript.save"), Action::Save);
        let save_name = self.keyed("transcript.save", Action::Save);
        let clicked = button_row(
            ui,
            &[
                (transcribe.as_str(), transcribe_name.as_str(), can_transcribe),
                (toggle.as_str(), toggle_name.as_str(), has_transcript),
                (copy.as_str(), copy_name.as_str(), has_transcript),
                (save.as_str(), save_name.as_str(), has_transcript),
            ],
        );
        if clicked[0] {
            if transcribing {
                self.stop();
            } else {
                self.transcribe_audio(String::new());
            }
        }
        if clicked[1] {
            self.player.show_transcript = !self.player.show_transcript;
        }
        if clicked[2] {
            let ctx = ui.ctx().clone();
            self.copy_transcript(&ctx);
        }
        if clicked[3] {
            self.save_transcript();
        }
        if self.player.show_transcript && has_transcript {
            text_area(ui, &t("transcript.label"), &self.player.transcript);
        }
    }

    /// While the audio file plays, Escape stops it rather than transcribing,
    /// so the button doesn't offer Escape then.
    pub(super) fn stop_transcribing_label(&self) -> String {
        if self.player.control.is_some() { t("transcript.stop") } else { self.keyed("general.stop_transcribing", Action::Stop) }
    }

    /// How far through the audio file playback is, from 0.0 to 1.0.
    pub(super) fn audio_fraction(&self) -> f32 {
        match &self.player.audio {
            Some((_, audio)) if !audio.duration.is_zero() => {
                (self.player.position.as_secs_f32() / audio.duration.as_secs_f32()).min(1.0)
            }
            _ => 0.0,
        }
    }
}
