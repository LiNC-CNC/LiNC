//! The ZX-PLC ladder grid: cells, cursor, element picker, operand list.
//!
//! The page keeps the state (`cell_cursor`, `picker`, `operand_pick`); this
//! module draws the grid and owns the keys while the editor overlay is open.
//! Element letters live inside the picker rather than on the page, so they can
//! never collide with a shell shortcut or a text field.

use egui::{Align2, Color32, FontId, RichText, Sense, Stroke, Ui, Vec2};
use linc_plc_core::grid::{Cell, POSITIONS};
use linc_plc_core::{Coil, CoilKind, PouLanguage, Rung};

use crate::screens::plc::{Action, ExprEdit, PlcPage, PlcUi};
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
    if app.plc.page != PlcPage::Edit
        || app.plc.picker.is_some()
        || app.plc.operand_pick.is_some()
        || app.plc.coil_pick.is_some()
    {
        return;
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

    let (left, right, up, down, enter, delete) = ui.input_mut(|input| {
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
        (
            input.consume_key(egui::Modifiers::CTRL, egui::Key::C),
            input.consume_key(egui::Modifiers::CTRL, egui::Key::V),
            input.consume_key(egui::Modifiers::CTRL, egui::Key::X),
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
    if cursor.2 >= POSITIONS {
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
                coil: Coil::new(var, kind),
            });
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
        let block = sections
            .get(cursor.0)
            .and_then(|section| section.rungs.get(cursor.1))
            .and_then(|rung| rung.cells.get(cursor.2))
            .map(|cell| matches!(cell, Cell::Block { .. }))
            .unwrap_or(false);
        if block {
            app.plc.block_edit = Some(cursor);
        } else {
            app.plc.picker = Some(cursor);
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
pub fn pickers(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    // One picker owns a frame's keys: the element list is drawn before the
    // operand list, and when its letter chose an item the same keystroke must
    // not also land in the operand filter.
    if element_picker(plc, ctx, actions) {
        return;
    }
    operand_picker(plc, ctx, actions);
    coil_picker(plc, ctx, actions);
    expr_editor(plc, ctx, actions);
    block_editor(plc, ctx, actions);
    xref_window(plc, ctx);
}

/// Timer and counter boxes: which of the four instances, and the preset. ZX
/// asks for both when the box is placed; here the same window edits a placed box.
fn block_editor(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let Some((section, rung, position)) = plc.block_edit else {
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
        return;
    };
    let mut close = false;
    let mut patch_instance: Option<String> = None;
    let mut patch_preset: Option<String> = None;
    egui::Window::new(format!("{block} BOX"))
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(
                RichText::new(format!("{instance}   {preset}"))
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            ui.add_space(4.0);
            ui.label(
                RichText::new("instance")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            ui.horizontal(|ui| {
                for number in 0..4 {
                    let name = format!("{block}{number}");
                    if theme::action_button(ui, &name, true).clicked() {
                        patch_instance = Some(name);
                    }
                }
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new("preset")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            ui.horizontal_wrapped(|ui| {
                let presets: &[&str] = if block.starts_with("CT") {
                    &["1", "5", "10", "50", "100", "1000"]
                } else {
                    &["T#100ms", "T#500ms", "T#1s", "T#2s", "T#5s", "T#10s"]
                };
                for value in presets {
                    if ui.button(*value).clicked() {
                        patch_preset = Some((*value).to_string());
                    }
                }
            });
            ui.add_space(4.0);
            if theme::action_button(ui, "Cancel", true).clicked() {
                close = true;
            }
        });
    if patch_instance.is_some() || patch_preset.is_some() {
        actions.push(Action::SetBlockField {
            section,
            rung,
            position,
            instance: patch_instance,
            preset: patch_preset,
        });
        close = true;
    }
    if close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        plc.block_edit = None;
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
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(true)
        .default_width(420.0)
        .show(ctx, |ui| {
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
            if theme::action_button(ui, "Close", true).clicked() {
                close = true;
            }
        });
    if close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
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

fn element_picker(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) -> bool {
    let Some((section, rung, position)) = plc.picker else {
        return false;
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
                    plc.expr_edit = Some(ExprEdit {
                        section,
                        rung,
                        position,
                        compute: false,
                        stage: 0,
                        op: "GT".to_string(),
                        left: "MW0".to_string(),
                        right: "0".to_string(),
                        target: "MW0".to_string(),
                    });
                    close = true;
                }
                (false, "compute") => {
                    plc.expr_edit = Some(ExprEdit {
                        section,
                        rung,
                        position,
                        compute: true,
                        stage: 0,
                        op: "ADD".to_string(),
                        left: "MW0".to_string(),
                        right: "1".to_string(),
                        target: "MW1".to_string(),
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
        return true;
    }
    let mut visible = true;
    egui::Window::new("ELEMENT")
        .open(&mut visible)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(
                RichText::new(if coil_position {
                    "coil position"
                } else {
                    "element position"
                })
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
            );
            ui.add_space(4.0);
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
                    ("Set coil  S", CoilKind::Set),
                    ("Reset coil  R", CoilKind::Reset),
                ] {
                    if theme::action_button(ui, label, true).clicked() {
                        actions.push(Action::SetCoil {
                            section,
                            rung,
                            coil: Coil::new(var.clone(), kind),
                        });
                        close = true;
                    }
                }
            } else {
                for (label, cell) in [
                    ("Wire  -", Cell::Wire),
                    (
                        "Contact  C",
                        Cell::Contact {
                            var: first.clone(),
                            negated: false,
                        },
                    ),
                    (
                        "Negated contact  /",
                        Cell::Contact {
                            var: first.clone(),
                            negated: true,
                        },
                    ),
                    ("Link  V", Cell::Link),
                ] {
                    if theme::action_button(ui, label, true).clicked() {
                        match cell {
                            Cell::Contact { .. } => {
                                next_operand = Some((section, rung, position, cell));
                            }
                            other => actions.push(Action::SetCell {
                                section,
                                rung,
                                position,
                                cell: other,
                            }),
                        }
                        close = true;
                    }
                }
                ui.add_space(4.0);
                ui.label(
                    RichText::new("boxes")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                for (label, block, instance, preset) in [
                    ("TON  T", "TON", "TON0", "T#1s"),
                    ("TOF  F", "TOF", "TOF0", "T#1s"),
                    ("TP   U", "TP", "TP0", "T#1s"),
                    ("CTU  K", "CTU", "CTU0", "10"),
                    ("CTD  D", "CTD", "CTD0", "10"),
                ] {
                    if theme::action_button(ui, label, true).clicked() {
                        actions.push(Action::SetCell {
                            section,
                            rung,
                            position,
                            cell: Cell::Block {
                                block: block.to_string(),
                                instance: instance.to_string(),
                                preset: preset.to_string(),
                            },
                        });
                        close = true;
                    }
                }
                ui.add_space(4.0);
                if theme::action_button(ui, "Compare  G", true).clicked() {
                    plc.expr_edit = Some(ExprEdit {
                        section,
                        rung,
                        position,
                        compute: false,
                        stage: 0,
                        op: "GT".to_string(),
                        left: "MW0".to_string(),
                        right: "0".to_string(),
                        target: "MW0".to_string(),
                    });
                    close = true;
                }
                if theme::action_button(ui, "Compute  M", true).clicked() {
                    plc.expr_edit = Some(ExprEdit {
                        section,
                        rung,
                        position,
                        compute: true,
                        stage: 0,
                        op: "ADD".to_string(),
                        left: "MW0".to_string(),
                        right: "1".to_string(),
                        target: "MW1".to_string(),
                    });
                    close = true;
                }
                {
                    let (label, reason) = ("Edge contact  P N", "edge contacts");
                    ui.add_enabled(
                        false,
                        egui::Button::new(format!("{label} — {reason} not implemented")),
                    );
                }
            }
            ui.add_space(4.0);
            if theme::action_button(ui, "Cancel", true).clicked() {
                close = true;
            }
        });
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
        return true;
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
        return true;
    }
    if let Some(pick) = next_operand {
        plc.operand_pick = Some(pick);
    }
    if close || !visible {
        plc.picker = None;
        return true;
    }
    false
}

fn operand_picker(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let Some((section, rung, position, cell)) = plc.operand_pick.clone() else {
        return;
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
    if plc.operand_filter.is_empty() && names.len() > 6 {
        plc.operand_filter.clear();
    }
    let mut close = false;
    let keys = modal_keys(ctx);
    for character in &keys.typed {
        plc.operand_filter.push(*character);
    }
    if keys.backspace {
        plc.operand_filter.pop();
    }
    if keys.escape {
        plc.operand_pick = None;
        plc.operand_filter.clear();
        return;
    }
    let mut visible = true;
    egui::Window::new("OPERAND")
        .open(&mut visible)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            if names.is_empty() {
                ui.label("No symbols declared yet. Add an input, output or variable.");
            }
            // Type to narrow, Enter takes a single match: with a dozen symbols
            // the list is long and clicking it is a coordinate game.
            // The filter is text the picker collected itself, so it shows what
            // was actually typed on the frame it arrived.
            ui.label(
                RichText::new(if plc.operand_filter.is_empty() {
                    "type to narrow, Enter picks the first match".to_string()
                } else {
                    plc.operand_filter.clone()
                })
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
            );
            let needle = plc.operand_filter.trim().to_ascii_lowercase();
            let matches = names
                .iter()
                .filter(|(name, _)| {
                    needle.is_empty() || name.to_ascii_lowercase().contains(&needle)
                })
                .collect::<Vec<_>>();
            if keys.enter {
                if let Some((name, _)) = matches.first() {
                    let chosen = with_operand(&cell, name);
                    actions.push(Action::SetCell {
                        section,
                        rung,
                        position,
                        cell: chosen,
                    });
                    close = true;
                }
            }
            for (name, kind) in matches {
                let comment = plc.symbol_comment(name).unwrap_or("");
                let label = if comment.is_empty() {
                    format!("{name}   {kind}")
                } else {
                    format!("{name}   {kind}   — {comment}")
                };
                if ui.button(label).clicked() {
                    let chosen = with_operand(&cell, name);
                    actions.push(Action::SetCell {
                        section,
                        rung,
                        position,
                        cell: chosen,
                    });
                    close = true;
                }
            }
            ui.add_space(4.0);
            if theme::action_button(ui, "Cancel", true).clicked() {
                close = true;
            }
        });
    if close || !visible {
        plc.operand_pick = None;
        plc.operand_filter.clear();
    }
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
fn coil_picker(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let Some((section, rung)) = plc.coil_pick else {
        return;
    };
    // Every symbol, not only the outputs: a coil may drive a variable, and a
    // list that filtered them out could be empty, which left Enter with nothing
    // to commit and the window open for good.
    let names = plc
        .project
        .io
        .iter()
        .map(|point| point.name.clone())
        .chain(plc.project.vars.iter().map(|var| var.name.clone()))
        .collect::<Vec<_>>();
    let mut close = false;
    let keys = modal_keys(ctx);
    for character in &keys.typed {
        plc.operand_filter.push(*character);
    }
    if keys.backspace {
        plc.operand_filter.pop();
    }
    if keys.escape {
        plc.coil_pick = None;
        plc.operand_filter.clear();
        return;
    }
    let mut visible = true;
    egui::Window::new("COIL OPERAND")
        .open(&mut visible)
        .order(egui::Order::Foreground)
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            ui.label(
                RichText::new(if plc.operand_filter.is_empty() {
                    "type to narrow, Enter picks the first match".to_string()
                } else {
                    plc.operand_filter.clone()
                })
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
            );
            let needle = plc.operand_filter.trim().to_ascii_lowercase();
            let matches = names
                .iter()
                .filter(|name| needle.is_empty() || name.to_ascii_lowercase().contains(&needle))
                .collect::<Vec<_>>();
            // The picker is modal, so Enter always takes the first match; the
            // focus test made it depend on a frame the field had already lost.
            if keys.enter {
                if let Some(name) = matches.first() {
                    actions.push(Action::SetCoilVar {
                        section,
                        rung,
                        var: (*name).clone(),
                    });
                }
                // Enter with nothing to commit still closes the window: a modal
                // that cannot be left with the keyboard is a dead end.
                close = true;
            }
            for name in matches {
                if ui.button(name).clicked() {
                    actions.push(Action::SetCoilVar {
                        section,
                        rung,
                        var: name.clone(),
                    });
                    close = true;
                }
            }
            if theme::action_button(ui, "Cancel", true).clicked() {
                close = true;
            }
        });
    if close || !visible {
        plc.coil_pick = None;
        plc.operand_filter.clear();
    }
}

/// Compare and compute are built in steps, as ZX asks: operator, then the
/// values one by one, then the destination for a compute coil.
fn expr_editor(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let Some(edit) = plc.expr_edit.clone() else {
        return;
    };
    let mut close = false;
    let mut next: Option<ExprEdit> = None;
    let mut finish: Option<Cell> = None;
    egui::Window::new(if edit.compute {
        "COMPUTE COIL  M"
    } else {
        "COMPARE CONTACT  G"
    })
    .collapsible(false)
    .resizable(false)
    .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
    .show(ctx, |ui| {
        let stage = if edit.compute {
            ["operator", "left value", "right value", "destination"][edit.stage.min(3) as usize]
        } else {
            ["operator", "left value", "right value"][edit.stage.min(2) as usize]
        };
        ui.label(
            RichText::new(format!(
                "{}   {}   {}   {}",
                stage, edit.op, edit.left, edit.right
            ))
            .monospace()
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
        );
        ui.add_space(4.0);
        match edit.stage {
            0 => {
                let operators: &[&str] = if edit.compute {
                    &["ADD", "SUB", "MUL", "DIV", "MOVE"]
                } else {
                    &["GT", "GE", "EQ", "NE", "LE", "LT"]
                };
                for op in operators {
                    if theme::action_button(ui, op, true).clicked() {
                        let mut edited = edit.clone();
                        edited.op = (*op).to_string();
                        edited.stage = 1;
                        next = Some(edited);
                    }
                }
            }
            1 | 2 => {
                let mut chosen: Option<String> = None;
                ui.horizontal_wrapped(|ui| {
                    for index in 0..16 {
                        if ui.button(format!("MW{index}")).clicked() {
                            chosen = Some(format!("MW{index}"));
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    for value in ["0", "1", "5", "10", "100", "T#1s", "T#10s"] {
                        if ui.button(value).clicked() {
                            chosen = Some(value.to_string());
                        }
                    }
                });
                ui.horizontal_wrapped(|ui| {
                    for kind in ["TON", "TOF", "TP"] {
                        for number in 0..4 {
                            let name = format!("{kind}{number}.ET");
                            if ui.button(&name).clicked() {
                                chosen = Some(name);
                            }
                        }
                    }
                    for kind in ["CTU", "CTD"] {
                        for number in 0..4 {
                            let name = format!("{kind}{number}.CV");
                            if ui.button(&name).clicked() {
                                chosen = Some(name);
                            }
                        }
                    }
                });
                if let Some(value) = chosen {
                    let mut edited = edit.clone();
                    if edited.stage == 1 {
                        edited.left = value;
                        edited.stage = 2;
                    } else {
                        edited.right = value;
                        if edited.compute {
                            edited.stage = 3;
                        } else {
                            finish = Some(Cell::Compare {
                                op: edited.op.clone(),
                                left: edited.left.clone(),
                                right: edited.right.clone(),
                            });
                        }
                    }
                    next = Some(edited);
                }
            }
            _ => {
                for index in 0..16 {
                    if ui.button(format!("MW{index} := result")).clicked() {
                        finish = Some(Cell::Compute {
                            op: edit.op.clone(),
                            target: format!("MW{index}"),
                            left: edit.left.clone(),
                            right: edit.right.clone(),
                        });
                    }
                }
            }
        }
        ui.add_space(4.0);
        if theme::action_button(ui, "Cancel", true).clicked() {
            close = true;
        }
    });
    if let Some(cell) = finish {
        let (section, rung, position) = (edit.section, edit.rung, edit.position);
        actions.push(Action::SetCell {
            section,
            rung,
            position,
            cell,
        });
        close = true;
    } else if let Some(next) = next {
        plc.expr_edit = Some(next);
    }
    if close {
        plc.expr_edit = None;
    }
    if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        plc.expr_edit = None;
    }
}

/// One grid rung: nine element positions, the rail, and the coil.
pub fn grid_rung(
    plc: &PlcUi,
    ui: &mut Ui,
    section: usize,
    rung_index: usize,
    rung: &Rung,
    actions: &mut Vec<Action>,
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
    if !rung.comment.is_empty() {
        ui.label(
            RichText::new(format!("// {}", rung.comment))
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::COMMENT),
        );
    }
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        for position in 0..POSITIONS {
            let cell = cells.get(position).cloned().unwrap_or(Cell::Wire);
            let label = grid_cell_label(plc, &cell);
            cell_box(
                plc,
                ui,
                section,
                rung_index,
                position,
                &label,
                matches!(cell, Cell::Wire),
            );
        }
        ui.label(
            RichText::new("—")
                .monospace()
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
        );
        let coil = format!("({})", plc.operand_label(&rung.coil.var));
        cell_box(
            plc,
            ui,
            section,
            rung_index,
            POSITIONS,
            &coil,
            rung.coil.var.is_empty(),
        );
    });
    ui.add_space(4.0);
}

fn cell_box(
    plc: &PlcUi,
    ui: &mut Ui,
    section: usize,
    rung: usize,
    position: usize,
    label: &str,
    quiet: bool,
) {
    let size = Vec2::new(52.0, 24.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let selected = plc.cell_cursor == Some((section, rung, position));
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, theme::PANEL_RAISED);
    let stroke = if selected {
        Stroke::new(1.6, theme::ACCENT)
    } else {
        Stroke::new(1.0, theme::C3)
    };
    painter.rect_stroke(rect, 2.0, stroke, egui::StrokeKind::Inside);
    let color: Color32 = if quiet {
        theme::MUTED
    } else if selected {
        theme::ACCENT
    } else {
        theme::FG
    };
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        label,
        FontId::monospace(theme::font_size(9.5)),
        color,
    );
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
        Cell::Edge { var, rising } => {
            let name = plc.operand_label(var);
            format!("{}{name}", if *rising { "P " } else { "N " })
        }
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
