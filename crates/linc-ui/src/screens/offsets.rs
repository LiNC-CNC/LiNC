//! Work offsets G54-G59.3 and tool offsets.

use egui::{FontId, RichText, Ui};
use linc_core::command::MachineCommand;
use linc_core::model::AxisMap;

use crate::screens::common;
use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    // No page title: the navigation names the page, and the banner below
    // already says why writes are unavailable.
    common::focus_anchor(ui, "offsets-page-focus");
    // Arriving on the page selects the system the machine is in, not G54: the
    // operator is nearly always working in the active offset, and G54 may be one
    // of nine stored rows.
    let now = linc_core::diag::now_ms();
    if now - app.offsets.last_frame_ms > 200 {
        if let Some(active) = app.snapshot.offsets.active_index {
            if let Some(row) = app
                .snapshot
                .offsets
                .offsets
                .iter()
                .position(|offset| offset.index == active)
            {
                app.offsets.selected_row = row;
                app.offsets.selected = active;
                app.offsets.selected_column = 1;
            }
        }
    }
    app.offsets.last_frame_ms = now;
    // Page-local: `A` selects the highlighted row as the active work system.
    // The handler lives here, so no other page consumes or sees the key.
    handle_activate_key(app, ui);
    // The EXT band's keys come before the page's own, so an open band keeps the
    // keyboard it opened with: `X`/`Y`/`Z` move between its fields instead of
    // opening the WCS editor behind it.
    handle_ext_keys(app, ui);
    let writable = app.snapshot.writes_allowed();
    if !writable {
        let reason = if app.snapshot.panel.protect_effective() {
            "PROTECT ON — offset writes blocked"
        } else if app.snapshot.task.running {
            "PROGRAM RUNNING — offsets are read-only"
        } else if app.panel_offline() {
            "PANEL OFFLINE — offset writes blocked"
        } else {
            "Machine must be ON and idle to write offsets"
        };
        common::read_only_hint(ui, reason);
    }
    ui.add_space(2.0);
    // One width for the page, pinned before anything inside the scroll area is
    // laid out: the readout rows, the table and the preview all end on the same
    // right edge, and the scroll area's own idea of the width cannot shift one
    // row against another.
    let available = (ui.available_width() - ui.spacing().scroll.bar_width).max(240.0);
    egui::ScrollArea::vertical()
        .id_salt("offsets-body")
        .max_height(ui.available_height())
        .show(ui, |ui| {
            ui.set_min_width(available);
            ui.set_max_width(available);
            // The rows take the page width and divide it themselves, so both
            // end on the same edge as the panes below.
            work_position_dro(app, ui, writable, available);
            ext_row(app, ui, writable, available);
            ui.add_space(6.0);
            // Both panes are measured from the same pinned width with no floor
            // of their own: a floor is what let one pane outgrow the row and put
            // its right edge past the readout cards above it.
            let table_width = available * 0.62;
            // The preview must end on the table's bottom edge, not on a fixed
            // height: the table grows with the number of systems.
            let mut table_height = PREVIEW_HEIGHT;
            ui.horizontal_top(|ui| {
                // The gap between the panes is the one named below: an implicit
                // one on each side of it widened the row past the readout cards.
                ui.spacing_mut().item_spacing.x = 0.0;
                ui.allocate_ui_with_layout(
                    egui::Vec2::new(table_width, PREVIEW_HEIGHT),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        work_offsets_card(app, ui, writable);
                        table_height = ui.min_rect().height().max(PREVIEW_HEIGHT);
                    },
                );
                ui.add_space(8.0);
                let preview_width = (available - table_width - 8.0).max(120.0);
                ui.allocate_ui_with_layout(
                    egui::Vec2::new(preview_width, table_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_min_width(preview_width);
                        ui.set_max_width(preview_width);
                        offset_preview(app, ui, table_height);
                    },
                );
            });
        });
}

