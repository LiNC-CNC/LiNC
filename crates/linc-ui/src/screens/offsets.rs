//! Work offsets G54-G59.3 and tool offsets.

use egui::{RichText, Ui};
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
    work_position_dro(app, ui, writable);
    ui.add_space(6.0);
    egui::ScrollArea::vertical()
        .id_salt("offsets-body")
        .max_height(ui.available_height())
        .show(ui, |ui| {
            let available = ui.available_width();
            let table_width = (available * 0.62).max(420.0);
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::Vec2::new(table_width, PREVIEW_HEIGHT),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        work_offsets_card(app, ui, writable);
                    },
                );
                ui.add_space(8.0);
                ui.allocate_ui_with_layout(
                    egui::Vec2::new((available - table_width - 8.0).max(260.0), PREVIEW_HEIGHT),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| offset_preview(app, ui, PREVIEW_HEIGHT),
                );
            });
        });
}

/// DRO cards at the top of the page: the active system's work coordinates,
/// big, with the machine coordinate small underneath — the same shape as the
/// live DROs elsewhere. Clicking a value edits it; the entry takes arithmetic,
/// and committing writes the position, so the machine reports what was typed.
fn work_position_dro(app: &mut LincApp, ui: &mut Ui, writable: bool) {
    let axes = app.axes();
    if axes.is_empty() {
        return;
    }
    let active = app.snapshot.offsets.active_index;
    let gap = 8.0;
    let width = ((ui.available_width() - gap * (axes.len() as f32 - 1.0))
        / axes.len() as f32)
        .max(120.0);
    const HEIGHT: f32 = 84.0;

    handle_axis_keys(app, ui);
    handle_dro_keys(app, ui);

    let mut start: Option<(String, f64)> = None;
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for axis in &axes {
            let (rect, response) =
                ui.allocate_exact_size(egui::vec2(width, HEIGHT), egui::Sense::click());
            let editing = app
                .offsets
                .dro_edit
                .as_ref()
                .map(|edit| edit.axis == *axis)
                .unwrap_or(false);
            let position = app.snapshot.positions.axes.get(axis);
            let work = position.map(|p| p.work).unwrap_or(0.0);
            let machine = position.map(|p| p.machine).unwrap_or(0.0);
            let painter = ui.painter_at(rect);
            painter.rect_filled(rect, 6, theme::PANEL);
            painter.rect_stroke(
                rect,
                6,
                egui::Stroke::new(1.0, if editing { theme::ACCENT } else { theme::BORDER }),
                egui::StrokeKind::Inside,
            );
            painter.text(
                rect.left_top() + egui::vec2(14.0, 9.0),
                egui::Align2::LEFT_TOP,
                axis,
                egui::FontId::proportional(theme::font_size(13.0)),
                theme::C3,
            );
            painter.text(
                rect.left_bottom() + egui::vec2(14.0, -11.0),
                egui::Align2::LEFT_BOTTOM,
                format!("MACH {machine:.3}"),
                egui::FontId::monospace(theme::font_size(9.0)),
                theme::MUTED,
            );
            if editing {
                let field = egui::Rect::from_min_max(
                    egui::pos2(rect.left() + 10.0, rect.top() + 30.0),
                    egui::pos2(rect.right() - 10.0, rect.top() + 60.0),
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
                        .id(egui::Id::unique("linc-offsets-dro"))
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
                    let id = egui::Id::unique("linc-offsets-dro");
                    if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), id) {
                        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
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
            } else {
                painter.text(
                    rect.left_top() + egui::vec2(14.0, 32.0),
                    egui::Align2::LEFT_TOP,
                    theme::format_position(work, 3),
                    egui::FontId::monospace(theme::font_size(23.0)),
                    theme::FG,
                );
                if response.clicked() && writable && active.is_some() {
                    start = Some((axis.clone(), work));
                }
            }
        }
    });
    if let Some((axis, work)) = start {
        app.offsets.dro_edit = Some(crate::state::DroEdit {
            axis,
            buffer: format!("{work:.3}"),
            focus: true,
        });
    }
}

/// X, Y and Z start editing that axis's DRO, so the operator does not have to
/// reach for the mouse to enter a coordinate.
fn handle_axis_keys(app: &mut LincApp, ui: &mut Ui) {
    if app.offsets.dro_edit.is_some() || app.offsets.editing {
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
        (height - 4.0).max(140.0),
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
fn handle_dro_keys(app: &mut LincApp, ui: &mut Ui) {
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
        commit_dro_edit(app);
    } else if cancel {
        app.offsets.dro_edit = None;
    }
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
    const ROW_HEIGHT: f32 = 26.0;
    let axes = app.axes();
    let offsets = app.snapshot.offsets.offsets.clone();
    let active_index = app.snapshot.offsets.active_index;
    let columns = axes.len() + 1; // axes then rotation
    if app.offsets.selected_row >= offsets.len() {
        app.offsets.selected_row = offsets.len().saturating_sub(1);
    }
    if app.offsets.selected_column >= columns + 1 {
        app.offsets.selected_column = columns;
    }
    // The table holds the keyboard, as the tool table does, so the arrow keys
    // move its selection instead of the page navigation.
    let focus_id = egui::Id::unique("offsets-table-focus");
    let (focus_rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    let focus = ui.interact(focus_rect, focus_id, egui::Sense::focusable_noninteractive());
    if !app.offsets.editing && app.offsets.dro_edit.is_none() && !focus.has_focus() {
        focus.request_focus();
    }
    if app.offsets.dro_edit.is_none() {
        handle_table_keys(app, ui, &offsets, columns);
    }

    let mut selected = app.offsets.selected;
    let commit: Option<u8> = None;
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, "WORK OFFSETS");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new("ARROWS MOVE · ENTER EDIT · + - * / ACCEPTED")
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
            });
        });
        if app.offsets.g92.len() != axes.len() {
            let mut values = vec!["0.0000".into(); axes.len()];
            for (index, value) in app.offsets.g92.iter().take(values.len()).enumerate() {
                values[index] = value.clone();
            }
            app.offsets.g92 = values;
        }
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
                    body.row(ROW_HEIGHT, |mut row| {
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
                                    .color(if is_active { theme::SUCCESS } else { theme::MUTED }),
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
    let draft = app
        .offsets
        .rows
        .entry(index)
        .or_insert_with(crate::state::OffsetDraft::default);
    let text = match &field {
        CellField::Axis(axis) => draft
            .values
            .entry(axis.clone())
            .or_insert_with(|| "0.0000".into()),
        CellField::Rotation => &mut draft.rotation,
    };
    ui.add_space(CELL_TEXT_PAD);
    if editing {
        let response = ui.add(
            egui::TextEdit::singleline(&mut app.offsets.edit_buffer)
                .desired_width(100.0)
                .font(egui::TextStyle::Monospace)
                .margin(egui::Margin::symmetric(4, 1))
                .id(egui::Id::unique("linc-offset-cell")),
        );
        if !response.has_focus() {
            response.request_focus();
        }
    } else {
        // Same size as the WCS name, and clear of the table edge so a minus
        // sign cannot be clipped by the cell's padding.
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
        }
        if selected {
            // Outline the selected cell, as the tool table does.
            ui.painter().rect_stroke(
                response.rect.expand(2.0),
                egui::CornerRadius::same(3),
                egui::Stroke::new(1.5, theme::ACCENT),
                egui::StrokeKind::Inside,
            );
        }
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
            app.offsets.error = Some(format!("{} {axis}: invalid entry {text:?}", linc_core::model::wcs_name(index)));
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
