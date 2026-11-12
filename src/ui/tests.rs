//! Drives the whole window the way a keyboard and screen reader user would:
//! pressing keys, then looking at the accessibility tree AccessKit gives to
//! screen readers. Nothing here uses the pointer.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use egui::accesskit::{Live, Role, Toggled};
use egui::{Event, Key, Modifiers};
use egui_kittest::Harness;
use egui_kittest::kittest::NodeT;

use super::*;
use crate::store::{AiKind, Appearance};

/// Stands in for the native dialogs: answers from a list, and remembers asks.
#[derive(Clone, Default)]
struct Script {
    files: Rc<RefCell<VecDeque<PathBuf>>>,
    confirm: Rc<RefCell<bool>>,
    asked: Rc<RefCell<Vec<String>>>,
}

impl Dialogs for Script {
    fn pick_file(&self, title: &str, _: &str, _: &[&str]) -> Option<PathBuf> {
        self.asked.borrow_mut().push(title.to_owned());
        self.files.borrow_mut().pop_front()
    }
    fn pick_folder(&self, title: &str) -> Option<PathBuf> {
        self.asked.borrow_mut().push(title.to_owned());
        self.files.borrow_mut().pop_front()
    }
    fn save_file(&self, title: &str, name: &str, _: &str, _: &[&str]) -> Option<PathBuf> {
        self.asked.borrow_mut().push(format!("{title}: {name}"));
        self.files.borrow_mut().pop_front()
    }
    fn confirm(&self, title: &str, _: &str) -> bool {
        self.asked.borrow_mut().push(title.to_owned());
        *self.confirm.borrow()
    }
}

/// The app is made during the first frame, so that it can use the harness's context.
type Window = Harness<'static, Option<App>>;

fn open(store: Store) -> (Window, Script) {
    let script = Script::default();
    let dialogs = script.clone();
    let mut parts = Some((store, dialogs));
    let mut harness = Harness::builder().with_size(egui::vec2(1000.0, 1900.0)).build_ui_state(
        move |ui, app: &mut Option<App>| {
            let app = app.get_or_insert_with(|| {
                let (store, dialogs) = parts.take().expect("made once");
                App::new(ui.ctx(), store, Box::new(dialogs), String::new())
            });
            app.show(ui);
        },
        None,
    );
    harness.run_steps(3);
    // Whether this computer has system voices is no concern of most tests, so
    // wait for the search to finish and clear any message about it.
    let end = Instant::now() + Duration::from_secs(30);
    while matches!(harness.state().as_ref().unwrap().voices.get(&crate::speech::Provider::System), Some(Loadable::Loading)) {
        assert!(Instant::now() < end, "the search for system voices never finished");
        harness.step();
        std::thread::sleep(Duration::from_millis(10));
    }
    harness.step();
    harness.state_mut().as_mut().unwrap().status = Status { text: "Ready.".to_owned(), error: false };
    harness.run_steps(2);
    (harness, script)
}

fn window() -> (Window, Script) {
    open(Store::in_memory().unwrap())
}

fn app(h: &Window) -> &App {
    h.state().as_ref().unwrap()
}

fn app_mut(h: &mut Window) -> &mut App {
    h.state_mut().as_mut().unwrap()
}

fn press(h: &mut Window, key: Key) {
    h.key_press(key);
    h.run_steps(3);
}

/// Escape, as the real window's input hook handles it.
fn escape(h: &mut Window) {
    let mut input = egui::RawInput {
        events: vec![
            Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE },
            Event::Key { key: Key::Escape, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE },
        ],
        ..Default::default()
    };
    app_mut(h).take_escape(&mut input);
    assert!(input.events.is_empty(), "egui must not see Escape, or it would drop the focus");
    h.run_steps(3);
}

fn press_with(h: &mut Window, modifiers: Modifiers, key: Key) {
    h.key_press_modifiers(modifiers, key);
    h.run_steps(3);
}

fn command(h: &mut Window, key: Key) {
    press_with(h, Modifiers::COMMAND, key);
}

fn type_text(h: &mut Window, text: &str) {
    h.event(Event::Text(text.to_owned()));
    h.run_steps(2);
}

/// What a screen reader would say the focused control is called.
fn focused(h: &Window) -> Option<String> {
    let node = h.root().children_recursive().find(|n| n.accesskit_node().is_focused())?;
    Some(node.accesskit_node().label().unwrap_or_else(|| "(no name)".to_owned()))
}

fn focused_role(h: &Window) -> Option<Role> {
    h.root().children_recursive().find(|n| n.accesskit_node().is_focused()).map(|n| n.accesskit_node().role())
}

/// Presses Tab until the focus comes back round, giving each stop's name.
fn tab_stops(h: &mut Window, modifiers: Modifiers) -> Vec<String> {
    let mut stops = Vec::new();
    for _ in 0..60 {
        press_with(h, modifiers, Key::Tab);
        let Some(name) = focused(h) else { continue };
        if stops.first() == Some(&name) {
            return stops;
        }
        stops.push(name);
    }
    panic!("the focus never came back round: {stops:?}");
}

fn status(h: &Window) -> String {
    h.root()
        .children_recursive()
        .find(|n| n.accesskit_node().live() != Live::Off)
        .and_then(|n| n.accesskit_node().value().or(n.accesskit_node().label()))
        .expect("the status bar is a live region")
}

fn status_live(h: &Window) -> Live {
    h.root().children_recursive().find(|n| n.accesskit_node().live() != Live::Off).unwrap().accesskit_node().live()
}

/// Waits for background work to change the status bar to something with `text` in it.
fn wait_for_status(h: &mut Window, text: &str) {
    let end = Instant::now() + Duration::from_secs(60);
    while Instant::now() < end {
        h.step();
        if status(h).contains(text) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the status never said {text:?}; it says {:?}", status(h));
}

fn node_by_label<'a>(h: &'a Window, label: &str) -> egui_kittest::Node<'a> {
    h.root()
        .children_recursive()
        .find(|n| n.accesskit_node().label().is_some_and(|l| l.contains(label)))
        .unwrap_or_else(|| panic!("nothing is labelled {label:?}"))
}

fn focus_on(h: &mut Window, label: &str) {
    for _ in 0..60 {
        if focused(h).is_some_and(|f| f.contains(label)) {
            return;
        }
        press(h, Key::Tab);
    }
    panic!("the keyboard can't reach {label:?}");
}

fn saved_settings(h: &Window) -> crate::store::Settings {
    app(h).store.settings().unwrap()
}

#[test]
fn the_keyboard_reaches_every_control_on_general_in_order() {
    let (mut h, _) = window();
    assert_eq!(focused(&h).as_deref(), Some("General"), "the focus starts somewhere");
    let stops = tab_stops(&mut h, Modifiers::NONE);
    let expected = [
        "Settings",
        "Wordlist",
        "Audio Player",
        "What to read",
        "File to read",
        "Choose a file",
        "Speech Service",
        "Voice",
        "Read Aloud",
        "Pause",
        "Stop",
        "Save as MP3",
        "Text that will be read aloud",
        "General",
    ];
    assert_eq!(stops.len(), expected.len(), "{stops:?}");
    for (stop, wanted) in stops.iter().zip(expected) {
        assert!(stop.contains(wanted), "{stop:?} should be {wanted:?}: {stops:?}");
    }
    h.check_accessibility();
}

#[test]
fn shift_tab_goes_backwards() {
    let (mut h, _) = window();
    press_with(&mut h, Modifiers::SHIFT, Key::Tab);
    assert!(focused(&h).unwrap().contains("Text that will be read aloud"), "{:?}", focused(&h));
    press_with(&mut h, Modifiers::SHIFT, Key::Tab);
    assert_eq!(focused(&h).as_deref(), Some("Save as MP3"));
}

