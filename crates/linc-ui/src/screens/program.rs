//! Program page: SPLIT and CODE layouts, editable G-code, syntax-gated
//! preview and live/program toolpath layers.

use std::sync::Arc;

use egui::{text::LayoutJob, Align, Color32, FontId, Galley, RichText, TextFormat, Ui, Vec2};
use linc_core::gcode::{lex_line, TokenKind};

use crate::screens::common;
use crate::shell::LincApp;
use crate::state::ProgramView;
use crate::theme;

fn editor_font() -> f32 {
    theme::font_size(13.0)
}

/// Space between the pane's left edge and the current-line bar.
const SELECTION_EDGE_PADDING: f32 = 4.0;

fn line_height() -> f32 {
    editor_font() * 1.36
}

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    if !app.snapshot.probe.enabled && app.profile.variant == linc_core::model::MachineVariant::Lathe
    {
        // The lathe profile may disable the probe; Program is unaffected.
    }
    // No page header: the panes start at the top of the page. The view flags
    // and RUN FROM LINE live in the editor pane's header, next to the file they
    // belong to.
    common::focus_anchor(ui, "program-page-focus");
    match app.program.view {
        ProgramView::Split => split(app, ui),
        ProgramView::Code => code(app, ui),
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

    let codes = app
        .snapshot
        .program
        .modal
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
    common::modal_pane_in(app, &mut modal_ui, 8, "split-modal", modal_rect);
    ui.advance_cursor_after_rect(egui::Rect::from_min_max(left_rect.min, right_rect.max));
}

fn code(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_size();
    let height = (ui.max_rect().bottom() - ui.cursor().top() - 10.0).max(220.0);
    // CODE is an editing layout: the code gets the width, and the graph column
    // is a thumbnail strip beside it.
    let editor_width = (available.x * 0.76).max(520.0);
    ui.horizontal_top(|ui| {
        let editor_rect = egui::Rect::from_min_size(
            ui.cursor().min,
            Vec2::new(editor_width, height),
        );
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
                let codes = app
                    .snapshot
                    .program
                    .modal
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
                common::modal_pane_in(app, &mut modal_ui, 4, "program-modal", modal_rect);
            },
        );
    });
}

#[derive(Clone, Copy)]
enum ViewKind {
    Full,
    MiniXy,
}

fn graph_card(app: &mut LincApp, ui: &mut Ui, rect: egui::Rect, kind: ViewKind) {
    let preview = app.program.last_valid_preview.clone();
    common::shell_card(ui, rect, |ui| {
        let top = ui.cursor().top();
        match kind {
            ViewKind::MiniXy => {
                common::card_title(ui, "PATH");
            }
            ViewKind::Full => common::toolpath_header(
                ui,
                app.program.graph_view.label(),
                app.program.graph_zoom,
            ),
        }
        // Measured from the cursor: `min_rect` already holds the minimum size
        // the shell set, so it would report the whole card as the header and
        // shrink the graph to nothing.
        let header_height = ui.cursor().top() - top;
        let view = match kind {
            ViewKind::MiniXy => linc_view::ViewMode::TopXy,
            ViewKind::Full => app.program.graph_view,
        };
        let mut zoom = app.program.graph_zoom;
        let mut pan = app.program.graph_pan;
        let mut yaw = app.program.graph_yaw;
        let mut pitch = app.program.graph_pitch;
        let mut fit = app.program.fit_requested;
        common::trajectory(
            app,
            ui,
            &preview,
            (rect.height() - 8.0 - header_height - 2.0).max(60.0),
            view,
            &mut zoom,
            &mut pan,
            &mut yaw,
            &mut pitch,
            &mut fit,
            Some(app.program.selected_line),
            Some(app.snapshot.program.executing_line()),
            false,
        );
        app.program.graph_zoom = zoom;
        app.program.graph_pan = pan;
        app.program.graph_yaw = yaw;
        app.program.graph_pitch = pitch;
        app.program.fit_requested = false;
    });
}



