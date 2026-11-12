//! The controls the tabs are built from. Each takes a whole line and the whole
//! width of the pane, apart from a row of buttons, which share one. All can be
//! reached and used from the keyboard, and have names that screen readers can say.

use egui::accesskit::Role;
use egui::{Button, Color32, EventFilter, Key, KeyboardShortcut, Modifiers, Response, Ui};

pub const CONTROL_HEIGHT: f32 = 36.0;

/// A heading that screen readers list as a heading, so users can jump to it.
pub fn heading(ui: &mut Ui, text: &str, level: usize) {
    let resp = if level <= 2 { ui.heading(text) } else { ui.label(egui::RichText::new(text).strong().size(18.0)) };
    ui.ctx().accesskit_node_builder(resp.id, |node| {
        node.set_role(Role::Heading);
        node.set_level(level);
        // egui stores a plain label's text as its value; headings need a name.
        node.set_label(text);
    });
}

/// A button filling the line. `salt` keeps its identity, and so the keyboard
/// focus, when its label or the controls above it change. A button is never
/// greyed out, because a keyboard can't reach a greyed-out button and a screen
/// reader user wouldn't know it existed. Pressing one that can't work yet says why.
pub fn full_button(ui: &mut Ui, salt: &str, text: &str, shortcut: Option<&KeyboardShortcut>) -> Response {
    stable_scope(ui, salt, |ui| {
        let resp = ui.add_sized([ui.available_width(), CONTROL_HEIGHT], Button::new(text));
        match shortcut {
            Some(shortcut) => {
                let shortcut = ui.ctx().format_shortcut(shortcut);
                ui.ctx().accesskit_node_builder(resp.id, |node| node.set_keyboard_shortcut(shortcut.clone()));
                resp.on_hover_text(shortcut)
            }
            None => resp,
        }
    })
}

/// One button in a [`button_row`].
pub struct RowButton<'a, A> {
    pub action: A,
    /// Keeps the button's identity, and so the focus, when its label changes.
    pub salt: &'a str,
    pub text: &'a str,
    pub shortcut: Option<&'a KeyboardShortcut>,
}

/// How wide each button in a row should be: an equal share if every label fits
/// that way, or else each as wide as its label plus an equal share of what is
/// left. Only when even that is too wide (a narrow window, or large text) do
/// they go back to equal shares, with the labels wrapping onto more lines.
fn row_widths(natural: &[f32], room: f32) -> Vec<f32> {
    let count = natural.len().max(1) as f32;
    let widest = natural.iter().copied().fold(0.0, f32::max);
    let total: f32 = natural.iter().sum();
    if widest * count > room && total <= room {
        natural.iter().map(|w| w + (room - total) / count).collect()
    } else {
        vec![room / count; natural.len()]
    }
}