/// DRO cards at the top of the page: the active system's work coordinates,
/// big, with the machine coordinate small underneath — the same shape as the
/// live DROs elsewhere. Clicking a value edits it; the entry takes arithmetic,
/// and committing writes the position, so the machine reports what was typed.
fn work_position_dro(app: &mut LincApp, ui: &mut Ui, writable: bool, width: f32) {
    let axes = app.axes();
    if axes.is_empty() {
        return;
    }
    let active = app.snapshot.offsets.active_index;
    // The Machine page's readout without the DTG line and move bar.
    let height = theme::DRO_CARD_HEIGHT_OFFSETS;

    handle_axis_keys(app, ui);
    handle_dro_keys(app, ui);

    let mut start: Option<(String, f64)> = None;
    {
        let rects = card_row(ui, width, axes.len(), height);
        for (rect, axis) in rects.into_iter().zip(axes.iter()) {
            let response = ui.interact(
                rect,
                egui::Id::unique(format!("offsets-dro-{axis}")),
                egui::Sense::click(),
            );
            let editing = app
                .offsets
                .dro_edit
                .as_ref()
                .map(|edit| edit.axis == *axis)
                .unwrap_or(false);
            let position = app.snapshot.positions.axes.get(axis);
            let actual_work = position.map(|p| p.work).unwrap_or(0.0);
            let (work, machine, _) = common::axis_display(app, axis).unwrap_or((0.0, 0.0, 0.0));
            // The Machine page's readout, without its DTG line and move bar.
            common::paint_dro_frame(ui, rect, editing);
            common::paint_dro_readout(
                ui,
                rect,
                axis,
                work,
                machine,
                false,
                !editing,
                app.reads_x_as_diameter(),
            );
            if editing {
                // Below the axis label, on the value line. The edit field covers
                // the machine coordinate while it is open; it returns on commit.
                // Over the readout line the field replaces.
                let field = egui::Rect::from_min_max(
                    egui::pos2(rect.left() + 10.0, rect.top() + 8.0),
                    egui::pos2(rect.right() - 10.0, rect.top() + 36.0),
                );
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(field)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                let mut buffer = app
                    .offsets
                    .dro_edit
                    .as_ref()
                    .map(|edit| edit.buffer.clone())
                    .unwrap_or_default();
                let edit = child.add_sized(
                    [field.width(), 28.0],
                    egui::TextEdit::singleline(&mut buffer)
                        // Stable id so the shell can leave keys to the field.
                        .id(egui::Id::unique(crate::state::DRO_FIELD_ID))
                        .font(egui::FontId::monospace(theme::font_size(20.0)))
                        .margin(egui::Margin::symmetric(4, 0)),
                );
                if app
                    .offsets
                    .dro_edit
                    .as_ref()
                    .map(|edit| edit.focus)
                    .unwrap_or(false)
                {
                    edit.request_focus();
                    // Pre-select the value: the operator is replacing a reading,
                    // not appending to it.
                    let id = egui::Id::unique(crate::state::DRO_FIELD_ID);
                    if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), id) {
                        state
                            .cursor
                            .set_char_range(Some(egui::text::CCursorRange::two(
                                egui::text::CCursor::new(0),
                                egui::text::CCursor::new(buffer.chars().count()),
                            )));
                        state.store(ui.ctx(), id);
                    }
                    if let Some(slot) = app.offsets.dro_edit.as_mut() {
                        slot.focus = false;
                    }
                }
                if edit.changed() {
                    if let Some(slot) = app.offsets.dro_edit.as_mut() {
                        slot.buffer = buffer;
                    }
                }
            } else if response.clicked() && writable && active.is_some() {
                // The edit seeds from the controller's own work position: what
                // the operator types overwrites it and is written back through
                // the same path, so the commanded display frame never leaks
                // into the value being set.
                start = Some((axis.clone(), actual_work));
            }
        }
    }
    if let Some((axis, work)) = start {
        app.offsets.dro_edit = Some(crate::state::DroEdit {
            axis,
            buffer: format!("{work:.3}"),
            focus: true,
        });
    }
}

/// The register value as the operator reads it: a lathe reads X as a diameter,
/// so the shift reads as one too.
fn ext_display(axis: &str, value: f64, diameter: bool) -> f64 {
    if diameter && axis == "X" {
        value * 2.0
    } else {
        value
    }
}

/// The operator's typed value in the units the command carries. One conversion
/// point: the write path speaks machine units, so a diameter typed into a lathe
/// field is halved here and nowhere else.
fn ext_machine_value(axis: &str, typed: f64, diameter: bool) -> f64 {
    if diameter && axis == "X" {
        typed / 2.0
    } else {
        typed
    }
}

/// One row of evenly divided cards, as explicit rects.
///
/// Not a horizontal layout: egui adds its own item spacing between allocated
/// items, so a row built that way ended a few pixels past the pane below it and
/// the page's right edge had three different answers. The widths come from the
/// row's own width, and the row advances the cursor by its height.
fn card_row(ui: &mut Ui, width: f32, cards: usize, height: f32) -> Vec<egui::Rect> {
    const GAP: f32 = 8.0;
    let top_left = ui.cursor().min;
    let card_width =
        ((width - GAP * (cards.saturating_sub(1)) as f32) / cards.max(1) as f32).max(80.0);
    let rects: Vec<egui::Rect> = (0..cards)
        .map(|index| {
            egui::Rect::from_min_size(
                egui::pos2(top_left.x + index as f32 * (card_width + GAP), top_left.y),
                egui::vec2(card_width, height),
            )
        })
        .collect();
    let row = egui::Rect::from_min_size(top_left, egui::vec2(width, height));
    ui.allocate_rect(row, egui::Sense::hover());
    rects
}

