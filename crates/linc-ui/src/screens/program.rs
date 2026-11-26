//! Program page: SPLIT and CODE layouts, editable G-code, syntax-gated
//! preview and live/program toolpath layers.

use std::sync::Arc;

use egui::{text::LayoutJob, Align, Color32, FontId, Galley, RichText, TextFormat, Ui, Vec2};
use linc_core::gcode::{lex_line, TokenKind};

use crate::screens::common;
use crate::shell::LincApp;
use crate::spacing;
use crate::state::{DocumentFront, ProgramView};
use crate::theme;

fn editor_font() -> f32 {
    theme::font_size(13.0)
}

fn line_height() -> f32 {
    editor_font() * 1.36
}

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    // `Ctrl+U`: unload the program. This is also the external-link release -
    // while a program is loaded the link is held down (armed), so unloading is
    // the operator's deliberate "done cutting" action that lets it come back and
    // with it SSH and log reading. Page-local, like `S` on Tools.
    //
    // Consumed before the panes draw: a focused text edit would otherwise be free
    // to take the chord for its own purposes.
    let unload = ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::U));

    // The fault keys walk the marks, wrapping. Page-local, like Ctrl+U: they
    // belong to the file the page is showing, and they are consumed before any
    // pane draws so a focused text edit or a header widget cannot take them. Two
    // chords rather than F8 and Shift+F8: F8 is the OFFSETS page and the panel's
    // keyboard has no Shift key.
    let mut issue_step = 0isize;
    let (next, previous) = (app.prefs.keymap.fault_next, app.prefs.keymap.fault_prev);
    ui.input_mut(|input| {
        if input.consume_key(next.modifiers, next.key) {
            issue_step = 1;
        }
        if input.consume_key(previous.modifiers, previous.key) {
            issue_step = -1;
        }
    });
    if issue_step != 0 {
        app.cycle_issue(issue_step);
    }

    if !app.snapshot.probe.enabled && app.profile.variant == linc_core::model::MachineVariant::Lathe
    {
        // The lathe profile may disable the probe; Program is unaffected.
    }
    // No page header beyond the strip: the panes start under it. The view flags
    // and RUN FROM LINE live in the editor pane's header, next to the file they
    // belong to.
    //
    // The focus anchor comes first so the arrow keys reach the strip's handoff
    // control before they walk on into the panes.
    common::focus_anchor(ui, "program-page-focus");
    program_strip(app, ui);
    match app.program.view {
        ProgramView::Split => split(app, ui),
        ProgramView::Code => code(app, ui),
    }
    if unload {
        app.unload_program();
    }
}

/// The run row above the panes while a program is being prepared: the state,
/// the loaded file, the line and block the machine is on, and how far through
/// the program it is. The prepared file is named in the editor's own header,
/// not here, and the row carries no control.
fn program_strip(app: &mut LincApp, ui: &mut Ui) {
    if app.program.prepared.is_none() {
        return;
    }
    let program = &app.snapshot.program;
    let holds_program = program.line_count > 0;
    let (state, state_color) = if app.snapshot.task.running {
        ("RUNNING", theme::SUCCESS)
    } else if program.paused {
        ("PAUSED", theme::WARNING)
    } else {
        ("IDLE", theme::MUTED)
    };
    let name = if program.display_name.is_empty() {
        app.program
            .loaded()
            .path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        program.display_name.clone()
    };
    let line = format!("LN {}/{}", program.executing_line(), program.line_count);
    let block = program.current_block.clone();
    let progress = app.program.progress(program);

    theme::run_strip(ui, |ui| {
        ui.set_min_width(ui.available_width());
        // One row, and the progress bar is the item that takes what the text
        // leaves. Pinning the interact size keeps the row from taking the
        // theme's 44 px touch floor.
        const ROW_HEIGHT: f32 = 30.0;
        ui.spacing_mut().interact_size.y = ROW_HEIGHT;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(state)
                    .monospace()
                    .strong()
                    .size(theme::font_size(12.0))
                    .color(state_color),
            );
            if !holds_program {
                ui.label(
                    RichText::new("NO PROGRAM LOADED")
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                return;
            }
            let gap = ui.spacing().item_spacing.x;
            let available = ui.available_width();
            let name_room = theme::measure(ui, &name, 12.0)
                .min(240.0)
                .min((available * 0.30).max(60.0));

            let shown = theme::fit_middle(
                |candidate| theme::measure(ui, candidate, 12.0),
                &name,
                name_room,
            );
            let name_label = ui.label(
                RichText::new(shown.clone())
                    .monospace()
                    .size(theme::font_size(12.0))
                    .color(theme::FG),
            );
            if shown != name {
                name_label.on_hover_text(&name);
            }
            // The name and the program text are different kinds of field: a
            // rule between them keeps them from reading as one run-on.
            ui.add_space(gap * 0.5);
            ui.separator();
            ui.add_space(gap * 0.5);
            // The bar takes a fifth of what the rule leaves; the line and
            // block share the rest, and the block truncates when it runs out.
            let bar_width = (ui.available_width() * 0.20).max(40.0);
            let (bar, _) = ui.allocate_exact_size(Vec2::new(bar_width, 7.0), egui::Sense::hover());
            paint_run_progress(ui, bar, progress);
            ui.label(
                RichText::new(&line)
                    .monospace()
                    .size(theme::font_size(12.0))
                    .color(theme::MUTED),
            );
            ui.add_space(gap * 0.5);
            ui.separator();
            ui.add_space(gap * 0.5);
            let block_room = ui.available_width().max(60.0);
            let shown_block = fit_text_sized(ui, &block, 12.0, block_room);
            let block_label = ui.label(
                RichText::new(shown_block.clone())
                    .monospace()
                    .size(theme::font_size(12.0))
                    .color(theme::FG),
            );
            if shown_block != block {
                block_label.on_hover_text(&block);
            }
        });
    });
}

/// The run's progress, as a thin capsule: a dark track and the success-green
/// fill. The number is on the bottom bar; the row carries the shape.
fn paint_run_progress(ui: &Ui, rect: egui::Rect, fraction: f32) {
    let radius = egui::CornerRadius::same(3);
    ui.painter().rect_filled(rect, radius, theme::RUN_TRACK);
    let width = rect.width() * fraction.clamp(0.0, 1.0);
    if width > 0.5 {
        let fill = egui::Rect::from_min_size(rect.min, Vec2::new(width, rect.height()));
        ui.painter().rect_filled(fill, radius, theme::SUCCESS);
    }
}

fn split(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_size();
    let top = ui.cursor().top();
    // Reach to the bottom of the page less a small gap: `available_size` stops
    // short of it, which left the cards ending well above the hint row.
    let height = (ui.max_rect().bottom() - top - 10.0).max(220.0);
    let half = ((available.x - 8.0) * 0.5).max(300.0);
    let left_rect =
        egui::Rect::from_min_size(egui::pos2(ui.cursor().left(), top), Vec2::new(half, height));
    let right_rect = egui::Rect::from_min_size(
        egui::pos2(left_rect.right() + 8.0, top),
        Vec2::new(half, height),
    );
    // Both cards are positioned from rects rather than stacked: the graph's
    // rendered height drifts by a few pixels, and stacking let that drift move
    // the pane below it, so the two columns never ended together.
    editor_pane(app, ui, left_rect);

    let modal_rows = modal_rows(app);
    let codes = modal_rows
        .iter()
        .filter(|(key, _)| key.starts_with('G'))
        .count();
    let modal_h = common::modal_height(codes, 8);
    let modal_rect = egui::Rect::from_min_size(
        egui::pos2(right_rect.left(), right_rect.bottom() - modal_h),
        Vec2::new(half, modal_h),
    );
    let graph_rect = egui::Rect::from_min_max(
        right_rect.min,
        egui::pos2(right_rect.right(), modal_rect.top() - 4.0),
    );
    graph_card(app, ui, graph_rect, ViewKind::Full);
    let mut modal_ui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(modal_rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
    );
    common::modal_pane_in(&mut modal_ui, &modal_rows, 8, "split-modal", modal_rect);
    ui.advance_cursor_after_rect(egui::Rect::from_min_max(left_rect.min, right_rect.max));
}