fn choose_free_text(h: &mut Window) {
    focus_on(h, "What to read");
    press(h, Key::ArrowDown);
    assert_eq!(app(h).source, general::Source::FreeText);
    assert!(node_by_label(h, "What to read").accesskit_node().value().unwrap().contains("Free Text"));
}

#[test]
fn free_text_replaces_the_file_box_and_fills_the_pane() {
    let (mut h, _) = window();
    assert!(node_by_label(&h, "What to read").accesskit_node().value().unwrap().contains("Read a File"));
    choose_free_text(&mut h);
    let labels: Vec<String> = h.root().children_recursive().filter_map(|n| n.accesskit_node().label()).collect();
    assert!(!labels.iter().any(|l| l.contains("File to read") || l == "Choose a file"), "{labels:?}");
    assert!(!labels.iter().any(|l| l.contains("Text that will be read aloud")), "{labels:?}");
    let free = node_by_label(&h, "Free Text: type or paste").rect();
    let dropdown = node_by_label(&h, "What to read").rect();
    assert!((free.left() - dropdown.left()).abs() < 2.0 && (free.right() - dropdown.right()).abs() < 2.0, "{free:?} {dropdown:?}");
    assert!(free.height() > 150.0, "{free:?}");
    let stops = tab_stops(&mut h, Modifiers::NONE);
    let at = stops.iter().position(|s| s.contains("Free Text")).expect("the box can be reached");
    assert_eq!(stops[at + 1], "Speech Service", "Tab moves on from the box: {stops:?}");
    h.check_accessibility();
}

#[test]
fn free_text_is_what_gets_read_and_saved() {
    let (mut h, script) = window();
    choose_free_text(&mut h);
    command(&mut h, Key::R);
    assert_eq!(status(&h), "Type some text to read first.");
    focus_on(&mut h, "Free Text: type or paste");
    type_text(&mut h, "Hello from the keyboard.");
    assert_eq!(app(&h).free_text, "Hello from the keyboard.");
    // No file is given, so saving is cancelled after the dialog asks.
    command(&mut h, Key::S);
    assert_eq!(script.asked.borrow().last().unwrap(), "Save the speech as an MP3 file: speech.mp3");
    let job = app_mut(&mut h).speech_job();
    assert_eq!(job.map(|j| j.text.as_str().to_owned()).ok().as_deref(), Some("Hello from the keyboard."));
}

#[test]
fn choosing_a_file_goes_back_to_reading_a_file_and_keeps_the_typed_text() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.txt");
    std::fs::write(&path, "From the file.").unwrap();
    let (mut h, script) = window();
    choose_free_text(&mut h);
    focus_on(&mut h, "Free Text: type or paste");
    type_text(&mut h, "Typed.");
    script.files.borrow_mut().push_back(path);
    command(&mut h, Key::O);
    assert_eq!(app(&h).source, general::Source::File);
    wait_for_status(&mut h, "note.txt is ready");
    assert_eq!(app(&h).free_text, "Typed.");
}

#[test]
fn the_text_box_does_not_trap_the_keyboard() {
    let (mut h, _) = window();
    focus_on(&mut h, "Text that will be read aloud");
    press(&mut h, Key::Tab);
    assert_eq!(focused(&h).as_deref(), Some("General"));
}

#[test]
fn every_tab_passes_the_accessible_name_check() {
    let (mut h, _) = window();
    for key in [Key::Num1, Key::Num2, Key::Num3, Key::Num4] {
        command(&mut h, key);
        h.check_accessibility();
        for stop in tab_stops(&mut h, Modifiers::NONE) {
            assert_ne!(stop, "(no name)");
            assert!(!stop.trim().is_empty());
        }
    }
}

/// The buttons that control reading, left to right, as a screen reader finds them.
fn reading_buttons(h: &Window) -> Vec<(String, egui::Rect)> {
    let names = ["Read Aloud", "Pause", "Resume", "Stop", "Save as MP3"];
    h.root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == Role::Button)
        .filter_map(|n| n.accesskit_node().label().filter(|l| names.contains(&l.as_str())).map(|l| (l, n.rect())))
        .collect()
}

#[test]
fn the_reading_buttons_share_a_line_that_fills_the_pane() {
    let (h, _) = window();
    let buttons = reading_buttons(&h);
    let order: Vec<&str> = buttons.iter().map(|b| b.0.as_str()).collect();
    assert_eq!(order, ["Read Aloud", "Pause", "Stop", "Save as MP3"]);
    let choose = node_by_label(&h, "Progress").rect();
    let (first, last) = (buttons[0].1, buttons[3].1);
    assert!((first.left() - choose.left()).abs() < 2.0 && (last.right() - choose.right()).abs() < 2.0, "{first:?} {last:?} {choose:?}");
    for (name, rect) in &buttons {
        assert_eq!(rect.top(), first.top(), "{name} is on the same line");
        assert!((rect.width() - first.width()).abs() < 1.0, "{name} has an equal share");
    }
    for pair in buttons.windows(2) {
        assert!(pair[1].1.left() >= pair[0].1.right(), "{pair:?} overlap");
    }
}

#[test]
fn the_reading_buttons_stay_in_the_pane_when_the_window_is_narrow() {
    let (mut h, _) = window();
    // The smallest the window can be, then larger text as with Ctrl+Plus.
    for (width, zoom) in [(560.0, 1.0), (560.0, 2.0)] {
        h.set_size(egui::vec2(width, 1900.0));
        h.ctx.set_zoom_factor(zoom);
        h.run_steps(3);
        let buttons = reading_buttons(&h);
        let choose = node_by_label(&h, "Progress").rect();
        assert!(choose.width() > 100.0, "the pane keeps usable room: {choose:?}");
        for (name, rect) in &buttons {
            assert!(rect.left() >= choose.left() - 2.0 && rect.right() <= choose.right() + 2.0, "{name}: {rect:?} {choose:?}");
            assert!(rect.width() > 30.0, "{name}: {rect:?}");
        }
        for (i, (name, rect)) in buttons.iter().enumerate() {
            for (other, other_rect) in &buttons[i + 1..] {
                assert!(!rect.shrink(1.0).intersects(*other_rect), "{name} and {other} overlap");
            }
        }
    }
}

#[test]
fn the_reading_buttons_work_and_keep_the_focus_when_pause_changes_to_resume() {
    let (mut h, _) = window();
    focus_on(&mut h, "Pause");
    let control = std::sync::Arc::new(crate::worker::Control::default());
    app_mut(&mut h).job = Some((JobKind::Speaking, control));
    press(&mut h, Key::Space);
    assert_eq!(status(&h), "Paused.");
    assert_eq!(focused(&h).as_deref(), Some("Resume"), "the same button, with its new name");
    press(&mut h, Key::Enter);
    assert_eq!(status(&h), "Resumed.");
    assert_eq!(focused(&h).as_deref(), Some("Pause"));
    press(&mut h, Key::Tab);
    assert_eq!(focused(&h).as_deref(), Some("Stop"));
    press(&mut h, Key::Space);
    assert_eq!(status(&h), "Stopping.");
}