/// One EXT card per axis, under the DRO row: the same frame and value treatment
/// as the work coordinates above, with no machine coordinate — this number is
/// the shift itself, not a position.
pub(crate) fn ext_row(app: &mut LincApp, ui: &mut Ui, writable: bool, width: f32) {
    let axes = app.axes();
    if axes.is_empty() {
        return;
    }
    let height = theme::DRO_CARD_HEIGHT_OFFSETS;
    let diameter = app.reads_x_as_diameter();
    let writable = writable;
    let mut start: Option<String> = None;
    ui.add_space(6.0);
    {
        let rects = card_row(ui, width, axes.len(), height);
        for (rect, axis) in rects.into_iter().zip(axes.iter()) {
            let response = ui.interact(
                rect,
                egui::Id::unique(format!("ext-card-{axis}")),
                egui::Sense::click(),
            );
            let editing = app
                .offsets
                .ext_edit
                .as_ref()
                .map(|edit| edit.axis == *axis)
                .unwrap_or(false);
            let value = app.snapshot.offsets.g92.get(axis).copied().unwrap_or(0.0);
            let shown = ext_display(axis, value, diameter);
            common::paint_dro_frame(ui, rect, editing);
            let painter = ui.painter_at(rect);
            let line_y = rect.top() + theme::DRO_LINE_Y;
            painter.text(
                egui::pos2(rect.left() + theme::DRO_INSET, line_y),
                egui::Align2::LEFT_CENTER,
                format!("EXT {axis}"),
                FontId::monospace(theme::font_size(theme::DRO_FONT)),
                if value.abs() > 1e-9 {
                    theme::WARNING
                } else {
                    theme::C3
                },
            );
            if editing {
                let field_rect = egui::Rect::from_min_max(
                    egui::pos2(rect.left() + 88.0, rect.top() + 8.0),
                    egui::pos2(rect.right() - 10.0, rect.top() + 36.0),
                );
                let mut child = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(field_rect)
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                let mut buffer = app
                    .offsets
                    .ext_edit
                    .as_ref()
                    .map(|edit| edit.buffer.clone())
                    .unwrap_or_default();
                let id = egui::Id::unique(format!("{}-{axis}", crate::state::EXT_FIELD_ID));
                let field = child.add_sized(
                    [field_rect.width(), 28.0],
                    egui::TextEdit::singleline(&mut buffer)
                        .id(id)
                        .font(egui::FontId::monospace(theme::font_size(20.0)))
                        .margin(egui::Margin::symmetric(4, 0)),
                );
                if app
                    .offsets
                    .ext_edit
                    .as_ref()
                    .map(|edit| edit.focus)
                    .unwrap_or(false)
                {
                    field.request_focus();
                    if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), id) {
                        state
                            .cursor
                            .set_char_range(Some(egui::text::CCursorRange::two(
                                egui::text::CCursor::new(0),
                                egui::text::CCursor::new(buffer.chars().count()),
                            )));
                        state.store(ui.ctx(), id);
                    }
                    if let Some(slot) = app.offsets.ext_edit.as_mut() {
                        slot.focus = false;
                    }
                }
                if field.changed() {
                    if let Some(slot) = app.offsets.ext_edit.as_mut() {
                        slot.buffer = buffer;
                    }
                }
            } else {
                painter.text(
                    egui::pos2(rect.right() - theme::DRO_INSET, line_y),
                    egui::Align2::RIGHT_CENTER,
                    format!("{shown:+.3}"),
                    FontId::monospace(theme::font_size(theme::DRO_FONT)),
                    if value.abs() > 1e-9 {
                        theme::WARNING
                    } else {
                        theme::MUTED
                    },
                );
                if response.clicked() && writable {
                    start = Some(axis.clone());
                }
            }
        }
    }
    if let Some(axis) = start {
        start_ext_edit(app, axis);
    }
}

/// Page-local keys for the EXT cards: `Ctrl+X`/`Ctrl+Y`/`Ctrl+Z` edit that
/// axis, `Enter` commits, `Esc` discards, and a bare `0` clears every axis. The
/// chord is what keeps the digits free for typing and the arrows with the caret.
pub(crate) fn handle_ext_keys(app: &mut LincApp, ui: &mut Ui) {
    if app.overlay_open() {
        return;
    }
    let axes = app.axes();
    if axes.is_empty() {
        return;
    }
    let mut open: Option<String> = None;
    let mut clear = false;
    let mut commit = false;
    let mut cancel = false;
    let editing = app.offsets.ext_edit.is_some();
    // Read the context state before taking the input lock: `text_edit_focused`
    // reaches into the same context write that `input_mut` holds.
    let text_focused = ui.ctx().text_edit_focused();
    let table_editing = app.offsets.editing;
    let edit_x = app.prefs.keymap.ext_edit_x;
    let edit_y = app.prefs.keymap.ext_edit_y;
    let edit_z = app.prefs.keymap.ext_edit_z;
    let zero_binding = app.prefs.keymap.offset_ext_zero;
    ui.input_mut(|input| {
        if !text_focused && !table_editing {
            for (axis, binding) in [("X", edit_x), ("Y", edit_y), ("Z", edit_z)] {
                if axes.iter().any(|present| present == axis)
                    && input.consume_key(binding.modifiers, binding.key)
                {
                    open = Some(axis.to_string());
                }
            }
            if input.consume_key(zero_binding.modifiers, zero_binding.key) {
                clear = true;
            }
        }
        if editing {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                commit = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                cancel = true;
            }
        }
    });
    if let Some(axis) = open {
        start_ext_edit(app, axis);
        return;
    }
    if clear {
        send_ext_zero(app);
    }
    if commit && !commit_ext_edit(app) {
        return;
    }
    if cancel {
        app.offsets.ext_edit = None;
        release_ext_focus(ui);
    }
    if commit {
        release_ext_focus(ui);
    }
}

