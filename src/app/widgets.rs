//! The controls the tabs are built from, drawn so screen readers and the
//! keyboard work with them, and the window's fonts and colours.

use super::*;

// ----- widgets -----------------------------------------------------------

/// Font family used for the tab labels (Google Sans Bold).
pub(super) fn tab_font() -> egui::FontFamily {
    egui::FontFamily::Name("tab".into())
}

/// Google Sans (SIL Open Font License, see assets/fonts/OFL.txt): Medium for
/// all body text, Bold for the tabs. egui's built-in fonts stay behind them
/// as fallbacks for symbols and scripts Google Sans does not cover.
fn setup_fonts(ctx: &egui::Context) {
    use egui::{FontData, FontDefinitions, FontFamily};
    use std::sync::Arc;

    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "GoogleSans-Medium".into(),
        Arc::new(FontData::from_static(include_bytes!("../../assets/fonts/GoogleSans-Medium.ttf"))),
    );
    fonts.font_data.insert(
        "GoogleSans-Bold".into(),
        Arc::new(FontData::from_static(include_bytes!("../../assets/fonts/GoogleSans-Bold.ttf"))),
    );
    let fallbacks = fonts.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "GoogleSans-Medium".into());
    let mut tab = vec!["GoogleSans-Bold".to_owned()];
    tab.extend(fallbacks);
    fonts.families.insert(tab_font(), tab);
    ctx.set_fonts(fonts);
}

pub(super) fn setup_style(ctx: &egui::Context) {
    use egui::{FontId, TextStyle};
    setup_fonts(ctx);
    ctx.all_styles_mut(|style| {
        style.text_styles = [
            (TextStyle::Heading, FontId::proportional(24.0)),
            (TextStyle::Body, FontId::proportional(17.0)),
            (TextStyle::Button, FontId::proportional(17.0)),
            (TextStyle::Monospace, FontId::monospace(16.0)),
            (TextStyle::Small, FontId::proportional(14.0)),
        ]
        .into();
        style.spacing.button_padding = egui::vec2(10.0, 6.0);
        style.spacing.interact_size.y = CONTROL_HEIGHT;
    });
}

pub(super) fn error_color(ui: &Ui) -> Color32 {
    if ui.visuals().dark_mode { Color32::from_rgb(255, 140, 140) } else { Color32::from_rgb(170, 0, 0) }
}

/// A heading that screen readers list as a heading, so users can jump to it.
pub(super) fn heading(ui: &mut Ui, text: &str) {
    let resp = ui.heading(text);
    ui.ctx().accesskit_node_builder(resp.id, |node| {
        node.set_role(Role::Heading);
        node.set_level(2);
        // egui stores a plain label's text as its value; headings need a name.
        node.set_label(text);
    });
}

pub(super) fn full_button(ui: &mut Ui, text: &str, enabled: bool) -> egui::Response {
    ui.add_enabled_ui(enabled, |ui| ui.add_sized([ui.available_width(), CONTROL_HEIGHT], Button::new(text)))
        .inner
}

pub(super) fn text_field(ui: &mut Ui, label: &str, value: &mut String, editable: bool) -> egui::Response {
    let label = ui.label(label);
    let resp = if editable {
        let edit = egui::TextEdit::singleline(value).desired_width(f32::INFINITY);
        ui.add_sized([ui.available_width(), CONTROL_HEIGHT], edit)
    } else {
        // Read-only, but still focusable so a screen reader can review it.
        let mut text = value.as_str();
        let edit = egui::TextEdit::singleline(&mut text).desired_width(f32::INFINITY);
        ui.add_sized([ui.available_width(), CONTROL_HEIGHT], edit)
    };
    let resp = resp.labelled_by(label.id);
    if !editable {
        ui.ctx().accesskit_node_builder(resp.id, |node| node.set_read_only());
    }
    resp
}

/// The box for typing or pasting text to read aloud. Tab and Shift+Tab move
/// out of it rather than typing a tab, so keyboard users are never trapped.
pub(super) fn paste_box(ui: &mut Ui, label: &str, value: &mut String, enabled: bool) -> egui::Response {
    let label = ui.label(label);
    let edit = egui::TextEdit::multiline(value)
        .id_salt("pasted_text")
        .lock_focus(false)
        .char_limit(crate::document::MAX_PASTED_CHARS)
        .desired_width(f32::INFINITY)
        .desired_rows(10);
    ui.add_enabled(enabled, edit).labelled_by(label.id)
}

/// A read-only box of several lines that a screen reader can review.
pub(super) fn text_area(ui: &mut Ui, label: &str, value: &str) {
    let label = ui.label(label);
    let mut text = value;
    let edit = egui::TextEdit::multiline(&mut text).desired_width(f32::INFINITY).desired_rows(10);
    let resp = ui.add(edit).labelled_by(label.id);
    ui.ctx().accesskit_node_builder(resp.id, |node| node.set_read_only());
}

