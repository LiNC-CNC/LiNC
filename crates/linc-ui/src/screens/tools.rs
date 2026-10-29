//! Tools page: standard LinuxCNC tool table, quick change and manual confirm.

use egui::{RichText, Ui};
use linc_core::command::MachineCommand;
use linc_core::model::{MachineVariant, ToolRow};

use crate::shell::LincApp;
use crate::state::ToolDraft;
use crate::theme;
use egui_extras::{Column as ExtraColumn, TableBuilder};

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    // No page title: the navigation names the page.
    // CURRENT; T1   PREPARED; T2 — the tool numbers carry the amber the axis
    // labels use, so they read as data rather than prose.
    ui.horizontal(|ui| {
        for (label, tool) in [
            ("CURRENT:", app.snapshot.tool_change.current_tool),
            ("PREPARED:", app.snapshot.tool_change.prepared_tool),
        ] {
            ui.label(
                RichText::new(label)
                    .size(theme::font_size(10.0))
                    .color(theme::MUTED),
            );
            ui.label(
                RichText::new(format!("T{tool}"))
                    .monospace()
                    .size(theme::font_size(12.0))
                    .color(theme::C3),
            );
            ui.add_space(12.0);
        }
    });
    // Keys are taken before the table's scroll area draws: a vertical scroll
    // area is focusable and consumes the up/down keys for its own scrolling.
    let lathe = app.profile.variant == MachineVariant::Lathe;
    let pocket = app.profile.changer.uses_pockets();
    let columns = column_specs(ui, lathe, pocket);
    if let Some(tool) = app.tools.pending_select_tool {
        if let Some(index) = app
            .snapshot
            .tool_table
            .rows
            .iter()
            .position(|row| row.tool == tool)
        {
            app.tools.selected_row = index;
            app.tools.scroll_to_selected = true;
            app.tools.pending_select_tool = None;
        }
    }
    let row_count = app.snapshot.tool_table.rows.len();
    if app.tools.selected_row >= row_count.max(1) {
        app.tools.selected_row = row_count.saturating_sub(1);
    }
    if app.tools.selected_column >= columns.len() {
        app.tools.selected_column = columns.len().saturating_sub(1);
    }
    handle_keys(app, ui, &columns);
    // The typed-number dialog lives over the table, not instead of it: the
    // table keeps drawing behind the window.
    draw_number_dialog(app, ui);
    ui.add_space(4.0);
    // No change/changer panes: the table is the page. Loading the selected tool
    // is the `T` shortcut below, and the manual-change prompt is drawn by the
    // shell when the controller asks for it.
    table_card(app, ui);
    // The table holds keyboard focus, and claims the keys it needs.
    //
    // With no focused widget egui treats the arrow keys as focus navigation and
    // removes them before any page code sees them. A focused widget whose
    // filter matches the arrows keeps them in the event stream, which is how
    // the table gets them. While a cell is being edited its field holds focus
    // instead.
    let focus_id = egui::Id::unique("tools-table-focus");
    let (focus_rect, _) =
        ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    let focus = ui.interact(
        focus_rect,
        focus_id,
        egui::Sense::focusable_noninteractive(),
    );
    // Never while a dialog or the tool-change prompt is up: this widget claims
    // focus every frame, and it was stealing it back from the dialog's buttons.
    let overlay_open = app.overlay_open();
    if !app.tools.editing && app.tools.kind_popup.is_none() && !overlay_open && !focus.has_focus() {
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
    let tool_width = theme::measure(ui, "T00000", 12.0) + pad;
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

fn cell_text(
    draft: &ToolDraft,
    entry: &linc_core::model::ToolRow,
    cell: Cell,
    kind_override: Option<&str>,
) -> String {
    match cell {
        Cell::Tool => {
            let _ = entry;
            draft.tool.clone()
        }
        Cell::Description => draft.description.clone(),
        Cell::Kind => {
            // The LinuxCNC tool table has no type column: the label is inferred
            // unless the operator picked one in the TYPE selector.
            let kind = kind_override.unwrap_or(&entry.kind);
            if kind.is_empty() {
                "—".to_string()
            } else {
                kind.to_string()
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
    if spec.cell == Cell::Kind {
        // The TYPE cell is not typed: Enter opens the selector at its rect.
        app.tools.kind_popup_pending = true;
        return;
    }
    let text = cell_text(draft_for(app, entry.tool), &entry, spec.cell, None);
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
    // The tool-number dialog owns the keyboard while it is open: its Enter,
    // Escape and Backspace are taken here, before the table and before the
    // field itself draws. It returns early, so no table key moves underneath.
    if app.tools.number.is_some() {
        number_dialog_keys(app, ui);
        return;
    }
    if app.overlay_open() {
        // The manual-change dialog owns the keyboard: no cursor movement, no
        // cell editor and no T underneath it.
        return;
    }
    if app.tools.kind_popup.is_some() {
        // The selector handles its own keys; the table must not move or edit
        // underneath it while it is open.
        return;
    }
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
    let mut load = false;
    let mut number = false;
    let mut new_tool = false;
    let mut delete_tool = false;
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
        // Bare `T`: load the selected row's tool and its length offset. The
        // editing branch returns above, so typing a `t` into a cell never
        // reaches this.
        if input.consume_key(egui::Modifiers::NONE, egui::Key::T) {
            load = true;
        }
        // The page-local typed tool number. `consume_key` matches the modifiers
        // exactly, so the bare `T` above is untouched; the chord comes from
        // `[keys] tool_number` and a profile can move it.
        if input.consume_key(app.keymap.tool_number.modifiers, app.keymap.tool_number.key) {
            number = true;
        }
        if input.consume_key(egui::Modifiers::CTRL, egui::Key::N) {
            new_tool = true;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Backspace) {
            delete_tool = true;
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
    if load {
        load_selected_tool(app);
    }
    if number {
        open_number_dialog(app);
    }
    if new_tool {
        create_tool(app);
    }
    if delete_tool {
        request_delete_tool(app);
    }
}

/// Load the selected row's tool with its length compensation: `Tn M6` and
/// `G43 Hn` in one backend command.
fn load_selected_tool(app: &mut LincApp) {
    let tool = selected_tool(app);
    if tool == 0 {
        app.tools.error = Some("T0 unloads the spindle; select a tool row".into());
        return;
    }
    if let Err(reason) = app.snapshot.issue_allowed() {
        app.tools.error = Some(reason);
        return;
    }
    app.tools.error = None;
    app.toast(format!("T{tool} M6 + G43 H{tool} queued"), theme::ACCENT);
    app.send(MachineCommand::LoadTool { tool });
}

/// Ctrl+N: write a fresh row with the next free tool number. The probe tool
/// number is skipped, and an ATC table gets the next free pocket.
fn create_tool(app: &mut LincApp) {
    if !app.snapshot.writes_allowed() {
        app.tools.error = Some(write_refusal(app));
        return;
    }
    let probe_tool = app.profile.config.probe.tool_number;
    let rows = &app.snapshot.tool_table.rows;
    let Some(tool) = (1..=u16::MAX)
        .find(|tool| *tool != probe_tool && !rows.iter().any(|row| row.tool == *tool))
    else {
        app.tools.error = Some("no free tool number".into());
        return;
    };
    let pocket = if app.profile.changer.uses_pockets() {
        let Some(pocket) =
            (1..=u16::MAX).find(|pocket| !rows.iter().any(|row| row.pocket == Some(*pocket)))
        else {
            app.tools.error = Some("no free pocket".into());
            return;
        };
        Some(pocket)
    } else {
        None
    };
    let mut row = ToolRow::new(tool);
    row.geometry.length = Some(0.0);
    row.geometry.diameter = Some(0.0);
    row.pocket = pocket;
    row.kind = if app.profile.variant == MachineVariant::Lathe {
        "TURN".into()
    } else {
        "MILL".into()
    };
    app.tools.error = None;
    app.tools.pending_select_tool = Some(tool);
    app.toast(format!("T{tool} created"), theme::ACCENT);
    app.send(MachineCommand::SetToolTableRow {
        row,
        old_tool: None,
        expected_revision: app.snapshot.tool_table.revision,
    });
}

/// Backspace: ask before removing the selected row. The probe tool is never
/// deletable because the probe routines depend on its table entry.
fn request_delete_tool(app: &mut LincApp) {
    let Some(row) = app.snapshot.tool_table.rows.get(app.tools.selected_row) else {
        app.tools.error = Some("select a tool row".into());
        return;
    };
    if row.tool == app.profile.config.probe.tool_number {
        app.tools.error = Some(format!(
            "T{} is the probe tool and cannot be deleted",
            row.tool
        ));
        return;
    }
    if row.tool == app.snapshot.tool_change.current_tool {
        app.tools.error = Some(format!(
            "T{} is the active tool and cannot be deleted",
            row.tool
        ));
        return;
    }
    if !app.snapshot.writes_allowed() {
        app.tools.error = Some(write_refusal(app));
        return;
    }
    app.tools.error = None;
    app.modal = Some(crate::state::Modal::ConfirmToolDelete {
        tool: row.tool,
        description: row.description.clone(),
    });
}

/// Open the typed tool-number dialog, prefilled with the tool in the spindle.
fn open_number_dialog(app: &mut LincApp) {
    app.tools.number = Some(crate::state::ToolNumberEntry::new(
        app.snapshot.tool_change.current_tool,
    ));
    app.tools.error = None;
}

/// Close it and give the keyboard back: the field held it, and the panel has no
/// Tab to walk out with.
fn close_number_dialog(app: &mut LincApp, ui: &mut Ui) {
    app.tools.number = None;
    ui.memory_mut(|memory| memory.stop_text_input());
}

/// The dialog's own keys, taken before the field and the table draw.
///
/// Enter confirms and Escape cancels; the digits and the Backspace reach the
/// focused field itself. Nothing else on the page reacts while this runs,
/// because `handle_keys` returns straight after it.
fn number_dialog_keys(app: &mut LincApp, ui: &mut Ui) {
    let (confirm, cancel) = ui.input_mut(|input| {
        // Escape is the configured back key on every profile; both are taken so
        // a rebound back key does not leave the dialog with no way out.
        let cancel = input.consume_key(app.keymap.back.modifiers, app.keymap.back.key)
            || input.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
        let confirm = input.consume_key(app.keymap.activate.modifiers, app.keymap.activate.key);
        (confirm, cancel)
    });
    if cancel {
        close_number_dialog(app, ui);
        return;
    }
    if !confirm {
        return;
    }
    let Some(mut entry) = app.tools.number.take() else {
        return;
    };
    match confirm_number(app, &entry.text) {
        Ok(()) => close_number_dialog(app, ui),
        Err(reason) => {
            // Refused: the dialog stays open with the text, and says why.
            entry.error = Some(reason);
            app.tools.number = Some(entry);
        }
    }
}

/// What a confirmed number sends, exactly as bare `T` loads a row: the same
/// `LoadTool` command, and the same precondition. `0` is accepted: the spindle
/// ends up empty and `G43 H0` applies no length compensation.
fn confirm_number(app: &mut LincApp, text: &str) -> Result<(), String> {
    let tool = tool_number_verdict(text, &app.snapshot.tool_table.rows)?;
    // A refused issue is reported inside the dialog rather than silently doing
    // nothing; `load_selected_tool` keeps the same gate for a table row.
    app.snapshot.issue_allowed()?;
    app.tools.error = None;
    app.toast(load_tool_message(tool), theme::ACCENT);
    app.send(MachineCommand::LoadTool { tool });
    Ok(())
}

/// What `LoadTool` is about to do, in the wording the bar and the toast use.
fn load_tool_message(tool: u16) -> String {
    if tool == 0 {
        // `LoadTool { tool: 0 }` is `T0 M6` and then `G43 H0`: tool 0 has no
        // length offset, so the spindle is empty with nothing compensated.
        "T0 M6 queued — spindle empty, no length compensation".to_string()
    } else {
        format!("T{tool} M6 + G43 H{tool} queued")
    }
}

/// The verdict on a typed tool number: the number to load, or why it is refused.
///
/// Only a number the table carries is accepted, plus `0` for an empty spindle.
/// Digits only: the panel keyboard can type a letter, and it must not be read
/// as a tool number.
fn tool_number_verdict(text: &str, rows: &[ToolRow]) -> Result<u16, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("type a tool number".into());
    }
    if !trimmed.chars().all(|digit| digit.is_ascii_digit()) {
        return Err(format!("{trimmed} is not a tool number"));
    }
    let tool = trimmed
        .parse::<u16>()
        .map_err(|_| format!("T{trimmed} is out of range"))?;
    if tool == 0 || rows.iter().any(|row| row.tool == tool) {
        Ok(tool)
    } else {
        Err(format!("T{tool} is not in the tool table"))
    }
}

/// The dialog itself: a `egui::Window` kept inside the screen, one line of
/// digits, the numbers the table accepts and - when the last confirm was
/// refused - the reason, in red. It is drawn from the page because the page
/// owns the command; `LincApp::overlay_open` is what makes the table, the cell
/// editor and the shell's global keys stand down behind it.
fn draw_number_dialog(app: &mut LincApp, ui: &mut Ui) {
    let Some(mut entry) = app.tools.number.take() else {
        return;
    };
    let accepted = accepted_numbers(&app.snapshot.tool_table.rows);
    let fresh = std::mem::replace(&mut entry.fresh, false);
    let error = entry.error.clone();
    let mut text = entry.text.clone();
    let cancel_key = app.keymap.back.label();
    let id = egui::Id::unique("linc-tool-number");
    egui::Window::new("TOOL NUMBER")
        .id(egui::Id::unique("linc-tool-number-dialog"))
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        // Wide enough for the VALID line on one row at 9 points: a wrapped
        // two-line hint made the dialog change height as the table grew.
        .default_width(400.0)
        .show(ui.ctx(), |ui| {
            ui.label(
                RichText::new(&accepted)
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            ui.add_space(6.0);
            let field = ui.add(
                egui::TextEdit::singleline(&mut text)
                    .id(id)
                    .font(egui::TextStyle::Monospace)
                    .desired_width(140.0)
                    .hint_text("tool"),
            );
            if fresh {
                // No click-to-focus and no Tab on this keyboard: the field
                // takes it on the frame the dialog opens, with the whole value
                // selected, so Enter alone re-loads it and the first digit
                // replaces it.
                field.request_focus();
                if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), id) {
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(text.chars().count()),
                        )));
                    state.store(ui.ctx(), id);
                }
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("ENTER LOADS · {cancel_key} CANCELS"))
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            if let Some(message) = &error {
                ui.label(
                    RichText::new(message)
                        .size(theme::font_size(10.0))
                        .color(theme::ERROR),
                );
            }
        });
    entry.text = text;
    // Put it back: the key handler re-inserts it too when a confirm was
    // refused, and this frame drew that same entry.
    app.tools.number = Some(entry);
}

