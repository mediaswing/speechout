//! Keyboard shortcuts: which keys do what, checks that keep them clear of
//! screen readers and the operating system, and matching of key presses.
//!
//! Screen readers see every key first and keep their own commands (the NVDA,
//! JAWS and Narrator keys, and VoiceOver's Control+Option), so the app never
//! receives those. The shortcuts offered here are also kept clear of:
//! * single letters, digits and punctuation, which people type and screen
//!   readers use for quick navigation (WCAG 2.1.4);
//! * Alt and Option, because Ctrl+Alt is AltGr on Windows and Option types
//!   characters on a Mac;
//! * Tab, Enter, Space, the arrow keys and editing shortcuts such as Ctrl+C;
//! * keys the operating system or its accessibility features use.
//!
//! Modifiers must match exactly, so typing "ś" with AltGr+S never saves. Each
//! shortcut can be changed or turned off on the Settings tab, and all of them
//! can be turned off at once.

use crate::i18n::{self, t};
use egui::{Event, Key, Modifiers};
use std::collections::BTreeMap;

/// The value saved in settings for a shortcut that has been turned off.
pub const OFF: &str = "off";

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Platform {
    Windows,
    Mac,
    Linux,
}

impl Platform {
    #[cfg(test)]
    pub const ALL: [Platform; 3] = [Platform::Windows, Platform::Mac, Platform::Linux];

    pub const CURRENT: Platform = if cfg!(target_os = "macos") {
        Platform::Mac
    } else if cfg!(target_os = "windows") {
        Platform::Windows
    } else {
        Platform::Linux
    };
}

/// Modifier keys. `command` is Cmd on a Mac and Ctrl elsewhere. `ctrl` is
/// the Control key on a Mac, and is never set elsewhere.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct Mods {
    pub command: bool,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

