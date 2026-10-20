//! Tools page: standard LinuxCNC tool table, quick change and manual confirm.

use egui::{RichText, Ui};
use linc_core::command::{MachineCommand, MdiOrigin};
use linc_core::model::{MachineVariant, ToolChangeStatus, ToolChangerKind, ToolGeometry, ToolRow};

use crate::screens::common;
use crate::shell::LincApp;
use crate::state::ToolsUi;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    common::page_header(
        ui,
        "TOOLS",
        "Standard LinuxCNC tool table. Explicit change; manual install confirmation is separate.",
    );
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!(
                "CURRENT T{}   PREPARED T{}   CHANGER {}",
                app.snapshot.tool_change.current_tool,
                app.snapshot.tool_change.prepared_tool,
                app.profile.changer.label()
            ))
            .monospace()
            .size(theme::font_size(12.0))
            .color(theme::FG),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if app.profile.changer == ToolChangerKind::Manual {
                ui.label(
                    RichText::new("POCKET COLUMN HIDDEN (MANUAL CHANGER)")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
            }
        });
    });
    ui.add_space(4.0);
    let available = ui.available_width();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::Vec2::new(available * 0.64, 470.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| table_card(app, ui),
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            egui::Vec2::new((available - available * 0.64 - 8.0).max(300.0), 470.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                change_card(app, ui);
                edit_card(app, ui);
                if let Some(atc) = app.snapshot.atc.as_ref() {
                    theme::card(ui, |ui| {
                        theme::section_header(ui, app.profile.changer.label());
                        theme::kv_row(ui, "POCKETS", atc.pockets.to_string());
                        theme::kv_row(
                            ui,
                            "CURRENT",
                            atc.current_pocket
                                .map(|p| format!("P{p}"))
                                .unwrap_or_else(|| "—".into()),
                        );
                        theme::kv_row(
                            ui,
                            "PREPARED",
                            atc.prepared_pocket
                                .map(|p| format!("P{p}"))
                                .unwrap_or_else(|| "—".into()),
                        );
                        theme::kv_row(
                            ui,
                            "CLAMP",
                            atc.clamped
                                .map(|c| if c { "CLAMPED" } else { "UNCLAMPED" })
                                .unwrap_or("UNKNOWN"),
                        );
                        theme::kv_row(ui, "INDEX", if atc.indexing { "INDEXING" } else { "READY" });
                        if let Some(fault) = &atc.fault {
                            ui.label(RichText::new(fault).color(theme::ERROR));
                        }
                    });
                }
            },
        );
    });
}

