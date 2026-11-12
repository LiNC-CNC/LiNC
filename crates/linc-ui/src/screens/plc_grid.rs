//! The ZX-PLC ladder grid: cells, cursor, element picker, operand list.
//!
//! The page keeps the state (`cell_cursor`, `picker`, `operand_pick`); this
//! module draws the grid and owns the keys while the editor overlay is open.
//! Element letters live inside the picker rather than on the page, so they can
//! never collide with a shell shortcut or a text field.

use egui::{Align2, Color32, FontId, RichText, Sense, Stroke, Ui, Vec2};
use linc_plc_core::grid::{Cell, POSITIONS};
use linc_plc_core::{Coil, CoilKind, PouLanguage, Rung};

use crate::screens::plc::{Action, CompareEdit, ComputeEdit, PlcPage, PlcUi, RAIL_GROUP_GAP};
use crate::shell::LincApp;
use crate::theme;

/// Keys the ladder editor owns. Consumed before any widget sees them.
pub fn editor_keys(app: &mut LincApp, ui: &Ui, actions: &mut Vec<Action>) {
    if app.plc.page == PlcPage::Trend {
        // Z cycles the window, as the manual's trend page does; the daemon
        // restarts the history when the window changes.
        let zoom = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Z));
        if zoom {
            app.plc.trend_window_ms = match app.plc.trend_window_ms {
                4_000 => 20_000,
                20_000 => 100_000,
                _ => 4_000,
            };
            app.plc.trend_remote.samples.clear();
        }
        return;
    }
    // Escape always gives the keyboard back to the grid: a property field that
    // took it must never be able to wedge the editor.
    if app.plc.text_editing
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
    {
        app.plc.text_editing = false;
        return;
    }
    // Any modal owns the frame's keys: the element list, the operand and coil
    // lists, and the compare/compute and box windows. Without the last two an
    // Enter that advanced a stage here also opened the element list there.
    if app.plc.page != PlcPage::Edit
        || app.plc.picker.is_some()
        || app.plc.operand_pick.is_some()
        || app.plc.coil_pick.is_some()
        || app.plc.compare_edit.is_some()
        || app.plc.compute_edit.is_some()
        || app.plc.block_edit.is_some()
    {
        return;
    }
    // A click anywhere on the page takes the keyboard back. A text field left
    // focused by an earlier visit otherwise kept `text_editing` true and the grid
    // ignored every key, which looked like a dead editor rather than a lost focus.
    if ui.input(|input| input.pointer.any_pressed()) {
        ui.ctx().memory_mut(|memory| {
            if let Some(id) = memory.focused() {
                memory.surrender_focus(id);
            }
        });
        app.plc.text_editing = false;
    }
    if app.plc.text_editing {
        return;
    }
    let ld = app
        .plc
        .current_pou()
        .map(|pou| pou.language == PouLanguage::Ld)
        .unwrap_or(false);
    if !ld {
        return;
    }
    let sections = app.plc.current_pou_sections();
    if sections.is_empty() {
        return;
    }
    let rows = sections
        .iter()
        .map(|section| section.rungs.len())
        .collect::<Vec<_>>();
    let mut cursor = app.plc.cell_cursor.unwrap_or((0, 0, 0));
    cursor.0 = cursor.0.min(rows.len() - 1);
    cursor.1 = cursor.1.min(rows[cursor.0].saturating_sub(1));
    cursor.2 = cursor.2.min(POSITIONS);

    let (left, right, up, down, enter, delete, page_up, page_down) = ui.input_mut(|input| {
        (
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft)
                || input.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowLeft),
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight)
                || input.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowRight),
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp)
                || input.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowUp),
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
                || input.consume_key(egui::Modifiers::CTRL, egui::Key::ArrowDown),
            input.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            input.consume_key(egui::Modifiers::NONE, egui::Key::Backspace),
            input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp),
            input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown),
        )
    });
    let (undo, redo) = ui.input_mut(|input| {
        (
            input.consume_key(egui::Modifiers::CTRL, egui::Key::Z),
            input.consume_key(egui::Modifiers::CTRL, egui::Key::Y),
        )
    });
    let mark = ui.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::A));
    if mark {
        app.plc.cell_mark = if app.plc.cell_mark.is_some() {
            None
        } else {
            Some(cursor)
        };
    }
    let (copy, paste, xref, watch) = ui.input_mut(|input| {
        // egui-winit turns Ctrl+X into a Cut event for the clipboard, so the key
        // itself never arrives; the cut is the cross-reference chord here.
        let cut = input
            .events
            .iter()
            .any(|event| matches!(event, egui::Event::Cut));
        if cut {
            input
                .events
                .retain(|event| !matches!(event, egui::Event::Cut));
        }
        (
            input.consume_key(egui::Modifiers::CTRL, egui::Key::C),
            input.consume_key(egui::Modifiers::CTRL, egui::Key::V),
            input.consume_key(egui::Modifiers::CTRL, egui::Key::X) || cut,
            input.consume_key(egui::Modifiers::CTRL, egui::Key::W),
        )
    });
    // ZX element letters. They live on the page now that the editor overlay owns
    // the keyboard, and a text field's focus stops the handler above.
    let letter = |input: &mut egui::InputState, key: egui::Key| {
        input.consume_key(egui::Modifiers::NONE, key)
    };
    let (
        contact,
        negated,
        wire,
        link,
        edge_p,
        edge_n,
        ton,
        tof,
        tp,
        ctu,
        ctd,
        compare,
        compute,
        set_coil,
        reset_coil,
    ) = ui.input_mut(|input| {
        (
            letter(input, egui::Key::C),
            letter(input, egui::Key::X),
            letter(input, egui::Key::W),
            letter(input, egui::Key::V),
            letter(input, egui::Key::P),
            letter(input, egui::Key::N),
            letter(input, egui::Key::T),
            letter(input, egui::Key::F),
            letter(input, egui::Key::U),
            letter(input, egui::Key::K),
            letter(input, egui::Key::D),
            letter(input, egui::Key::G),
            letter(input, egui::Key::M),
            letter(input, egui::Key::S),
            letter(input, egui::Key::R),
        )
    });
    // Ctrl+Enter adds a row below the cursor, the manual's SS+ENTER substitute.
    // It is consumed here, before the page draws any widget, so a focused field
    // cannot take the chord first.
    if ui.input_mut(|input| {
        input.consume_key(egui::Modifiers::CTRL, egui::Key::Enter)
            || input.consume_key(egui::Modifiers::ALT, egui::Key::Enter)
    }) {
        actions.push(Action::AddRung);
        app.plc.cell_cursor = Some((cursor.0, cursor.1 + 1, 0));
        return;
    }
    // Space holds a contact's bit for the preview: the key lives on the cell, so
    // no bank select or bit number is needed.
    let space = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Space));
    if space && cursor.2 < POSITIONS {
        let operand = sections
            .get(cursor.0)
            .and_then(|section| section.rungs.get(cursor.1))
            .and_then(|rung| rung.cells.get(cursor.2))
            .and_then(|cell| cell.operand())
            .map(str::to_string);
        if let Some(name) = operand {
            let held = app
                .plc
                .preview
                .iter()
                .find(|(held, _)| *held == name)
                .map(|(_, value)| *value)
                .or_else(|| app.plc.bit_value(&name))
                .unwrap_or(false);
            app.plc.preview.retain(|(held, _)| *held != name);
            app.plc.preview.push((name, !held));
        }
    }
    let insert = ui.input_mut(|input| letter(input, egui::Key::I));
    // `/` on an empty row starts a network comment row, as the manual does.
    let slash = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Slash));
    if slash {
        let empty = sections
            .get(cursor.0)
            .and_then(|section| section.rungs.get(cursor.1))
            .map(|rung| {
                rung.coil.var.trim().is_empty()
                    && rung.cells.iter().all(|cell| matches!(cell, Cell::Wire))
            })
            .unwrap_or(false);
        if empty {
            actions.push(Action::AddCommentRow {
                section: cursor.0,
                rung: cursor.1,
            });
            return;
        }
    }
    let first = sections
        .get(cursor.0)
        .and_then(|section| section.rungs.get(cursor.1))
        .and_then(|rung| rung.cells.get(cursor.2))
        .cloned();
    // Movement comes first: the coil branch below returns, and while it did so
    // before the arrow handling the cursor could never leave the coil at all.
    if left || right || up || down || page_up || page_down {
        // A page key moves by a screenful, as in every other list. The scroll
        // to the cursor happens after the draw, once the cell's rect is known.
        if page_up {
            cursor.1 = cursor.1.saturating_sub(5);
        }
        if page_down {
            cursor.1 = (cursor.1 + 5).min(rows[cursor.0].saturating_sub(1));
        }
        if right {
            cursor.2 = (cursor.2 + 1).min(POSITIONS);
        }
        if left {
            cursor.2 = cursor.2.saturating_sub(1);
        }
        if down {
            if cursor.1 + 1 < rows[cursor.0] {
                cursor.1 += 1;
            } else if cursor.0 + 1 < rows.len() {
                cursor.0 += 1;
                cursor.1 = 0;
            }
        }
        if up {
            if cursor.1 > 0 {
                cursor.1 -= 1;
            } else if cursor.0 > 0 {
                cursor.0 -= 1;
                cursor.1 = rows[cursor.0].saturating_sub(1);
            }
        }
        app.plc.cell_cursor = Some(cursor);
        app.plc.scroll_to_cursor = true;
        return;
    }
    if cursor.2 >= POSITIONS {
        // Enter on the coil edits its operand. This branch returns before the
        // general Enter handling, so the coil's Enter lives here.
        if enter {
            let var = sections
                .get(cursor.0)
                .and_then(|section| section.rungs.get(cursor.1))
                .map(|rung| rung.coil.var.clone())
                .unwrap_or_default();
            app.plc.operand_cursor = coil_position(&app.plc, &var);
            app.plc.operand_filter.clear();
            app.plc.coil_pick = Some((cursor.0, cursor.1));
        }
        let kind = if contact {
            Some(CoilKind::Normal)
        } else if negated {
            Some(CoilKind::Negated)
        } else if set_coil {
            Some(CoilKind::Set)
        } else if reset_coil {
            Some(CoilKind::Reset)
        } else {
            None
        };
        if let Some(kind) = kind {
            let var = sections
                .get(cursor.0)
                .and_then(|section| section.rungs.get(cursor.1))
                .map(|rung| rung.coil.var.clone())
                .unwrap_or_default();
            actions.push(Action::SetCoil {
                section: cursor.0,
                rung: cursor.1,
                coil: Coil::new(var.clone(), kind),
            });
            app.plc.operand_cursor = operand_position(&app.plc, &var);
            app.plc.operand_filter.clear();
            app.plc.coil_pick = Some((cursor.0, cursor.1));
        }
        return;
    }
    let mut element: Option<Cell> = None;
    let mut pick_operand: Option<Cell> = None;
    if contact {
        pick_operand = Some(Cell::Contact {
            var: String::new(),
            negated: false,
        });
    }
    if negated {
        pick_operand = Some(Cell::Contact {
            var: String::new(),
            negated: true,
        });
    }
    if edge_p {
        pick_operand = Some(Cell::Edge {
            var: String::new(),
            rising: true,
        });
    }
    if edge_n {
        pick_operand = Some(Cell::Edge {
            var: String::new(),
            rising: false,
        });
    }
    if wire {
        element = Some(Cell::Wire);
    }
    let minus = ui.input_mut(|input| letter(input, egui::Key::Minus));
    if minus {
        element = Some(Cell::Wire);
    }
    // V toggles: a link on this position becomes a wire again.
    if link {
        let current = sections
            .get(cursor.0)
            .and_then(|section| section.rungs.get(cursor.1))
            .and_then(|rung| rung.cells.get(cursor.2))
            .cloned();
        if matches!(current, Some(Cell::Link)) {
            element = Some(Cell::Wire);
        } else {
            element = Some(Cell::Link);
            let has_next = sections
                .get(cursor.0)
                .map(|section| cursor.1 + 1 < section.rungs.len())
                .unwrap_or(false);
            if !has_next {
                actions.push(Action::AddRowAfter {
                    section: cursor.0,
                    rung: cursor.1,
                });
            }
        }
    }
    // A letter on an existing box edits it instead of replacing it.
    if (ton || tof || tp || ctu || ctd) && cursor.2 < POSITIONS {
        let existing = sections
            .get(cursor.0)
            .and_then(|section| section.rungs.get(cursor.1))
            .and_then(|rung| rung.cells.get(cursor.2));
        if matches!(existing, Some(Cell::Block { .. })) {
            app.plc.block_edit = Some((cursor.0, cursor.1, cursor.2));
            return;
        }
    }
    if ton || tof || tp {
        let block = if ton {
            "TON"
        } else if tof {
            "TOF"
        } else {
            "TP"
        };
        element = Some(Cell::Block {
            block: block.to_string(),
            instance: format!("{block}0"),
            preset: "T#1s".to_string(),
        });
    }
    if ctu || ctd {
        let block = if ctu { "CTU" } else { "CTD" };
        element = Some(Cell::Block {
            block: block.to_string(),
            instance: format!("{block}0"),
            preset: "10".to_string(),
        });
    }
    // G on an existing compare cell opens its window, as a block letter edits
    // the box already there instead of replacing it with defaults.
    if compare && cursor.2 < POSITIONS {
        let existing = sections
            .get(cursor.0)
            .and_then(|section| section.rungs.get(cursor.1))
            .and_then(|rung| rung.cells.get(cursor.2));
        if let Some(Cell::Compare { op, left, right }) = existing {
            app.plc.compare_edit = Some(CompareEdit {
                section: cursor.0,
                rung: cursor.1,
                position: cursor.2,
                op: op.clone(),
                left: left.clone(),
                right: right.clone(),
                focus: 0,
                error: None,
            });
            return;
        }
    }
    if compare {
        element = Some(Cell::Compare {
            op: "GT".to_string(),
            left: "MW0".to_string(),
            right: "0".to_string(),
        });
    }
    if compute {
        element = Some(Cell::Compute {
            op: "ADD".to_string(),
            target: "MW0".to_string(),
            left: "MW0".to_string(),
            right: "1".to_string(),
        });
    }
    if let Some(cell) = pick_operand {
        if cursor.2 < POSITIONS {
            let name = cell.operand().unwrap_or("").to_string();
            app.plc.operand_cursor = operand_position(&app.plc, &name);
            app.plc.operand_filter.clear();
            app.plc.operand_pick = Some((cursor.0, cursor.1, cursor.2, cell));
        }
    } else if let Some(cell) = element {
        if cursor.2 < POSITIONS {
            app.plc.operand_filter.clear();
            app.plc.cell_cursor = Some((cursor.0, cursor.1, (cursor.2 + 1).min(POSITIONS)));
            actions.push(if insert {
                Action::InsertCell {
                    section: cursor.0,
                    rung: cursor.1,
                    position: cursor.2,
                    cell,
                }
            } else {
                Action::SetCell {
                    section: cursor.0,
                    rung: cursor.1,
                    position: cursor.2,
                    cell,
                }
            });
        }
    }
    let _ = first;
    if undo {
        app.plc.undo();
    }
    if redo {
        app.plc.redo();
    }
    if right {
        cursor.2 = (cursor.2 + 1).min(POSITIONS);
    }
    if left {
        cursor.2 = cursor.2.saturating_sub(1);
    }
    if down {
        if cursor.1 + 1 < rows[cursor.0] {
            cursor.1 += 1;
        } else if cursor.0 + 1 < rows.len() {
            cursor.0 += 1;
            cursor.1 = 0;
        }
    }
    if up {
        if cursor.1 > 0 {
            cursor.1 -= 1;
        } else if cursor.0 > 0 {
            cursor.0 -= 1;
            cursor.1 = rows[cursor.0].saturating_sub(1);
        }
    }
    app.plc.cell_cursor = Some(cursor);
    if copy {
        // A mark in the same section copies the rectangle between it and the
        // cursor, one row per rung; without one, just the cell under the cursor.
        let sections_view = &sections;
        let block = match app.plc.cell_mark {
            Some((mark_section, mark_rung, mark_position)) if mark_section == cursor.0 => {
                let rung_range = if mark_rung <= cursor.1 {
                    mark_rung..=cursor.1
                } else {
                    cursor.1..=mark_rung
                };
                let (low, high) = if mark_position <= cursor.2 {
                    (mark_position, cursor.2)
                } else {
                    (cursor.2, mark_position)
                };
                let rows = rung_range
                    .filter_map(|rung_index| {
                        sections_view
                            .get(mark_section)
                            .and_then(|section| section.rungs.get(rung_index))
                    })
                    .map(|rung| {
                        (low..=high)
                            .map(|position| rung.cells.get(position).cloned().unwrap_or(Cell::Wire))
                            .collect::<Vec<Cell>>()
                    })
                    .collect::<Vec<_>>();
                rows
            }
            _ => Vec::new(),
        };
        if block.is_empty() {
            app.plc.cell_clipboard = sections_view
                .get(cursor.0)
                .and_then(|section| section.rungs.get(cursor.1))
                .and_then(|rung| rung.cells.get(cursor.2))
                .cloned();
            app.plc.cell_block.clear();
        } else {
            app.plc.cell_block = block;
            app.plc.cell_clipboard = None;
        }
    }
    if paste {
        if !app.plc.cell_block.is_empty() {
            for (row_offset, row) in app.plc.cell_block.clone().into_iter().enumerate() {
                let rung = cursor.1 + row_offset;
                for (column_offset, cell) in row.into_iter().enumerate() {
                    let position = cursor.2 + column_offset;
                    if position < POSITIONS {
                        actions.push(Action::SetCell {
                            section: cursor.0,
                            rung,
                            position,
                            cell,
                        });
                    }
                }
            }
        } else if let (Some(cell), true) = (app.plc.cell_clipboard.clone(), cursor.2 < POSITIONS) {
            actions.push(Action::SetCell {
                section: cursor.0,
                rung: cursor.1,
                position: cursor.2,
                cell,
            });
        }
    }
    if xref {
        app.plc.xref = sections
            .get(cursor.0)
            .and_then(|section| section.rungs.get(cursor.1))
            .and_then(|rung| rung.cells.get(cursor.2))
            .and_then(|cell| cell.operand())
            .map(|operand| operand.to_string());
    }
    if watch {
        if let Some(operand) = sections
            .get(cursor.0)
            .and_then(|section| section.rungs.get(cursor.1))
            .and_then(|rung| rung.cells.get(cursor.2))
            .and_then(|cell| cell.operand())
        {
            actions.push(Action::ToggleWatch(operand.to_string()));
        }
    }
    if enter {
        // Enter opens what is already in the cell: a box asks for its instance
        // and preset, a compare asks for its condition and operands, and an
        // empty or plain cell asks which element to put there.
        let existing = sections
            .get(cursor.0)
            .and_then(|section| section.rungs.get(cursor.1))
            .and_then(|rung| rung.cells.get(cursor.2))
            .cloned();
        match enter_target(cursor, existing.as_ref()) {
            EnterTarget::Block(cell) => app.plc.block_edit = Some(cell),
            EnterTarget::Compare(edit) => app.plc.compare_edit = Some(edit),
            EnterTarget::Compute(edit) => app.plc.compute_edit = Some(edit),
            EnterTarget::Operand((section, rung, position, cell)) => {
                let name = cell.operand().unwrap_or("").to_string();
                app.plc.operand_cursor = operand_position(&app.plc, &name);
                app.plc.operand_filter.clear();
                app.plc.operand_pick = Some((section, rung, position, cell));
            }
            EnterTarget::Coil((section, rung, _)) => {
                let var = sections
                    .get(section)
                    .and_then(|section| section.rungs.get(rung))
                    .map(|rung| rung.coil.var.clone())
                    .unwrap_or_default();
                app.plc.operand_cursor = coil_position(&app.plc, &var);
                app.plc.operand_filter.clear();
                app.plc.coil_pick = Some((section, rung));
            }
            EnterTarget::Picker => {
                app.plc.picker = Some(cursor);
                app.plc.picker_cursor = 0;
            }
        }
    }
    if delete && cursor.2 < POSITIONS {
        // ZX closes the row up to the left; a wire in place would leave a gap.
        actions.push(Action::DeleteCell {
            section: cursor.0,
            rung: cursor.1,
            position: cursor.2,
        });
    }
}

