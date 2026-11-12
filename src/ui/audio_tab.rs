//! The Audio Player tab: choose an MP3, play it, and if it is mostly speech,
//! read what was said, with each voice named "Speaker 1", "Speaker 2" and so on.
//!
//! Nothing is installed or downloaded without asking first: whisper.cpp, if the
//! system has a way to install it, and its model, the first time either is
//! needed. Saying no is respected, if not without comment.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use egui::{Key, Ui};

use super::widgets::{RowButton, button_row, full_button, heading, path_field, progress_bar, text_area};
use super::{App, shortcuts};
use crate::transcribe::{Heard, clock, sarcastic, whisper};
use crate::worker::{self, Control, TranscribeJob, display_name};

/// The kinds of audio file the player takes.
pub(super) const AUDIO_EXTENSIONS: &[&str] = &["mp3", "wav"];

/// Everything the Audio Player tab keeps.
#[derive(Default)]
pub(super) struct AudioState {
    path_input: String,
    file: Option<PathBuf>,
    name: String,
    heard: Option<Arc<Heard>>,
    /// Which file being heard is the latest. Answers about others are ignored.
    hear_id: u64,
    hearing: bool,
    playing: Option<Arc<Control>>,
    /// Which press of Play is the latest.
    play_id: u64,
    pub(super) paused: bool,
    transcribing: Option<Arc<Control>>,
    /// Which transcription is the latest. Messages from others are ignored.
    transcribe_id: u64,
    stage: String,
    progress: f32,
    transcript: String,
    /// Where the model is, if not in the usual place. Only tests change it.
    pub(super) model: Option<PathBuf>,
}

impl AudioState {
    pub(super) fn busy(&self) -> bool {
        self.playing.is_some() || self.transcribing.is_some()
    }
}

#[derive(Clone, Copy)]
enum Action {
    Play,
    Pause,
    Stop,
    Transcribe,
}

pub(super) fn is_audio(path: &std::path::Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| AUDIO_EXTENSIONS.iter().any(|a| e.eq_ignore_ascii_case(a)))
}

impl App {
    pub(super) fn choose_audio(&mut self) {
        if let Some(path) = self.dialogs.pick_file("Choose an audio file to play", "MP3 or WAV audio", AUDIO_EXTENSIONS) {
            self.open_audio(path);
        }
    }

    pub(super) fn open_audio(&mut self, path: PathBuf) {
        let path = std::path::absolute(&path).unwrap_or(path);
        self.audio.path_input = path.display().to_string();
        if !is_audio(&path) {
            return self.set_error("This kind of file can't be played. Choose an MP3 or WAV file.");
        }
        // A new file means the old one's playing and transcript are finished with.
        for control in [self.audio.playing.take(), self.audio.transcribing.take()].into_iter().flatten() {
            control.request_stop();
        }
        let name = display_name(&path);
        self.audio.hear_id += 1;
        self.audio.paused = false;
        self.audio.heard = None;
        self.audio.hearing = true;
        self.audio.transcript.clear();
        self.audio.progress = 0.0;
        self.audio.stage.clear();
        self.audio.file = Some(path.clone());
        self.audio.name = name.clone();
        self.set_info(format!("Opened {name}. Listening to it, to tell whether it is speech."));
        worker::hear(&self.rep, self.audio.hear_id, path);
    }

    pub(super) fn audio_heard(&mut self, id: u64, result: Result<Arc<Heard>, String>) {
        if id != self.audio.hear_id {
            return;
        }
        self.audio.hearing = false;
        let name = self.audio.name.clone();
        let heard = match result {
            Ok(heard) => heard,
            Err(e) => return self.set_error(format!("{name}: {e}")),
        };
        let verdict = heard.verdict;
        let shortened = heard.shortened;
        self.audio.heard = Some(heard);
        if verdict.is_speech() {
            let note = if shortened { " Only its first two hours will be transcribed." } else { "" };
            self.set_info(format!("{name} sounds like speech, so it will be transcribed.{note}"));
            self.start_transcription();
        } else if verdict.sound_seconds == 0 {
            self.set_info(format!("{name} is silent, so there's nothing to transcribe."));
        } else {
            let percent = (verdict.speech_share * 100.0).round();
            self.set_info(format!(
                "{name} sounds mostly like music or other sounds, with only about {percent}% speech, so it won't be \
                 transcribed. Press Transcribe to try anyway."
            ));
        }
    }