fn table_card(app: &mut LincApp, ui: &mut Ui) {
    let pocket = app.profile.changer.uses_pockets();
    let lathe = app.profile.variant == MachineVariant::Lathe;
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, "TOOL TABLE");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "REV {}   {}",
                        app.snapshot.tool_table.revision,
                        app.snapshot.tool_table.path.display()
                    ))
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
                );
            });
        });
        if let Some(error) = &app.snapshot.tool_table.error {
            ui.label(RichText::new(error).color(theme::ERROR));
        }
        egui::ScrollArea::vertical()
            .id_salt("tool-table-scroll")
            .max_height(360.0)
            .show(ui, |ui| {
                egui::Grid::new("tool-table")
                    .num_columns(if pocket { 7 } else { 6 })
                    .striped(true)
                    .spacing([14.0, 5.0])
                    .show(ui, |ui| {
                        for header in ["T", "DESCRIPTION"] {
                            ui.label(
                                RichText::new(header)
                                    .size(theme::font_size(9.0))
                                    .strong()
                                    .color(theme::MUTED),
                            );
                        }
                        if lathe {
                            for header in ["X", "Z", "NOSE R", "ORIENT"] {
                                ui.label(
                                    RichText::new(header)
                                        .size(theme::font_size(9.0))
                                        .strong()
                                        .color(theme::MUTED),
                                );
                            }
                        } else {
                            for header in ["Ø", "LENGTH", "TYPE"] {
                                ui.label(
                                    RichText::new(header)
                                        .size(theme::font_size(9.0))
                                        .strong()
                                        .color(theme::MUTED),
                                );
                            }
                        }
                        if pocket {
                            ui.label(
                                RichText::new(if lathe { "STATION" } else { "POCKET" })
                                    .size(theme::font_size(9.0))
                                    .strong()
                                    .color(theme::MUTED),
                            );
                        }
                        ui.end_row();
                        let rows = app.snapshot.tool_table.rows.clone();
                        for (index, row) in rows.iter().enumerate() {
                            let selected = app.tools.selected_row == index;
                            let current = row.tool == app.snapshot.tool_change.current_tool;
                            let response = ui.selectable_label(
                                selected,
                                RichText::new(format!("T{}", row.tool))
                                    .monospace()
                                    .size(theme::font_size(12.0))
                                    .color(if current { theme::SUCCESS } else { theme::FG }),
                            );
                            if response.clicked() {
                                app.tools.selected_row = index;
                                app.tools.edit_row = None;
                            }
                            ui.label(
                                RichText::new(if row.description.is_empty() {
                                    "—"
                                } else {
                                    &row.description
                                })
                                .size(theme::font_size(11.0))
                                .color(if selected {
                                    theme::FG
                                } else {
                                    theme::MUTED
                                }),
                            );
                            if lathe {
                                for axis in ["X", "Z"] {
                                    ui.label(
                                        RichText::new(format!(
                                            "{:+.3}",
                                            row.geometry.values.get(axis).copied().unwrap_or(0.0)
                                        ))
                                        .monospace()
                                        .size(theme::font_size(10.0)),
                                    );
                                }
                                ui.label(
                                    RichText::new(
                                        row.geometry
                                            .nose_radius
                                            .map(|v| format!("{v:.3}"))
                                            .unwrap_or_else(|| "—".into()),
                                    )
                                    .monospace()
                                    .size(theme::font_size(10.0)),
                                );
                                ui.label(
                                    RichText::new(
                                        row.geometry
                                            .orientation
                                            .map(|v| v.to_string())
                                            .unwrap_or_else(|| "—".into()),
                                    )
                                    .monospace()
                                    .size(theme::font_size(10.0)),
                                );
                            } else {
                                ui.label(
                                    RichText::new(
                                        row.geometry
                                            .diameter
                                            .map(|v| format!("{v:.3}"))
                                            .unwrap_or_else(|| "—".into()),
                                    )
                                    .monospace()
                                    .size(theme::font_size(10.0)),
                                );
                                ui.label(
                                    RichText::new(
                                        row.geometry
                                            .length
                                            .map(|v| format!("{v:+.3}"))
                                            .unwrap_or_else(|| "—".into()),
                                    )
                                    .monospace()
                                    .size(theme::font_size(10.0)),
                                );
                                ui.label(
                                    RichText::new(&row.kind)
                                        .size(theme::font_size(9.0))
                                        .color(theme::MUTED),
                                );
                            }
                            if pocket {
                                ui.label(
                                    RichText::new(
                                        row.pocket
                                            .map(|p| format!("P{p}"))
                                            .unwrap_or_else(|| "—".into()),
                                    )
                                    .monospace()
                                    .size(theme::font_size(10.0)),
                                );
                            }
                            ui.end_row();
                        }
                    });
            });
    });
}