/// The element picker and, after it, the operand list.

/// Draws every window that sits on top of the grid and returns true when one of
/// them held the keyboard this frame, so the grid's own keys stand down.
pub fn pickers(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) -> bool {
    // A window that was up when the frame began owns the keys even if it closes
    // on this frame's Enter: otherwise that Enter reached the grid and opened a
    // picker on the cell the window had just written.
    let owned_at_entry = modal_open(plc);
    // Escape closes any picker here, before the editor's own Escape, so leaving
    // a list never also leaves the editor.
    if modal_keys(ctx).escape {
        clear_pickers(plc);
        return owned_at_entry;
    }
    // One picker owns a frame's keys: the element list is drawn before the
    // operand list, and when its letter chose an item the same keystroke must
    // not also land in the operand filter.
    let element = element_picker(plc, ctx, actions);
    if element.handled {
        return true;
    }
    let operand_drawn = operand_picker(plc, ctx, actions);
    let coil_drawn = coil_picker(plc, ctx, actions);
    // A state flag without a window this frame is a stuck picker: nothing can
    // draw it and nothing can close it with the keyboard, so drop it.
    if picker_is_stale(plc.picker.is_some(), element.drawn) {
        plc.picker = None;
    }
    if picker_is_stale(plc.operand_pick.is_some(), operand_drawn) {
        plc.operand_pick = None;
        plc.operand_filter.clear();
    }
    if picker_is_stale(plc.coil_pick.is_some(), coil_drawn) {
        plc.coil_pick = None;
        plc.operand_filter.clear();
    };
    compare_editor(plc, ctx, actions);
    compute_editor(plc, ctx, actions);
    block_editor(plc, ctx, actions);
    xref_window(plc, ctx);
    owned_at_entry || modal_open(plc)
}

/// True when any window of this module is up. Read after the draw, so a window
/// that closed on this frame's Enter does not hand the same Enter to the grid.
fn modal_open(plc: &PlcUi) -> bool {
    plc.picker.is_some()
        || plc.operand_pick.is_some()
        || plc.coil_pick.is_some()
        || plc.compare_edit.is_some()
        || plc.compute_edit.is_some()
        || plc.block_edit.is_some()
}

/// Timer and counter boxes: pick the instance from the ones the block type has,
/// type the preset, and the window says what a preset may look like. The panel
/// keyboard has no Tab, so nothing here needs it: the arrows walk the instance
/// list, typed characters go into the preset, Enter applies, Esc leaves.
fn block_editor(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let Some((section, rung, position)) = plc.block_edit else {
        plc.block_fields = None;
        return;
    };
    let cell = plc
        .current_pou_sections()
        .get(section)
        .and_then(|section| section.rungs.get(rung))
        .and_then(|rung| rung.cells.get(position))
        .cloned();
    let Some(Cell::Block {
        block,
        instance,
        preset,
    }) = cell
    else {
        plc.block_edit = None;
        plc.block_fields = None;
        return;
    };
    // The buffer follows the box, never the other way round: a frame that
    // re-synced every draw would fight the operator's typing.
    if plc.block_fields != Some((section, rung, position)) {
        plc.block_fields = Some((section, rung, position));
        plc.block_preset_text = preset.clone();
        plc.block_preset_error = None;
        plc.block_instance_cursor = instance_number(&block, &instance).unwrap_or(0);
        plc.block_focus_preset = false;
    }
    let used = used_instances(plc, &block, (section, rung, position));
    let counters = block.starts_with("CT");
    let mut close = false;
    let mut apply = false;
    egui::Window::new(box_title(&block))
        // Above the workbench overlay (Foreground), which is itself above the page.
        .order(egui::Order::Tooltip)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_min_width(340.0);
            ui.label(
                RichText::new("INSTANCE")
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            // One row per instance, in the tool table's dressing: the band marks
            // the choice, the right-hand word says whether something else
            // already holds it.
            for number in 0..4 {
                let name = format!("{block}{number}");
                let row_selected = plc.block_instance_cursor == number;
                let taken = used.contains(&name);
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), theme::NAV_ROW_HEIGHT_SECONDARY),
                    egui::Sense::click(),
                );
                if response.clicked() {
                    plc.block_instance_cursor = number;
                    plc.block_focus_preset = false;
                }
                let painter = ui.painter();
                if row_selected {
                    painter.rect_filled(rect, egui::CornerRadius::ZERO, theme::PANEL_RAISED);
                    // The accent bar says where the keys are; the plain band keeps
                    // saying which instance is chosen while the preset is focused.
                    if !plc.block_focus_preset {
                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                rect.min,
                                egui::vec2(theme::SELECTION_BAR, rect.height()),
                            ),
                            egui::CornerRadius::ZERO,
                            theme::ACCENT,
                        );
                    }
                } else if number % 2 == 1 {
                    painter.rect_filled(rect, egui::CornerRadius::ZERO, theme::SURFACE);
                }
                painter.text(
                    egui::pos2(rect.left() + theme::SELECTION_BAR + 6.0, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    &name,
                    egui::FontId::monospace(theme::font_size(10.0)),
                    if taken { theme::C3 } else { theme::FG },
                );
                painter.text(
                    egui::pos2(rect.right() - 6.0, rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    if taken { "IN USE" } else { "FREE" },
                    egui::FontId::monospace(theme::font_size(9.0)),
                    if taken { theme::C3 } else { theme::MUTED },
                );
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new(if counters {
                    "PRESET (PV)"
                } else {
                    "PRESET (PT)"
                })
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
            );
            // The typed value is the window's own buffer: no text widget holds
            // the keyboard, so the arrows never have to be given back to the
            // instance list first.
            let (rect, response) = ui.allocate_exact_size(
                egui::vec2(ui.available_width(), theme::MIN_TARGET),
                egui::Sense::click(),
            );
            if response.clicked() {
                plc.block_focus_preset = true;
            }
            let focused = plc.block_focus_preset;
            let painter = ui.painter();
            painter.rect_filled(rect, egui::CornerRadius::same(3), theme::SURFACE);
            painter.rect_stroke(
                rect,
                egui::CornerRadius::same(3),
                egui::Stroke::new(
                    1.0,
                    if focused {
                        theme::ACCENT
                    } else {
                        theme::BORDER
                    },
                ),
                egui::StrokeKind::Inside,
            );
            painter.text(
                egui::pos2(rect.left() + 8.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                if focused {
                    format!("{}▌", plc.block_preset_text)
                } else {
                    plc.block_preset_text.clone()
                },
                egui::FontId::monospace(theme::font_size(11.0)),
                theme::FG,
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new(if counters {
                    "a whole number, e.g. 25"
                } else {
                    "T#<n>ms, T#<n>s or T#<n>m — e.g. T#250ms, T#2s, T#1m"
                })
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
            );
            if let Some(error) = &plc.block_preset_error {
                ui.label(
                    RichText::new(error)
                        .size(theme::font_size(9.0))
                        .color(theme::ERROR),
                );
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new("↑ ↓ MOVE BETWEEN INSTANCES AND THE PRESET")
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            ui.label(
                RichText::new("ENTER APPLY · ESC CANCEL")
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
    let keys = modal_keys(ctx);
    if keys.up {
        // Past the first row the focus leaves the list for the preset, and from
        // there any arrow gives the list back at the row it left.
        if plc.block_focus_preset {
            plc.block_focus_preset = false;
        } else if plc.block_instance_cursor == 0 {
            plc.block_focus_preset = true;
        } else {
            plc.block_instance_cursor -= 1;
        }
    }
    if keys.down {
        if plc.block_focus_preset {
            plc.block_focus_preset = false;
        } else if plc.block_instance_cursor + 1 >= 4 {
            plc.block_focus_preset = true;
        } else {
            plc.block_instance_cursor += 1;
        }
    }
    // The preset edits only while it holds the keys; on the list Backspace is
    // not a delete key for anything.
    if keys.backspace && plc.block_focus_preset {
        plc.block_preset_text.pop();
    }
    for character in keys.typed {
        // A preset is digits, `#`, letters for the unit and a dot; anything else
        // is refused here so the buffer cannot fill with keys that never parse.
        if character.is_ascii_alphanumeric() || matches!(character, '#' | '.' | ':') {
            plc.block_preset_text.push(character);
        }
    }
    if keys.enter {
        apply = true;
        // Enter belongs to this window: leaving it in the frame let the grid
        // behind reopen the box in the same keystroke, so Apply looked like
        // Cancel followed by a reopen.
        ctx.input_mut(|input| {
            input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
        });
    }
    if keys.escape {
        close = true;
    }
    // The grid reads these too; a window that holds the keyboard takes them with
    // it so nothing lands on the ladder behind.
    ctx.input_mut(|input| {
        for key in [
            egui::Key::ArrowUp,
            egui::Key::ArrowDown,
            egui::Key::Backspace,
        ] {
            input.consume_key(egui::Modifiers::NONE, key);
        }
    });
    if apply {
        let instance = format!("{block}{}", plc.block_instance_cursor);
        let preset = plc.block_preset_text.trim().to_string();
        match preset_is_valid(counters, &preset) {
            Ok(()) => {
                actions.push(Action::SetBlockField {
                    section,
                    rung,
                    position,
                    instance: (instance != instance_at(plc, section, rung, position))
                        .then_some(instance),
                    preset: (!preset.is_empty()
                        && preset != preset_at(plc, section, rung, position))
                    .then_some(preset),
                });
                close = true;
            }
            // The window stays and says why: closing on a bad preset would write
            // nothing and look like it worked.
            Err(error) => plc.block_preset_error = Some(error),
        }
    }
    if close {
        plc.block_edit = None;
        plc.block_fields = None;
    }
}

/// What the box does, in the words the standard uses, with its code so the
/// title still matches the label on the ladder. "TON BOX" says nothing about
/// whether the timer delays the on or the off edge.
fn box_title(block: &str) -> String {
    match block {
        "TON" => "ON-DELAY TIMER (TON)".to_string(),
        "TOF" => "OFF-DELAY TIMER (TOF)".to_string(),
        "TP" => "PULSE TIMER (TP)".to_string(),
        "CTU" => "UP COUNTER (CTU)".to_string(),
        "CTD" => "DOWN COUNTER (CTD)".to_string(),
        other => format!("{other} BOX"),
    }
}

/// The instance index in `TON2`, when the name has the block's own prefix.
fn instance_number(block: &str, instance: &str) -> Option<usize> {
    instance
        .trim()
        .strip_prefix(block)
        .and_then(|digits| digits.parse::<usize>().ok())
        .filter(|number| *number < 4)
}

/// Instances of this block type that another box in the project already holds.
fn used_instances(plc: &PlcUi, block: &str, editing: (usize, usize, usize)) -> Vec<String> {
    let mut used = Vec::new();
    for (pou_index, pou) in plc.project.all_pous().into_iter().enumerate() {
        // Only the POU on screen can be the one being edited; its index is the
        // project's own ordering, which is what `editing` indexes into.
        if pou_index != plc.pou {
            for section in &pou.sections {
                for rung in &section.rungs {
                    for cell in &rung.cells {
                        if let Cell::Block {
                            block: kind,
                            instance,
                            ..
                        } = cell
                        {
                            if kind == block && !used.contains(instance) {
                                used.push(instance.clone());
                            }
                        }
                    }
                }
            }
            continue;
        }
        for (section_index, section) in pou.sections.iter().enumerate() {
            for (rung_index, rung) in section.rungs.iter().enumerate() {
                for (position, cell) in rung.cells.iter().enumerate() {
                    if (section_index, rung_index, position) == editing {
                        continue;
                    }
                    if let Cell::Block {
                        block: kind,
                        instance,
                        ..
                    } = cell
                    {
                        if kind == block && !used.contains(instance) {
                            used.push(instance.clone());
                        }
                    }
                }
            }
        }
    }
    used
}

/// What a preset may hold. The rules are the ones the window states, so a value
/// the operator can type and the dialog accepts are the same set.
fn preset_is_valid(counters: bool, preset: &str) -> Result<(), String> {
    if preset.is_empty() {
        return Err("a preset is required, e.g. T#1s".to_string());
    }
    if counters {
        return preset
            .parse::<i32>()
            .map(|value| {
                let _ = value;
            })
            .map_err(|_| format!("{preset} is not a whole number, e.g. 25"));
    }
    let rest = preset
        .strip_prefix("T#")
        .ok_or_else(|| format!("{preset} must start with T#, e.g. T#250ms"))?;
    let number_len = rest
        .find(|character: char| !character.is_ascii_digit() && character != '.')
        .unwrap_or(rest.len());
    let (number, unit) = rest.split_at(number_len);
    if number.is_empty() || number.parse::<f32>().is_err() {
        return Err(format!("{preset} has no number, e.g. T#250ms"));
    }
    match unit {
        "ms" | "s" | "m" => Ok(()),
        "" => Err(format!("{preset} needs a unit: ms, s or m")),
        other => Err(format!("{other} is not a unit: use ms, s or m")),
    }
}

/// The box's current instance, read back from the project.
fn instance_at(plc: &PlcUi, section: usize, rung: usize, position: usize) -> String {
    match plc
        .current_pou_sections()
        .get(section)
        .and_then(|section| section.rungs.get(rung))
        .and_then(|rung| rung.cells.get(position))
        .cloned()
    {
        Some(Cell::Block { instance, .. }) => instance,
        _ => String::new(),
    }
}

/// The box's current preset, read back from the project.
fn preset_at(plc: &PlcUi, section: usize, rung: usize, position: usize) -> String {
    match plc
        .current_pou_sections()
        .get(section)
        .and_then(|section| section.rungs.get(rung))
        .and_then(|rung| rung.cells.get(position))
        .cloned()
    {
        Some(Cell::Block { preset, .. }) => preset,
        _ => String::new(),
    }
}

/// Cross references of the operand under the cursor: every cell and coil that
/// names it, across every POU.
fn xref_window(plc: &mut PlcUi, ctx: &egui::Context) {
    let Some(name) = plc.xref.clone() else {
        return;
    };
    let mut close = false;
    egui::Window::new(format!("CROSS REFERENCES  {name}"))
        // Above the workbench overlay (Foreground), which is itself above the page.
        .order(egui::Order::Tooltip)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_min_width(380.0);
            let mut hits = 0usize;
            for pou in plc.project.all_pous() {
                for section in &pou.sections {
                    for (rung_index, rung) in section.rungs.iter().enumerate() {
                        for (position, cell) in rung.cells.iter().enumerate() {
                            if cell
                                .operand()
                                .map(|operand| operand.eq_ignore_ascii_case(&name))
                                .unwrap_or(false)
                            {
                                hits += 1;
                                ui.label(format!(
                                    "{}  {}  rung {}  position {}",
                                    pou.name,
                                    section.name,
                                    rung_index + 1,
                                    position
                                ));
                            }
                        }
                        if rung.coil.var.eq_ignore_ascii_case(&name) {
                            hits += 1;
                            ui.label(format!(
                                "{}  {}  rung {}  coil",
                                pou.name,
                                section.name,
                                rung_index + 1
                            ));
                        }
                    }
                }
            }
            if hits == 0 {
                ui.label("no references");
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new("ENTER OR ESC CLOSES")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            if theme::action_button(ui, "Close", true).clicked()
                || modal_keys(ctx).enter
                || modal_keys(ctx).escape
            {
                close = true;
            }
        });
    if close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        // The window took this Enter or Escape; leaving it in the frame let the
        // ladder act on the same key and open the element list behind the
        // closed cross references.
        ctx.input_mut(|input| {
            input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
            input.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
        });
        plc.xref = None;
    }
}

/// What a modal picker does with the keys pressed this frame.
///
/// The pickers read events themselves rather than a text field: a field needs a
/// frame to take focus, and a letter typed inside that frame was lost, which is
/// how a typed `estop` once became the list's first entry. These keys work on
/// the frame they arrive.
#[derive(Default)]
struct ModalKeys {
    typed: Vec<char>,
    backspace: bool,
    enter: bool,
    escape: bool,
    up: bool,
    down: bool,
}

/// True when `key` is pressed this frame. Read-only so the caller can decide.
fn ui_key(ctx: &egui::Context, key: egui::Key) -> bool {
    ctx.input(|input| {
        input.events.iter().any(|event| {
            matches!(event, egui::Event::Key { key: pressed, pressed: true, .. } if *pressed == key)
        })
    })
}

fn modal_keys(ctx: &egui::Context) -> ModalKeys {
    let mut keys = ModalKeys::default();
    for event in ctx.input(|input| input.events.clone()) {
        match event {
            egui::Event::Text(text) => {
                keys.typed
                    .extend(text.chars().filter(|c| c.is_ascii_graphic() || *c == ' '));
            }
            egui::Event::Key {
                key, pressed: true, ..
            } => match key {
                egui::Key::Backspace => keys.backspace = true,
                egui::Key::Enter => keys.enter = true,
                egui::Key::Escape => keys.escape = true,
                egui::Key::ArrowUp => keys.up = true,
                egui::Key::ArrowDown => keys.down = true,
                _ => {}
            },
            _ => {}
        }
    }
    keys
}

/// The element a letter stands for, as the ZX front panel labels it.
fn element_for_key(character: char) -> Option<&'static str> {
    Some(match character.to_ascii_uppercase() {
        'W' | '-' => "wire",
        'C' => "contact",
        'X' | '/' => "negated",
        'V' => "link",
        'T' => "TON",
        'F' => "TOF",
        'U' => "TP",
        'K' => "CTU",
        'D' => "CTD",
        'G' => "compare",
        'M' => "compute",
        _ => return None,
    })
}

