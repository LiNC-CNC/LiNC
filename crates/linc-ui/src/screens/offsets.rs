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

fn work_offsets_card(app: &mut LincApp, ui: &mut Ui, writable: bool) {
    let axes = app.axes();
    let offsets = app.snapshot.offsets.offsets.clone();
    let active_index = app.snapshot.offsets.active_index;
    let mut selected = app.offsets.selected;
    let mut write_row = None;
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, "WORK OFFSETS");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new("DIRECT CELL EDIT")
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
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.spacing_mut().interact_size.y = 24.0;
            egui::Grid::new("work-offsets-direct")
                .num_columns(axes.len() + 4)
                .striped(true)
                .spacing([8.0, 2.0])
                .show(ui, |ui| {
                    ui.label(
                        RichText::new("WCS")
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                    for axis in &axes {
                        ui.label(
                            RichText::new(axis)
                                .size(theme::font_size(9.0))
                                .strong()
                                .color(theme::C3),
                        );
                    }
                    ui.label(
                        RichText::new("ROTATION")
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                    ui.label(
                        RichText::new("STATE")
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                    ui.label(
                        RichText::new("WRITE")
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                    ui.end_row();

                    for offset in &offsets {
                        let index = offset.index;
                        let is_active = active_index == Some(index);
                        let is_selected = selected == index;
                        let label = format!("G{}", 53 + index);
                        let response = ui.selectable_label(
                            is_selected,
                            RichText::new(label)
                                .monospace()
                                .size(theme::font_size(12.0))
                                .color(if is_active { theme::SUCCESS } else { theme::FG }),
                        );
                        if response.clicked() {
                            selected = index;
                        }
                        let draft = app
                            .offsets
                            .rows
                            .entry(index)
                            .or_insert_with(crate::state::OffsetDraft::default);
                        for axis in &axes {
                            let text = draft
                                .values
                                .entry(axis.clone())
                                .or_insert_with(|| "0.0000".into());
                            ui.add_enabled(
                                writable,
                                egui::TextEdit::singleline(text)
                                    .desired_width(82.0)
                                    .font(egui::TextStyle::Monospace)
                                    .margin(egui::Margin::symmetric(4, 1))
                                    .min_size(egui::Vec2::new(82.0, 22.0)),
                            );
                        }
                        ui.add_enabled(
                            writable,
                            egui::TextEdit::singleline(&mut draft.rotation)
                                .desired_width(82.0)
                                .font(egui::TextStyle::Monospace)
                                .margin(egui::Margin::symmetric(4, 1))
                                .min_size(egui::Vec2::new(82.0, 22.0)),
                        );
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
                        if theme::action_button(ui, "WRITE", writable).clicked() {
                            write_row = Some(index);
                        }
                        ui.end_row();
                    }

                });
        });
        if let Some(error) = &app.offsets.error {
            ui.label(RichText::new(error).color(theme::ERROR));
        }
    });
    app.offsets.selected = selected;
    if let Some(index) = write_row {
        apply_work_offset(app, index);
    }
}

fn apply_work_offset(app: &mut LincApp, index: u8) {
    let axes = app.axes();
    let draft = app.offsets.rows.get(&index).cloned().unwrap_or_default();
    let mut values = AxisMap::new();
    for axis in &axes {
        let text = draft.values.get(axis).cloned().unwrap_or_default();
        let Ok(value) = text.trim().parse::<f64>() else {
            app.offsets.error = Some(format!("G{} {axis}: invalid number {text:?}", 53 + index));
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
        match draft.rotation.trim().parse::<f64>() {
            Ok(value) => Some(value),
            Err(_) => {
                app.offsets.error = Some("rotation: invalid number".into());
                return;
            }
        }
    };
    app.offsets.error = None;
    app.send(MachineCommand::SetWorkOffset {
        index,
        values,
        rotation,
    });
    app.toast(
        format!("G{} offset write queued", 53 + index),
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