/// Draws the waveform of an audio file, with the part already played in the
/// highlight colour and a line where playback is. Screen readers get it as an
/// image named `description`.
pub(super) fn waveform(ui: &mut Ui, peaks: &[f32], played: f32, description: &str) {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 72.0), egui::Sense::hover());
    let visuals = ui.visuals();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
    let bar_width = (rect.width() / peaks.len().max(1) as f32).max(1.0);
    let middle = rect.center().y;
    for (i, peak) in peaks.iter().enumerate() {
        let at = (i as f32 + 0.5) / peaks.len() as f32;
        let x = rect.left() + at * rect.width();
        let half = (peak * rect.height() / 2.0 * 0.9).max(0.5);
        let colour = if at <= played { visuals.selection.bg_fill } else { visuals.weak_text_color() };
        let bar = [egui::pos2(x, middle - half), egui::pos2(x, middle + half)];
        painter.line_segment(bar, egui::Stroke::new(bar_width, colour));
    }
    let x = rect.left() + played * rect.width();
    let playhead = [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())];
    painter.line_segment(playhead, egui::Stroke::new(2.0, visuals.text_color()));
    ui.ctx().accesskit_node_builder(resp.id, |node| {
        node.set_role(Role::Image);
        node.set_label(description);
    });
}

/// The picture from a video being described, as large as fits the width in
/// a 16:9 frame, or `none` written in the frame while there isn't one.
/// Screen readers get it as an image named after the picture.
pub(super) fn video_preview(ui: &mut Ui, preview: Option<&(egui::TextureHandle, String)>, none: &str) {
    let width = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(width, (width * 9.0 / 16.0).min(360.0)), egui::Sense::hover());
    let visuals = ui.visuals();
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 4.0, visuals.extreme_bg_color);
    let label = match preview {
        Some((texture, label)) => {
            let size = texture.size_vec2();
            let scale = (rect.width() / size.x).min(rect.height() / size.y);
            let shown = egui::Rect::from_center_size(rect.center(), size * scale);
            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
            painter.image(texture.id(), shown, uv, Color32::WHITE);
            label.as_str()
        }
        None => {
            let font = egui::TextStyle::Body.resolve(ui.style());
            painter.text(rect.center(), egui::Align2::CENTER_CENTER, none, font, visuals.weak_text_color());
            none
        }
    };
    ui.ctx().accesskit_node_builder(resp.id, |node| {
        node.set_role(Role::Image);
        node.set_label(label);
    });
}

pub(super) fn password_field(ui: &mut Ui, label: &str, value: &mut String) {
    let label = ui.label(label);
    ui.add_sized(
        [ui.available_width(), CONTROL_HEIGHT],
        egui::TextEdit::singleline(value).password(true).desired_width(f32::INFINITY),
    )
    .labelled_by(label.id);
}

/// A labelled, full-width dropdown. While it has focus and is closed, the Up,
/// Down, Home and End keys change the choice directly, as in a native combo
/// box; Space, Enter or Alt+Down (Option+Down on a Mac) opens the list, and
/// Alt+Up or Escape closes it.
pub(super) fn dropdown(ui: &mut Ui, id_salt: &str, label: &str, options: &[String], selected: &mut usize, enabled: bool) -> bool {
    let label_resp = ui.label(label);
    let mut changed = false;
    let current = options.get(*selected).cloned().unwrap_or_default();
    let inner = ui.add_enabled_ui(enabled, |ui| {
        egui::ComboBox::from_id_salt(id_salt)
            .width(ui.available_width())
            .height(400.0)
            .selected_text(current.clone())
            .show_ui(ui, |ui| {
                for (i, option) in options.iter().enumerate() {
                    let resp = ui.add_sized(
                        [ui.available_width(), CONTROL_HEIGHT],
                        Button::selectable(i == *selected, option.as_str()),
                    );
                    if resp.clicked() {
                        *selected = i;
                        changed = true;
                    }
                }
            })
    });
    let combo = inner.inner;
    let resp = combo.response.labelled_by(label_resp.id);
    let popup_open = combo.inner.is_some();
    // Where egui keeps whether this dropdown's list is open.
    let popup_id = resp.id.with("popup");

    if enabled && popup_open && ui.input_mut(|i| consume_exact(i, Modifiers::ALT, Key::ArrowUp)) {
        egui::Popup::close_id(ui.ctx(), popup_id);
    }
    if enabled && resp.has_focus() && !popup_open && !options.is_empty() {
        ui.memory_mut(|m| {
            m.set_focus_lock_filter(resp.id, EventFilter { vertical_arrows: true, ..Default::default() })
        });
        if ui.input_mut(|i| consume_exact(i, Modifiers::ALT, Key::ArrowDown)) {
            egui::Popup::open_id(ui.ctx(), popup_id);
        }
        let last = options.len() - 1;
        let new = ui.input_mut(|i| {
            if consume_exact(i, Modifiers::NONE, Key::ArrowDown) {
                Some((*selected + 1).min(last))
            } else if consume_exact(i, Modifiers::NONE, Key::ArrowUp) {
                Some(selected.saturating_sub(1))
            } else if consume_exact(i, Modifiers::NONE, Key::Home) {
                Some(0)
            } else if consume_exact(i, Modifiers::NONE, Key::End) {
                Some(last)
            } else {
                None
            }
        });
        if let Some(new) = new
            && new != *selected {
                *selected = new;
                changed = true;
                ui.ctx().request_repaint();
            }
    }
    let value = options.get(*selected).cloned().unwrap_or_default();
    ui.ctx().accesskit_node_builder(resp.id, |node| {
        node.set_role(Role::ComboBox);
        node.set_value(value);
    });
    changed
}

