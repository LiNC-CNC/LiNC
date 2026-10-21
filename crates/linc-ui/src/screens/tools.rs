//! Tools page: standard LinuxCNC tool table, quick change and manual confirm.

use egui::{RichText, Ui};
use linc_core::command::MachineCommand;
use linc_core::model::{MachineVariant, ToolChangeStatus, ToolChangerKind};

use crate::shell::LincApp;
use crate::state::ToolDraft;
use egui_extras::{Column as ExtraColumn, TableBuilder};
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {

    // No page title: the navigation names the page.
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
    // Keys are taken before the table's scroll area draws: a vertical scroll
    // area is focusable and consumes the up/down keys for its own scrolling.
    let lathe = app.profile.variant == MachineVariant::Lathe;
    let pocket = app.profile.changer.uses_pockets();
    let columns = column_specs(ui, lathe, pocket);
    let row_count = app.snapshot.tool_table.rows.len();
    if app.tools.selected_row >= row_count.max(1) {
        app.tools.selected_row = row_count.saturating_sub(1);
    }
    if app.tools.selected_column >= columns.len() {
        app.tools.selected_column = columns.len().saturating_sub(1);
    }
    handle_keys(app, ui, &columns);
    ui.add_space(4.0);
    let available = ui.available_width();
    let content_height = (ui.available_height() - 8.0).max(240.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            egui::Vec2::new(available * 0.64, content_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| table_card(app, ui),
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
    // The table holds keyboard focus, and claims the keys it needs.
    //
    // With no focused widget egui treats the arrow keys as focus navigation and
    // removes them before any page code sees them. A focused widget whose
    // filter matches the arrows keeps them in the event stream, which is how
    // the table gets them. While a cell is being edited its field holds focus
    // instead.
    let focus_id = egui::Id::unique("tools-table-focus");
    let (focus_rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    let focus = ui.interact(focus_rect, focus_id, egui::Sense::focusable_noninteractive());
    if !app.tools.editing && !focus.has_focus() {
        focus.request_focus();
    }
    ui.memory_mut(|memory| {
        memory.set_focus_lock_filter(
            focus_id,
            egui::EventFilter {
                tab: true,
                horizontal_arrows: true,
                vertical_arrows: true,
                escape: true,
            },
        );
    });
}

/// What a table column holds.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cell {
    Tool,
    Description,
    Number(Field),
    Kind,
}

/// Numeric fields a cell can edit.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Field {
    Axis(&'static str),
    Diameter,
    Length,
    NoseRadius,
    Orientation,
    Pocket,
}

struct ColumnSpec {
    cell: Cell,
    heading: &'static str,
    width: f32,
}

fn column_specs(ui: &Ui, lathe: bool, pocket: bool) -> Vec<ColumnSpec> {
    let pad = theme::CELL_PAD;
    let tool_width = theme::measure(ui, "T00", 12.0) + pad;
    let number_width = theme::measure(ui, "-000.00", 11.0) + 12.0;
    let kind_width = theme::measure(ui, "DRILL", 9.0) + 12.0;
    let mut columns = vec![
        ColumnSpec {
            cell: Cell::Tool,
            heading: "T",
            width: tool_width,
        },
        ColumnSpec {
            cell: Cell::Description,
            heading: "DESCRIPTION",
            width: tool_width,
        },
    ];
    if lathe {
        for axis in ["X", "Z"] {
            columns.push(ColumnSpec {
                cell: Cell::Number(Field::Axis(axis)),
                heading: if axis == "X" { "X" } else { "Z" },
                width: number_width,
            });
        }
        for (field, heading) in [
            (Field::NoseRadius, "NOSE R"),
            (Field::Orientation, "ORIENT"),
        ] {
            columns.push(ColumnSpec {
                cell: Cell::Number(field),
                heading,
                width: number_width,
            });
        }
    } else {
        for (field, heading) in [(Field::Diameter, "Ø"), (Field::Length, "LENGTH")] {
            columns.push(ColumnSpec {
                cell: Cell::Number(field),
                heading,
                width: number_width,
            });
        }
        columns.push(ColumnSpec {
            cell: Cell::Kind,
            heading: "TYPE",
            width: kind_width,
        });
    }
    if pocket {
        columns.push(ColumnSpec {
            cell: Cell::Number(Field::Pocket),
            heading: if lathe { "STATION" } else { "POCKET" },
            width: number_width,
        });
    }
    columns
}

fn draft_for(app: &mut LincApp, tool: u16) -> &mut ToolDraft {
    let row = app
        .snapshot
        .tool_table
        .rows
        .iter()
        .find(|row| row.tool == tool)
        .cloned();
    app.tools
        .drafts
        .entry(tool)
        .or_insert_with(|| row.map(|row| ToolDraft::from_row(&row)).unwrap_or_default())
}

/// Text shown in a cell while navigating.
/// Four decimals, as the tool table and probe_basic show them.
fn format_cell(text: &str) -> String {
    text.trim()
        .parse::<f64>()
        .map(|value| format!("{value:.4}"))
        .unwrap_or_else(|_| text.to_string())
}

fn cell_text(draft: &ToolDraft, entry: &linc_core::model::ToolRow, cell: Cell) -> String {
    match cell {
        Cell::Tool => format!("T{}", entry.tool),
        Cell::Description => draft.description.clone(),
        Cell::Kind => {
            if entry.kind.is_empty() {
                "—".to_string()
            } else {
                entry.kind.clone()
            }
        }
        Cell::Number(field) => format_cell(&match field {
            Field::Axis(axis) => draft.axes.get(axis).cloned().unwrap_or_default(),
            Field::Diameter => draft.diameter.clone(),
            Field::Length => draft.length.clone(),
            Field::NoseRadius => draft.nose_radius.clone(),
            Field::Orientation => draft.orientation.clone(),
            Field::Pocket => draft.pocket.clone(),
        }),
    }
}

/// The draft field a cell edits, if it edits one.
fn cell_field(draft: &mut ToolDraft, cell: Cell) -> Option<&mut String> {
    match cell {
        Cell::Tool | Cell::Kind => None,
        Cell::Description => Some(&mut draft.description),
        Cell::Number(field) => Some(match field {
            Field::Axis(axis) => draft.axes.entry(axis.to_string()).or_default(),
            Field::Diameter => &mut draft.diameter,
            Field::Length => &mut draft.length,
            Field::NoseRadius => &mut draft.nose_radius,
            Field::Orientation => &mut draft.orientation,
            Field::Pocket => &mut draft.pocket,
        }),
    }
}

fn begin_edit(app: &mut LincApp, columns: &[ColumnSpec]) {
    let row = app.tools.selected_row;
    let column = app.tools.selected_column;
    let Some(entry) = app.snapshot.tool_table.rows.get(row).cloned() else {
        return;
    };
    let Some(spec) = columns.get(column) else {
        return;
    };
    let text = cell_text(draft_for(app, entry.tool), &entry, spec.cell);
    if cell_field(draft_for(app, entry.tool), spec.cell).is_some() {
        app.tools.edit_buffer = text;
        app.tools.editing = true;
        app.tools.edit_focus = true;
    }
}

fn commit_edit(app: &mut LincApp, columns: &[ColumnSpec]) {
    let row = app.tools.selected_row;
    let column = app.tools.selected_column;
    if let Some(entry) = app.snapshot.tool_table.rows.get(row).cloned() {
        if let Some(spec) = columns.get(column) {
            let buffer = std::mem::take(&mut app.tools.edit_buffer);
            if let Some(field) = cell_field(draft_for(app, entry.tool), spec.cell) {
                *field = buffer;
            }
        }
    }
    app.tools.editing = false;
}

/// Table keys, consumed before the table draws so no widget takes them first.
fn handle_keys(app: &mut LincApp, ui: &mut Ui, columns: &[ColumnSpec]) {
    if app.tools.editing {
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
            commit_edit(app, columns);
        } else if cancel {
            app.tools.edit_buffer.clear();
            app.tools.editing = false;
        }
        return;
    }
    let rows = app.snapshot.tool_table.rows.len();
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
    if move_row != 0 && rows > 0 {
        let target = app.tools.selected_row as isize + move_row;
        app.tools.selected_row = target.clamp(0, rows as isize - 1) as usize;
    }
    if move_column != 0 {
        let target = app.tools.selected_column as isize + move_column;
        app.tools.selected_column = target.clamp(0, columns.len() as isize - 1) as usize;
    }
    if edit {
        begin_edit(app, columns);
    }
}

fn table_card(app: &mut LincApp, ui: &mut Ui) {
    let pocket = app.profile.changer.uses_pockets();
    let lathe = app.profile.variant == MachineVariant::Lathe;
    let writable = app.snapshot.writes_allowed();
    let rows = app.snapshot.tool_table.rows.clone();
    let current_tool = app.snapshot.tool_change.current_tool;
    let mut write_selected = false;
    theme::card(ui, |ui| {
        let mut columns = column_specs(ui, lathe, pocket);
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
        // The description takes what the fixed columns leave.
        let fixed: f32 = columns
            .iter()
            .filter(|column| column.cell != Cell::Description)
            .map(|column| column.width)
            .sum();
        let description_min = theme::measure(ui, "DESCRIPTION_DESCRIPTION", 11.0);
        let gaps = ui.spacing().item_spacing.x * columns.len().saturating_sub(1) as f32;
        let description_width = (ui.available_width() - fixed - gaps).max(description_min);
        for column in &mut columns {
            if column.cell == Cell::Description {
                column.width = description_width;
            }
        }
        let selected_row = app.tools.selected_row;
        let selected_column = app.tools.selected_column;
        let editing = app.tools.editing;
        if rows.is_empty() {
            ui.label(
                RichText::new("no tools in the table")
                    .size(theme::font_size(10.0))
                    .color(theme::MUTED),
            );
        } else {
            ui.spacing_mut().item_spacing.x = 0.0;
            {
                let visuals = ui.visuals_mut();
                visuals.widgets.inactive.corner_radius = egui::CornerRadius::ZERO;
                visuals.widgets.hovered.corner_radius = egui::CornerRadius::ZERO;
                visuals.widgets.active.corner_radius = egui::CornerRadius::ZERO;
                visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::ZERO;
            }
            let painter = ui.painter().clone();
            let mut builder = TableBuilder::new(ui)
                .id_salt("tool-table-v3")
                .striped(true)
                .resizable(false)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
            for column in &columns {
                builder = builder.column(ExtraColumn::exact(column.width));
            }
            let clicked = std::cell::Cell::new(None::<(usize, usize)>);
            builder
                .header(16.0, |mut header| {
                    for column in &columns {
                        header.col(|ui| {
                            ui.label(
                                RichText::new(column.heading)
                                    .size(theme::font_size(9.0))
                                    .strong()
                                    .color(theme::MUTED),
                            );
                        });
                    }
                })
                .body(|body| {
                    body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                        let index = row.index();
                        let entry = rows[index].clone();
                        row.set_selected(index == selected_row);
                        for (position, column) in columns.iter().enumerate() {
                            let selected_cell = index == selected_row && position == selected_column;
                            let response = row.col(|ui| {
                                if selected_cell && editing {
                                    let edit = ui.add_sized(
                                        [column.width, FIELD_HEIGHT],
                                        egui::TextEdit::singleline(&mut app.tools.edit_buffer)
                                            .font(egui::TextStyle::Monospace)
                                            .margin(egui::Margin::symmetric(2, 0)),
                                    );
                                    if app.tools.edit_focus {
                                        edit.request_focus();
                                        app.tools.edit_focus = false;
                                    }
                                } else {
                                    let text = cell_text(draft_for(app, entry.tool), &entry, column.cell);
                                    ui.label(
                                        RichText::new(text)
                                            .monospace()
                                            .size(theme::font_size(11.0))
                                            .color(if column.cell == Cell::Tool && entry.tool == current_tool {
                                                theme::SUCCESS
                                            } else if column.cell == Cell::Kind {
                                                theme::MUTED
                                            } else {
                                                theme::FG
                                            }),
                                    );
                                }
                            });
                            if selected_cell {
                                painter.rect_stroke(
                                    response.0,
                                    egui::CornerRadius::ZERO,
                                    egui::Stroke::new(1.0, theme::ACCENT),
                                    egui::StrokeKind::Inside,
                                );
                            }
                            if response.1.clicked() {
                                clicked.set(Some((index, position)));
                            }
                        }
                        let _ = &clicked;
                    });
                });
            if let Some((index, position)) = clicked.get() {
                if index != app.tools.selected_row || position != app.tools.selected_column {
                    app.tools.editing = false;
                    app.tools.selected_row = index;
                    app.tools.selected_column = position;
                }
            }
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let tool = rows
                .get(app.tools.selected_row)
                .map(|row| row.tool)
                .unwrap_or(0);
            if theme::action_button(ui, &format!("WRITE T{tool}"), writable && !rows.is_empty())
                .clicked()
            {
                write_selected = true;
            }
            ui.label(
                RichText::new("ARROWS MOVE · ENTER EDITS · ESC CANCELS")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
    });
    if write_selected {
        if let Some(row) = rows.get(app.tools.selected_row) {
            let tool = row.tool;
            save_tool_row(app, tool);
        }
    }
}

/// Height of a table row, and of the field a cell opens for editing.
const ROW_HEIGHT: f32 = 26.0;
const FIELD_HEIGHT: f32 = 22.0;

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