    /// Asks before installing or downloading anything, then starts transcribing.
    fn start_transcription(&mut self) {
        if self.audio.transcribing.is_some() {
            return self.set_info("It is already being transcribed.");
        }
        let Some(heard) = self.audio.heard.clone() else {
            return match (&self.audio.file, self.audio.hearing) {
                (None, _) => self.set_error("Choose an audio file first."),
                (Some(_), true) => self.set_info(format!("Still listening to {}. Try again in a moment.", self.audio.name)),
                (Some(_), false) => self.set_error(sarcastic("the audio couldn't be read, so there's nothing to transcribe")),
            };
        };
        let program = whisper::find_program(self.settings.whisper_program.as_deref());
        let mut install = None;
        if program.is_none() {
            if let Some(chosen) = &self.settings.whisper_program {
                return self.set_error(sarcastic(&format!(
                    "the whisper.cpp program chosen on the Settings tab, {}, isn't there any more",
                    chosen.display()
                )));
            }
            let Some(installer) = crate::platform::whisper_installer() else {
                return self.set_error(sarcastic(crate::platform::WHISPER_HELP));
            };
            let question = format!(
                "Transcribing needs whisper.cpp, which isn't installed. Install it now by running \"{}\"? This \
                 installs a program on your computer and can take a few minutes.",
                installer.command
            );
            if !self.dialogs.confirm("Install whisper.cpp?", &question) {
                return self.set_error(sarcastic(
                    "you said no to installing whisper.cpp. Fair enough, but I can't transcribe by guessing",
                ));
            }
            install = Some(installer);
        }
        let model = self.audio.model.clone().unwrap_or_else(whisper::model_path);
        let download = !model.is_file();
        if download {
            let question = format!(
                "Transcribing needs a speech recognition model, which is downloaded once from Hugging Face: about \
                 {} MB, saved in {}. Your audio is never sent anywhere. Download it now?",
                whisper::MODEL_BYTES / 1_000_000,
                model.parent().map_or_else(String::new, |d| d.display().to_string())
            );
            if !self.dialogs.confirm("Download the speech recognition model?", &question) {
                return self.set_error(sarcastic(
                    "you said no to downloading the speech recognition model, and without it I'm just guessing",
                ));
            }
        }
        let control = Arc::new(Control::default());
        self.audio.transcribe_id += 1;
        self.audio.transcribing = Some(control.clone());
        self.audio.progress = 0.0;
        self.audio.transcript.clear();
        let job = TranscribeJob { heard, program, install, model, download };
        worker::transcribe(&self.rep, self.audio.transcribe_id, control, job);
    }

    /// Whether a message is from the transcription still under way.
    fn is_current_transcription(&self, id: u64) -> bool {
        id == self.audio.transcribe_id && self.audio.transcribing.is_some()
    }

    pub(super) fn transcription_stage(&mut self, id: u64, stage: String) {
        if self.is_current_transcription(id) {
            self.audio.stage = stage.clone();
            self.audio.progress = 0.0;
            self.set_info(stage);
        }
    }

    pub(super) fn transcription_progress(&mut self, id: u64, progress: f32) {
        if self.is_current_transcription(id) {
            self.audio.progress = progress.clamp(0.0, 1.0);
        }
    }

    pub(super) fn transcribed(&mut self, id: u64, result: Result<Option<String>, String>) {
        // An answer for a file that has since been replaced is no longer wanted,
        // and mustn't end the new file's transcription.
        if !self.is_current_transcription(id) {
            return;
        }
        self.audio.transcribing = None;
        self.audio.stage.clear();
        self.audio.progress = 0.0;
        match result {
            Ok(Some(text)) => {
                let speakers = (1..)
                    .take_while(|n| text.contains(&format!("Speaker {n} (")))
                    .last()
                    .unwrap_or(1);
                let people = if speakers == 1 { "1 speaker".to_owned() } else { format!("{speakers} speakers") };
                let words = text.split_whitespace().count();
                self.audio.transcript = text;
                self.set_info(format!("Transcribed {}: {words} words, {people}.", self.audio.name));
            }
            Ok(None) => self.set_info("Stopped transcribing."),
            Err(e) => self.set_error(e),
        }
    }