/// Hand the keyboard back once the EXT field stops being drawn: egui keeps a
/// text edit's focus after its widget is gone and every page key stays inert.
fn release_ext_focus(ui: &Ui) {
    for axis in ["X", "Y", "Z"] {
        ui.ctx().memory_mut(|memory| {
            memory.surrender_focus(egui::Id::unique(format!(
                "{}-{axis}",
                crate::state::EXT_FIELD_ID
            )))
        });
    }
}

fn start_ext_edit(app: &mut LincApp, axis: String) {
    if axis.is_empty() {
        return;
    }
    let diameter = app.reads_x_as_diameter();
    let value = app.snapshot.offsets.g92.get(&axis).copied().unwrap_or(0.0);
    let seed = ext_display(&axis, value, diameter);
    app.offsets.ext_edit = Some(crate::state::ExtEdit {
        axis,
        buffer: format!("{seed:.3}"),
        focus: true,
    });
}

/// Send one axis's shift. The command carries the value the operator asked for,
/// so the controller keeps ownership of the register and the card repaints from
/// what it reports.
fn commit_ext_edit(app: &mut LincApp) -> bool {
    let Some(edit) = app.offsets.ext_edit.clone() else {
        return false;
    };
    let Some(typed) = crate::state::eval_expression(&edit.buffer) else {
        app.toast("not a number or expression", theme::WARNING);
        return false;
    };
    let value = ext_machine_value(&edit.axis, typed, app.reads_x_as_diameter());
    let mut values = AxisMap::new();
    values.insert(edit.axis.clone(), value);
    app.send(MachineCommand::SetG92Offset { values });
    app.offsets.ext_edit = None;
    true
}

fn send_ext_zero(app: &mut LincApp) {
    if !app.snapshot.writes_allowed() {
        app.toast("offset writes are blocked", theme::WARNING);
        return;
    }
    app.send(MachineCommand::ClearG92Offset);
    app.offsets.ext_edit = None;
}

pub(crate) fn handle_axis_keys(app: &mut LincApp, ui: &mut Ui) {
    if app.overlay_open()
        || app.offsets.dro_edit.is_some()
        || app.offsets.editing
        || ui.ctx().text_edit_focused()
    {
        return;
    }
    let axes = app.axes();
    let mut start: Option<String> = None;
    ui.input_mut(|input| {
        for (axis, key) in [
            ("X", egui::Key::X),
            ("Y", egui::Key::Y),
            ("Z", egui::Key::Z),
        ] {
            if axes.iter().any(|present| present == axis)
                && input.consume_key(egui::Modifiers::NONE, key)
            {
                start = Some(axis.to_string());
            }
        }
    });
    let Some(axis) = start else { return };
    if !app.snapshot.writes_allowed() || app.snapshot.offsets.active_index.is_none() {
        app.toast("offset writes are blocked", theme::WARNING);
        return;
    }
    let work = app
        .snapshot
        .positions
        .axes
        .get(&axis)
        .map(|position| position.work)
        .unwrap_or(0.0);
    app.offsets.dro_edit = Some(crate::state::DroEdit {
        axis,
        buffer: format!("{work:.3}"),
        focus: true,
    });
}

/// `A` makes the selected table row the active work system (`G54`..`G59.3`).
///
/// A SHORTCUTS row click takes the same route.
///
/// Page-local by construction: only `offsets::show` calls this, so the key is
/// neither consumed nor acted on while another page is in front. An in-place
/// edit or a dialog owns the keyboard first, and a plain text field keeps `A`
/// as typed input.
fn handle_activate_key(app: &mut LincApp, ui: &mut Ui) {
    if app.overlay_open()
        || app.offsets.editing
        || app.offsets.dro_edit.is_some()
        || ui.ctx().text_edit_focused()
    {
        return;
    }
    // The SHORTCUTS row that names ACTIVATE runs the same code the key does.
    let activate = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::A))
        || app.take_shortcut_click() == Some(crate::state::ShortcutCommand::OffsetsActivate);
    if !activate {
        return;
    }
    let Some(index) = app
        .snapshot
        .offsets
        .offsets
        .get(app.offsets.selected_row)
        .map(|offset| offset.index)
    else {
        app.toast("no work offset selected", theme::WARNING);
        return;
    };
    if app.snapshot.offsets.active_index == Some(index) {
        app.toast(
            format!("{} is already active", linc_core::model::wcs_name(index)),
            theme::MUTED,
        );
        return;
    }
    app.send(MachineCommand::ActivateWorkOffset { index });
}