/// One control on each line, taking up the whole width of the pane. The buttons
/// that control reading share a line, as checked above, and a path box shares
/// one with its Choose button.
#[test]
fn every_control_fills_its_own_line() {
    let (mut h, _) = window();
    app_mut(&mut h).saved_keys.insert(crate::speech::Provider::OpenAi, true);
    for key in [Key::Num1, Key::Num2, Key::Num3, Key::Num4] {
        command(&mut h, key);
        let mut rows: Vec<(Role, egui::Rect)> = h
            .root()
            .children_recursive()
            .filter(|n| {
                let row = ["Read Aloud", "Pause", "Stop", "Save as MP3", "Play", "Transcribe"];
                !n.accesskit_node().label().is_some_and(|l| row.contains(&l.as_str()))
            })
            .filter(|n| {
                matches!(
                    n.accesskit_node().role(),
                    Role::Button
                        | Role::ComboBox
                        | Role::TextInput
                        | Role::PasswordInput
                        | Role::MultilineTextInput
                        | Role::CheckBox
                        | Role::ProgressIndicator
                )
            })
            .map(|n| (n.accesskit_node().role(), n.rect()))
            .collect();
        assert!(rows.len() >= 3, "{key:?}");
        rows.sort_by(|a, b| a.1.top().total_cmp(&b.1.top()));
        // The pane's edges, from the widest control.
        let left = rows.iter().map(|r| r.1.left()).fold(f32::INFINITY, f32::min);
        let right = rows.iter().map(|r| r.1.right()).fold(f32::NEG_INFINITY, f32::max);
        assert!(right - left > 700.0, "{key:?}: {left} to {right}");
        // The pane starts after the tabs and the window is 1000 wide.
        assert!(left < 220.0 && right > 960.0, "{key:?}: {left} to {right}");
        // Controls on the same line: alone, or a path box with its Choose button.
        let mut lines: Vec<Vec<(Role, egui::Rect)>> = Vec::new();
        for row in rows {
            match lines.last_mut() {
                Some(line) if (line[0].1.top() - row.1.top()).abs() < 2.0 => line.push(row),
                _ => lines.push(vec![row]),
            }
        }
        for line in &lines {
            let (first, last) = (line[0].1, line[line.len() - 1].1);
            assert!((first.left() - left).abs() < 2.0 && (last.right() - right).abs() < 2.0, "{key:?} {line:?}");
            match line.as_slice() {
                [_] => {}
                [(Role::TextInput, field), (Role::Button, button)] => {
                    assert!(button.left() >= field.right(), "{key:?}: {line:?} overlap");
                }
                _ => panic!("{key:?}: {line:?} share a line"),
            }
        }
        for pair in lines.windows(2) {
            assert!(pair[1][0].1.top() >= pair[0][0].1.bottom() - 1.0, "{key:?}: {pair:?} overlap");
        }
    }
}

#[test]
fn tabs_are_tabs_with_plain_names_and_a_selected_state() {
    let (mut h, _) = window();
    let selected = |h: &Window| -> Vec<String> {
        h.root()
            .children_recursive()
            .filter(|n| n.accesskit_node().role() == Role::Tab && n.accesskit_node().is_selected() == Some(true))
            .map(|n| n.accesskit_node().label().unwrap())
            .collect()
    };
    let names: Vec<String> = h
        .root()
        .children_recursive()
        .filter(|n| n.accesskit_node().role() == Role::Tab)
        .map(|n| n.accesskit_node().label().unwrap())
        .collect();
    assert_eq!(names, ["General", "Settings", "Wordlist", "Audio Player"]);
    assert_eq!(selected(&h), ["General"]);
    for (key, name) in [(Key::Num2, "Settings"), (Key::Num3, "Wordlist"), (Key::Num4, "Audio Player"), (Key::Num1, "General")] {
        command(&mut h, key);
        assert_eq!(selected(&h), [name]);
        assert_eq!(focused(&h).as_deref(), Some(name), "the focus goes with the tab");
    }
}

#[test]
fn tabs_belong_to_a_tab_list() {
    let (h, _) = window();
    let lists: Vec<_> = h.root().children_recursive().filter(|n| n.accesskit_node().role() == Role::TabList).collect();
    assert_eq!(lists.len(), 1, "one tab list");
    let tabs = lists[0].children_recursive().filter(|n| n.accesskit_node().role() == Role::Tab).count();
    assert_eq!(tabs, 4);
}

#[test]
fn enter_and_space_activate_a_tab() {
    let (mut h, _) = window();
    press(&mut h, Key::Tab);
    assert_eq!(focused(&h).as_deref(), Some("Settings"));
    press(&mut h, Key::Space);
    assert_eq!(app(&h).tab, Tab::Settings);
    press_with(&mut h, Modifiers::SHIFT, Key::Tab);
    press(&mut h, Key::Enter);
    assert_eq!(app(&h).tab, Tab::General);
}

#[test]
fn headings_are_headings() {
    let (mut h, _) = window();
    let headings = |h: &Window| -> Vec<String> {
        h.root()
            .children_recursive()
            .filter(|n| n.accesskit_node().role() == Role::Heading)
            .map(|n| n.accesskit_node().label().unwrap())
            .collect()
    };
    assert_eq!(headings(&h), ["General"]);
    command(&mut h, Key::Num2);
    assert_eq!(headings(&h), ["Settings", "Speech Services", "Appearance", "Describing Images", "Transcribing Audio", "Updates"]);
    command(&mut h, Key::Num3);
    assert_eq!(headings(&h), ["Wordlist", "Installed Wordlists"]);
    command(&mut h, Key::Num4);
    assert_eq!(headings(&h), ["Audio Player"]);
    press(&mut h, Key::F1);
    assert_eq!(headings(&h), ["Keyboard Shortcuts"]);
}

#[test]
fn f1_shows_the_shortcuts_and_closing_them_puts_the_focus_back() {
    let (mut h, _) = window();
    focus_on(&mut h, "Stop");

    press(&mut h, Key::F1);
    assert!(focused(&h).unwrap().contains("Keyboard shortcuts"), "{:?}", focused(&h));
    assert_eq!(focused_role(&h), Some(Role::MultilineTextInput));
    let list = node_by_label(&h, "one on each line").accesskit_node().value().unwrap();
    assert!(list.contains("Choose a file"), "{list}");
    assert!(list.contains("F1: Show or hide the keyboard shortcuts"), "{list}");

    press(&mut h, Key::F1);
    assert_eq!(focused(&h).as_deref(), Some("Stop"));

    press(&mut h, Key::F1);
    escape(&mut h);
    assert_eq!(focused(&h).as_deref(), Some("Stop"));

    press(&mut h, Key::F1);
    press(&mut h, Key::Tab);
    assert_eq!(focused(&h).as_deref(), Some("Close keyboard shortcuts"), "Tab moves on from the list");
    press(&mut h, Key::Enter);
    h.run_steps(2);
    assert_eq!(focused(&h).as_deref(), Some("Stop"));
}

#[test]
fn going_to_a_tab_closes_the_shortcuts() {
    let (mut h, _) = window();
    press(&mut h, Key::F1);
    command(&mut h, Key::Num2);
    assert!(!app(&h).shortcuts_open);
    assert_eq!(app(&h).tab, Tab::Settings);
    assert_eq!(focused(&h).as_deref(), Some("Settings"));
}

#[test]
fn the_shortcuts_match_the_readme() {
    let (h, _) = window();
    let readme = include_str!("../../README.md");
    for (_, action) in shortcuts::describe(&h.ctx) {
        assert!(action.starts_with(char::is_uppercase), "{action}");
        assert!(readme.contains(&format!("| {action} |")), "the README's Keyboard section is missing {action:?}");
    }
}

#[test]
fn the_status_bar_is_a_live_region_and_errors_interrupt() {
    let (mut h, _) = window();
    assert_eq!(status(&h), "Ready.");
    assert_eq!(status_live(&h), Live::Polite);
    command(&mut h, Key::R);
    assert_eq!(status(&h), "Choose a file first.");
    assert_eq!(status_live(&h), Live::Assertive);
}

#[test]
fn a_repeated_message_is_still_announced() {
    let (mut h, _) = window();
    command(&mut h, Key::R);
    let first = status(&h);
    command(&mut h, Key::R);
    assert_ne!(status(&h), first, "screen readers ignore text that doesn't change");
    assert_eq!(status(&h).trim_end_matches('\u{a0}'), first);
    press(&mut h, Key::F2);
    assert_eq!(status(&h), first, "F2 says it again by changing it back");
}

