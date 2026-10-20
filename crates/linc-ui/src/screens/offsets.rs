//! Work offsets G54-G59, G92 and tool offsets.

use egui::{RichText, Ui};
use linc_core::command::MachineCommand;
use linc_core::model::AxisMap;

use crate::screens::common;
use crate::shell::LincApp;
use crate::state::Page;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    common::page_header(
        ui,
        "OFFSETS",
        "Work offsets G54–G59, G92 and tool offsets. Read-only while running or protected.",
    );
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
    ui.horizontal(|ui| {
        for index in 1..=6u8 {
            let label = format!("G{}", 53 + index);
            if common::page_tab(ui, &label, app.offsets.selected == index).clicked() {
                app.offsets.selected = index;
                app.sync_offsets_from_snapshot(true);
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!("ACTIVE {}", app.active_wcs_label()))
                    .monospace()
                    .size(theme::font_size(11.0))
                    .color(theme::ACCENT),
            );
        });
    });
    ui.add_space(4.0);
    let available = ui.available_width();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::Vec2::new(available * 0.58, 470.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                work_offset_card(app, ui, writable);
                g92_card(app, ui, writable);
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            egui::Vec2::new((available - available * 0.58 - 8.0).max(300.0), 320.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                common::positions_table(app, ui);
                tool_offset_card(app, ui);
            },
        );
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if theme::action_button(ui, "PROBE", app.snapshot.probe.enabled).clicked() {
            app.set_page(Page::Probe);
        }
        if theme::action_button(ui, "TOOLS", true).clicked() {
            app.set_page(Page::Tools);
        }
        ui.label(
            RichText::new(
                "Offset writes are validated against INI travel and rechecked by the backend.",
            )
            .size(theme::font_size(10.0))
            .color(theme::MUTED),
        );
    });
}

fn work_offset_card(app: &mut LincApp, ui: &mut Ui, writable: bool) {
    let axes = app.axes();
    theme::card(ui, |ui| {
        ui.set_max_height(280.0);
        ui.horizontal(|ui| {
            theme::section_header(ui, &format!("WORK OFFSET G{}", 53 + app.offsets.selected));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(
                        if app.snapshot.offsets.active_index == Some(app.offsets.selected) {
                            "ACTIVE"
                        } else {
                            "STORED"
                        },
                    )
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(
                        if app.snapshot.offsets.active_index == Some(app.offsets.selected) {
                            theme::SUCCESS
                        } else {
                            theme::MUTED
                        },
                    ),
                );
            });
        });
        if app.offsets.values.len() != axes.len() {
            app.offsets.values = vec!["0.0000".into(); axes.len()];
        }
        ui.horizontal_wrapped(|ui| {
            for (index, axis) in axes.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(axis)
                            .size(theme::font_size(9.0))
                            .monospace()
                            .color(theme::C3),
                    );
                    ui.add_enabled(
                        writable,
                        egui::TextEdit::singleline(&mut app.offsets.values[index])
                            .desired_width(120.0)
                            .font(egui::TextStyle::Monospace)
                            .min_size(egui::Vec2::new(120.0, 32.0)),
                    );
                });
            }
            ui.vertical(|ui| {
                ui.label(
                    RichText::new("ROT")
                        .size(theme::font_size(9.0))
                        .monospace()
                        .color(theme::C3),
                );
                ui.add_enabled(
                    writable,
                    egui::TextEdit::singleline(&mut app.offsets.rotation)
                        .desired_width(110.0)
                        .font(egui::TextStyle::Monospace)
                        .min_size(egui::Vec2::new(110.0, 32.0)),
                );
            });
        });
        ui.add_space(6.0);
        if let Some(error) = &app.offsets.error {
            ui.label(RichText::new(error).color(theme::ERROR));
        }
        if theme::accent_button(ui, "APPLY WORK OFFSET", writable).clicked() {
            apply_work_offset(app);
        }
    });
}

fn apply_work_offset(app: &mut LincApp) {
    let axes = app.axes();
    let mut values = AxisMap::new();
    for (index, axis) in axes.iter().enumerate() {
        let text = app.offsets.values.get(index).cloned().unwrap_or_default();
        let Ok(value) = text.trim().parse::<f64>() else {
            app.offsets.error = Some(format!("{axis}: invalid number {text:?}"));
            return;
        };
        if let Err(error) = app.profile.machine_ini.validate_axis_value(axis, value) {
            app.offsets.error = Some(error);
            return;
        }
        values.insert(axis.clone(), value);
    }
    let rotation = match app.offsets.rotation.trim().parse::<f64>() {
        Ok(value) => Some(value),
        Err(_) => {
            app.offsets.error = Some("rotation: invalid number".into());
            return;
        }
    };
    app.offsets.error = None;
    app.send(MachineCommand::SetWorkOffset {
        index: app.offsets.selected,
        values,
        rotation,
    });
    app.toast(
        format!("G{} offset write queued", 53 + app.offsets.selected),
        theme::ACCENT,
    );
}

fn g92_card(app: &mut LincApp, ui: &mut Ui, writable: bool) {
    let axes = app.axes();
    theme::card(ui, |ui| {
        theme::section_header(ui, "G92 OFFSET");
        if app.offsets.g92.len() != axes.len() {
            app.offsets.g92 = vec!["0.0000".into(); axes.len()];
            app.sync_offsets_from_snapshot(true);
        }
        ui.horizontal_wrapped(|ui| {
            for (index, axis) in axes.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.label(
                        RichText::new(axis)
                            .size(theme::font_size(9.0))
                            .monospace()
                            .color(theme::C3),
                    );
                    ui.add_enabled(
                        writable,
                        egui::TextEdit::singleline(&mut app.offsets.g92[index])
                            .desired_width(130.0)
                            .font(egui::TextStyle::Monospace)
                            .min_size(egui::Vec2::new(130.0, 32.0)),
                    );
                });
            }
        });
        if theme::action_button(ui, "APPLY G92", writable).clicked() {
            let mut values = AxisMap::new();
            for (index, axis) in axes.iter().enumerate() {
                match app
                    .offsets
                    .g92
                    .get(index)
                    .and_then(|text| text.trim().parse::<f64>().ok())
                {
                    Some(value) => {
                        values.insert(axis.clone(), value);
                    }
                    None => {
                        app.offsets.error = Some(format!("G92 {axis}: invalid number"));
                        return;
                    }
                }
            }
            app.send(MachineCommand::SetG92Offset { values });
        }
    });
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