/// Buttons side by side on one line that fills the width, with each label
/// centred. If there isn't room for even their longest words that way, as with
/// very large text, they go one above another instead, so that no word is broken.
/// Tab moves through them in order. Returns the action of the button pressed, if any.
pub fn button_row<A: Copy>(ui: &mut Ui, buttons: &[RowButton<'_, A>]) -> Option<A> {
    let spacing = ui.spacing().item_spacing.x;
    let count = buttons.len().max(1) as f32;
    let room = (ui.available_width() - spacing * (count - 1.0)).max(count);
    let padding = egui::vec2(8.0, ui.spacing().button_padding.y);
    let font = egui::TextStyle::Button.resolve(ui.style());
    let layout = |ui: &Ui, text: &str, wrap_width: f32| {
        let mut job = egui::text::LayoutJob::simple(text.to_owned(), font.clone(), Color32::PLACEHOLDER, wrap_width);
        job.halign = egui::Align::Center;
        ui.fonts_mut(|f| f.layout_job(job))
    };
    let natural: Vec<f32> = buttons.iter().map(|b| layout(ui, b.text, f32::INFINITY).size().x + 2.0 * padding.x).collect();
    let longest_word = buttons
        .iter()
        .flat_map(|b| b.text.split_whitespace())
        .map(|word| layout(ui, word, f32::INFINITY).size().x + 2.0 * padding.x)
        .fold(0.0, f32::max);
    let side_by_side = longest_word * count <= room;
    let widths = if side_by_side { row_widths(&natural, room) } else { vec![ui.available_width(); buttons.len()] };
    let galleys: Vec<_> =
        buttons.iter().zip(&widths).map(|(b, width)| layout(ui, b.text, (width - 2.0 * padding.x).max(1.0))).collect();
    let tallest = galleys.iter().map(|g| g.size().y).fold(0.0, f32::max);
    let height = (tallest + 2.0 * padding.y).max(CONTROL_HEIGHT);
    let gap = ui.spacing().item_spacing.y;
    let total_height = if side_by_side { height } else { (height + gap) * count - gap };

    let (area, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), total_height), egui::Sense::hover());
    let mut clicked = None;
    let mut corner = area.min;
    for ((button, galley), width) in buttons.iter().zip(galleys).zip(widths) {
        let rect = egui::Rect::from_min_size(corner, egui::vec2(width, height));
        if side_by_side {
            corner.x += width + spacing;
        } else {
            corner.y += height + gap;
        }
        let resp = ui.interact(rect, ui.scope_id().with(button.salt), egui::Sense::click());
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, ui.is_enabled(), button.text));
        if ui.is_rect_visible(rect) {
            let visuals = ui.style().interact(&resp);
            ui.painter().rect(rect, visuals.corner_radius, visuals.weak_bg_fill, visuals.bg_stroke, egui::StrokeKind::Inside);
            // A centred galley's lines are laid out around x = 0.
            let position = egui::pos2(rect.center().x, rect.center().y - galley.size().y / 2.0);
            ui.painter().galley(position, galley, visuals.text_color());
        }
        let resp = match button.shortcut {
            Some(shortcut) => {
                let shortcut = ui.ctx().format_shortcut(shortcut);
                ui.ctx().accesskit_node_builder(resp.id, |node| node.set_keyboard_shortcut(shortcut.clone()));
                resp.on_hover_text(shortcut)
            }
            None => resp,
        };
        if resp.clicked() {
            clicked = Some(button.action);
        }
    }
    clicked
}

/// Runs `add` so that the ids of what it adds depend only on `salt`. egui bases
/// a widget's id on how many came before it, even inside `push_id`, so without
/// this a control loses the keyboard focus whenever one above it comes or goes.
pub fn stable_scope<R>(ui: &mut Ui, salt: &str, add: impl FnOnce(&mut Ui) -> R) -> R {
    let id = ui.scope_id().with(salt);
    ui.scope_builder(egui::UiBuilder::new().scope_id(id), add).inner
}

/// A text box's frame, with an edge that can be seen and a filled background,
/// including for read-only boxes, which egui would otherwise leave invisible.
fn field_frame(ui: &Ui) -> egui::Frame {
    let visuals = ui.visuals();
    egui::Frame::new()
        .fill(visuals.text_edit_bg_color())
        .stroke(visuals.widgets.inactive.bg_stroke)
        .corner_radius(3.0)
        .inner_margin(6.0)
}

fn single_line_field(ui: &mut Ui, label: &str, salt: &str, value: &mut String, password: bool) -> Response {
    let label = ui.label(label);
    let edit = egui::TextEdit::singleline(value).id_salt(salt).password(password).frame(field_frame(ui)).desired_width(f32::INFINITY);
    ui.add_sized([ui.available_width(), CONTROL_HEIGHT], edit).labelled_by(label.id)
}

/// A labelled text box on its own line.
pub fn text_field(ui: &mut Ui, label: &str, salt: &str, value: &mut String) -> Response {
    single_line_field(ui, label, salt, value, false)
}

/// A labelled text box whose contents are hidden, on screen and from screen readers.
pub fn password_field(ui: &mut Ui, label: &str, salt: &str, value: &mut String) -> Response {
    single_line_field(ui, label, salt, value, true)
}

/// What [`path_field`] reports.
pub struct PathField {
    pub field: Response,
    /// The button beside it was pressed.
    pub choose: bool,
}

