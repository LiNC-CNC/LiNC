//! Tools page: standard LinuxCNC tool table, quick change and manual confirm.

use egui::{RichText, Ui};
use linc_core::command::MachineCommand;
use linc_core::model::{MachineVariant, ToolChangeStatus, ToolChangerKind};

use crate::screens::common;
use crate::shell::LincApp;
use crate::state::ToolDraft;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    // No page title: the navigation names the page.
    common::focus_anchor(ui, "tools-page-focus");
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
    let content_height = (ui.available_height() - 8.0).max(240.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::Vec2::new(available * 0.64, content_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("tools-table-column")
                    .max_height(content_height - 4.0)
                    .show(ui, |ui| table_card(app, ui));
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            egui::Vec2::new(
                (available - available * 0.64 - 8.0).max(300.0),
                content_height,
            ),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("tools-right-column")
                    .max_height(content_height - 4.0)
                    .show(ui, |ui| {
                        change_card(app, ui);
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
                                theme::kv_row(
                                    ui,
                                    "INDEX",
                                    if atc.indexing { "INDEXING" } else { "READY" },
                                );
                                if let Some(fault) = &atc.fault {
                                    ui.label(RichText::new(fault).color(theme::ERROR));
                                }
                            });
                        } else {
                            theme::card(ui, |ui| {
                                theme::section_header(ui, "TOOL CHANGER");
                                ui.label(
                                    RichText::new("NO ATC / CAROUSEL CONFIGURED")
                                        .size(theme::font_size(9.0))
                                        .color(theme::MUTED),
                                );
                                theme::kv_row(ui, "MODE", app.profile.changer.label());
                                theme::kv_row(ui, "POCKET COLUMN", "HIDDEN");
                                ui.label(
                            RichText::new(
                                "Manual change uses CHANGE TOOL and the install confirmation.",
                            )
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                        );
                            });
                        }
                    });
            },
        );
    });
}

fn table_card(app: &mut LincApp, ui: &mut Ui) {
    let pocket = app.profile.changer.uses_pockets();
    let lathe = app.profile.variant == MachineVariant::Lathe;
    let writable = app.snapshot.writes_allowed();
    let rows = app.snapshot.tool_table.rows.clone();
    let current_tool = app.snapshot.tool_change.current_tool;
    let mut write_tool = None;
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, "TOOL TABLE");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "DIRECT CELL EDIT · REV {}",
                        app.snapshot.tool_table.revision
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
        // Columns are measured from the font and the description takes the
        // remaining width, so the table fits the card instead of a set of
        // guessed widths; a card too narrow for the minimums scrolls sideways
        // rather than hiding a column.
        let columns = 3 + if lathe { 4 } else { 3 } + usize::from(pocket);
        let pad = theme::CELL_PAD;
        let tool_width = theme::measure(ui, "T00", 12.0) + pad;
        let number_width = theme::measure(ui, "-000.000", 11.0) + pad;
        let kind_width = theme::measure(ui, "BORING", 9.0) + pad;
        let write_width = 96.0;
        let mut fixed = tool_width + write_width + pad;
        fixed += if lathe {
            4.0 * number_width
        } else {
            2.0 * number_width + kind_width
        };
        if pocket {
            fixed += number_width;
        }
        let description_min = theme::measure(ui, "DESCRIPTION_DESCRIPTION", 11.0) + pad;
        let description_width = (ui.available_width() - fixed).max(description_min);
        {
            egui::ScrollArea::horizontal()
                .id_salt("tool-table-scroll")
                .show(ui, |ui| {
            egui::Grid::new("tool-table-direct")
                .num_columns(columns)
                .striped(true)
                .spacing([8.0, 5.0])
                .show(ui, |ui| {
                    header_label(ui, "T", tool_width);
                    header_label(ui, "DESCRIPTION", description_width);
                    if lathe {
                        header_label(ui, "X", number_width);
                        header_label(ui, "Z", number_width);
                        header_label(ui, "NOSE R", number_width);
                        header_label(ui, "ORIENT", number_width);
                    } else {
                        header_label(ui, "Ø", number_width);
                        header_label(ui, "LENGTH", number_width);
                        header_label(ui, "TYPE", kind_width);
                    }
                    if pocket {
                        header_label(ui, if lathe { "STATION" } else { "POCKET" }, number_width);
                    }
                    header_label(ui, "WRITE", write_width);
                    ui.end_row();

                    for (index, row) in rows.iter().enumerate() {
                        let selected = app.tools.selected_row == index;
                        let current = row.tool == current_tool;
                        let response = ui.selectable_label(
                            selected,
                            RichText::new(format!("T{}", row.tool))
                                .monospace()
                                .size(theme::font_size(12.0))
                                .color(if current { theme::SUCCESS } else { theme::FG }),
                        );
                        if response.clicked() {
                            app.tools.selected_row = index;
                        }
                        let draft = app
                            .tools
                            .drafts
                            .entry(row.tool)
                            .or_insert_with(|| ToolDraft::from_row(row));
                        text_cell(ui, writable, &mut draft.description, description_width);
                        if lathe {
                            for axis in ["X", "Z"] {
                                let text = draft.axes.entry(axis.into()).or_default();
                                number_cell(ui, writable, text, number_width);
                            }
                            number_cell(ui, writable, &mut draft.nose_radius, number_width);
                            number_cell(ui, writable, &mut draft.orientation, number_width);
                        } else {
                            number_cell(ui, writable, &mut draft.diameter, number_width);
                            number_cell(ui, writable, &mut draft.length, number_width);
                            ui.add_sized(
                                [kind_width, 24.0],
                                egui::Label::new(
                                    RichText::new(if row.kind.is_empty() {
                                        "—"
                                    } else {
                                        &row.kind
                                    })
                                    .size(theme::font_size(9.0))
                                    .color(theme::MUTED),
                                ),
                            );
                        }
                        if pocket {
                            number_cell(ui, writable, &mut draft.pocket, number_width);
                        }
                        if theme::action_button(ui, "WRITE", writable).clicked() {
                            write_tool = Some(row.tool);
                        }
                        ui.end_row();
                    }
                    if rows.is_empty() {
                        ui.label(
                            RichText::new("no tools in the table")
                                .size(theme::font_size(10.0))
                                .color(theme::MUTED),
                        );
                        ui.end_row();
                    }
                });
                });
        }
        if let Some(error) = &app.tools.error {
            ui.label(RichText::new(error).color(theme::ERROR));
        }
    });
    if let Some(tool) = write_tool {
        save_tool_row(app, tool);
    }
}

