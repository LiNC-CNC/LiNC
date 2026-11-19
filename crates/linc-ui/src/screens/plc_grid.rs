//! The ladder grid: cells, cursor, element picker, operand list.
//!
//! The page keeps the state (`cell_cursor`, `picker`, `operand_pick`); this
//! module draws the grid and owns the keys while the editor overlay is open.
//! Element letters live inside the picker rather than on the page, so they can
//! never collide with a shell shortcut or a text field.

use egui::{Align2, Color32, FontId, RichText, Sense, Stroke, Ui, Vec2};
use linc_plc_core::grid::{Cell, POSITIONS};
use linc_plc_core::profile::spec::Shape;
use linc_plc_core::{Coil, CoilKind, PouLanguage, Rung};

use crate::screens::plc::{Action, CompareEdit, ComputeEdit, PlcPage, PlcUi, RAIL_GROUP_GAP};
use crate::shell::LincApp;
use crate::theme;

/// Keys the ladder editor owns. Consumed before any widget sees them.
/// The rung comment's own field, one line above the rung.
///
/// `Shift+C` opens it on the rung under the cursor (`C` alone is a contact), the
/// band shows the buffer with a caret, Enter stores it through
/// `Action::SetRungComment`, and Escape drops it. The band is drawn by
/// `grid_rung`, so the field is where the comment will end up rather than in a
/// dialog over the ladder.
///
/// Returns true while the field owns the keyboard, so the grid's keys stay out.
pub fn rung_text_keys(plc: &mut PlcUi, ui: &Ui, actions: &mut Vec<Action>) -> bool {
    // The label and the comment are typed the same way in the same frame, so they
    // share one handler: two copies drifted apart the moment one of them changed.
    // The pairs are `Copy`, so reading them here does not hold a borrow of `plc`.
    let active = match (plc.rung_label_edit, plc.rung_comment_edit) {
        (Some(range), _) => Some((RungField::Label, range)),
        (None, Some(range)) => Some((RungField::Comment, range)),
        (None, None) => None,
    };
    if let Some((field, (section, rung))) = active {
        let mut buffer = match field {
            RungField::Label => std::mem::take(&mut plc.rung_label_buffer),
            RungField::Comment => std::mem::take(&mut plc.rung_comment_buffer),
        };
        let store = |actions: &mut Vec<Action>, text: &str| match field {
            RungField::Label => actions.push(Action::SetRungLabel {
                section,
                rung,
                label: text.trim().to_string(),
            }),
            RungField::Comment => actions.push(Action::SetRungComment {
                section,
                rung,
                comment: text.trim().to_string(),
            }),
        };
        let mut entered = false;
        let mut leave = false;
        let mut step: Option<isize> = None;
        for event in ui.input(|input| input.events.clone()) {
            match event {
                egui::Event::Text(text) => buffer.push_str(&text),
                egui::Event::Key {
                    key, pressed: true, ..
                } => match key {
                    egui::Key::Backspace => {
                        buffer.pop();
                    }
                    egui::Key::Escape => {
                        // The one key that throws the typing away.
                        buffer.clear();
                        leave = true;
                    }
                    egui::Key::Enter => {
                        entered = true;
                        leave = true;
                    }
                    egui::Key::ArrowLeft => {
                        // Out of the field and back onto the rung's last cell.
                        entered = true;
                        leave = true;
                    }
                    // Up and down walk the comment column: the field stores what it
                    // holds and opens the comment of the row above or below.
                    egui::Key::ArrowUp if field == RungField::Comment => {
                        entered = true;
                        step = Some(-1);
                    }
                    egui::Key::ArrowDown if field == RungField::Comment => {
                        entered = true;
                        step = Some(1);
                    }
                    _ => {}
                },
                _ => {}
            }
        }
        if entered {
            store(actions, &buffer);
        }
        if leave {
            if field == RungField::Label {
                plc.rung_label_edit = None;
            } else {
                plc.rung_comment_edit = None;
                // The cell cursor is where the field was entered from and where it
                // goes back to: the rung's last cell, never the comment itself.
                plc.cell_cursor = Some((section, rung, POSITIONS));
            }
            plc.text_editing = false;
            return true;
        }
        if let Some(step) = step {
            if let Some(target) = neighbour_rung(plc, section, rung, step) {
                let row = plc
                    .current_pou_sections()
                    .get(section)
                    .and_then(|section| section.rungs.get(target))
                    .cloned();
                open_rung_comment_into(plc, section, target, row.as_ref());
                plc.cell_cursor = Some((section, target, POSITIONS));
                plc.text_editing = true;
                return true;
            }
        }
        match field {
            RungField::Label => plc.rung_label_buffer = buffer,
            RungField::Comment => plc.rung_comment_buffer = buffer,
        }
        return true;
    }
    let (label, comment) = ui.input_mut(|input| {
        (
            input.consume_key(egui::Modifiers::SHIFT, egui::Key::L),
            input.consume_key(egui::Modifiers::SHIFT, egui::Key::C),
        )
    });
    if !label && !comment {
        return false;
    }
    let cursor = plc.cell_cursor.unwrap_or((0, 0, 0));
    let rung = plc
        .current_pou_sections()
        .get(cursor.0)
        .and_then(|section| section.rungs.get(cursor.1))
        .cloned();
    if label {
        plc.rung_label_edit = Some((cursor.0, cursor.1));
        plc.rung_label_buffer = rung
            .map(|rung| rung.label.trim().to_string())
            .unwrap_or_default();
    } else {
        open_rung_comment_into(plc, cursor.0, cursor.1, rung.as_ref());
    }
    plc.text_editing = true;
    true
}

/// The next row in the comment column, in the same section. A comment row is the
/// comment itself and has no field of its own, so it is stepped over.
fn neighbour_rung(plc: &PlcUi, section: usize, rung: usize, step: isize) -> Option<usize> {
    let sections = plc.current_pou_sections();
    let rungs = &sections.get(section)?.rungs;
    let mut index = rung as isize;
    loop {
        index += step;
        if index < 0 || index as usize >= rungs.len() {
            return None;
        }
        if !rungs[index as usize].is_comment_row() {
            return Some(index as usize);
        }
    }
}

/// Open the comment of the rung at `(section, rung)` for typing, filled from the rung.
fn open_rung_comment(app: &mut LincApp, section: usize, rung: usize) {
    let row = app
        .plc
        .current_pou_sections()
        .get(section)
        .and_then(|section| section.rungs.get(rung))
        .cloned();
    open_rung_comment_into(&mut app.plc, section, rung, row.as_ref());
    app.plc.text_editing = true;
}

/// The same, on the state alone: Shift+C already holds the rung it read.
fn open_rung_comment_into(plc: &mut PlcUi, section: usize, rung: usize, row: Option<&Rung>) {
    plc.rung_comment_edit = Some((section, rung));
    plc.rung_comment_buffer = row
        .map(|rung| {
            rung.comment
                .trim()
                .trim_start_matches("//")
                .trim()
                .to_string()
        })
        .unwrap_or_default();
}

/// Which of a rung's two inline texts a keystroke is going into.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RungField {
    Label,
    Comment,
}

pub fn editor_keys(app: &mut LincApp, ui: &Ui, actions: &mut Vec<Action>) {
    if matches!(app.plc.page, PlcPage::Trend | PlcPage::Watch) {
        // Z cycles the window, as the manual's trend page does; the daemon
        // restarts the history when the window changes. While a watch row is
        // being typed into, Z is a letter for the field, not a window.
        let typing = app.plc.watch_edit.is_some() || app.plc.watch_value_edit.is_some();
        if !typing {
            let zoom = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Z));
            if zoom {
                app.plc.trend_window_ms =
                    crate::screens::plc::next_trend_window(app.plc.trend_window_ms);
                app.plc.trend_remote.samples.clear();
            }
        }
        return;
    }
    // The rung's comment is typed into the band above it, with the field's own
    // keys: it must run before the blanket Escape rule, or Escape would drop
    // `text_editing` and leave the buffer behind.
    if rung_text_keys(&mut app.plc, ui, actions) {
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
        || app.plc.coil_kind_pick.is_some()
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
        // egui-winit reports Ctrl as CTRL|COMMAND on Linux, so both spellings
        // must be consumed.
        let chord = |input: &mut egui::InputState, key: egui::Key| {
            input.consume_key(egui::Modifiers::CTRL, key)
                || input.consume_key(egui::Modifiers::CTRL | egui::Modifiers::COMMAND, key)
        };
        (
            chord(input, egui::Key::C),
            chord(input, egui::Key::V),
            chord(input, egui::Key::X) || cut,
            chord(input, egui::Key::W),
        )
    });
    // Cross references and the watch pin belong to the cell under the cursor,
    // coil slot included: the coil branch returns before the element handling,
    // so they must be read here.
    if xref {
        app.plc.xref_scroll = 0;
        let operand = sections
            .get(cursor.0)
            .and_then(|section| section.rungs.get(cursor.1))
            .map(|rung| {
                if cursor.2 >= POSITIONS {
                    rung.coil().var.clone()
                } else {
                    rung.cells
                        .get(cursor.2)
                        .and_then(|cell| cell.operand())
                        .unwrap_or("")
                        .to_string()
                }
            })
            .filter(|operand| !operand.is_empty());
        app.plc.xref = operand;
    }
    if watch {
        let operand = sections
            .get(cursor.0)
            .and_then(|section| section.rungs.get(cursor.1))
            .map(|rung| {
                if cursor.2 >= POSITIONS {
                    rung.coil().var.clone()
                } else {
                    rung.cells
                        .get(cursor.2)
                        .and_then(|cell| cell.operand())
                        .unwrap_or("")
                        .to_string()
                }
            })
            .filter(|operand| !operand.is_empty());
        if let Some(operand) = operand {
            actions.push(Action::ToggleWatch(operand));
        }
    }
    // Element letters. They live on the page now that the editor overlay owns
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
        jump_coil,
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
            letter(input, egui::Key::J),
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
    // Space is the contact's hold key and the coil's kind key. Reading it here for
    // the whole row consumed it before the coil branch could cycle the kind, so a
    // coil had no way to change type at all.
    let space = cursor.2 < POSITIONS
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Space));
    if space {
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
                rung.coil().var.trim().is_empty()
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
    // Right on the coil slot steps out of the cells and into the rung's comment: the
    // comment is drawn in the right margin, and that is where the cursor goes. The
    // cell cursor stays on the coil, so Left (or Escape) comes back to it.
    if right && cursor.2 >= POSITIONS {
        open_rung_comment(app, cursor.0, cursor.1);
        app.plc.cell_cursor = Some(cursor);
        return;
    }
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
        // Space cycles the coil's kind: Normal, Set, Reset, Negated, Jump,
        // Call. The coil letters and their rail group are gone; one cell key
        // does the job.
        let space =
            ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Space));
        if space {
            if let Some(rung) = sections
                .get(cursor.0)
                .and_then(|section| section.rungs.get(cursor.1))
            {
                let next = match rung.coil().kind {
                    CoilKind::Normal => CoilKind::Set,
                    CoilKind::Set => CoilKind::Reset,
                    CoilKind::Reset => CoilKind::Negated,
                    CoilKind::Negated => CoilKind::Jump,
                    CoilKind::Jump => CoilKind::Call,
                    CoilKind::Call => CoilKind::Compute,
                    CoilKind::Compute => CoilKind::Normal,
                };
                actions.push(Action::SetCoil {
                    section: cursor.0,
                    rung: cursor.1,
                    coil: Coil::new(rung.coil().var.clone(), next),
                });
            }
        }
        // Backspace on the coil slot removes the coil: the branch returns before the
        // general delete below, so the coil's own key lives here.
        if delete {
            actions.push(Action::DeleteCell {
                section: cursor.0,
                rung: cursor.1,
                position: cursor.2,
            });
            return;
        }
        // Enter on the coil edits its operand. This branch returns before the
        // general Enter handling, so the coil's Enter lives here. An empty tail has
        // no coil to name yet, so Enter asks for the kind first.
        if enter {
            open_coil_enter(app, cursor.0, cursor.1);
        }
        let kind = if contact {
            Some(CoilKind::Normal)
        } else if negated {
            Some(CoilKind::Negated)
        } else if set_coil {
            Some(CoilKind::Set)
        } else if reset_coil {
            Some(CoilKind::Reset)
        } else if jump_coil {
            Some(CoilKind::Jump)
        } else {
            None
        };
        if let Some(kind) = kind {
            let var = sections
                .get(cursor.0)
                .and_then(|section| section.rungs.get(cursor.1))
                .map(|rung| rung.coil().var.clone())
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
    // Cross references and the watch pin belong to the cell under the cursor,
    // coil slot included: the coil branch returns before the element handling,
    // so they must be read here.
    if xref {
        app.plc.xref_scroll = 0;
        let operand = sections
            .get(cursor.0)
            .and_then(|section| section.rungs.get(cursor.1))
            .map(|rung| {
                if cursor.2 >= POSITIONS {
                    rung.coil().var.clone()
                } else {
                    rung.cells
                        .get(cursor.2)
                        .and_then(|cell| cell.operand())
                        .unwrap_or("")
                        .to_string()
                }
            })
            .filter(|operand| !operand.is_empty());
        app.plc.xref = operand;
    }
    if watch {
        let operand = sections
            .get(cursor.0)
            .and_then(|section| section.rungs.get(cursor.1))
            .map(|rung| {
                if cursor.2 >= POSITIONS {
                    rung.coil().var.clone()
                } else {
                    rung.cells
                        .get(cursor.2)
                        .and_then(|cell| cell.operand())
                        .unwrap_or("")
                        .to_string()
                }
            })
            .filter(|operand| !operand.is_empty());
        if let Some(operand) = operand {
            actions.push(Action::ToggleWatch(operand));
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
            EnterTarget::Coil((section, rung, _)) => open_coil_enter(app, section, rung),
            EnterTarget::Picker => {
                app.plc.picker = Some(cursor);
                app.plc.picker_cursor = 0;
            }
        }
    }
    if delete {
        // Delete closes the row up to the left; a wire in place would leave a gap.
        // On the coil slot it removes the coil instead, and the rung keeps its
        // cells: the slot becomes a plain conductor again.
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
    let kind_drawn = coil_kind_picker(plc, ctx, actions);
    // The kind window opens the operand list on the same frame it is answered, so
    // the operand list owns the next one.
    if kind_drawn {
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
    if picker_is_stale(plc.coil_kind_pick.is_some(), kind_drawn) {
        plc.coil_kind_pick = None;
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
        || plc.coil_kind_pick.is_some()
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
    let mut lines: Vec<String> = Vec::new();
    for pou in plc.project.all_pous() {
        for section in &pou.sections {
            for (rung_index, rung) in section.rungs.iter().enumerate() {
                for (position, cell) in rung.cells.iter().enumerate() {
                    if cell
                        .operand()
                        .map(|operand| operand.eq_ignore_ascii_case(&name))
                        .unwrap_or(false)
                    {
                        lines.push(format!(
                            "{}  {}  rung {}  position {}",
                            pou.name,
                            section.name,
                            rung_index + 1,
                            position
                        ));
                    }
                }
                if rung.coil().var.eq_ignore_ascii_case(&name) {
                    lines.push(format!(
                        "{}  {}  rung {}  coil",
                        pou.name,
                        section.name,
                        rung_index + 1
                    ));
                }
            }
        }
    }
    if lines.is_empty() {
        lines.push("no references".to_string());
    }
    // The window owns the arrows: the list scrolls, the grid behind stays put.
    const ROWS: usize = 10;
    let keys = modal_keys(ctx);
    let mut delta = 0isize;
    if keys.up {
        delta -= 1;
    }
    if keys.down {
        delta += 1;
    }
    // These are the window's keys now: consuming them keeps the grid's cell
    // cursor where it was while the list scrolls.
    let page = ctx.input_mut(|input| {
        let mut page = 0isize;
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
            page -= 1;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
            page += 1;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
            page -= ROWS as isize;
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
            page += ROWS as isize;
        }
        page
    });
    let delta = if page != 0 { page } else { delta };
    let max_offset = lines.len().saturating_sub(ROWS);
    plc.xref_scroll = (plc.xref_scroll as isize + delta).clamp(0, max_offset as isize) as usize;
    let offset = plc.xref_scroll;
    let mut close = false;
    egui::Window::new(format!("CROSS REFERENCES  {name}"))
        // Above the workbench overlay (Foreground), which is itself above the page.
        .order(egui::Order::Tooltip)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.set_min_width(380.0);
            for line in lines.iter().skip(offset).take(ROWS) {
                ui.label(
                    RichText::new(line)
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::FG),
                );
            }
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "↑ ↓ SCROLL · ENTER OR ESC CLOSES · {} REFERENCE(S)",
                    lines.len()
                ))
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
            );
            if theme::action_button(ui, "Close", true).clicked() || keys.enter || keys.escape {
                close = true;
            }
        });
    if close {
        // The window took this Enter or Escape; leaving it in the frame let the
        // ladder act on the same key and open the element list behind the
        // closed cross references.
        ctx.input_mut(|input| {
            input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
            input.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp);
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown);
            input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp);
            input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown);
        });
        plc.xref = None;
        plc.xref_scroll = 0;
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