/// The numbers a typed entry can be, in one short line inside the dialog.
fn accepted_numbers(rows: &[ToolRow]) -> String {
    const SHOWN: usize = 8;
    let mut text = String::from("VALID: ");
    if rows.is_empty() {
        text.push_str("none in the table");
    } else {
        let listed: Vec<String> = rows
            .iter()
            .take(SHOWN)
            .map(|row| format!("T{}", row.tool))
            .collect();
        text.push_str(&listed.join(" "));
        if rows.len() > SHOWN {
            text.push_str(&format!(" +{} more", rows.len() - SHOWN));
        }
    }
    text.push_str("  ·  0 = NO TOOL");
    text
}
fn table_card(app: &mut LincApp, ui: &mut Ui) {
    let pocket = app.profile.changer.uses_pockets();
    let lathe = app.profile.variant == MachineVariant::Lathe;
    // The hint row names the configured chord, not a hardcoded one.
    let number_key = app.keymap.tool_number.label();
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
        if let Some(error) = &app.tools.error {
            ui.label(
                RichText::new(error)
                    .size(theme::font_size(10.0))
                    .color(theme::ERROR),
            );
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
            // One text line per row with half a line between rows, the same
            // pitch as the offset and file tables.
            let row_font = egui::FontId::monospace(theme::font_size(11.0));
            let line_height = ui
                .painter()
                .layout_no_wrap("0".to_string(), row_font, theme::FG)
                .size()
                .y;
            let row_height = line_height.max(16.0);
            ui.spacing_mut().item_spacing.y = line_height * 0.5;
            {
                let visuals = ui.visuals_mut();
                visuals.widgets.inactive.corner_radius = egui::CornerRadius::ZERO;
                visuals.widgets.hovered.corner_radius = egui::CornerRadius::ZERO;
                visuals.widgets.active.corner_radius = egui::CornerRadius::ZERO;
                visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::ZERO;
            }
            let painter = ui.painter().clone();
            // Room for the hint row under the table, inside the card: without a
            // bound the table takes the whole card and pushes it out. The card's
            // own padding, the gap above the hint and the hint's line are part
            // of the budget too.
            let table_height = (ui.available_height() - FOOTER_HEIGHT - 44.0).max(120.0);
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
            let clicked_kind = std::cell::Cell::new(None::<(usize, egui::Rect)>);
            let pending_kind = std::cell::Cell::new(None::<(usize, egui::Rect)>);
            builder
                .header(HEADER_HEIGHT, |mut header| {
                    for (position, column) in columns.iter().enumerate() {
                        let response = header.col(|ui| {
                            let text = RichText::new(column.heading)
                                .size(theme::font_size(9.0))
                                .strong()
                                .color(if position == selected_column {
                                    theme::ACCENT
                                } else {
                                    theme::MUTED
                                });
                            if column.cell == Cell::Description {
                                ui.label(text);
                            } else {
                                ui.centered_and_justified(|ui| {
                                    ui.label(text);
                                });
                            }
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
                    body.rows(row_height, rows.len(), |mut row| {
                        let index = row.index();
                        let entry = rows[index].clone();
                        // `TableRow::response` may only be called after a cell
                        // exists, so the row's geometry is taken from the cells.
                        // `row.col` returns the cell Ui's content rect, which
                        // is empty when the content lives in a child Ui; take
                        // the cell's own rect from `ui.max_rect()` instead.
                        let first_rect = std::cell::Cell::new(None::<egui::Rect>);
                        let last_rect = std::cell::Cell::new(None::<egui::Rect>);
                        let selected_rect = std::cell::Cell::new(None::<egui::Rect>);
                        for (position, column) in columns.iter().enumerate() {
                            let selected_cell =
                                index == selected_row && position == selected_column;
                            let response = row.col(|ui| {
                                // One inner box, both paddings reserved: the
                                // selector then never touches a glyph on either
                                // side. The T column leaves extra room on the
                                // left for the row's rim bar.
                                let left_pad = CELL_TEXT_PAD
                                    + if column.cell == Cell::Tool {
                                        theme::SELECTION_BAR
                                    } else {
                                        0.0
                                    };
                                let inner_width =
                                    (column.width - left_pad - CELL_TEXT_PAD - RIGHT_GAP).max(8.0);
                                let cell_rect = ui.max_rect();
                                if position == 0 {
                                    first_rect.set(Some(cell_rect));
                                }
                                if position + 1 == columns.len() {
                                    last_rect.set(Some(cell_rect));
                                }
                                if selected_cell {
                                    selected_rect.set(Some(cell_rect));
                                }
                                if column.cell == Cell::Kind
                                    && selected_cell
                                    && app.tools.kind_popup_pending
                                {
                                    pending_kind.set(Some((index, cell_rect)));
                                }
                                // The label only covers the glyphs; a click on
                                // the rest of the cell selects it too, so the
                                // row under the pointer is the row `T` loads.
                                if !(selected_cell && editing) {
                                    let cell_response = ui.interact(
                                        cell_rect,
                                        egui::Id::unique(("tool-cell", index, position)),
                                        egui::Sense::click(),
                                    );
                                    if cell_response.clicked() {
                                        clicked.set(Some((index, position)));
                                        if column.cell == Cell::Kind {
                                            clicked_kind.set(Some((index, cell_rect)));
                                        }
                                    }
                                }
                                let right_aligned =
                                    matches!(column.cell, Cell::Number(_) | Cell::Tool);
                                let mut inner = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(egui::Rect::from_min_size(
                                            cell_rect.min + egui::vec2(left_pad, 0.0),
                                            egui::vec2(inner_width, cell_rect.height()),
                                        ))
                                        .layout(if right_aligned {
                                            egui::Layout::right_to_left(egui::Align::Center)
                                        } else {
                                            egui::Layout::left_to_right(egui::Align::Center)
                                        }),
                                );
                                if selected_cell && editing {
                                    let edit = inner.add_sized(
                                        [inner_width, row_height],
                                        egui::TextEdit::singleline(&mut app.tools.edit_buffer)
                                            // Stable id: the shell checks it to
                                            // leave Home and End to the cell.
                                            .id(egui::Id::unique("linc-tool-cell"))
                                            .font(egui::TextStyle::Monospace)
                                            .frame(egui::Frame::NONE)
                                            .margin(egui::Margin::symmetric(2, 0)),
                                    );
                                    if app.tools.edit_focus {
                                        edit.request_focus();
                                        // The whole value is selected, so typing
                                        // replaces it: the operator is entering a
                                        // tool dimension, not appending to one.
                                        let id = egui::Id::unique("linc-tool-cell");
                                        if let Some(mut state) =
                                            egui::text_edit::TextEditState::load(ui.ctx(), id)
                                        {
                                            state.cursor.set_char_range(Some(
                                                egui::text::CCursorRange::two(
                                                    egui::text::CCursor::new(0),
                                                    egui::text::CCursor::new(
                                                        app.tools.edit_buffer.chars().count(),
                                                    ),
                                                ),
                                            ));
                                            state.store(ui.ctx(), id);
                                        }
                                        app.tools.edit_focus = false;
                                    }
                                } else {
                                    let kind_override = app
                                        .preferences
                                        .tool_kinds
                                        .get(&entry.tool.to_string())
                                        .cloned();
                                    let text = cell_text(
                                        draft_for(app, entry.tool),
                                        &entry,
                                        column.cell,
                                        kind_override.as_deref(),
                                    );
                                    let label = egui::Label::new(
                                        RichText::new(text)
                                            .monospace()
                                            .size(theme::font_size(11.0))
                                            .color(
                                                if column.cell == Cell::Tool
                                                    && entry.tool == current_tool
                                                {
                                                    theme::SUCCESS
                                                } else if column.cell == Cell::Kind {
                                                    theme::MUTED
                                                } else {
                                                    theme::FG
                                                },
                                            ),
                                    )
                                    .truncate();
                                    // Numbers align on the right so the decimal
                                    // points line up and a minus sign hangs into
                                    // the padding instead of shifting the digits.
                                    let label_response = inner.add(label);
                                    // The label is the click target; the cell's
                                    // own response does not see a click that a
                                    // child consumed.
                                    if label_response.clicked() {
                                        clicked.set(Some((index, position)));
                                        if column.cell == Cell::Kind {
                                            clicked_kind.set(Some((index, cell_rect)));
                                        }
                                    }
                                }
                            });
                            let _ = response;
                        }
                        if index == selected_row {
                            if let (Some(first), Some(last)) = (first_rect.get(), last_rect.get()) {
                                // One band across the row, so the gaps between
                                // columns are filled and the highlight is
                                // continuous instead of a row of boxes.
                                let band = egui::Rect::from_min_max(
                                    egui::pos2(first.left(), first.top()),
                                    egui::pos2(last.right(), last.bottom()),
                                );
                                painter.rect_filled(
                                    band,
                                    egui::CornerRadius::same((theme::SELECTION_RADIUS / 2.0) as u8),
                                    theme::tint(theme::ACCENT, theme::SELECTION_ALPHA),
                                );
                                // Solid bar on the rim: the band alone is faint,
                                // and the bar is what the eye lands on.
                                painter.rect_filled(
                                    egui::Rect::from_min_size(
                                        egui::pos2(first.left(), first.top()),
                                        egui::vec2(theme::SELECTION_BAR, band.height()),
                                    ),
                                    egui::CornerRadius::same(1),
                                    theme::ACCENT,
                                );
                            }
                        }
                        if let Some(rect) = selected_rect.get() {
                            // Paint the active outline after the row band, and
                            // let it grow vertically so the edit caret sits
                            // inside a little padding rather than touching the
                            // outline.
                            let border = egui::Rect::from_min_max(
                                egui::pos2(
                                    rect.left() + SELECTION_INSET,
                                    rect.top() - ACTIVE_BORDER_PAD,
                                ),
                                egui::pos2(
                                    rect.right() - SELECTION_INSET,
                                    rect.bottom() + ACTIVE_BORDER_PAD,
                                ),
                            );
                            painter.rect_stroke(
                                border,
                                egui::CornerRadius::same(theme::SELECTION_RADIUS as u8),
                                egui::Stroke::new(
                                    1.5,
                                    if editing { theme::ERROR } else { theme::ACCENT },
                                ),
                                egui::StrokeKind::Inside,
                            );
                        }
                        let _ = &clicked;
                    });
                });
            // A dialog over the table takes the pointer with the keys: a click
            // behind it must not move the cursor, start an editor under the
            // field, or open the TYPE selector.
            let inert = app.overlay_open();
            if let Some((index, position)) = clicked.get().filter(|_| !inert) {
                if index != app.tools.selected_row || position != app.tools.selected_column {
                    app.tools.editing = false;
                    app.tools.selected_row = index;
                    app.tools.selected_column = position;
                }
            }
            let opened_kind = clicked_kind
                .get()
                .or_else(|| pending_kind.get())
                .filter(|_| !inert);
            if let Some((index, rect)) = opened_kind {
                app.tools.kind_popup = Some((index, rect));
                app.tools.kind_popup_pending = false;
            }
            draw_kind_popup(app, ui, opened_kind.is_some());
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "ARROWS MOVE · ENTER EDITS · ESC CANCELS · T LOADS + G43 · {number_key} TYPES NUMBER · + - * / ACCEPTED"
                ))
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
            );
        });
    });
}