    pub(super) fn play_audio(&mut self) {
        if self.audio.playing.is_some() {
            if self.audio.paused {
                return self.toggle_audio_pause();
            }
            return self.set_info(format!("{} is already playing.", self.audio.name));
        }
        let Some(path) = self.audio.file.clone() else {
            return self.set_error("Choose an audio file first.");
        };
        let control = Arc::new(Control::default());
        self.audio.play_id += 1;
        self.audio.playing = Some(control.clone());
        self.audio.paused = false;
        self.set_info(format!("Playing {}.", self.audio.name));
        worker::play(&self.rep, self.audio.play_id, control, path);
    }

    pub(super) fn audio_played(&mut self, id: u64, result: Result<bool, String>) {
        if id != self.audio.play_id {
            return;
        }
        let was_playing = self.audio.playing.take().is_some();
        self.audio.paused = false;
        match result {
            Ok(true) => self.set_info(format!("Finished playing {}.", self.audio.name)),
            Ok(false) if was_playing => self.set_info("Stopped playing."),
            Ok(false) => {}
            Err(e) => self.set_error(format!("{}: {e}", self.audio.name)),
        }
    }

    pub(super) fn toggle_audio_pause(&mut self) {
        let Some(control) = &self.audio.playing else {
            return self.set_error("Nothing is playing.");
        };
        let paused = !self.audio.paused;
        control.set_paused(paused);
        self.audio.paused = paused;
        self.set_info(if paused { "Paused." } else { "Resumed." });
    }

    /// Stops playing, or if nothing is playing, stops transcribing.
    pub(super) fn stop_audio(&mut self) {
        if let Some(control) = self.audio.playing.take() {
            control.request_stop();
            self.audio.paused = false;
            self.set_info("Stopped playing.");
        } else if self.audio.transcribing.is_some() {
            self.stop_transcribing();
        } else {
            self.set_info("Nothing is playing.");
        }
    }

    fn stop_transcribing(&mut self) {
        if let Some(control) = &self.audio.transcribing {
            control.request_stop();
            self.set_info("Stopping the transcription.");
        }
    }

    pub(super) fn save_transcript(&mut self) {
        if self.audio.transcript.is_empty() {
            return self.set_error("There's no transcript to save yet.");
        }
        let stem = self.audio.name.rsplit_once('.').map_or(self.audio.name.as_str(), |(s, _)| s);
        let suggested = format!("{stem} transcript.txt");
        let Some(mut path) = self.dialogs.save_file("Save the transcript", &suggested, "Text", &["txt"]) else {
            return;
        };
        if !path.extension().is_some_and(|e| e.eq_ignore_ascii_case("txt")) {
            path.as_mut_os_string().push(".txt");
        }
        match std::fs::write(&path, &self.audio.transcript) {
            Ok(()) => self.set_info(format!("Saved {}.", display_name(&path))),
            Err(e) => {
                log::warn!("Saving a transcript failed: {e}");
                self.set_error(format!("{} could not be saved.", display_name(&path)));
            }
        }
    }

    fn playback_bar(&self, ui: &mut Ui) {
        let length = self.audio.heard.as_ref().map(|h| h.seconds());
        let (fraction, text) = match &self.audio.playing {
            Some(control) => {
                // The position only moves while playing, so the window has to look again.
                ui.ctx().request_repaint_after(Duration::from_millis(250));
                let at = control.position().unwrap_or_default();
                let at_text = clock(at.as_millis() as u64);
                let doing = if self.audio.paused { "Paused at" } else { "Playing," };
                match length.filter(|l| *l > 0.0 && !self.audio.heard.as_ref().is_some_and(|h| h.shortened)) {
                    Some(l) => (
                        Some((at.as_secs_f32() / l).clamp(0.0, 1.0)),
                        format!("{doing} {at_text} of {}", clock((l * 1000.0) as u64)),
                    ),
                    None => (Some(0.0), format!("{doing} {at_text}")),
                }
            }
            None => (Some(0.0), "Not playing".to_owned()),
        };
        progress_bar(ui, "playback", "Playback", fraction, &text);
    }

