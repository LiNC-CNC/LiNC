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
    // The page must end above the status line: reserve the bottom padding and
    // never ask for more height than is there.
    let content_height = (ui.available_height() - 14.0).max(240.0);
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
    // Never while a dialog or the tool-change prompt is up: this widget claims
    // focus every frame, and it was stealing it back from the dialog's buttons.
    let overlay_open = app.modal.is_some()
        || app.snapshot.tool_change.status == ToolChangeStatus::ManualPrompt;
    if !app.tools.editing && !overlay_open && !focus.has_focus() {
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
    let number_width = theme::measure(ui, "-000.0000", 11.0) + CELL_TEXT_PAD * 2.0;
    // The type is read at 9 points but the cell pads its text, and "DRILL" was
    // truncating at the bare measurement.
    let kind_width = theme::measure(ui, "DRILL", 9.0) + CELL_TEXT_PAD * 2.0 + 8.0;
    let mut columns = vec![
        ColumnSpec {
            cell: Cell::Tool,
            heading: "T",
            width: tool_width,
        },
        ColumnSpec {
            cell: Cell::Kind,
            heading: "TYPE",
            width: kind_width,
        },
    ];
    // Same order as probe_basic: offsets before the description, which takes the
    // remaining width at the end.
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
        for (field, heading) in [(Field::Length, "OFFSET"), (Field::Diameter, "Ø")] {
            columns.push(ColumnSpec {
                cell: Cell::Number(field),
                heading,
                width: number_width,
            });
        }
    }
    if pocket {
        columns.push(ColumnSpec {
            cell: Cell::Number(Field::Pocket),
            heading: if lathe { "STATION" } else { "POCKET" },
            width: number_width,
        });
    }
    columns.push(ColumnSpec {
        cell: Cell::Description,
        heading: "DESCRIPTION",
        width: tool_width,
    });
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
        Cell::Tool => {
            let _ = entry;
            draft.tool.clone()
        }
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
        Cell::Tool => Some(&mut draft.tool),
        Cell::Kind => None,
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

/// Commit the edited cell and write the row.
///
/// There is no separate write button: Enter edits and writes in one step. When
/// writes are locked the edit is refused before the draft changes, so the table
/// never shows a value that was not written.
fn commit_edit(app: &mut LincApp, columns: &[ColumnSpec]) {
    let row = app.tools.selected_row;
    let column = app.tools.selected_column;
    let Some(entry) = app.snapshot.tool_table.rows.get(row).cloned() else {
        app.tools.editing = false;
        return;
    };
    if !app.snapshot.writes_allowed() {
        app.tools.error = Some(write_refusal(app));
        app.tools.edit_buffer.clear();
        app.tools.editing = false;
        return;
    }
    if let Some(spec) = columns.get(column) {
        let buffer = std::mem::take(&mut app.tools.edit_buffer);
        if let Some(field) = cell_field(draft_for(app, entry.tool), spec.cell) {
            *field = buffer;
        }
    }
    app.tools.editing = false;
    save_tool_row(app, entry.tool);
}