/// The codes the MODAL pane lists for the document in front: the controller's
/// own modal while its program is shown, and the prepared file's parsed modal
/// while that one is. Everything the page visualises describes the document in
/// front, and the controller's modal describes the other one.
fn modal_rows(app: &LincApp) -> Vec<(String, String)> {
    match app.program.front {
        // The prepared file has no interpreter to ask, so the pane reads the
        // parsed state at the selected line rather than the state at the end.
        DocumentFront::Prepared => app
            .program
            .doc()
            .preview
            .modal_at_line(app.program.doc().selected_line)
            .lines(),
        // A running program's modal is the machine's own; an idle one is being
        // read as a file, so the selected line is the honest state.
        DocumentFront::Loaded if app.snapshot.task.running || app.snapshot.program.paused => {
            app.snapshot.program.modal.clone()
        }
        DocumentFront::Loaded => app
            .program
            .doc()
            .preview
            .modal_at_line(app.program.doc().selected_line)
            .lines(),
    }
}

fn code(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_size();
    let height = (ui.max_rect().bottom() - ui.cursor().top() - 10.0).max(220.0);
    // CODE is an editing layout: the code gets the width, and the graph column
    // is a thumbnail strip beside it.
    let editor_width = (available.x * 0.76).max(520.0);
    ui.horizontal_top(|ui| {
        let editor_rect =
            egui::Rect::from_min_size(ui.cursor().min, Vec2::new(editor_width, height));
        editor_pane(app, ui, editor_rect);
        ui.add_space(8.0);
        let width = (available.x - editor_width - 8.0).max(220.0);
        ui.allocate_ui_with_layout(
            Vec2::new(width, available.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                // Both cards are sized from what they actually render at, not
                // from guessed constants: the chrome inside `graph_card` and
                // the height of the MODAL card change with the font scale and
                // the number of active codes. The first frame uses these
                // defaults and the next corrects them.
                let modal_rows = modal_rows(app);
                let codes = modal_rows
                    .iter()
                    .filter(|(key, _)| key.starts_with('G'))
                    .count();
                let modal_h = common::modal_height(codes, 4);
                let column = egui::Rect::from_min_size(
                    egui::pos2(ui.cursor().left() + editor_width + 8.0, ui.cursor().top()),
                    Vec2::new(width, height),
                );
                let modal_rect = egui::Rect::from_min_size(
                    egui::pos2(column.left(), column.bottom() - modal_h),
                    Vec2::new(width, modal_h),
                );
                let graph_rect = egui::Rect::from_min_max(
                    column.min,
                    egui::pos2(column.right(), modal_rect.top() - 4.0),
                );
                graph_card(app, ui, graph_rect, ViewKind::MiniXy);
                let mut modal_ui = ui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(modal_rect)
                        .layout(egui::Layout::top_down(egui::Align::Min)),
                );
                common::modal_pane_in(&mut modal_ui, &modal_rows, 4, "program-modal", modal_rect);
            },
        );
    });
}

#[derive(Clone, Copy)]
enum ViewKind {
    Full,
    MiniXy,
}

/// Whether the thumbnail card's rect changed since it last drew.
///
/// The CODE layout's card is a rectangle that moves whenever the editor column
/// does, and `linc_view` only fits on request: a fit computed for the old rect
/// leaves the part hanging out of the new one. The full card is deliberately
/// not measured here — it shares the layout's rect with nothing else, and a
/// pixel of jitter refitting it would undo the operator's own zoom every frame.
fn mini_card_resized(ui: &Ui, kind: ViewKind, size: Vec2) -> bool {
    if !matches!(kind, ViewKind::MiniXy) {
        return false;
    }
    let id = egui::Id::unique("linc-program-mini-card-size").with(ui.scope_id());
    let previous: Option<Vec2> = ui.ctx().data(|data| data.get_temp(id));
    ui.ctx().data_mut(|data| data.insert_temp(id, size));
    previous.is_some_and(|previous| (previous - size).length() > 0.5)
}

/// The path the toolpath card draws and the machine layer that goes with it.
///
/// The card draws the document in front: its last valid preview, and the line
/// the operator is on in it. The machine's own layer - the live trail, the tool
/// head and the executing line - belongs only to the loaded program, so it is
/// left off while the prepared file is in front. That is the whole difference
/// between the two drawings; the card itself is one draw path.
pub(crate) struct GraphInputs {
    pub preview: linc_core::gcode::Preview,
    pub selected_line: Option<usize>,
    pub current_line: Option<usize>,
    pub live: bool,
}

pub(crate) fn graph_inputs(app: &LincApp) -> GraphInputs {
    let live = app.program.front == DocumentFront::Loaded;
    GraphInputs {
        preview: app.program.doc().last_valid_preview.clone(),
        selected_line: Some(app.program.doc().selected_line),
        current_line: if live {
            app.snapshot.program.highlight_line()
        } else {
            None
        },
        live,
    }
}

/// The time the card reports for the document it draws: the prepared file's own
/// total while it is in front, and the run's estimate to the end while the
/// loaded program is. With no estimate to give the card says nothing rather than
/// inventing a zero.
fn card_estimate(app: &LincApp) -> Option<String> {
    match app.program.front {
        DocumentFront::Prepared => {
            let doc = app.program.doc();
            let (generation, total) = doc.cycle_plan?;
            if generation != doc.preview_generation {
                return None;
            }
            let overrides = linc_core::cycletime::Overrides::from_feed_state(&app.snapshot.feed);
            let secs = total.scaled(overrides);
            Some(common::estimate_clock((secs * 1000.0).round() as u64))
        }
        DocumentFront::Loaded => app.program.cycle.remaining_ms().map(common::estimate_clock),
    }
}

fn graph_card(app: &mut LincApp, ui: &mut Ui, rect: egui::Rect, kind: ViewKind) {
    let inputs = graph_inputs(app);
    // The thumbnail draws no status row, so it must not pay for the estimate.
    let estimate = match kind {
        ViewKind::Full => card_estimate(app),
        ViewKind::MiniXy => None,
    };
    // The live trail is the machine's own tool-tip path, while the program path
    // is parsed at `program words + the work offset the program selects`. The
    // two frames differ by the tool length `G43` applies; draw the trail in the
    // path's frame so the two layers show the same cut.
    common::shell_card(ui, rect, |ui| {
        let top = ui.cursor().top();
        let header = match kind {
            ViewKind::MiniXy => {
                common::card_title(ui, "PATH");
                None
            }
            ViewKind::Full => {
                // REMAINING is the live view of the run; a prepared path has
                // nothing eaten, so the legend does not claim it.
                Some(common::toolpath_header(
                    ui,
                    inputs.live && app.graph_style.remaining,
                ))
            }
        };
        // Measured from the cursor: `min_rect` already holds the minimum size
        // the shell set, so it would report the whole card as the header and
        // shrink the graph to nothing.
        let header_height = ui.cursor().top() - top;
        // The thumbnail is not a different view, only a smaller rect. Forcing it
        // to TOP took the operator's perspective away — an oversized editor
        // shrank the card, the view jumped to plan, and nothing but the view
        // cycle brought it back. Yaw and pitch are the operator's: the card
        // draws them as they are and only re-frames.
        let view = app.program.graph_view;
        let mut zoom = app.program.graph_zoom;
        let mut pan = app.program.graph_pan;
        let mut yaw = app.program.graph_yaw;
        let mut pitch = app.program.graph_pitch;
        let mut fit = app.program.fit_requested || mini_card_resized(ui, kind, rect.size());
        let mut auto_fit = app.program.graph_auto_fit;
        let drawn = common::trajectory(
            app,
            ui,
            &inputs.preview,
            (rect.height() - 8.0 - header_height - 2.0).max(60.0),
            view,
            &mut zoom,
            &mut pan,
            &mut yaw,
            &mut pitch,
            &mut fit,
            &mut auto_fit,
            inputs.selected_line,
            inputs.current_line,
            false,
            false,
            false,
            false,
            false,
            inputs.live,
            None,
        );
        if let Some(header) = header {
            common::toolpath_status(ui, header, view.label(), drawn.span_mm, estimate.as_deref());
        }
        app.program.graph_zoom = zoom;
        app.program.graph_pan = pan;
        app.program.graph_yaw = yaw;
        app.program.graph_pitch = pitch;
        app.program.fit_requested = false;
        app.program.graph_auto_fit = auto_fit;
    });
}