#[test]
fn buttons_that_cannot_work_yet_say_why() {
    let (mut h, _) = window();
    for (key, wanted) in [(Key::R, "Choose a file first"), (Key::S, "Choose a file first"), (Key::P, "Nothing is being read")] {
        command(&mut h, key);
        assert!(status(&h).contains(wanted), "{key:?}: {}", status(&h));
    }
    focus_on(&mut h, "Stop");
    press(&mut h, Key::Space);
    assert!(status(&h).contains("Nothing is being read aloud."), "{}", status(&h));
    for name in ["Read Aloud", "Pause", "Stop", "Save as MP3"] {
        assert!(!node_by_label(&h, name).accesskit_node().is_disabled(), "{name}");
    }
}

#[test]
fn opening_a_text_file_by_typing_its_path_and_pressing_enter() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("note.txt");
    std::fs::write(&path, "Hello from a text file.").unwrap();
    let (mut h, _) = window();
    focus_on(&mut h, "File to read");
    type_text(&mut h, &path.display().to_string());
    press(&mut h, Key::Enter);
    assert!(focused(&h).unwrap().contains("File to read"), "the focus stays in the box");
    wait_for_status(&mut h, "note.txt is ready to read aloud: 5 words");
    assert_eq!(status_live(&h), Live::Polite);
    h.step();
    let shown = node_by_label(&h, "Text that will be read aloud").accesskit_node();
    assert!(shown.value().unwrap().contains("Hello from a text file."));
    assert!(shown.is_read_only());
}

#[test]
fn unsupported_and_missing_files_are_explained() {
    let (mut h, script) = window();
    script.files.borrow_mut().push_back(PathBuf::from("/tmp/virus.exe"));
    command(&mut h, Key::O);
    assert!(status(&h).contains("can't be read"), "{}", status(&h));
    script.files.borrow_mut().push_back(PathBuf::from("/does/not/exist.pdf"));
    command(&mut h, Key::O);
    wait_for_status(&mut h, "exist.pdf: The file could not be opened");
    assert_eq!(status_live(&h), Live::Assertive);
}

#[test]
fn a_long_document_is_shortened_only_in_the_text_box() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("long.txt");
    std::fs::write(&path, "word ".repeat(10_000)).unwrap();
    let (mut h, script) = window();
    script.files.borrow_mut().push_back(path);
    command(&mut h, Key::O);
    wait_for_status(&mut h, "10000 words");
    h.step();
    let opened = app(&h).opened.as_ref().unwrap();
    assert_eq!(opened.text.split_whitespace().count(), 10_000);
    assert!(opened.preview.len() < opened.text.len());
    assert!(opened.preview.ends_with("All of it is read aloud and saved."));
}

