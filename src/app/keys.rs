//! Keyboard shortcuts: taking them before egui sees them, and doing what they
//! ask.

use super::*;

impl SpeechApp {
    /// Something can be stopped: reading, saving, a download, a translation,
    /// a transcription, or an audio file opening or playing.
    fn can_stop(&self) -> bool {
        self.player.control.is_some() || self.player.loader.is_some() || self.video.control.is_some() || self.is_busy()
    }

    /// Takes shortcut key presses out of the input before egui sees it, so
    /// egui's own handling (Escape dropping focus, arrows moving focus)
    /// doesn't also happen. The commands run in `run_shortcuts`.
    pub(super) fn take_shortcuts(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        let focused = ctx.memory(|m| m.focused());
        let cx = KeyContext {
            // Read from memory: this runs between frames, when egui's
            // per-frame record of open popups isn't kept.
            popup_open: egui::Popup::is_any_open(ctx),
            focused_tab: focused.and_then(|id| self.tab_buttons.iter().position(|b| *b == id)),
            tab_count: Tab::ALL.len(),
        };
        let can_stop = self.can_stop();
        let ready = |a: Action| !matches!(a, Action::Stop | Action::StopSecond) || can_stop;
        for command in self.keymap.take(&mut raw_input.events, cx, ready) {
            match command {
                Command::CloseList => egui::Popup::close_all(ctx),
                _ => self.pending.push(command),
            }
        }
    }

    pub(super) fn run_shortcuts(&mut self) {
        for command in std::mem::take(&mut self.pending) {
            match command {
                Command::Tab(i) => {
                    if let Some(tab) = Tab::ALL.get(i) {
                        self.switch_tab(*tab);
                    }
                }
                Command::Run(action) => self.run_action(action),
                // Handled in `take_shortcuts`.
                Command::CloseList => {}
            }
        }
    }

    fn run_action(&mut self, action: Action) {
        // On the Audio Player tab, the usual keys work on the audio file.
        let on_player = self.tab == Tab::Player;
        // Pause, progress and stop work on the audio file wherever it plays.
        let audio_playing = self.player.control.is_some();
        // The video playing beside its description, unless something is
        // being read aloud. Other jobs, such as a download, leave the keys
        // to the video.
        let reading = matches!(self.job, Some((JobKind::Speaking | JobKind::Previewing, _)));
        let video_playing = self.video.control.is_some() && !reading;
        let count = Tab::ALL.len();
        let current = Tab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        match action {
            Action::Open => match self.tab {
                Tab::Player => self.choose_audio(),
                Tab::Video => self.choose_video(),
                _ => self.choose_file(),
            },
            Action::Save => {
                if on_player { self.save_transcript() } else { self.save_audio() }
            }
            Action::NextTab => self.switch_tab(Tab::ALL[(current + 1) % count]),
            Action::PreviousTab => self.switch_tab(Tab::ALL[(current + count - 1) % count]),
            // Handled as `Command::Tab`.
            Action::GoToTab => {}
            Action::Read | Action::ReadSecond => {
                if on_player { self.play_audio() } else { self.read_aloud() }
            }
            // Reading aloud pauses the video, so while something is read
            // aloud these keys work on that instead.
            Action::Pause => {
                if audio_playing {
                    self.toggle_audio_pause()
                } else if video_playing {
                    self.toggle_video_pause()
                } else {
                    self.toggle_pause()
                }
            }
            Action::Progress => {
                if audio_playing {
                    self.announce_audio_position()
                } else if video_playing {
                    self.announce_video_position()
                } else {
                    self.announce_progress()
                }
            }
            Action::Stop | Action::StopSecond => {
                // Stopping also stops an audio file being opened.
                if audio_playing || self.player.loader.is_some() {
                    self.stop_audio()
                } else if video_playing {
                    self.stop_video()
                } else {
                    self.stop()
                }
            }
            Action::Help => self.show_shortcuts(),
        }
    }

    /// `key`'s text with " (F5)" or similar filled in for `{shortcut}`, or
    /// nothing when `action` has no shortcut.
    pub(super) fn keyed(&self, key: &str, action: Action) -> String {
        tf(key, &[("shortcut", &self.keymap.hint(action))])
    }

    /// Lists the shortcuts in a dialog, which screen readers read out when
    /// it opens.
    pub(super) fn show_shortcuts(&mut self) {
        rfd::MessageDialog::new()
            .set_title(t("shortcuts.dialog_title"))
            .set_description(self.keymap.reference())
            .set_buttons(rfd::MessageButtons::Ok)
            .set_level(rfd::MessageLevel::Info)
            .show();
    }

    pub(super) fn set_shortcuts(&mut self) {
        self.keymap = Keymap::new(self.settings.shortcuts_enabled(), &self.settings.shortcuts, Platform::CURRENT);
        i18n::set_hints(self.keymap.hints());
        self.settings.save();
    }
}