/// `card_height` is the total height of the card, margins included, so the
/// pane's bottom edge lines up with whatever sits beside it.
fn editor_pane(app: &mut LincApp, ui: &mut Ui, rect: egui::Rect) {
    let editable = app.editing_allowed();
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
            let text = app.program.editor.text();
            let cursor = state
                .cursor
                .char_range()
                .map(|range| usize::from(range.primary.index))
                .unwrap_or(0);
            let line_count = app.program.editor.line_count();
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
                .set_char_range(Some(egui::text::CCursorRange::one(egui::text::CCursor::new(
                    index,
                ))));
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
            app.program.selected_line = target_line;
            app.program.code_scroll_to_line = Some(target_line);
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
            ui.label(
                RichText::new(file)
                    .monospace()
                    .size(theme::font_size(12.0))
                    .color(theme::FG),
            );
            if app.program.editor.dirty {
                ui.label(RichText::new("*").color(theme::WARNING));
            }
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                if app.snapshot.task.running {
                    ui.label(
                        RichText::new("RUNNING")
                            .size(theme::font_size(9.0))
                            .color(theme::SUCCESS),
                    );
                }
                ui.label(
                    RichText::new(if editable { "EDITABLE" } else { "READ-ONLY" })
                        .size(theme::font_size(9.0))
                        .color(if editable {
                            theme::SUCCESS
                        } else {
                            theme::WARNING
                        }),
                );
                ui.label(
                    RichText::new(format!("{} LINES", app.program.editor.line_count()))
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                // Syntax / preview state belongs with the buffer it describes.
                let (status, color) = if let Some(error) = &app.program.preview.error {
                    (format!("SYNTAX ERROR LN {}", error.line), theme::WARNING)
                } else {
                    ("VALID".to_string(), theme::SUCCESS)
                };
                ui.label(
                    RichText::new(status)
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(color),
                );
            });
        });
        theme::separator(ui);
        // Sized from the card's rect, not from `available_height`: a frame does
        // not bound what it gives its content, so the editor asked for more
        // than the card had and pushed the card past the column beside it.
        const FOOTER_HEIGHT: f32 = 32.0;
        let header_height = ui.cursor().top() - content_top;
        let editor_height =
            (rect.height() - 8.0 - header_height - FOOTER_HEIGHT - 4.0).max(80.0);
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
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(format!(
                    "LN {}  /  {} LINES",
                    app.program.selected_line,
                    app.program.editor.line_count()
                ))
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                let (text, color) = if let Some(error) = &app.program.preview.error {
                    (
                        format!("SYNTAX LN {}: {}", error.line, error.message),
                        theme::ERROR,
                    )
                } else {
                    ("PREVIEW VALID".to_string(), theme::SUCCESS)
                };
                ui.label(
                    RichText::new(text)
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(color),
                );
            });
        });
    });
}