#[test]
fn describing_an_image_with_a_local_ai_system() {
    let (address, _requests) = crate::vision::mock_server(r#"{"models":[{"name":"moondream"}],"response":"A red square."}"#);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("photo.JPG");
    std::fs::write(&path, crate::vision::geotagged_jpeg(None, (16, 16))).unwrap();
    let store = Store::in_memory().unwrap();
    let mut settings = store.settings().unwrap();
    settings.ai_address = address;
    let mut store = store;
    store.save_settings(&settings).unwrap();
    let (mut h, script) = open(store);
    script.files.borrow_mut().push_back(path);
    command(&mut h, Key::O);
    wait_for_status(&mut h, "photo.JPG is ready to read aloud");
    assert_eq!(app(&h).opened.as_ref().unwrap().text.as_str(), "A red square.");
    assert!(script.asked.borrow()[0].contains("image to describe"));
}

#[test]
fn starting_ollama_is_announced_and_old_news_is_ignored() {
    let (mut h, _) = window();
    let a = app_mut(&mut h);
    a.loading = Some("photo.jpg".to_owned());
    a.load_id = 3;
    let (tx, rx) = channel();
    a.rx = rx;
    tx.send(Msg::LoadStatus(2, "From an earlier file.".to_owned())).unwrap();
    a.handle_messages();
    assert_eq!(a.status.text, "Ready.");
    tx.send(Msg::LoadStatus(3, "Starting Ollama. This can take a moment.".to_owned())).unwrap();
    a.handle_messages();
    assert_eq!(a.status.text, "Starting Ollama. This can take a moment.");
    assert!(!a.status.error);
}

#[test]
fn a_remote_ai_address_is_refused() {
    let (mut h, _) = window();
    command(&mut h, Key::Num2);
    focus_on(&mut h, "Local AI address");
    app_mut(&mut h).ai_address_input.clear();
    type_text(&mut h, "http://192.168.1.10:11434");
    press(&mut h, Key::Enter);
    assert!(status(&h).contains("must be on this computer"), "{}", status(&h));
    assert_eq!(status_live(&h), Live::Assertive);
    assert_eq!(saved_settings(&h).ai_address, AiKind::Ollama.default_address());
}

#[test]
fn saving_a_key_offers_its_service_and_the_key_stays_hidden() {
    const SECRET: &str = "sk-this-should-stay-hidden";
    let (mut h, _) = window();
    assert_eq!(app(&h).available_providers(), [crate::speech::Provider::System]);
    command(&mut h, Key::Num2);
    choose_key_service(&mut h, "OpenAI TTS");
    focus_on(&mut h, "OpenAI TTS API key");
    assert_eq!(focused_role(&h), Some(Role::PasswordInput));
    type_text(&mut h, SECRET);
    assert!(
        !h.root().children_recursive().any(|n| {
            let n = n.accesskit_node();
            n.value().is_some_and(|v| v.contains(SECRET)) || n.label().is_some_and(|l| l.contains(SECRET))
        }),
        "a screen reader must not be given the key"
    );
    command(&mut h, Key::S);
    assert!(status(&h).contains("Saved the API key for OpenAI TTS"), "{}", status(&h));
    assert!(!status(&h).contains(SECRET));
    assert_eq!(app(&h).store.api_key(crate::speech::Provider::OpenAi).unwrap().as_deref(), Some(SECRET));
    assert!(app(&h).available_providers().contains(&crate::speech::Provider::OpenAi));
    assert!(node_by_label(&h, "OpenAI TTS API key (one is saved").accesskit_node().value().is_none_or(|v| !v.contains(SECRET)));

    // Choosing it on the General tab shows its voices and says the text will leave.
    command(&mut h, Key::Num1);
    focus_on(&mut h, "Speech Service");
    press(&mut h, Key::ArrowDown);
    assert_eq!(saved_settings(&h).provider, crate::speech::Provider::OpenAi);
    h.run_steps(2);
    wait_for_label(&mut h, "text is sent to it over the internet");
    wait_voice_is(&mut h, "Alloy");

    // Removing it goes back to system voices and leaves the focus somewhere.
    command(&mut h, Key::Num2);
    focus_on(&mut h, "Remove the OpenAI TTS API key");
    press(&mut h, Key::Space);
    h.run_steps(3);
    assert_eq!(focused(&h).as_deref(), Some("Save API key"));
    assert_eq!(app(&h).store.api_key(crate::speech::Provider::OpenAi).unwrap(), None);
    assert_eq!(saved_settings(&h).provider, crate::speech::Provider::System);
}

fn focused_value(h: &Window) -> Option<String> {
    h.root().children_recursive().find(|n| n.accesskit_node().is_focused())?.accesskit_node().value()
}

/// Picks a service in the dropdown above the API key box, from the Settings tab.
fn choose_key_service(h: &mut Window, service: &str) {
    focus_on(h, "Speech Service");
    press(h, Key::Home);
    for _ in crate::speech::Provider::CLOUD {
        if focused_value(h).is_some_and(|v| v.starts_with(service)) {
            return;
        }
        press(h, Key::ArrowDown);
    }
    panic!("the dropdown doesn't offer {service}");
}

#[test]
fn each_service_has_a_centred_sign_up_link_under_its_key_box() {
    let (mut h, _) = window();
    command(&mut h, Key::Num2);
    for provider in crate::speech::Provider::CLOUD {
        let service = provider.label();
        choose_key_service(&mut h, service);
        let name = format!("Sign up for {service}");
        let links: Vec<_> = h
            .root()
            .children_recursive()
            .filter(|n| n.accesskit_node().role() == Role::Link)
            .map(|n| (n.accesskit_node().label().unwrap_or_default(), n.rect()))
            .collect();
        assert_eq!(links.len(), 1, "one link, for the chosen service: {links:?}");
        let (label, link) = &links[0];
        assert_eq!(label, &name);
        let key_box = node_by_label(&h, &format!("{service} API key")).rect();
        assert!(link.top() >= key_box.bottom(), "{link:?} is under {key_box:?}");
        assert!((link.center().x - key_box.center().x).abs() < 2.0, "{link:?} is centred under {key_box:?}");
        focus_on(&mut h, &name);
        // Each key event gets a frame of its own, so look at the press's frame.
        let key = |pressed| Event::Key { key: Key::Enter, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE };
        h.event(key(true));
        h.step();
        let opened = h.output().platform_output.commands.iter().find_map(|c| match c {
            egui::OutputCommand::OpenUrl(open) => Some(open.url.clone()),
            _ => None,
        });
        h.event(key(false));
        h.run_steps(2);
        assert_eq!(opened.as_deref(), provider.signup_url(), "Enter opens the {service} page");
    }
}

#[test]
fn there_is_one_api_key_box_and_a_dropdown_chooses_its_service() {
    let (mut h, _) = window();
    command(&mut h, Key::Num2);
    let boxes = |h: &Window| h.root().children_recursive().filter(|n| n.accesskit_node().role() == Role::PasswordInput).count();
    assert_eq!(boxes(&h), 1);
    for service in ["ElevenLabs", "OpenAI TTS", "Deepgram Aura", "Speechify"] {
        choose_key_service(&mut h, service);
        assert_eq!(boxes(&h), 1);
        focus_on(&mut h, &format!("{service} API key"));
    }
}

#[test]
fn a_key_typed_for_one_service_is_never_saved_for_another() {
    use crate::speech::Provider::{ElevenLabs, OpenAi};
    let (mut h, _) = window();
    command(&mut h, Key::Num2);
    focus_on(&mut h, "ElevenLabs API key");
    type_text(&mut h, "meant-for-elevenlabs");
    choose_key_service(&mut h, "OpenAI TTS");
    assert!(status(&h).contains("cleared the API key you had typed"), "{}", status(&h));
    assert!(app(&h).key_input.is_empty());
    command(&mut h, Key::S);
    assert!(status(&h).contains("Type the OpenAI TTS API key first"), "{}", status(&h));
    assert_eq!(app(&h).store.api_key(OpenAi).unwrap(), None);
    assert_eq!(app(&h).store.api_key(ElevenLabs).unwrap(), None);
}

#[test]
fn the_dropdown_says_which_services_have_a_key() {
    let (mut h, _) = window();
    command(&mut h, Key::Num2);
    focus_on(&mut h, "ElevenLabs API key");
    type_text(&mut h, "a-key");
    command(&mut h, Key::S);
    h.run_steps(2);
    focus_on(&mut h, "Speech Service");
    assert_eq!(focused_value(&h).as_deref(), Some("ElevenLabs (key saved)"));
    choose_key_service(&mut h, "OpenAI TTS");
    assert_eq!(focused_value(&h).as_deref(), Some("OpenAI TTS"));
    assert_eq!(app(&h).store.api_key(crate::speech::Provider::ElevenLabs).unwrap().as_deref(), Some("a-key"));
}

fn wait_for_label(h: &mut Window, text: &str) {
    assert!(
        h.root().children_recursive().any(|n| {
            let n = n.accesskit_node();
            n.value().is_some_and(|v| v.contains(text)) || n.label().is_some_and(|l| l.contains(text))
        }),
        "nothing says {text:?}"
    );
}

fn wait_voice_is(h: &mut Window, name: &str) {
    let end = Instant::now() + Duration::from_secs(10);
    while Instant::now() < end {
        h.step();
        if node_by_label(h, "Voice").accesskit_node().value().is_some_and(|v| v == name) {
            return;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("the voice never became {name}");
}

#[test]
fn a_new_key_for_the_current_service_finds_its_voices_again() {
    let (mut h, _) = window();
    let openai = crate::speech::Provider::OpenAi;
    app(&h).store.set_api_key(openai, "old").unwrap();
    {
        let a = app_mut(&mut h);
        a.saved_keys.insert(openai, true);
        a.settings.provider = openai;
        a.ensure_voices(openai);
    }
    wait_voice_is(&mut h, "Alloy");
    command(&mut h, Key::Num2);
    choose_key_service(&mut h, "OpenAI TTS");
    focus_on(&mut h, "OpenAI TTS API key");
    type_text(&mut h, "new");
    command(&mut h, Key::S);
    assert!(status(&h).contains("without encryption"), "{}", status(&h));
    command(&mut h, Key::Num1);
    wait_voice_is(&mut h, "Alloy");
}

#[test]
fn choosing_a_cloud_service_says_the_text_will_leave() {
    let (mut h, _) = window();
    app_mut(&mut h).saved_keys.insert(crate::speech::Provider::Deepgram, true);
    focus_on(&mut h, "Speech Service");
    press(&mut h, Key::ArrowDown);
    assert!(status(&h).contains("Deepgram Aura is a cloud service"), "{}", status(&h));
    press(&mut h, Key::ArrowUp);
    assert_eq!(status(&h), "System voices stay on this computer.");
}

#[test]
fn a_failed_job_leaves_a_file_being_opened_alone() {
    let (mut h, _) = window();
    let a = app_mut(&mut h);
    a.loading = Some("big.pdf".to_owned());
    a.load_id = 7;
    a.job = Some((JobKind::Speaking, std::sync::Arc::new(crate::worker::Control::default())));
    a.handle_failure_for_test("The service failed.");
    a.file_loaded(7, Ok("Words here.".to_owned()));
    assert!(a.job.is_none());
    assert_eq!(a.status.text, "big.pdf is ready to read aloud: 2 words.");
}

#[test]
fn stop_goes_to_the_job_before_a_file_being_opened() {
    let (mut h, _) = window();
    let a = app_mut(&mut h);
    a.loading = Some("big.pdf".to_owned());
    a.job = Some((JobKind::Speaking, std::sync::Arc::new(crate::worker::Control::default())));
    a.stop();
    assert_eq!(a.status.text, "Stopping.");
    assert!(a.loading.is_some());
}

#[test]
fn choosing_a_file_keeps_the_focus_on_the_general_tab() {
    let (mut h, _) = window();
    focus_on(&mut h, "Save as MP3");
    command(&mut h, Key::O);
    assert_eq!(focused(&h).as_deref(), Some("Save as MP3"));
}

#[test]
fn a_dropdown_list_can_be_used_from_the_keyboard_alone() {
    let (mut h, _) = window();
    command(&mut h, Key::Num2);
    focus_on(&mut h, "Colours and text");
    press(&mut h, Key::Space);
    assert!(egui::Popup::is_any_open(&h.ctx));
    press(&mut h, Key::Tab);
    press(&mut h, Key::Tab);
    assert!(focused(&h).unwrap().starts_with("High Contrast"), "{:?}", focused(&h));
    press(&mut h, Key::Enter);
    h.run_steps(3);
    assert!(!egui::Popup::is_any_open(&h.ctx), "choosing closes the list");
    assert_eq!(saved_settings(&h).appearance, Appearance::HighContrast);
    assert!(focused(&h).unwrap().contains("Colours and text"), "the focus returns: {:?}", focused(&h));
}

#[test]
fn a_key_with_spaces_is_refused() {
    let (mut h, _) = window();
    command(&mut h, Key::Num2);
    choose_key_service(&mut h, "Deepgram Aura");
    focus_on(&mut h, "Deepgram Aura API key");
    type_text(&mut h, "two words");
    command(&mut h, Key::S);
    assert!(status(&h).contains("spaces"), "{}", status(&h));
    assert_eq!(app(&h).store.api_key(crate::speech::Provider::Deepgram).unwrap(), None);
}

#[test]
fn dropdowns_work_with_arrows_letters_and_home_and_end() {
    let (mut h, _) = window();
    command(&mut h, Key::Num2);
    focus_on(&mut h, "Say where a geotagged photo");
    assert_eq!(focused_role(&h), Some(Role::ComboBox));
    assert!(!saved_settings(&h).resolve_gps, "off unless asked for");
    press(&mut h, Key::ArrowDown);
    assert!(saved_settings(&h).resolve_gps);
    let value = node_by_label(&h, "Say where a geotagged photo").accesskit_node().value().unwrap();
    assert!(value.contains("OpenStreetMap"), "a screen reader hears the choice: {value}");
    assert!(focused(&h).unwrap().contains("Say where"), "arrows don't move the focus away");
    press(&mut h, Key::Home);
    assert!(!saved_settings(&h).resolve_gps);
    press(&mut h, Key::End);
    assert!(saved_settings(&h).resolve_gps);
    type_text(&mut h, "n");
    assert!(!saved_settings(&h).resolve_gps, "typing a letter jumps to the choice");
}

#[test]
fn dropdown_lists_open_and_close_from_the_keyboard() {
    let (mut h, _) = window();
    command(&mut h, Key::Num2);
    focus_on(&mut h, "Colours and text");
    press_with(&mut h, Modifiers::ALT, Key::ArrowDown);
    assert!(egui::Popup::is_any_open(&h.ctx), "Alt+Down opens the list");
    escape(&mut h);
    assert!(!egui::Popup::is_any_open(&h.ctx), "Escape closes it");
    assert!(focused(&h).unwrap().contains("Colours and text"), "and the focus stays: {:?}", focused(&h));
    assert_eq!(saved_settings(&h).appearance, Appearance::Standard);
    press(&mut h, Key::Space);
    assert!(egui::Popup::is_any_open(&h.ctx), "Space opens the list");
    press(&mut h, Key::Tab);
    assert!(focused(&h).is_some(), "the keyboard can go into the list");
}

#[test]
fn escape_never_loses_the_focus_and_stops_what_is_running() {
    let (mut h, _) = window();
    focus_on(&mut h, "Read Aloud");
    escape(&mut h);
    assert_eq!(focused(&h).as_deref(), Some("Read Aloud"));
    let control = std::sync::Arc::new(crate::worker::Control::default());
    app_mut(&mut h).job = Some((JobKind::Speaking, control));
    escape(&mut h);
    assert_eq!(status(&h), "Stopping.");
    assert_eq!(focused(&h).as_deref(), Some("Read Aloud"));
}

#[test]
fn the_focus_stays_put_when_controls_above_it_change() {
    let (mut h, _) = window();
    app_mut(&mut h).saved_keys.insert(crate::speech::Provider::OpenAi, true);
    focus_on(&mut h, "Read Aloud");
    // Choosing a cloud service adds a line above this button.
    app_mut(&mut h).settings.provider = crate::speech::Provider::OpenAi;
    h.run_steps(3);
    assert_eq!(focused(&h).as_deref(), Some("Read Aloud"));
}

#[test]
fn high_contrast_is_saved_and_applied() {
    let (mut h, _) = window();
    command(&mut h, Key::Num2);
    focus_on(&mut h, "Colours and text");
    press(&mut h, Key::ArrowDown);
    assert_eq!(saved_settings(&h).appearance, Appearance::HighContrast);
    h.run_steps(2);
    assert!(h.ctx.global_style().visuals.dark_mode);
    assert!(status(&h).contains("High Contrast"));
}

#[test]
fn the_log_folder_can_be_changed_from_the_keyboard_and_a_dialog() {
    let dir = tempfile::tempdir().unwrap();
    let chosen = dir.path().join("chosen");
    let (mut h, script) = window();
    command(&mut h, Key::Num2);
    focus_on(&mut h, "Choose the folder for debug logs");
    script.files.borrow_mut().push_back(chosen.clone());
    press(&mut h, Key::Space);
    assert_eq!(saved_settings(&h).log_dir, Some(chosen.clone()));
    assert!(chosen.join(crate::logging::LOG_FILE).exists());
    assert!(status(&h).contains("The debug log is now"), "{}", status(&h));

    // A folder that can't work keeps the old one and says why.
    focus_on(&mut h, "Folder for debug logs");
    app_mut(&mut h).log_dir_input.clear();
    type_text(&mut h, "relative/path");
    press(&mut h, Key::Enter);
    assert!(status(&h).contains("was not moved"), "{}", status(&h));
    assert_eq!(saved_settings(&h).log_dir, Some(chosen));
}

#[test]
fn the_log_box_comes_below_the_location_dropdown() {
    let (mut h, _) = window();
    command(&mut h, Key::Num2);
    let stops = tab_stops(&mut h, Modifiers::NONE);
    let at = |name: &str| stops.iter().position(|s| s.contains(name)).unwrap_or_else(|| panic!("{name}: {stops:?}"));
    assert_eq!(at("Say where a geotagged photo") + 1, at("Folder for debug logs"), "{stops:?}");
    at("Speech Service");
    at("ElevenLabs API key");
    at("Local AI system");
    at("Image description model");
}

#[test]
fn wordlists_can_be_turned_on_and_off_with_the_keyboard() {
    let (mut h, _) = window();
    command(&mut h, Key::Num3);
    focus_on(&mut h, "Classroom safe");
    assert_eq!(focused_role(&h), Some(Role::CheckBox));
    let toggled = |h: &Window| node_by_label(h, "Classroom safe:").accesskit_node().toggled();
    assert_eq!(toggled(&h), Some(Toggled::False), "the classroom list starts off");
    press(&mut h, Key::Space);
    assert_eq!(toggled(&h), Some(Toggled::True));
    assert!(status(&h).contains("Turned the wordlist Classroom safe on"), "{}", status(&h));
    let stored = app(&h).store.wordlists().unwrap();
    assert!(stored.iter().find(|w| w.list.name == "Classroom safe").unwrap().enabled);
    assert!(focused(&h).unwrap().contains("Classroom safe"), "the focus stays");
    press(&mut h, Key::Enter);
    assert_eq!(toggled(&h), Some(Toggled::False));
}

#[test]
fn importing_a_wordlist_and_bad_ones_are_explained() {
    let dir = tempfile::tempdir().unwrap();
    let good = dir.path().join("mine.xml");
    std::fs::write(&good, r#"<wordlist name="Mine" description="Mine"><entry match="a" replace="b"/></wordlist>"#).unwrap();
    let bad = dir.path().join("bad.xml");
    std::fs::write(&bad, "<!DOCTYPE x [<!ENTITY a 'b'>]><wordlist name='&a;'/>").unwrap();
    let (mut h, script) = window();
    command(&mut h, Key::Num3);
    focus_on(&mut h, "Import a wordlist");
    script.files.borrow_mut().push_back(good.clone());
    press(&mut h, Key::Space);
    assert!(status(&h).contains("Imported the wordlist Mine"), "{}", status(&h));
    wait_for_label(&mut h, "Mine: Mine (1 entries)");
    script.files.borrow_mut().push_back(good);
    press(&mut h, Key::Space);
    assert!(status(&h).contains("already installed"), "{}", status(&h));
    script.files.borrow_mut().push_back(bad);
    press(&mut h, Key::Space);
    assert!(status(&h).contains("bad.xml could not be imported"), "{}", status(&h));
    assert_eq!(status_live(&h), Live::Assertive);
}

#[test]
fn removing_wordlists_asks_first_and_never_strands_the_focus() {
    let (mut h, script) = window();
    command(&mut h, Key::Num3);
    let before = app(&h).wordlists.len();
    focus_on(&mut h, "Remove this wordlist");
    press(&mut h, Key::Space);
    assert_eq!(app(&h).wordlists.len(), before, "answering no keeps it");
    assert_eq!(script.asked.borrow().last().unwrap(), "Remove the wordlist");
    *script.confirm.borrow_mut() = true;
    for left in (0..before).rev() {
        press(&mut h, Key::Space);
        assert_eq!(app(&h).wordlists.len(), left);
        h.run_steps(2);
        assert!(focused(&h).is_some(), "the focus isn't lost with {left} left");
    }
    assert_eq!(focused(&h).as_deref(), Some("Import a wordlist"));
    wait_for_label(&mut h, "No wordlists are installed.");
    press(&mut h, Key::Space);
}

#[test]
fn all_controls_answer_screen_reader_clicks() {
    // Screen readers press controls through AccessKit, not the keyboard.
    let (mut h, _) = window();
    node_by_label(&h, "Wordlist").click_accesskit();
    h.run_steps(3);
    assert_eq!(app(&h).tab, Tab::Wordlist);
    node_by_label(&h, "General").click_accesskit();
    h.run_steps(3);
    node_by_label(&h, "Read Aloud").click_accesskit();
    h.run_steps(3);
    assert_eq!(status(&h), "Choose a file first.");
}

#[test]
fn window_saves_nothing_when_its_database_is_new() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("settings.sqlite3");
    {
        let (mut h, _) = open(Store::open(&path).unwrap());
        command(&mut h, Key::Num2);
        focus_on(&mut h, "Say where a geotagged photo");
        press(&mut h, Key::ArrowDown);
    }
    let store = Store::open(&path).unwrap();
    assert!(store.settings().unwrap().resolve_gps, "a change survives closing the app");
    assert_eq!(store.wordlists().unwrap().len(), crate::wordlist::EXAMPLES.len());
}

#[test]
fn the_title_is_the_full_name() {
    assert_eq!(crate::APP_TITLE, "Common Speech Output");
}

#[test]
fn not_finding_voices_is_explained() {
    let (mut h, _) = window();
    let provider = crate::speech::Provider::System;
    app_mut(&mut h).voices_loaded(provider, Err("Install the voices.".to_owned()));
    h.run_steps(2);
    assert_eq!(status(&h), "Install the voices.");
    assert_eq!(status_live(&h), Live::Assertive);
    assert!(node_by_label(&h, "Voice").accesskit_node().value().unwrap().contains("No voices"));
}

/// Uses the real system voices. CI installs eSpeak NG for this on Ubuntu.
#[cfg(unix)]
#[test]
fn from_file_to_mp3_through_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("note.txt");
    std::fs::write(&source, "Leicester is a city.").unwrap();
    let out = dir.path().join("chosen name");
    let (mut h, script) = window();
    script.files.borrow_mut().push_back(source);
    command(&mut h, Key::O);
    wait_for_status(&mut h, "note.txt is ready");
    script.files.borrow_mut().push_back(out.clone());
    command(&mut h, Key::S);
    assert!(status(&h).starts_with("Saving"), "{}", status(&h));
    assert_eq!(script.asked.borrow().last().unwrap(), "Save the speech as an MP3 file: note.mp3");
    wait_for_status(&mut h, "Saved chosen name.mp3");
    let saved = dir.path().join("chosen name.mp3");
    let pcm = crate::audio::decode(std::fs::read(saved).unwrap()).unwrap();
    assert!(pcm.samples.len() > pcm.rate as usize / 2);
    assert!(app(&h).job.is_none());
}


/// A held chord, saved as an MP3: sound, but not speech.
fn chord_mp3(dir: &std::path::Path) -> PathBuf {
    let path = dir.join("chord.mp3");
    let pcm = crate::audio::Pcm { samples: crate::transcribe::detect::tests::chord(3.0), rate: 16000 };
    crate::audio::save_mp3(&pcm, &path).unwrap();
    path
}

#[test]
fn the_keyboard_reaches_every_control_on_the_audio_player_in_order() {
    let (mut h, _) = window();
    command(&mut h, Key::Num4);
    assert_eq!(focused(&h).as_deref(), Some("Audio Player"));
    let stops = tab_stops(&mut h, Modifiers::NONE);
    let expected = [
        "Audio file to play",
        "Choose an audio file",
        "Play",
        "Pause",
        "Stop",
        "Transcribe",
        "Transcript",
        "Save the transcript",
        "General",
        "Settings",
        "Wordlist",
        "Audio Player",
    ];
    assert_eq!(stops.len(), expected.len(), "{stops:?}");
    for (stop, wanted) in stops.iter().zip(expected) {
        assert!(stop.contains(wanted), "{stop:?} should be {wanted:?}: {stops:?}");
    }
    h.check_accessibility();
}

#[test]
fn the_audio_player_says_what_it_needs_first() {
    let (mut h, _) = window();
    command(&mut h, Key::Num4);
    command(&mut h, Key::R);
    assert_eq!(status(&h), "Choose an audio file first.");
    command(&mut h, Key::P);
    assert_eq!(status(&h), "Nothing is playing.");
    command(&mut h, Key::S);
    assert_eq!(status(&h), "There's no transcript to save yet.");
    focus_on(&mut h, "Transcribe");
    press(&mut h, Key::Space);
    assert_eq!(status(&h), "Choose an audio file first.");
    focus_on(&mut h, "Audio file to play");
    type_text(&mut h, "notes.pdf");
    press(&mut h, Key::Enter);
    assert!(status(&h).contains("can't be played"), "{}", status(&h));
    assert_eq!(focused(&h).as_deref().map(|f| f.starts_with("Audio file to play")), Some(true));
}

#[test]
fn ctrl_o_on_the_audio_player_chooses_audio() {
    let (mut h, script) = window();
    command(&mut h, Key::Num4);
    command(&mut h, Key::O);
    assert_eq!(script.asked.borrow().last().unwrap(), "Choose an audio file to play");
    assert_eq!(app(&h).tab, Tab::AudioPlayer);
}

#[test]
fn music_is_not_transcribed_and_nothing_is_asked() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, script) = window();
    command(&mut h, Key::Num4);
    script.files.borrow_mut().push_back(chord_mp3(dir.path()));
    command(&mut h, Key::O);
    wait_for_status(&mut h, "won't be transcribed");
    assert!(status(&h).contains("chord.mp3 sounds mostly like music"), "{}", status(&h));
    assert_eq!(script.asked.borrow().len(), 1, "only the file was asked for: {:?}", script.asked.borrow());
    assert!(!app(&h).audio.is_transcribing());
}

