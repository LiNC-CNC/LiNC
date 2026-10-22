//! Work offsets G54-G59 and tool offsets.

use egui::{RichText, Ui};
use linc_core::command::MachineCommand;
use linc_core::model::AxisMap;

use crate::screens::common;
use crate::shell::LincApp;
use crate::state::Page;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    // No page title: the navigation names the page, and the banner below
    // already says why writes are unavailable.
    common::focus_anchor(ui, "offsets-page-focus");
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
    egui::ScrollArea::vertical()
        .id_salt("offsets-body")
        .max_height(ui.available_height())
        .show(ui, |ui| {
            work_offsets_card(app, ui, writable);
            ui.add_space(6.0);
            let available = ui.available_width();
            ui.horizontal_top(|ui| {
                ui.allocate_ui_with_layout(
                    egui::Vec2::new(available * 0.58, 230.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        common::positions_table(app, ui);
                    },
                );
                ui.add_space(8.0);
                ui.allocate_ui_with_layout(
                    egui::Vec2::new((available - available * 0.58 - 8.0).max(300.0), 230.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        tool_offset_card(app, ui);
                    },
                );
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Offset writes are validated against INI travel and rechecked by the backend.",
                )
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
            );
        });
}

/// G54-G59 as an editable table, built and driven like the tool table: one band
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
    if !app.offsets.editing && !focus.has_focus() {
        focus.request_focus();
    }
    handle_table_keys(app, ui, &offsets, columns);

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
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
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
                            // The band behind a selected row is a faint wash, so
                            // the label keeps its normal colour.
                            let colour = if is_active { theme::SUCCESS } else { theme::FG };
                            let response = ui.selectable_label(
                                selected == index,
                                RichText::new(format!("G{}", 53 + index))
                                    .monospace()
                                    .size(theme::font_size(12.0))
                                    .color(colour),
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
        let response = ui.add_enabled(
            writable,
            egui::Label::new(
                RichText::new(text.as_str())
                    .monospace()
                    .size(theme::font_size(11.0))
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
            app.offsets.error = Some(format!("G{} {axis}: invalid entry {text:?}", 53 + index));
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
        format!("G{} offset written", 53 + index),
        theme::ACCENT,
    );
}

fn tool_offset_card(app: &mut LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "TOOL OFFSET");
        let tool = app.snapshot.tool_change.current_tool;
        let Some(row) = app
            .snapshot
            .tool_table
            .rows
            .iter()
            .find(|row| row.tool == tool)
        else {
            ui.label(RichText::new(format!("T{tool}: not in tool table")).color(theme::MUTED));
            return;
        };
        ui.label(
            RichText::new(format!("T{} {}", row.tool, row.description))
                .monospace()
                .size(theme::font_size(12.0))
                .color(theme::FG),
        );
        if app.profile.variant == linc_core::model::MachineVariant::Lathe {
            let get = |axis: &str| row.geometry.values.get(axis).copied().unwrap_or(0.0);
            for (label, value) in [
                ("X", get("X")),
                ("Z", get("Z")),
                ("NOSE R", row.geometry.nose_radius.unwrap_or(0.0)),
                ("ORIENT", row.geometry.orientation.unwrap_or(0) as f64),
            ] {
                theme::kv_row(ui, label, format!("{value:+.4}"));
            }
            theme::kv_row(
                ui,
                "STATION",
                row.pocket
                    .map(|p| format!("P{p}"))
                    .unwrap_or_else(|| "—".into()),
            );
        } else {
            theme::kv_row(
                ui,
                "DIAMETER",
                row.geometry
                    .diameter
                    .map(|value| format!("{value:.4}"))
                    .unwrap_or_else(|| "—".into()),
            );
            theme::kv_row(
                ui,
                "LENGTH",
                row.geometry
                    .length
                    .map(|value| format!("{value:+.4}"))
                    .unwrap_or_else(|| "—".into()),
            );
        }
        if theme::action_button(ui, "EDIT IN TOOLS", true).clicked() {
            app.set_page(Page::Tools);
        }
    });
}