fn write_refusal(app: &LincApp) -> String {
    if app.snapshot.panel.protect_effective() {
        "PROTECT ON — tool table writes are locked".into()
    } else if !app.snapshot.panel.connected || app.snapshot.panel.communication_error {
        "PANEL OFFLINE — tool table writes are locked".into()
    } else {
        "machine must be ON and idle in EDIT — tool table writes are locked".into()
    }
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
        // Page keys move by a screenful, as in every other list.
        if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
            move_row -= TABLE_PAGE;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
            move_row += TABLE_PAGE;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            edit = true;
        }
    });
    if move_row != 0 && rows > 0 {
        let target = app.tools.selected_row as isize + move_row;
        app.tools.selected_row = target.clamp(0, rows as isize - 1) as usize;
        // The table scrolls to follow the keyboard cursor; a click on a visible
        // row needs no scroll.
        app.tools.scroll_to_selected = true;
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
    let rows = app.snapshot.tool_table.rows.clone();
    let current_tool = app.snapshot.tool_change.current_tool;
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
        // Leave room for the table's own scrollbar: without it the columns sum
        // to more than the table has, and egui_extras rescales them, which puts
        // each column's text into its neighbour's padding.
        let description_width =
            (ui.available_width() - fixed - gaps - SCROLLBAR_ALLOWANCE).max(description_min);
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
            // A gap between columns keeps each cell's text and selector clear of
            // its neighbour; the row band is painted as one rect, so the gap
            // does not break the highlight.
            ui.spacing_mut().item_spacing.x = CELL_GAP;
            {
                let visuals = ui.visuals_mut();
                visuals.widgets.inactive.corner_radius = egui::CornerRadius::ZERO;
                visuals.widgets.hovered.corner_radius = egui::CornerRadius::ZERO;
                visuals.widgets.active.corner_radius = egui::CornerRadius::ZERO;
                visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::ZERO;
            }
            let painter = ui.painter().clone();
            // Room for the hint row under the table, inside the card: without a
            // bound the table takes the whole card and pushes it out.
            let table_height = (ui.available_height() - FOOTER_HEIGHT).max(120.0);
            let mut builder = TableBuilder::new(ui)
                .id_salt("tool-table-v3")
                .max_scroll_height(table_height)
                .striped(true)
                .resizable(false)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
            for column in &columns {
                builder = builder.column(ExtraColumn::exact(column.width));
            }
            if app.tools.scroll_to_selected {
                app.tools.scroll_to_selected = false;
                // `scroll_to_row` lives on the builder: the table computes its
                // scroll offsets before the body runs.
                builder = builder.scroll_to_row(selected_row, Some(egui::Align::Center));
            }
            let clicked = std::cell::Cell::new(None::<(usize, usize)>);
            builder
                .header(HEADER_HEIGHT, |mut header| {
                    for (position, column) in columns.iter().enumerate() {
                        let response = header.col(|ui| {
                            ui.label(
                                RichText::new(column.heading)
                                    .size(theme::font_size(9.0))
                                    .strong()
                                    .color(if position == selected_column {
                                        theme::ACCENT
                                    } else {
                                        theme::MUTED
                                    }),
                            );
                        });
                        if position == selected_column {
                            let rect = response.0;
                            painter.rect_filled(
                                egui::Rect::from_min_size(
                                    egui::pos2(rect.left(), rect.bottom() - theme::SELECTION_BAR),
                                    egui::vec2(rect.width(), theme::SELECTION_BAR),
                                ),
                                egui::CornerRadius::same(1),
                                theme::ACCENT,
                            );
                        }
                    }
                })
                .body(|body| {
                    body.rows(ROW_HEIGHT, rows.len(), |mut row| {
                        let index = row.index();
                        let entry = rows[index].clone();
                        // `TableRow::response` may only be called after a cell
                        // exists, so the row's geometry is taken from the cells.
                        let mut row_rect: Option<egui::Rect> = None;
                        let mut row_span: Option<(f32, f32)> = None;
                        for (position, column) in columns.iter().enumerate() {
                            let selected_cell = index == selected_row && position == selected_column;
                            let response = row.col(|ui| {
                                if selected_cell && editing {
                                    ui.add_space(CELL_TEXT_PAD);
                                    let edit = ui.add_sized(
                                        [column.width - CELL_TEXT_PAD, FIELD_HEIGHT],
                                        egui::TextEdit::singleline(&mut app.tools.edit_buffer)
                                            // Stable id: the shell checks it to
                                            // leave Home and End to the cell.
                                            .id(egui::Id::unique("linc-tool-cell"))
                                            .font(egui::TextStyle::Monospace)
                                            .margin(egui::Margin::symmetric(2, 0)),
                                    );
                                    if app.tools.edit_focus {
                                        edit.request_focus();
                                        app.tools.edit_focus = false;
                                    }
                                } else {
                                    // Air between the cell selector and the
                                    // text, in every row so nothing shifts when
                                    // the selection moves.
                                    ui.add_space(CELL_TEXT_PAD);
                                    if column.cell == Cell::Tool {
                                        ui.add_space(theme::SELECTION_BAR);
                                    }
                                    let text = cell_text(draft_for(app, entry.tool), &entry, column.cell);
                                    // Reserve the same air on the right as on the
                                    // left, and ellipsise rather than run under
                                    // the selector's frame.
                                    // Clamp the wrap width instead of sizing the
                                    // label: `add_sized` centres it, which broke the
                                    // left alignment.
                                    // A touch more on the right: the glyphs carry
                                    // no trailing bearing, so equal widths looked
                                    // tighter on that side.
                                    ui.set_max_width(
                                        (column.width - CELL_TEXT_PAD * 2.0 - RIGHT_GAP).max(8.0),
                                    );
                                    let label = egui::Label::new(
                                        RichText::new(text)
                                            .monospace()
                                            .size(theme::font_size(11.0))
                                            .color(if column.cell == Cell::Tool
                                                && entry.tool == current_tool
                                            {
                                                theme::SUCCESS
                                            } else if column.cell == Cell::Kind {
                                                theme::MUTED
                                            } else {
                                                theme::FG
                                            }),
                                    )
                                    .truncate();
                                    // Numbers align on the right so the decimal
                                    // points line up and a minus sign hangs into
                                    // the padding instead of shifting the digits.
                                    if matches!(column.cell, Cell::Number(_) | Cell::Tool) {
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.add(label);
                                            },
                                        );
                                    } else {
                                        ui.add(label);
                                    }
                                }
                            });
                            let rect = response.0;
                            row_rect.get_or_insert(rect);
                            if index == selected_row {
                                row_span = Some(match row_span {
                                    Some((left, right)) => (left.min(rect.left()), right.max(rect.right())),
                                    None => (rect.left(), rect.right()),
                                });
                            }
                            if selected_cell {
                                painter.rect_stroke(
                                    rect.shrink2(egui::vec2(SELECTION_INSET, 1.0)),
                                    egui::CornerRadius::same(theme::SELECTION_RADIUS as u8),
                                    egui::Stroke::new(1.5, theme::ACCENT),
                                    egui::StrokeKind::Inside,
                                );
                            }

                            if response.1.clicked() {
                                clicked.set(Some((index, position)));
                            }
                        }
                        if let (Some((left, right)), Some(row_rect)) = (row_span, row_rect) {
                            // One band across the row, so the gaps between
                            // columns are filled and the highlight is
                            // continuous instead of a row of boxes.
                            let band = egui::Rect::from_min_max(
                                egui::pos2(left, row_rect.top()),
                                egui::pos2(right, row_rect.bottom()),
                            );
                            painter.rect_filled(
                                band,
                                egui::CornerRadius::same((theme::SELECTION_RADIUS / 2.0) as u8),
                                theme::tint(theme::ACCENT, theme::SELECTION_ALPHA),
                            );
                            // Solid bar on the rim: the band alone is faint, and
                            // the bar is what the eye lands on.
                            painter.rect_filled(
                                egui::Rect::from_min_size(
                                    egui::pos2(left, row_rect.top()),
                                    egui::vec2(theme::SELECTION_BAR, row_rect.height()),
                                ),
                                egui::CornerRadius::same(1),
                                theme::ACCENT,
                            );
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
            ui.label(
                RichText::new("ARROWS MOVE · ENTER EDITS · ESC CANCELS")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
    });
}

/// Height of a table row, and of the field a cell opens for editing.
const ROW_HEIGHT: f32 = 26.0;
/// Rows a PageUp/PageDown moves: the table shows roughly this many at once.
const TABLE_PAGE: isize = 8;
/// Space kept under the table for the key hint, inside the card.
const FOOTER_HEIGHT: f32 = 26.0;
const FIELD_HEIGHT: f32 = 22.0;
const HEADER_HEIGHT: f32 = 18.0;

/// Faint washes under the selected row and column; the solid bars on the rim
/// carry the selection, so the wash must stay weak enough to read text through.
/// Air between a cell's edge and its text.
const CELL_TEXT_PAD: f32 = 8.0;
/// Gap between columns.
const CELL_GAP: f32 = 6.0;
/// How far the cell selector is drawn inside its cell.
const SELECTION_INSET: f32 = 1.0;
/// Room kept for the table's vertical scrollbar.
const SCROLLBAR_ALLOWANCE: f32 = 14.0;
/// Extra room on the right of a cell, on top of the text padding.
const RIGHT_GAP: f32 = 0.0;
/// Corner radius of the cell selector.

/// A translucent version of a solid colour. `from_rgba_unmultiplied` premultiplies
/// for us; a premultiplied constructor with a plain colour renders nearly solid.

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
        // The tool number is editable, so a row can be renumbered; it must parse
        // and must not collide with another row.
        let renumbered = draft.tool.trim().parse::<u16>().map_err(|_| {
            format!("T{tool}: invalid tool number {:?}", draft.tool)
        })?;
        if renumbered != tool
            && app
                .snapshot
                .tool_table
                .rows
                .iter()
                .any(|other| other.tool == renumbered && other.tool != tool)
        {
            return Err(format!("T{renumbered} already exists"));
        }
        row.tool = renumbered;
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
    let written = row.tool;
    app.send(MachineCommand::SetToolTableRow {
        row,
        expected_revision: app.snapshot.tool_table.revision,
    });
    // Follow the row if it was renumbered, so later edits target the new tool.
    if written != tool {
        if let Some(draft) = app.tools.drafts.remove(&tool) {
            app.tools.drafts.insert(written, draft);
        }
    }
    app.toast(
        format!("T{written} tool table write queued"),
        theme::ACCENT,
    );
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
                // Allowed while the program runs: this is the M6 handshake the
                // interpreter is waiting for, not motion.
                if app.snapshot.tool_change.confirmed {
                    ui.label(
                        RichText::new("CONFIRMED — WAITING FOR THE MACHINE")
                            .size(theme::font_size(11.0))
                            .color(theme::SUCCESS),
                    );
                } else if theme::accent_button(ui, "CONFIRM INSTALLED", app.snapshot.panel.connected)
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