/// Height of the toolpath panel beside the table.
const PREVIEW_HEIGHT: f32 = 260.0;

/// The toolpath beside the work-offset table.
///
/// The active system keeps its own cross; the selected row (including an edit
/// that has not been written yet) is drawn in the current-line colour, so the
/// operator can see where the offset sits in the machine envelope.
fn offset_preview(app: &mut LincApp, ui: &mut Ui, height: f32) {
    let preview = app.program.last_valid_preview.clone();
    let highlight = selected_highlight(app);
    let mut zoom = app.offsets.preview_zoom;
    let mut pan = app.offsets.preview_pan;
    let mut yaw = app.offsets.preview_yaw;
    let mut pitch = app.offsets.preview_pitch;
    let mut fit = app.offsets.preview_fit;
    let mut auto_fit = app.offsets.preview_auto_fit;
    common::trajectory(
        app,
        ui,
        &preview,
        height.max(140.0),
        app.program.graph_view,
        &mut zoom,
        &mut pan,
        &mut yaw,
        &mut pitch,
        &mut fit,
        &mut auto_fit,
        None,
        None,
        false,
        true,
        false,
        true,
        true,
        highlight,
    );
    app.offsets.preview_zoom = zoom;
    app.offsets.preview_pan = pan;
    app.offsets.preview_yaw = yaw;
    app.offsets.preview_pitch = pitch;
    app.offsets.preview_fit = fit;
    app.offsets.preview_auto_fit = auto_fit;
}

/// The selected system's origin, taken from the draft so an edit previews as it
/// is typed.
fn selected_highlight(app: &LincApp) -> Option<([f64; 3], String)> {
    let index = app.offsets.selected;
    let row = app
        .snapshot
        .offsets
        .offsets
        .iter()
        .find(|offset| offset.index == index)?;
    let mut point = [0.0; 3];
    for (slot, axis) in ["X", "Y", "Z"].iter().enumerate() {
        let drafted = app
            .offsets
            .rows
            .get(&index)
            .and_then(|draft| draft.values.get(*axis))
            .and_then(|text| text.trim().parse::<f64>().ok());
        point[slot] = drafted
            .or_else(|| row.values.get(axis).copied())
            .unwrap_or(0.0);
    }
    Some((point, linc_core::model::wcs_name(index)))
}

/// Enter commits the typed position, Escape drops it. Both are consumed before
/// the field is drawn, so the number cannot be committed twice.
/// Enter writes the typed value, Escape drops it. Shared with MAIN's matrix for
/// the same reason as [`handle_axis_keys`].
pub(crate) fn handle_dro_keys(app: &mut LincApp, ui: &mut Ui) {
    if app.overlay_open() {
        return;
    }
    if app.offsets.dro_edit.is_none() {
        return;
    }
    let mut commit = false;
    let mut cancel = false;
    ui.input_mut(|input| {
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            commit = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            cancel = true;
        }
    });
    if commit {
        // A rejected value leaves the field open. Keep the keyboard on it too,
        // or the next keystroke goes to the page and the value can only be
        // corrected with the pointer.
        if !commit_dro_edit(app) {
            return;
        }
    } else if cancel {
        app.offsets.dro_edit = None;
    } else {
        return;
    }
    // The field is about to stop being drawn. egui would keep its focus and keep
    // answering `text_edit_focused` for a widget nobody can see, so hand the
    // keyboard back here rather than leaving the next keypress to a dead field.
    let id = egui::Id::unique(crate::state::DRO_FIELD_ID);
    ui.ctx().memory_mut(|memory| memory.surrender_focus(id));
}

/// Send the typed position. `G10 L20` computes the offset from the position the
/// machine holds, so the DRO then reads the entered value.
pub(crate) fn commit_dro_edit(app: &mut LincApp) -> bool {
    let Some(edit) = app.offsets.dro_edit.clone() else {
        return false;
    };
    let Some(value) = crate::state::eval_expression(&edit.buffer) else {
        app.toast("not a number or expression", theme::WARNING);
        return false;
    };
    let Some(index) = app.snapshot.offsets.active_index else {
        app.toast("no active work offset", theme::WARNING);
        return false;
    };
    let mut values = AxisMap::new();
    values.insert(edit.axis.clone(), value);
    app.send(MachineCommand::SetWorkPosition { index, values });
    app.offsets.dro_edit = None;
    true
}