/// A picker state whose window did not draw this frame: nothing can render it
/// and nothing can close it with the keyboard, so the page must drop it.
pub fn picker_is_stale(state: bool, drawn: bool) -> bool {
    state && !drawn
}

/// Drop every modal picker and the text they were collecting.
fn clear_pickers(plc: &mut PlcUi) {
    plc.picker = None;
    plc.operand_pick = None;
    plc.coil_pick = None;
    plc.operand_filter.clear();
    // The block window is a picker too. Leaving it out of the sweep kept its
    // state alive, so Escape skipped its draw for one frame and the window came
    // straight back on the next.
    plc.block_edit = None;
    plc.block_fields = None;
    plc.compare_edit = None;
    plc.compute_edit = None;
    plc.xref = None;
}

/// A picker row is taller than a rail row so the glyphs keep a clear band of
/// padding: the contact bars reach 8 px either side of the conductor, so the
/// 18 px glyph box would touch its neighbours at the rail's own height.
const PICKER_ROW_HEIGHT: f32 = theme::NAV_ROW_HEIGHT_SECONDARY + 8.0;

/// What one picker frame did: whether it took the keys, and whether it drew.
struct PickerFrame {
    handled: bool,
    drawn: bool,
}

fn element_picker(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) -> PickerFrame {
    let Some((section, rung, position)) = plc.picker else {
        return PickerFrame {
            handled: false,
            drawn: false,
        };
    };
    let first = plc
        .project
        .io
        .iter()
        .map(|point| point.name.clone())
        .chain(plc.project.vars.iter().map(|var| var.name.clone()))
        .next()
        .unwrap_or_default();
    let coil_position = position >= POSITIONS;
    let mut close = false;
    let mut next_operand = None;
    let keys = modal_keys(ctx);
    let mut key_cell: Option<Cell> = None;
    let mut key_coil: Option<CoilKind> = None;
    if let Some(character) = keys.typed.first().copied() {
        if let Some(element) = element_for_key(character) {
            match (coil_position, element) {
                (true, "contact") => key_coil = Some(CoilKind::Normal),
                (true, "negated") => key_coil = Some(CoilKind::Set),
                (true, _) => key_coil = Some(CoilKind::Normal),
                (false, "wire") => key_cell = Some(Cell::Wire),
                (false, "link") => key_cell = Some(Cell::Link),
                (false, "contact") => {
                    key_cell = Some(Cell::Contact {
                        var: first.clone(),
                        negated: false,
                    })
                }
                (false, "negated") => {
                    key_cell = Some(Cell::Contact {
                        var: first.clone(),
                        negated: true,
                    })
                }
                (false, "compare") => {
                    plc.compare_edit = Some(CompareEdit {
                        section,
                        rung,
                        position,
                        op: "GT".to_string(),
                        left: "MW0".to_string(),
                        right: "0".to_string(),
                        focus: 0,
                        error: None,
                    });
                    close = true;
                }
                (false, "compute") => {
                    plc.compute_edit = Some(ComputeEdit {
                        section,
                        rung,
                        position,
                        op: "ADD".to_string(),
                        target: "MW1".to_string(),
                        left: "MW0".to_string(),
                        right: "1".to_string(),
                        focus: 0,
                        error: None,
                    });
                    close = true;
                }
                (false, block) => {
                    let (name, preset) = if block.starts_with("CT") {
                        ("10", "10")
                    } else {
                        ("T#1s", "T#1s")
                    };
                    key_cell = Some(Cell::Block {
                        block: block.to_string(),
                        instance: format!("{block}0"),
                        preset: preset.to_string(),
                    });
                    let _ = name;
                }
            }
        }
    }
    if keys.escape {
        plc.picker = None;
        return PickerFrame {
            handled: true,
            drawn: true,
        };
    }
    // One table drives the drawing, the cursor and the chord keys, so the list,
    // the highlight and the action cannot disagree. A row reads like a
    // navigation row: ladder glyph on the left, chord on the right.
    #[derive(Clone)]
    enum Act {
        Cell(Cell),
        Coil(CoilKind),
        Compare,
        Compute,
    }
    struct Item {
        group: &'static str,
        symbol: CellSymbol,
        label: &'static str,
        chord: &'static str,
        act: Act,
    }
    let mut items: Vec<Item> = Vec::new();
    if coil_position {
        let var = plc
            .selection
            .as_ref()
            .and_then(|node| {
                plc.current_pou_sections()
                    .get(node.section)
                    .and_then(|section| section.rungs.get(node.rung))
                    .map(|rung| rung.coil.var.clone())
            })
            .filter(|var| !var.is_empty())
            .unwrap_or_else(|| first.clone());
        for (label, kind) in [
            ("Coil", CoilKind::Normal),
            ("Set coil", CoilKind::Set),
            ("Reset coil", CoilKind::Reset),
        ] {
            let chord = match kind {
                CoilKind::Normal => "C",
                CoilKind::Set => "S",
                CoilKind::Reset => "R",
                CoilKind::Negated => "N",
            };
            items.push(Item {
                group: "COIL",
                symbol: CellSymbol::Coil(kind),
                label,
                chord,
                act: Act::Coil(kind),
            });
        }
        let _ = var;
    } else {
        for (symbol, label, chord, cell) in [
            (CellSymbol::Wire, "Wire", "-", Cell::Wire),
            (
                CellSymbol::Contact,
                "Contact",
                "C",
                Cell::Contact {
                    var: first.clone(),
                    negated: false,
                },
            ),
            (
                CellSymbol::ContactNc,
                "Negated contact",
                "X",
                Cell::Contact {
                    var: first.clone(),
                    negated: true,
                },
            ),
            (CellSymbol::Link, "Link", "V", Cell::Link),
        ] {
            items.push(Item {
                group: "CONTACTS",
                symbol,
                label,
                chord,
                act: Act::Cell(cell),
            });
        }
        for (label, chord, block, instance, preset) in [
            ("TON", "T", "TON", "TON0", "T#1s"),
            ("TOF", "F", "TOF", "TOF0", "T#1s"),
            ("TP", "U", "TP", "TP0", "T#1s"),
            ("CTU", "K", "CTU", "CTU0", "10"),
            ("CTD", "D", "CTD", "CTD0", "10"),
        ] {
            items.push(Item {
                group: "BOXES",
                symbol: CellSymbol::Block,
                label,
                chord,
                act: Act::Cell(Cell::Block {
                    block: block.to_string(),
                    instance: instance.to_string(),
                    preset: preset.to_string(),
                }),
            });
        }
        items.push(Item {
            group: "EXPRESSIONS",
            symbol: CellSymbol::Compare,
            label: "Compare",
            chord: "G",
            act: Act::Compare,
        });
        items.push(Item {
            group: "EXPRESSIONS",
            symbol: CellSymbol::Compute,
            label: "Compute",
            chord: "M",
            act: Act::Compute,
        });
    }
    plc.picker_cursor = plc.picker_cursor.min(items.len().saturating_sub(1));
    if keys.up {
        plc.picker_cursor = plc.picker_cursor.saturating_sub(1);
    }
    if keys.down && plc.picker_cursor + 1 < items.len() {
        plc.picker_cursor += 1;
    }
    let mut chosen: Option<usize> = None;
    if keys.enter {
        chosen = Some(plc.picker_cursor);
    }
    let mut visible = true;
    let shown = egui::Window::new("ELEMENT")
        .open(&mut visible)
        // Above the workbench overlay (Foreground), which is itself above the page.
        .order(egui::Order::Tooltip)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_min_width(240.0);
            let mut last_group = "";
            for (index, item) in items.iter().enumerate() {
                if item.group != last_group {
                    if !last_group.is_empty() {
                        ui.add_space(RAIL_GROUP_GAP);
                    }
                    last_group = item.group;
                    let (rect, _) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width(), theme::NAV_ROW_HEIGHT_SECONDARY),
                        egui::Sense::hover(),
                    );
                    ui.painter().text(
                        egui::pos2(rect.left() + 6.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        item.group,
                        FontId::proportional(theme::font_size(8.0)),
                        theme::C3,
                    );
                }
                let selected = index == plc.picker_cursor;
                let (rect, response) = ui.allocate_exact_size(
                    egui::vec2(ui.available_width(), PICKER_ROW_HEIGHT),
                    egui::Sense::click(),
                );
                if selected {
                    ui.painter().rect_filled(rect, 2.0, theme::PANEL_RAISED);
                } else if response.hovered() {
                    ui.painter().rect_filled(rect, 2.0, theme::SURFACE);
                }
                // The glyph says what the element looks like on the ladder; the
                // label and chord say what it reads and what types it.
                // The ladder painters place the conductor at `CONDUCTOR_Y`
                // below the rect they are given, so the rect is pushed up until
                // that conductor lands on this row's centre; a clip box keeps the
                // long leads out of the neighbouring rows and the label column.
                let centre = egui::pos2(rect.left() + 21.0, rect.center().y);
                let symbol_rect = egui::Rect::from_min_size(
                    egui::pos2(centre.x - 15.0, centre.y - CONDUCTOR_Y),
                    egui::vec2(30.0, CONDUCTOR_Y + 6.0),
                );
                let clip = egui::Rect::from_center_size(centre, egui::vec2(34.0, 18.0));
                let painter = ui.painter().with_clip_rect(clip);
                draw_cell_symbol(
                    &painter,
                    symbol_rect,
                    item.symbol,
                    if selected { theme::FG } else { theme::MUTED },
                );
                ui.painter().text(
                    egui::pos2(rect.left() + 44.0, rect.center().y),
                    Align2::LEFT_CENTER,
                    item.label,
                    FontId::proportional(theme::font_size(9.5)),
                    if selected { theme::FG } else { theme::MUTED },
                );
                ui.painter().text(
                    egui::pos2(rect.right() - 6.0, rect.center().y),
                    Align2::RIGHT_CENTER,
                    item.chord,
                    FontId::monospace(theme::font_size(8.5)),
                    theme::ACCENT,
                );
                if response.clicked() {
                    chosen = Some(index);
                }
            }
        });
    if let Some(index) = chosen {
        match items[index].act.clone() {
            Act::Cell(cell @ Cell::Contact { .. }) => {
                next_operand = Some((section, rung, position, cell));
            }
            Act::Cell(cell) => actions.push(Action::SetCell {
                section,
                rung,
                position,
                cell,
            }),
            Act::Coil(kind) => {
                let var = plc
                    .current_pou_sections()
                    .get(section)
                    .and_then(|section| section.rungs.get(rung))
                    .map(|rung| rung.coil.var.clone())
                    .filter(|var| !var.is_empty())
                    .unwrap_or_else(|| first.clone());
                actions.push(Action::SetCoil {
                    section,
                    rung,
                    coil: Coil::new(var, kind),
                });
            }
            Act::Compare => {
                let (op, left, right) = match plc
                    .current_pou_sections()
                    .get(section)
                    .and_then(|section| section.rungs.get(rung))
                    .and_then(|rung| rung.cells.get(position))
                {
                    Some(Cell::Compare { op, left, right }) => {
                        (op.clone(), left.clone(), right.clone())
                    }
                    _ => ("GT".to_string(), "MW0".to_string(), "0".to_string()),
                };
                plc.compare_edit = Some(CompareEdit {
                    section,
                    rung,
                    position,
                    op,
                    left,
                    right,
                    focus: 0,
                    error: None,
                });
            }
            Act::Compute => {
                let (op, target, left, right) = match plc
                    .current_pou_sections()
                    .get(section)
                    .and_then(|section| section.rungs.get(rung))
                    .and_then(|rung| rung.cells.get(position))
                {
                    Some(Cell::Compute {
                        op,
                        target,
                        left,
                        right,
                    }) => (op.clone(), target.clone(), left.clone(), right.clone()),
                    _ => (
                        "ADD".to_string(),
                        "MW1".to_string(),
                        "MW0".to_string(),
                        "1".to_string(),
                    ),
                };
                plc.compute_edit = Some(ComputeEdit {
                    section,
                    rung,
                    position,
                    op,
                    target,
                    left,
                    right,
                    focus: 0,
                    error: None,
                });
            }
        }
        close = true;
    }
    if let Some(cell) = key_cell {
        match cell {
            Cell::Contact { .. } => {
                plc.operand_pick = Some((section, rung, position, cell));
            }
            other => actions.push(Action::SetCell {
                section,
                rung,
                position,
                cell: other,
            }),
        }
        plc.picker = None;
        return PickerFrame {
            handled: true,
            drawn: true,
        };
    }
    if let Some(kind) = key_coil {
        let var = plc
            .current_pou_sections()
            .get(section)
            .and_then(|section| section.rungs.get(rung))
            .map(|rung| rung.coil.var.clone())
            .filter(|var| !var.is_empty())
            .unwrap_or_else(|| first.clone());
        actions.push(Action::SetCoil {
            section,
            rung,
            coil: Coil::new(var, kind),
        });
        plc.picker = None;
        return PickerFrame {
            handled: true,
            drawn: true,
        };
    }
    if let Some(pick) = next_operand {
        plc.operand_pick = Some(pick);
    }
    let drawn = shown.is_some() && visible;
    if close || !visible {
        plc.picker = None;
    }
    PickerFrame {
        handled: close || !visible,
        drawn,
    }
}