/// A labelled box for a file or folder's path with its Choose button beside it
/// on the same line. When there isn't room for both, as with very large text,
/// the button goes underneath instead. Tab goes from the box to the button.
pub fn path_field(
    ui: &mut Ui,
    label: &str,
    salt: &str,
    value: &mut String,
    button: &str,
    shortcut: Option<&KeyboardShortcut>,
) -> PathField {
    stable_scope(ui, salt, |ui| {
        let label = ui.label(label);
        let font = egui::TextStyle::Button.resolve(ui.style());
        let text_width = ui.fonts_mut(|f| f.layout_no_wrap(button.to_owned(), font, Color32::PLACEHOLDER).size().x);
        let button_width = text_width + 2.0 * ui.spacing().button_padding.x + 4.0;
        let gap = ui.spacing().item_spacing.x;
        let field_width = ui.available_width() - button_width - gap;
        let frame = field_frame(ui);
        fn edit<'a>(value: &'a mut String, salt: &str, frame: egui::Frame) -> egui::TextEdit<'a> {
            egui::TextEdit::singleline(value).id_salt(salt).frame(frame).desired_width(f32::INFINITY)
        }
        // Below this the box would be too narrow to see a path in.
        let side_by_side = field_width >= 200.0;
        let (field, choose) = if side_by_side {
            ui.horizontal(|ui| {
                let field = ui.add_sized([field_width, CONTROL_HEIGHT], edit(value, salt, frame)).labelled_by(label.id);
                let choose = full_button(ui, "choose", button, shortcut).clicked();
                (field, choose)
            })
            .inner
        } else {
            let field = ui.add_sized([ui.available_width(), CONTROL_HEIGHT], edit(value, salt, frame)).labelled_by(label.id);
            (field, full_button(ui, "choose", button, shortcut).clicked())
        };
        PathField { field, choose }
    })
}

/// A progress bar filling the line, with its text centred. Screen readers get
/// it as a progress indicator named `label`, with the text as its value.
/// `None` means how far it has got isn't known, so the bar is animated instead.
pub fn progress_bar(ui: &mut Ui, salt: &str, label: &str, fraction: Option<f32>, text: &str) {
    let bar = egui::ProgressBar::new(fraction.unwrap_or(0.0))
        .desired_width(ui.available_width())
        .desired_height(24.0)
        .animate(fraction.is_none());
    let resp = stable_scope(ui, salt, |ui| ui.add(bar));
    if ui.is_rect_visible(resp.rect) {
        let galley =
            egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Truncate), resp.rect.width() - 8.0, egui::TextStyle::Button);
        let position = resp.rect.center() - galley.size() / 2.0;
        // The colour egui gives a progress bar's own text, which shows on both
        // the filled and the empty part.
        let colour = ui.visuals().override_text_color.unwrap_or(ui.visuals().selection.stroke.color);
        ui.painter().with_clip_rect(resp.rect).galley(position, galley, colour);
    }
    let text = text.to_owned();
    ui.ctx().accesskit_node_builder(resp.id, |node| {
        node.set_role(Role::ProgressIndicator);
        node.set_label(label);
        node.set_value(text);
        if let Some(f) = fraction {
            node.set_min_numeric_value(0.0);
            node.set_max_numeric_value(100.0);
            node.set_numeric_value(f64::from((f * 100.0).round()));
        }
    });
}

/// A read-only box of several lines that the keyboard and a screen reader can
/// go through. Tab moves on from it, so nobody is trapped inside.
pub fn text_area(ui: &mut Ui, label: &str, value: &str) -> Response {
    let label = ui.label(label);
    let mut text = value;
    let edit = egui::TextEdit::multiline(&mut text)
        .id_salt("text_area")
        .lock_focus(false)
        .frame(field_frame(ui))
        .desired_width(f32::INFINITY)
        .desired_rows(10);
    let resp = ui.add(edit).labelled_by(label.id);
    ui.ctx().accesskit_node_builder(resp.id, |node| node.set_read_only());
    resp
}

