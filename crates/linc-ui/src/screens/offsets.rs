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

/// G54-G59 as an editable table.
///
/// Cells are edited in place and committed on Enter or when focus leaves them,
/// the way the tool table works. A commit writes the whole row through
/// `SetWorkOffset`, which the bridge issues as `G10 L2` in MDI, switching mode
/// for the block and restoring the previous mode afterwards.
fn work_offsets_card(app: &mut LincApp, ui: &mut Ui, writable: bool) {
    const WCS: f32 = 64.0;
    const VALUE: f32 = 100.0;
    let axes = app.axes();
    let offsets = app.snapshot.offsets.offsets.clone();
    let active_index = app.snapshot.offsets.active_index;
    let mut selected = app.offsets.selected;
    let mut commit: Option<u8> = None;
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, "WORK OFFSETS");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new("ENTER COMMITS · + - * / ACCEPTED")
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
            .striped(true)
            .column(egui_extras::Column::exact(WCS))
            .columns(egui_extras::Column::exact(VALUE), axes.len())
            .column(egui_extras::Column::exact(VALUE))
            .column(egui_extras::Column::remainder())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
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
                for offset in &offsets {
                    let index = offset.index;
                    let is_active = active_index == Some(index);
                    let is_selected = selected == index;
                    body.row(26.0, |mut row| {
                        row.col(|ui| {
                            // The selected chip fills with the accent colour, so
                            // its text has to be dark or it disappears into it.
                            let colour = if is_selected {
                                theme::SURFACE
                            } else if is_active {
                                theme::SUCCESS
                            } else {
                                theme::FG
                            };
                            let response = ui.selectable_label(
                                is_selected,
                                RichText::new(format!("G{}", 53 + index))
                                    .monospace()
                                    .size(theme::font_size(12.0))
                                    .color(colour),
                            );
                            if response.clicked() {
                                selected = index;
                            }
                        });
                        for axis in &axes {
                            row.col(|ui| {
                                let draft = app
                                    .offsets
                                    .rows
                                    .entry(index)
                                    .or_insert_with(crate::state::OffsetDraft::default);
                                let text = draft
                                    .values
                                    .entry(axis.clone())
                                    .or_insert_with(|| "0.0000".into());
                                let response = ui.add_enabled(
                                    writable,
                                    egui::TextEdit::singleline(text)
                                        .desired_width(VALUE - 8.0)
                                        .font(egui::TextStyle::Monospace)
                                        .margin(egui::Margin::symmetric(4, 1)),
                                );
                                if committed(&response) {
                                    commit = Some(index);
                                }
                            });
                        }
                        row.col(|ui| {
                            let draft = app
                                .offsets
                                .rows
                                .entry(index)
                                .or_insert_with(crate::state::OffsetDraft::default);
                            let response = ui.add_enabled(
                                writable,
                                egui::TextEdit::singleline(&mut draft.rotation)
                                    .desired_width(VALUE - 8.0)
                                    .font(egui::TextStyle::Monospace)
                                    .margin(egui::Margin::symmetric(4, 1)),
                            );
                            if committed(&response) {
                                commit = Some(index);
                            }
                        });
                        row.col(|ui| {
                            ui.label(
                                RichText::new(if is_active { "ACTIVE" } else { "STORED" })
                                    .monospace()
                                    .size(theme::font_size(9.0))
                                    .color(if is_active { theme::SUCCESS } else { theme::MUTED }),
                            );
                        });
                    });
                }
            });
        if let Some(error) = &app.offsets.error {
            ui.label(
                RichText::new(error)
                    .size(theme::font_size(10.0))
                    .color(theme::ERROR),
            );
        }
    });
    app.offsets.selected = selected;
    if let Some(index) = commit {
        apply_work_offset(app, index);
    }
}

/// Enter, or leaving the field, commits a cell.
fn committed(response: &egui::Response) -> bool {
    response.lost_focus()
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