/// The types the TYPE selector offers. The LinuxCNC tool table has no type
/// column, so these labels override the inferred one and live in preferences.
fn kind_choices(lathe: bool) -> &'static [&'static str] {
    if lathe {
        &["TURN", "BORING", "TOOL"]
    } else {
        &["MILL", "DRILL"]
    }
}

/// Popup selector for the TYPE cell. Opened by a click on the cell and anchored
/// under it; a choice updates the preference and closes.
fn draw_kind_popup(app: &mut LincApp, ui: &mut Ui, opened_now: bool) {
    let Some((row, anchor)) = app.tools.kind_popup else {
        return;
    };
    let Some(entry) = app.snapshot.tool_table.rows.get(row).cloned() else {
        app.tools.kind_popup = None;
        return;
    };
    let lathe = app.profile.variant == MachineVariant::Lathe;
    let current = app
        .preferences
        .tool_kinds
        .get(&entry.tool.to_string())
        .cloned()
        .unwrap_or_else(|| entry.kind.clone());
    let choices = kind_choices(lathe);
    if opened_now {
        app.tools.kind_popup_index = choices
            .iter()
            .position(|kind| *kind == current.as_str())
            .unwrap_or(0);
    }
    let mut index = app
        .tools
        .kind_popup_index
        .min(choices.len().saturating_sub(1));
    let mut chosen: Option<&'static str> = None;
    let mut close = false;
    ui.input_mut(|input| {
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
            index = (index + 1) % choices.len();
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
            index = (index + choices.len() - 1) % choices.len();
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
            chosen = Some(choices[index]);
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::Escape) {
            close = true;
        }
    });
    app.tools.kind_popup_index = index;
    let area = egui::Area::new(egui::Id::unique("tool-kind-selector"))
        .order(egui::Order::Foreground)
        .fixed_pos(anchor.left_bottom() + egui::vec2(0.0, 2.0))
        .show(ui.ctx(), |ui| {
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .corner_radius(egui::CornerRadius::same(4))
                .inner_margin(egui::Margin::same(4))
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    for (choice_index, kind) in choices.iter().enumerate() {
                        let selected = choice_index == index;
                        // Same face and size as the TYPE cells it edits, so the
                        // popup reads as an extension of the row.
                        let label = RichText::new(*kind)
                            .monospace()
                            .size(theme::font_size(11.0))
                            .color(if selected { theme::FG } else { theme::MUTED });
                        if ui.selectable_label(selected, label).clicked() {
                            chosen = Some(*kind);
                        }
                    }
                });
        });
    if let Some(kind) = chosen {
        app.preferences
            .tool_kinds
            .insert(entry.tool.to_string(), kind.to_string());
        app.touch_preferences();
        app.tools.kind_popup = None;
        return;
    }
    if close {
        app.tools.kind_popup = None;
        return;
    }
    // The click that opened the popup is outside it; ignore that frame so the
    // selector does not close the instant it appears.
    if opened_now {
        return;
    }
    let pointer = ui.input(|input| input.pointer.interact_pos());
    let clicked_outside = ui.input(|input| input.pointer.any_click())
        && pointer
            .map(|point| !area.response.rect.contains(point))
            .unwrap_or(false);
    if clicked_outside || ui.input(|input| input.key_pressed(egui::Key::Escape)) {
        app.tools.kind_popup = None;
    }
}