/// The element a letter stands for.
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
    plc.coil_kind_pick = None;
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
        face: Face<'static>,
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
                    .map(|rung| rung.coil().var.clone())
            })
            .filter(|var| !var.is_empty())
            .unwrap_or_else(|| first.clone());
        for (label, kind) in [
            ("Coil", CoilKind::Normal),
            ("Set coil", CoilKind::Set),
            ("Reset coil", CoilKind::Reset),
            ("Jump coil", CoilKind::Jump),
            // `C` already sets a normal coil at this cursor, so a call has no
            // direct key; the list and the Space cycle reach it.
            ("Call coil", CoilKind::Call),
        ] {
            let chord = match kind {
                CoilKind::Normal => "C",
                CoilKind::Set => "S",
                CoilKind::Reset => "R",
                CoilKind::Negated => "N",
                CoilKind::Jump => "J",
                CoilKind::Call => "",
                CoilKind::Compute => "",
            };
            items.push(Item {
                group: "COIL",
                face: Face::coil(kind),
                label,
                chord,
                act: Act::Coil(kind),
            });
        }
        let _ = var;
    } else {
        for (face, label, chord, cell) in [
            (
                Face {
                    shape: Shape::Contact,
                    glyph: "",
                    rows: &[],
                    ..Face::default()
                },
                "Wire",
                "-",
                Cell::Wire,
            ),
            (
                Face {
                    shape: Shape::Contact,
                    glyph: "",
                    rows: &[],
                    ..Face::default()
                },
                "Contact",
                "C",
                Cell::Contact {
                    var: first.clone(),
                    negated: false,
                },
            ),
            (
                Face {
                    shape: Shape::ContactNc,
                    glyph: "",
                    rows: &[],
                    ..Face::default()
                },
                "Negated contact",
                "X",
                Cell::Contact {
                    var: first.clone(),
                    negated: true,
                },
            ),
            (
                Face {
                    shape: Shape::Contact,
                    glyph: "",
                    rows: &[],
                    ..Face::default()
                },
                "Link",
                "V",
                Cell::Link,
            ),
        ] {
            items.push(Item {
                group: "CONTACTS",
                face,
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
            let cell = Cell::Block {
                block: block.to_string(),
                instance: instance.to_string(),
                preset: preset.to_string(),
            };
            items.push(Item {
                group: "BOXES",
                face: Face::from_cell(&cell),
                label,
                chord,
                act: Act::Cell(cell),
            });
        }
        items.push(Item {
            group: "EXPRESSIONS",
            face: Face {
                shape: Shape::Small,
                glyph: "",
                rows: &[],
                ..Face::default()
            },
            label: "Compare",
            chord: "G",
            act: Act::Compare,
        });
        items.push(Item {
            group: "EXPRESSIONS",
            face: Face {
                shape: Shape::Small,
                glyph: ":=",
                rows: &[],
                ..Face::default()
            },
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
                let ink = if selected { theme::FG } else { theme::MUTED };
                draw_face(
                    &painter,
                    symbol_rect,
                    &item.face,
                    Style::new(ink, theme::C4, State::Off),
                    CELL_HEIGHT,
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
                    .map(|rung| rung.coil().var.clone())
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
            .map(|rung| rung.coil().var.clone())
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
    let names = operand_choices(plc);
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

/// Every instance the project names, with its instruction-table mnemonic.
fn block_instances(plc: &PlcUi) -> Vec<(String, &'static str)> {
    let mut used: Vec<(String, &'static str)> = Vec::new();
    for pou in plc.project.all_pous() {
        for section in &pou.sections {
            for rung in &section.rungs {
                for cell in &rung.cells {
                    let Cell::Block {
                        block, instance, ..
                    } = cell
                    else {
                        continue;
                    };
                    let Some(spec) = linc_plc_core::profile::spec::spec(block) else {
                        continue;
                    };
                    if !used.iter().any(|(name, _)| name == instance) {
                        used.push((instance.clone(), spec.mnemonic));
                    }
                }
            }
        }
    }
    used
}

/// The names a contact may read: the project's symbols, then each block's
/// members by instance, so `TON0` narrows to its own two bits. A timer has `Q`
/// and `TT`; a counter has `Q`; a word member is not listed, because a contact
/// reads a bit and the validator would refuse it — the compare window is where
/// `CV` and `ET` are read.
fn operand_choices(plc: &PlcUi) -> Vec<(String, String)> {
    let mut names = plc
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
    for (instance, mnemonic) in block_instances(plc) {
        names.push((format!("{instance}.Q"), format!("{mnemonic} Q")));
        if matches!(mnemonic, "TON" | "TOF" | "TP") {
            names.push((format!("{instance}.TT"), format!("{mnemonic} TT")));
        }
    }
    names
}

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
fn coil_position(plc: &PlcUi, section: usize, rung: usize, name: &str) -> usize {
    coil_choices(plc, section, rung)
        .iter()
        .position(|(candidate, _)| candidate == name)
        .unwrap_or(0)
}

fn coil_choices(plc: &PlcUi, section: usize, rung: usize) -> Vec<(String, String)> {
    // A jump names a label in its own section and a call names a POU; only a
    // bit coil names a writable symbol. Offering bit variables for a jump would
    // hand the operator a target the validator refuses.
    let kind = plc
        .current_pou_sections()
        .get(section)
        .and_then(|section| section.rungs.get(rung))
        .map(|rung| rung.coil().kind);
    match kind {
        Some(CoilKind::Jump) => plc
            .current_pou_sections()
            .get(section)
            .map(|section| {
                section
                    .rungs
                    .iter()
                    .map(|rung| rung.label.trim())
                    .filter(|label| !label.is_empty())
                    .map(|label| (label.to_string(), "LABEL".to_string()))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        Some(CoilKind::Call) => {
            // Calling the POU that is being edited is a cycle, so it is not
            // offered; every other POU is a legal target.
            let current = plc.current_pou().map(|pou| pou.name).unwrap_or_default();
            plc.project
                .all_pous()
                .into_iter()
                .filter(|pou| !pou.name.eq_ignore_ascii_case(&current))
                .map(|pou| (pou.name, "POU".to_string()))
                .collect()
        }
        _ => plc
            .project
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
            // A counter's reset pin is written by a member coil — the corpus
            // pattern is `reset_count → (R) CTU0.R` — so it is a coil target.
            .chain(
                block_instances(plc)
                    .into_iter()
                    .filter(|(_, mnemonic)| matches!(*mnemonic, "CTU" | "CTD"))
                    .map(|(instance, mnemonic)| (format!("{instance}.R"), format!("{mnemonic} R"))),
            )
            .collect(),
    }
}

/// What a coil list with nothing to offer says: what is missing for this kind, and
/// the chord that makes one. A bare empty list reads as a broken window.
fn no_targets_hint(kind: CoilKind) -> (&'static str, &'static str) {
    match kind {
        CoilKind::Jump => ("no labels in this section", "Shift+L on a rung types one"),
        CoilKind::Call => ("no other POUs in this project", "Ctrl+N adds one"),
        _ => ("no writable symbols", "Ctrl+I, Ctrl+O and Ctrl+B add one"),
    }
}

/// Every kind a coil may have, in the order the window lists them: the plain coil,
/// the negated one, and the latching pair. Jump and Call are offered because their
/// targets are things the operand list already holds (a rung label, another POU);
/// Compute keeps its own window, because what it writes is an expression rather
/// than one symbol.
const COIL_KINDS: [(CoilKind, &str, &str); 6] = [
    (CoilKind::Normal, "-[ ]-", "COIL"),
    (CoilKind::Negated, "-[/]-", "NEGATED COIL"),
    (CoilKind::Set, "-[S]-", "SET, LATCHING"),
    (CoilKind::Reset, "-[R]-", "RESET, UNLATCHING"),
    (CoilKind::Jump, "-[J]-", "JUMP"),
    (CoilKind::Call, "-[C]-", "CALL"),
];

/// The first step of putting a coil on an empty tail: which kind it is. Enter takes
/// the highlighted kind and immediately opens the operand list, which is the second
/// step; Escape leaves the rung untouched.
fn coil_kind_picker(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) -> bool {
    let Some((section, rung)) = plc.coil_kind_pick else {
        return false;
    };
    let keys = modal_keys(ctx);
    if keys.escape {
        plc.coil_kind_pick = None;
        return false;
    }
    plc.coil_kind_cursor = plc.coil_kind_cursor.min(COIL_KINDS.len() - 1);
    if keys.up {
        plc.coil_kind_cursor = plc.coil_kind_cursor.saturating_sub(1);
    }
    if keys.down {
        plc.coil_kind_cursor = (plc.coil_kind_cursor + 1).min(COIL_KINDS.len() - 1);
    }
    let mut chosen: Option<CoilKind> = None;
    let mut visible = true;
    let shown = egui::Window::new("COIL TYPE")
        .open(&mut visible)
        // Above the workbench overlay, like the element and operand lists.
        .order(egui::Order::Tooltip)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_min_width(340.0);
            ui.set_max_width(340.0);
            ui.label(
                RichText::new("what the coil does with the rung's power")
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            ui.add_space(4.0);
            for (index, (kind, glyph, label)) in COIL_KINDS.iter().enumerate() {
                let selected = index == plc.coil_kind_cursor;
                if window_row(ui, glyph, label, selected, true, index % 2 == 1).clicked() {
                    chosen = Some(*kind);
                }
            }
            if keys.enter {
                chosen = COIL_KINDS.get(plc.coil_kind_cursor).map(|row| row.0);
            }
            ui.add_space(6.0);
            ui.label(
                RichText::new("↑ ↓ KIND · ENTER TAKES IT, THEN THE OPERAND · ESC")
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
    if let Some(kind) = chosen {
        actions.push(Action::SetCoil {
            section,
            rung,
            coil: Coil::new(String::new(), kind),
        });
        // Second step: the operand of the coil just chosen.
        plc.operand_cursor = 0;
        plc.operand_filter.clear();
        plc.coil_pick = Some((section, rung));
        plc.coil_kind_pick = None;
    }
    if !visible {
        plc.coil_kind_pick = None;
    }
    shown.is_some()
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
        .map(|rung| rung.coil().var.clone())
        .unwrap_or_default();
    // The stale value the rung still holds is listed so the window shows what is
    // stored, but it is not a target: the empty-state hint asks the real list, or a
    // jump with no labels would look like a jump with one.
    let candidates = coil_choices(plc, section, rung);
    let no_candidates = candidates.is_empty();
    let mut names = candidates;
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
            if matches.is_empty() && !no_candidates {
                ui.add_space(4.0);
                ui.label(
                    RichText::new("no target matches")
                        .monospace()
                        .size(theme::font_size(10.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("backspace clears the filter")
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::C3),
                );
            }
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
            // Nothing this kind can name: say what is missing and how to make one. The
            // stored value is still listed above, so the window says what the rung
            // holds *and* why it cannot be re-pointed yet.
            if no_candidates {
                let kind = plc
                    .current_pou_sections()
                    .get(section)
                    .and_then(|section| section.rungs.get(rung))
                    .map(|rung| rung.coil().kind)
                    .unwrap_or(CoilKind::Normal);
                let (what, how) = no_targets_hint(kind);
                ui.add_space(4.0);
                ui.label(
                    RichText::new(what)
                        .monospace()
                        .size(theme::font_size(10.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new(how)
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::C3),
                );
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

/// Enter on a coil slot. A coil that is already named edits its operand; an empty
/// tail has no coil yet, so the kind comes first and the operand list follows it —
/// the type is what decides which targets the list may offer.
fn open_coil_enter(app: &mut LincApp, section: usize, rung: usize) {
    let sections = app.plc.current_pou_sections();
    let row = sections
        .get(section)
        .and_then(|section| section.rungs.get(rung));
    let named = row
        .map(|rung| rung.has_coil() && rung.names_a_target())
        .unwrap_or(false);
    if named {
        let var = row.map(|rung| rung.coil().var.clone()).unwrap_or_default();
        app.plc.operand_cursor = coil_position(&app.plc, section, rung, &var);
        app.plc.operand_filter.clear();
        app.plc.coil_pick = Some((section, rung));
    } else {
        app.plc.coil_kind_pick = Some((section, rung));
        app.plc.coil_kind_cursor = 0;
    }
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

/// The six comparison conditions, in operator order,
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
    let mut choices: Vec<Choice> = linc_plc_core::profile::resolve::marker_words()
        .map(|(_, text)| Choice::new(text, "WORD"))
        .collect();
    for (instance, mnemonic) in block_instances(plc) {
        // The word that reads the block's value: `ET` for a timer, `CV` for a
        // counter. A block with neither is not a compare operand.
        let member = match mnemonic {
            "TON" | "TOF" | "TP" => "ET",
            "CTU" | "CTD" => "CV",
            _ => continue,
        };
        choices.push(Choice::new(format!("{instance}.{member}"), "BLOCK"));
    }
    for value in ["0", "1", "5", "10", "25", "100", "1000"] {
        choices.push(Choice::new(value, "NUMBER"));
    }
    for value in ["T#100ms", "T#250ms", "T#1s", "T#10s", "T#1m"] {
        choices.push(Choice::new(value, "TIME"));
    }
    choices
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
    if linc_plc_core::profile::resolve::resolve(value, &Default::default()).is_ok() {
        return None;
    }
    let upper = value.to_ascii_uppercase();
    Some(
        if linc_plc_core::profile::resolve::looks_like_marker_word(value) {
            format!(
                "{value} is not a marker word: {}",
                linc_plc_core::profile::resolve::marker_range()
            )
        } else if upper.starts_with("T#") {
            format!("{value} needs a unit: T#250ms, T#2s or T#1m")
        } else if value.starts_with('-') || value.starts_with(|c: char| c.is_ascii_digit()) {
            format!("{value} is not a whole number inside -32768 to 32767")
        } else {
            format!("{value} is not an operand: a word, a number, a time or a block value")
        },
    )
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
    linc_plc_core::profile::resolve::marker_words()
        .map(|(_, text)| Choice::new(text, "WORD"))
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
fn preview_scan(
    plc: &PlcUi,
    cells: &[Cell],
    node: Option<bool>,
) -> (Vec<Option<bool>>, Option<bool>) {
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
            // The preview evaluates contacts and compares from the frame's
            // snapshot. An expression needs the compiler's stack, so the cell
            // stays neutral and the preview power carries on unchanged rather
            // than guessing which way it would go.
            Cell::Expr { .. } => None,
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
    // A row in a linked group is fed by the group's node, not by its own cells
    // alone: the junction takes power from every joined row. The top row ANDs its
    // continuation after the junction; a branch row's whole row is an entry, so
    // the node is its output.
    let mut output = power;
    if let Some(node) = node {
        output = if let Some(link) = cells.iter().position(|cell| matches!(cell, Cell::Link)) {
            node && series_power(plc, &cells[link + 1..])
        } else {
            node
        };
    }
    (local, Some(output))
}

/// The node each row of a linked group sees, in row order.
///
/// A link joins the row below at its column, and that junction is shared: every
/// joined row feeds it from the left and every joined row's continuation is fed
/// by it. The node is therefore `OR` of the top row's cells before the junction
/// and each branch row's whole row — the same value `compile::lower_grid_rung`
/// builds, so the picture and the program agree.
///
/// Rows outside a group get `None`, and so does a link with no row under it.
pub fn group_nodes(plc: &PlcUi, rungs: &[Rung]) -> Vec<Option<bool>> {
    let mut nodes = vec![None; rungs.len()];
    let mut index = 0;
    while index < rungs.len() {
        let cells = &rungs[index].cells;
        let links = cells
            .iter()
            .filter(|cell| matches!(cell, Cell::Link))
            .count();
        if links == 0 || index + links >= rungs.len() {
            index += 1;
            continue;
        }
        let junction = cells
            .iter()
            .position(|cell| matches!(cell, Cell::Link))
            .unwrap_or(0);
        let mut node = series_power(plc, &cells[..junction]);
        for step in 1..=links {
            node = node || series_power(plc, &rungs[index + step].cells);
        }
        for offset in 0..=links {
            nodes[index + offset] = Some(node);
        }
        index += 1 + links;
    }
    nodes
}

/// A rung's comment as it reads in the margin: plain text, with the `//` marker
/// some projects keep in the stored string stripped. The marker is a storage
/// detail of the importer's comment rows, not part of what the operator wrote.
pub fn comment_plain(comment: &str) -> String {
    comment.trim().trim_start_matches("//").trim().to_string()
}

/// True when this rung's inline text (its label or its comment) owns the keyboard. A
/// focused field and a selected cell are two different things: while the comment is
/// being typed, no cell of that rung is selected.
pub(crate) fn rung_text_focused(plc: &PlcUi, section: usize, rung: usize) -> bool {
    plc.rung_comment_edit == Some((section, rung)) || plc.rung_label_edit == Some((section, rung))
}

/// True when this cell is the one the keyboard is on. The coil slot stops counting as
/// selected once the rung's comment takes the focus.
fn cell_selected(plc: &PlcUi, section: usize, rung: usize, position: usize) -> bool {
    plc.cell_cursor == Some((section, rung, position)) && !rung_text_focused(plc, section, rung)
}

/// One row's series power, for the link join. Links inside the segment read as
/// closed (they are the join points, not conditions).
fn series_power(plc: &PlcUi, cells: &[Cell]) -> bool {
    cells.iter().all(|cell| match cell {
        Cell::Contact { .. } | Cell::Edge { .. } | Cell::Compare { .. } => {
            cell_active(plc, cell).unwrap_or(false)
        }
        _ => true,
    })
}

/// One grid rung: nine element positions, the rail, and the coil.
pub fn grid_rung(
    plc: &PlcUi,
    ui: &mut Ui,
    section: usize,
    rung_index: usize,
    rung: &Rung,
    below: Option<&Rung>,
    node: Option<bool>,
    actions: &mut Vec<Action>,
    cursor_rect: &mut Option<egui::Rect>,
) -> egui::Rect {
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
    let local = previewing.then(|| preview_scan(plc, &cells, node));
    // The rung's own output from the snapshot, for the wires and for a coil whose
    // target the snapshot does not name.
    let power = rung_power(plc, &cells);
    let delivered = local.as_ref().map(|(_, coil)| *coil).unwrap_or(power);
    // A link drops to the next row's conductor, which is `CONDUCTOR_Y` above that
    // row's bottom edge; a row that grew for a block body or a comment band moves
    // its conductor down with it, so the drop follows the row it joins.
    let below_height = below.map(strip_height_of).unwrap_or(CELL_HEIGHT);
    // The comment sits on a line of its own above the operand names, so the rung
    // grows by one band and the names move down into the next one.
    // The rung whose label is being typed: the band shows the buffer and a caret in
    // place of the stored text, and the band exists while it is typed even when the
    // stored label is still empty.
    let label_editing = plc.rung_label_edit == Some((section, rung_index));
    let label = if label_editing {
        format!("{}▌", plc.rung_label_buffer)
    } else {
        rung.label.trim().to_string()
    };
    // The rung whose comment is being typed: the margin shows the buffer and a
    // caret in place of the stored text.
    let editing = plc.rung_comment_edit == Some((section, rung_index));
    // Only a label takes the upper band: the comment moved to the right margin,
    // so a commented rung keeps the same height as a plain one.
    let banded = !label.is_empty();
    // A row holding a block grows to make room for its label band; a symbol-only
    // row keeps `CELL_HEIGHT`.
    let rows = cells
        .iter()
        .filter_map(|cell| match cell {
            Cell::Block { block, .. } => {
                linc_plc_core::profile::spec::spec(block).map(|spec| spec.pins.0.len().max(1))
            }
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let strip_height = row_height(rows) + if banded { LABEL_ROW_BAND } else { 0.0 };
    let name_y = if banded { BANDED_NAME_Y } else { LABEL_Y };
    let strip = ui.horizontal(|ui| {
        // Zero spacing: neighbouring cells share one conductor with no joint.
        ui.spacing_mut().item_spacing.x = 0.0;
        let mut spans_right = false;
        for position in 0..POSITIONS {
            if spans_right {
                // The block already drew this position: it is the block's value
                // column, not a cell of its own.
                spans_right = false;
                continue;
            }
            let cell = cells.get(position).cloned().unwrap_or(Cell::Wire);
            let label = grid_cell_label(plc, &cell);
            let live = live_at(plc, &local, power, &cell);
            // A block's rows: pin names from the instruction table, values
            // formatted here, because the renderer positions text and never
            // formats a number.
            let (glyph, pins_in, pins_out) = match &cell {
                Cell::Block { block, .. } => match linc_plc_core::profile::spec::spec(block) {
                    Some(spec) => (spec.mnemonic, spec.pins.0, spec.pins.1),
                    None => ("", &[][..], &[][..]),
                },
                _ => ("", &[][..], &[][..]),
            };
            // A block's pins light one at a time, each from the member bit the
            // scan publishes under that pin's own name: `CTU0.CU` lights the `CU`
            // label, `TON0.Q` the `Q`. A pin the snapshot does not name — a word
            // pin, or a pin the scan does not keep — stays dark.
            let (pin_rows, q, in_power) = match &cell {
                Cell::Block {
                    instance, preset, ..
                } if !pins_in.is_empty() => block_state(plc, instance, preset, pins_in, pins_out),
                _ => (Vec::new(), false, false),
            };
            let rows_slice: Vec<Row<'_>> = pin_rows
                .iter()
                .map(|row| Row {
                    ins: row.ins,
                    out: row.out,
                    value_in: row.value_in.as_deref().filter(|value| !value.is_empty()),
                    value_out: row.value_out.as_deref().filter(|value| !value.is_empty()),
                    ins_lit: row.ins_lit,
                    out_lit: row.out_lit,
                })
                .collect();
            let face = if rows_slice.is_empty() {
                Face::from_cell(&cell)
            } else {
                Face {
                    shape: Shape::Block,
                    glyph,
                    rows: &rows_slice,
                    q,
                    in_power,
                }
            };
            let wide = matches!(cell, Cell::Block { .. });
            let rect = cell_box(
                plc,
                ui,
                section,
                rung_index,
                position,
                &label,
                &face,
                name_y,
                below_height,
                Vec2::new(if wide { SPAN } else { CELL_WIDTH }, strip_height),
                live,
            );
            if cell_selected(plc, section, rung_index, position)
                || (wide && cell_selected(plc, section, rung_index, position + 1))
            {
                *cursor_rect = Some(rect);
            }
            spans_right = wide;
        }
        let coil = plc.operand_label(&rung.coil().var);
        let coil_rect = cell_box(
            plc,
            ui,
            section,
            rung_index,
            POSITIONS,
            &coil,
            // A rung with no coil has an empty coil slot: draw the conductor,
            // not a coil symbol with nothing written in it.
            &if rung.has_coil() {
                Face::coil(rung.coil().kind)
            } else {
                Face::from_cell(&Cell::Wire)
            },
            name_y,
            below_height,
            Vec2::new(CELL_WIDTH, strip_height),
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
                coil_live(plc, &rung.coil().var, delivered)
            },
        );
        if cell_selected(plc, section, rung_index, POSITIONS) {
            *cursor_rect = Some(coil_rect);
        }
        // The right margin: reserved like a cell so the strip is as wide as what
        // it draws, and the comment sits level with the conductor, plain text. The
        // margin is at least `COMMENT_W` — room to type in — and wider when the
        // comment needs it, so a long one is never clipped at the window edge.
        let comment = if editing {
            format!("{}_", plc.rung_comment_buffer)
        } else {
            comment_plain(&rung.comment)
        };
        let font = FontId::monospace(theme::font_size(9.0));
        let needed = if comment.is_empty() {
            0.0
        } else {
            ui.painter()
                .layout_no_wrap(comment.clone(), font.clone(), theme::FG)
                .size()
                .x
        };
        let (margin, _) = ui.allocate_exact_size(
            Vec2::new((needed + 2.0 * COMMENT_GAP).max(COMMENT_W), strip_height),
            Sense::hover(),
        );
        if !comment.is_empty() {
            ui.painter().text(
                egui::pos2(margin.left() + COMMENT_GAP, conductor_y(margin)),
                Align2::LEFT_CENTER,
                comment,
                font,
                if editing {
                    theme::ACCENT
                } else {
                    theme::COMMENT
                },
            );
        }
    });
    // The focused comment is what the view has to keep in sight while the arrows walk
    // the column: no cell of the row is selected then, the row still is.
    if rung_text_focused(plc, section, rung_index) {
        *cursor_rect = Some(strip.response.rect);
    }
    if cell_selected(plc, section, rung_index, POSITIONS) {
        // A faint edge says the whole rung is the unit being addressed while the
        // coil is the cell with the cursor. It is not drawn while the comment has the
        // keyboard: the comment is the focused thing then, and a marked coil beside it
        // read as two selections at once.
        let rect = strip.response.rect;
        ui.painter().rect_filled(
            egui::Rect::from_min_size(rect.min, egui::vec2(3.0, rect.height())),
            1.0,
            Color32::from_rgba_unmultiplied(0x4d, 0xa3, 0xff, 36),
        );
    }
    if banded {
        // The label's band, while it is stored or being typed. The operand names sit one band below it (see
        // `BANDED_NAME_Y`), so the label cannot print over the first cell's name.
        let rect = strip.response.rect;
        ui.painter().text(
            egui::pos2(rect.left() + 2.0, rect.top() + LABEL_Y),
            Align2::LEFT_CENTER,
            format!("{label}:"),
            FontId::monospace(theme::font_size(NAME_PX)),
            theme::COMMENT,
        );
    }
    // Only the circuit: the comment margin is outside the right rail, and the
    // rails span the rows they feed.
    let row_rect = egui::Rect::from_min_size(
        strip.response.rect.min,
        egui::vec2(
            (POSITIONS + 1) as f32 * CELL_WIDTH,
            strip.response.rect.height(),
        ),
    );
    ui.add_space(ROW_GAP);
    row_rect
}

/// How an element is painted. Weight and colour both come from `State`, so a call
/// site cannot remember one and forget the other, and never by shape: three states
/// have one outline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    /// The snapshot cannot answer for this element, so it is neither on nor off.
    /// Drawing it as `Off` would be a claim the program has not made.
    Unknown,
    Off,
    On,
}

/// A stroke's weight and colour, both derived from the state.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Style {
    ink: Color32,
    accent: Color32,
    state: State,
}

impl Style {
    pub fn new(ink: Color32, accent: Color32, state: State) -> Self {
        Self { ink, accent, state }
    }

    /// Outline, bars, arcs. Dimmed when the state is unknown, so an element the
    /// snapshot cannot answer for is not painted as an off one.
    fn body(&self, s: f32) -> Stroke {
        let width = if matches!(self.state, State::On) {
            2.2
        } else {
            1.6
        };
        Stroke::new(width * s, self.ink())
    }

    /// The ink for this state: `Unknown` is dimmed, not a claim of off.
    fn ink(&self) -> Color32 {
        match self.state {
            State::Unknown => self.ink.gamma_multiply(0.55),
            _ => self.ink,
        }
    }

    /// The conductor. Dimmed when the state is unknown: the wire is there, what
    /// flows through it is not known.
    fn lead(&self, s: f32) -> Stroke {
        let width = if matches!(self.state, State::On) {
            1.6
        } else {
            1.2
        };
        Stroke::new(width * s, self.ink())
    }
}

/// One row of a block body. Row 0 is the power row, and the only row that carries
/// conductor. Names come from the instruction table; values arrive already
/// formatted, because the renderer positions text and never formats a number.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Row<'a> {
    /// The input pin's name, inside the body on the left.
    pub ins: Option<&'a str>,
    /// The output pin's name, inside the body on the right.
    pub out: Option<&'a str>,
    /// The input value, outside the body, right-aligned.
    pub value_in: Option<&'a str>,
    /// The output value, outside the body, left-aligned.
    pub value_out: Option<&'a str>,
    /// The input pin on this row is TRUE, so its name lights.
    pub ins_lit: bool,
    /// The output pin on this row is TRUE, so its name lights.
    pub out_lit: bool,
}

/// What to draw for one cell: a shape from the table, a glyph, and the block rows.
///
/// A block's live state is per pin, not per box: `q` lights the frame, the `Q`
/// label and the output lead, `in_power` lights the power row's input label and
/// its lead, and each row's own flags light its two labels. Nothing else
/// changes, so a timer that is timing does not turn its whole box green.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Face<'a> {
    pub shape: Shape,
    pub glyph: &'a str,
    pub rows: &'a [Row<'a>],
    /// The block's `Q` this scan.
    pub q: bool,
    /// The power reaching the block's first input pin this scan.
    pub in_power: bool,
}

impl Default for Face<'_> {
    /// A face with no state of its own: the picker draws a face, not a scan.
    fn default() -> Self {
        Face {
            shape: Shape::Contact,
            glyph: "",
            rows: &[],
            q: false,
            in_power: false,
        }
    }
}

impl<'a> Face<'a> {
    /// The one cell-to-symbol mapping. The shape comes from the instruction table,
    /// never from matching a `Cell` variant here, so deleting `CellSymbol` cannot
    /// leave two mappings to drift.
    pub fn from_cell(cell: &Cell) -> Face<'static> {
        let (shape, glyph) = match cell {
            Cell::Contact { negated, .. } => (
                if *negated {
                    Shape::ContactNc
                } else {
                    Shape::Contact
                },
                "",
            ),
            Cell::Edge { rising, .. } => (Shape::Contact, if *rising { "P" } else { "N" }),
            Cell::Block { block, .. } => (
                Shape::Block,
                linc_plc_core::profile::spec::spec(block)
                    .map(|spec| spec.mnemonic)
                    .unwrap_or(""),
            ),
            Cell::Compare { op, .. } => (Shape::Small, compare_sign(op)),
            Cell::Expr { .. } => (Shape::Small, ""),
            Cell::Compute { .. } => (Shape::Small, ":="),
            Cell::Wire => (Shape::Wire, ""),
            Cell::Link => (Shape::Link, ""),
        };
        Face {
            shape,
            glyph,
            rows: &[],
            ..Face::default()
        }
    }

    /// A coil's letter is its glyph; a negated coil's glyph is the slash.
    pub fn coil(kind: CoilKind) -> Face<'static> {
        let glyph = match kind {
            CoilKind::Set => "S",
            CoilKind::Reset => "R",
            CoilKind::Jump => "J",
            CoilKind::Call => "C",
            CoilKind::Compute => "=",
            CoilKind::Negated => "/",
            CoilKind::Normal => "",
        };
        Face {
            shape: Shape::Coil,
            glyph,
            rows: &[],
            ..Face::default()
        }
    }
}

/// One continuous conductor per rung, name above the symbol, no
/// cell borders and no spacing between cells. A row is a strip whose conductor
/// runs along `CONDUCTOR_Y` from its left edge to its right edge.
pub const CELL_WIDTH: f32 = 48.0;
pub const CELL_HEIGHT: f32 = 50.0;
/// The conductor is centred in the cell. A conductor below the centre puts the
/// rung low and makes the row gap read as an empty band above every symbol.
pub const CONDUCTOR_Y: f32 = CELL_HEIGHT / 2.0;
pub const ROW_GAP: f32 = 0.0;
/// Distance from one row's conductor to the next row's conductor.
pub const ROW_STRIDE: f32 = CELL_HEIGHT + ROW_GAP;

// --- contacts --------------------------------------------------------------
/// Half the separation between a contact's bars: they stand at `cx +- BAR_HALF`.
/// 16 apart is Table 61's proportion, and it leaves the gap wide enough for the
/// `P`/`N` letter to sit inside it.
pub const BAR_HALF: f32 = 8.0;
/// Half a bar's height.
pub const BAR_HALF_HEIGHT: f32 = 8.0;
/// The NC slash spans bar to bar, at 45 degrees, and stops there. A slash past
/// the bars reads as a stray stroke next to the contact instead of part of it.
pub const SLASH: f32 = BAR_HALF;
/// The letter on an edge contact sits on the conductor, between the bars: above
/// the bars it reads as a separate annotation.
pub const EDGE_LETTER_Y: f32 = CONDUCTOR_Y;
/// The power rails' stroke, in cell units. A rail is a conductor, not furniture:
/// it takes the same weight as the leads it terminates and does not change with
/// the rung's state, because it is fed from outside the ladder.
pub const RAIL_W: f32 = 1.2;

/// The room a label needs above a block body before the row has to grow. A
/// symbol-only row is `CELL_HEIGHT` tall; a row holding a block grows by whatever
/// the body needs beyond that, and `CELL_HEIGHT` itself never changes.
pub const LABEL_BAND: f32 = 12.0;
/// Where a cell's operand name sits. The conductor is centred, so this band
/// shares the space above the bars with it.
pub const LABEL_Y: f32 = 8.0;

/// The size every operand name over a cell draws at. One size, not one per cell: a
/// name that needs more room is clipped, never shrunk.
pub const NAME_PX: f32 = 8.5;
/// A rung with a label grows by this much: the label takes a line of its own
/// above the operand names, because both shared `LABEL_Y` and a label printed
/// over the first cell's name. It is the label band plus the 2 px a 13 px line
/// box needs to clear the one above it. A rung's *comment* does not grow the row:
/// it sits in the right margin, level with the conductor.
pub const LABEL_ROW_BAND: f32 = LABEL_BAND + 2.0;
/// Where the operand names sit on such a rung.
pub const BANDED_NAME_Y: f32 = LABEL_Y + LABEL_ROW_BAND;

/// The margin a rung reserves to the right of its coil for the rung comment. It
/// is part of the strip's width, allocated like a cell, so the scroll area counts
/// it and the text is never clipped; the conductor still ends at the right rail,
/// which sits inside this margin.
pub const COMMENT_W: f32 = 220.0;
/// The gap between the right rail and the comment, so the text cannot touch the
/// circuit it describes.
pub const COMMENT_GAP: f32 = 8.0;

// --- coils -----------------------------------------------------------------
pub const COIL_R: f32 = 8.0;
/// A half circle. A coil is two of them, split on the vertical axis: a left half
/// and a right half, each crossing the conductor at its own side, so a lead ends
/// on the arc where it enters the coil. Two 180 degree halves of one circle would
/// meet at the top and bottom and stroke into a single closed ring, which reads as
/// a circle and not as the symbol Table 62 draws; `COIL_OPEN` keeps those two
/// junctions open.
pub const COIL_SPAN: f32 = std::f32::consts::PI;
/// The opening at the top and bottom junction of the two coil arcs, in radians
/// (about 17 degrees): wide enough to show at the 1.6 unit stroke. The sides stay
/// full, because that is where the leads enter and their ends must sit on the arc.
pub const COIL_OPEN: f32 = 0.30;
/// Where a coil's lead ends: on the arc, not at a contact's half-gap.
pub const COIL_LEAD_X: f32 = COIL_R;

// --- small boxes: compare, arithmetic, `:=` ---------------------------------
pub const SMALL_W: f32 = 28.0;
pub const SMALL_H: f32 = 18.0;
pub const SMALL_R: f32 = 3.0;

// --- block bodies ----------------------------------------------------------
/// A block occupies two positions. Its pin values are drawn outside the body and
/// there is no room for them in one cell, so the body sits centred on the
/// boundary: 29 units of value column on each side.
pub const SPAN: f32 = 2.0 * CELL_WIDTH;
pub const BLOCK_W: f32 = 38.0;
/// 4 of 38: under 3 the box reads as a rectangle, over 6 it reads as a button.
pub const BLOCK_R: f32 = 4.0;
/// Height of a one-row body, before the row pitch and the bottom margin.
pub const BLOCK_H: f32 = 18.0;
/// One extra row adds one pitch.
pub const ROW_PITCH: f32 = 9.0;
/// Bottom margin, so a three-row body is not flush with its last row.
pub const BLOCK_PAD: f32 = 5.0;
/// The gap between the body and a pin's value.
pub const PAD_GAP: f32 = 6.0;
/// Room for a value such as `2.4s` or `007`.
pub const VALUE_W: f32 = 21.0;
/// Breathing space between the value column and the next cell's symbol.
pub const MARGIN: f32 = 2.0;
/// One side of the body: pad, value, margin. Two of these plus the body is the
/// span exactly.
pub const SIDE: f32 = PAD_GAP + VALUE_W + MARGIN;
/// Pin names and values, and the block's type name.
pub const TITLE_PX: f32 = 6.5;
/// Half a text line's box at `TITLE_PX`: the glyphs span this either side of the
/// line's centre.
pub const TEXT_HALF: f32 = 5.0;
/// The clearance a pin line keeps from the body's rim, top and bottom. With the
/// content centred, `BLOCK_H`, `BLOCK_PAD` and `ROW_PITCH` give exactly this at
/// every row count: a one-row body is 23 tall and keeps 11.5 - (4.5 + 5).
pub const BLOCK_TEXT_PAD: f32 = 2.0;
/// Padding from the body's edge to its text.
pub const INNER_PAD: f32 = 6.0;
/// A symbol's lead to the strip edge, for a symbol that fits in one position.
pub const LEAD: f32 = (CELL_WIDTH - BLOCK_W) / 2.0;
/// The tallest body is the tightest fit: a three-row block keeps 4.5 units of
/// clearance above and below, and a shorter one keeps more.
pub const BLOCK_LEAD_V: f32 = (CELL_HEIGHT - (BLOCK_H + 2.0 * ROW_PITCH + BLOCK_PAD)) / 2.0;

/// Body height for a block of `rows` rows: 23, 32, 41 for 1, 2, 3. The height
/// follows the row count so no block is padded with an empty row and none is
/// clipped.
pub fn block_height(rows: usize) -> f32 {
    BLOCK_H + ROW_PITCH * rows.saturating_sub(1) as f32 + BLOCK_PAD
}

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

/// A contact's two bars, as segments. The painter draws these and the tests
/// measure them, so a test cannot pass on a figure the picture does not use.
fn contact_bars(cx: f32, y: f32, s: f32) -> [[egui::Pos2; 2]; 2] {
    let bar = BAR_HALF * s;
    let half = BAR_HALF_HEIGHT * s;
    [
        [
            egui::pos2(cx - bar, y - half),
            egui::pos2(cx - bar, y + half),
        ],
        [
            egui::pos2(cx + bar, y - half),
            egui::pos2(cx + bar, y + half),
        ],
    ]
}

/// The NC slash: bar to bar at 45 degrees, ending on the bars' ends. It spans the
/// bars and goes no further; a longer slash reads as a stray stroke beside the
/// contact rather than part of it.
fn contact_slash(cx: f32, y: f32, s: f32) -> [egui::Pos2; 2] {
    let slash = SLASH * s;
    [
        egui::pos2(cx - slash, y + slash),
        egui::pos2(cx + slash, y - slash),
    ]
}

/// Where a `P` or `N` edge letter goes: on the conductor, midway between the bars,
/// which is the only place Table 61 puts it. Above the bars it reads as a separate
/// annotation and the contact as a different symbol.
fn edge_letter_at(cx: f32, y: f32, s: f32) -> egui::Pos2 {
    egui::pos2(cx, y + (EDGE_LETTER_Y - CONDUCTOR_Y) * s)
}

/// A coil's two arcs, split on the vertical axis: the left half runs from just
/// above the bottom junction to just below the top one, the right half mirrors it.
/// Each arc crosses the conductor at its own side, where the lead ends at
/// `COIL_LEAD_X`; the two openings sit at the top and bottom junctions, so the
/// strokes cannot weld into one closed ring.
fn coil_arcs(cx: f32, y: f32, s: f32) -> (Vec<egui::Pos2>, Vec<egui::Pos2>) {
    let centre = egui::pos2(cx, y);
    let radius = COIL_R * s;
    let half = std::f32::consts::PI;
    // Angles are measured from 3 o'clock, y downwards: `PI/2` is the bottom of the
    // circle and `-PI/2` its top, so the trims land on those two junctions.
    let left = arc_points(
        centre,
        radius,
        half / 2.0 + COIL_OPEN,
        3.0 * half / 2.0 - COIL_OPEN,
        16,
    );
    let right = arc_points(
        centre,
        radius,
        -half / 2.0 + COIL_OPEN,
        half / 2.0 - COIL_OPEN,
        16,
    );
    (left, right)
}

/// Where a coil's letter goes: the middle of the loop, between the arcs, as
/// Table 62 draws `--(S)--`. Outside the coil it reads as a separate annotation
/// the coil does not own.
fn coil_letter_at(cx: f32, y: f32) -> egui::Pos2 {
    egui::pos2(cx, y)
}

/// A negated coil's slash: inside the loop, spanning the arc gap, so the stroke
/// belongs to the coil rather than floating past its right edge.
fn coil_slash(cx: f32, y: f32, s: f32) -> [egui::Pos2; 2] {
    let reach = COIL_R * s * 0.72;
    [
        egui::pos2(cx - reach, y + reach),
        egui::pos2(cx + reach, y - reach),
    ]
}

/// The row baselines of a block body, top to bottom, in the body's coordinates.
/// Row 0 sits on the conductor; each row after it is one pitch lower.
/// The y of one line in a block body, `line` 0 being the type name and 1.. the pin
/// rows, with the whole content block centred: a body of `rows` pin rows holds
/// `rows + 1` lines one `ROW_PITCH` apart, centred on the body's centre.
fn block_line_y(body: egui::Rect, line: usize, rows: usize, s: f32) -> f32 {
    let lines = (rows + 1) as f32;
    body.center().y + ROW_PITCH * s * (line as f32 - (lines - 1.0) / 2.0)
}

/// The height a strip needs when it holds a block of `rows` rows: the body, plus
/// the label band it needs above it. `CELL_HEIGHT` itself does not change — only
/// the stride between rows does, so a symbol-only row keeps its size.
/// The lowest a block's own rim may sit below the strip's top edge: the instance
/// label is drawn in the band above it, and its line box plus the clearance the
/// name tests ask for must fit there. Without this floor a one-row body (23 tall)
/// put its rim at 13.5 while the label's line ran to 14.5 and the instance printed
/// over the box.
pub const BLOCK_LABEL_BAND: f32 = LABEL_Y + 6.5 + 2.5;

/// The height a row's strip takes: `CELL_HEIGHT` for symbols, and for a block body
/// whichever is larger — the body plus the label band above it, or the body's own
/// half-height plus the conductor offset and the band the label needs.
fn row_height(rows: usize) -> f32 {
    // `0` rows means the row holds no block at all: a symbol-only row is
    // `CELL_HEIGHT` and the block floor below does not apply to it.
    if rows == 0 {
        return CELL_HEIGHT;
    }
    let body = block_height(rows);
    CELL_HEIGHT
        .max(body + 2.0 * LABEL_BAND)
        .max(body / 2.0 + CONDUCTOR_Y + BLOCK_LABEL_BAND)
}

/// One strip's conductor, `CONDUCTOR_Y` above the strip's bottom edge.
fn conductor_y(rect: egui::Rect) -> f32 {
    rect.bottom() - (CELL_HEIGHT - CONDUCTOR_Y)
}

pub(crate) fn draw_wire(painter: &egui::Painter, rect: egui::Rect, style: Style) {
    let s = rect.height() / CELL_HEIGHT;
    let y = conductor_y(rect);
    painter.line_segment(
        [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
        style.lead(s),
    );
}

pub(crate) fn draw_link(
    painter: &egui::Painter,
    rect: egui::Rect,
    style: Style,
    below_height: f32,
) {
    let s = rect.height() / CELL_HEIGHT;
    let y = conductor_y(rect);
    let cx = rect.center().x;
    painter.line_segment(
        [egui::pos2(rect.left(), y), egui::pos2(rect.right(), y)],
        style.lead(s),
    );
    // The junction reaches the next row's conductor: `CONDUCTOR_Y` above that
    // row's bottom edge, whatever its height. A constant half-cell drop stopped
    // short of every row that grew for a block body or a comment band, which left
    // the branch visibly unjoined.
    painter.line_segment(
        [
            egui::pos2(cx, y),
            egui::pos2(cx, rect.bottom() + link_drop(below_height)),
        ],
        style.lead(s),
    );
}

/// How far a link's junction reaches below its own row: to the conductor of the
/// row underneath, which sits `CONDUCTOR_Y` above that row's bottom edge.
fn link_drop(below_height: f32) -> f32 {
    (below_height - CONDUCTOR_Y).max(0.0)
}

/// The height a row's strip takes, from the rung alone: block bodies grow it, and
/// a label or a comment adds the band the comment sits in.
fn strip_height_of(rung: &Rung) -> f32 {
    let rows = rung
        .cells
        .iter()
        .filter_map(|cell| match cell {
            Cell::Block { block, .. } => {
                linc_plc_core::profile::spec::spec(block).map(|spec| spec.pins.0.len().max(1))
            }
            _ => None,
        })
        .max()
        .unwrap_or(0);
    let banded = !rung.label.trim().is_empty();
    row_height(rows) + if banded { LABEL_ROW_BAND } else { 0.0 }
}

/// Draw one symbol. Rung furniture and the symbol are here; the operand label is
/// `grid_cell_label`'s, and `glyph` is symbol furniture only.
pub(crate) fn draw_face(
    painter: &egui::Painter,
    rect: egui::Rect,
    face: &Face<'_>,
    style: Style,
    below_height: f32,
) {
    let s = rect.height() / CELL_HEIGHT;
    let y = conductor_y(rect);
    let cx = rect.center().x;
    let left = rect.left();
    let right = rect.right();
    let lead = style.lead(s);
    match face.shape {
        // A bare conductor and its branch draw no symbol: the span is one line.
        Shape::Wire => draw_wire(painter, rect, style),
        Shape::Link => draw_link(painter, rect, style, below_height),
        Shape::Contact | Shape::ContactNc => {
            let bar = BAR_HALF * s;
            painter.line_segment([egui::pos2(left, y), egui::pos2(cx - bar, y)], lead);
            painter.line_segment([egui::pos2(cx + bar, y), egui::pos2(right, y)], lead);
            for segment in contact_bars(cx, y, s) {
                painter.line_segment(segment, style.body(s));
            }
            if face.shape == Shape::ContactNc {
                painter.line_segment(contact_slash(cx, y, s), style.body(s));
            }
            if !face.glyph.is_empty() {
                painter.text(
                    edge_letter_at(cx, y, s),
                    Align2::CENTER_CENTER,
                    face.glyph,
                    FontId::monospace(8.0 * s),
                    style.ink,
                );
            }
        }
        Shape::Coil => {
            let radius = COIL_R * s;
            painter.line_segment([egui::pos2(left, y), egui::pos2(cx - radius, y)], lead);
            painter.line_segment([egui::pos2(cx + radius, y), egui::pos2(right, y)], lead);
            let (left_arc, right_arc) = coil_arcs(cx, y, s);
            painter.add(egui::Shape::line(left_arc, style.body(s)));
            painter.add(egui::Shape::line(right_arc, style.body(s)));
            if face.glyph == "/" {
                painter.line_segment(coil_slash(cx, y, s), style.body(s));
            } else if !face.glyph.is_empty() {
                painter.text(
                    coil_letter_at(cx, y),
                    Align2::CENTER_CENTER,
                    face.glyph,
                    FontId::monospace(8.5 * s),
                    style.ink,
                );
            }
        }
        Shape::Small => {
            let body = egui::Rect::from_center_size(
                egui::pos2(cx, y),
                egui::vec2(SMALL_W * s, SMALL_H * s),
            );
            painter.rect_stroke(body, SMALL_R * s, style.body(s), egui::StrokeKind::Middle);
            painter.line_segment([egui::pos2(left, y), egui::pos2(body.left(), y)], lead);
            painter.line_segment([egui::pos2(body.right(), y), egui::pos2(right, y)], lead);
            if !face.glyph.is_empty() {
                painter.text(
                    body.center(),
                    Align2::CENTER_CENTER,
                    face.glyph,
                    FontId::monospace(9.0 * s),
                    style.ink,
                );
            }
        }
        Shape::Block => draw_block(painter, rect, face, style, s),
    }
}

/// A block's two leads: the only horizontal lines the body has. Both are on the
/// power row; every other row is a value row and draws no line at all, because a
/// stub there would invent a connection that does not exist.
fn block_leads(rect: egui::Rect, body: egui::Rect, s: f32) -> [[egui::Pos2; 2]; 2] {
    let y = conductor_y(rect);
    let _ = s;
    [
        [egui::pos2(rect.left(), y), egui::pos2(body.left(), y)],
        [egui::pos2(body.right(), y), egui::pos2(rect.right(), y)],
    ]
}

fn draw_block(painter: &egui::Painter, rect: egui::Rect, face: &Face<'_>, style: Style, s: f32) {
    // A lit conductor is the same weight `Style::lead` gives a true element.
    let lead = style.lead(s);
    let hot = Stroke::new(1.6 * s, style.accent);
    let y = conductor_y(rect);
    let cx = rect.center().x;
    let rows = face.rows.len().max(1);
    let body = egui::Rect::from_center_size(
        egui::pos2(cx, y),
        egui::vec2(BLOCK_W * s, block_height(rows) * s),
    );
    // The power row is the only row with conductor: the block is in series, so the
    // wire runs from the span's edges to the body on both sides. Each side carries
    // its own state — the input lead the power arriving, the output lead `Q` — so a
    // timer that is enabled reads lit on the left and dark on the right.
    let leads = block_leads(rect, body, s);
    painter.line_segment(leads[0], if face.in_power { hot } else { lead });
    painter.line_segment(leads[1], if face.q { hot } else { lead });
    // The frame is the block's output, not its input: only `Q` lights the outline,
    // so a timing timer keeps a grey box with a green `TT`/`IN`.
    if face.q {
        painter.rect_stroke(
            body,
            BLOCK_R * s,
            Stroke::new(2.2 * s, style.accent),
            egui::StrokeKind::Middle,
        );
    } else {
        painter.rect_stroke(body, BLOCK_R * s, style.body(s), egui::StrokeKind::Middle);
    }
    if !face.glyph.is_empty() {
        // The type text is the block's own name, not its instance: the instance is
        // in the label above the span. It is the first line of the content block,
        // one pitch above the first pin row and centred with it.
        painter.text(
            egui::pos2(cx, block_line_y(body, 0, rows, s)),
            Align2::CENTER_CENTER,
            face.glyph,
            FontId::monospace(TITLE_PX * s),
            style.ink,
        );
    }
    let name_size = FontId::monospace(TITLE_PX * s);
    let value_size = FontId::monospace(TITLE_PX * s);
    for (index, row) in face.rows.iter().enumerate() {
        let ry = block_line_y(body, index + 1, rows, s);
        // Names are text, not elements: they take the same ink the body does, so
        // a not-lit pin and an Unknown symbol are the same grey rather than two.
        if let Some(name) = row.ins {
            painter.text(
                egui::pos2(body.left() + INNER_PAD * s, ry),
                Align2::LEFT_CENTER,
                name,
                name_size.clone(),
                if row.ins_lit {
                    style.accent
                } else {
                    style.ink()
                },
            );
        }
        if let Some(name) = row.out {
            painter.text(
                egui::pos2(body.right() - INNER_PAD * s, ry),
                Align2::RIGHT_CENTER,
                name,
                name_size.clone(),
                if row.out_lit {
                    style.accent
                } else {
                    style.ink()
                },
            );
        }
        // Values sit outside the body, on their own row, and no line is drawn to
        // them: a stub there would invent a connection that does not exist.
        if let Some(value) = row.value_in {
            painter.text(
                egui::pos2(body.left() - PAD_GAP * s, ry),
                Align2::RIGHT_CENTER,
                value,
                value_size.clone(),
                style.ink,
            );
        }
        if let Some(value) = row.value_out {
            painter.text(
                egui::pos2(body.right() + PAD_GAP * s, ry),
                Align2::LEFT_CENTER,
                value,
                value_size.clone(),
                style.ink,
            );
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
        // No local evaluator for a free-text expression: the cell stays neutral
        // instead of showing a guess.
        Cell::Expr { .. } => None,
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

/// The power a rung delivers to its coil, from the snapshot alone.
///
/// Three-valued: a rung with a false condition is off whatever else is unknown,
/// and a rung whose conditions are all true but for one unanswered cell is
/// unknown, not off.
pub fn rung_power(plc: &PlcUi, cells: &[Cell]) -> Option<bool> {
    let mut unknown = false;
    for cell in cells {
        let value = match cell {
            Cell::Contact { .. }
            | Cell::Edge { .. }
            | Cell::Compare { .. }
            | Cell::Block { .. }
            | Cell::Expr { .. } => cell_active(plc, cell),
            // A bare wire, a link, a compute cell: the conductor itself.
            _ => continue,
        };
        match value {
            Some(false) => return Some(false),
            Some(true) => {}
            None => unknown = true,
        }
    }
    (!unknown).then_some(true)
}

/// A coil's live state. Its own bit when the snapshot names it; for a member
/// write (`CTU0.R`, a counter's reset) the snapshot has no readable bit, so the
/// coil shows the power that reached it — the write *is* that power.
pub(crate) fn coil_live(plc: &PlcUi, var: &str, delivered: Option<bool>) -> Option<bool> {
    plc.bit_value(var)
        .or_else(|| var.contains('.').then_some(delivered).flatten())
}

/// The element's live state from the preview when one is held, otherwise from the
/// snapshot.
fn live_at(
    plc: &PlcUi,
    local: &Option<(Vec<Option<bool>>, Option<bool>)>,
    power: Option<bool>,
    cell: &Cell,
) -> Option<bool> {
    match cell {
        // A wire carries the rung's *output*, not the power that happens to have
        // reached its column: lighting every wire upstream of a toggled contact
        // said the rung was true when its output was still false. With no preview
        // held the output comes from the snapshot, so a live ladder's wires show
        // the rung's state as it is.
        Cell::Wire | Cell::Link => match local {
            Some((_, coil)) => *coil,
            None => power,
        },
        // A symbol answers for itself: a contact's bars follow its own bit, so
        // toggling it shows on the contact even when the rung's output does not
        // move.
        _ => cell_active(plc, cell),
    }
}

/// One pin row of a block face, with the two facts the renderer needs about it:
/// the labels, the formatted values, and whether each label is lit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PinRow<'a> {
    pub ins: Option<&'a str>,
    pub out: Option<&'a str>,
    pub value_in: Option<String>,
    pub value_out: Option<String>,
    pub ins_lit: bool,
    pub out_lit: bool,
}

/// A block's pin rows and its two facts, read from the member bits the scan
/// publishes: `q` is `Q` (the frame and the output lead), `in_power` is the
/// first input pin (the input lead). Pure, so a test can read it without a UI.
pub(crate) fn block_state<'a>(
    plc: &PlcUi,
    instance: &str,
    preset: &str,
    pins_in: &[&'a str],
    pins_out: &[&'a str],
) -> (Vec<PinRow<'a>>, bool, bool) {
    let bit = |pin: &str| plc.bit_value(&format!("{instance}.{pin}")) == Some(true);
    let rows = (0..pins_in.len().max(pins_out.len()))
        .map(|index| {
            let ins = pins_in.get(index).copied().filter(|name| !name.is_empty());
            let out = pins_out.get(index).copied().filter(|name| !name.is_empty());
            let (value_in, value_out) = match (ins, out) {
                (Some(pin), Some(out)) => {
                    let (left, right) = pin_values(plc, instance, preset, pin, out);
                    (Some(left), Some(right))
                }
                (Some(pin), None) => (Some(pin_values(plc, instance, preset, pin, "").0), None),
                (None, Some(out)) => (None, Some(pin_values(plc, instance, preset, "", out).1)),
                (None, None) => (None, None),
            };
            PinRow {
                ins,
                out,
                value_in,
                value_out,
                ins_lit: ins.map(bit).unwrap_or(false),
                out_lit: out.map(bit).unwrap_or(false),
            }
        })
        .collect::<Vec<_>>();
    let q = bit("Q");
    let in_power = pins_in.first().map(|pin| bit(pin)).unwrap_or(false);
    (rows, q, in_power)
}

/// A block pin's value, formatted for display. The UI decides what a value means;
/// the renderer only positions the string.
///
/// Only TIME and INT pins carry text. A Boolean pin shows nothing — its name lights
/// instead — and an operand the snapshot cannot answer for shows `-`, never `0`, so
/// an unanswered pin never reads as a value.
fn pin_values(plc: &PlcUi, instance: &str, preset: &str, pin: &str, out: &str) -> (String, String) {
    let member = |name: &str| plc.word_value(&format!("{instance}.{name}"));
    let mut left = String::new();
    let mut right = String::new();
    // The pin name is the family's own marker: a timer reads `PT`/`ET` and counts
    // milliseconds, a counter reads `PV`/`CV` and counts pulses. Printing `CV`
    // through the timer's formatter showed `0.0s` for a counter that had counted.
    match pin {
        "PT" => left = display_preset(preset, true),
        "PV" => left = display_preset(preset, false),
        _ => {}
    }
    match out {
        // Milliseconds turned into seconds, for a timer's elapsed time.
        "ET" => right = member(out).map(display_ms).unwrap_or_else(|| "-".into()),
        // The count itself, dimensionless, updating per scan like `ET`.
        "CV" => {
            right = member(out)
                .map(|count| count.to_string())
                .unwrap_or_else(|| "-".into())
        }
        _ => {}
    }
    (left, right)
}

/// `T#2400ms` and `2400` both read as a time on the face, because a timer's preset
/// is a time whichever way the cell spells it.
fn display_preset(preset: &str, timer: bool) -> String {
    match linc_plc_core::profile::resolve::parse_preset(preset, timer) {
        Some(value) if timer => display_ms(value),
        _ => preset.to_string(),
    }
}

/// Seconds with one decimal, `T#` dropped: `4s`, `2.4s`.
fn display_ms(value: i32) -> String {
    if value % 1000 == 0 {
        format!("{}s", value / 1000)
    } else {
        format!("{:.1}s", value as f32 / 1000.0)
    }
}

fn cell_box(
    plc: &PlcUi,
    ui: &mut Ui,
    section: usize,
    rung: usize,
    position: usize,
    label: &str,
    face: &Face<'_>,
    name_y: f32,
    below_height: f32,
    size: Vec2,
    live: Option<bool>,
) -> egui::Rect {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let selected = cell_selected(plc, section, rung, position);
    let painter = ui.painter();
    // No per-cell background or border: the strip is bare and the conductor is
    // what the eye follows. Selection tints the element and marks the cell with
    // two small ticks, never a box.
    let color: Color32 = if live == Some(true) {
        // A true element is green, as the manual's monitor shows it.
        theme::C4
    } else if selected {
        theme::ACCENT
    } else {
        theme::FG
    };
    let state = match live {
        Some(true) => State::On,
        Some(false) => State::Off,
        None => State::Unknown,
    };
    let style = Style::new(color, theme::C4, state);
    if selected {
        let tick = Stroke::new(1.6, theme::ACCENT);
        let y = conductor_y(rect);
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
    // The symbol area is the bottom `CELL_HEIGHT` of the strip: a row grows upwards
    // for a block's label band, and the symbol keeps its size.
    let symbol_rect = egui::Rect::from_min_size(
        egui::pos2(rect.left(), rect.bottom() - CELL_HEIGHT),
        egui::vec2(rect.width(), CELL_HEIGHT),
    );
    draw_face(painter, symbol_rect, face, style, below_height);
    if !label.is_empty() {
        // Every name over a cell draws at `NAME_PX`, whatever the cell or the length
        // of the name. Fitting each name to its own cell made `reset_count` and
        // `five_cycles` smaller than `enable` beside them, so a row read as two
        // different typographies. A name too long for its cell is clipped to the cell
        // instead of shrunk, which keeps one size across the whole ladder.
        let galley = painter.layout_no_wrap(
            label.to_string(),
            FontId::monospace(theme::font_size(NAME_PX)),
            color,
        );
        // Centred over its cell, and not clipped to it: a name wider than one cell
        // draws whole and reaches into the empty space above its neighbours, which is
        // where `five_cycles` and `reset_count` live. Clipping to the cell turned them
        // into `five_cy` and `reset_c`.
        let at = egui::pos2(
            rect.center().x - galley.size().x / 2.0,
            rect.top() + name_y - galley.size().y / 2.0,
        );
        painter.galley(at, galley, color);
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

/// The sign a compare draws, in the label and in the box: one table, so the
/// operand above the cell and the operator inside it cannot disagree. An operator
/// the profile does not know draws `?` rather than an empty box, because an empty
/// box reads as a symbol with nothing to say.
fn compare_sign(op: &str) -> &'static str {
    match op.to_ascii_uppercase().as_str() {
        "GT" => ">",
        "GE" => ">=",
        "EQ" => "=",
        "NE" => "<>",
        "LE" => "<=",
        "LT" => "<",
        _ => "?",
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
        Cell::Compare { op, left, right } => format!("{left}{}{right}", compare_sign(op)),
        Cell::Expr { text } => text.clone(),
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

    /// D3: the shape comes from the table, never from matching a cell variant in the
    /// renderer. This is the regression that keeps that true now `CellSymbol` is
    /// gone: one mapping, `Face::from_cell`, and the tests that read it.
    #[test]
    fn face_from_cell_yields_the_table_shape() {
        let block = Face::from_cell(&Cell::Block {
            block: "TON".to_string(),
            instance: "TON0".to_string(),
            preset: "T#1s".to_string(),
        });
        assert_eq!(block.shape, Shape::Block);
        assert_eq!(block.glyph, "TON", "the type name, not the instance");

        let nc = Face::from_cell(&Cell::Contact {
            var: "a".to_string(),
            negated: true,
        });
        assert_eq!(nc.shape, Shape::ContactNc);
        let no = Face::from_cell(&Cell::Contact {
            var: "a".to_string(),
            negated: false,
        });
        assert_eq!(no.shape, Shape::Contact);

        let compute = Face::from_cell(&Cell::Compute {
            op: "ADD".to_string(),
            target: "MW2".to_string(),
            left: "MW1".to_string(),
            right: "5".to_string(),
        });
        assert_eq!(compute.shape, Shape::Small);
        assert_eq!(compute.glyph, ":=");

        let edge = Face::from_cell(&Cell::Edge {
            var: "a".to_string(),
            rising: true,
        });
        assert_eq!(edge.shape, Shape::Contact);
        assert_eq!(edge.glyph, "P", "the letter is inside the gap");

        // A wire is a conductor with no symbol. Mapping it to a contact drew two
        // bars in the middle of every empty cell, so the shapes are their own.
        assert_eq!(Face::from_cell(&Cell::Wire).shape, Shape::Wire);
        assert_eq!(Face::from_cell(&Cell::Link).shape, Shape::Link);
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

    /// A block's instance label sits in the band above its body, and the band must
    /// clear the rim: a one-row body is 23 tall and its rim landed on the label's
    /// last line, which is how `RT0` came to print over the box.
    #[test]
    fn the_instance_label_clears_a_block_rim() {
        const LABEL_HALF: f32 = 6.5; // half a 13 px line box, as the editor draws it
        const CLEARANCE: f32 = 2.5;
        for rows in 1..=3 {
            let height = row_height(rows);
            let rim = height - CONDUCTOR_Y - block_height(rows) / 2.0;
            let label_bottom = LABEL_Y + LABEL_HALF;
            assert!(
                rim - label_bottom >= CLEARANCE,
                "{rows} rows: the rim at {rim} to the label bottom at {label_bottom} \
                 is under {CLEARANCE} px (row height {height})"
            );
        }
    }

    /// The pin rows and the type name are one content block, centred in the body,
    /// and it keeps `BLOCK_TEXT_PAD` from the rim top and bottom. A row that sat on
    /// the body's centre pushed the last pin line through the bottom border.
    #[test]
    fn the_block_content_is_centred_and_clears_the_rim() {
        for rows in 1..=3 {
            let body = egui::Rect::from_center_size(
                egui::pos2(100.0, 33.0),
                egui::vec2(BLOCK_W, block_height(rows)),
            );
            let lines: Vec<f32> = (0..=rows)
                .map(|line| block_line_y(body, line, rows, 1.0))
                .collect();
            // Centred: the first and last lines are the same distance from the
            // body's centre.
            let first = lines[0] - body.center().y;
            let last = lines[rows] - body.center().y;
            assert!(
                (first + last).abs() < 0.01,
                "{rows} rows: content {lines:?} is not centred in {body:?}"
            );
            // Clear of the rim, top and bottom, by the named pad.
            let half = block_height(rows) / 2.0;
            let pad = half - (last.abs() + TEXT_HALF);
            assert!(
                pad >= BLOCK_TEXT_PAD - 0.01,
                "{rows} rows: {pad} from the rim, want {BLOCK_TEXT_PAD}"
            );
        }
    }

    /// The operand label must sit clear of the symbol. With the strip's fixed
    /// geometry the gap is arithmetic, so a change to either constant that closes
    /// the clearance fails here instead of shipping a crowded ladder.
    #[test]
    fn the_label_clears_the_symbol_on_commented_and_plain_rows() {
        const LABEL_HALF: f32 = 4.5; // half a 9 px galley
        const SYMBOL_HALF: f32 = 9.0; // tallest one-position symbol above the conductor
                                      // The conductor is centred, so the band above it is `CONDUCTOR_Y` tall
                                      // and the label shares it with the symbol's upper half.
        for (name_y, conductor) in [
            (LABEL_Y, CONDUCTOR_Y),
            (BANDED_NAME_Y, LABEL_ROW_BAND + CONDUCTOR_Y),
        ] {
            let label_bottom = name_y + LABEL_HALF;
            let symbol_top = conductor - SYMBOL_HALF;
            assert!(
                symbol_top - label_bottom >= 2.5,
                "label bottom {label_bottom} to symbol top {symbol_top} must be 2.5 px"
            );
        }
        // The comment and the names are two lines, not one: a 13 px line box
        // must fit between their anchors, or the comment prints over the names.
        assert!(
            BANDED_NAME_Y - LABEL_Y >= 13.0,
            "the comment line and the name line must not share a row"
        );
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
            label: String::new(),
            cells: vec![Cell::Wire],
            comment: String::new(),
            logic: linc_plc_core::model::Logic::Const { value: true },
            coil: Some(linc_plc_core::model::Coil::new(
                "",
                linc_plc_core::model::CoilKind::Normal,
            )),
        };
        let mut actions = Vec::new();
        let (advance, spacing) = pass_with(|ui| {
            crate::screens::plc::zero_rung_spacing(ui);
            let before = ui.cursor().top();
            let mut cursor_rect = None;
            grid_rung(
                &plc,
                ui,
                0,
                0,
                &rung,
                None,
                None,
                &mut actions,
                &mut cursor_rect,
            );
            let step = ui.cursor().top() - before;
            grid_rung(
                &plc,
                ui,
                0,
                1,
                &rung,
                None,
                None,
                &mut actions,
                &mut cursor_rect,
            );
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
                label: String::new(),
                comment: String::new(),
                logic: linc_plc_core::model::Logic::Const { value: true },
                coil: Some(linc_plc_core::model::Coil::new(
                    "",
                    linc_plc_core::model::CoilKind::Normal,
                )),
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
                address: String::new(),
            })
        };
        point("pump", linc_plc_core::model::IoDirection::In);
        point("run", linc_plc_core::model::IoDirection::Out);
        plc.project.vars.push(linc_plc_core::model::VarDecl {
            name: "seal".into(),
            kind: linc_plc_core::model::VarKind::Bit,
            ty: linc_plc_core::model::VarType::Bool,
            init: Vec::new(),
            retain: false,
            comment: String::new(),
            direction: None,
        });
        let names: Vec<String> = coil_choices(&plc, 0, 0)
            .into_iter()
            .map(|(name, _)| name)
            .collect();
        assert!(
            !names.contains(&"pump".to_string()),
            "input offered: {names:?}"
        );
        assert!(names.contains(&"run".to_string()));
        assert!(names.contains(&"seal".to_string()));
    }

    /// A jump's operand is a label in its own section and a call's is a POU;
    /// offering bit symbols for either would hand the operator a target the
    /// validator refuses.
    /// A list with nothing to offer says what is missing and how to make one: a jump
    /// needs a rung label (`Shift+L`), a call needs another POU (`Ctrl+N`).
    /// One size for every name over a cell. Fitting each label to its own cell put
    /// `reset_count` and `five_cycles` at a smaller size than `enable` beside them, so
    /// a row of the ladder read as two different typographies.
    /// A focused comment and a selected cell are two different things: while the
    /// comment is typed, none of that rung's cells draws as selected.
    #[test]
    fn a_focused_comment_leaves_no_cell_selected() {
        let mut plc = PlcUi::default();
        plc.cell_cursor = Some((0, 1, POSITIONS));
        assert!(cell_selected(&plc, 0, 1, POSITIONS), "the coil is selected");
        assert!(!cell_selected(&plc, 0, 1, 0));

        plc.rung_comment_edit = Some((0, 1));
        assert!(
            !cell_selected(&plc, 0, 1, POSITIONS),
            "the comment has the focus"
        );
        assert!(!cell_selected(&plc, 0, 1, 0));
        // Another rung's cells are untouched by this rung's focus: only the row whose
        // text is focused loses its selection.
        plc.cell_cursor = Some((0, 0, 2));
        assert!(
            cell_selected(&plc, 0, 0, 2),
            "the cursor really is on row 0"
        );

        plc.rung_comment_edit = None;
        plc.rung_label_edit = Some((0, 1));
        assert!(
            !cell_selected(&plc, 0, 1, POSITIONS),
            "the label counts too"
        );
    }

    #[test]
    fn every_cell_name_draws_at_one_size() {
        let plc = PlcUi::default();
        let ctx = egui::Context::default();
        // An area paints the frame after it is placed.
        let mut warm = ctx.run_pass(egui::RawInput::default(), |ctx| {
            egui::Area::new(egui::Id::unique("cell-names"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ctx, |_ui| {});
        });
        warm.textures_delta.clear();
        let mut output = ctx.run_pass(egui::RawInput::default(), |ctx| {
            egui::Area::new(egui::Id::unique("cell-names"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ctx, |ui| {
                    for (index, label) in ["enable", "reset_count", "five_cycles", "MW2:=MW1+5"]
                        .iter()
                        .enumerate()
                    {
                        cell_box(
                            &plc,
                            ui,
                            0,
                            0,
                            index,
                            label,
                            &Face::default(),
                            LABEL_Y,
                            CELL_HEIGHT,
                            egui::vec2(CELL_WIDTH, CELL_HEIGHT),
                            None,
                        );
                    }
                });
        });
        output.textures_delta.clear();
        fn walk(shape: &egui::Shape, out: &mut Vec<(String, f32)>) {
            match shape {
                egui::Shape::Text(text) => {
                    out.push((text.galley.text().to_string(), text.galley.size().y))
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        walk(shape, out);
                    }
                }
                _ => {}
            }
        }
        let mut sizes = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut sizes);
        }
        let height = |label: &str| {
            sizes
                .iter()
                .find(|(text, _)| text == label)
                .unwrap_or_else(|| panic!("`{label}` is painted: {sizes:?}"))
                .1
        };
        let expected = height("enable");
        for label in ["reset_count", "five_cycles", "MW2:=MW1+5"] {
            assert_eq!(
                height(label),
                expected,
                "`{label}` draws at the same size as `enable`"
            );
        }
    }

    #[test]
    fn an_empty_coil_list_says_what_to_add() {
        assert_eq!(
            no_targets_hint(CoilKind::Jump),
            ("no labels in this section", "Shift+L on a rung types one")
        );
        assert_eq!(
            no_targets_hint(CoilKind::Call),
            ("no other POUs in this project", "Ctrl+N adds one")
        );
        assert_eq!(no_targets_hint(CoilKind::Normal).0, "no writable symbols");
    }

    #[test]
    fn a_jump_or_call_coil_offers_its_own_targets() {
        let mut plc = PlcUi::default();
        let mut main = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Ld);
        main.sections = vec![linc_plc_core::Section {
            name: "main".into(),
            rungs: vec![
                Rung::new(
                    linc_plc_core::Logic::Const { value: true },
                    Coil::new("LOOP", CoilKind::Normal),
                ),
                Rung::new(
                    linc_plc_core::Logic::Const { value: true },
                    Coil::new("", CoilKind::Jump),
                ),
            ],
        }];
        main.sections[0].rungs[0].label = "LOOP".to_string();
        plc.project.set_pous(vec![
            main,
            linc_plc_core::Pou::new("SUB", linc_plc_core::PouLanguage::Ld),
        ]);

        assert_eq!(
            coil_choices(&plc, 0, 1),
            vec![("LOOP".to_string(), "LABEL".to_string())],
            "a jump offers the section's labels"
        );

        plc.project.pous[0].sections[0].rungs[1].set_coil(Coil::new("SUB", CoilKind::Call));
        assert_eq!(
            coil_choices(&plc, 0, 1),
            vec![("SUB".to_string(), "POU".to_string())],
            "a call offers the other POUs, never the one being edited"
        );
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
        let (local, coil) = preview_scan(&plc, &cells, None);
        assert_eq!(local[0], Some(true));
        assert_eq!(local[1], Some(true));
        assert_eq!(coil, Some(true), "the coil follows the held contact");

        plc.preview = vec![("start".into(), false)];
        let (local, coil) = preview_scan(&plc, &cells, None);
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
            Some("A MW99 is not a marker word: MW0-15")
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
                label: String::new(),
                comment: String::new(),
                logic: linc_plc_core::model::Logic::Const { value: true },
                coil: Some(linc_plc_core::model::Coil::new(
                    "",
                    linc_plc_core::model::CoilKind::Normal,
                )),
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
#[cfg(test)]
mod block_face_tests {
    use super::*;
    use linc_plc_core::profile::spec::spec;

    /// A `TON` is a named two-row body: `IN`/`PT` on the left, `Q`/`ET` on the right.
    #[test]
    fn a_timer_body_is_named_and_two_rows_tall() {
        let row = spec("TON").expect("the table has TON");
        assert_eq!(row.pins.0, &["IN", "PT"]);
        assert_eq!(row.pins.1, &["Q", "ET"]);
        assert_eq!(block_height(2), 32.0);
        assert_eq!(row.shape, Shape::Block);
        assert_eq!(row.mnemonic, "TON");
    }

    /// A `CTU` is three rows, and its reset row has an input and no output, so the
    /// right side gets no name and no value — only the body's edge.
    #[test]
    fn a_counter_body_is_three_rows_tall_with_a_blank_right_side() {
        let row = spec("CTU").expect("the table has CTU");
        assert_eq!(block_height(3), 41.0);
        assert_eq!(row.pins.0, &["CU", "R", "PV"]);
        assert_eq!(row.pins.1[1], "", "the reset row has no output pin");
    }

    /// Only the power row carries conductor: the two leads are horizontal and both
    /// sit on it, so no value row has a stub.
    #[test]
    fn a_block_draws_one_line_per_side_and_both_on_the_power_row() {
        let rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(SPAN, CELL_HEIGHT));
        let body = egui::Rect::from_center_size(
            egui::pos2(rect.center().x, conductor_y(rect)),
            egui::vec2(BLOCK_W, block_height(3)),
        );
        let leads = block_leads(rect, body, 1.0);
        assert_eq!(leads.len(), 2);
        for segment in leads {
            assert_eq!(segment[0].y, segment[1].y, "a lead is horizontal");
            assert_eq!(segment[0].y, rect.top() + CONDUCTOR_Y);
        }
        // One on each side of the body.
        assert_eq!(leads[0][1].x, body.left());
        assert_eq!(leads[1][0].x, body.right());
    }

    /// A row grows only for a body that needs the label band, and `CELL_HEIGHT`
    /// itself never changes: only the stride between rows does.
    #[test]
    fn a_block_row_grows_only_for_the_label_band() {
        assert_eq!(CELL_HEIGHT, 50.0);
        assert_eq!(
            row_height(1),
            53.5,
            "a one-row body grows so its instance label clears the rim"
        );
        assert_eq!(row_height(2), 58.0);
        assert_eq!(row_height(3), 65.0);
        for rows in 1..=3 {
            assert!(row_height(rows) >= block_height(rows) + 2.0 * LABEL_BAND);
        }
    }

    /// D8: `Unknown` is visibly not `Off`, and `On` differs in both weight and
    /// colour from either.
    #[test]
    fn unknown_is_visibly_not_off() {
        let ink = Color32::from_rgb(0x20, 0x30, 0x40);
        let accent = Color32::from_rgb(0x4d, 0xa3, 0xff);
        let unknown = Style::new(ink, accent, State::Unknown).body(1.0);
        let off = Style::new(ink, accent, State::Off).body(1.0);
        let on = Style::new(ink, accent, State::On).body(1.0);
        assert_ne!(unknown.color, off.color, "unknown is dimmed, not off");
        assert_eq!(unknown.width, off.width, "weight does not claim a state");
        assert_ne!(on.width, off.width);
        assert_eq!(on.color, off.color, "the state's colour is the ink");
    }

    /// Three states, one shape: `body` and `lead` change weight and colour only.
    #[test]
    fn the_state_never_changes_the_shape() {
        let ink = Color32::from_rgb(0x20, 0x30, 0x40);
        let accent = Color32::from_rgb(0x4d, 0xa3, 0xff);
        for state in [State::Unknown, State::Off, State::On] {
            let style = Style::new(ink, accent, state);
            assert!(style.body(1.0).width >= 1.6);
            assert!(style.lead(1.0).width >= 1.2);
        }
    }

    /// D5: weight scales with the cell, so a zoomed strip keeps its proportions.
    #[test]
    fn weight_scales_with_the_cell() {
        let ink = Color32::from_rgb(0x20, 0x30, 0x40);
        let style = Style::new(ink, ink, State::Off);
        assert_eq!(style.body(2.0).width, 2.0 * style.body(1.0).width);
        assert_eq!(style.lead(2.0).width, 2.0 * style.lead(1.0).width);
    }

    /// The three states, and the wire and link painters, exist because the pin
    /// values and the conductor are drawn separately from any instruction.
    #[test]
    fn a_pin_value_reads_as_a_time_and_an_unset_one_as_a_dash() {
        assert_eq!(display_ms(4_000), "4s");
        assert_eq!(display_ms(2_400), "2.4s");
        // A sub-second preset rounds to the column's one decimal: `T#250ms` reads
        // `0.2s`. The width is fixed so a counting value does not jitter.
        assert_eq!(display_preset("T#250ms", true), "0.2s");
        assert_eq!(display_preset("T#1s", true), "1s");
        assert_eq!(display_preset("10", false), "10");
    }
}
#[cfg(test)]
mod golden_tests {
    use super::*;

    /// The picture a row draws, as text: every shape, its points and its stroke.
    ///
    /// This is the second layer, not the first. It answers "did the picture change",
    /// never "is the geometry right" — the arithmetic tests in `geometry_tests` and
    /// `block_face_tests` answer that. Reviewers rubber-stamp golden diffs, so a
    /// failure here means: look at what moved, decide whether it was meant, and only
    /// then re-record.
    fn digest(cells: Vec<Cell>, coil: Coil) -> String {
        let rung = Rung {
            cells,
            ..Rung::new(linc_plc_core::model::Logic::Const { value: true }, coil)
        };
        digest_rung(&PlcUi::default(), &rung)
    }

    /// The picture a given rung draws, for the tests that need a comment, a label
    /// or an open field rather than a cell list.
    fn digest_rung(plc: &PlcUi, rung: &Rung) -> String {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::pos2(0.0, 0.0),
                egui::vec2(640.0, 96.0),
            )),
            ..Default::default()
        };
        let output = ctx.run_ui(input, |ui| {
            let mut actions = Vec::new();
            let mut cursor = None;
            grid_rung(plc, ui, 0, 0, rung, None, None, &mut actions, &mut cursor);
        });
        let mut output = output;
        output.textures_delta.clear();
        let mut lines = Vec::new();
        for clipped in &output.shapes {
            describe(&clipped.shape, &mut lines);
        }
        lines.join("\n")
    }

    fn point(x: f32, y: f32) -> String {
        format!("({x:.2},{y:.2})")
    }

    fn describe(shape: &egui::Shape, out: &mut Vec<String>) {
        match shape {
            egui::Shape::LineSegment { points, stroke } => out.push(format!(
                "line {} {} w={:.2}",
                point(points[0].x, points[0].y),
                point(points[1].x, points[1].y),
                stroke.width
            )),
            egui::Shape::Path(path) => {
                let points: Vec<String> = path.points.iter().map(|p| point(p.x, p.y)).collect();
                out.push(format!(
                    "path {} w={:.2} {}",
                    points.len(),
                    path.stroke.width,
                    points.join(" ")
                ));
            }
            egui::Shape::Rect(rect) => out.push(format!(
                "rect {} {} w={:.2} r={:.2}",
                point(rect.rect.left(), rect.rect.top()),
                point(rect.rect.right(), rect.rect.bottom()),
                rect.stroke.width,
                rect.corner_radius.nw
            )),
            egui::Shape::Text(text) => out.push(format!(
                "text {:?} at {}",
                text.galley.text(),
                point(text.pos.x, text.pos.y)
            )),
            other => out.push(format!("{other:?}").chars().take(24).collect()),
        }
    }

    const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/plc-ton-row.txt");

    fn check(drawn: String) {
        if std::env::var("UPDATE_GOLDEN").is_ok() {
            std::fs::write(GOLDEN, &drawn).expect("the golden file is writable");
            return;
        }
        let golden = std::fs::read_to_string(GOLDEN)
            .expect("tests/golden/plc-ton-row.txt is committed; re-record with UPDATE_GOLDEN=1");
        assert_eq!(
            drawn, golden,
            "the rendered row changed; if that was intended, re-record with UPDATE_GOLDEN=1"
        );
    }

    /// The x of the first text shape whose string is exactly `wanted`.
    fn text_x(drawn: &str, wanted: &str) -> f32 {
        let needle = format!("text \"{wanted}\" at (");
        let line = drawn
            .lines()
            .find(|line| line.starts_with(&needle))
            .unwrap_or_else(|| panic!("no text {wanted:?} in:\n{drawn}"));
        let rest = &line[needle.len()..];
        rest.split(',').next().unwrap().parse().unwrap()
    }

    /// The right edge of the circuit: the ten cell spans, coil included. A rung's
    /// comment sits beyond it, in the margin.
    fn conductor_right() -> f32 {
        (POSITIONS + 1) as f32 * CELL_WIDTH
    }

    /// A rung comment is plain text in the right margin, level with the conductor:
    /// no `// ` marker, and nothing printed over the rung.
    #[test]
    fn the_rung_comment_draws_plain_beyond_the_coil() {
        let rung = Rung {
            comment: "// seal in".to_string(),
            ..Rung::new(
                linc_plc_core::model::Logic::Const { value: true },
                Coil::new("run", linc_plc_core::model::CoilKind::Normal),
            )
        };
        let drawn = digest_rung(&PlcUi::default(), &rung);
        assert!(
            drawn
                .lines()
                .any(|line| line.starts_with("text \"seal in\" at (")),
            "the comment draws without its marker:\n{drawn}"
        );
        assert!(
            !drawn.contains("//"),
            "no `//` anywhere in the picture:\n{drawn}"
        );
        assert!(
            text_x(&drawn, "seal in") > conductor_right(),
            "the comment is beyond the coil, in the margin:\n{drawn}"
        );
    }

    /// While the field is open it draws in the margin, where the comment lives,
    /// with a caret — not in a dialog and not in the label band.
    #[test]
    fn the_comment_field_draws_in_the_margin() {
        let mut plc = PlcUi::default();
        plc.rung_comment_edit = Some((0, 0));
        plc.rung_comment_buffer = "typing".to_string();
        let rung = Rung {
            comment: "old text".to_string(),
            ..Rung::new(
                linc_plc_core::model::Logic::Const { value: true },
                Coil::new("run", linc_plc_core::model::CoilKind::Normal),
            )
        };
        let drawn = digest_rung(&plc, &rung);
        assert!(
            drawn
                .lines()
                .any(|line| line.starts_with("text \"typing_\" at (")),
            "the field carries the buffer and its caret:\n{drawn}"
        );
        assert!(
            !drawn.contains("old text"),
            "the stored comment is replaced while editing:\n{drawn}"
        );
        assert!(
            text_x(&drawn, "typing_") > conductor_right(),
            "the field is where the comment will be:\n{drawn}"
        );
    }

    /// A `TON` row: the body, its two rows and the values outside it.
    #[test]
    fn the_timer_row_is_the_picture_we_agreed_to() {
        let cells = vec![
            Cell::Contact {
                var: "start".into(),
                negated: false,
            },
            Cell::Block {
                block: "TON".into(),
                instance: "TON0".into(),
                preset: "T#2s".into(),
            },
            Cell::Wire,
        ];
        check(digest(
            cells,
            Coil::new("lamp", linc_plc_core::model::CoilKind::Normal),
        ));
    }

    /// A coil row: two half circles, the letter inside, and the negation slash.
    #[test]
    fn the_coil_row_is_the_picture_we_agreed_to() {
        let cells = vec![Cell::Contact {
            var: "a".into(),
            negated: true,
        }];
        let drawn = digest(cells, Coil::new("y", linc_plc_core::model::CoilKind::Set));
        if std::env::var("UPDATE_GOLDEN").is_ok() {
            let path = GOLDEN.replace("plc-ton-row", "plc-coil-row");
            std::fs::write(path, &drawn).expect("the golden file is writable");
            return;
        }
        let path = GOLDEN.replace("plc-ton-row", "plc-coil-row");
        let golden = std::fs::read_to_string(&path).expect("the coil golden is committed");
        assert_eq!(drawn, golden, "the rendered coil changed");
    }
}

#[cfg(test)]
mod rung_comment_tests {
    use super::*;

    fn keys(plc: &mut PlcUi, events: Vec<egui::Event>) -> (bool, Vec<Action>) {
        let ctx = egui::Context::default();
        let mut actions = Vec::new();
        let mut owned = false;
        let input = egui::RawInput {
            events,
            ..Default::default()
        };
        let mut output = ctx.run_pass(input, |ctx| {
            egui::Area::new(egui::Id::unique("rung-comment"))
                .fixed_pos(egui::pos2(0.0, 0.0))
                .show(ctx, |ui| {
                    owned = rung_text_keys(plc, ui, &mut actions);
                });
        });
        output.textures_delta.clear();
        (owned, actions)
    }

    fn key(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn ld_pou() -> PlcUi {
        let mut plc = PlcUi::default();
        plc.add_pou(PouLanguage::Ld);
        // A new POU has no sections; the field needs a rung to sit above.
        let mut pous = plc.project.all_pous();
        pous[plc.pou].sections = vec![linc_plc_core::model::Section {
            name: "main".to_string(),
            rungs: vec![Rung::new(
                linc_plc_core::model::Logic::Const { value: true },
                Coil::new("run", CoilKind::Normal),
            )],
        }];
        plc.project.set_pous(pous);
        plc.page = PlcPage::Edit;
        plc.cell_cursor = Some((0, 0, 0));
        plc
    }

    /// `Shift+C` opens the rung's comment field, typing reaches it, Enter stores
    /// what was typed and Escape stores nothing.
    #[test]
    fn shift_c_opens_the_rung_comment_field_and_enter_stores_it() {
        let mut plc = ld_pou();
        let (owned, actions) = keys(&mut plc, vec![key(egui::Key::C, egui::Modifiers::SHIFT)]);
        assert!(owned, "the field owns the keyboard once it opens");
        assert_eq!(plc.rung_comment_edit, Some((0, 0)));
        assert!(actions.is_empty(), "opening stores nothing");
        assert!(plc.text_editing, "the grid's keys stay out while typing");

        let (owned, actions) = keys(&mut plc, vec![egui::Event::Text("seal in".to_string())]);
        assert!(owned);
        assert_eq!(plc.rung_comment_buffer, "seal in");
        assert!(actions.is_empty(), "typing stores nothing yet");

        let (owned, actions) = keys(&mut plc, vec![key(egui::Key::Enter, egui::Modifiers::NONE)]);
        assert!(owned);
        assert_eq!(plc.rung_comment_edit, None);
        assert!(!plc.text_editing, "Enter gives the keyboard back");
        assert_eq!(actions.len(), 1, "one store action: {:?}", actions.len());
        match &actions[0] {
            Action::SetRungComment {
                section,
                rung,
                comment,
            } => {
                assert_eq!((*section, *rung), (0, 0));
                assert_eq!(comment, "seal in");
            }
            other => panic!("unexpected action {other:?}"),
        }
    }

    /// Reopening prefills the field with the stored comment, and Escape drops the
    /// edit without an action.
    #[test]
    fn the_field_prefills_and_escape_cancels() {
        let mut plc = ld_pou();
        let mut pous = plc.project.all_pous();
        pous[plc.pou].sections[0].rungs[0].comment = "// seal in".to_string();
        plc.project.set_pous(pous);

        let (_, _) = keys(&mut plc, vec![key(egui::Key::C, egui::Modifiers::SHIFT)]);
        assert_eq!(
            plc.rung_comment_buffer, "seal in",
            "the marker is not part of the text"
        );

        let (owned, actions) = keys(
            &mut plc,
            vec![key(egui::Key::Escape, egui::Modifiers::NONE)],
        );
        assert!(owned);
        assert!(actions.is_empty(), "Escape stores nothing");
        assert_eq!(plc.rung_comment_edit, None);
        assert!(!plc.text_editing);
    }

    /// `C` alone is a contact, not a comment: the field must not open.
    #[test]
    fn a_plain_c_does_not_open_the_comment_field() {
        let mut plc = ld_pou();
        let (owned, actions) = keys(&mut plc, vec![key(egui::Key::C, egui::Modifiers::NONE)]);
        assert!(!owned);
        assert!(actions.is_empty());
        assert_eq!(plc.rung_comment_edit, None);
    }
}

#[cfg(test)]
mod coil_shape_tests {
    use super::*;

    /// A coil is two half circles split on the vertical axis: full arcs at the
    /// sides, where the leads enter, and the openings at the top and bottom where
    /// the two arcs would otherwise meet and weld into a closed ring.
    #[test]
    fn the_coil_is_two_open_arcs_not_a_ring() {
        let cx = 100.0;
        let y = 50.0;
        let (left, right) = coil_arcs(cx, y, 1.0);
        assert!(left.len() > 8, "an arc is a polyline, not a chord");
        assert!(right.len() > 8);

        // The sides are full: each arc passes through its own lead point, exactly
        // on the conductor at the coil's radius, so a lead ends on the arc.
        let nearest = |arc: &[egui::Pos2], target: egui::Pos2| {
            arc.iter()
                .map(|point| (*point - target).length())
                .fold(f32::MAX, f32::min)
        };
        assert!(
            nearest(&left, egui::pos2(cx - COIL_R, y)) < 0.01,
            "the left arc crosses the conductor where the left lead ends"
        );
        assert!(
            nearest(&right, egui::pos2(cx + COIL_R, y)) < 0.01,
            "the right arc crosses the conductor where the right lead ends"
        );

        // The openings are at the top and bottom junction: every arc end sits on
        // the vertical axis, a radius above or below the conductor, and short of
        // the axis-crossing point by the trim.
        let trim_x = COIL_R * COIL_OPEN.sin();
        for arc in [&left, &right] {
            for point in [arc[0], arc[arc.len() - 1]] {
                let dx = (point.x - cx).abs();
                let dy = (point.y - y).abs();
                assert!(
                    dx <= trim_x + 0.01,
                    "an arc ends away from the junctions at {point:?}: the opening is at a side"
                );
                assert!(
                    dx > 1.0,
                    "an arc ends on the vertical axis at {point:?}: no opening at the junction"
                );
                assert!(
                    dy > COIL_R * 0.5,
                    "an arc end at {point:?} is not a half circle"
                );
            }
        }

        // Neither arc passes through the top or bottom point, where the pair would
        // join and read as one closed circle.
        for arc in [&left, &right] {
            for point in arc {
                let dx = (point.x - cx).abs();
                let dy = (point.y - y).abs();
                assert!(
                    !(dx < 1.0 && dy > COIL_R - 0.5),
                    "a point sits on the top or bottom junction: {point:?}"
                );
            }
        }

        // The drawn length is short of a full circle in proportion to the trim,
        // which a welded ring would not be.
        let drawn: f32 = [&left, &right]
            .iter()
            .flat_map(|arc| arc.windows(2))
            .map(|pair| (pair[1] - pair[0]).length())
            .sum();
        let circle = 2.0 * std::f32::consts::PI * COIL_R;
        assert!(
            drawn < circle * 0.95,
            "{drawn} of {circle}: the arcs cover the circle"
        );
        assert_eq!(COIL_SPAN, std::f32::consts::PI);
    }
}

#[cfg(test)]
mod preview_colour_tests {
    use super::*;

    fn plc_with(vars: &[(&str, bool)]) -> PlcUi {
        let mut plc = PlcUi::default();
        plc.preview = vec![("a".to_string(), true)];
        plc.vars = vars
            .iter()
            .map(|(name, value)| linc_plc_core::VarValue {
                name: (*name).to_string(),
                value: *value,
                kind: linc_plc_core::SymbolKind::Input,
                forced: false,
            })
            .collect();
        plc
    }

    /// A contact lights on its own bit; a wire lights on the rung's output. The
    /// old rule gave both the power that had reached their column, so a toggled
    /// contact lit every wire to its right even when the rung's coil stayed off.
    #[test]
    fn a_contact_lights_by_its_bit_and_a_wire_by_the_rung_output() {
        let plc = plc_with(&[("a", true), ("b", false)]);
        let cells = vec![
            Cell::Wire,
            Cell::Contact {
                var: "a".to_string(),
                negated: false,
            },
            Cell::Contact {
                var: "b".to_string(),
                negated: false,
            },
        ];
        let local = Some(preview_scan(&plc, &cells, None));
        let scan = preview_scan(&plc, &cells, None);
        let power = rung_power(&plc, &cells);
        assert_eq!(
            scan.0[0],
            Some(true),
            "the scan still carries power up to the wire's column"
        );
        assert_eq!(
            live_at(&plc, &local, power, &cells[1]),
            Some(true),
            "the toggled contact shows its own bit"
        );
        assert_eq!(
            live_at(&plc, &local, power, &cells[2]),
            Some(false),
            "the untoggled contact stays dark"
        );
        assert_eq!(
            live_at(&plc, &local, power, &cells[0]),
            Some(false),
            "a AND b is false, so the wire is not lit"
        );
    }

    /// A live ladder's wires show the rung's own output from the snapshot: before
    /// this, a wire had a state only while a contact was held in the preview, so
    /// every wire in a running program stayed neutral.
    #[test]
    fn a_wire_follows_the_rung_from_the_snapshot() {
        let plc = plc_with(&[("a", true), ("b", false)]);
        let on = [Cell::Contact {
            var: "a".to_string(),
            negated: false,
        }];
        let off = [
            Cell::Contact {
                var: "a".to_string(),
                negated: false,
            },
            Cell::Contact {
                var: "b".to_string(),
                negated: false,
            },
        ];
        assert_eq!(rung_power(&plc, &on), Some(true));
        assert_eq!(rung_power(&plc, &off), Some(false));
        assert_eq!(
            live_at(&plc, &None, rung_power(&plc, &on), &Cell::Wire),
            Some(true),
            "the wire carries the rung's output with no preview held"
        );
        // A computed cell has no local evaluator, so the rung is unknown; but one
        // false condition still settles it.
        let unknown = [Cell::Expr {
            text: "bogus".to_string(),
        }];
        assert_eq!(rung_power(&plc, &unknown), None, "unknown, not off");
        let mut mixed = unknown.to_vec();
        mixed.push(Cell::Contact {
            var: "b".to_string(),
            negated: false,
        });
        assert_eq!(
            rung_power(&plc, &mixed),
            Some(false),
            "a false condition settles the rung whatever else is unknown"
        );
    }
}

#[cfg(test)]
mod link_tests {
    use super::*;
    use linc_plc_core::model::Logic;

    fn contact(var: &str, negated: bool) -> Cell {
        Cell::Contact {
            var: var.to_string(),
            negated,
        }
    }

    fn plc_with(vars: &[(&str, bool)]) -> PlcUi {
        let mut plc = PlcUi::default();
        plc.vars = vars
            .iter()
            .map(|(name, value)| linc_plc_core::VarValue {
                name: (*name).to_string(),
                value: *value,
                kind: linc_plc_core::SymbolKind::Input,
                forced: false,
            })
            .collect();
        plc
    }

    fn rung(coil: &str, cells: Vec<Cell>) -> Rung {
        let mut rung = Rung::new(
            Logic::Const { value: true },
            Coil::new(coil, CoilKind::Normal),
        );
        rung.cells = cells;
        rung
    }

    /// A latching pair: the top row has a junction at column 2, the branch row
    /// below it, and both rows end in their own coil.
    fn pair(plc: &PlcUi) -> (Option<bool>, Option<bool>, Option<bool>) {
        let rows = vec![
            rung(
                "q0_0",
                vec![contact("i0_0", false), contact("i0_1", false), Cell::Link]
                    .into_iter()
                    .chain(std::iter::repeat_n(Cell::Wire, 6))
                    .collect(),
            ),
            rung(
                "q0_1",
                vec![contact("i0_2", false)]
                    .into_iter()
                    .chain(std::iter::repeat_n(Cell::Wire, 8))
                    .collect(),
            ),
        ];
        let nodes = group_nodes(plc, &rows);
        let top = preview_scan(&plc_top(plc), &rows[0].cells, nodes[0]).1;
        let bottom = preview_scan(plc, &rows[1].cells, nodes[1]).1;
        (nodes[0], top, bottom)
    }

    fn plc_top(plc: &PlcUi) -> PlcUi {
        let mut copy = PlcUi::default();
        copy.vars = plc.vars.clone();
        copy
    }

    /// The link is a shared node: power from either row's input feeds both rows
    /// and both coils. Only the upward direction worked before — the junction fed
    /// the top row's continuation and the branch row got nothing.
    #[test]
    fn a_link_feeds_both_rows_in_the_preview() {
        // Top input held: the bottom row and its coil must light too.
        let top_held = plc_with(&[("i0_0", true), ("i0_1", true), ("i0_2", false)]);
        let (node, top, bottom) = pair(&top_held);
        assert_eq!(node, Some(true));
        assert_eq!(top, Some(true));
        assert_eq!(bottom, Some(true), "power flows down through the node");

        // Bottom input held: the top row lights, as it did before.
        let bottom_held = plc_with(&[("i0_0", false), ("i0_1", false), ("i0_2", true)]);
        let (node, top, bottom) = pair(&bottom_held);
        assert_eq!(node, Some(true));
        assert_eq!(top, Some(true), "power flows up through the node");
        assert_eq!(bottom, Some(true));

        // Neither: both dark.
        let idle = plc_with(&[("i0_0", false), ("i0_1", false), ("i0_2", false)]);
        assert_eq!(pair(&idle), (Some(false), Some(false), Some(false)));

        // Both: both lit.
        let both = plc_with(&[("i0_0", true), ("i0_1", true), ("i0_2", true)]);
        assert_eq!(pair(&both), (Some(true), Some(true), Some(true)));
    }

    /// Mixed contacts on each row: the top row's entry is a series, so a false
    /// contact there makes its entry false, and the branch alone decides the node.
    #[test]
    fn mixed_contacts_on_each_row_still_share_one_node() {
        // Top entry i0_0 AND i0_1 is false (i0_1 open), branch i0_2 is true.
        let plc = plc_with(&[("i0_0", true), ("i0_1", false), ("i0_2", true)]);
        let (node, top, bottom) = pair(&plc);
        assert_eq!(node, Some(true), "the branch keeps the node live");
        assert_eq!(top, Some(true), "the node feeds the top row's continuation");
        assert_eq!(bottom, Some(true));

        // Both entries false: the node is false and nothing lights.
        let plc = plc_with(&[("i0_0", true), ("i0_1", false), ("i0_2", false)]);
        assert_eq!(pair(&plc), (Some(false), Some(false), Some(false)));

        // The top entry true through both contacts, branch false: both light.
        let plc = plc_with(&[("i0_0", true), ("i0_1", true), ("i0_2", false)]);
        assert_eq!(pair(&plc), (Some(true), Some(true), Some(true)));
    }

    /// The junction ends on the next row's conductor, not a half cell down: a row
    /// that grew for a comment band carries its conductor down with it.
    #[test]
    fn a_link_drops_to_the_next_rows_conductor() {
        assert_eq!(link_drop(CELL_HEIGHT), CELL_HEIGHT - CONDUCTOR_Y);
        assert_eq!(
            link_drop(CELL_HEIGHT + LABEL_ROW_BAND),
            CELL_HEIGHT + LABEL_ROW_BAND - CONDUCTOR_Y,
            "a banded row's conductor sits one band lower"
        );
        assert_eq!(
            link_drop(65.0),
            40.0,
            "a three-row block body is 65 tall, so its conductor is 40 below the row edge"
        );
        assert_eq!(link_drop(0.0), 0.0, "never negative");
    }

    /// The height `grid_rung` hands the row above: a labelled row is one band
    /// taller, so a link above it must reach further. A comment no longer changes
    /// the height — it sits in the right margin, level with the conductor.
    #[test]
    fn a_labelled_row_reports_its_full_height() {
        let mut rung = Rung::new(
            Logic::Const { value: true },
            Coil::new("run", CoilKind::Normal),
        );
        rung.cells = vec![Cell::Wire; linc_plc_core::grid::POSITIONS];
        assert_eq!(strip_height_of(&rung), CELL_HEIGHT);
        rung.comment = "branch of seal".to_string();
        assert_eq!(
            strip_height_of(&rung),
            CELL_HEIGHT,
            "a comment lives in the margin, not in a band"
        );
        rung.label = "SEAL".to_string();
        assert_eq!(strip_height_of(&rung), CELL_HEIGHT + LABEL_ROW_BAND);
    }
}

#[cfg(test)]
mod block_value_tests {
    use super::*;

    fn plc_with_words(words: &[(&str, i32)]) -> PlcUi {
        let mut plc = PlcUi::default();
        plc.words = words
            .iter()
            .map(|(name, value)| linc_plc_core::WordValue {
                name: (*name).to_string(),
                value: *value,
            })
            .collect();
        plc
    }

    /// A counter's CV is the count itself; a timer's ET is milliseconds turned into
    /// seconds. `CV` printed through the timer's formatter showed `0.0s` for a
    /// counter that had counted.
    #[test]
    fn a_counter_shows_its_count_and_a_timer_its_time() {
        let plc = plc_with_words(&[("CTU0.CV", 3), ("TON0.ET", 2400)]);
        assert_eq!(
            pin_values(&plc, "CTU0", "5", "PV", "CV"),
            ("5".to_string(), "3".to_string()),
            "the count, dimensionless"
        );
        assert_eq!(
            pin_values(&plc, "TON0", "T#3000ms", "PT", "ET"),
            ("3s".to_string(), "2.4s".to_string()),
            "the elapsed time in seconds"
        );
        // An unanswered pin is `-`, never `0`, so a snapshot that cannot answer
        // never reads as a count of nothing.
        assert_eq!(pin_values(&plc, "CTU1", "5", "PV", "CV").1, "-");
        let plc = plc_with_words(&[("R_TRIG0.CLK", 0)]);
        assert_eq!(
            pin_values(&plc, "R_TRIG0", "", "CLK", "Q"),
            (String::new(), String::new()),
            "a bit block carries no value text"
        );
    }

    /// The pin names are the family's own markers: `CV`/`PV` on counters, `ET`/`PT`
    /// on timers. The formatter keys on them, so this is what keeps a value and its
    /// unit together.
    #[test]
    fn the_value_pins_belong_to_their_family() {
        for spec in linc_plc_core::profile::spec::SPECS {
            let linc_plc_core::profile::spec::Family::Fb(name) = spec.family else {
                continue;
            };
            // The family's own markers, read from the pins the table declares.
            let _ = name;
            let pins: Vec<&str> = spec
                .pins
                .0
                .iter()
                .chain(spec.pins.1.iter())
                .copied()
                .collect();
            let counter = spec.mnemonic.starts_with("CT");
            let timer = matches!(spec.mnemonic, "TON" | "TOF" | "TP");
            assert_eq!(
                pins.contains(&"CV"),
                counter,
                "{}: CV belongs to counters only",
                spec.mnemonic
            );
            assert_eq!(
                pins.contains(&"ET"),
                timer,
                "{}: ET belongs to timers only",
                spec.mnemonic
            );
        }
    }

    /// The count is live: after a scan run the CV the block shows is the count the
    /// VM holds, updated per scan like a timer's ET.
    #[test]
    fn a_counter_counts_up_in_a_scan_run() {
        use linc_plc_core::grid::Cell;
        use linc_plc_core::{Coil, CoilKind, IoDirection, IoPoint, Logic, Project, Rung, Section};
        let mut project = Project::new("counter");
        project.io = vec![IoPoint::new("pulse", IoDirection::In)];
        let mut rung = Rung::new(
            Logic::Const { value: true },
            Coil::new("", CoilKind::Normal),
        );
        rung.coil = None;
        rung.cells = vec![
            Cell::Contact {
                var: "pulse".to_string(),
                negated: false,
            },
            Cell::Block {
                block: "CTU".to_string(),
                instance: "CTU0".to_string(),
                preset: "100".to_string(),
            },
        ];
        project.sections = vec![Section {
            name: "main".to_string(),
            rungs: vec![rung],
        }];
        project.set_pous(project.all_pous());
        let compiled = linc_plc_core::compile(&project).expect("compiles");
        let mut vm = linc_plc_core::Vm::new(&compiled);

        // Four pulses: a rising edge on each true scan after a false one.
        for _ in 0..4 {
            vm.scan(&[true]);
            vm.scan(&[false]);
        }
        // A block's members are published with the banks, not as declared symbols:
        // the snapshot the page reads carries `CTU0.CV` this way.
        let count = vm
            .word_snapshot()
            .iter()
            .find(|word| word.name == "CTU0.CV")
            .map(|word| word.value)
            .expect("CV is published with the words");
        assert_eq!(count, 4, "the counter counted the four pulses");

        let mut plc = PlcUi::default();
        plc.words = vec![linc_plc_core::WordValue {
            name: "CTU0.CV".to_string(),
            value: count,
        }];
        assert_eq!(
            pin_values(&plc, "CTU0", "100", "PV", "CV"),
            ("100".to_string(), "4".to_string()),
            "the face shows the live count, with no unit"
        );
    }
}

#[cfg(test)]
mod block_light_tests {
    use super::*;

    fn plc_with_bits(bits: &[(&str, bool)]) -> PlcUi {
        let mut plc = PlcUi::default();
        plc.vars = bits
            .iter()
            .map(|(name, value)| linc_plc_core::VarValue {
                name: (*name).to_string(),
                value: *value,
                kind: linc_plc_core::SymbolKind::Output,
                forced: false,
            })
            .collect();
        plc
    }

    /// A block lights one pin at a time. Timing: the power pin and `IN` are on, so
    /// the input lead and the `IN` label light; `Q` is off, so the frame, the `Q`
    /// label and the output lead stay dark. Before this the whole box went green
    /// from the rung's condition, and the `Q` row never lit at all.
    #[test]
    fn a_timer_lights_its_input_pin_and_not_its_output() {
        let plc = plc_with_bits(&[("TON0.IN", true), ("TON0.Q", false), ("TON0.TT", true)]);
        let (rows, q, in_power) = block_state(&plc, "TON0", "T#2s", &["IN", "PT"], &["Q", "ET"]);
        assert!(in_power, "the power pin is on");
        assert!(!q, "the output is not on");
        assert!(rows[0].ins_lit, "IN lights from its own bit");
        assert!(!rows[0].out_lit, "Q stays dark while the timer runs");
        assert_eq!(rows[1].ins.as_deref(), Some("PT"));

        // Done: Q on, TT off, IN still on. The frame and the Q label light; the
        // input is still lit because it is still asserted.
        let plc = plc_with_bits(&[("TON0.IN", true), ("TON0.Q", true), ("TON0.TT", false)]);
        let (rows, q, in_power) = block_state(&plc, "TON0", "T#2s", &["IN", "PT"], &["Q", "ET"]);
        assert!(q, "the frame follows Q");
        assert!(in_power);
        assert!(rows[0].out_lit, "the Q label lights");
        assert!(rows[0].ins_lit);

        // Idle: nothing lights, and the box is drawn off rather than unknown.
        let plc = plc_with_bits(&[("TON0.IN", false), ("TON0.Q", false)]);
        let (rows, q, in_power) = block_state(&plc, "TON0", "T#2s", &["IN", "PT"], &["Q", "ET"]);
        assert!(!q && !in_power);
        assert!(!rows[0].ins_lit && !rows[0].out_lit);
    }

    /// A counter's three rows answer for themselves: `CU` from the power pin, `R`
    /// from the reset pin, `Q` from the count. The `R` row exists and has no
    /// output pin, so it must not borrow the row above.
    #[test]
    fn a_counter_lights_cu_r_and_q_on_their_own_rows() {
        let plc = plc_with_bits(&[("CTU0.CU", true), ("CTU0.R", true), ("CTU0.Q", false)]);
        let (rows, q, in_power) =
            block_state(&plc, "CTU0", "5", &["CU", "R", "PV"], &["Q", "", "CV"]);
        assert!(in_power, "CU is the power pin");
        assert!(!q);
        assert_eq!(rows.len(), 3, "three rows, one per pin pair");
        assert!(rows[0].ins_lit, "CU lights");
        assert!(rows[1].ins_lit, "R lights from its own bit");
        assert!(rows[2].ins.is_some(), "PV keeps its row");
        assert!(!rows[2].ins_lit, "a word pin never lights");
        assert!(!rows[0].out_lit, "Q is off");
    }

    /// A pin the snapshot does not name stays dark instead of borrowing a
    /// neighbour's state: a block whose rows cannot be answered is grey.
    #[test]
    fn an_unpublished_pin_stays_dark() {
        let plc = plc_with_bits(&[("TON0.Q", true)]);
        let (rows, q, in_power) = block_state(&plc, "TON0", "T#2s", &["IN", "PT"], &["Q", "ET"]);
        assert!(q);
        assert!(!in_power, "no IN bit, no lit lead");
        assert!(!rows[0].ins_lit, "the IN label does not borrow Q");
        assert!(rows[0].out_lit, "each pin answers for itself");
    }

    /// The reset rung: `reset_count` closes onto a member-write coil `CTU0.R`,
    /// which has no readable bit — the coil shows the power that reached it, so
    /// the rung lights when its contact does.
    #[test]
    fn a_member_write_coil_lights_with_the_power_it_carries() {
        let plc = plc_with_bits(&[("reset_count", true)]);
        let cells = [Cell::Contact {
            var: "reset_count".to_string(),
            negated: false,
        }];
        let delivered = rung_power(&plc, &cells);
        assert_eq!(delivered, Some(true), "the rung's power");
        assert_eq!(
            coil_live(&plc, "CTU0.R", delivered),
            Some(true),
            "the reset coil lights although R is not a published bit"
        );
        // A coil whose target *is* a bit answers with its own value: the write is
        // what the coil stores, not the power alone.
        assert_eq!(
            coil_live(&plc, "lamp", Some(false)),
            None,
            "an unknown bit is not invented from the rung"
        );
        let plc = plc_with_bits(&[("lamp", false)]);
        assert_eq!(coil_live(&plc, "lamp", Some(true)), Some(false));
    }
}

#[cfg(test)]
mod operand_member_tests {
    use super::*;
    use linc_plc_core::model::{Logic, Pou, PouLanguage, Project, Rung, Section};

    /// A project with a timer, a counter and an edge block, one rung each.
    fn plc_with_blocks() -> PlcUi {
        let rung = |block: &str, instance: &str, preset: &str| Rung {
            label: String::new(),
            comment: String::new(),
            logic: Logic::Const { value: true },
            coil: None,
            cells: vec![
                Cell::Block {
                    block: block.to_string(),
                    instance: instance.to_string(),
                    preset: preset.to_string(),
                },
                Cell::Wire,
            ],
        };
        let mut pou = Pou::new("main", PouLanguage::Ld);
        pou.sections = vec![Section {
            name: String::new(),
            rungs: vec![
                rung("TON", "TON0", "T#2s"),
                rung("CTU", "CTU0", "5"),
                rung("R_TRIG", "RT0", ""),
            ],
        }];
        let mut plc = PlcUi::default();
        plc.project = Project::new("members");
        plc.project.pous = vec![pou];
        plc.pou = 0;
        plc
    }

    /// A contact can name a block's bit members, which it could not before: the
    /// list held only the project's I/O and variables, so `TON0.Q` and `TON0.TT`
    /// had to be typed. The members are listed per instance and carry the family
    /// in their kind, so typing the instance narrows to its own bits.
    #[test]
    fn the_contact_list_offers_a_blocks_bit_members() {
        let plc = plc_with_blocks();
        let names = operand_choices(&plc)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        for expected in ["TON0.Q", "TON0.TT", "CTU0.Q", "RT0.Q"] {
            assert!(
                names.iter().any(|name| name == expected),
                "{expected} is missing from {names:?}"
            );
        }
        // A timer has no `TT`; an edge block has none and no word member either.
        assert!(!names.iter().any(|name| name == "CTU0.TT"));
        assert!(!names.iter().any(|name| name == "RT0.TT"));
        // A word member is not a contact operand: the compare window reads those,
        // and the picker must not offer what the validator refuses.
        assert!(!names
            .iter()
            .any(|name| name.ends_with(".CV") || name.ends_with(".ET")));
        // Grouped by instance: the timer's two rows sit together.
        let ton = names
            .iter()
            .filter(|name| name.starts_with("TON0."))
            .collect::<Vec<_>>();
        assert_eq!(ton.len(), 2, "Q and TT, no more: {ton:?}");
    }

    /// A coil writes, so its list carries the one member a coil may write: a
    /// counter's reset, the pattern `reset_count → (R) CTU0.R` uses.
    #[test]
    fn the_coil_list_offers_a_counters_reset_member() {
        let plc = plc_with_blocks();
        let names = coil_choices(&plc, 0, 0)
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>();
        assert!(names.iter().any(|name| name == "CTU0.R"), "{names:?}");
        assert!(
            !names.iter().any(|name| name == "TON0.R"),
            "a timer has no reset pin: {names:?}"
        );
        assert!(
            !names.iter().any(|name| name == "CTU0.CV"),
            "a coil writes a bit, not the count: {names:?}"
        );
    }

    /// The compare window keeps reading the value members, and now names them by
    /// the family the table gives instead of guessing from the block's spelling.
    #[test]
    fn the_compare_list_offers_the_value_member_of_each_block() {
        let plc = plc_with_blocks();
        let names = compare_choices(&plc)
            .into_iter()
            .map(|choice| choice.value)
            .collect::<Vec<_>>();
        assert!(names.iter().any(|name| name == "TON0.ET"), "{names:?}");
        assert!(names.iter().any(|name| name == "CTU0.CV"), "{names:?}");
        assert!(
            !names.iter().any(|name| name == "RT0.ET"),
            "a bit block has no value: {names:?}"
        );
    }
}