fn operand_picker(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) -> bool {
    let Some((section, rung, position, cell)) = plc.operand_pick.clone() else {
        return false;
    };
    let names = plc
        .project
        .io
        .iter()
        .map(|point| (point.name.clone(), format!("{:?}", point.direction)))
        .chain(
            plc.project
                .vars
                .iter()
                .map(|var| (var.name.clone(), "VAR".to_string())),
        )
        .collect::<Vec<_>>();
    let mut close = false;
    let keys = modal_keys(ctx);
    // The filter takes this frame's keys before the rows are worked out, so a
    // name typed and confirmed in one batch is in the list the Enter reads.
    let typed = !keys.typed.is_empty();
    for character in &keys.typed {
        plc.operand_filter.push(*character);
    }
    if keys.backspace {
        plc.operand_filter.pop();
    }
    if typed || keys.backspace {
        plc.operand_cursor = 0;
    }
    let needle = plc.operand_filter.trim().to_ascii_lowercase();
    let matches = names
        .iter()
        .filter(|(name, _)| needle.is_empty() || name.to_ascii_lowercase().contains(&needle))
        .collect::<Vec<_>>();
    if plc.operand_cursor >= matches.len() {
        plc.operand_cursor = 0;
    }
    if keys.up {
        plc.operand_cursor = plc.operand_cursor.saturating_sub(1);
    }
    if keys.down {
        plc.operand_cursor = (plc.operand_cursor + 1).min(matches.len().saturating_sub(1));
    }
    if keys.escape {
        plc.operand_pick = None;
        plc.operand_filter.clear();
        return false;
    }
    let mut chosen: Option<String> = None;
    let mut visible = true;
    let shown = egui::Window::new("OPERAND")
        .open(&mut visible)
        // Above the workbench overlay (Foreground), which is itself above the page.
        .order(egui::Order::Tooltip)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_min_width(320.0);
            ui.set_max_width(320.0);
            if names.is_empty() {
                ui.label(
                    RichText::new("no symbols yet: add an input, output or variable")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                return;
            }
            // The filter is the picker's own buffer, so it shows the keys that
            // actually arrived rather than the frame's later state.
            ui.label(
                RichText::new(if plc.operand_filter.is_empty() {
                    "type to narrow".to_string()
                } else {
                    format!("{}▌", plc.operand_filter)
                })
                .monospace()
                .size(theme::font_size(10.0))
                .color(if plc.operand_filter.is_empty() {
                    theme::MUTED
                } else {
                    theme::FG
                }),
            );
            ui.add_space(4.0);
            // Every symbol is a row of the same table as the tool list: the band
            // marks the cursor and the right column says what the symbol is.
            let first = plc
                .operand_cursor
                .saturating_sub(2)
                .min(matches.len().saturating_sub(OPERAND_ROWS));
            for (offset, (name, kind)) in matches.iter().enumerate().skip(first).take(OPERAND_ROWS)
            {
                let selected = offset == plc.operand_cursor;
                let response = window_row(ui, name, kind, selected, true, offset % 2 == 1);
                let comment = plc.symbol_comment(name).unwrap_or("");
                if !comment.is_empty() {
                    ui.label(
                        RichText::new(comment)
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                }
                if response.clicked() {
                    chosen = Some((*name).clone());
                }
            }
            if keys.enter {
                if let Some((name, _)) = matches.get(plc.operand_cursor) {
                    chosen = Some((*name).clone());
                }
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new("↑ ↓ SELECT · ENTER PICKS · ESC CANCELS")
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
    if let Some(name) = chosen {
        actions.push(Action::SetCell {
            section,
            rung,
            position,
            cell: with_operand(&cell, &name),
        });
        close = true;
    }
    if close || !visible {
        plc.operand_pick = None;
        plc.operand_filter.clear();
    }
    shown.is_some()
}

/// Rows the operand list shows before it pages.
const OPERAND_ROWS: usize = 8;

/// The cell with its operand replaced, keeping its kind and negation.
pub fn with_operand(cell: &Cell, name: &str) -> Cell {
    match cell {
        Cell::Contact { negated, .. } => Cell::Contact {
            var: name.to_string(),
            negated: *negated,
        },
        Cell::Edge { rising, .. } => Cell::Edge {
            var: name.to_string(),
            rising: *rising,
        },
        other => other.clone(),
    }
}

/// The coil's operand: same filtered list as a contact, different target.
/// The symbols a coil can write: every output and every variable, never an
/// input. The validator rejects an input as a coil target, so the list must not
/// offer one.
fn coil_position(plc: &PlcUi, name: &str) -> usize {
    coil_choices(plc)
        .iter()
        .position(|(candidate, _)| candidate == name)
        .unwrap_or(0)
}

fn coil_choices(plc: &PlcUi) -> Vec<(String, String)> {
    plc.project
        .io
        .iter()
        .filter(|point| point.direction == linc_plc_core::model::IoDirection::Out)
        .map(|point| (point.name.clone(), "OUT".to_string()))
        .chain(
            plc.project
                .vars
                .iter()
                .map(|var| (var.name.clone(), "VAR".to_string())),
        )
        .collect()
}

fn coil_picker(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) -> bool {
    let Some((section, rung)) = plc.coil_pick else {
        return false;
    };
    // A coil writes, so only writable symbols are offered: outputs and
    // variables. An input pin is driven by the machine, and the validator
    // refuses it as a coil target; listing it here only produced broken cells.
    // The current operand stays in the list even if it is invalid, so opening
    // the window shows what the ladder holds instead of silently moving it.
    let current = plc
        .current_pou_sections()
        .get(section)
        .and_then(|section| section.rungs.get(rung))
        .map(|rung| rung.coil.var.clone())
        .unwrap_or_default();
    let mut names = coil_choices(plc);
    if !current.is_empty() && !names.iter().any(|(name, _)| *name == current) {
        names.insert(0, (current.clone(), "IN".to_string()));
    }
    let mut close = false;
    let keys = modal_keys(ctx);
    // Filter first, then read the rows, so a name typed and confirmed in one
    // batch is the one Enter takes.
    let typed = !keys.typed.is_empty();
    for character in &keys.typed {
        plc.operand_filter.push(*character);
    }
    if keys.backspace {
        plc.operand_filter.pop();
    }
    if typed || keys.backspace {
        plc.operand_cursor = 0;
    }
    let needle = plc.operand_filter.trim().to_ascii_lowercase();
    let matches = names
        .iter()
        .filter(|(name, _)| needle.is_empty() || name.to_ascii_lowercase().contains(&needle))
        .collect::<Vec<_>>();
    plc.operand_cursor = plc.operand_cursor.min(matches.len().saturating_sub(1));
    if keys.up {
        plc.operand_cursor = plc.operand_cursor.saturating_sub(1);
    }
    if keys.down {
        plc.operand_cursor = (plc.operand_cursor + 1).min(matches.len().saturating_sub(1));
    }
    if keys.escape {
        plc.coil_pick = None;
        plc.operand_filter.clear();
        return false;
    }
    let mut chosen: Option<String> = None;
    let mut visible = true;
    let shown = egui::Window::new("COIL OPERAND")
        .open(&mut visible)
        // Above the workbench overlay (Foreground), which is itself above the page.
        .order(egui::Order::Tooltip)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_min_width(320.0);
            ui.set_max_width(320.0);
            ui.label(
                RichText::new(if plc.operand_filter.is_empty() {
                    "type to narrow".to_string()
                } else {
                    format!("{}▌", plc.operand_filter)
                })
                .monospace()
                .size(theme::font_size(10.0))
                .color(if plc.operand_filter.is_empty() {
                    theme::MUTED
                } else {
                    theme::FG
                }),
            );
            ui.add_space(4.0);
            let first = plc
                .operand_cursor
                .saturating_sub(2)
                .min(matches.len().saturating_sub(OPERAND_ROWS));
            for (offset, (name, kind)) in matches.iter().enumerate().skip(first).take(OPERAND_ROWS)
            {
                let selected = offset == plc.operand_cursor;
                if window_row(ui, name, kind, selected, true, offset % 2 == 1).clicked() {
                    chosen = Some((*name).clone());
                }
            }
            if keys.enter {
                if let Some((name, _)) = matches.get(plc.operand_cursor) {
                    chosen = Some((*name).clone());
                }
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new("↑ ↓ SELECT · ENTER PICKS · ESC CANCELS")
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
    if let Some(name) = chosen {
        actions.push(Action::SetCoilVar {
            section,
            rung,
            var: name,
        });
        close = true;
    }
    if close || !visible {
        plc.coil_pick = None;
        plc.operand_filter.clear();
    }
    shown.is_some()
}

/// The row a symbol holds in the picker list, so the picker opens on the value
/// the cell already has rather than at the top.
fn operand_position(plc: &PlcUi, name: &str) -> usize {
    plc.project
        .io
        .iter()
        .map(|point| point.name.as_str())
        .chain(plc.project.vars.iter().map(|var| var.name.as_str()))
        .position(|candidate| candidate == name)
        .unwrap_or(0)
}

/// What Enter on a ladder cell opens. The cell decides: a box and a compare have
/// their own windows and their own values, so Enter edits what is there instead
/// of offering to replace it.
#[derive(Debug)]
enum EnterTarget {
    Block((usize, usize, usize)),
    Compare(CompareEdit),
    Compute(ComputeEdit),
    /// A contact or edge: Enter edits its operand, not the element kind.
    Operand((usize, usize, usize, Cell)),
    /// The coil slot: Enter picks the coil's operand.
    Coil((usize, usize, usize)),
    Picker,
}

fn enter_target(cursor: (usize, usize, usize), cell: Option<&Cell>) -> EnterTarget {
    if cursor.2 >= POSITIONS {
        return EnterTarget::Coil(cursor);
    }
    match cell {
        Some(Cell::Block { .. }) => EnterTarget::Block(cursor),
        Some(Cell::Compare { op, left, right }) => EnterTarget::Compare(CompareEdit {
            section: cursor.0,
            rung: cursor.1,
            position: cursor.2,
            op: op.clone(),
            left: left.clone(),
            right: right.clone(),
            focus: 0,
            error: None,
        }),
        Some(Cell::Contact { .. }) | Some(Cell::Edge { .. }) => EnterTarget::Operand((
            cursor.0,
            cursor.1,
            cursor.2,
            cell.cloned().unwrap_or(Cell::Wire),
        )),
        Some(Cell::Compute {
            op,
            target,
            left,
            right,
        }) => EnterTarget::Compute(ComputeEdit {
            section: cursor.0,
            rung: cursor.1,
            position: cursor.2,
            op: op.clone(),
            target: target.clone(),
            left: left.clone(),
            right: right.clone(),
            focus: 0,
            error: None,
        }),
        _ => EnterTarget::Picker,
    }
}

/// The six comparison conditions, in the order the ZX operator list shows them,
/// with the formula the standard writes for each.
const COMPARE_OPS: [(&str, &str); 6] = [
    ("GT", "A > B"),
    ("GE", "A >= B"),
    ("EQ", "A = B"),
    ("NE", "A <> B"),
    ("LE", "A <= B"),
    ("LT", "A < B"),
];

/// One operand the compare window offers: the text that goes into the cell and
/// the word that says what kind of thing it is.
struct Choice {
    value: String,
    kind: &'static str,
}

impl Choice {
    fn new(value: impl Into<String>, kind: &'static str) -> Self {
        Self {
            value: value.into(),
            kind,
        }
    }
}

/// Every operand the compiler accepts in a compare cell: the marker words,
/// every timer's and counter's published value, and the literal shapes. Numbers
/// and times are open-ended, so the list carries the common forms and the field
/// takes any other.
fn compare_choices(plc: &PlcUi) -> Vec<Choice> {
    let mut choices: Vec<Choice> = (0..linc_plc_core::compile::WORD_COUNT)
        .map(|index| Choice::new(format!("MW{index}"), "WORD"))
        .collect();
    for (instance, kind) in used_block_instances(plc) {
        choices.push(Choice::new(format!("{instance}.{kind}"), "BLOCK"));
    }
    for value in ["0", "1", "5", "10", "25", "100", "1000"] {
        choices.push(Choice::new(value, "NUMBER"));
    }
    for value in ["T#100ms", "T#250ms", "T#1s", "T#10s", "T#1m"] {
        choices.push(Choice::new(value, "TIME"));
    }
    choices
}

/// The instances the project actually uses, each with the word that reads its
/// value: `.ET` for a timer, `.CV` for a counter.
fn used_block_instances(plc: &PlcUi) -> Vec<(String, &'static str)> {
    let mut used: Vec<(String, &'static str)> = Vec::new();
    for pou in plc.project.all_pous() {
        for section in &pou.sections {
            for rung in &section.rungs {
                for cell in &rung.cells {
                    if let Cell::Block {
                        block, instance, ..
                    } = cell
                    {
                        let member = if block.starts_with("CT") { "CV" } else { "ET" };
                        if !used.iter().any(|(name, _)| name == instance) {
                            used.push((instance.clone(), member));
                        }
                    }
                }
            }
        }
    }
    used
}

/// What an operand that is not in the list is: the same words the list uses, so
/// a value the project only reads still says what kind of thing it is.
fn operand_kind(text: &str) -> &'static str {
    let value = text.trim().to_ascii_uppercase();
    if value.starts_with("MW") || value.starts_with("%MW") {
        "WORD"
    } else if value.starts_with("T#") || value.starts_with("TIME#") {
        "TIME"
    } else if value.contains(".ET")
        || value.contains(".CV")
        || value.contains(".PT")
        || value.contains(".PV")
    {
        "BLOCK"
    } else {
        "NUMBER"
    }
}

/// Why this text cannot be a compare operand, in the words the window uses. The
/// compiler is the judge; this only turns its refusal into a sentence.
fn operand_error(text: &str) -> Option<String> {
    let value = text.trim();
    if value.is_empty() {
        return Some("needs a value".to_string());
    }
    if linc_plc_core::compile::word_operand_value(value).is_some() {
        return None;
    }
    let upper = value.to_ascii_uppercase();
    Some(if upper.starts_with("MW") || upper.starts_with("%MW") {
        format!("{value} is not a marker word: MW0 to MW15")
    } else if upper.starts_with("T#") {
        format!("{value} needs a unit: T#250ms, T#2s or T#1m")
    } else if value.starts_with('-') || value.starts_with(|c: char| c.is_ascii_digit()) {
        format!("{value} is not a whole number inside -32768 to 32767")
    } else {
        format!("{value} is not an operand: a word, a number, a time or a block value")
    })
}

/// One row of the window's tables: the value on the left, what it is on the
/// right, the tool table's band and stripes.
fn window_row(
    ui: &mut Ui,
    left: &str,
    right: &str,
    selected: bool,
    keys_here: bool,
    striped: bool,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), theme::NAV_ROW_HEIGHT_SECONDARY),
        Sense::click(),
    );
    let painter = ui.painter();
    if selected {
        painter.rect_filled(rect, egui::CornerRadius::ZERO, theme::PANEL_RAISED);
        if keys_here {
            painter.rect_filled(
                egui::Rect::from_min_size(
                    rect.min,
                    egui::vec2(theme::SELECTION_BAR, rect.height()),
                ),
                egui::CornerRadius::ZERO,
                theme::ACCENT,
            );
        }
    } else if striped {
        painter.rect_filled(rect, egui::CornerRadius::ZERO, theme::SURFACE);
    }
    painter.text(
        egui::pos2(rect.left() + theme::SELECTION_BAR + 6.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        left,
        egui::FontId::monospace(theme::font_size(10.0)),
        if selected { theme::FG } else { theme::FG },
    );
    painter.text(
        egui::pos2(rect.right() - 6.0, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        right,
        egui::FontId::monospace(theme::font_size(9.0)),
        theme::MUTED,
    );
    response
}

/// One operand column: the field it types into, and the list of choices with the
/// cursor visible. Returns whether the field was clicked and which choice was.
fn operand_column(
    ui: &mut Ui,
    title: &str,
    text: &mut String,
    keys_here: bool,
    choices: &[Choice],
) -> (bool, Option<usize>) {
    ui.label(
        RichText::new(title)
            .monospace()
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
    );
    let (rect, response) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), theme::MIN_TARGET),
        Sense::click(),
    );
    let painter = ui.painter();
    painter.rect_filled(rect, egui::CornerRadius::same(3), theme::SURFACE);
    painter.rect_stroke(
        rect,
        egui::CornerRadius::same(3),
        Stroke::new(
            1.0,
            if keys_here {
                theme::ACCENT
            } else {
                theme::BORDER
            },
        ),
        egui::StrokeKind::Inside,
    );
    painter.text(
        egui::pos2(rect.left() + 8.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        if keys_here {
            format!("{text}▌")
        } else {
            text.clone()
        },
        egui::FontId::monospace(theme::font_size(11.0)),
        theme::FG,
    );
    let cursor = choices
        .iter()
        .position(|choice| choice.value == text.trim());
    // A value that is not one of the listed words — a typed literal, or a block
    // the project only reads — still gets a row of its own, so the chosen
    // operand is visible and selected instead of missing from the list.
    let mut rows = COMPARE_ROWS;
    if cursor.is_none() && !text.trim().is_empty() {
        let _ = window_row(ui, text.trim(), operand_kind(text), true, keys_here, false);
        rows -= 1;
    }
    let first = cursor
        .unwrap_or(0)
        .saturating_sub(2)
        .min(choices.len().saturating_sub(rows));
    let mut clicked = None;
    for (index, choice) in choices.iter().enumerate().skip(first).take(rows) {
        if window_row(
            ui,
            &choice.value,
            choice.kind,
            cursor == Some(index),
            keys_here,
            index % 2 == 1,
        )
        .clicked()
        {
            clicked = Some(index);
        }
    }
    (response.clicked(), clicked)
}

const COMPARE_ROWS: usize = 8;

/// The compare window: the condition and both operands at once. The keys belong
/// to one of the three, the arrows walk it and cross to the next at its ends, and
/// Enter takes the highlighted choice or the typed value before moving on. The
/// panel keyboard has no Tab, so nothing here needs it.
fn compare_editor(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let Some(mut edit) = plc.compare_edit.clone() else {
        return;
    };
    let choices = compare_choices(plc);
    let keys = modal_keys(ctx);
    let to_a = ui_key(ctx, egui::Key::ArrowLeft);
    let to_b = ui_key(ctx, egui::Key::ArrowRight);
    if keys.escape {
        plc.compare_edit = None;
        return;
    }
    let op_index = COMPARE_OPS
        .iter()
        .position(|(op, _)| *op == edit.op)
        .unwrap_or(0);

    // Typing goes to the operand that holds the keys; the condition takes no
    // letters, so a stray key cannot redraw the rung while the window is up.
    let editing_operand = edit.focus == 1 || edit.focus == 2;
    let mut typed = false;
    for character in &keys.typed {
        if !editing_operand {
            continue;
        }
        if character.is_ascii_alphanumeric() || matches!(character, '#' | '.' | ':' | '-') {
            if edit.focus == 1 {
                edit.left.push(*character);
            } else {
                edit.right.push(*character);
            }
            typed = true;
        }
    }
    if keys.backspace && editing_operand {
        if edit.focus == 1 {
            edit.left.pop();
        } else {
            edit.right.pop();
        }
        typed = true;
    }
    if typed {
        edit.error = None;
    }

    let left_cursor = choices.iter().position(|choice| choice.value == edit.left);
    let right_cursor = choices.iter().position(|choice| choice.value == edit.right);
    if keys.up {
        match edit.focus {
            0 => {
                if op_index == 0 {
                    edit.focus = 2;
                } else {
                    edit.op = COMPARE_OPS[op_index - 1].0.to_string();
                }
            }
            1 => match left_cursor {
                Some(0) | None => edit.focus = 0,
                Some(index) => edit.left = choices[index - 1].value.clone(),
            },
            _ => match right_cursor {
                Some(0) | None => edit.focus = 1,
                Some(index) => edit.right = choices[index - 1].value.clone(),
            },
        }
    }
    if keys.down {
        match edit.focus {
            0 => {
                if op_index + 1 >= COMPARE_OPS.len() {
                    edit.focus = 1;
                } else {
                    edit.op = COMPARE_OPS[op_index + 1].0.to_string();
                }
            }
            1 => match left_cursor {
                Some(index) if index + 1 < choices.len() => {
                    edit.left = choices[index + 1].value.clone()
                }
                _ => edit.focus = 2,
            },
            _ => match right_cursor {
                Some(index) if index + 1 < choices.len() => {
                    edit.right = choices[index + 1].value.clone()
                }
                _ => edit.focus = 0,
            },
        }
    }
    if to_a {
        edit.focus = 1;
    }
    if to_b {
        edit.focus = 2;
    }

    // Enter accepts what the field shows and moves on: condition, A, B, apply.
    let mut apply = false;
    if keys.enter {
        match edit.focus {
            0 => edit.focus = 1,
            1 => match operand_error(&edit.left) {
                Some(error) => edit.error = Some(format!("A {error}")),
                None => {
                    edit.left = edit.left.trim().to_string();
                    edit.focus = 2;
                    edit.error = None;
                }
            },
            _ => match operand_error(&edit.right) {
                Some(error) => edit.error = Some(format!("B {error}")),
                None => {
                    edit.right = edit.right.trim().to_string();
                    apply = true;
                }
            },
        }
    }

    let mut close = false;
    let mut clicked_op: Option<usize> = None;
    let mut clicked_operand: Option<(u8, usize)> = None;
    let mut clicked_field: Option<u8> = None;
    egui::Window::new("COMPARE CONTACT")
        // Above the workbench overlay (Foreground), which is itself above the page.
        .order(egui::Order::Tooltip)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_min_width(560.0);
            ui.set_max_width(560.0);
            let symbol = COMPARE_OPS[op_index].1.replace("A ", "").replace(" B", "");
            ui.label(
                RichText::new(format!("{} {symbol} {}", edit.left, edit.right))
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new("CONDITION")
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            for (index, (op, formula)) in COMPARE_OPS.iter().enumerate() {
                if window_row(
                    ui,
                    op,
                    formula,
                    index == op_index,
                    edit.focus == 0,
                    index % 2 == 1,
                )
                .clicked()
                {
                    clicked_op = Some(index);
                }
            }
            ui.add_space(6.0);
            ui.horizontal_top(|ui| {
                let width = (ui.available_width() - 12.0) / 2.0;
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        let (field, choice) =
                            operand_column(ui, "A", &mut edit.left, edit.focus == 1, &choices);
                        if field {
                            clicked_field = Some(1);
                        }
                        if let Some(index) = choice {
                            clicked_operand = Some((1, index));
                        }
                    },
                );
                ui.add_space(12.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        let (field, choice) =
                            operand_column(ui, "B", &mut edit.right, edit.focus == 2, &choices);
                        if field {
                            clicked_field = Some(2);
                        }
                        if let Some(index) = choice {
                            clicked_operand = Some((2, index));
                        }
                    },
                );
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new("MW0–MW15 · 25 · T#250ms · TON0.ET · CTU0.CV")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            if let Some(error) = &edit.error {
                ui.label(
                    RichText::new(error)
                        .size(theme::font_size(9.0))
                        .color(theme::ERROR),
                );
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new("↑ ↓ WALK · ← → A / B · ENTER NEXT · ESC CANCEL")
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
    if let Some(index) = clicked_op {
        edit.op = COMPARE_OPS[index].0.to_string();
        edit.focus = 0;
    }
    if let Some(field) = clicked_field {
        edit.focus = field;
    }
    if let Some((field, index)) = clicked_operand {
        if field == 1 {
            edit.left = choices[index].value.clone();
        } else {
            edit.right = choices[index].value.clone();
        }
        edit.error = None;
    }
    if apply {
        actions.push(Action::SetCell {
            section: edit.section,
            rung: edit.rung,
            position: edit.position,
            cell: Cell::Compare {
                op: COMPARE_OPS[op_index].0.to_string(),
                left: edit.left.clone(),
                right: edit.right.clone(),
            },
        });
        close = true;
    }
    if close {
        plc.compare_edit = None;
    } else {
        plc.compare_edit = Some(edit);
    }
}

/// The compute operations the compiler implements, in the order the rail shows
/// them, with the assignment each one writes.
const COMPUTE_OPS: [(&str, &str); 5] = [
    ("ADD", "D := A + B"),
    ("SUB", "D := A - B"),
    ("MUL", "D := A * B"),
    ("DIV", "D := A / B"),
    ("MOVE", "D := A"),
];

/// The destination of an assignment: a marker word, and nothing else. The
/// compiler writes compute results to `MWn`, so offering a literal or a block
/// member here would only produce a cell that never runs.
fn destination_choices() -> Vec<Choice> {
    (0..linc_plc_core::compile::WORD_COUNT)
        .map(|index| Choice::new(format!("MW{index}"), "WORD"))
        .collect()
}

/// Why this text cannot be an assignment destination.
fn destination_error(text: &str) -> Option<String> {
    let value = text.trim();
    if value.is_empty() {
        return Some("D needs a word".to_string());
    }
    if linc_plc_core::il::word_operand(value).is_some() {
        return None;
    }
    Some(format!("{value} is not a marker word: MW0 to MW15"))
}

/// The compute window: the assignment and its three words, like the compare
/// window but for `D := A op B`. MOVE has no second operand, so B stands down
/// and the window says so instead of asking for a value nobody reads.
fn compute_editor(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let Some(mut edit) = plc.compute_edit.clone() else {
        return;
    };
    let choices = compare_choices(plc);
    let words = destination_choices();
    let keys = modal_keys(ctx);
    let move_only = edit.op == "MOVE";
    if keys.escape {
        plc.compute_edit = None;
        return;
    }
    let op_index = COMPUTE_OPS
        .iter()
        .position(|(op, _)| *op == edit.op)
        .unwrap_or(0);
    let editing_value = edit.focus >= 1;
    let mut typed = false;
    for character in &keys.typed {
        if !editing_value {
            continue;
        }
        if character.is_ascii_alphanumeric() || matches!(character, '#' | '.' | ':' | '-') {
            match edit.focus {
                1 => edit.target.push(*character),
                2 => edit.left.push(*character),
                _ => edit.right.push(*character),
            }
            typed = true;
        }
    }
    if keys.backspace && editing_value {
        match edit.focus {
            1 => {
                edit.target.pop();
            }
            2 => {
                edit.left.pop();
            }
            _ => {
                edit.right.pop();
            }
        }
        typed = true;
    }
    if typed {
        edit.error = None;
    }

    let target_cursor = words.iter().position(|choice| choice.value == edit.target);
    let left_cursor = choices.iter().position(|choice| choice.value == edit.left);
    let right_cursor = choices.iter().position(|choice| choice.value == edit.right);
    if keys.up {
        match edit.focus {
            0 => {
                if op_index == 0 {
                    edit.focus = if move_only { 2 } else { 3 };
                } else {
                    edit.op = COMPUTE_OPS[op_index - 1].0.to_string();
                }
            }
            1 => match target_cursor {
                Some(0) | None => edit.focus = 0,
                Some(index) => edit.target = words[index - 1].value.clone(),
            },
            2 => match left_cursor {
                Some(0) | None => edit.focus = 1,
                Some(index) => edit.left = choices[index - 1].value.clone(),
            },
            _ => match right_cursor {
                Some(0) | None => edit.focus = 2,
                Some(index) => edit.right = choices[index - 1].value.clone(),
            },
        }
    }
    if keys.down {
        match edit.focus {
            0 => {
                if op_index + 1 >= COMPUTE_OPS.len() {
                    edit.focus = 1;
                } else {
                    edit.op = COMPUTE_OPS[op_index + 1].0.to_string();
                }
            }
            1 => match target_cursor {
                Some(index) if index + 1 < words.len() => {
                    edit.target = words[index + 1].value.clone()
                }
                _ => edit.focus = 2,
            },
            2 => match left_cursor {
                Some(index) if index + 1 < choices.len() => {
                    edit.left = choices[index + 1].value.clone()
                }
                _ => edit.focus = if move_only { 0 } else { 3 },
            },
            _ => match right_cursor {
                Some(index) if index + 1 < choices.len() => {
                    edit.right = choices[index + 1].value.clone()
                }
                _ => edit.focus = 0,
            },
        }
    }
    // Left and right walk the three word fields directly.
    let step = ui_key(ctx, egui::Key::ArrowRight) as i8 - ui_key(ctx, egui::Key::ArrowLeft) as i8;
    if step != 0 {
        let last = if move_only { 2 } else { 3 };
        let current = edit.focus.clamp(1, last);
        edit.focus = (current as i8 + step).clamp(1, last as i8) as u8;
    }

    let mut apply = false;
    if keys.enter {
        match edit.focus {
            0 => edit.focus = 1,
            1 => match destination_error(&edit.target) {
                Some(error) => edit.error = Some(format!("D {error}")),
                None => {
                    edit.target = edit.target.trim().to_ascii_uppercase();
                    edit.focus = 2;
                    edit.error = None;
                }
            },
            2 => match operand_error(&edit.left) {
                Some(error) => edit.error = Some(format!("A {error}")),
                None => {
                    edit.left = edit.left.trim().to_string();
                    edit.error = None;
                    if move_only {
                        apply = true;
                    } else {
                        edit.focus = 3;
                    }
                }
            },
            _ => match operand_error(&edit.right) {
                Some(error) => edit.error = Some(format!("B {error}")),
                None => {
                    edit.right = edit.right.trim().to_string();
                    apply = true;
                }
            },
        }
    }

    let mut close = false;
    let mut clicked_op: Option<usize> = None;
    let mut clicked_field: Option<u8> = None;
    let mut clicked_value: Option<(u8, usize)> = None;
    let op = COMPUTE_OPS[op_index].0.to_string();
    let symbol = match op.as_str() {
        "ADD" => "+",
        "SUB" => "-",
        "MUL" => "*",
        "DIV" => "/",
        _ => "",
    };
    let summary = if move_only {
        format!("{} := {}", edit.target, edit.left)
    } else {
        format!("{} := {} {symbol} {}", edit.target, edit.left, edit.right)
    };
    egui::Window::new("COMPUTE COIL")
        .order(egui::Order::Tooltip)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_min_width(660.0);
            ui.set_max_width(660.0);
            ui.label(
                RichText::new(summary.clone())
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new("OPERATION")
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            for (index, (name, formula)) in COMPUTE_OPS.iter().enumerate() {
                if window_row(
                    ui,
                    name,
                    formula,
                    index == op_index,
                    edit.focus == 0,
                    index % 2 == 1,
                )
                .clicked()
                {
                    clicked_op = Some(index);
                }
            }
            ui.add_space(6.0);
            ui.horizontal_top(|ui| {
                let gap = 12.0;
                let width = (ui.available_width() - gap * 2.0) / 3.0;
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        let (field, choice) =
                            operand_column(ui, "D", &mut edit.target, edit.focus == 1, &words);
                        if field {
                            clicked_field = Some(1);
                        }
                        if let Some(index) = choice {
                            clicked_value = Some((1, index));
                        }
                    },
                );
                ui.add_space(gap);
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        let (field, choice) =
                            operand_column(ui, "A", &mut edit.left, edit.focus == 2, &choices);
                        if field {
                            clicked_field = Some(2);
                        }
                        if let Some(index) = choice {
                            clicked_value = Some((2, index));
                        }
                    },
                );
                ui.add_space(gap);
                ui.allocate_ui_with_layout(
                    egui::vec2(width, 0.0),
                    egui::Layout::top_down(egui::Align::LEFT),
                    |ui| {
                        if move_only {
                            ui.label(
                                RichText::new("B")
                                    .monospace()
                                    .size(theme::font_size(9.0))
                                    .color(theme::MUTED),
                            );
                            ui.label(
                                RichText::new("not used by MOVE")
                                    .size(theme::font_size(10.0))
                                    .color(theme::MUTED),
                            );
                            return;
                        }
                        let (field, choice) =
                            operand_column(ui, "B", &mut edit.right, edit.focus == 3, &choices);
                        if field {
                            clicked_field = Some(3);
                        }
                        if let Some(index) = choice {
                            clicked_value = Some((3, index));
                        }
                    },
                );
            });
            ui.add_space(6.0);
            ui.label(
                RichText::new("D MW0–MW15 · A and B MW0–MW15, 25, T#250ms, TON0.ET, CTU0.CV")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            if let Some(error) = &edit.error {
                ui.label(
                    RichText::new(error)
                        .size(theme::font_size(9.0))
                        .color(theme::ERROR),
                );
            }
            ui.add_space(8.0);
            ui.label(
                RichText::new("↑ ↓ WALK · ← → D / A / B · ENTER NEXT · ESC CANCEL")
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
    if let Some(index) = clicked_op {
        edit.op = COMPUTE_OPS[index].0.to_string();
        edit.focus = 0;
    }
    if let Some(field) = clicked_field {
        edit.focus = field;
    }
    if let Some((field, index)) = clicked_value {
        match field {
            1 => edit.target = words[index].value.clone(),
            2 => edit.left = choices[index].value.clone(),
            _ => edit.right = choices[index].value.clone(),
        }
        edit.error = None;
    }
    if apply {
        // MOVE reads only A, but the compiler still needs a parseable B on the
        // cell; zero is what it has always been given.
        let right = if move_only {
            "0".to_string()
        } else {
            edit.right.clone()
        };
        actions.push(Action::SetCell {
            section: edit.section,
            rung: edit.rung,
            position: edit.position,
            cell: Cell::Compute {
                op: COMPUTE_OPS[op_index].0.to_string(),
                target: edit.target.clone(),
                left: edit.left.clone(),
                right,
            },
        });
        close = true;
    }
    if close {
        plc.compute_edit = None;
    } else {
        plc.compute_edit = Some(edit);
    }
}

/// One scan of a rung for the preview: the power flow a held input produces, so
/// contacts and the coil light together. Blocks are opaque; they pass the power
/// they were given because their logic is not evaluated here.
fn preview_scan(plc: &PlcUi, cells: &[Cell]) -> (Vec<Option<bool>>, Option<bool>) {
    let mut local = vec![None; POSITIONS];
    let mut power = true;
    for (position, cell) in cells.iter().enumerate() {
        let value = match cell {
            Cell::Contact { var, negated } => {
                let bit = plc.bit_value(var).unwrap_or(false);
                power = power && (bit != *negated);
                Some(power)
            }
            Cell::Edge { var, .. } => {
                let bit = plc.bit_value(var).unwrap_or(false);
                power = power && bit;
                Some(power)
            }
            Cell::Compare { op, left, right } => {
                let result = match (plc.word_value(left), plc.word_value(right)) {
                    (Some(a), Some(b)) => match op.to_ascii_uppercase().as_str() {
                        "GT" => a > b,
                        "GE" => a >= b,
                        "EQ" => a == b,
                        "NE" => a != b,
                        "LE" => a <= b,
                        "LT" => a < b,
                        _ => false,
                    },
                    _ => false,
                };
                power = power && result;
                Some(power)
            }
            Cell::Compute { .. } | Cell::Block { .. } => Some(power),
            Cell::Wire | Cell::Link => Some(power),
        };
        if let Some(slot) = local.get_mut(position) {
            *slot = value;
        }
    }
    (local, Some(power))
}

/// One grid rung: nine element positions, the rail, and the coil.
pub fn grid_rung(
    plc: &PlcUi,
    ui: &mut Ui,
    section: usize,
    rung_index: usize,
    rung: &Rung,
    actions: &mut Vec<Action>,
    cursor_rect: &mut Option<egui::Rect>,
) {
    let _ = actions;
    // A rung drawn before the grid existed has no cells; its flat series is
    // shown as cells without being written back, so the canvas is the same
    // shape whether or not the project has been touched yet.
    let cells = if rung.cells.is_empty() {
        linc_plc_core::grid::migrate(&rung.logic).unwrap_or_default()
    } else {
        rung.cells.clone()
    };
    // With a preview bit held, the rung is evaluated locally so the rest of the
    // logic follows the held input: only the toggled contact turning green said
    // nothing about what the rung would do.
    let previewing = !plc.preview.is_empty();
    let local = previewing.then(|| preview_scan(plc, &cells));
    // The comment sits inside the strip's upper band so every rung is exactly
    // `CELL_HEIGHT` tall: the link's drop to the branch row stays exact.
    let commented = !rung.comment.is_empty();
    let strip = ui.horizontal(|ui| {
        // Zero spacing: neighbouring cells share one conductor with no joint.
        ui.spacing_mut().item_spacing.x = 0.0;
        for position in 0..POSITIONS {
            let cell = cells.get(position).cloned().unwrap_or(Cell::Wire);
            let label = grid_cell_label(plc, &cell);
            let rect = cell_box(
                plc,
                ui,
                section,
                rung_index,
                position,
                &label,
                matches!(cell, Cell::Wire),
                cell_symbol(&cell),
                if commented { 18.0 } else { 16.0 },
                local
                    .as_ref()
                    .and_then(|(values, _)| values.get(position).copied().flatten())
                    .or_else(|| cell_active(plc, &cell)),
            );
            if plc.cell_cursor == Some((section, rung_index, position)) {
                *cursor_rect = Some(rect);
            }
        }
        let coil = plc.operand_label(&rung.coil.var);
        let coil_rect = cell_box(
            plc,
            ui,
            section,
            rung_index,
            POSITIONS,
            &coil,
            rung.coil.var.is_empty(),
            CellSymbol::Coil(rung.coil.kind),
            if commented { 18.0 } else { 16.0 },
            // A compute rung writes through its cell, so its coil slot is unused
            // and must not light up from a variable of the same name.
            if let Some((_, coil)) = &local {
                *coil
            } else if cells
                .iter()
                .any(|cell| matches!(cell, linc_plc_core::grid::Cell::Compute { .. }))
            {
                None
            } else {
                plc.bit_value(&rung.coil.var)
            },
        );
        if plc.cell_cursor == Some((section, rung_index, POSITIONS)) {
            *cursor_rect = Some(coil_rect);
        }
    });
    if plc.cell_cursor == Some((section, rung_index, POSITIONS)) {
        // A faint edge says the whole rung is the unit being addressed while the
        // coil is the cell with the cursor.
        let rect = strip.response.rect;
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            1.0,
            Color32::from_rgba_unmultiplied(0x4d, 0xa3, 0xff, 36),
        );
    }
    if commented {
        // Inside the strip: the rung stays exactly CELL_HEIGHT tall, so the link's
        // drop to the branch row lands on that row's conductor.
        let rect = strip.response.rect;
        ui.painter().text(
            egui::pos2(rect.left() + 2.0, rect.top() + 8.0),
            Align2::LEFT_CENTER,
            format!("// {}", rung.comment),
            FontId::monospace(theme::font_size(8.0)),
            theme::COMMENT,
        );
    }
    ui.add_space(ROW_GAP);
}