/// `card_height` is the total height of the card, margins included, so the
/// pane's bottom edge lines up with whatever sits beside it.
fn editor_pane(app: &mut LincApp, ui: &mut Ui, rect: egui::Rect) {
    let editable = app.editing_allowed();
    // A read-only editor must not keep the text cursor: egui still paints (and
    // blinks) the cursor whenever the TextEdit has focus, even when it is not
    // interactive, so starting a run left the caret blinking on its old line
    // while the current-line band followed the program.
    let editor_id = egui::Id::unique("linc-program-editor");
    if !editable && ui.memory(|memory| memory.has_focus(editor_id)) {
        ui.memory_mut(|memory| memory.surrender_focus(editor_id));
    }
    // egui has no page-key support in a text edit or a scroll area, and the
    // header row's widgets would take the key first, so the editor moves its
    // selection by a screenful and scrolls to it, before anything else draws.
    let editor_focused =
        ui.memory(|memory| memory.has_focus(egui::Id::unique("linc-program-editor")));
    if editor_focused {
        let rows = (((rect.height() - 56.0) / line_height()).floor() as usize).max(2) - 1;
        let mut step = 0isize;
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp) {
                step = -(rows as isize);
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown) {
                step = rows as isize;
            }
        });
        if step != 0 {
            // The text edit owns the cursor and rewrites `selected_line` from
            // it every frame, so the page key moves the cursor itself. egui
            // then scrolls the view to it.
            let id = egui::Id::unique("linc-program-editor");
            let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
            let text = app.program.doc().editor.text();
            let cursor = state
                .cursor
                .char_range()
                .map(|range| usize::from(range.primary.index))
                .unwrap_or(0);
            let line_count = app.program.doc().editor.line_count();
            let last = line_count.saturating_sub(1);
            let mut line = 0usize;
            for (index, character) in text.chars().enumerate() {
                if index >= cursor {
                    break;
                }
                if character == '\n' {
                    line += 1;
                }
            }
            // A page covers the whole program when it is shorter than one, and
            // then the key means "start" or "end" instead of "a screenful away".
            let index = if line_count <= rows {
                if step < 0 {
                    0
                } else {
                    text.chars().count()
                }
            } else {
                let wanted = (line as isize + step).clamp(0, last as isize) as usize;
                let mut current = 0usize;
                let mut found = 0usize;
                for (index, character) in text.chars().enumerate() {
                    if current == wanted {
                        found = index;
                        break;
                    }
                    if character == '\n' {
                        current += 1;
                        found = index + 1;
                    }
                }
                found
            };
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::one(
                    egui::text::CCursor::new(index),
                )));
            state.store(ui.ctx(), id);
            let mut target_line = 0usize;
            for (position, character) in text.chars().enumerate() {
                if position >= index {
                    break;
                }
                if character == '\n' {
                    target_line += 1;
                }
            }
            app.program.doc_mut().selected_line = target_line;
            app.program.doc_mut().code_scroll_to_line = Some(target_line);
            // The operator has taken the cursor; do not drag it back to the
            // running line until the next run starts.
            app.program.follow_current = false;
        }
    }
    common::shell_card(ui, rect, |ui| {
        let content_top = ui.cursor().top();
        ui.horizontal(|ui| {
            let file = app
                .program
                .doc()
                .editor
                .path
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .or_else(|| {
                    if app.snapshot.program.display_name.is_empty() {
                        None
                    } else {
                        Some(app.snapshot.program.display_name.clone())
                    }
                })
                .unwrap_or_else(|| "UNTITLED".into());
            // The header shares its row with the editability chip and the RUNNING
            // word, and the row does not wrap: a name wider than what is left is
            // cut in the middle, so the extension stays on screen and neither chip
            // is pushed off the card.
            let state_word = if editable { "EDITABLE" } else { "READ-ONLY" };
            let preparing = app.program.prepared.is_some()
                && app.program.front == crate::state::DocumentFront::Prepared;
            // The word that shares the row with the editability chip: a running
            // program, or the mark a finished one leaves until the next run.
            let run_word = if app.snapshot.task.running {
                Some(("RUNNING", theme::SUCCESS))
            } else if app.program.completed_at.is_some() {
                Some(("FINISHED", theme::ACCENT))
            } else {
                None
            };
            let reserved = theme::measure(ui, state_word, 9.0)
                + if preparing {
                    theme::measure(ui, "PREPARING:", 9.0) + 8.0
                } else {
                    0.0
                }
                + run_word.map_or(0.0, |(word, _)| theme::measure(ui, word, 9.0))
                + if app.program.doc().editor.dirty {
                    theme::measure(ui, "*", 12.0)
                } else {
                    0.0
                }
                + 32.0;
            let room = (ui.available_width() - reserved).max(80.0);
            if preparing {
                ui.label(
                    RichText::new("PREPARING:")
                        .monospace()
                        .strong()
                        .size(theme::font_size(9.0))
                        .color(theme::ACCENT),
                );
            }
            let shown =
                theme::fit_middle(|candidate| theme::measure(ui, candidate, 12.0), &file, room);
            let name = ui.label(
                RichText::new(shown.clone())
                    .monospace()
                    .size(theme::font_size(12.0))
                    .color(theme::FG),
            );
            if shown != file {
                name.on_hover_text(&file);
            }
            if app.program.doc().editor.dirty {
                ui.label(RichText::new("*").color(theme::WARNING));
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                if let Some((word, color)) = run_word {
                    ui.label(RichText::new(word).size(theme::font_size(9.0)).color(color));
                }
                ui.label(RichText::new(state_word).size(theme::font_size(9.0)).color(
                    if editable {
                        theme::SUCCESS
                    } else {
                        theme::WARNING
                    },
                ));
            });
        });
        theme::separator(ui);
        // Sized from the card's rect, not from `available_height`: a frame does
        // not bound what it gives its content, so the editor asked for more
        // than the card had and pushed the card past the column beside it.
        const FOOTER_HEIGHT: f32 = 22.0;
        let header_height = ui.cursor().top() - content_top;
        let editor_height = (rect.height() - 8.0 - header_height - FOOTER_HEIGHT - 4.0).max(80.0);
        let (rect, _) = ui.allocate_exact_size(
            Vec2::new(ui.available_width(), editor_height),
            egui::Sense::hover(),
        );
        let mut child = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(rect)
                .layout(egui::Layout::top_down(egui::Align::Min)),
        );
        scroll_editor(app, &mut child, editor_height, editable);
        ui.add_space(6.0);
        // One status line. The buffer's name and editability are in the header;
        // everything here is progress, so a wrapped row would only cost the
        // listing a line to say nothing twice.
        //
        // The counter and the line number are what tells the operator where he is
        // in the list, so they are measured against the row and never cut. The
        // message is the part that gives way: it is fitted on its own into what
        // the prefix leaves, so a long fault reads `SYNTAX 7/10  LN 28: paramete…`
        // instead of pushing the row past the pane. The walk itself has no
        // buttons: `fault_next`/`fault_prev` are the way, and the hint row names
        // them.
        let doc = app.program.doc();
        let issue_count = doc.issues.len();
        let (status, fault_message, status_color) = match doc.issues.get(doc.issue_index) {
            Some(issue) if issue_count > 1 => (
                format!(
                    "SYNTAX {}/{}  LN {}: ",
                    doc.issue_index + 1,
                    issue_count,
                    issue.line
                ),
                Some(issue.message.as_str()),
                theme::ERROR,
            ),
            Some(issue) => (
                format!("SYNTAX LN {}: ", issue.line),
                Some(issue.message.as_str()),
                theme::ERROR,
            ),
            None => ("PREVIEW VALID".to_string(), None, theme::SUCCESS),
        };
        // Inside a subroutine the controller reports a line of the routine's own
        // file and does not publish that file's name, so the preview names it.
        let chip = subroutine_chip(app);
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "LN {}  /  {} LINES",
                    app.program.doc().selected_line,
                    app.program.doc().editor.line_count()
                ))
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
            );
            // Where the run is inside a loop: each loop's counters and the pass
            // it is on. The controller publishes no parameters, so this is the
            // preview's reading; the marker says so when the test reads the
            // machine. Cut to what the right group leaves rather than painted
            // over it.
            if let Some(readout) = crate::state::LoopReadout::current(
                &app.program.doc().preview,
                &app.program.loop_progress,
            ) {
                let reserved = theme::measure(ui, &status, 9.0)
                    + chip
                        .as_deref()
                        .map_or(0.0, |text| theme::measure(ui, text, 9.0) + 10.0)
                    + 16.0;
                // No floor either: the status to its right keeps its glyphs, and
                // the readout is the one that disappears when the row is short.
                let room = (ui.available_width() - reserved).max(0.0);
                if room > 24.0 {
                    let text = fit_text(ui, &readout.line(), room);
                    let cut = text != readout.line();
                    let response = ui.label(
                        RichText::new(text)
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::ACCENT),
                    );
                    if cut {
                        response.on_hover_text(readout.line());
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                let chip_room = chip
                    .as_deref()
                    .map_or(0.0, |text| theme::measure(ui, text, 9.0) + 10.0);
                // Right-to-left: this group owns what the left-hand labels left
                // behind, so its own available width is the whole room. No floor:
                // a floor is what pushed the row past the pane on a narrow window.
                let room = (ui.available_width() - chip_room).max(0.0);
                if room > 8.0 {
                    let shown = fitted_status(ui, &status, fault_message, room);
                    let response = ui.label(
                        RichText::new(shown.clone())
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(status_color),
                    );
                    // A cut message stays readable on hover instead of wrapping
                    // the row into a second line.
                    if let Some(message) = fault_message {
                        let full = format!("{status}{message}");
                        if shown != full {
                            response.on_hover_text(full);
                        }
                    }
                }
                if let Some(chip) = &chip {
                    ui.label(
                        RichText::new(chip)
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::ACCENT),
                    );
                }
            });
        });
    });
}