/// Opens the chord, waits until it's heard, and presses Transcribe.
fn transcribe_the_chord(h: &mut Window, script: &Script, dir: &std::path::Path) {
    command(h, Key::Num4);
    script.files.borrow_mut().push_back(chord_mp3(dir));
    command(h, Key::O);
    wait_for_status(h, "won't be transcribed");
    focus_on(h, "Transcribe");
    press(h, Key::Space);
}

#[test]
fn a_missing_whisper_program_is_reported_with_feeling() {
    let dir = tempfile::tempdir().unwrap();
    let (mut h, script) = window();
    app_mut(&mut h).settings.whisper_program = Some(dir.path().join("gone").join("whisper-cli"));
    transcribe_the_chord(&mut h, &script, dir.path());
    let said = status(&h);
    assert!(said.contains("No transcript, because the whisper.cpp program chosen on the Settings tab"), "{said}");
    assert_eq!(status_live(&h), Live::Assertive);
    assert!(!app(&h).audio.is_transcribing());
}

#[test]
fn nothing_is_downloaded_without_asking() {
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("whisper-cli");
    std::fs::write(&program, b"").unwrap();
    let model = dir.path().join("model.bin");
    let (mut h, script) = window();
    app_mut(&mut h).settings.whisper_program = Some(program);
    app_mut(&mut h).audio.model = Some(model.clone());
    *script.confirm.borrow_mut() = false;
    transcribe_the_chord(&mut h, &script, dir.path());
    assert_eq!(script.asked.borrow().last().unwrap(), "Download the speech recognition model?");
    assert!(status(&h).contains("you said no to downloading"), "{}", status(&h));
    assert!(!app(&h).audio.is_transcribing());
    assert!(!model.exists());
}