fn scroll_editor(app: &mut LincApp, ui: &mut Ui, height: f32, editable: bool) {
    let mut text = app.program.editor.text();
    let current_line = app.program.selected_line;
    let search = app.program.search_needle.clone();
    let matches = app.program.search_matches.clone();
    let syntax_error_line = app.program.preview.error.as_ref().map(|error| error.line);
    let line_count = app.program.editor.line_count();
    let desired_rows = line_count.max(12);
    let numbers: String = number_gutter(ui, line_count);
    // `vertical_scroll_offset` does not move this scroll area, so the view is
    // asked to reveal a rect instead — the same call the text edit makes when
    // its cursor moves.
    let scroll_to = app
        .program
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
    scroll.show(ui, |ui| {
        ui.horizontal_top(|ui| {
            // Lane for the current-line bar, so it sits left of the numbers
            // instead of on top of them.
            ui.add_space(SELECTION_EDGE_PADDING + theme::SELECTION_BAR + 4.0);
            ui.add(
                egui::Label::new(
                    RichText::new(numbers)
                        .monospace()
                        .size(editor_font())
                        .color(theme::LINE_NUMBER),
                )
                .selectable(false),
            );
            ui.separator();
            let available = (ui.available_width() - 12.0).max(240.0);
            // Lexing and laying out the whole buffer every frame is the most
            // expensive thing the UI does. Cache the galley on the editor
            // revision plus the highlight inputs, so a still buffer is laid
            // out once (SPEC §7.3 "cache tokens by buffer revision", §10
            // "cache text layout and visible-line tokens").
            let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
                let key: GalleyKey = (
                    app.program.editor.revision,
                    syntax_error_line,
                    search.clone(),
                    matches.clone(),
                    wrap_width.to_bits(),
                    buffer.as_str().len(),
                );
                let cache_id = egui::Id::unique("linc-editor-galley");
                if let Some((cached, galley)) =
                    ui.ctx().data(|data| data.get_temp::<(GalleyKey, Arc<Galley>)>(cache_id))
                {
                    if cached == key {
                        return galley;
                    }
                }
                let mut job = highlight_job(
                    buffer.as_str(),
                    syntax_error_line,
                    &search,
                    &matches,
                );
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
            if let Some(line) = app.program.cursor_line.take() {
                let id = egui::Id::unique("linc-program-editor");
                let char_index: usize = text
                    .lines()
                    .take(line.saturating_sub(1))
                    .map(|row| row.chars().count() + 1)
                    .sum();
                let mut state = egui::text_edit::TextEditState::load(ui.ctx(), id)
                    .unwrap_or_default();
                state.cursor.set_char_range(Some(egui::text::CCursorRange::one(
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
            if app.program.focus_editor {
                output.response.request_focus();
                app.program.focus_editor = false;
            }
            if editable && output.response.changed() {
                let new_text = text.clone();
                let before = app.program.editor.text();
                if before != new_text {
                    app.program.editor.replace_text(new_text.clone());
                    app.mark_editor_changed();
                }
            }
            if let Some(range) = output.cursor_range {
                let cursor =
                    usize::from(range.primary.index).min(text.chars().count().saturating_sub(1));
                let line = text.chars().take(cursor).filter(|ch| *ch == '\n').count() + 1;
                app.program.selected_line = line.max(1).min(line_count.max(1));
            }
            // The current line, styled like the tool table's selected row: a
            // translucent accent band across the row and a solid bar on the
            // rim. The shell keeps the selection on the executing line while a
            // program runs, so this is also what follows the cut.
            let current = app.program.selected_line.max(1);
            if current <= line_count {
                let clip = ui.clip_rect();
                // Anchor on the galley's own rows: the font's nominal line
                // height is not the row pitch egui lays out, and the difference
                // accumulated into the band sitting half a line low.
                let rows = &output.galley.rows;
                let first_top = output.galley_pos.y + rows.first().map_or(0.0, |row| row.pos.y);
                let pitch = if rows.len() >= 2 {
                    rows[1].pos.y - rows[0].pos.y
                } else {
                    line_height()
                };
                let top = first_top + (current as f32 - 1.0) * pitch;
                let band = egui::Rect::from_min_max(
                    egui::pos2(clip.left() + SELECTION_EDGE_PADDING, top),
                    egui::pos2(clip.right(), top + pitch),
                )
                .intersect(clip);
                if band.height() > 0.0 {
                    ui.painter().rect_filled(
                        band,
                        egui::CornerRadius::same((theme::SELECTION_RADIUS / 2.0) as u8),
                        theme::tint(theme::ACCENT, theme::SELECTION_ALPHA),
                    );
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(
                            egui::pos2(band.left(), band.top()),
                            egui::vec2(theme::SELECTION_BAR, band.height()),
                        ),
                        egui::CornerRadius::same(1),
                        theme::ACCENT,
                    );
                }
            }
            // A little room under the last line, so it does not sit flush
            // against the bottom of the scroll viewport.
            ui.add_space(6.0);
            let _ = output.state;
            let _ = available;
            let _ = wrap_width_unused();
        });
        if let Some(line) = scroll_to {
            let content = ui.min_rect();
            let rect = egui::Rect::from_min_size(
                egui::pos2(content.left(), content.top() + line as f32 * line_height()),
                Vec2::new(content.width().max(10.0), line_height()),
            );
            ui.scroll_to_rect(rect, Some(egui::Align::Center));
        }
    });

}

/// Cache key for the editor galley: editor revision, highlight inputs, wrap
/// width and buffer length.
type GalleyKey = (u64, Option<usize>, String, Vec<usize>, u32, usize);

/// The line-number gutter only changes when the line count does.
fn number_gutter(ui: &egui::Ui, line_count: usize) -> String {
    let cache_id = egui::Id::unique("linc-editor-gutter");
    if let Some((cached, text)) =
        ui.ctx().data(|data| data.get_temp::<(usize, String)>(cache_id))
    {
        if cached == line_count {
            return text;
        }
    }
    let text: String = (1..=line_count)
        .map(|index| format!("{index:>4}"))
        .collect::<Vec<_>>()
        .join("\n");
    ui.ctx()
        .data_mut(|data| data.insert_temp(cache_id, (line_count, text.clone())));
    text
}

fn wrap_width_unused() -> f32 {
    0.0
}

fn highlight_job(
    text: &str,
    error_line: Option<usize>,
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
        if let Some(error) = error_line {
            if error == line_no {
                format.background = Color32::from_rgba_premultiplied(0xff, 0xb0, 0x20, 0x2b);
            }
        }
        if !search.is_empty() && matches.contains(&line_no) {
            format.background = theme::ACCENT_DARK;
        }
        if line.is_empty() {
            job.append(" ", 0.0, format.clone());
        } else {
            let mut cursor = 0usize;
            for token in lex_line(line) {
                if token.start > cursor {
                    job.append(&line[cursor..token.start], 0.0, format.clone());
                }
                let mut token_format = TextFormat {
                    font_id: FontId::monospace(editor_font()),
                    color: token_color(token.kind),
                    background: format.background,
                    italics: token.kind == TokenKind::Comment,
                    ..Default::default()
                };
                if token.kind == TokenKind::Comment {
                    token_format.color = theme::COMMENT;
                }
                job.append(&token.display(), 0.0, token_format);
                cursor = token.end;
            }
            if cursor < line.len() {
                job.append(&line[cursor..], 0.0, format.clone());
            }
        }
        if index + 1 < lines.len() {
            job.append("\n", 0.0, format.clone());
        }
    }
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
        TokenKind::Parameter => theme::C6,
        TokenKind::Variable => theme::C6,
        TokenKind::Comment => theme::COMMENT,
        TokenKind::ProgramNumber => theme::C6,
        TokenKind::LineNumber => theme::LINE_NUMBER,
        TokenKind::OWord => theme::C2,
        TokenKind::Other => theme::FG,
    }
}

/// Exposed for tests.
pub fn layout_for_test(text: &str, current: usize) -> LayoutJob {
    let _ = current;
    highlight_job(text, None, "", &[])
}