    fn transcription_bar(&self, ui: &mut Ui) {
        let (fraction, text) = if self.audio.transcribing.is_some() {
            let stage = self.audio.stage.split('.').next().unwrap_or("Starting").to_owned();
            (Some(self.audio.progress), format!("{stage}, {:.0}%", self.audio.progress * 100.0))
        } else if self.audio.hearing {
            (None, format!("Listening to {}", self.audio.name))
        } else {
            (Some(0.0), "Not transcribing".to_owned())
        };
        progress_bar(ui, "transcription", "Transcription", fraction, &text);
    }

    pub(super) fn audio_tab(&mut self, ui: &mut Ui) {
        heading(ui, "Audio Player", 2);
        let picker = path_field(
            ui,
            "Audio file to play (MP3 or WAV). Type its path and press Enter, or use the button to choose it.",
            "audio_path",
            &mut self.audio.path_input,
            "Choose an audio file",
            Some(&shortcuts::OPEN),
        );
        let field = picker.field;
        if field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
            field.request_focus();
            let typed = self.audio.path_input.trim().to_owned();
            if typed.is_empty() {
                self.set_error("Type the path of an audio file first.");
            } else {
                self.open_audio(PathBuf::from(typed));
            }
        }
        if picker.choose {
            self.choose_audio();
        }
        ui.label(
            "If it is mostly speech, it is transcribed on this computer with whisper.cpp, and each voice is named \
             Speaker 1, Speaker 2 and so on. Nothing is installed or downloaded without asking you first.",
        );

        let pause_label = if self.audio.paused { "Resume" } else { "Pause" };
        let transcribe_label = if self.audio.transcribing.is_some() { "Stop Transcribing" } else { "Transcribe" };
        let pressed = button_row(
            ui,
            &[
                RowButton { action: Action::Play, salt: "play", text: "Play", shortcut: Some(&shortcuts::READ) },
                RowButton { action: Action::Pause, salt: "pause", text: pause_label, shortcut: Some(&shortcuts::PAUSE) },
                RowButton { action: Action::Stop, salt: "stop", text: "Stop", shortcut: Some(&shortcuts::STOP) },
                RowButton { action: Action::Transcribe, salt: "transcribe", text: transcribe_label, shortcut: None },
            ],
        );
        match pressed {
            Some(Action::Play) => self.play_audio(),
            Some(Action::Pause) => self.toggle_audio_pause(),
            Some(Action::Stop) => self.stop_audio(),
            Some(Action::Transcribe) if self.audio.transcribing.is_some() => self.stop_transcribing(),
            Some(Action::Transcribe) => self.start_transcription(),
            None => {}
        }
        self.playback_bar(ui);
        self.transcription_bar(ui);

        text_area(ui, "Transcript", &self.audio.transcript);
        if full_button(ui, "save_transcript", "Save the transcript", Some(&shortcuts::SAVE)).clicked() {
            self.save_transcript();
        }
    }
}

/// Used by the Settings tab, so it can say what is installed.
pub(super) fn whisper_status(chosen: Option<&std::path::Path>) -> String {
    let program = match whisper::find_program(chosen) {
        Some(path) => format!("whisper.cpp is at {}.", path.display()),
        None if chosen.is_some() => "The whisper.cpp program chosen isn't there.".to_owned(),
        None => "whisper.cpp wasn't found.".to_owned(),
    };
    let model = if whisper::model_path().is_file() {
        "The speech recognition model is downloaded.".to_owned()
    } else {
        format!(
            "The speech recognition model, about {} MB, will be downloaded the first time it's needed, if you agree.",
            whisper::MODEL_BYTES / 1_000_000
        )
    };
    format!("{program} {model}")
}

#[cfg(test)]
impl AudioState {
    /// Only the Unix test with a stand-in whisper.cpp needs it.
    #[cfg(unix)]
    pub(super) fn transcript(&self) -> &str {
        &self.transcript
    }

    pub(super) fn is_transcribing(&self) -> bool {
        self.transcribing.is_some()
    }

    /// Starts a transcription as far as the window can tell, returning its number.
    pub(super) fn begin_transcription_for_test(&mut self) -> u64 {
        self.transcribe_id += 1;
        self.transcribing = Some(Arc::new(Control::default()));
        self.transcribe_id
    }
}