/// Rows a PageUp/PageDown moves: the table shows roughly this many at once.
const TABLE_PAGE: isize = 8;
/// Space kept under the table for the key hint, inside the card.
const FOOTER_HEIGHT: f32 = 26.0;
const HEADER_HEIGHT: f32 = 18.0;

/// Faint washes under the selected row and column; the solid bars on the rim
/// carry the selection, so the wash must stay weak enough to read text through.
/// Air between a cell's edge and its text.
const CELL_TEXT_PAD: f32 = 8.0;
/// Gap between columns.
const CELL_GAP: f32 = 6.0;
/// How far the cell selector is drawn inside its cell.
const SELECTION_INSET: f32 = 1.0;
/// Extra vertical room around the active cell border so the edit caret and
/// its glyph rows are never clipped by the outline.
const ACTIVE_BORDER_PAD: f32 = 2.0;
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
    // Tool dimensions take the same arithmetic as the offset table: a copied
    // `-25.15/2` can be typed straight into OFFSET or Ø.
    crate::state::eval_expression(text)
        .map(Some)
        .ok_or_else(|| format!("{label}: invalid number or expression {text:?}"))
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
    // A manual table has no pocket field in the UI, so an edit must keep the
    // loaded value: dropping it would write P0 over a real pocket.
    let uses_pockets = app.profile.changer.uses_pockets();
    let result = (|| -> Result<(), String> {
        // The tool number is editable, so a row can be renumbered; it must parse
        // and must not collide with another row.
        let renumbered = draft
            .tool
            .trim()
            .parse::<u16>()
            .map_err(|_| format!("T{tool}: invalid tool number {:?}", draft.tool))?;
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
        if uses_pockets {
            row.pocket = parse_number(&draft.pocket, "pocket")?.map(|value| value as u16);
        }
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
    if written != tool {
        if tool == app.profile.config.probe.tool_number {
            app.tools.drafts.remove(&tool);
            app.tools.error = Some(format!(
                "T{tool} is the probe tool and cannot be renumbered"
            ));
            return;
        }
        if tool == app.snapshot.tool_change.current_tool {
            app.tools.drafts.remove(&tool);
            app.tools.error = Some(format!(
                "T{tool} is the active tool and cannot be renumbered"
            ));
            return;
        }
    }
    app.send(MachineCommand::SetToolTableRow {
        row,
        old_tool: (written != tool).then_some(tool),
        expected_revision: app.snapshot.tool_table.revision,
    });
    // Follow the row if it was renumbered, so later edits target the new tool.
    if written != tool {
        if let Some(draft) = app.tools.drafts.remove(&tool) {
            app.tools.drafts.insert(written, draft);
        }
    }
    // Drop the draft so the cell shows the written value, not the expression
    // that produced it (`10+2` must read `12.0000` after Enter).
    app.tools.drafts.remove(&written);
    app.toast(format!("T{written} tool table write queued"), theme::ACCENT);
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

#[cfg(test)]
mod tests {
    use super::{accepted_numbers, load_tool_message, parse_number, tool_number_verdict};
    use linc_core::model::ToolRow;

    fn table() -> Vec<ToolRow> {
        vec![ToolRow::new(1), ToolRow::new(7), ToolRow::new(99)]
    }

    /// The typed tool number accepts a table number and 0, and refuses
    /// everything else with a reason the dialog can show: an unknown number, an
    /// empty entry and a non-numeric one are three different messages.
    #[test]
    fn a_typed_tool_number_is_a_table_number_or_zero() {
        assert_eq!(tool_number_verdict("7", &table()), Ok(7));
        assert_eq!(
            tool_number_verdict(" 99 ", &table()),
            Ok(99),
            "padding is trimmed"
        );
        assert_eq!(
            tool_number_verdict("0", &table()),
            Ok(0),
            "0 empties the spindle"
        );
        // The third row's tool is T99: a row index is not a tool number.
        assert_eq!(
            tool_number_verdict("2", &table()),
            Err("T2 is not in the tool table".to_string())
        );
        assert_eq!(
            tool_number_verdict("", &table()),
            Err("type a tool number".to_string())
        );
        assert_eq!(
            tool_number_verdict("   ", &table()),
            Err("type a tool number".to_string())
        );
        for text in ["abc", "7x", "-7", "1.5"] {
            assert!(
                tool_number_verdict(text, &table())
                    .is_err_and(|error| error.contains("is not a tool number")),
                "{text} must be refused as a non-number"
            );
        }
        assert!(
            tool_number_verdict("99999", &table()).is_err(),
            "a number past u16 is refused"
        );
        // An empty table still takes 0, and nothing else.
        assert_eq!(tool_number_verdict("0", &[]), Ok(0));
        assert!(tool_number_verdict("1", &[]).is_err());
    }

    /// The dialog's own text: the numbers the table has, and what the two
    /// conventions (a table row, 0 for none) mean.
    #[test]
    fn the_dialog_lists_the_numbers_it_accepts() {
        let text = accepted_numbers(&table());
        assert_eq!(text, "VALID: T1 T7 T99  ·  0 = NO TOOL");
        assert!(accepted_numbers(&[]).contains("none in the table"));
        let many: Vec<ToolRow> = (1..=12).map(ToolRow::new).collect();
        assert!(accepted_numbers(&many).contains("+4 more"));
        // The toast names what the load does, and 0 has no length compensation.
        assert_eq!(load_tool_message(7), "T7 M6 + G43 H7 queued");
        assert!(load_tool_message(0).contains("no length compensation"));
    }

    #[test]
    fn tool_dimensions_accept_arithmetic() {
        assert_eq!(parse_number("10+2.5", "offset").unwrap(), Some(12.5));
        assert_eq!(parse_number("6/2", "diameter").unwrap(), Some(3.0));
        assert_eq!(parse_number("-(1+2)*2", "offset").unwrap(), Some(-6.0));
        assert_eq!(parse_number("  ", "offset").unwrap(), None);
        assert!(parse_number("1/0", "offset").is_err());
        assert!(parse_number("12mm", "offset").is_err());
    }
}