/// The subroutine the controller is inside, named from the preview's bodies.
fn subroutine_chip(app: &LincApp) -> Option<String> {
    if app.snapshot.program.call_level == 0 {
        return None;
    }
    let line = app.snapshot.program.sub_line.max(1);
    let named =
        linc_core::gcode::SubroutineBody::named_at(&app.program.doc().preview.subroutines, line)
            .map(|body| body.name.clone());
    Some(match named {
        Some(name) => format!("SUBROUTINE {name} LN {line}"),
        None => format!("SUBROUTINE LN {line}"),
    })
}

/// `text` cut to `room` pixels on a character boundary, marked where it ends.
/// The status string cut to `room` without ever losing the fault counter or the
/// line number: the prefix is measured first and kept whole, and only the message
/// is fitted into what is left. A message with no room to say anything is dropped
/// rather than dragged past the edge of the pane.
fn fitted_status(ui: &Ui, prefix: &str, message: Option<&str>, room: f32) -> String {
    let prefix_room = theme::measure(ui, prefix, 9.0);
    // A pane narrow enough that even the counter does not fit: cut the line as a
    // whole, so the row still stops at the edge and keeps as much of the counter
    // as there is room for. Measured, not guessed: at 9 pt the mill counter
    // `SYNTAX 7/10  LN 28: ` is 135 px, which is already wider than a quarter of
    // the pane.
    if prefix_room > room {
        let full = match message {
            Some(message) => format!("{prefix}{message}"),
            None => prefix.to_string(),
        };
        return fit_text(ui, &full, room);
    }
    let Some(message) = message else {
        return prefix.to_string();
    };
    let left = room - prefix_room;
    if left <= 12.0 {
        return prefix.to_string();
    }
    format!("{prefix}{}", fit_text(ui, message, left))
}

fn fit_text(ui: &Ui, text: &str, room: f32) -> String {
    fit_text_sized(ui, text, 9.0, room)
}

fn fit_text_sized(ui: &Ui, text: &str, size: f32, room: f32) -> String {
    if theme::measure(ui, text, size) <= room {
        return text.to_string();
    }
    const ELLIPSIS: &str = "\u{2026}";
    let mut chars: Vec<char> = text.chars().collect();
    while !chars.is_empty() {
        chars.pop();
        let candidate = format!("{}{}", chars.iter().collect::<String>(), ELLIPSIS);
        if theme::measure(ui, &candidate, size) <= room {
            return candidate;
        }
    }
    ELLIPSIS.to_string()
}