/// The whole way through, with a stand-in for whisper.cpp that answers as it does.
#[cfg(unix)]
#[test]
fn transcribing_through_the_window() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let program = dir.path().join("whisper-cli");
    std::fs::write(
        &program,
        "#!/bin/sh\nout=''\nwhile [ $# -gt 0 ]; do [ \"$1\" = --output-file ] && out=\"$2\"; shift; done\n\
         echo 'whisper_print_progress_callback: progress =  50%' >&2\n\
         printf '%s' '{\"transcription\":[{\"offsets\":{\"from\":0,\"to\":1500},\"text\":\" Hello there.\"},\
         {\"offsets\":{\"from\":1500,\"to\":2900},\"text\":\" [BLANK_AUDIO]\"}]}' > \"$out.json\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
    let model = dir.path().join("model.bin");
    std::fs::write(&model, b"stand-in").unwrap();
    let (mut h, script) = window();
    app_mut(&mut h).settings.whisper_program = Some(program);
    app_mut(&mut h).audio.model = Some(model);
    transcribe_the_chord(&mut h, &script, dir.path());
    wait_for_status(&mut h, "Transcribed chord.mp3");
    assert_eq!(status(&h), "Transcribed chord.mp3: 5 words, 1 speaker.");
    assert_eq!(app(&h).audio.transcript(), "Speaker 1 (0:00): Hello there.");
    assert_eq!(script.asked.borrow().len(), 1, "nothing needed asking: {:?}", script.asked.borrow());

    let out = dir.path().join("words");
    script.files.borrow_mut().push_back(out.clone());
    command(&mut h, Key::S);
    assert_eq!(script.asked.borrow().last().unwrap(), "Save the transcript: chord transcript.txt");
    assert_eq!(std::fs::read_to_string(dir.path().join("words.txt")).unwrap(), "Speaker 1 (0:00): Hello there.");
}