/// The graphical element a cell stands for. The ladder draws IEC symbols, not
/// text: a contact is two bars, a coil a rounded loop, a link a junction, and so
/// on. The operand name is drawn beside the symbol, never instead of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CellSymbol {
    Wire,
    Link,
    /// Normally open contact.
    Contact,
    /// Normally closed contact: the bars plus a slash.
    ContactNc,
    /// Rising or falling edge contact.
    Edge(bool),
    /// Coil: normal, set or reset.
    Coil(CoilKind),
    Block,
    Compare,
    Compute,
}

/// Map a cell to the symbol the grid draws.
pub fn cell_symbol(cell: &Cell) -> CellSymbol {
    match cell {
        Cell::Wire => CellSymbol::Wire,
        Cell::Link => CellSymbol::Link,
        Cell::Contact { negated, .. } => {
            if *negated {
                CellSymbol::ContactNc
            } else {
                CellSymbol::Contact
            }
        }
        Cell::Edge { rising, .. } => CellSymbol::Edge(*rising),
        Cell::Block { .. } => CellSymbol::Block,
        Cell::Compare { .. } => CellSymbol::Compare,
        Cell::Compute { .. } => CellSymbol::Compute,
    }
}

/// ZX geometry: one continuous conductor per rung, name above the symbol, no
/// cell borders and no spacing between cells. A row is a strip whose conductor
/// runs along `CONDUCTOR_Y` from its left edge to its right edge.
pub const CELL_WIDTH: f32 = 48.0;
pub const CELL_HEIGHT: f32 = 50.0;
pub const CONDUCTOR_Y: f32 = 39.0;
pub const ROW_GAP: f32 = 0.0;
/// Distance from one row's conductor to the next row's conductor.
pub const ROW_STRIDE: f32 = CELL_HEIGHT + ROW_GAP;

