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



/// Scratch key for the measured chrome of a Program graph column.
///
/// The cards report their own height, so the layout does not carry constants
/// that silently rot when the font scale or the code count changes.
fn chrome_key(view: &str) -> egui::Id {
    egui::Id::unique(match view {
        "code" => "linc-program-chrome-code",
        _ => "linc-program-chrome-split",
    })
}

fn split(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_size();
    // `theme::card` adds 12 px of inner margin per side. A card that also draws
    // a `section_header` adds a header row on top. Heights passed to
    // `graph_card` are the *view* height, so the chrome has to be reserved, and
    // the cards report their own so the numbers cannot rot.
    const CARD_CHROME: f32 = 28.0;
    let main_height = (available.y - 4.0).max(220.0);
    // Equal halves: the two panes describe the same program and neither is
    // subordinate to the other.
    let editor_width = (available.x * 0.5 - 6.0).max(400.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(editor_width, main_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| editor_pane(app, ui, main_height - CARD_CHROME - 4.0),
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new((available.x - editor_width - 8.0).max(300.0), main_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                let (graph_chrome, modal_total) = ui
                    .ctx()
                    .data(|data| data.get_temp::<(f32, f32)>(chrome_key("split")))
                    .unwrap_or((86.0, 110.0));
                let path_height =
                    (main_height - graph_chrome - modal_total - 8.0).max(120.0);
                let top = ui.cursor().top();
                graph_card(app, ui, path_height, ViewKind::Full);
                let after_graph = ui.cursor().top();
                common::modal_pane(app, ui, 8, "split-modal");
                let after_modal = ui.cursor().top();
                ui.ctx().data_mut(|data| {
                    data.insert_temp(
                        chrome_key("split"),
                        (after_graph - top - path_height, after_modal - after_graph),
                    )
                });
            },
        );
    });
}

fn code(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_size();
    // CODE is an editing layout: the code gets the width, and the graph column
    // is a thumbnail strip beside it.
    let editor_width = (available.x * 0.76).max(520.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(editor_width, available.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| editor_pane(app, ui, available.y - 28.0),
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new((available.x - editor_width - 8.0).max(220.0), available.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                // Both cards are sized from what they actually render at, not
                // from guessed constants: the chrome inside `graph_card` and
                // the height of the MODAL card change with the font scale and
                // the number of active codes. The first frame uses these
                // defaults and the next corrects them.
                let (graph_chrome, modal_height) = ui
                    .ctx()
                    .data(|data| data.get_temp::<(f32, f32)>(chrome_key("code")))
                    .unwrap_or((86.0, 135.0));
                let path_height =
                    (available.y - graph_chrome - modal_height - 8.0).max(120.0);
                let top = ui.cursor().top();
                graph_card(app, ui, path_height, ViewKind::MiniXy);
                let after_graph = ui.cursor().top();
                common::modal_pane(app, ui, 4, "program-modal");
                let after_modal = ui.cursor().top();
                ui.ctx().data_mut(|data| {
                    data.insert_temp(
                        chrome_key("code"),
                        (after_graph - top - path_height, after_modal - after_graph),
                    )
                });
            },
        );
    });
}

#[derive(Clone, Copy)]
enum ViewKind {
    Full,
    MiniXy,
}

fn graph_card(app: &mut LincApp, ui: &mut Ui, height: f32, kind: ViewKind) {
    let preview = app.program.last_valid_preview.clone();
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(
                ui,
                if matches!(kind, ViewKind::MiniXy) {
                    "PATH"
                } else {
                    "TOOLPATH"
                },
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                // FIT and CLEAR TRAIL are page-local keyboard commands; their
                // bindings are listed in the bottom hint row.
                if !matches!(kind, ViewKind::MiniXy) {
                    ui.label(
                        RichText::new(if app.program.preview.error.is_some() {
                            "LAST VALID"
                        } else {
                            "PROGRAM + LIVE"
                        })
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                    );
                }
            });
        });
        let view = match kind {
            ViewKind::MiniXy => linc_view::ViewMode::TopXy,
            ViewKind::Full => app.program.graph_view,
        };
        let mut zoom = app.program.graph_zoom;
        let mut pan = app.program.graph_pan;
        let mut fit = app.program.fit_requested;
        common::trajectory(
            app,
            ui,
            &preview,
            height,
            view,
            &mut zoom,
            &mut pan,
            &mut fit,
            Some(app.program.selected_line),
            Some(app.snapshot.program.current_line),
            // The mini graph in Code view is a thumbnail: the legend would
            // cover most of it and name colours that are barely visible.
            !matches!(kind, ViewKind::MiniXy),
        );
        app.program.graph_zoom = zoom;
        app.program.graph_pan = pan;
        app.program.fit_requested = false;
    });
}



fn editor_pane(app: &mut LincApp, ui: &mut Ui, height: f32) {
    let editable = app.editing_allowed();
    theme::card(ui, |ui| {
        ui.set_min_height((height - 24.0).max(120.0));
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
        // Reserve an exact rect for the editor: the scroll area otherwise adds
        // its own chrome on top of the height it was given, and the card grows
        // past the slot it was allocated until the bottom is clipped.
        const FOOTER_HEIGHT: f32 = 32.0;
        let editor_height = (ui.available_height() - FOOTER_HEIGHT).max(80.0);
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
    let offset = if app.snapshot.task.running || app.program.code_scroll_to_line.is_some() {
        let target = app
            .program
            .code_scroll_to_line
            .take()
            .unwrap_or(current_line);
        Some(((target as f32 - 3.0) * line_height()).max(0.0))
    } else {
        None
    };
    let mut scroll = egui::ScrollArea::vertical()
        .id_salt("program-editor")
        .max_height(height)
        .auto_shrink([false, false]);
    if let Some(offset) = offset {
        scroll = scroll.vertical_scroll_offset(offset);
    }
    scroll.show(ui, |ui| {
        ui.horizontal_top(|ui| {
            let gutter = ui.add(
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
                    current_line,
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
                    current_line,
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
            let output = egui::TextEdit::multiline(&mut text)
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
            // Mark the line the interpreter is on the way the Machine page's
            // G-code display does: an accent bar in the gutter plus a tinted
            // row. The cursor line keeps its underline from the layouter, so
            // the two are distinguishable while editing.
            let running = app.snapshot.program.current_line.max(1);
            let live = app.snapshot.task.running
                || app.snapshot.program.paused
                || app.snapshot.task.program_started;
            if live && running <= line_count {
                let top = output.galley_pos.y + (running as f32 - 1.0) * line_height();
                let row = egui::Rect::from_min_size(
                    egui::pos2(gutter.rect.left() - 6.0, top),
                    egui::vec2(ui.available_width() + gutter.rect.width(), line_height()),
                );
                let row = row.intersect(ui.clip_rect());
                if row.height() > 0.0 {
                    ui.painter().rect_filled(
                        row,
                        2.0,
                        theme::ACCENT.gamma_multiply(0.12),
                    );
                    ui.painter().rect_filled(
                        egui::Rect::from_min_size(row.left_top(), egui::vec2(2.0, row.height())),
                        1.0,
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
    });
}

/// Cache key for the editor galley: editor revision, highlight inputs, wrap
/// width and buffer length.
type GalleyKey = (u64, usize, Option<usize>, String, Vec<usize>, u32, usize);

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
    current_line: usize,
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
        if line_no == current_line {
            format.underline = egui::Stroke::new(1.0, theme::ACCENT);
        }
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
    highlight_job(text, current, None, "", &[])
}