fn scroll_editor(app: &mut LincApp, ui: &mut Ui, height: f32, editable: bool) {
    let doc = app.program.doc();
    let mut text = doc.editor.text();
    let current_line = doc.selected_line;
    let search = app.program.search_needle.clone();
    let matches = app.program.search_matches.clone();
    let issues = doc.issues.clone();
    let current_issue = (doc.issue_index < issues.len()).then_some(doc.issue_index);
    let line_count = doc.editor.line_count();
    let desired_rows = line_count.max(12);
    let gutter_font = FontId::monospace(editor_font());
    let gutter_width = ui.fonts_mut(|fonts| {
        fonts
            .layout_no_wrap(
                line_count.max(1).to_string(),
                gutter_font.clone(),
                theme::LINE_NUMBER,
            )
            .size()
            .x
    }) + 10.0;
    // `vertical_scroll_offset` does not move this scroll area, so the view is
    // asked to reveal a rect instead — the same call the text edit makes when
    // its cursor moves.
    let scroll_to =
        app.program
            .doc_mut()
            .code_scroll_to_line
            .take()
            .or(if app.snapshot.task.running {
                Some(current_line)
            } else {
                None
            });
    let scroll = egui::ScrollArea::vertical()
        .id_salt("program-editor")
        .max_height(height)
        .auto_shrink([false, false]);
    let mut scroll_target: Option<(f32, f32)> = None;
    scroll.show(ui, |ui| {
        ui.horizontal_top(|ui| {
            // Lane for the current-line bar, so it sits left of the numbers
            // instead of on top of them.
            ui.add_space(common::SELECTION_EDGE_PADDING + theme::SELECTION_BAR + 4.0);
            // Numbers are painted after the galley is laid out, aligned to each
            // logical line's first visual row. A separate multiline label would
            // keep one row per line and drift away from wrapped text.
            ui.add_space(gutter_width);
            // The rule is the gutter's right edge: the numbers are painted
            // against its rect, not against the text galley, so the last digit
            // cannot land under the line when the item spacing changes.
            let rule = ui.separator();
            let number_right = rule.rect.left() - 6.0;
            let available = (ui.available_width() - 12.0).max(240.0);
            // Lexing and laying out the whole buffer every frame is the most
            // expensive thing the UI does. Cache the galley on the editor
            // revision plus the highlight inputs, so a still buffer is laid
            // out once (SPEC §7.3 "cache tokens by buffer revision", §10
            // "cache text layout and visible-line tokens").
            // Wrap long lines to the editor's width. The line number stays on
            // the logical line's first row; the gutter and selection band are
            // painted from the galley's row mapping below, not counted in
            // screen rows, so logical line numbers still match the graph.
            let wrap_width = available.max(64.0);
            // Insert spaces before G/M and value words the operator types,
            // exactly like the MDI input: the shared rule runs over the whole
            // buffer before the edit draws, so the typed character lands after
            // the space it needs and the caret stays on the right glyph. Only
            // in an editable editor that already holds the keyboard, and only
            // for typed characters — a paste is a whole block the operator
            // chose. This runs before the layouter is built, so it may update
            // `app` without fighting the layouter's own borrow of the editor's
            // revision.
            let editor_id = egui::Id::unique("linc-program-editor");
            let mut typed: Vec<char> = Vec::new();
            ui.input(|input| {
                for event in &input.events {
                    if let egui::Event::Text(text) = event {
                        typed.extend(text.chars());
                    }
                }
            });
            if editable && !typed.is_empty() && ui.memory(|memory| memory.has_focus(editor_id)) {
                let chars: Vec<char> = text.chars().collect();
                let mut state =
                    egui::text_edit::TextEditState::load(ui.ctx(), editor_id).unwrap_or_default();
                let (caret, anchor) = match state.cursor.char_range() {
                    Some(range) => {
                        let range = range.as_sorted_char_range();
                        (range.end.0, range.start.0)
                    }
                    // No cursor yet: the editor was just focused, and egui puts
                    // the caret at the end of the text.
                    None => (chars.len(), chars.len()),
                };
                if let Some((spaced, caret)) = spacing::space_typed(&chars, caret, anchor, &typed) {
                    text = spaced.into_iter().collect();
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::one(
                            egui::text::CCursor::new(caret),
                        )));
                    state.store(ui.ctx(), editor_id);
                    app.program.doc_mut().editor.replace_text(text.clone());
                    app.mark_editor_changed();
                }
            }
            let revision = app.program.doc().editor.revision;
            let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, _wrap_width: f32| {
                // `pixels_per_point` is part of the key: the shell changes the
                // design zoom with the window, and reusing a galley laid out at
                // the old scale makes epaint warn during tessellation.
                let key: GalleyKey = (
                    revision,
                    issues
                        .iter()
                        .map(|issue| issue.line)
                        .collect::<Vec<usize>>(),
                    current_issue,
                    search.clone(),
                    matches.clone(),
                    buffer.as_str().len(),
                    ui.ctx().pixels_per_point().to_bits(),
                    wrap_width.to_bits(),
                );
                let cache_id = egui::Id::unique("linc-editor-galley");
                if let Some((cached, galley)) = ui
                    .ctx()
                    .data(|data| data.get_temp::<(GalleyKey, Arc<Galley>)>(cache_id))
                {
                    if cached == key {
                        return galley;
                    }
                }
                let mut job =
                    highlight_job(buffer.as_str(), &issues, current_issue, &search, &matches);
                job.wrap.max_width = wrap_width;
                let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
                ui.ctx()
                    .data_mut(|data| data.insert_temp(cache_id, (key, galley.clone())));
                galley
            };
            // A jump (Ctrl+G) has to move the text cursor, not just the
            // highlight: the cursor feeds `selected_line` back every frame while
            // the editor has focus, so a highlight-only move would be undone on
            // the next frame.
            if let Some(line) = app.program.doc_mut().cursor_line.take() {
                let id = egui::Id::unique("linc-program-editor");
                let char_index: usize = text
                    .lines()
                    .take(line.saturating_sub(1))
                    .map(|row| row.chars().count() + 1)
                    .sum();
                let mut state =
                    egui::text_edit::TextEditState::load(ui.ctx(), id).unwrap_or_default();
                state
                    .cursor
                    .set_char_range(Some(egui::text::CCursorRange::one(
                        egui::text::CCursor::new(char_index),
                    )));
                state.store(ui.ctx(), id);
            }
            let output = egui::TextEdit::multiline(&mut text)
                // Fixed id so the shell can tell whether the editor holds the
                // keyboard, and leave Home, End and the page keys to it.
                .id(egui::Id::unique("linc-program-editor"))
                .font(FontId::monospace(editor_font()))
                .desired_rows(desired_rows)
                .desired_width(available)
                .frame(egui::Frame::NONE)
                .interactive(editable)
                .layouter(&mut layouter)
                .show(ui);
            // A dialog owns the keyboard while it is open: the editor is drawn
            // behind it, and taking the focus here would pull it off the
            // dialog's default button. The request waits for the dialog to go.
            if app.program.focus_editor && !app.overlay_open() {
                if editable {
                    output.response.request_focus();
                }
                app.program.focus_editor = false;
            }
            if editable && output.response.changed() {
                let new_text = text.clone();
                let before = app.program.doc().editor.text();
                if before != new_text {
                    app.program.doc_mut().editor.replace_text(new_text.clone());
                    app.mark_editor_changed();
                }
            }
            // Only an editable editor owns the line selection through its text
            // cursor. A read-only editor keeps the cursor wherever it was when
            // the run started; feeding that back every frame fought the shell's
            // follow-the-executing-line update and made the band (and the view)
            // flicker back to that old line.
            if editable {
                if let Some(range) = output.cursor_range {
                    let cursor = usize::from(range.primary.index)
                        .min(text.chars().count().saturating_sub(1));
                    let line = text.chars().take(cursor).filter(|ch| *ch == '\n').count() + 1;
                    app.program.doc_mut().selected_line = line.max(1).min(line_count.max(1));
                }
            }
            // A row ends a logical line only when the text had a newline there;
            // wrapped continuation rows do not. Deriving the starts from that
            // flag keeps the gutter aligned even when wrapping drops whitespace
            // (a character-offset mapping skipped a number at the first wrap).
            let starts = line_row_starts(
                output
                    .galley
                    .rows
                    .iter()
                    .map(|placed| placed.ends_with_newline),
            );
            let rows = &output.galley.rows;
            let clip = ui.clip_rect();
            let painter = ui.painter_at(clip);
            let fault_lines: std::collections::BTreeSet<usize> =
                issues.iter().map(|issue| issue.line).collect();
            for (line, start) in starts.iter().enumerate() {
                let Some(placed) = rows.get(*start) else {
                    continue;
                };
                let y = output.galley_pos.y + placed.pos.y;
                if y + line_height() < clip.top() || y > clip.bottom() {
                    continue;
                }
                let colour = if fault_lines.contains(&(line + 1)) {
                    theme::ERROR
                } else {
                    theme::LINE_NUMBER
                };
                painter.text(
                    egui::pos2(number_right, y),
                    egui::Align2::RIGHT_TOP,
                    (line + 1).to_string(),
                    gutter_font.clone(),
                    colour,
                );
            }
            // A bar on the gutter's left edge for every line holding a fault: the
            // number says which line, the bar says the file has one at a glance,
            // and the one the operator is on is drawn wider so F8 visibly moves.
            let bar_left = number_right - gutter_width + 1.0;
            for line in &fault_lines {
                if *line == 0 || *line > starts.len() {
                    continue;
                }
                let (top, bottom) = line_span(&output.galley, output.galley_pos.y, &starts, *line);
                let band = egui::Rect::from_min_max(
                    egui::pos2(bar_left, top),
                    egui::pos2(bar_left + 3.0, bottom.max(top + line_height())),
                )
                .intersect(clip);
                if band.width() <= 0.0 || band.height() <= 0.0 {
                    continue;
                }
                let current_fault = issues
                    .get(app.program.doc().issue_index)
                    .is_some_and(|issue| issue.line == *line);
                painter.rect_filled(
                    if current_fault {
                        egui::Rect::from_min_max(
                            band.min - egui::vec2(1.0, 0.0),
                            band.max + egui::vec2(1.0, 0.0),
                        )
                    } else {
                        band
                    },
                    egui::CornerRadius::same(1),
                    theme::ERROR,
                );
            }
            // The current line, styled like the tool table's selected row: a
            // translucent accent band across every wrapped row of the logical
            // line, and a solid bar on the rim. The shell keeps the selection
            // on the executing line while a program runs.
            let current = app.program.doc().selected_line.max(1);
            if current <= starts.len() {
                let (top, bottom) =
                    line_span(&output.galley, output.galley_pos.y, &starts, current);
                let band = egui::Rect::from_min_max(
                    egui::pos2(clip.left() + common::SELECTION_EDGE_PADDING, top),
                    egui::pos2(clip.right(), bottom),
                );
                common::selection_band(ui, band);
            }
            // The block a finished run ended on. The selection was sent back to
            // the top, and this dim mark keeps "it got to here" on screen
            // instead of leaving the operator to remember it.
            if let Some(finished) = app.program.completed_at {
                if finished != current && finished <= starts.len() {
                    let (top, bottom) =
                        line_span(&output.galley, output.galley_pos.y, &starts, finished);
                    let band = egui::Rect::from_min_max(
                        egui::pos2(clip.left() + common::SELECTION_EDGE_PADDING, top),
                        egui::pos2(clip.right(), bottom),
                    )
                    .intersect(clip);
                    if band.width() > 0.0 && band.height() > 0.0 {
                        painter.rect_filled(
                            band,
                            egui::CornerRadius::same((theme::SELECTION_RADIUS / 2.0) as u8),
                            theme::tint(theme::MUTED, theme::SELECTION_ALPHA / 2),
                        );
                        painter.rect_filled(
                            egui::Rect::from_min_size(
                                band.min,
                                egui::vec2(theme::SELECTION_BAR, band.height()),
                            ),
                            egui::CornerRadius::same(1),
                            theme::MUTED,
                        );
                    }
                }
            }
            // The fault under the cursor, in full. The footer shortens the
            // message to fit one row, so this is where the operator reads it: a
            // red box clear of the line it belongs to, above when the line sits
            // low in the viewport and below when it sits high. A wrapped line is
            // cleared across every row it occupies, which is why the box is
            // placed from the line's row span and not from its first row.
            if current <= starts.len() {
                let say: Vec<&str> = issues
                    .iter()
                    .filter(|issue| issue.line == current)
                    .map(|issue| issue.message.as_str())
                    .collect();
                if !say.is_empty() {
                    let (top, bottom) =
                        line_span(&output.galley, output.galley_pos.y, &starts, current);
                    fault_box(
                        ui,
                        &painter,
                        clip,
                        output.galley_pos.x,
                        (top, bottom),
                        &say.join("  \u{00b7}  "),
                    );
                }
            }
            if let Some(line) = scroll_to {
                let index = line.max(1).min(starts.len());
                scroll_target = Some(line_span(
                    &output.galley,
                    output.galley_pos.y,
                    &starts,
                    index,
                ));
            }
            // A little room under the last line, so it does not sit flush
            // against the bottom of the scroll viewport.
            ui.add_space(6.0);
            let _ = output.state;
            let _ = available;
        });
        if let Some((top, bottom)) = scroll_target {
            let content = ui.min_rect();
            // A block taller than the viewport cannot be centred whole; cap the
            // rect at one viewport so its first row stays on screen.
            let bottom = bottom
                .max(top + line_height())
                .min(top + height.max(line_height()));
            let rect = egui::Rect::from_min_max(
                egui::pos2(content.left(), top),
                egui::pos2(content.left() + content.width().max(10.0), bottom),
            );
            ui.scroll_to_rect(rect, Some(egui::Align::Center));
        }
    });
}