/// Opening a new file stops the old file's transcription and may start one for
/// the new file, before the old one has said it stopped. What the old one says
/// then mustn't end, or show up as, the new one.
#[test]
fn a_stopped_transcription_cannot_end_the_next_one() {
    let (mut h, _) = window();
    let a = app_mut(&mut h);
    let old = a.audio.begin_transcription_for_test();
    let new = a.audio.begin_transcription_for_test();
    a.transcription_stage(old, "Downloading the speech recognition model.".to_owned());
    a.transcribed(old, Ok(None));
    assert!(a.audio.is_transcribing(), "the new transcription carries on");
    assert_ne!(a.status.text, "Stopped transcribing.");
    assert!(!a.status.text.starts_with("Downloading"));

    a.transcribed(new, Ok(Some("Speaker 1 (0:00): Hello there.".to_owned())));
    assert!(!a.audio.is_transcribing());
    assert!(a.status.text.starts_with("Transcribed"), "{}", a.status.text);
}

#[test]
fn audio_files_are_told_apart_from_documents() {
    assert!(audio_tab::is_audio(std::path::Path::new("a.MP3")));
    assert!(audio_tab::is_audio(std::path::Path::new("a.wav")));
    assert!(!audio_tab::is_audio(std::path::Path::new("a.pdf")));
    assert!(!audio_tab::is_audio(std::path::Path::new("mp3")));
}

impl App {
    fn handle_failure_for_test(&mut self, text: &str) {
        let (tx, rx) = channel();
        tx.send(Msg::Failed(text.to_owned())).unwrap();
        self.rx = rx;
        self.handle_messages();
    }
}

#[test]
fn a_path_box_and_its_button_share_a_line_until_the_text_is_too_large() {
    let (mut h, _) = window();
    let field = node_by_label(&h, "File to read").rect();
    let button = node_by_label(&h, "Choose a file").rect();
    assert_eq!(field.top(), button.top(), "{field:?} {button:?}");
    assert!(field.width() > button.width() * 2.0, "the box gets most of the room");
    h.set_size(egui::vec2(560.0, 1900.0));
    h.ctx.set_zoom_factor(2.0);
    h.run_steps(3);
    let field = node_by_label(&h, "File to read").rect();
    let button = node_by_label(&h, "Choose a file").rect();
    assert!(button.top() >= field.bottom() - 1.0, "the button goes underneath: {field:?} {button:?}");
}

impl App {
    fn update_checked_for_test(&mut self, result: Result<Option<crate::update::Version>, String>, requested: bool) {
        let (tx, rx) = channel();
        tx.send(Msg::Update(result, requested)).unwrap();
        self.rx = rx;
        self.handle_messages();
    }
}

#[test]
fn a_new_version_is_announced_and_linked_to() {
    let (mut h, _) = window();
    command(&mut h, Key::Num2);
    let links = |h: &Window| h.root().children_recursive().filter(|n| n.accesskit_node().role() == Role::Link).count();
    let before = links(&h);
    let newer = crate::update::Version::parse("999.0.0").unwrap();
    app_mut(&mut h).update_checked_for_test(Ok(Some(newer)), false);
    h.run_steps(2);
    assert!(status(&h).contains("Version 999.0.0"), "{}", status(&h));
    assert_eq!(status_live(&h), Live::Polite, "news, not an error");
    assert_eq!(links(&h), before + 1);
    focus_on(&mut h, "Download version 999.0.0 from GitHub");
    let key = |pressed| Event::Key { key: Key::Enter, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE };
    h.event(key(true));
    h.step();
    let opened = h.output().platform_output.commands.iter().find_map(|c| match c {
        egui::OutputCommand::OpenUrl(open) => Some(open.url.clone()),
        _ => None,
    });
    h.event(key(false));
    h.run_steps(2);
    assert_eq!(opened.as_deref(), Some(crate::update::RELEASES_URL));
}

#[test]
fn startup_checks_stay_quiet_unless_there_is_news() {
    let (mut h, _) = window();
    app_mut(&mut h).update_checked_for_test(Ok(None), false);
    app_mut(&mut h).update_checked_for_test(Err("GitHub could not be reached.".to_owned()), false);
    h.run_steps(2);
    assert_eq!(status(&h), "Ready.");
    // A problem already being shown isn't covered up by news.
    app_mut(&mut h).set_error("Your settings could not be read.");
    let newer = crate::update::Version::parse("999.0.0").unwrap();
    app_mut(&mut h).update_checked_for_test(Ok(Some(newer)), false);
    h.run_steps(2);
    assert_eq!(status(&h), "Your settings could not be read.");
}

#[test]
fn checking_by_hand_always_answers() {
    let (mut h, _) = window();
    app_mut(&mut h).update_checked_for_test(Ok(None), true);
    h.run_steps(2);
    assert!(status(&h).starts_with("You have the latest version"), "{}", status(&h));
    app_mut(&mut h).update_checked_for_test(Err("GitHub could not be reached.".to_owned()), true);
    h.run_steps(2);
    assert_eq!(status(&h), "Could not check for updates. GitHub could not be reached.");
    assert_eq!(status_live(&h), Live::Assertive);
}

#[test]
fn update_checks_can_be_turned_off_from_the_keyboard() {
    let (mut h, _) = window();
    assert!(saved_settings(&h).check_updates, "on unless turned off");
    command(&mut h, Key::Num2);
    focus_on(&mut h, "Check for updates when the app starts");
    press(&mut h, Key::ArrowDown);
    assert!(!saved_settings(&h).check_updates);
    focus_on(&mut h, "Check for updates now");
}