fn header_label(ui: &mut Ui, text: &str, width: f32) {
    ui.allocate_ui_with_layout(
        egui::Vec2::new(width, 16.0),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.label(
                RichText::new(text)
                    .size(theme::font_size(9.0))
                    .strong()
                    .color(theme::MUTED),
            );
        },
    );
}

fn text_cell(ui: &mut Ui, writable: bool, text: &mut String, width: f32) {
    ui.add_enabled_ui(writable, |ui| {
        ui.add_sized(
            [width, 24.0],
            egui::TextEdit::singleline(text)
                .font(egui::TextStyle::Monospace)
                .margin(egui::Margin::symmetric(4, 1)),
        );
    });
}

fn number_cell(ui: &mut Ui, writable: bool, text: &mut String, width: f32) {
    text_cell(ui, writable, text, width);
}

fn parse_number(text: &str, label: &str) -> Result<Option<f64>, String> {
    if text.trim().is_empty() {
        return Ok(None);
    }
    text.trim()
        .parse::<f64>()
        .map(Some)
        .map_err(|_| format!("{label}: invalid number {text:?}"))
}

fn save_tool_row(app: &mut LincApp, tool: u16) {
    let Some(mut row) = app
        .snapshot
        .tool_table
        .rows
        .iter()
        .find(|row| row.tool == tool)
        .cloned()
    else {
        app.tools.error = Some(format!("T{tool}: row no longer exists"));
        return;
    };
    let Some(draft) = app.tools.drafts.get(&tool).cloned() else {
        app.tools.error = Some(format!("T{tool}: no editable draft"));
        return;
    };
    let result = (|| -> Result<(), String> {
        row.description = draft.description.clone();
        row.geometry.diameter = parse_number(&draft.diameter, "diameter")?;
        row.geometry.length = parse_number(&draft.length, "length")?;
        row.geometry.nose_radius = parse_number(&draft.nose_radius, "nose radius")?;
        row.geometry.orientation = parse_number(&draft.orientation, "orientation")?
            .map(|value| value.clamp(0.0, 9.0) as u8);
        row.pocket = parse_number(&draft.pocket, "pocket")?.map(|value| value as u16);
        for axis in ["X", "Z", "I", "J"] {
            let text = draft.axes.get(axis).cloned().unwrap_or_default();
            match parse_number(&text, axis)? {
                Some(value) => {
                    row.geometry.values.insert(axis.to_string(), value);
                    match axis {
                        "I" => row.geometry.front_angle = Some(value),
                        "J" => row.geometry.back_angle = Some(value),
                        _ => {}
                    }
                }
                None => {
                    let remaining = row
                        .geometry
                        .values
                        .iter()
                        .filter(|(name, _)| *name != axis)
                        .map(|(name, value)| (name.to_string(), *value))
                        .collect::<Vec<_>>();
                    row.geometry.values = linc_core::model::AxisMap::from_entries(remaining);
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
    app.toast(format!("T{tool} tool table write queued"), theme::ACCENT);
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

pub fn set_current(app: &mut LincApp) {
    let tool = selected_tool(app);
    app.send(MachineCommand::SetCurrentTool { tool });
}