/// Where a fault box goes: on the side of the line with the room for it, and
/// never overlapping the line's rows. `line` is the block's full painted span, so
/// a wrapped block is cleared whole.
///
/// The box is placed, then clamped into the clip rect on the side it chose. When
/// neither side holds the whole box, the side with more room keeps it and the box
/// is shortened; the edge nearest the line never crosses into the line.
fn fault_box_rect(clip: egui::Rect, left: f32, line: (f32, f32), size: egui::Vec2) -> egui::Rect {
    // The clear space either side of the line is wider than the drop shadow
    // reaches, so the shadow falls on the editor's empty rows and never on the
    // line the box is explaining.
    const GAP: f32 = 9.0;
    let (top, bottom) = line;
    let above = (top - GAP - clip.top()).max(0.0);
    let below = (clip.bottom() - bottom - GAP).max(0.0);
    // Below first when it has room: the line under the cursor is read downwards,
    // and a box that jumps above it on every small scroll is harder to follow.
    let use_below = below >= size.y || below >= above;
    let (start, room) = if use_below {
        (bottom + GAP, below)
    } else {
        (top - GAP - size.y.min(above), above)
    };
    let height = size.y.min(room.max(0.0));
    // Left-aligned to the first glyph of the block, which is what the operator
    // reads as that line's own text; clamped for a scrolled buffer.
    let left = left.max(clip.left());
    let width = size.x.min((clip.right() - left).max(0.0));
    let y = if use_below {
        start.min(clip.bottom() - height)
    } else {
        (top - GAP - height).max(clip.top())
    };
    egui::Rect::from_min_size(
        egui::pos2(left, y.max(clip.top())),
        egui::vec2(width, height),
    )
}

/// The box itself: red rim, red text, wrapped to the pane. Long messages grow
/// downwards from wherever the box was placed.
fn fault_box(
    ui: &Ui,
    painter: &egui::Painter,
    clip: egui::Rect,
    text_left: f32,
    line: (f32, f32),
    message: &str,
) {
    const PAD: egui::Vec2 = egui::vec2(7.0, 4.0);
    const MAX_WIDTH: f32 = 420.0;
    let font = FontId::monospace(theme::font_size(9.5));
    let left = text_left.max(clip.left());
    let wrap = (clip.right() - left - 2.0 * PAD.x).min(MAX_WIDTH).max(80.0);
    let mut job = LayoutJob::default();
    job.wrap.max_width = wrap;
    job.append(
        message,
        0.0,
        TextFormat {
            font_id: font,
            color: theme::ERROR,
            background: Color32::TRANSPARENT,
            italics: false,
            ..Default::default()
        },
    );
    let galley = ui.fonts_mut(|fonts| fonts.layout_job(job));
    let size = galley.size() + 2.0 * PAD;
    let rect = fault_box_rect(clip, left, line, size);
    if rect.width() <= 0.0 || rect.height() <= 0.0 {
        return;
    }
    let corner = egui::CornerRadius::same(3);
    // The shadow falls on the editor, not on the box: the box is opaque, so the
    // lift has to read outside it, and the gap above keeps the fuzzy edge off the
    // glyphs of the line it explains.
    painter.add(
        egui::epaint::Shadow {
            offset: [0, 3],
            blur: 8,
            spread: 0,
            color: Color32::from_black_alpha(180),
        }
        .as_shape(rect, corner),
    );
    painter.rect_filled(rect, corner, theme::ERROR_BOX);
    painter.rect_stroke(
        rect,
        corner,
        egui::Stroke::new(1.0, theme::ERROR),
        egui::StrokeKind::Inside,
    );
    // Clipped to its own box: a message taller than the room on its side is cut
    // at the rim instead of spilling over the line or the pane edge.
    painter
        .with_clip_rect(rect)
        .galley(rect.min + PAD, galley, theme::ERROR);
}

/// Top and bottom of one logical line's painted rows, from its first row to
/// the row after its last. A wrapped line spans every row it was laid out on,
/// which is what lets the selection band and the follow scroll centre the whole
/// block instead of its first row.
fn line_span(galley: &egui::Galley, galley_top: f32, starts: &[usize], line: usize) -> (f32, f32) {
    let rows = &galley.rows;
    let first = starts.get(line - 1).copied().unwrap_or(0);
    let next = starts.get(line).copied().unwrap_or(rows.len());
    let top = galley_top + rows.get(first).map_or(0.0, |placed| placed.pos.y);
    let bottom = if next < rows.len() {
        galley_top + rows[next].pos.y
    } else {
        rows.last().map_or(top + line_height(), |placed| {
            galley_top + placed.rect().bottom()
        })
    };
    (top, bottom.max(top + line_height()))
}

/// Cache key for the editor galley: editor revision, highlight inputs, wrap
/// width and buffer length.
/// Everything the laid-out buffer depends on. The fault list enters as the
/// lines it marks and the one the operator is on: a rebuild that changes a
/// message without changing a line still changes the editor revision, which is
/// already the first element.
type GalleyKey = (
    u64,
    Vec<usize>,
    Option<usize>,
    String,
    Vec<usize>,
    usize,
    u32,
    u32,
);