/// G54-G59.3 as an editable table, built and driven like the tool table: one band
/// and a rim bar mark the selected row, an outline marks the cell, the arrow keys
/// move between cells, Enter edits and Escape cancels.
fn work_offsets_card(app: &mut LincApp, ui: &mut Ui, writable: bool) {
    const WCS: f32 = 64.0;
    const VALUE: f32 = 100.0;
    let axes = app.axes();
    let offsets = app.snapshot.offsets.offsets.clone();
    let active_index = app.snapshot.offsets.active_index;
    let columns = axes.len() + 1; // axes then rotation
    if app.offsets.selected_row >= offsets.len() {
        app.offsets.selected_row = offsets.len().saturating_sub(1);
    }
    if app.offsets.selected_column > columns {
        app.offsets.selected_column = columns;
    }
    // The table holds the keyboard, as the tool table does, so the arrow keys
    // move its selection instead of the page navigation.
    let focus_id = egui::Id::unique("offsets-table-focus");
    // The focus anchor must exist before the rows draw, but it must not push the
    // card down: the table's top edge lines up with the preview pane beside it.
    let spacing_y = ui.spacing().item_spacing.y;
    ui.spacing_mut().item_spacing.y = 0.0;
    let (focus_rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 0.0), egui::Sense::hover());
    let focus = ui.interact(
        focus_rect,
        focus_id,
        egui::Sense::focusable_noninteractive(),
    );
    ui.spacing_mut().item_spacing.y = spacing_y;
    if !app.offsets.editing
        && app.offsets.dro_edit.is_none()
        && app.offsets.ext_edit.is_none()
        && !focus.has_focus()
    {
        focus.request_focus();
    }
    if app.offsets.dro_edit.is_none() {
        handle_table_keys(app, ui, &offsets, columns);
    }

    // The selected system follows the selected row: clicking a cell in another
    // row must move the preview marker too, not only the row band. The row is
    // the truth on this page, so derive it here rather than trusting a stale
    // `selected` from the last WCS-name click.
    let mut selected = offsets
        .get(app.offsets.selected_row)
        .map(|offset| offset.index)
        .unwrap_or(app.offsets.selected);
    let commit: Option<u8> = None;
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, "WORK OFFSETS");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new("ARROWS MOVE · ENTER EDIT · A ACTIVATE · + - * / ACCEPTED")
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
            });
        });
        // Half a line of air between rows, not a whole one: the rows are text
        // and a band, and nine systems at the old pitch (row height plus the
        // table's full item spacing) grew the card past the page.
        let row_font = egui::FontId::monospace(theme::font_size(12.0));
        let line_height = ui
            .painter()
            .layout_no_wrap("0".to_string(), row_font, theme::FG)
            .size()
            .y;
        let row_height = line_height + 2.0;
        ui.spacing_mut().item_spacing.y = line_height * 0.5;
        let table = egui_extras::TableBuilder::new(ui)
            .id_salt("work-offsets-table")
            .column(egui_extras::Column::exact(WCS))
            .columns(egui_extras::Column::exact(VALUE), axes.len())
            .column(egui_extras::Column::exact(VALUE))
            .column(egui_extras::Column::remainder())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            // All nine systems are part of the table: an inner scroll area cut
            // G59.1..G59.3 out of sight. The page's own scroll area handles a
            // window too short for the rows.
            .vscroll(false);
        let mut bands: Vec<(egui::Rect, bool)> = Vec::new();
        table
            .header(18.0, |mut header| {
                header.col(|ui| {
                    ui.label(
                        RichText::new("WCS")
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                });
                for axis in &axes {
                    header.col(|ui| {
                        ui.label(
                            RichText::new(axis)
                                .size(theme::font_size(9.0))
                                .strong()
                                .color(theme::C3),
                        );
                    });
                }
                header.col(|ui| {
                    ui.label(
                        RichText::new("ROTATION")
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                });
                header.col(|ui| {
                    ui.label(
                        RichText::new("STATE")
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                });
            })
            .body(|mut body| {
                for (row_index, offset) in offsets.iter().enumerate() {
                    let index = offset.index;
                    let is_active = active_index == Some(index);
                    let is_selected_row = app.offsets.selected_row == row_index;
                    body.row(row_height, |mut row| {
                        let (_wcs_rect, _) = row.col(|ui| {
                            ui.add_space(WCS_TEXT_PAD);
                            // No solid selection fill here: the row band and the
                            // rim bar already mark the selection, and a blue box
                            // behind the name read as a second, contradictory
                            // state next to the ACTIVE column.
                            let colour = if is_active { theme::SUCCESS } else { theme::FG };
                            let response = ui.add(
                                egui::Label::new(
                                    RichText::new(linc_core::model::wcs_name(index))
                                        .monospace()
                                        .size(theme::font_size(12.0))
                                        .color(colour),
                                )
                                .sense(egui::Sense::click()),
                            );
                            if response.clicked() {
                                selected = index;
                            }
                        });
                        for (column, axis) in axes.iter().enumerate() {
                            let (_rect, _) = row.col(|ui| {
                                cell(
                                    app,
                                    ui,
                                    index,
                                    row_index,
                                    column + 1,
                                    CellField::Axis(axis.clone()),
                                    writable,
                                );
                            });
                        }
                        let (_rect, _) = row.col(|ui| {
                            cell(
                                app,
                                ui,
                                index,
                                row_index,
                                columns,
                                CellField::Rotation,
                                writable,
                            );
                        });
                        let (state_rect, _) = row.col(|ui| {
                            ui.label(
                                RichText::new(if is_active { "ACTIVE" } else { "STORED" })
                                    .monospace()
                                    .size(theme::font_size(9.0))
                                    .color(if is_active {
                                        theme::SUCCESS
                                    } else {
                                        theme::MUTED
                                    }),
                            );
                        });
                        bands.push((_wcs_rect.union(state_rect), is_selected_row));
                    });
                }
            });
        for (rect, selected_row) in bands {
            if !selected_row {
                continue;
            }
            let painter = ui.painter();
            painter.rect_filled(
                rect,
                egui::CornerRadius::same(2),
                theme::tint(theme::ACCENT, 34),
            );
            painter.rect_filled(
                egui::Rect::from_min_size(
                    egui::pos2(rect.left(), rect.top()),
                    egui::vec2(3.0, rect.height()),
                ),
                egui::CornerRadius::same(1),
                theme::ACCENT,
            );
        }
        if let Some(error) = &app.offsets.error {
            ui.label(
                RichText::new(error)
                    .size(theme::font_size(10.0))
                    .color(theme::ERROR),
            );
        }
    });
    app.offsets.selected = selected;
    if app.offsets.editing {
        return;
    }
    if let Some(index) = commit {
        apply_work_offset(app, index);
    }
}