fn change_card(app: &mut LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "TOOL CHANGE");
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("TARGET")
                    .size(theme::font_size(10.0))
                    .color(theme::MUTED),
            );
            ui.add(
                egui::TextEdit::singleline(&mut app.tools.target)
                    .desired_width(64.0)
                    .font(egui::TextStyle::Monospace),
            );
            ui.label(
                RichText::new("T0 UNLOADS")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
        let gate = app.snapshot.issue_allowed();
        let can_change = gate.is_ok() && !app.snapshot.task.running;
        if let Err(reason) = &gate {
            ui.label(
                RichText::new(reason)
                    .size(theme::font_size(10.0))
                    .color(theme::WARNING),
            );
        }
        if theme::accent_button(ui, "CHANGE TOOL", can_change).clicked() {
            issue_change(app);
        }
        if theme::action_button(ui, "SET CURRENT (M61)", can_change).clicked() {
            let tool = app
                .tools
                .target
                .trim()
                .parse::<u16>()
                .unwrap_or_else(|_| selected_tool(app));
            app.send(MachineCommand::SetCurrentTool { tool });
            app.toast(
                format!("M61 Q{tool} queued (not a tool change)"),
                theme::ACCENT,
            );
        }
        ui.label(
            RichText::new("CHANGE TOOL issues Tn M6 immediately via issue_mdi; no staging or cycle-start gate.")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        if let Some(error) = &app.tools.error {
            ui.label(RichText::new(error).color(theme::ERROR));
        }

        if app.snapshot.tool_change.status == ToolChangeStatus::ManualPrompt {
            theme::separator(ui);
            ui.label(
                RichText::new(&app.snapshot.tool_change.message)
                    .size(theme::font_size(15.0))
                    .strong()
                    .color(theme::WARNING),
            );
            if let Some(token) = app.snapshot.tool_change.token.clone() {
                if theme::accent_button(
                    ui,
                    "CONFIRM INSTALLED",
                    app.snapshot.panel.connected && !app.snapshot.task.running,
                )
                .clicked()
                {
                    app.send(MachineCommand::ConfirmManualToolChange { token });
                }
            }
            ui.label(
                RichText::new("Writes linc.tool-change-confirm; never auto-confirms.")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        } else {
            theme::kv_row(ui, "CHANGE STATE", app.snapshot.tool_change.status.label());
            if !app.snapshot.tool_change.message.is_empty() {
                ui.label(
                    RichText::new(&app.snapshot.tool_change.message)
                        .size(theme::font_size(10.0))
                        .color(theme::MUTED),
                );
            }
        }
    });
}

fn selected_tool(app: &LincApp) -> u16 {
    app.snapshot
        .tool_table
        .rows
        .get(app.tools.selected_row)
        .map(|row| row.tool)
        .unwrap_or(0)
}

pub fn issue_change(app: &mut LincApp) {
    let tool = app
        .tools
        .target
        .trim()
        .parse::<u16>()
        .unwrap_or_else(|_| selected_tool(app));
    if let Err(error) = app.snapshot.issue_allowed() {
        app.tools.error = Some(error);
        return;
    }
    if let Some(row) = app
        .snapshot
        .tool_table
        .rows
        .iter()
        .find(|row| row.tool == tool)
    {
        app.tools.target = tool.to_string();
        app.toast(
            format!("T{} M6 queued — {}", tool, row.description),
            theme::ACCENT,
        );
    } else if tool != 0 {
        app.toast(
            format!("T{tool} is not in the tool table; issuing anyway"),
            theme::WARNING,
        );
    }
    app.tools.error = None;
    app.send(MachineCommand::IssueToolChange { tool });
}

fn edit_card(app: &mut LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "EDIT SELECTED ROW");
        let writable = app.snapshot.writes_allowed();
        let selected = selected_tool(app);
        if app.tools.edit_row.is_none() {
            if theme::action_button(ui, "EDIT SELECTED", writable && selected != 0).clicked() {
                load_edit_row(app);
            }
            ui.label(
                RichText::new("Table writes require idle, machine ON and PROTECT OFF.")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            return;
        }
        let lathe = app.profile.variant == MachineVariant::Lathe;
        egui::Grid::new("tool-edit")
            .num_columns(2)
            .spacing([10.0, 4.0])
            .show(ui, |ui| {
                for (key, label) in [
                    ("description", "DESCRIPTION"),
                    ("diameter", if lathe { "D" } else { "Ø" }),
                    ("length", if lathe { "Z" } else { "LENGTH" }),
                    ("nose_radius", "NOSE R"),
                    ("orientation", "ORIENT"),
                    ("X", "X"),
                    ("I", "I"),
                    ("J", "J"),
                    ("pocket", "POCKET"),
                ] {
                    if !lathe && matches!(key, "X" | "I" | "J" | "nose_radius" | "orientation") {
                        continue;
                    }
                    if lathe && key == "length" {
                        continue;
                    }
                    if key == "pocket" && !app.profile.changer.uses_pockets() {
                        continue;
                    }
                    ui.label(
                        RichText::new(label)
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                    let field = app.tools.edit_fields.entry(key.to_string()).or_default();
                    ui.add_enabled(
                        writable,
                        egui::TextEdit::singleline(field)
                            .desired_width(120.0)
                            .font(egui::TextStyle::Monospace),
                    );
                    ui.end_row();
                }
            });
        if theme::accent_button(ui, "SAVE ROW", writable).clicked() {
            save_row(app);
        }
        if theme::action_button(ui, "CANCEL", true).clicked() {
            app.tools.edit_row = None;
            app.tools.edit_fields.clear();
        }
        if let Some(error) = &app.tools.error {
            ui.label(RichText::new(error).color(theme::ERROR));
        }
    });
}