/// First galley row of each logical line, derived from whether the row ended
/// with a newline. Wrapped rows continue the same logical line.
fn line_row_starts(ends_with_newline: impl Iterator<Item = bool>) -> Vec<usize> {
    let mut starts = vec![0];
    for (row, ends) in ends_with_newline.enumerate() {
        if ends {
            starts.push(row + 1);
        }
    }
    starts
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The colour the editor paints the character at `at`.
    fn colour_at(job: &LayoutJob, at: usize) -> Color32 {
        job.format_at_byte(egui::text::ByteIndex(at)).color
    }

    #[test]
    fn an_o_statement_colours_its_keyword_and_operators() {
        let job = layout_for_test("o100 WHILE [#<i> Lt 4]", 0);
        let text = job.text.clone();
        let at = |needle: &str| text.find(needle).expect("the line is laid out");
        // The statement word and the comparison word have their own colours;
        // the letters are not on-screen as an axis word and parameter words.
        assert_eq!(colour_at(&job, at("WHILE")), theme::C2);
        assert_eq!(colour_at(&job, at("LT")), theme::C5);
        assert_ne!(colour_at(&job, at("WHILE")), theme::C3);
        assert_ne!(colour_at(&job, at("LT")), theme::C6);
        // A named label is one token too.
        let job = layout_for_test("o<probe_z> call [1]", 0);
        let text = job.text.clone();
        assert_eq!(
            colour_at(&job, text.find("PROBE_Z").expect("laid out")),
            theme::C2
        );
        assert_eq!(
            colour_at(&job, text.find("CALL").expect("laid out")),
            theme::C2
        );
        // An arc centre word reads like the axis words beside it, not like a
        // `#` parameter reference.
        let job = layout_for_test("G2 X10 Y0 I2 J0", 0);
        let text = job.text.clone();
        assert_eq!(
            colour_at(&job, text.find("I2").expect("laid out")),
            colour_at(&job, text.find("X10").expect("laid out"))
        );
        let job = layout_for_test("#1 = 2", 0);
        assert_ne!(
            colour_at(&job, job.text.find("#1").expect("laid out")),
            theme::C3,
            "a parameter reference is not an axis word"
        );
        // Symbol operators are operators too, on an assignment as well.
        let job = layout_for_test("#1 = [sqrt[4] + 2 * 3]", 0);
        let text = job.text.clone();
        for needle in ["=", "+", "*"] {
            assert_eq!(
                colour_at(&job, text.find(needle).expect("laid out")),
                theme::C5,
                "{needle} is an operator"
            );
        }
    }

    #[test]
    fn wrapped_rows_do_not_start_new_logical_lines() {
        // long line wrapped to two rows, then a short line, then a blank line.
        assert_eq!(
            line_row_starts([false, true, true].into_iter()),
            vec![0, 2, 3]
        );
        // A single few-row line with no trailing newline.
        assert_eq!(line_row_starts([false].into_iter()), vec![0]);
        // Blank lines are rows that end with a newline.
        assert_eq!(line_row_starts([true, true].into_iter()), vec![0, 1, 2]);
    }

    #[test]
    fn the_editor_layout_is_the_buffer_character_for_character() {
        // Blank lines, a comment line and a long line must all come back
        // exactly as written. The text edit maps the caret through the
        // galley's character indices, so a placeholder character for an empty
        // line would shift every edit after it by one glyph.
        let text = "G0 X0\n\n(comment line)\nG1 X10 Y20 Z30 F120\n";
        let job = layout_for_test(text, 0);
        assert_eq!(job.text, text);
    }

    #[test]
    fn every_fault_line_is_underlaid_in_red() {
        let text = "G0 X0\nG1 X\nG1 X10 X20 F100\nM2\n";
        let issues = linc_core::lint::lint_program(text);
        assert_eq!(issues.len(), 2, "{issues:?}");
        let job = highlight_job(text, &issues, Some(0), "", &[]);
        let first = job.text.find("G0 X0").expect("the first line lays out");
        let second = job.text.find("G1 X\n").expect("the first fault lays out");
        let third = job.text.find("G1 X10").expect("the second fault lays out");
        assert_eq!(
            job.format_at_byte(egui::text::ByteIndex(first)).background,
            Color32::TRANSPARENT,
            "a clean line stays unpainted"
        );
        assert_eq!(
            job.format_at_byte(egui::text::ByteIndex(second)).background,
            theme::tint(theme::ERROR, 0x55),
            "the fault the operator is on carries the stronger wash"
        );
        assert_eq!(
            job.format_at_byte(egui::text::ByteIndex(third)).background,
            theme::tint(theme::ERROR, 0x2b),
            "a fault further down the file stays marked"
        );
    }

    /// The status row keeps the counter and the line and gives up message text:
    /// the row must not grow past the width it was given, whatever the message.
    /// The fault box never covers the line it describes: below a line high in the
    /// viewport, above one low in it, and clear of a wrapped block's whole span
    /// either way.
    #[test]
    fn the_fault_box_keeps_off_the_line_it_describes() {
        let clip = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(400.0, 300.0));

        // A line near the top: the box goes below it.
        let high = (20.0, 36.0);
        let below = fault_box_rect(clip, 64.0, high, egui::vec2(300.0, 40.0));
        assert_eq!(
            below.left(),
            64.0,
            "the box starts where the block's text starts, not at the pane edge"
        );
        assert!(below.top() >= high.1, "{below:?} covers the line {high:?}");

        // A line near the bottom: above it, and still inside the viewport.
        let low = (250.0, 272.0);
        let above = fault_box_rect(clip, 64.0, low, egui::vec2(300.0, 40.0));
        assert!(above.bottom() <= low.0, "{above:?} covers the line {low:?}");
        assert!(above.top() >= clip.top());

        // A wrapped block spans several rows; the box clears all of them.
        let wrapped = (120.0, 210.0);
        let boxed = fault_box_rect(clip, 64.0, wrapped, egui::vec2(300.0, 30.0));
        assert!(
            boxed.bottom() <= wrapped.0 || boxed.top() >= wrapped.1,
            "{boxed:?} overlaps the wrapped span {wrapped:?}"
        );

        // A box taller than either side is shortened, not allowed to overlap.
        let tight = fault_box_rect(clip, 64.0, (120.0, 180.0), egui::vec2(300.0, 500.0));
        assert!(
            tight.bottom() <= 120.0 || tight.top() >= 180.0,
            "{tight:?} overlaps the line (120..180)"
        );
        assert!(tight.top() >= clip.top() && tight.bottom() <= clip.bottom());

        // Never wider than the pane, whatever the message wants.
        let wide = fault_box_rect(clip, 64.0, high, egui::vec2(900.0, 20.0));
        assert!(
            wide.width() <= clip.width(),
            "{wide:?} is wider than {clip:?}"
        );
    }

    #[test]
    fn a_long_fault_message_is_cut_and_never_pushes_the_row() {
        let ctx = egui::Context::default();
        let mut results: Vec<(String, f32, f32, f32)> = Vec::new();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let prefix = "SYNTAX 7/10  LN 28: ";
            let message = "parameter assignment has no value";
            let prefix_room = theme::measure(ui, prefix, 9.0);
            // 400 px holds the line in full; the rest are narrower than the
            // message, the prefix, and finally than the counter itself.
            for room in [400.0_f32, 240.0, 180.0, 90.0, 24.0] {
                let shown = fitted_status(ui, prefix, Some(message), room);
                let width = theme::measure(ui, &shown, 9.0);
                results.push((shown, room, width, prefix_room));
            }
        });
        // The first frame paints into the font atlas; egui insists the deltas are
        // either uploaded or dropped deliberately.
        output.textures_delta.clear();
        for (index, (shown, room, width, prefix_room)) in results.iter().enumerate() {
            assert!(
                *width <= *room + 0.5,
                "{shown:?} is {width} px wide in {room} px"
            );
            if index == 0 {
                continue;
            }
            if *room >= *prefix_room {
                // Enough room for the counter and the line: they are kept whole
                // and the message gives way.
                assert!(
                    shown.starts_with("SYNTAX 7/10  LN 28: "),
                    "the counter and the line must survive the cut: {shown:?}"
                );
            }
            assert!(
                !shown.contains("assignment"),
                "the message must be the part that gives way: {shown:?}"
            );
        }
        // The widest room shows the message in full: nothing is cut that fits.
        assert_eq!(
            results[0].0,
            "SYNTAX 7/10  LN 28: parameter assignment has no value"
        );
    }

    #[test]
    fn the_word_at_fault_is_underlined_not_the_whole_block() {
        let text = "G0 X0\nG1 X10 X20 F100\nM2\n";
        let issues = linc_core::lint::lint_program(text);
        let job = highlight_job(text, &issues, Some(0), "", &[]);
        // The second `X` word is the duplicate; the first one is correct.
        let duplicate = job.text.rfind("X20").expect("the duplicate word lays out");
        let first_word = job.text.find("X10").expect("the first word lays out");
        assert_ne!(
            job.format_at_byte(egui::text::ByteIndex(duplicate))
                .underline,
            egui::Stroke::NONE
        );
        assert_eq!(
            job.format_at_byte(egui::text::ByteIndex(first_word))
                .underline,
            egui::Stroke::NONE
        );
    }

    /// The follow scroll centres the logical line, not its first row: a line
    /// that soft-wraps covers several rows, and centring only the first one
    /// left the rest of the line hanging below the middle of the pane.
    #[test]
    fn a_wrapped_line_spans_every_row_it_occupies() {
        let ctx = egui::Context::default();
        let mut laid_out: Option<Arc<Galley>> = None;
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            let mut job = LayoutJob::default();
            job.append(
                "first\nG1 X0 Y0 (a line long enough to wrap over several rows)\nthird\n",
                0.0,
                TextFormat {
                    font_id: FontId::monospace(13.0),
                    ..Default::default()
                },
            );
            job.wrap.max_width = 140.0;
            laid_out = Some(ui.fonts_mut(|fonts| fonts.layout_job(job)));
        });
        output.textures_delta.clear();
        let galley = laid_out.expect("the buffer lays out");
        let starts = line_row_starts(galley.rows.iter().map(|placed| placed.ends_with_newline));
        assert!(
            starts[2] - starts[1] > 1,
            "the middle line wraps: starts {starts:?}"
        );
        let (top, bottom) = line_span(&galley, 0.0, &starts, 2);
        assert!((top - galley.rows[starts[1]].pos.y).abs() < 0.01);
        assert!((bottom - galley.rows[starts[2]].pos.y).abs() < 0.01);
        assert!(
            bottom - top > line_height() * 1.5,
            "every wrapped row belongs to the line: {top}..{bottom}"
        );
    }
}