/// Takes a press of `key` with exactly `modifiers` out of the input. egui's
/// own `consume_key` ignores extra Shift and Alt, so Alt+Down would count as
/// Down.
fn consume_exact(input: &mut egui::InputState, modifiers: Modifiers, key: Key) -> bool {
    let before = input.events.len();
    input.events.retain(|e| {
        !matches!(e, egui::Event::Key { key: k, pressed: true, modifiers: m, .. } if *k == key && m.matches_exact(modifiers))
    });
    input.events.len() != before
}

/// Draws a thick, high-contrast outline around the focused control.
pub(super) fn draw_focus_ring(ctx: &egui::Context) {
    let Some(id) = ctx.memory(|m| m.focused()) else { return };
    let Some(resp) = ctx.read_response(id) else { return };
    let color = if ctx.global_style().visuals.dark_mode {
        Color32::from_rgb(255, 215, 0)
    } else {
        Color32::from_rgb(0, 70, 200)
    };
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, Id::new("focus_ring")));
    painter.rect_stroke(
        resp.rect.expand(3.0),
        4.0,
        egui::Stroke::new(3.0, color),
        egui::StrokeKind::Outside,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, RawInput};
    use std::collections::BTreeMap;

    fn press(key: Key, modifiers: Modifiers) -> Event {
        Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers }
    }

    /// Runs one frame of `ui_fn` with `events` as the input.
    fn frame(ctx: &egui::Context, events: Vec<Event>, ui_fn: &mut impl FnMut(&mut Ui)) {
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0));
        let input = RawInput { events, screen_rect: Some(screen), ..Default::default() };
        let mut output = ctx.run_ui(input, |ui| ui_fn(ui));
        output.textures_delta.clear();
    }

    fn focused(ctx: &egui::Context) -> Option<Id> {
        ctx.memory(|m| m.focused())
    }

    /// Presses Tab and lets egui move focus.
    fn tab(ctx: &egui::Context, ui_fn: &mut impl FnMut(&mut Ui)) {
        frame(ctx, vec![press(Key::Tab, Modifiers::NONE)], ui_fn);
        frame(ctx, vec![], ui_fn);
    }

    #[test]
    fn dropdown_keys() {
        let ctx = egui::Context::default();
        let options: Vec<String> = ["One", "Two", "Three"].map(String::from).to_vec();
        let mut selected = 0;
        let mut changes = 0;
        let mut ui_fn = |ui: &mut Ui| {
            if dropdown(ui, "test", "Number", &options, &mut selected, true) {
                changes += 1;
            }
        };
        frame(&ctx, vec![], &mut ui_fn);
        tab(&ctx, &mut ui_fn);
        assert!(focused(&ctx).is_some(), "Tab moves focus to the dropdown");

        frame(&ctx, vec![press(Key::ArrowDown, Modifiers::NONE)], &mut ui_fn);
        frame(&ctx, vec![press(Key::End, Modifiers::NONE)], &mut ui_fn);
        frame(&ctx, vec![press(Key::ArrowUp, Modifiers::NONE)], &mut ui_fn);
        // Shift+Down is not Down.
        frame(&ctx, vec![press(Key::ArrowDown, Modifiers::SHIFT)], &mut ui_fn);
        assert_eq!((selected, changes), (1, 3));

        let mut ui_fn = |ui: &mut Ui| {
            dropdown(ui, "test", "Number", &options, &mut selected, true);
        };
        // Alt+Down opens the list without changing the choice; Alt+Up closes it.
        frame(&ctx, vec![press(Key::ArrowDown, Modifiers::ALT)], &mut ui_fn);
        frame(&ctx, vec![], &mut ui_fn);
        assert!(egui::Popup::is_any_open(&ctx), "Alt+Down opens the list");
        frame(&ctx, vec![press(Key::ArrowUp, Modifiers::ALT)], &mut ui_fn);
        frame(&ctx, vec![], &mut ui_fn);
        assert!(!egui::Popup::is_any_open(&ctx), "Alt+Up closes the list");
        assert_eq!(selected, 1);
    }

    #[test]
    fn tab_leaves_the_paste_box() {
        let ctx = egui::Context::default();
        let mut text = String::new();
        let boxed = std::cell::Cell::new(None);
        let mut ui_fn = |ui: &mut Ui| {
            boxed.set(Some(paste_box(ui, "Text", &mut text, true).id));
            let _ = ui.button("After");
        };
        frame(&ctx, vec![], &mut ui_fn);
        tab(&ctx, &mut ui_fn);
        let box_id = boxed.get().unwrap();
        assert_eq!(focused(&ctx), Some(box_id));

        frame(&ctx, vec![Event::Text("Hello".into())], &mut ui_fn);
        tab(&ctx, &mut ui_fn);
        assert_ne!(focused(&ctx), Some(box_id), "Tab moves on instead of typing a tab");
        assert!(focused(&ctx).is_some());
        assert_eq!(text, "Hello");
    }

    #[test]
    fn escape_closes_a_list_and_keeps_focus() {
        let ctx = egui::Context::default();
        let keymap = Keymap::new(true, &BTreeMap::new(), Platform::Windows);
        let options: Vec<String> = ["One", "Two"].map(String::from).to_vec();
        let mut selected = 0;
        let mut ui_fn = |ui: &mut Ui| {
            dropdown(ui, "test", "Number", &options, &mut selected, true);
        };
        frame(&ctx, vec![], &mut ui_fn);
        tab(&ctx, &mut ui_fn);
        let combo = focused(&ctx).expect("the dropdown has focus");
        frame(&ctx, vec![press(Key::ArrowDown, Modifiers::ALT)], &mut ui_fn);
        frame(&ctx, vec![], &mut ui_fn);
        assert!(egui::Popup::is_any_open(&ctx));

        // As `take_shortcuts` does, with something running so Escape could
        // also stop: closing the list comes first.
        let cx = KeyContext { popup_open: egui::Popup::is_any_open(&ctx), tab_count: 4, ..KeyContext::default() };
        let mut events = vec![press(Key::Escape, Modifiers::NONE)];
        assert_eq!(keymap.take(&mut events, cx, |_| true), vec![Command::CloseList]);
        egui::Popup::close_all(&ctx);
        frame(&ctx, events, &mut ui_fn);
        frame(&ctx, vec![], &mut ui_fn);
        assert!(!egui::Popup::is_any_open(&ctx), "the list is closed");
        assert_eq!(focused(&ctx), Some(combo), "focus stays on the dropdown");
    }

    #[test]
    fn escape_keeps_focus_and_shortcuts_skip_text() {
        let ctx = egui::Context::default();
        let keymap = Keymap::new(true, &BTreeMap::new(), Platform::Windows);
        let cx = KeyContext { tab_count: 4, ..KeyContext::default() };
        let mut text = String::new();
        let mut ui_fn = |ui: &mut Ui| {
            paste_box(ui, "Text", &mut text, true);
        };
        frame(&ctx, vec![], &mut ui_fn);
        tab(&ctx, &mut ui_fn);
        let box_id = focused(&ctx).expect("the box has focus");

        // Nothing is running, so Escape doesn't stop anything, and is taken
        // out before egui would drop focus.
        let mut events = vec![press(Key::Escape, Modifiers::NONE)];
        assert!(keymap.take(&mut events, cx, |a| a != Action::Stop).is_empty());
        frame(&ctx, events, &mut ui_fn);
        assert_eq!(focused(&ctx), Some(box_id));

        // Typing goes into the box even with shortcuts on, including AltGr
        // characters, while the shortcut keys themselves are taken.
        let altgr = Modifiers { alt: true, ctrl: true, command: true, ..Modifiers::NONE };
        let mut events =
            vec![press(Key::S, altgr), Event::Text("ś".into()), press(Key::F5, Modifiers::NONE), Event::Text("h".into())];
        let commands = keymap.take(&mut events, cx, |_| true);
        assert_eq!(commands, vec![Command::Run(Action::Read)]);
        frame(&ctx, events, &mut ui_fn);
        assert_eq!(text, "śh");
    }
}