fn arc_points(
    centre: egui::Pos2,
    radius: f32,
    from: f32,
    to: f32,
    steps: usize,
) -> Vec<egui::Pos2> {
    (0..=steps)
        .map(|step| {
            let t = from + (to - from) * (step as f32 / steps as f32);
            egui::pos2(centre.x + radius * t.cos(), centre.y + radius * t.sin())
        })
        .collect()
}

pub(crate) fn draw_cell_symbol(
    painter: &egui::Painter,
    rect: egui::Rect,
    symbol: CellSymbol,
    color: Color32,
) {
    let line = Stroke::new(1.6, color);
    let thin = Stroke::new(1.2, color);
    let y = rect.top() + CONDUCTOR_Y;
    let cx = rect.center().x;
    // Every element's leads run to the strip edges, so neighbouring cells share
    // one unbroken conductor with no visible joint.
    let left = rect.left();
    let right = rect.right();
    match symbol {
        CellSymbol::Wire => {
            painter.line_segment([egui::pos2(left, y), egui::pos2(right, y)], thin);
        }
        CellSymbol::Link => {
            painter.line_segment([egui::pos2(left, y), egui::pos2(right, y)], thin);
            // The junction reaches the branch row's conductor exactly.
            painter.line_segment([egui::pos2(cx, y), egui::pos2(cx, y + ROW_STRIDE)], line);
        }
        CellSymbol::Contact | CellSymbol::ContactNc | CellSymbol::Edge(_) => {
            let bar = 6.5;
            painter.line_segment([egui::pos2(left, y), egui::pos2(cx - bar, y)], thin);
            painter.line_segment([egui::pos2(cx + bar, y), egui::pos2(right, y)], thin);
            painter.line_segment(
                [egui::pos2(cx - bar, y - 8.0), egui::pos2(cx - bar, y + 8.0)],
                line,
            );
            painter.line_segment(
                [egui::pos2(cx + bar, y - 8.0), egui::pos2(cx + bar, y + 8.0)],
                line,
            );
            if symbol == CellSymbol::ContactNc {
                painter.line_segment(
                    [egui::pos2(cx - 9.0, y + 8.0), egui::pos2(cx + 9.0, y - 8.0)],
                    line,
                );
            }
            if let CellSymbol::Edge(rising) = symbol {
                // ZX writes the letter between the bars; an arrow over the bar read
                // as a stray plus sign at this size.
                painter.text(
                    egui::pos2(cx, y - 11.0),
                    Align2::CENTER_CENTER,
                    if rising { "P" } else { "N" },
                    FontId::monospace(8.0),
                    color,
                );
            }
        }
        CellSymbol::Coil(kind) => {
            // Two curved parentheses, as the reference draws a coil.
            let radius = 9.0;
            let centre = egui::pos2(cx, y);
            painter.line_segment([egui::pos2(left, y), egui::pos2(cx - radius, y)], thin);
            painter.line_segment([egui::pos2(cx + radius, y), egui::pos2(right, y)], thin);
            let left_arc = arc_points(centre, radius, 1.9, 4.4, 12);
            let right_arc = arc_points(centre, radius, -1.25, 1.25, 12);
            painter.add(egui::Shape::line(left_arc, line));
            painter.add(egui::Shape::line(right_arc, line));
            match kind {
                CoilKind::Set | CoilKind::Reset => {
                    let letter = if kind == CoilKind::Set { "S" } else { "R" };
                    painter.text(
                        egui::pos2(cx - radius - 5.0, y - 9.0),
                        Align2::CENTER_CENTER,
                        letter,
                        FontId::monospace(8.5),
                        color,
                    );
                }
                CoilKind::Negated => {
                    painter.line_segment(
                        [
                            egui::pos2(cx + radius + 2.0, y - 7.0),
                            egui::pos2(cx + radius + 8.0, y + 7.0),
                        ],
                        line,
                    );
                }
                CoilKind::Normal => {}
            }
        }
        CellSymbol::Block | CellSymbol::Compare | CellSymbol::Compute => {
            let body = egui::Rect::from_min_max(
                egui::pos2(cx - 16.0, y - 9.0),
                egui::pos2(cx + 16.0, y + 9.0),
            );
            painter.rect_stroke(body, 0.0, line, egui::StrokeKind::Middle);
            painter.line_segment([egui::pos2(left, y), egui::pos2(body.left(), y)], thin);
            painter.line_segment([egui::pos2(body.right(), y), egui::pos2(right, y)], thin);
            if symbol == CellSymbol::Compute {
                painter.text(
                    body.center(),
                    Align2::CENTER_CENTER,
                    ":=",
                    FontId::monospace(9.0),
                    color,
                );
            }
        }
    }
}

/// The element's live state, when the snapshot can answer for it. `None` means the
/// value is not in the snapshot — a block's `Q` is not published yet — so the
/// renderer leaves the element neutral instead of guessing.
pub fn cell_active(plc: &PlcUi, cell: &Cell) -> Option<bool> {
    match cell {
        Cell::Contact { var, negated } => plc.bit_value(var).map(|value| value != *negated),
        Cell::Edge { var, .. } => plc.bit_value(var),
        Cell::Compare { op, left, right } => {
            let a = plc.word_value(left)?;
            let b = plc.word_value(right)?;
            Some(match op.to_ascii_uppercase().as_str() {
                "GT" => a > b,
                "GE" => a >= b,
                "EQ" => a == b,
                "NE" => a != b,
                "LE" => a <= b,
                "LT" => a < b,
                _ => a > b,
            })
        }
        Cell::Block { instance, .. } => plc.bit_value(&format!("{instance}.Q")),
        Cell::Wire | Cell::Link | Cell::Compute { .. } => None,
    }
}