fn load_edit_row(app: &mut LincApp) {
    let index = app.tools.selected_row;
    let Some(row) = app.snapshot.tool_table.rows.get(index).cloned() else {
        return;
    };
    let mut fields = std::collections::HashMap::new();
    fields.insert("description".into(), row.description.clone());
    fields.insert(
        "diameter".into(),
        row.geometry
            .diameter
            .map(|v| v.to_string())
            .unwrap_or_default(),
    );
    fields.insert(
        "length".into(),
        row.geometry
            .length
            .map(|v| v.to_string())
            .unwrap_or_default(),
    );
    fields.insert(
        "nose_radius".into(),
        row.geometry
            .nose_radius
            .map(|v| v.to_string())
            .unwrap_or_default(),
    );
    fields.insert(
        "orientation".into(),
        row.geometry
            .orientation
            .map(|v| v.to_string())
            .unwrap_or_default(),
    );
    fields.insert(
        "pocket".into(),
        row.pocket.map(|v| v.to_string()).unwrap_or_default(),
    );
    for axis in ["X", "Z", "I", "J"] {
        fields.insert(
            axis.into(),
            row.geometry
                .values
                .get(axis)
                .map(|v| v.to_string())
                .unwrap_or_default(),
        );
    }
    app.tools.edit_row = Some(row);
    app.tools.edit_fields = fields;
}

fn field(app: &ToolsUi, key: &str) -> String {
    app.edit_fields.get(key).cloned().unwrap_or_default()
}

fn parse_optional(app: &ToolsUi, key: &str) -> Result<Option<f64>, String> {
    let value = field(app, key);
    if value.trim().is_empty() {
        return Ok(None);
    }
    value
        .trim()
        .parse::<f64>()
        .map(Some)
        .map_err(|_| format!("{key}: invalid number"))
}

fn save_row(app: &mut LincApp) {
    let Some(mut row) = app.tools.edit_row.clone() else {
        return;
    };
    let result = (|| -> Result<(), String> {
        row.description = field(&app.tools, "description");
        row.geometry.diameter = parse_optional(&app.tools, "diameter")?;
        row.geometry.length = parse_optional(&app.tools, "length")?;
        row.geometry.nose_radius = parse_optional(&app.tools, "nose_radius")?;
        row.geometry.orientation = parse_optional(&app.tools, "orientation")?.map(|v| v as u8);
        row.pocket = parse_optional(&app.tools, "pocket")?.map(|v| v as u16);
        for axis in ["X", "Z", "I", "J"] {
            if let Some(value) = parse_optional(&app.tools, axis)? {
                row.geometry.values.insert(axis, value);
                match axis {
                    "I" => row.geometry.front_angle = Some(value),
                    "J" => row.geometry.back_angle = Some(value),
                    _ => {}
                }
            }
        }
        row.kind = if app.profile.variant == MachineVariant::Lathe {
            "TURN".into()
        } else if row.geometry.diameter.unwrap_or(0.0) <= 0.5 {
            "DRILL".into()
        } else {
            "MILL".into()
        };
        Ok(())
    })();
    if let Err(error) = result {
        app.tools.error = Some(error);
        return;
    }
    app.tools.error = None;
    app.send(MachineCommand::SetToolTableRow {
        row,
        expected_revision: app.snapshot.tool_table.revision,
    });
    app.tools.edit_row = None;
    app.tools.edit_fields.clear();
    app.toast("tool table row write queued", theme::ACCENT);
}

pub fn set_current(app: &mut LincApp) {
    let tool = selected_tool(app);
    app.send(MachineCommand::SetCurrentTool { tool });
}

pub fn _unused(_: MdiOrigin, _: ToolGeometry, _: ToolRow) {}
