//! Keyboard shortcuts, and the view F1 shows them in. The README lists them too,
//! and a test keeps the two lists in step. "Ctrl" is Cmd (⌘) on macOS.

use egui::{Key, KeyboardShortcut, Modifiers, Ui};

use super::widgets::{full_button, heading, stable_scope, text_area};
use super::{App, Tab};

pub(super) const OPEN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
pub(super) const READ: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::R);
pub(super) const PAUSE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::P);
pub(super) const SAVE: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::S);
pub(super) const STOP: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::Escape);
const SHOW_SHORTCUTS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F1);
const REPEAT_STATUS: KeyboardShortcut = KeyboardShortcut::new(Modifiers::NONE, Key::F2);
// egui itself acts on these three.
const ZOOM_IN: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Plus);
const ZOOM_OUT: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Minus);
const ZOOM_RESET: KeyboardShortcut = KeyboardShortcut::new(Modifiers::COMMAND, Key::Num0);

const TAB_KEYS: [Key; 4] = [Key::Num1, Key::Num2, Key::Num3, Key::Num4];

/// Ctrl+1 for the first tab, Ctrl+2 for the second, and so on.
pub(super) fn tab(tab: Tab) -> KeyboardShortcut {
    let index = Tab::ALL.iter().position(|t| *t == tab).expect("every tab is in Tab::ALL");
    KeyboardShortcut::new(Modifiers::COMMAND, TAB_KEYS[index])
}

/// Every shortcut and what it does, as (keys, action), in the README's order.
pub(super) fn describe(ctx: &egui::Context) -> Vec<(String, &'static str)> {
    let keys = |shortcuts: &[KeyboardShortcut]| {
        shortcuts.iter().map(|shortcut| ctx.format_shortcut(shortcut)).collect::<Vec<_>>().join(", ")
    };
    vec![
        ("Tab, Shift+Tab".to_owned(), "Move between controls"),
        ("Space, Enter".to_owned(), "Press the control with the focus"),
        (keys(&Tab::ALL.map(tab)), "Go to General, Settings, Wordlist or Audio Player"),
        (keys(&[OPEN]), "Choose a file; on the Audio Player tab, an audio file"),
        (keys(&[READ]), "Read aloud; on the Audio Player tab, play"),
        (keys(&[PAUSE]), "Pause or resume"),
        (
            keys(&[STOP]),
            "Stop reading, saving, opening, playing or transcribing; close a dropdown list or the keyboard shortcuts",
        ),
        (keys(&[SAVE]), "Save as MP3; on the Settings tab, save the API key; on the Audio Player tab, save the transcript"),
        (keys(&[SHOW_SHORTCUTS]), "Show or hide the keyboard shortcuts"),
        (keys(&[REPEAT_STATUS]), "Say the status message again"),
        ("Up, Down, Home, End, or a letter".to_owned(), "Change the choice in a dropdown"),
        ("Alt+Down (Option+Down on a Mac)".to_owned(), "Open a dropdown's list; Alt+Up closes it"),
        (keys(&[ZOOM_IN, ZOOM_OUT, ZOOM_RESET]), "Make everything larger or smaller, or reset it"),
    ]
}

impl App {
    fn toggle_shortcuts(&mut self, ctx: &egui::Context) {
        if self.shortcuts_open {
            self.close_shortcuts(ctx);
        } else {
            self.shortcuts_open = true;
            self.focus_before_shortcuts = ctx.memory(|m| m.focused());
            self.focus_wanted = Some("shortcuts");
            egui::Popup::close_all(ctx);
        }
    }

    /// Puts the focus back where it was when the view opened.
    pub(super) fn close_shortcuts(&mut self, ctx: &egui::Context) {
        if !self.shortcuts_open {
            return;
        }
        self.shortcuts_open = false;
        match self.focus_before_shortcuts.take() {
            Some(id) => ctx.memory_mut(|m| m.request_focus(id)),
            None => self.focus_wanted = Some(self.tab.name()),
        }
    }

    /// Takes the place of the selected tab's controls while it is open. The text
    /// is in a read-only box so a screen reader can go through it line by line.
    pub(super) fn shortcuts_view(&mut self, ui: &mut Ui) {
        heading(ui, "Keyboard Shortcuts", 2);
        let lines: Vec<String> = describe(ui.ctx()).into_iter().map(|(keys, action)| format!("{keys}: {action}")).collect();
        let list = stable_scope(ui, "shortcuts", |ui| text_area(ui, "Keyboard shortcuts, one on each line", &lines.join("\n")));
        if self.wants_focus("shortcuts") {
            list.request_focus();
        }
        if full_button(ui, "close_shortcuts", "Close keyboard shortcuts", Some(&SHOW_SHORTCUTS)).clicked() {
            self.close_shortcuts(ui.ctx());
        }
    }

    /// Takes Escape out of the input before egui sees it, because egui would
    /// take the keyboard focus away from whichever control has it, which leaves
    /// keyboard and screen reader users with no idea where they are.
    pub(super) fn take_escape(&mut self, input: &mut egui::RawInput) {
        input.events.retain(|event| match event {
            egui::Event::Key { key: Key::Escape, pressed, modifiers, .. } if modifiers.is_none() => {
                self.escape_pressed |= *pressed;
                false
            }
            _ => true,
        });
    }

    /// Act on shortcuts pressed this frame. Runs before the panels are drawn, so
    /// the focus requests it makes land on this frame's widgets.
    pub(super) fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        for t in Tab::ALL {
            if ctx.input_mut(|i| i.consume_shortcut(&tab(t))) {
                self.switch_tab(t);
            }
        }
        let on_audio = self.tab == Tab::AudioPlayer;
        if ctx.input_mut(|i| i.consume_shortcut(&OPEN)) {
            self.close_shortcuts(ctx);
            if on_audio {
                self.choose_audio();
            } else {
                // The focus stays where it is if already on this tab.
                if self.tab != Tab::General {
                    self.switch_tab(Tab::General);
                }
                self.choose_file();
            }
        }
        if ctx.input_mut(|i| i.consume_shortcut(&READ)) {
            if on_audio { self.play_audio() } else { self.read_aloud() }
        }
        if ctx.input_mut(|i| i.consume_shortcut(&PAUSE)) {
            if on_audio { self.toggle_audio_pause() } else { self.toggle_pause() }
        }
        if ctx.input_mut(|i| i.consume_shortcut(&SAVE)) {
            match self.tab {
                Tab::Settings => self.save_api_key(),
                Tab::AudioPlayer => self.save_transcript(),
                _ => self.save_as_mp3(),
            }
        }
        if ctx.input_mut(|i| i.consume_shortcut(&SHOW_SHORTCUTS)) {
            self.toggle_shortcuts(ctx);
        }
        if ctx.input_mut(|i| i.consume_shortcut(&REPEAT_STATUS)) {
            let (text, error) = (self.status.text.clone(), self.status.error);
            self.announce(text.trim_end_matches('\u{a0}').to_owned(), error);
        }
        // Escape closes an open dropdown first, as everywhere else.
        if std::mem::take(&mut self.escape_pressed) {
            if egui::Popup::is_any_open(ctx) {
                egui::Popup::close_all(ctx);
            } else if self.shortcuts_open {
                self.close_shortcuts(ctx);
            } else if on_audio && self.audio.busy() {
                self.stop_audio();
            } else if self.job.is_some() || self.loading.is_some() {
                self.stop();
            } else if self.audio.busy() {
                self.stop_audio();
            }
        }
    }
}