fn cell_box(
    plc: &PlcUi,
    ui: &mut Ui,
    section: usize,
    rung: usize,
    position: usize,
    label: &str,
    quiet: bool,
    symbol: CellSymbol,
    name_y: f32,
    live: Option<bool>,
) -> egui::Rect {
    let size = Vec2::new(CELL_WIDTH, CELL_HEIGHT);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let selected = plc.cell_cursor == Some((section, rung, position));
    let painter = ui.painter();
    // No per-cell background or border: the strip is bare and the conductor is
    // what the eye follows. Selection tints the element and marks the cell with
    // two small ticks, never a box.
    let color: Color32 = if live == Some(true) {
        // A true element is green, as the manual's monitor shows it.
        theme::C4
    } else if quiet {
        theme::MUTED
    } else if selected {
        theme::ACCENT
    } else {
        theme::FG
    };
    if selected {
        let tick = Stroke::new(1.6, theme::ACCENT);
        let y = rect.top() + CONDUCTOR_Y;
        painter.line_segment(
            [
                egui::pos2(rect.left() + 1.0, y - 13.0),
                egui::pos2(rect.left() + 1.0, y - 7.0),
            ],
            tick,
        );
        painter.line_segment(
            [
                egui::pos2(rect.right() - 1.0, y - 13.0),
                egui::pos2(rect.right() - 1.0, y - 7.0),
            ],
            tick,
        );
    }
    draw_cell_symbol(painter, rect, symbol, color);
    if !label.is_empty() {
        // Fit the operand inside the cell: expressions such as `MW2:=MW1+5` and
        // long coil names were clipped at the strip edges. The font shrinks to fit
        // rather than the name being cut, and a very long name keeps a floor so it
        // stays legible.
        let available = rect.width() - 4.0;
        let mut size = theme::font_size(8.5);
        loop {
            let galley = painter.layout_no_wrap(label.to_string(), FontId::monospace(size), color);
            if galley.size().x <= available || size <= 5.5 {
                painter.galley(
                    egui::pos2(
                        rect.center().x - galley.size().x / 2.0,
                        rect.top() + name_y - galley.size().y / 2.0,
                    ),
                    galley,
                    color,
                );
                break;
            }
            size -= 0.5;
        }
    }
    rect
}

/// What a cell shows: the operand in the chosen name/address mode, with the
/// negation and edge markers the element itself carries. Rendering it through
/// `operand_label` alone dropped the `/`, which made a negated contact look
/// identical to a plain one.
pub fn grid_cell_label(plc: &PlcUi, cell: &Cell) -> String {
    match cell {
        Cell::Contact { var, negated } => {
            let name = plc.operand_label(var);
            if *negated {
                format!("/{name}")
            } else {
                name
            }
        }
        Cell::Edge { var, .. } => plc.operand_label(var),
        Cell::Block { instance, .. } => plc.operand_label(instance),
        other => cell_label(other),
    }
}

fn cell_label(cell: &Cell) -> String {
    match cell {
        Cell::Wire | Cell::Link => String::new(),
        Cell::Contact { var, negated } => {
            if *negated {
                format!("/{var}")
            } else {
                var.clone()
            }
        }
        Cell::Edge { var, rising } => format!("{}{var}", if *rising { "P " } else { "N " }),
        Cell::Block { instance, .. } => instance.clone(),
        Cell::Compare { op, left, right } => {
            let upper = op.to_ascii_uppercase();
            let symbol = match upper.as_str() {
                "GT" => ">",
                "GE" => ">=",
                "EQ" => "=",
                "NE" => "<>",
                "LE" => "<=",
                "LT" => "<",
                other => other,
            };
            format!("{left}{symbol}{right}")
        }
        Cell::Compute {
            op,
            target,
            left,
            right,
        } => {
            let upper = op.to_ascii_uppercase();
            let symbol = match upper.as_str() {
                "ADD" => "+",
                "SUB" => "-",
                "MUL" => "*",
                "DIV" => "/",
                "MOVE" => "",
                other => other,
            };
            if symbol.is_empty() {
                format!("{target}:={left}")
            } else {
                format!("{target}:={left}{symbol}{right}")
            }
        }
    }
}

#[cfg(test)]
mod symbol_tests {
    use super::*;

    /// The grid draws IEC symbols, so every element the manual has maps to a
    /// distinct shape rather than to a text label.
    #[test]
    fn every_element_maps_to_its_own_symbol() {
        assert_eq!(cell_symbol(&Cell::Wire), CellSymbol::Wire);
        assert_eq!(cell_symbol(&Cell::Link), CellSymbol::Link);
        assert_eq!(
            cell_symbol(&Cell::Contact {
                var: "a".into(),
                negated: false
            }),
            CellSymbol::Contact
        );
        assert_eq!(
            cell_symbol(&Cell::Contact {
                var: "a".into(),
                negated: true
            }),
            CellSymbol::ContactNc
        );
        assert_eq!(
            cell_symbol(&Cell::Edge {
                var: "a".into(),
                rising: true
            }),
            CellSymbol::Edge(true)
        );
        assert_eq!(
            cell_symbol(&Cell::Edge {
                var: "a".into(),
                rising: false
            }),
            CellSymbol::Edge(false)
        );
        assert_eq!(
            cell_symbol(&Cell::Block {
                block: "TON".into(),
                instance: "TON0".into(),
                preset: "T#1s".into()
            }),
            CellSymbol::Block
        );
        assert_eq!(
            cell_symbol(&Cell::Compare {
                op: "GT".into(),
                left: "MW1".into(),
                right: "5".into()
            }),
            CellSymbol::Compare
        );
        assert_eq!(
            cell_symbol(&Cell::Compute {
                op: "ADD".into(),
                target: "MW2".into(),
                left: "MW1".into(),
                right: "5".into()
            }),
            CellSymbol::Compute
        );
        // The three coil kinds each draw differently.
        assert_ne!(
            CellSymbol::Coil(CoilKind::Set),
            CellSymbol::Coil(CoilKind::Reset)
        );
        assert_ne!(
            CellSymbol::Coil(CoilKind::Normal),
            CellSymbol::Coil(CoilKind::Set)
        );
    }

    /// A cell keeps room for its operand name beside the symbol.
    #[test]
    fn the_symbol_leaves_room_for_the_name() {
        let plc = PlcUi::default();
        let cell = Cell::Contact {
            var: "pump".into(),
            negated: false,
        };
        assert_eq!(grid_cell_label(&plc, &cell), "pump");
        let negated = Cell::Contact {
            var: "estop".into(),
            negated: true,
        };
        assert_eq!(grid_cell_label(&plc, &negated), "/estop");
        assert!(
            CELL_WIDTH >= 40.0,
            "a cell has room for a symbol and its name"
        );
    }
}

#[cfg(test)]
mod label_clearance_tests {
    use super::*;

    /// The operand label must sit clear of the symbol. With the strip's fixed
    /// geometry the gap is arithmetic, so a change to either constant that closes
    /// the clearance fails here instead of shipping a crowded ladder.
    #[test]
    fn the_label_clears_the_symbol_on_commented_and_plain_rows() {
        const LABEL_HALF: f32 = 4.5; // half a 9 px galley
        const SYMBOL_HALF: f32 = 9.0; // tallest symbol above the conductor
        for name_y in [18.0_f32, 10.0] {
            let label_bottom = name_y + LABEL_HALF;
            let symbol_top = CONDUCTOR_Y - SYMBOL_HALF;
            assert!(
                symbol_top - label_bottom >= 7.0,
                "label bottom {label_bottom} to symbol top {symbol_top} must be 7 px"
            );
        }
        assert!(
            CONDUCTOR_Y < CELL_HEIGHT,
            "the conductor stays inside the strip"
        );
    }

    /// The link reaches the next row's conductor exactly: the drop is the strip
    /// height plus the conductor offset, with no row gap.
    #[test]
    fn the_link_pitch_is_one_strip() {
        assert_eq!(ROW_STRIDE, CELL_HEIGHT + ROW_GAP);
        assert_eq!(ROW_GAP, 0.0);
    }
}

#[cfg(test)]
mod rung_pitch_tests {
    use super::*;

    fn pass_with<T>(mut body: impl FnMut(&mut Ui) -> T) -> T {
        let ctx = egui::Context::default();
        let mut out: Option<T> = None;
        let mut output = ctx.run_pass(egui::RawInput::default(), |ctx| {
            egui::Area::new(egui::Id::new("rung-pitch"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ctx, |ui| {
                    out = Some(body(ui));
                });
        });
        let _ = &ctx;
        output.textures_delta.clear();
        out.expect("body ran")
    }

    /// The ladder must not inherit egui's vertical item spacing: the link's drop is
    /// one strip long, so two rungs have to be exactly `CELL_HEIGHT` apart in the
    /// real layout, not only in the constants.
    #[test]
    fn consecutive_rungs_are_one_strip_apart_in_the_layout() {
        let mut plc = PlcUi::default();
        let rung = Rung {
            cells: vec![Cell::Wire],
            comment: String::new(),
            logic: linc_plc_core::model::Logic::Const { value: true },
            coil: linc_plc_core::model::Coil::new("", linc_plc_core::model::CoilKind::Normal),
        };
        let mut actions = Vec::new();
        let (advance, spacing) = pass_with(|ui| {
            crate::screens::plc::zero_rung_spacing(ui);
            let before = ui.cursor().top();
            let mut cursor_rect = None;
            grid_rung(&plc, ui, 0, 0, &rung, &mut actions, &mut cursor_rect);
            let step = ui.cursor().top() - before;
            grid_rung(&plc, ui, 0, 1, &rung, &mut actions, &mut cursor_rect);
            let two = ui.cursor().top() - before;
            (step, two - step)
        });
        let _ = &mut plc;
        assert!(
            (advance - CELL_HEIGHT).abs() < 0.5,
            "one rung advances {advance}, expected {CELL_HEIGHT}"
        );
        assert!(
            (spacing - CELL_HEIGHT).abs() < 0.5,
            "the second rung starts one strip lower, got {spacing}"
        );
    }

    /// Zeroing the spacing is what the ladder does, and it returns the value so the
    /// caller cannot silently keep egui's default.
    #[test]
    fn the_ladder_zeroes_the_vertical_spacing() {
        let spacing = pass_with(|ui| crate::screens::plc::zero_rung_spacing(ui));
        assert_eq!(spacing, 0.0);
    }
}

#[cfg(test)]
mod block_window_tests {
    use super::*;

    /// The hint under the field is the rule: what it says is accepted, what it
    /// does not say is refused with a reason.
    #[test]
    fn timer_presets_need_a_number_and_a_unit() {
        assert!(preset_is_valid(false, "T#250ms").is_ok());
        assert!(preset_is_valid(false, "T#2s").is_ok());
        assert!(preset_is_valid(false, "T#1.5s").is_ok());
        assert!(preset_is_valid(false, "T#1m").is_ok());
        assert_eq!(
            preset_is_valid(false, "T#2").unwrap_err(),
            "T#2 needs a unit: ms, s or m"
        );
        assert_eq!(
            preset_is_valid(false, "2s").unwrap_err(),
            "2s must start with T#, e.g. T#250ms"
        );
        assert_eq!(
            preset_is_valid(false, "T#2h").unwrap_err(),
            "h is not a unit: use ms, s or m"
        );
    }

    #[test]
    fn counter_presets_are_whole_numbers() {
        assert!(preset_is_valid(true, "25").is_ok());
        assert_eq!(
            preset_is_valid(true, "2.5").unwrap_err(),
            "2.5 is not a whole number, e.g. 25"
        );
    }

    /// The window title says what the box does, not just its code.
    #[test]
    fn the_box_window_is_titled_by_its_job() {
        assert_eq!(box_title("TON"), "ON-DELAY TIMER (TON)");
        assert_eq!(box_title("TOF"), "OFF-DELAY TIMER (TOF)");
        assert_eq!(box_title("TP"), "PULSE TIMER (TP)");
        assert_eq!(box_title("CTU"), "UP COUNTER (CTU)");
        assert_eq!(box_title("CTD"), "DOWN COUNTER (CTD)");
    }

    /// The box being edited does not hold its own instance: showing "IN USE" for
    /// the instance the window is editing reads as if another box had it.
    #[test]
    fn the_box_being_edited_does_not_use_its_own_instance() {
        let mut plc = plc_with_timer("TON0");
        let editing = (0, 0, 1);
        assert!(used_instances(&plc, "TON", editing).is_empty());
        assert_eq!(
            used_instances(&plc, "TON", (0, 0, 0)),
            vec!["TON0".to_string()],
            "another cell still holds it"
        );
    }

    /// The window's instance list is the block type's four slots, named the way
    /// the ladder names them.
    #[test]
    fn the_instance_list_is_four_slots_of_the_same_block() {
        assert_eq!(instance_number("TON", "TON2"), Some(2));
        assert_eq!(instance_number("TON", "TOF2"), None);
        assert_eq!(instance_number("TON", "TON9"), None);
    }

    fn frame(plc: &mut PlcUi, events: Vec<egui::Event>) -> Vec<Action> {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut actions = Vec::new();
        let mut output = ctx.run_pass(input, |ctx| {
            pickers(plc, ctx, &mut actions);
        });
        output.textures_delta.clear();
        actions
    }

    fn plc_with_timer(instance: &str) -> PlcUi {
        let mut plc = PlcUi::default();
        let mut pou = linc_plc_core::model::Pou::new("main", linc_plc_core::model::PouLanguage::Ld);
        pou.sections = vec![linc_plc_core::model::Section {
            name: String::new(),
            rungs: vec![linc_plc_core::model::Rung {
                comment: String::new(),
                logic: linc_plc_core::model::Logic::Const { value: true },
                coil: linc_plc_core::model::Coil::new("", linc_plc_core::model::CoilKind::Normal),
                cells: vec![
                    Cell::Wire,
                    Cell::Block {
                        block: "TON".into(),
                        instance: instance.into(),
                        preset: "T#1s".into(),
                    },
                ],
            }],
        }];
        plc.project = linc_plc_core::model::Project::new("t");
        plc.project.pous = vec![pou];
        plc.pou = 0;
        plc.block_edit = Some((0, 0, 1));
        plc
    }

    /// The arrows are the only way between the two fields, because the panel
    /// keyboard has no Tab: walking off either end of the instance list puts the
    /// keys in the preset, and the next arrow gives the list back where it was.
    #[test]
    fn the_arrows_move_focus_between_the_list_and_the_preset() {
        let keys = |plc: &mut PlcUi, key: egui::Key| {
            frame(
                plc,
                vec![egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            )
        };
        let mut plc = plc_with_timer("TON0");
        keys(&mut plc, egui::Key::ArrowUp);
        assert!(
            plc.block_focus_preset,
            "up off the first row enters the preset"
        );
        keys(&mut plc, egui::Key::ArrowUp);
        assert!(!plc.block_focus_preset, "an arrow gives the list back");
        assert_eq!(plc.block_instance_cursor, 0, "it comes back where it left");

        let mut plc = plc_with_timer("TON3");
        keys(&mut plc, egui::Key::ArrowUp);
        assert!(!plc.block_focus_preset);
        assert_eq!(plc.block_instance_cursor, 2);
        keys(&mut plc, egui::Key::ArrowDown);
        assert_eq!(plc.block_instance_cursor, 3);
        keys(&mut plc, egui::Key::ArrowDown);
        assert!(
            plc.block_focus_preset,
            "down off the last row enters the preset"
        );
    }

    /// Backspace belongs to the field being edited: on the instance list it must
    /// not eat the preset, and it must never reach the ladder behind either.
    #[test]
    fn backspace_only_edits_the_preset_while_the_preset_holds_the_keys() {
        let key = |plc: &mut PlcUi| {
            frame(
                plc,
                vec![egui::Event::Key {
                    key: egui::Key::Backspace,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }],
            )
        };
        let mut plc = plc_with_timer("TON0");
        key(&mut plc); // first frame syncs the buffer from the box
        plc.block_preset_text = "T#5s".into();
        key(&mut plc);
        assert_eq!(
            plc.block_preset_text, "T#5s",
            "the list leaves the preset alone"
        );
        assert_eq!(
            plc.block_instance_cursor, 0,
            "and it is not a list key either"
        );

        plc.block_focus_preset = true;
        key(&mut plc);
        assert_eq!(plc.block_preset_text, "T#5", "the preset field edits");
    }

    /// Escape used to skip the window's draw for one frame and leave its state
    /// alive, so the window came straight back. The sweep clears it.
    #[test]
    fn escape_drops_the_block_window_with_the_pickers() {
        let mut plc = PlcUi::default();
        plc.block_edit = Some((0, 0, 1));
        plc.block_fields = Some((0, 0, 1));
        clear_pickers(&mut plc);
        assert!(plc.block_edit.is_none());
        assert!(plc.block_fields.is_none());
    }
}

#[cfg(test)]
mod compare_window_tests {
    use super::*;

