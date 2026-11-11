//! The ZX-PLC ladder grid: cells, cursor, element picker, operand list.
//!
//! The page keeps the state (`cell_cursor`, `picker`, `operand_pick`); this
//! module draws the grid and owns the keys while the editor overlay is open.
//! Element letters live inside the picker rather than on the page, so they can
//! never collide with a shell shortcut or a text field.

use egui::{Align2, Color32, FontId, RichText, Sense, Stroke, Ui, Vec2};
use linc_plc_core::grid::{Cell, POSITIONS};
use linc_plc_core::{Coil, CoilKind, PouLanguage, Rung};

use crate::screens::plc::{Action, PlcPage, PlcUi};
use crate::shell::LincApp;
use crate::theme;

/// Keys the ladder editor owns. Consumed before any widget sees them.
pub fn editor_keys(app: &mut LincApp, ui: &Ui, actions: &mut Vec<Action>) {
    if app.plc.page != PlcPage::Edit || app.plc.picker.is_some() || app.plc.operand_pick.is_some()
    {
        return;
    }
    if ui.ctx().memory(|memory| memory.focused().is_some()) {
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
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
            input.consume_key(egui::Modifiers::NONE, egui::Key::Enter),
            input.consume_key(egui::Modifiers::NONE, egui::Key::Backspace),
        )
    });
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
    if enter {
        app.plc.picker = Some(cursor);
    }
    if delete && cursor.2 < POSITIONS {
        actions.push(Action::SetCell {
            section: cursor.0,
            rung: cursor.1,
            position: cursor.2,
            cell: Cell::Wire,
        });
    }
}

/// The element picker and, after it, the operand list.
pub fn pickers(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    element_picker(plc, ctx, actions);
    operand_picker(plc, ctx, actions);
}

fn element_picker(plc: &mut PlcUi, ctx: &egui::Context, actions: &mut Vec<Action>) {
    let Some((section, rung, position)) = plc.picker else {
        return;
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
    egui::Window::new("ELEMENT")
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
                for (label, reason) in [
                    ("Edge contact  P N", "edge contacts"),
                    ("Link  V", "parallel links"),
                    ("Timer / counter  T F U K D", "timer and counter boxes"),
                    ("Compare  G", "compare contacts"),
                    ("Compute  M", "compute coils"),
                ] {
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
    if let Some(pick) = next_operand {
        plc.operand_pick = Some(pick);
    }
    if close {
        plc.picker = None;
    }
    if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        plc.picker = None;
    }
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
    let mut close = false;
    egui::Window::new("OPERAND")
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            if names.is_empty() {
                ui.label("No symbols declared yet. Add an input, output or variable.");
            }
            for (name, kind) in &names {
                if ui.button(format!("{name}   {kind}")).clicked() {
                    let chosen = match &cell {
                        Cell::Contact { negated, .. } => Cell::Contact {
                            var: name.clone(),
                            negated: *negated,
                        },
                        other => other.clone(),
                    };
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
    if close || ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
        plc.operand_pick = None;
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
            cell_box(
                plc,
                ui,
                section,
                rung_index,
                position,
                &cell_label(&cell),
                matches!(cell, Cell::Wire),
            );
        }
        ui.label(
            RichText::new("—")
                .monospace()
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
        );
        let coil = format!("({})", rung.coil.var);
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
        Cell::Compare { left, .. } => left.clone(),
        Cell::Compute { target, .. } => target.clone(),
    }
}