/// Air between a cell's edge and its text, so the rim bar never touches a glyph
/// and a minus sign is not clipped.
const WCS_TEXT_PAD: f32 = 8.0;
const CELL_TEXT_PAD: f32 = 6.0;

#[derive(Clone)]
enum CellField {
    Axis(String),
    Rotation,
}

/// One value cell: a label normally, a text field while it is being edited.
fn cell(
    app: &mut LincApp,
    ui: &mut Ui,
    index: u8,
    row_index: usize,
    column: usize,
    field: CellField,
    writable: bool,
) {
    let selected = app.offsets.selected_row == row_index && app.offsets.selected_column == column;
    let editing = selected && app.offsets.editing;
    let draft = app.offsets.rows.entry(index).or_default();
    let text = match &field {
        CellField::Axis(axis) => draft
            .values
            .entry(axis.clone())
            .or_insert_with(|| "0.0000".into()),
        CellField::Rotation => &mut draft.rotation,
    };
    // Captured before any content: the border must use this rect whether the
    // cell shows a label or the editor, so opening it cannot change its size.
    let cell_rect = ui.max_rect();
    if editing {
        // Place the field in an exact interior rect. `add_sized` let the field's
        // minimum height grow the row, and the red border grew with it.
        let inner_rect = egui::Rect::from_min_max(
            egui::pos2(cell_rect.left() + CELL_TEXT_PAD, cell_rect.top() + 1.0),
            egui::pos2(
                (cell_rect.right() - CELL_TEXT_PAD).max(cell_rect.left() + 24.0),
                cell_rect.bottom() - 1.0,
            ),
        );
        let response = ui.put(
            inner_rect,
            egui::TextEdit::singleline(&mut app.offsets.edit_buffer)
                .font(egui::TextStyle::Monospace)
                .frame(egui::Frame::NONE)
                .margin(egui::Margin::symmetric(2, 0))
                .id(egui::Id::unique("linc-offset-cell")),
        );
        if !response.has_focus() {
            response.request_focus();
        }
        if app.offsets.edit_focus {
            let id = egui::Id::unique("linc-offset-cell");
            if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), id) {
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::two(
                        egui::text::CCursor::new(0),
                        egui::text::CCursor::new(app.offsets.edit_buffer.chars().count()),
                    )));
                state.store(ui.ctx(), id);
            }
            app.offsets.edit_focus = false;
        }
    } else {
        // Values are monospaced with four decimals, so right-aligning the
        // cell lines every decimal point up regardless of the integer width.
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.add_space(CELL_TEXT_PAD);
            let response = ui.add_enabled(
                writable,
                egui::Label::new(
                    RichText::new(text.as_str())
                        .monospace()
                        .size(theme::font_size(12.0))
                        .color(theme::FG),
                )
                .sense(egui::Sense::click()),
            );
            if response.clicked() {
                app.offsets.selected_row = row_index;
                app.offsets.selected_column = column;
                app.offsets.selected = index;
            }
        });
    }
    if selected {
        // Same rect and inset as the tool table: the box keeps its size when the
        // cell opens for editing, and red marks the open cell.
        ui.painter().rect_stroke(
            cell_rect.shrink2(egui::vec2(1.0, 1.0)),
            egui::CornerRadius::same(theme::SELECTION_RADIUS as u8),
            egui::Stroke::new(1.5, if editing { theme::ERROR } else { theme::ACCENT }),
            egui::StrokeKind::Inside,
        );
    }
}