/// A labelled box of several lines to type into, filling the width of the pane.
/// Tab moves on from it, so nobody is trapped inside.
pub fn editable_text_area(ui: &mut Ui, label: &str, salt: &str, value: &mut String) -> Response {
    let label = ui.label(label);
    let edit = egui::TextEdit::multiline(value)
        .id_salt(salt)
        .lock_focus(false)
        .frame(field_frame(ui))
        .desired_width(f32::INFINITY)
        .desired_rows(12);
    ui.add(edit).labelled_by(label.id)
}

/// A checkbox whose whole line can be clicked. egui's own only reaches as far
/// as its text, which would break the line of full-width controls.
pub fn full_checkbox(ui: &mut Ui, salt: &str, checked: &mut bool, text: &str) -> Response {
    let width = ui.available_width();
    let box_size = ui.spacing().icon_width;
    let gap = ui.spacing().icon_spacing;
    let wrap = (width - box_size - gap * 2.0).max(1.0);
    let galley = egui::WidgetText::from(text).into_galley(ui, Some(egui::TextWrapMode::Wrap), wrap, egui::TextStyle::Body);
    let height = (galley.size().y + 12.0).max(CONTROL_HEIGHT);
    let id = ui.scope_id().with(salt);
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
    let mut resp = ui.interact(rect, id, egui::Sense::click());
    if resp.clicked() {
        *checked = !*checked;
        resp.mark_changed();
    }
    let state = *checked;
    resp.widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), state, text));
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&resp);
        ui.painter().rect(rect, visuals.corner_radius, visuals.weak_bg_fill, visuals.bg_stroke, egui::StrokeKind::Inside);
        let square = egui::Rect::from_center_size(
            egui::pos2(rect.left() + gap + box_size / 2.0, rect.center().y),
            egui::vec2(box_size, box_size),
        );
        ui.painter().rect(square, 2.0, visuals.bg_fill, visuals.fg_stroke, egui::StrokeKind::Inside);
        if state {
            let (l, c, r) = (square.left_center() + egui::vec2(3.0, 0.0), square.center_bottom() - egui::vec2(1.0, 3.0), square.right_top() + egui::vec2(-3.0, 3.0));
            ui.painter().line(vec![l, c, r], egui::Stroke::new(2.0, visuals.fg_stroke.color));
        }
        let position = egui::pos2(square.right() + gap, rect.center().y - galley.size().y / 2.0);
        ui.painter().galley(position, galley, visuals.text_color());
    }
    resp
}