fn highlight_job(
    text: &str,
    issues: &[linc_core::lint::LintIssue],
    current: Option<usize>,
    search: &str,
    matches: &[usize],
) -> LayoutJob {
    let mut job = LayoutJob::default();
    let base = TextFormat {
        font_id: FontId::monospace(editor_font()),
        color: theme::FG,
        background: Color32::TRANSPARENT,
        italics: false,
        ..Default::default()
    };
    let lines: Vec<&str> = text.split('\n').collect();
    for (index, line) in lines.iter().enumerate() {
        let line_no = index + 1;
        let mut format = base.clone();
        let mut spans: Vec<(usize, usize)> = Vec::new();
        for (position, issue) in issues.iter().enumerate() {
            if issue.line != line_no {
                continue;
            }
            // Behind the glyphs, so the text keeps its syntax colours: a dim
            // transparent red wash marks every line that holds a fault, and the
            // fault the operator is on gets a stronger one so cycling is
            // visible without reading the counter.
            let alpha = if Some(position) == current {
                0x55
            } else {
                0x2b
            };
            format.background = theme::tint(theme::ERROR, alpha);
            if let Some(span) = issue.span {
                spans.push(span);
            }
        }
        if !search.is_empty() && matches.contains(&line_no) {
            format.background = theme::ACCENT_DARK;
        }
        // An empty line stays empty: the galley must be the buffer character
        // for character, or the caret maps one glyph past every blank line
        // before it. epaint still emits a row for an empty paragraph, so the
        // line keeps its height and its number without a placeholder character.
        if !line.is_empty() {
            append_gcode_line(&mut job, line, &format, &spans);
        }
        if index + 1 < lines.len() {
            job.append("\n", 0.0, format.clone());
        }
    }
    job
}

/// Append one G-code line in highlighted runs, gaps included: every token plus
/// the source text between and after them, so the laid-out job is the line
/// character for character. `format` supplies the font and any background, and
/// every run inside `spans` is underlined in red: the word at fault, not the
/// whole block.
fn append_gcode_line(
    job: &mut LayoutJob,
    line: &str,
    format: &TextFormat,
    spans: &[(usize, usize)],
) {
    let mut cuts: Vec<usize> = spans
        .iter()
        .flat_map(|(start, end)| [*start, *end])
        .collect();
    cuts.sort_unstable();
    cuts.dedup();
    let mut cursor = 0usize;
    for token in lex_line(line) {
        if token.start > cursor {
            push_run(
                job,
                &line[cursor..token.start],
                cursor,
                format,
                spans,
                &cuts,
            );
        }
        let mut token_format = format.clone();
        token_format.color = token_color(token.kind);
        token_format.italics = token.kind == TokenKind::Comment;
        if token.kind == TokenKind::Comment {
            token_format.color = theme::COMMENT;
        }
        // `display()` uppercases G/M/axis words. That keeps the byte length, so
        // the token's own source offsets still map onto it; if a future display
        // transform ever changes the length, the run is painted whole rather
        // than sliced at an offset that no longer means anything.
        let shown = token.display();
        if shown.len() == token.end - token.start {
            push_run(job, &shown, token.start, &token_format, spans, &cuts);
        } else {
            push_run(job, &shown, token.start, &token_format, &[], &[]);
        }
        cursor = token.end;
    }
    if cursor < line.len() {
        push_run(job, &line[cursor..], cursor, format, spans, &cuts);
    }
}

/// Append the source text that starts at `base`, split at the span boundaries
/// falling inside it, underlining the pieces that sit inside a fault span.
fn push_run(
    job: &mut LayoutJob,
    text: &str,
    base: usize,
    format: &TextFormat,
    spans: &[(usize, usize)],
    cuts: &[usize],
) {
    if text.is_empty() {
        return;
    }
    let end = base + text.len();
    let mut boundaries = vec![base];
    boundaries.extend(cuts.iter().copied().filter(|cut| *cut > base && *cut < end));
    boundaries.push(end);
    for pair in boundaries.windows(2) {
        let (start, stop) = (pair[0], pair[1]);
        if start >= stop
            || !text.is_char_boundary(start - base)
            || !text.is_char_boundary(stop - base)
        {
            continue;
        }
        let mut run = format.clone();
        if spans.iter().any(|(a, b)| start >= *a && stop <= *b) {
            run.underline = egui::Stroke::new(1.5, theme::ERROR);
            run.background = theme::tint(theme::ERROR, 0x4d);
        }
        job.append(&text[start - base..stop - base], 0.0, run);
    }
}

/// One block laid out at `size`, for a caller outside the editor (the MDI
/// pane's listing rows and its input). The same gap-preserving layout as the
/// editor, so the pane reads exactly the block it will issue.
pub(crate) fn line_job(line: &str, size: f32) -> LayoutJob {
    let mut job = LayoutJob::default();
    append_gcode_line(
        &mut job,
        line,
        &TextFormat {
            font_id: FontId::monospace(theme::font_size(size)),
            color: theme::FG,
            background: Color32::TRANSPARENT,
            italics: false,
            ..Default::default()
        },
        &[],
    );
    job
}

fn token_color(kind: TokenKind) -> Color32 {
    match kind {
        TokenKind::GCode => theme::C1,
        TokenKind::MCode => theme::C2,
        TokenKind::Axis => theme::C3,
        TokenKind::Number => theme::FG,
        TokenKind::FeedSpeed => theme::C4,
        TokenKind::Tool => theme::C5,
        // A value word reads like an axis word, not like a parameter reference.
        TokenKind::Word => theme::C3,
        TokenKind::Variable => theme::C6,
        TokenKind::Comment => theme::COMMENT,
        TokenKind::ProgramNumber => theme::C6,
        TokenKind::LineNumber => theme::LINE_NUMBER,
        TokenKind::OWord | TokenKind::Keyword => theme::C2,
        TokenKind::Operator => theme::C5,
        TokenKind::Other => theme::FG,
    }
}

/// Exposed for tests.
pub fn layout_for_test(text: &str, current: usize) -> LayoutJob {
    let _ = current;
    highlight_job(text, &[], None, "", &[])
}