/// Arrow keys move the selected cell; Enter edits it, again to commit, Escape to
/// cancel.
fn handle_table_keys(
    app: &mut LincApp,
    ui: &mut Ui,
    offsets: &[linc_core::model::WorkOffset],
    columns: usize,
) {
    if app.overlay_open() {
        return;
    }
    if app.offsets.editing {
        let mut commit = false;
        let mut cancel = false;
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                commit = true;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
                cancel = true;
            }
        });
        if commit || cancel {
            if commit {
                let index = offsets
                    .get(app.offsets.selected_row)
                    .map(|offset| offset.index)
                    .unwrap_or(1);
                store_cell(app, index);
                apply_work_offset(app, index);
            }
            app.offsets.editing = false;
            app.offsets.edit_buffer.clear();
        }
        return;
    }
    let mut move_row = 0isize;
    let mut move_column = 0isize;
    let mut edit = false;
    ui.input_mut(|input| {
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
            move_row -= 1;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
            move_row += 1;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft) {
            move_column -= 1;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight) {
            move_column += 1;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            edit = true;
        }
    });
    if move_row != 0 && !offsets.is_empty() {
        let target = app.offsets.selected_row as isize + move_row;
        app.offsets.selected_row = target.clamp(0, offsets.len() as isize - 1) as usize;
    }
    if move_column != 0 {
        let target = app.offsets.selected_column as isize + move_column;
        app.offsets.selected_column = target.clamp(1, columns as isize) as usize;
    }
    if edit {
        let index = offsets
            .get(app.offsets.selected_row)
            .map(|offset| offset.index)
            .unwrap_or(1);
        let column = app.offsets.selected_column;
        let axes = app.axes();
        let draft = app.offsets.rows.entry(index).or_default();
        app.offsets.edit_buffer = if column <= axes.len() {
            axes.get(column - 1)
                .and_then(|axis| draft.values.get(axis).cloned())
                .unwrap_or_default()
        } else {
            draft.rotation.clone()
        };
        app.offsets.editing = true;
        app.offsets.edit_focus = true;
    }
}

/// Copy the edit buffer into the draft cell.
fn store_cell(app: &mut LincApp, index: u8) {
    let column = app.offsets.selected_column;
    let axes = app.axes();
    let buffer = app.offsets.edit_buffer.clone();
    let _ = &axes;
    let draft = app.offsets.rows.entry(index).or_default();
    if column <= axes.len() {
        if let Some(axis) = axes.get(column - 1) {
            draft.values.insert(axis.clone(), buffer);
        }
    } else {
        draft.rotation = buffer;
    }
}

/// Write one work offset row, evaluating each cell as an expression.
fn apply_work_offset(app: &mut LincApp, index: u8) {
    let axes = app.axes();
    let draft = app.offsets.rows.get(&index).cloned().unwrap_or_default();
    let mut values = AxisMap::new();
    for axis in &axes {
        let text = draft.values.get(axis).cloned().unwrap_or_default();
        let Some(value) = crate::state::eval_expression(&text) else {
            app.offsets.error = Some(format!(
                "{} {axis}: invalid entry {text:?}",
                linc_core::model::wcs_name(index)
            ));
            return;
        };
        if let Err(error) = app.profile.machine_ini.validate_axis_value(axis, value) {
            app.offsets.error = Some(error);
            return;
        }
        values.insert(axis.clone(), value);
    }
    let rotation = if draft.rotation.trim().is_empty() {
        Some(0.0)
    } else {
        match crate::state::eval_expression(&draft.rotation) {
            Some(value) => Some(value),
            None => {
                app.offsets.error = Some("rotation: invalid entry".into());
                return;
            }
        }
    };
    app.offsets.error = None;
    // Show the evaluated numbers, so an expression is visible as its result.
    if let Some(draft) = app.offsets.rows.get_mut(&index) {
        for (axis, value) in values.iter() {
            draft.values.insert(axis.to_string(), format!("{value:.4}"));
        }
        draft.rotation = format!("{:.4}", rotation.unwrap_or(0.0));
    }
    app.send(MachineCommand::SetWorkOffset {
        index,
        values,
        rotation,
    });
    app.toast(
        format!("{} offset written", linc_core::model::wcs_name(index)),
        theme::ACCENT,
    );
}