/// A labelled dropdown filling the line. While it has focus and is closed, the
/// Up, Down, Home and End keys change the choice directly, and typing a letter
/// jumps to the next choice starting with it, as in a native combo box. Space,
/// Enter or Alt+Down (Option+Down on a Mac) opens the list, and Alt+Up or Escape
/// closes it. Returns whether the choice changed.
pub fn dropdown(ui: &mut Ui, salt: &str, label: &str, options: &[String], selected: &mut usize) -> bool {
    let label_resp = ui.label(label);
    let mut changed = false;
    let current = options.get(*selected).cloned().unwrap_or_default();
    let combo = egui::ComboBox::from_id_salt(salt)
        .width(ui.available_width())
        .height(400.0)
        .selected_text(current)
        .show_ui(ui, |ui| {
            for (i, option) in options.iter().enumerate() {
                let resp = ui.add_sized([ui.available_width(), CONTROL_HEIGHT], Button::selectable(i == *selected, option.as_str()));
                if resp.clicked() {
                    *selected = i;
                    changed = true;
                }
            }
        });
    let resp = combo.response.labelled_by(label_resp.id);
    let popup_open = combo.inner.is_some();
    // Where egui keeps whether this dropdown's list is open.
    let popup_id = resp.id.with("popup");

    // However the list closes, the focus goes back to the dropdown, in case it
    // had moved into the list, so the keyboard carries on from the same place.
    let was_open_id = resp.id.with("was_open");
    let was_open = ui.data_mut(|d| d.get_temp::<bool>(was_open_id)).unwrap_or(false);
    ui.data_mut(|d| d.insert_temp(was_open_id, popup_open));
    if was_open && !popup_open {
        resp.request_focus();
    }

    // egui only closes the list itself for a pointer click, not Enter or Space.
    if popup_open && (changed || ui.input_mut(|i| consume_exact(i, Modifiers::ALT, Key::ArrowUp))) {
        egui::Popup::close_id(ui.ctx(), popup_id);
    }
    if resp.has_focus() && !popup_open && !options.is_empty() {
        ui.memory_mut(|m| m.set_focus_lock_filter(resp.id, EventFilter { vertical_arrows: true, ..Default::default() }));
        if ui.input_mut(|i| consume_exact(i, Modifiers::ALT, Key::ArrowDown)) {
            egui::Popup::open_id(ui.ctx(), popup_id);
        }
        if let Some(new) = keyboard_choice(ui, options, *selected)
            && new != *selected
        {
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

/// The choice the keys pressed this frame move a closed dropdown to.
fn keyboard_choice(ui: &mut Ui, options: &[String], selected: usize) -> Option<usize> {
    let last = options.len() - 1;
    ui.input_mut(|i| {
        if consume_exact(i, Modifiers::NONE, Key::ArrowDown) {
            return Some((selected + 1).min(last));
        }
        if consume_exact(i, Modifiers::NONE, Key::ArrowUp) {
            return Some(selected.saturating_sub(1));
        }
        if consume_exact(i, Modifiers::NONE, Key::Home) {
            return Some(0);
        }
        if consume_exact(i, Modifiers::NONE, Key::End) {
            return Some(last);
        }
        let typed = i.events.iter().find_map(|e| match e {
            egui::Event::Text(text) => text.chars().next().filter(|c| !c.is_whitespace()),
            _ => None,
        })?;
        next_starting_with(options, selected, typed)
    })
}

/// The next option after `selected` that starts with `letter`, going round.
fn next_starting_with(options: &[String], selected: usize, letter: char) -> Option<usize> {
    let wanted = letter.to_lowercase().next()?;
    (1..=options.len())
        .map(|step| (selected + step) % options.len())
        .find(|&i| options[i].chars().next().and_then(|c| c.to_lowercase().next()) == Some(wanted))
}

/// Takes a press of `key` with exactly `modifiers` out of the input. egui's own
/// `consume_key` ignores extra Shift and Alt, so Alt+Down would count as Down.
fn consume_exact(input: &mut egui::InputState, modifiers: Modifiers, key: Key) -> bool {
    let before = input.events.len();
    input.events.retain(|e| {
        !matches!(e, egui::Event::Key { key: k, pressed: true, modifiers: m, .. } if *k == key && m.matches_exact(modifiers))
    });
    input.events.len() != before
}

/// Draws a thick, high-contrast outline around the focused control, in both
/// appearances, so people who can see can always tell where the keyboard is.
pub fn draw_focus_ring(ctx: &egui::Context) {
    let Some(id) = ctx.memory(|m| m.focused()) else { return };
    let Some(resp) = ctx.read_response(id) else { return };
    let colour = if ctx.global_style().visuals.dark_mode {
        Color32::from_rgb(255, 215, 0)
    } else {
        Color32::from_rgb(0, 70, 200)
    };
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("focus_ring")));
    painter.rect_stroke(resp.rect.expand(2.0), 4.0, egui::Stroke::new(3.0, colour), egui::StrokeKind::Outside);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_widths_share_the_room() {
        // Everything fits in an equal share.
        assert_eq!(row_widths(&[50.0, 100.0], 300.0), [150.0, 150.0]);
        // The long label wouldn't, so each gets its own width and a share of the rest.
        assert_eq!(row_widths(&[50.0, 150.0], 240.0), [70.0, 170.0]);
        // Even that is too wide, so labels will wrap in equal shares.
        assert_eq!(row_widths(&[150.0, 150.0], 200.0), [100.0, 100.0]);
    }

    #[test]
    fn typing_a_letter_goes_round_the_choices() {
        let options: Vec<String> = ["Alice", "Bob", "albert", "Carol"].map(String::from).to_vec();
        assert_eq!(next_starting_with(&options, 0, 'a'), Some(2));
        assert_eq!(next_starting_with(&options, 2, 'A'), Some(0));
        assert_eq!(next_starting_with(&options, 0, 'c'), Some(3));
        assert_eq!(next_starting_with(&options, 0, 'z'), None);
    }
}