impl Mods {
    /// The modifiers held for a key press.
    fn pressed(m: Modifiers, platform: Platform) -> Self {
        match platform {
            Platform::Mac => Mods { command: m.mac_cmd, ctrl: m.ctrl, shift: m.shift, alt: m.alt },
            _ => Mods { command: m.ctrl || m.command, ctrl: false, shift: m.shift, alt: m.alt },
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Shortcut {
    pub mods: Mods,
    pub key: Key,
}

impl Shortcut {
    const fn key(key: Key) -> Self {
        Shortcut { mods: Mods { command: false, ctrl: false, shift: false, alt: false }, key }
    }

    /// Cmd on a Mac, Ctrl elsewhere.
    const fn command(key: Key) -> Self {
        Shortcut { mods: Mods { command: true, ctrl: false, shift: false, alt: false }, key }
    }

    /// The Control key on every platform, as in Ctrl+Tab.
    fn ctrl(key: Key, shift: bool, platform: Platform) -> Self {
        let mac = platform == Platform::Mac;
        Shortcut { mods: Mods { command: !mac, ctrl: mac, shift, alt: false }, key }
    }

    /// Reads a shortcut as saved in settings, such as "Ctrl+Shift+Tab" or
    /// "F5". "Cmd" means the Command key on a Mac and Ctrl elsewhere.
    pub fn parse(text: &str, platform: Platform) -> Option<Self> {
        let mut parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let key = Key::from_name(parts.pop()?)?;
        let mut mods = Mods::default();
        for part in parts {
            let flag = match part {
                "Cmd" => &mut mods.command,
                "Ctrl" if platform == Platform::Mac => &mut mods.ctrl,
                "Ctrl" => &mut mods.command,
                "Shift" => &mut mods.shift,
                "Alt" | "Option" => &mut mods.alt,
                _ => return None,
            };
            if *flag {
                return None;
            }
            *flag = true;
        }
        Some(Shortcut { mods, key })
    }

    /// The text saved in settings, which `parse` reads back.
    pub fn to_text(self, platform: Platform) -> String {
        let mut parts = Vec::new();
        if self.mods.ctrl {
            parts.push("Ctrl");
        }
        if self.mods.command {
            parts.push(if platform == Platform::Mac { "Cmd" } else { "Ctrl" });
        }
        if self.mods.alt {
            parts.push("Alt");
        }
        if self.mods.shift {
            parts.push("Shift");
        }
        parts.push(self.key.name());
        parts.join("+")
    }

    /// As shown on buttons and lists, such as "Ctrl+O" or "Esc".
    pub fn short(self, platform: Platform) -> String {
        self.describe(platform, false)
    }

    /// As said in spoken messages, such as "Control+O" or "Escape".
    pub fn spoken(self, platform: Platform) -> String {
        self.describe(platform, true)
    }

    fn describe(self, platform: Platform, spoken: bool) -> String {
        let mac = platform == Platform::Mac;
        let mut parts = Vec::new();
        if self.mods.ctrl {
            parts.push(if spoken { "Control" } else { "Ctrl" });
        }
        if self.mods.command {
            parts.push(match (mac, spoken) {
                (true, true) => "Command",
                (true, false) => "Cmd",
                (false, true) => "Control",
                (false, false) => "Ctrl",
            });
        }
        if self.mods.alt {
            parts.push(if mac { "Option" } else { "Alt" });
        }
        if self.mods.shift {
            parts.push("Shift");
        }
        parts.push(match self.key {
            Key::Escape if spoken => "Escape",
            Key::Escape => "Esc",
            Key::Period if spoken => "Full stop",
            Key::Period => ".",
            Key::Enter if mac => "Return",
            Key::PageUp => "Page Up",
            Key::PageDown => "Page Down",
            key => key.name(),
        });
        parts.join("+")
    }
}

/// Why a shortcut is not offered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Conflict {
    /// A letter, digit or symbol on its own: typed as text, and used by
    /// screen readers to move around a window (WCAG 2.1.4).
    CharacterKey,
    /// Moves focus, scrolls, edits text or presses the focused control.
    NavigationKey,
    /// Alt or Option: menus, AltGr characters and VoiceOver commands.
    AltKey,
    /// Insert is the NVDA, JAWS and Narrator key.
    ScreenReaderKey,
    /// Used by the operating system, its accessibility features, input
    /// methods or familiar editing commands.
    Reserved,
}

/// Checks a shortcut against the keys that screen readers, the operating
/// system and text editing rely on.
pub fn check(shortcut: Shortcut, platform: Platform) -> Result<(), Conflict> {
    use Key::*;
    let Shortcut { mods, key } = shortcut;
    let function_key = function_key_number(key);
    let navigation = matches!(
        key,
        Tab | Enter | Space | Backspace | Delete | ArrowUp | ArrowDown | ArrowLeft | ArrowRight | Home | End | PageUp
            | PageDown
    );
    if key == Insert {
        return Err(Conflict::ScreenReaderKey);
    }
    if mods.alt {
        return Err(Conflict::AltKey);
    }

    if !mods.command && !mods.ctrl {
        return match function_key {
            // Shift+F10 opens context menus; other Shift+function keys are
            // left alone too.
            Some(_) if mods.shift => Err(Conflict::Reserved),
            Some(1..=9) => Ok(()),
            // F10 opens menus, F11 is full screen or Show Desktop, F12 opens
            // developer tools and dashboards.
            Some(_) => Err(Conflict::Reserved),
            None if key == Escape && !mods.shift => Ok(()),
            None if key == Escape => Err(Conflict::Reserved),
            None if navigation => Err(Conflict::NavigationKey),
            None => Err(Conflict::CharacterKey),
        };
    }

    // Ctrl or Cmd with a function key: Cmd+F5 turns VoiceOver on and off,
    // Control+F1 to F8 are Full Keyboard Access on a Mac, Ctrl+F4 closes.
    if function_key.is_some() {
        return Err(Conflict::Reserved);
    }
    // Word and line movement, and deleting words.
    if matches!(key, ArrowUp | ArrowDown | ArrowLeft | ArrowRight | Home | End | Backspace | Delete) {
        return Err(Conflict::NavigationKey);
    }
    let tab_keys = matches!(key, Tab | PageUp | PageDown);
    if platform == Platform::Mac {
        if mods.ctrl {
            // Control on a Mac is otherwise Mission Control, Spaces, input
            // sources, and Command+Control system commands.
            return if !mods.command && tab_keys { Ok(()) } else { Err(Conflict::Reserved) };
        }
        if tab_keys {
            // Cmd+Tab switches apps.
            return Err(Conflict::Reserved);
        }
    }
    if mods.shift {
        // Only Ctrl+Shift+Tab, the previous tab. Shift combinations take
        // screenshots on a Mac, switch keyboard layouts on Windows and are
        // used by input methods and other apps.
        return if key == Tab { Ok(()) } else { Err(Conflict::Reserved) };
    }
    let reserved: &[Key] = match platform {
        // Editing, find, hide, minimise, new, print, quit, new tab, close,
        // settings, switching windows and Spotlight.
        Platform::Mac => &[A, C, V, X, Z, Y, F, G, H, M, N, P, Q, T, W, Comma, Backtick, Space, Escape],
        // Editing, find, replace, new, print, quit, new tab and close, the
        // Start menu, and input method switching (Ctrl+Space, Ctrl+.).
        _ => &[A, C, V, X, Z, Y, F, G, H, N, P, Q, T, W, Space, Escape, Period, Comma],
    };
    if reserved.contains(&key) { Err(Conflict::Reserved) } else { Ok(()) }
}

fn function_key_number(key: Key) -> Option<u8> {
    let name = key.name();
    name.strip_prefix('F').and_then(|n| n.parse().ok())
}

/// The digit on a key, for Ctrl+1 to Ctrl+4.
fn digit(key: Key) -> Option<usize> {
    let name = key.name();
    (name.len() == 1).then(|| name.parse().ok()).flatten()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Action {
    Open,
    Save,
    /// Ctrl+1 goes to the first tab, Ctrl+2 to the second and so on.
    GoToTab,
    NextTab,
    PreviousTab,
    Read,
    ReadSecond,
    Pause,
    Stop,
    StopSecond,
    Progress,
    Help,
}

impl Action {
    pub const ALL: [Action; 12] = [
        Action::Open,
        Action::Save,
        Action::GoToTab,
        Action::NextTab,
        Action::PreviousTab,
        Action::Read,
        Action::ReadSecond,
        Action::Pause,
        Action::Stop,
        Action::StopSecond,
        Action::Progress,
        Action::Help,
    ];

    /// The name saved in settings.
    pub fn id(self) -> &'static str {
        match self {
            Action::Open => "open",
            Action::Save => "save",
            Action::GoToTab => "go_to_tab",
            Action::NextTab => "next_tab",
            Action::PreviousTab => "previous_tab",
            Action::Read => "read",
            Action::ReadSecond => "read_second",
            Action::Pause => "pause",
            Action::Stop => "stop",
            Action::StopSecond => "stop_second",
            Action::Progress => "progress",
            Action::Help => "help",
        }
    }

    pub fn label(self) -> String {
        t(match self {
            Action::Open => "shortcut.open",
            Action::Save => "shortcut.save",
            Action::GoToTab => "shortcut.go_to_tab",
            Action::NextTab => "shortcut.next_tab",
            Action::PreviousTab => "shortcut.previous_tab",
            Action::Read => "shortcut.read",
            Action::ReadSecond => "shortcut.read_second",
            Action::Pause => "shortcut.pause",
            Action::Stop => "shortcut.stop",
            Action::StopSecond => "shortcut.stop_second",
            Action::Progress => "shortcut.progress",
            Action::Help => "shortcut.help",
        })
    }

    pub fn default_shortcut(self, platform: Platform) -> Option<Shortcut> {
        let mac = platform == Platform::Mac;
        Some(match self {
            Action::Open => Shortcut::command(Key::O),
            Action::Save => Shortcut::command(Key::S),
            Action::GoToTab => Shortcut::command(Key::Num1),
            Action::NextTab => Shortcut::ctrl(Key::Tab, false, platform),
            Action::PreviousTab => Shortcut::ctrl(Key::Tab, true, platform),
            Action::Read => Shortcut::key(Key::F5),
            // A Mac's function keys control the computer unless Fn is held,
            // so Macs get a second shortcut that needs no Fn.
            Action::ReadSecond if mac => Shortcut::command(Key::R),
            Action::Pause => Shortcut::key(Key::F6),
            Action::Stop => Shortcut::key(Key::Escape),
            // Cmd+. is the usual way to cancel on a Mac.
            Action::StopSecond if mac => Shortcut::command(Key::Period),
            Action::Progress => Shortcut::key(Key::F7),
            Action::Help => Shortcut::key(Key::F1),
            Action::ReadSecond | Action::StopSecond => return None,
        })
    }

    /// The shortcuts that can be chosen for this action, apart from Off.
    pub fn choices(self, platform: Platform) -> Vec<Shortcut> {
        let mut choices = match self {
            Action::GoToTab => return vec![Shortcut::command(Key::Num1)],
            Action::NextTab => {
                return vec![Shortcut::ctrl(Key::Tab, false, platform), Shortcut::ctrl(Key::PageDown, false, platform)];
            }
            Action::PreviousTab => {
                return vec![Shortcut::ctrl(Key::Tab, true, platform), Shortcut::ctrl(Key::PageUp, false, platform)];
            }
            Action::Help => vec![Shortcut::key(Key::F1)],
            Action::Stop | Action::StopSecond => vec![Shortcut::key(Key::Escape)],
            _ => Vec::new(),
        };
        choices.extend([Key::F2, Key::F3, Key::F4, Key::F5, Key::F6, Key::F7, Key::F8, Key::F9].map(Shortcut::key));
        choices.extend([Key::O, Key::S, Key::R, Key::E, Key::J, Key::K, Key::L, Key::Enter].map(Shortcut::command));
        if platform == Platform::Mac {
            choices.push(Shortcut::command(Key::Period));
        }
        choices
    }
}

/// What a key press asks the app to do.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Command {
    Run(Action),
    /// Go to the tab with this index, counting from 0.
    Tab(usize),
}

/// What the window looks like when a key is pressed.
#[derive(Clone, Copy, Debug, Default)]
pub struct KeyContext {
    /// A dropdown list is open, so keys belong to it.
    pub popup_open: bool,
    /// The index of the tab button that has focus, if one has.
    pub focused_tab: Option<usize>,
    pub tab_count: usize,
}

/// Placeholders in interface text that tell people which key to press, the
/// sentence they become, and the actions whose shortcut they name. The
/// first action with a shortcut is used.
const HINTS: [(&str, &str, &[Action]); 12] = [
    ("open_hint", "hint.open", &[Action::Open]),
    ("open_here_hint", "hint.open_here", &[Action::Open]),
    ("read_hint", "hint.read", &[Action::Read, Action::ReadSecond]),
    ("play_hint", "hint.play", &[Action::Read, Action::ReadSecond]),
    ("save_hint", "hint.save", &[Action::Save]),
    ("save_transcript_hint", "hint.save_transcript", &[Action::Save]),
    ("pause_hint", "hint.pause", &[Action::Pause]),
    ("resume_hint", "hint.resume", &[Action::Pause]),
    ("time_hint", "hint.time", &[Action::Progress]),
    ("stop_hint", "hint.stop", &[Action::Stop, Action::StopSecond]),
    ("cancel_hint", "hint.cancel", &[Action::Stop, Action::StopSecond]),
    ("help_hint", "hint.help", &[Action::Help]),
];

/// The shortcut chosen for each action.
#[derive(Clone, Debug)]
pub struct Keymap {
    platform: Platform,
    enabled: bool,
    bindings: Vec<(Action, Option<Shortcut>)>,
}

impl Keymap {
    /// The defaults, with the changes saved in settings. A saved shortcut
    /// that is not one of the action's choices, or is already used by an
    /// earlier action, is ignored, so a damaged or hand-edited settings file
    /// cannot bring back a conflicting key.
    pub fn new(enabled: bool, saved: &BTreeMap<String, String>, platform: Platform) -> Self {
        let mut bindings: Vec<(Action, Option<Shortcut>)> = Vec::new();
        for action in Action::ALL {
            let chosen = match saved.get(action.id()) {
                None => action.default_shortcut(platform),
                Some(text) if text == OFF => None,
                Some(text) => match Shortcut::parse(text, platform)
                    .filter(|s| action.choices(platform).contains(s) && check(*s, platform).is_ok())
                {
                    Some(shortcut) => Some(shortcut),
                    None => {
                        log::warn!("ignoring the saved shortcut {text:?} for {}", action.id());
                        action.default_shortcut(platform)
                    }
                },
            };
            let taken = chosen.is_some() && bindings.iter().any(|(_, b)| *b == chosen);
            if taken {
                log::warn!("turning off the shortcut for {}: another action already uses it", action.id());
            }
            bindings.push((action, chosen.filter(|_| !taken)));
        }
        Keymap { platform, enabled, bindings }
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// The shortcut chosen for `action`, even while shortcuts are off.
    pub fn binding(&self, action: Action) -> Option<Shortcut> {
        self.bindings.iter().find(|(a, _)| *a == action).and_then(|(_, b)| *b)
    }

    /// The shortcut that works for `action` now.
    pub fn active(&self, action: Action) -> Option<Shortcut> {
        self.binding(action).filter(|_| self.enabled)
    }

    /// The action other than `except` that uses `shortcut`.
    pub fn used_by(&self, shortcut: Shortcut, except: Action) -> Option<Action> {
        self.bindings.iter().find(|(a, b)| *a != except && *b == Some(shortcut)).map(|(a, _)| *a)
    }

    /// How a shortcut for `action` is shown, such as "Ctrl+1 to Ctrl+4"
    /// for going to a tab.
    pub fn describe(&self, action: Action, shortcut: Shortcut) -> String {
        let p = self.platform;
        if action == Action::GoToTab {
            let last = Shortcut { key: Key::Num4, ..shortcut };
            return format!("{} – {}", shortcut.short(p), last.short(p));
        }
        shortcut.short(p)
    }

    /// " (F5)" to follow a button label, or nothing if `action` has no
    /// shortcut at the moment.
    pub fn hint(&self, action: Action) -> String {
        self.active(action).map(|s| format!(" ({})", s.short(self.platform))).unwrap_or_default()
    }

    /// The sentences that tell people which key to press, for the interface
    /// text.
    pub fn hints(&self) -> Vec<i18n::Hint> {
        HINTS
            .iter()
            .map(|(placeholder, text_key, actions)| i18n::Hint {
                placeholder,
                text_key,
                key: actions.iter().find_map(|a| self.active(*a)).map(|s| s.spoken(self.platform)),
            })
            .collect()
    }

    /// Takes the key presses that are shortcuts out of `events`, so the
    /// controls never see them, and returns what they ask for. `ready` says
    /// whether an action can run now; when it can't, its key is left alone
    /// for the controls (Escape only stops while something is running).
    ///
    /// Also, while a tab has focus, the Left and Right arrows, Home and End
    /// move between tabs, as in the ARIA tabs pattern. Escape on its own is
    /// removed when no list is open, because otherwise it would take keyboard
    /// focus away from the control, leaving screen reader users nowhere.
    /// Typed text is never touched.
    pub fn take(&self, events: &mut Vec<Event>, cx: KeyContext, ready: impl Fn(Action) -> bool) -> Vec<Command> {
        let mut commands = Vec::new();
        events.retain(|event| {
            let Event::Key { key, pressed: true, modifiers, .. } = event else { return true };
            match self.command_for(*key, *modifiers, cx, &ready) {
                Some(command) => {
                    commands.push(command);
                    false
                }
                None => !(*key == Key::Escape && modifiers.is_none() && !cx.popup_open),
            }
        });
        commands
    }

    fn command_for(&self, key: Key, modifiers: Modifiers, cx: KeyContext, ready: &impl Fn(Action) -> bool) -> Option<Command> {
        if let Some(i) = cx.focused_tab
            && modifiers.is_none()
            && cx.tab_count > 0
        {
            let last = cx.tab_count - 1;
            let to = match key {
                Key::ArrowLeft => Some(if i == 0 { last } else { i - 1 }),
                Key::ArrowRight => Some(if i >= last { 0 } else { i + 1 }),
                Key::Home => Some(0),
                Key::End => Some(last),
                _ => None,
            };
            if let Some(to) = to {
                return Some(Command::Tab(to));
            }
        }
        if cx.popup_open || !self.enabled {
            return None;
        }
        let pressed = Mods::pressed(modifiers, self.platform);
        for (action, binding) in &self.bindings {
            let Some(binding) = binding else { continue };
            if binding.mods != pressed {
                continue;
            }
            if *action == Action::GoToTab {
                if let Some(n) = digit(key)
                    && (1..=cx.tab_count).contains(&n)
                {
                    return Some(Command::Tab(n - 1));
                }
            } else if binding.key == key && ready(*action) {
                return Some(Command::Run(*action));
            }
        }
        None
    }

    /// The quick reference shown by F1: each shortcut, then the keys that
    /// always work.
    pub fn reference(&self) -> String {
        let mut out = String::new();
        if self.enabled {
            for (action, binding) in &self.bindings {
                if let Some(shortcut) = binding {
                    out.push_str(&format!("{}: {}\n", action.label(), self.describe(*action, *shortcut)));
                }
            }
        } else {
            out.push_str(&t("shortcuts.dialog_off"));
            out.push('\n');
        }
        let alt = if self.platform == Platform::Mac { "Option" } else { "Alt" };
        out.push('\n');
        out.push_str(&i18n::tf("shortcuts.dialog_always", &[("alt", &alt)]));
        out.push_str("\n\n");
        out.push_str(&t("shortcuts.dialog_change"));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(key: Key, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    fn keymap(platform: Platform) -> Keymap {
        Keymap::new(true, &BTreeMap::new(), platform)
    }

    fn cx() -> KeyContext {
        KeyContext { popup_open: false, focused_tab: None, tab_count: 4 }
    }

    fn take(map: &Keymap, events: &mut Vec<Event>, cx: KeyContext) -> Vec<Command> {
        map.take(events, cx, |_| true)
    }

    // ----- conflicts ------------------------------------------------------

    #[test]
    fn defaults_are_safe_and_unique() {
        for p in Platform::ALL {
            let defaults: Vec<Shortcut> = Action::ALL.iter().filter_map(|a| a.default_shortcut(p)).collect();
            for s in &defaults {
                assert_eq!(check(*s, p), Ok(()), "{} on {p:?}", s.short(p));
            }
            for (i, s) in defaults.iter().enumerate() {
                assert!(!defaults[i + 1..].contains(s), "{} is used twice on {p:?}", s.short(p));
            }
        }
    }

    #[test]
    fn every_choice_is_safe() {
        for p in Platform::ALL {
            for action in Action::ALL {
                let choices = action.choices(p);
                if let Some(default) = action.default_shortcut(p) {
                    assert!(choices.contains(&default), "{action:?} on {p:?} doesn't offer its default");
                }
                for s in choices {
                    assert_eq!(check(s, p), Ok(()), "{action:?}: {} on {p:?}", s.short(p));
                }
            }
        }
    }

    #[test]
    fn single_character_keys_are_never_shortcuts() {
        // WCAG 2.1.4: no letter, digit or punctuation key on its own, or
        // with only Shift.
        for p in Platform::ALL {
            for action in Action::ALL {
                for s in action.choices(p) {
                    let typed = !s.mods.command && !s.mods.ctrl && !s.mods.alt;
                    assert!(!(typed && s.key.name().chars().count() == 1), "{} on {p:?}", s.short(p));
                }
            }
            for key in [Key::H, Key::K, Key::Num1, Key::Period, Key::Slash] {
                assert_eq!(check(Shortcut::key(key), p), Err(Conflict::CharacterKey));
                let shifted = Shortcut { mods: Mods { shift: true, ..Mods::default() }, key };
                assert_eq!(check(shifted, p), Err(Conflict::CharacterKey));
            }
        }
    }

    #[test]
    fn rejects_keys_that_others_rely_on() {
        let with = |command, ctrl, shift, alt, key| Shortcut { mods: Mods { command, ctrl, shift, alt }, key };
        for p in Platform::ALL {
            for key in [Key::Tab, Key::Enter, Key::Space, Key::ArrowDown, Key::Home, Key::PageDown] {
                assert_eq!(check(Shortcut::key(key), p), Err(Conflict::NavigationKey), "{key:?}");
            }
            // AltGr on Windows, Option on a Mac.
            assert_eq!(check(with(true, false, false, true, Key::S), p), Err(Conflict::AltKey));
            assert_eq!(check(with(false, false, false, true, Key::F5), p), Err(Conflict::AltKey));
            assert_eq!(check(with(true, false, false, false, Key::Insert), p), Err(Conflict::ScreenReaderKey));
            for key in [Key::C, Key::V, Key::X, Key::Z, Key::A, Key::F, Key::Q, Key::W, Key::Space] {
                assert_eq!(check(Shortcut::command(key), p), Err(Conflict::Reserved), "{key:?}");
            }
            for key in [Key::F10, Key::F11, Key::F12] {
                assert_eq!(check(Shortcut::key(key), p), Err(Conflict::Reserved));
            }
            // Cmd+F5 is VoiceOver; Ctrl+F4 closes windows.
            assert_eq!(check(Shortcut::command(Key::F5), p), Err(Conflict::Reserved));
            assert_eq!(check(Shortcut::command(Key::ArrowLeft), p), Err(Conflict::NavigationKey));
            // Ctrl+Shift+digit switches keyboard layouts; Cmd+Shift+3 takes a screenshot.
            assert_eq!(check(with(true, false, true, false, Key::Num3), p), Err(Conflict::Reserved));
        }
        let mac = Platform::Mac;
        // VoiceOver's Control+Option, and other Control combinations on a Mac.
        assert_eq!(check(with(false, true, false, true, Key::A), mac), Err(Conflict::AltKey));
        assert_eq!(check(with(false, true, false, false, Key::F2), mac), Err(Conflict::Reserved));
        assert_eq!(check(with(false, true, false, false, Key::ArrowRight), mac), Err(Conflict::NavigationKey));
        assert_eq!(check(with(false, true, false, false, Key::Space), mac), Err(Conflict::Reserved));
        assert_eq!(check(Shortcut::command(Key::Tab), mac), Err(Conflict::Reserved));
        assert_eq!(check(Shortcut::command(Key::H), mac), Err(Conflict::Reserved));
        // Ctrl+. switches input methods on Windows; Cmd+. cancels on a Mac.
        assert_eq!(check(Shortcut::command(Key::Period), Platform::Windows), Err(Conflict::Reserved));
        assert_eq!(check(Shortcut::command(Key::Period), mac), Ok(()));
    }

    // ----- matching key presses -------------------------------------------

    #[test]
    fn runs_shortcuts_and_removes_their_keys() {
        let map = keymap(Platform::Windows);
        let mut events = vec![press(Key::S, Modifiers::CTRL), press(Key::F5, Modifiers::NONE)];
        let got = take(&map, &mut events, cx());
        assert_eq!(got, vec![Command::Run(Action::Save), Command::Run(Action::Read)]);
        assert!(events.is_empty());
    }

    #[test]
    fn altgr_and_extra_modifiers_do_not_trigger_shortcuts() {
        let map = keymap(Platform::Windows);
        // AltGr+S types "ś" on a Polish keyboard; Windows reports Ctrl+Alt+S.
        let altgr = Modifiers { alt: true, ctrl: true, command: true, ..Modifiers::NONE };
        let mut events = vec![press(Key::S, altgr), Event::Text("ś".into()), press(Key::Num2, altgr)];
        assert!(take(&map, &mut events, cx()).is_empty());
        assert_eq!(events.len(), 3);
        // Ctrl+Shift+S is not Ctrl+S, and Shift+F5 is not F5.
        let mut events = vec![press(Key::S, Modifiers::CTRL | Modifiers::SHIFT), press(Key::F5, Modifiers::SHIFT)];
        assert!(take(&map, &mut events, cx()).is_empty());
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn typed_text_and_navigation_keys_are_left_alone() {
        let map = keymap(Platform::Windows);
        let mut events = vec![
            Event::Text("h".into()),
            press(Key::H, Modifiers::NONE),
            press(Key::Tab, Modifiers::NONE),
            press(Key::Tab, Modifiers::SHIFT),
            press(Key::Enter, Modifiers::NONE),
            press(Key::Space, Modifiers::NONE),
            press(Key::ArrowDown, Modifiers::NONE),
            press(Key::C, Modifiers::CTRL),
        ];
        let before = events.clone();
        assert!(take(&map, &mut events, cx()).is_empty());
        assert_eq!(events, before);
    }

    #[test]
    fn mac_uses_command_not_control() {
        let map = keymap(Platform::Mac);
        let cmd = Modifiers { mac_cmd: true, command: true, ..Modifiers::NONE };
        let mut events = vec![press(Key::O, cmd), press(Key::O, Modifiers::CTRL), press(Key::R, cmd)];
        let got = take(&map, &mut events, cx());
        assert_eq!(got, vec![Command::Run(Action::Open), Command::Run(Action::ReadSecond)]);
        assert_eq!(events.len(), 1, "Control+O is not a shortcut on a Mac");
        // Ctrl+Tab uses the Control key on a Mac too.
        let mut events = vec![press(Key::Tab, Modifiers::CTRL), press(Key::Tab, Modifiers::CTRL | Modifiers::SHIFT)];
        let got = take(&map, &mut events, cx());
        assert_eq!(got, vec![Command::Run(Action::NextTab), Command::Run(Action::PreviousTab)]);
    }

    #[test]
    fn number_shortcuts_go_to_tabs() {
        let map = keymap(Platform::Linux);
        let mut events = vec![press(Key::Num3, Modifiers::CTRL), press(Key::Num5, Modifiers::CTRL)];
        assert_eq!(take(&map, &mut events, cx()), vec![Command::Tab(2)]);
        assert_eq!(events.len(), 1, "there is no fifth tab");
    }

    #[test]
    fn escape_stops_only_while_something_runs_and_never_drops_focus() {
        let map = keymap(Platform::Windows);
        let mut events = vec![press(Key::Escape, Modifiers::NONE)];
        assert_eq!(map.take(&mut events, cx(), |_| true), vec![Command::Run(Action::Stop)]);

        // Nothing running: no command, and Escape is removed so egui doesn't
        // take focus away.
        let mut events = vec![press(Key::Escape, Modifiers::NONE)];
        assert!(map.take(&mut events, cx(), |a| a != Action::Stop).is_empty());
        assert!(events.is_empty());

        // A list is open: Escape is left for the list to close.
        let open = KeyContext { popup_open: true, ..cx() };
        let mut events = vec![press(Key::Escape, Modifiers::NONE)];
        assert!(map.take(&mut events, open, |_| true).is_empty());
        assert_eq!(events.len(), 1);
    }

    #[test]
    fn open_lists_keep_their_keys() {
        let map = keymap(Platform::Windows);
        let open = KeyContext { popup_open: true, ..cx() };
        let mut events = vec![press(Key::F5, Modifiers::NONE), press(Key::O, Modifiers::CTRL)];
        assert!(take(&map, &mut events, open).is_empty());
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn arrows_move_between_tabs_only_while_a_tab_has_focus() {
        let map = keymap(Platform::Windows);
        let on_tab = |i| KeyContext { focused_tab: Some(i), ..cx() };
        let keys = |key| vec![press(key, Modifiers::NONE)];
        assert_eq!(take(&map, &mut keys(Key::ArrowRight), on_tab(1)), vec![Command::Tab(2)]);
        assert_eq!(take(&map, &mut keys(Key::ArrowRight), on_tab(3)), vec![Command::Tab(0)]);
        assert_eq!(take(&map, &mut keys(Key::ArrowLeft), on_tab(0)), vec![Command::Tab(3)]);
        assert_eq!(take(&map, &mut keys(Key::Home), on_tab(2)), vec![Command::Tab(0)]);
        assert_eq!(take(&map, &mut keys(Key::End), on_tab(0)), vec![Command::Tab(3)]);
        // Elsewhere, arrows are left for the focused control.
        let mut events = keys(Key::ArrowRight);
        assert!(take(&map, &mut events, cx()).is_empty());
        assert_eq!(events.len(), 1);
        // They still work with every shortcut turned off: they are part of
        // how tabs work, not shortcuts.
        let off = Keymap::new(false, &BTreeMap::new(), Platform::Windows);
        assert_eq!(take(&off, &mut keys(Key::ArrowRight), on_tab(0)), vec![Command::Tab(1)]);
    }

    // ----- settings ----------------------------------------------------------

    #[test]
    fn turning_shortcuts_off() {
        let off = Keymap::new(false, &BTreeMap::new(), Platform::Windows);
        let mut events = vec![press(Key::F5, Modifiers::NONE), press(Key::O, Modifiers::CTRL)];
        assert!(take(&off, &mut events, cx()).is_empty());
        assert_eq!(events.len(), 2);
        assert_eq!(off.hint(Action::Read), "");
        assert!(off.hints().iter().all(|h| h.key.is_none()));

        let saved = BTreeMap::from([("read".to_owned(), OFF.to_owned())]);
        let map = Keymap::new(true, &saved, Platform::Windows);
        let mut events = vec![press(Key::F5, Modifiers::NONE)];
        assert!(take(&map, &mut events, cx()).is_empty());
        assert_eq!(map.hint(Action::Read), "");
    }

    #[test]
    fn reassigning_a_shortcut() {
        let saved = BTreeMap::from([("pause".to_owned(), "Ctrl+J".to_owned())]);
        let map = Keymap::new(true, &saved, Platform::Windows);
        let mut events = vec![press(Key::J, Modifiers::CTRL), press(Key::F6, Modifiers::NONE)];
        assert_eq!(take(&map, &mut events, cx()), vec![Command::Run(Action::Pause)]);
        assert_eq!(events.len(), 1, "F6 is free now");
        assert_eq!(map.hint(Action::Pause), " (Ctrl+J)");
        assert_eq!(map.used_by(Shortcut::command(Key::J), Action::Read), Some(Action::Pause));
        assert_eq!(map.used_by(Shortcut::command(Key::J), Action::Pause), None);
    }

    #[test]
    fn unsafe_unknown_or_duplicate_saved_shortcuts_are_ignored() {
        let saved = BTreeMap::from([
            ("open".to_owned(), "Ctrl+C".to_owned()),
            ("save".to_owned(), "H".to_owned()),
            ("pause".to_owned(), "nonsense".to_owned()),
            // F5 is already Read aloud.
            ("progress".to_owned(), "F5".to_owned()),
        ]);
        let map = Keymap::new(true, &saved, Platform::Windows);
        assert_eq!(map.binding(Action::Open), Some(Shortcut::command(Key::O)));
        assert_eq!(map.binding(Action::Save), Some(Shortcut::command(Key::S)));
        assert_eq!(map.binding(Action::Pause), Some(Shortcut::key(Key::F6)));
        assert_eq!(map.binding(Action::Progress), None);
        assert_eq!(map.binding(Action::Read), Some(Shortcut::key(Key::F5)));
    }

    #[test]
    fn saved_text_round_trips() {
        for p in Platform::ALL {
            for action in Action::ALL {
                for s in action.choices(p) {
                    assert_eq!(Shortcut::parse(&s.to_text(p), p), Some(s), "{} on {p:?}", s.to_text(p));
                }
            }
        }
        assert_eq!(Shortcut::parse("Ctrl+Ctrl+O", Platform::Windows), None);
        assert_eq!(Shortcut::parse("Hyper+O", Platform::Windows), None);
    }

    #[test]
    fn names_shown_and_spoken() {
        let w = Platform::Windows;
        let m = Platform::Mac;
        assert_eq!(Shortcut::command(Key::O).short(w), "Ctrl+O");
        assert_eq!(Shortcut::command(Key::O).short(m), "Cmd+O");
        assert_eq!(Shortcut::command(Key::O).spoken(m), "Command+O");
        assert_eq!(Shortcut::key(Key::Escape).short(w), "Esc");
        assert_eq!(Shortcut::key(Key::Escape).spoken(w), "Escape");
        assert_eq!(Shortcut::ctrl(Key::Tab, true, m).short(m), "Ctrl+Shift+Tab");
        assert_eq!(Shortcut::command(Key::Period).spoken(m), "Command+Full stop");
        let map = keymap(w);
        assert_eq!(map.describe(Action::GoToTab, Shortcut::command(Key::Num1)), "Ctrl+1 – Ctrl+4");
    }

    #[test]
    fn hints_name_the_first_action_with_a_shortcut() {
        let saved = BTreeMap::from([("stop".to_owned(), OFF.to_owned())]);
        let map = Keymap::new(true, &saved, Platform::Mac);
        let stop = map.hints().into_iter().find(|h| h.placeholder == "stop_hint").unwrap();
        assert_eq!(stop.key.as_deref(), Some("Command+Full stop"));
    }
}