    fn frame(plc: &mut PlcUi, events: Vec<egui::Event>) -> Vec<Action> {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut actions = Vec::new();
        let mut output = ctx.run_pass(input, |ctx| {
            pickers(plc, ctx, &mut actions);
        });
        output.textures_delta.clear();
        actions
    }

    fn frame_owned(plc: &mut PlcUi, events: Vec<egui::Event>) -> (Vec<Action>, bool) {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut actions = Vec::new();
        let mut owned = false;
        let mut output = ctx.run_pass(input, |ctx| {
            owned = pickers(plc, ctx, &mut actions);
        });
        output.textures_delta.clear();
        (actions, owned)
    }

    /// Enter on an existing cell opens that cell's own window, with its values
    /// in it; only an empty or plain cell offers the element list.
    #[test]
    fn enter_opens_the_window_the_cell_already_has() {
        let cursor = (1, 2, 3);
        assert!(matches!(
            enter_target(cursor, Some(&Cell::Block {
                block: "TON".into(),
                instance: "TON2".into(),
                preset: "T#5s".into(),
            })),
            EnterTarget::Block(seen) if seen == cursor
        ));
        match enter_target(
            cursor,
            Some(&Cell::Compare {
                op: "NE".into(),
                left: "MW7".into(),
                right: "T#2s".into(),
            }),
        ) {
            EnterTarget::Compare(edit) => {
                assert_eq!(edit.op, "NE");
                assert_eq!(edit.left, "MW7");
                assert_eq!(edit.right, "T#2s");
                assert_eq!(
                    (edit.section, edit.rung, edit.position),
                    (1, 2, 3),
                    "the window edits the cell Enter was pressed on"
                );
            }
            other => panic!("expected the compare window, got {other:?}"),
        }
        assert!(matches!(
            enter_target(
                cursor,
                Some(&Cell::Contact {
                    var: "pump".into(),
                    negated: false
                })
            ),
            EnterTarget::Operand(_)
        ));
        assert!(matches!(enter_target(cursor, None), EnterTarget::Picker));
    }

    /// A coil writes, so inputs are not choices: the validator refuses them as a
    /// coil target, and offering one only made a cell that cannot compile.
    #[test]
    fn a_coil_can_only_be_given_a_writable_symbol() {
        let mut plc = PlcUi::default();
        let mut point = |name: &str, direction| {
            plc.project.io.push(linc_plc_core::model::IoPoint {
                name: name.into(),
                direction,
                kind: linc_plc_core::model::IoKind::Bit,
                comment: String::new(),
            })
        };
        point("pump", linc_plc_core::model::IoDirection::In);
        point("run", linc_plc_core::model::IoDirection::Out);
        plc.project.vars.push(linc_plc_core::model::VarDecl {
            name: "seal".into(),
            kind: linc_plc_core::model::VarKind::Bit,
            retain: false,
            comment: String::new(),
        });
        let names: Vec<String> = coil_choices(&plc)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert!(!names.contains(&"pump".to_string()), "input offered: {names:?}");
        assert!(names.contains(&"run".to_string()));
        assert!(names.contains(&"seal".to_string()));
    }

    /// The coil slot and a contact each have their own Enter target: the operand
    /// list, not the element picker.
    #[test]
    fn enter_targets_the_coil_and_the_contact_operand() {
        let cursor = (0, 3, 0);
        assert!(matches!(
            enter_target((0, 3, POSITIONS), None),
            EnterTarget::Coil((0, 3, POSITIONS))
        ));
        assert!(matches!(
            enter_target(
                cursor,
                Some(&Cell::Contact {
                    var: "start".into(),
                    negated: false
                })
            ),
            EnterTarget::Operand(_)
        ));
    }

    /// A held input lights the rest of the rung: the local scan must carry power
    /// through the series contacts to the coil, not just colour the contact.
    #[test]
    fn the_preview_scan_carries_power_to_the_coil() {
        let mut plc = PlcUi::default();
        let cells = vec![
            Cell::Contact {
                var: "start".into(),
                negated: false,
            },
            Cell::Wire,
        ];
        plc.preview = vec![("start".into(), true)];
        let (local, coil) = preview_scan(&plc, &cells);
        assert_eq!(local[0], Some(true));
        assert_eq!(local[1], Some(true));
        assert_eq!(coil, Some(true), "the coil follows the held contact");

        plc.preview = vec![("start".into(), false)];
        let (local, coil) = preview_scan(&plc, &cells);
        assert_eq!(local[0], Some(false));
        assert_eq!(coil, Some(false));
    }

    /// A value outside the list still reads as what it is: a word, a time, a
    /// block member or a number.
    #[test]
    fn an_unlisted_operand_says_what_it_is() {
        assert_eq!(operand_kind("MW3"), "WORD");
        assert_eq!(operand_kind("%MW3"), "WORD");
        assert_eq!(operand_kind("T#250ms"), "TIME");
        assert_eq!(operand_kind("CTU0.CV"), "BLOCK");
        assert_eq!(operand_kind("TON1.ET"), "BLOCK");
        assert_eq!(operand_kind("-12"), "NUMBER");
        assert_eq!(operand_kind("12345"), "NUMBER");
    }

    /// The Enter that applies must not also reach the grid: it used to open a
    /// picker on the cell the window had just written.
    #[test]
    fn the_applying_frame_still_owns_its_enter() {
        let mut plc = PlcUi::default();
        compare(&mut plc);
        let _ = frame(&mut plc, key(egui::Key::Enter));
        let _ = frame(&mut plc, key(egui::Key::Enter));
        let (actions, owned) = frame_owned(&mut plc, key(egui::Key::Enter));
        assert!(!actions.is_empty(), "the cell was written");
        assert!(plc.compare_edit.is_none(), "the window closed");
        assert!(
            owned,
            "and this frame's Enter belongs to the window, not the grid"
        );
        let (_, owned) = frame_owned(&mut plc, vec![]);
        assert!(!owned, "the next frame is the grid's again");
    }

    fn key(key: egui::Key) -> Vec<egui::Event> {
        vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]
    }

    fn compare(plc: &mut PlcUi) {
        plc.compare_edit = Some(CompareEdit {
            section: 0,
            rung: 0,
            position: 1,
            op: "GT".to_string(),
            left: "MW0".to_string(),
            right: "0".to_string(),
            focus: 0,
            error: None,
        });
    }

    /// Enter walks the one window: condition, then A, then B, then writes the
    /// cell. The operator comes from the list, the operands from their fields.
    #[test]
    fn enter_walks_condition_a_b_and_applies() {
        let mut plc = PlcUi::default();
        compare(&mut plc);
        let _ = frame(&mut plc, key(egui::Key::Enter));
        assert_eq!(plc.compare_edit.as_ref().unwrap().focus, 1);
        let _ = frame(&mut plc, key(egui::Key::Enter));
        assert_eq!(plc.compare_edit.as_ref().unwrap().focus, 2);
        let actions = frame(&mut plc, key(egui::Key::Enter));
        assert!(plc.compare_edit.is_none(), "the window closed on apply");
        match &actions[0] {
            Action::SetCell { cell, .. } => match cell {
                Cell::Compare { op, left, right } => {
                    assert_eq!(
                        (op.as_str(), left.as_str(), right.as_str()),
                        ("GT", "MW0", "0")
                    );
                }
                other => panic!("expected a compare cell, got {other:?}"),
            },
            other => panic!("expected a cell write, got {other:?}"),
        }
    }

    /// The condition list is the operator picker: the arrows walk it without
    /// leaving the window, and the formula shown beside it comes from the table.
    #[test]
    fn the_arrows_walk_the_condition_list() {
        let mut plc = PlcUi::default();
        compare(&mut plc);
        let _ = frame(&mut plc, key(egui::Key::ArrowDown));
        assert_eq!(plc.compare_edit.as_ref().unwrap().op, "GE");
        let _ = frame(&mut plc, key(egui::Key::ArrowUp));
        let _ = frame(&mut plc, key(egui::Key::ArrowUp));
        let edit = plc.compare_edit.expect("still open");
        assert_eq!(
            edit.op, "GT",
            "up at the first condition does not wrap the list"
        );
        assert_eq!(edit.focus, 2, "it steps back into the field above it");
    }

    /// A refused operand keeps the window and says which field and why.
    #[test]
    fn a_bad_operand_is_refused_in_words() {
        let mut plc = PlcUi::default();
        compare(&mut plc);
        {
            let edit = plc.compare_edit.as_mut().unwrap();
            edit.focus = 1;
            edit.left = "MW99".into();
        }
        let actions = frame(&mut plc, key(egui::Key::Enter));
        assert!(actions.is_empty(), "nothing written");
        let edit = plc.compare_edit.as_ref().expect("window stays");
        assert_eq!(edit.focus, 1, "the keys stay in the bad field");
        assert_eq!(
            edit.error.as_deref(),
            Some("A MW99 is not a marker word: MW0 to MW15")
        );
    }

    /// The operand list is the keyboard's way to pick a word: down takes the next
    /// row into the field, no typing needed.
    #[test]
    fn the_operand_list_writes_the_highlighted_row_into_the_field() {
        let mut plc = PlcUi::default();
        compare(&mut plc);
        plc.compare_edit.as_mut().unwrap().focus = 1;
        let _ = frame(&mut plc, key(egui::Key::ArrowDown));
        assert_eq!(plc.compare_edit.as_ref().unwrap().left, "MW1");
        let _ = frame(&mut plc, key(egui::Key::ArrowDown));
        assert_eq!(plc.compare_edit.as_ref().unwrap().left, "MW2");
    }

    /// The choices are what the compiler accepts, plus the words the project
    /// actually publishes: the list carries MW0–MW15, a used timer's `.ET`, the
    /// counters' `.CV`, numbers and times.
    #[test]
    fn the_list_holds_the_words_and_the_literal_shapes() {
        let mut plc = PlcUi::default();
        let mut pou = linc_plc_core::model::Pou::new("main", linc_plc_core::model::PouLanguage::Ld);
        pou.sections = vec![linc_plc_core::model::Section {
            name: String::new(),
            rungs: vec![linc_plc_core::model::Rung {
                comment: String::new(),
                logic: linc_plc_core::model::Logic::Const { value: true },
                coil: linc_plc_core::model::Coil::new("", linc_plc_core::model::CoilKind::Normal),
                cells: vec![
                    Cell::Block {
                        block: "TON".into(),
                        instance: "TON0".into(),
                        preset: "T#1s".into(),
                    },
                    Cell::Block {
                        block: "CTU".into(),
                        instance: "CTU0".into(),
                        preset: "10".into(),
                    },
                ],
            }],
        }];
        plc.project.pous = vec![pou];
        let values: Vec<String> = compare_choices(&plc)
            .into_iter()
            .map(|choice| choice.value)
            .collect();
        for wanted in ["MW0", "MW15", "TON0.ET", "CTU0.CV", "25", "T#250ms"] {
            assert!(
                values.iter().any(|value| value == wanted),
                "{wanted} must be offered, list is {values:?}"
            );
        }
        assert!(
            values.iter().all(|value| value != "MW16"),
            "only the 16 words"
        );
    }
}

#[cfg(test)]
mod compute_window_tests {
    use super::*;

    fn frame(plc: &mut PlcUi, events: Vec<egui::Event>) -> Vec<Action> {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut actions = Vec::new();
        let mut output = ctx.run_pass(input, |ctx| {
            pickers(plc, ctx, &mut actions);
        });
        output.textures_delta.clear();
        actions
    }

    fn key(key: egui::Key) -> Vec<egui::Event> {
        vec![egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        }]
    }

    fn compute(plc: &mut PlcUi) {
        plc.compute_edit = Some(ComputeEdit {
            section: 0,
            rung: 0,
            position: 2,
            op: "ADD".to_string(),
            target: "MW1".to_string(),
            left: "MW0".to_string(),
            right: "1".to_string(),
            focus: 0,
            error: None,
        });
    }

    /// The window is the assignment: operation, destination, A, B, then write.
    #[test]
    fn enter_walks_operation_destination_a_b_and_applies() {
        let mut plc = PlcUi::default();
        compute(&mut plc);
        for focus in [1, 2, 3] {
            let _ = frame(&mut plc, key(egui::Key::Enter));
            assert_eq!(plc.compute_edit.as_ref().unwrap().focus, focus);
        }
        let actions = frame(&mut plc, key(egui::Key::Enter));
        assert!(plc.compute_edit.is_none(), "the window closed on apply");
        match &actions[0] {
            Action::SetCell { cell, .. } => match cell {
                Cell::Compute {
                    op,
                    target,
                    left,
                    right,
                } => assert_eq!(
                    (op.as_str(), target.as_str(), left.as_str(), right.as_str()),
                    ("ADD", "MW1", "MW0", "1")
                ),
                other => panic!("expected a compute cell, got {other:?}"),
            },
            other => panic!("expected a cell write, got {other:?}"),
        }
    }

    /// MOVE is `D := A`: the window offers no B for it, Enter skips straight to
    /// the write, and the cell still carries a parseable second operand.
    #[test]
    fn move_takes_only_the_destination_and_a() {
        let mut plc = PlcUi::default();
        compute(&mut plc);
        for _ in 0..4 {
            let _ = frame(&mut plc, key(egui::Key::ArrowDown));
        }
        assert_eq!(plc.compute_edit.as_ref().unwrap().op, "MOVE");
        let _ = frame(&mut plc, key(egui::Key::Enter));
        assert_eq!(plc.compute_edit.as_ref().unwrap().focus, 1);
        let _ = frame(&mut plc, key(egui::Key::Enter));
        assert_eq!(plc.compute_edit.as_ref().unwrap().focus, 2);
        let actions = frame(&mut plc, key(egui::Key::Enter));
        assert!(plc.compute_edit.is_none(), "MOVE applied after A");
        match &actions[0] {
            Action::SetCell { cell, .. } => match cell {
                Cell::Compute { op, right, .. } => {
                    assert_eq!(op, "MOVE");
                    assert_eq!(right, "0", "the compiler needs a second operand");
                }
                other => panic!("expected a compute cell, got {other:?}"),
            },
            other => panic!("expected a cell write, got {other:?}"),
        }
    }

    /// Only marker words can be written, and a refusal says so and stays put.
    #[test]
    fn the_destination_is_a_word_and_a_bad_one_is_refused() {
        let words: Vec<String> = destination_choices()
            .into_iter()
            .map(|choice| choice.value)
            .collect();
        assert_eq!(words.first().map(String::as_str), Some("MW0"));
        assert_eq!(words.last().map(String::as_str), Some("MW15"));
        assert!(
            !words.iter().any(|word| word == "25"),
            "literals cannot be written"
        );

        let mut plc = PlcUi::default();
        compute(&mut plc);
        {
            let edit = plc.compute_edit.as_mut().unwrap();
            edit.focus = 1;
            edit.target = "MW99".into();
        }
        let actions = frame(&mut plc, key(egui::Key::Enter));
        assert!(actions.is_empty());
        let edit = plc.compute_edit.as_ref().unwrap();
        assert_eq!(edit.focus, 1);
        assert_eq!(
            edit.error.as_deref(),
            Some("D MW99 is not a marker word: MW0 to MW15")
        );
    }

    /// The operands are the compare window's list: words, block values, numbers
    /// and times. The destination list is words only.
    #[test]
    fn the_operand_lists_are_shared_with_the_compare_window() {
        let mut plc = PlcUi::default();
        compute(&mut plc);
        plc.compute_edit.as_mut().unwrap().focus = 2;
        let _ = frame(&mut plc, key(egui::Key::ArrowDown));
        assert_eq!(plc.compute_edit.as_ref().unwrap().left, "MW1");
        let values: Vec<String> = compare_choices(&plc)
            .into_iter()
            .map(|choice| choice.value)
            .collect();
        assert!(values.iter().any(|value| value == "T#250ms"));
    }
}
