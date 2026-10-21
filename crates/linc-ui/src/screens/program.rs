//! Program page: SPLIT and CODE layouts, editable G-code, syntax-gated
//! preview and live/program toolpath layers.

use std::sync::Arc;

use egui::{text::LayoutJob, Align, Color32, FontId, Galley, RichText, TextFormat, Ui, Vec2};
use linc_core::gcode::{lex_line, TokenKind};
use linc_core::model::{PanelMode, SyntaxIssue};

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
    let mode = app.snapshot.panel.mode.mode();
    if mode == Some(PanelMode::Edit)
        && app.program.view != ProgramView::Code
        && app.snapshot.program_editing_allowed()
    {
        app.program.view = ProgramView::Code;
    }
    header(app, ui);
    ui.add_space(4.0);
    match app.program.view {
        ProgramView::Split => split(app, ui),
        ProgramView::Code => code(app, ui),
    }
}

fn header(app: &mut LincApp, ui: &mut Ui) {
    // The file name and current line are already in the top bar, so only the
    // state flags and the view tabs are worth a row here.
    common::focus_anchor(ui, "program-page-focus");
    ui.horizontal(|ui| {
        // The CODE/SPLIT toggle is a keyboard command now; the binding is
        // listed in the bottom hint row with the other page commands.
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.with_layout(egui::Layout::left_to_right(Align::Center), |ui| {
                if app.program.editor.dirty {
                    ui.label(
                        RichText::new("* UNSAVED")
                            .size(theme::font_size(10.0))
                            .strong()
                            .color(theme::WARNING),
                    );
                }
                if app.snapshot.task.running {
                    ui.label(
                        RichText::new("RUNNING / READ-ONLY")
                            .size(theme::font_size(10.0))
                            .color(theme::SUCCESS),
                    );
                }
                if app.program_protected() {
                    ui.label(
                        RichText::new("PROTECT ON")
                            .size(theme::font_size(10.0))
                            .color(theme::WARNING),
                    );
                }
            });
        });
    });
}

fn search_bar(app: &mut LincApp, ui: &mut Ui) {
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("FIND")
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
        );
        let response = ui.add(
            egui::TextEdit::singleline(&mut app.program.search_needle)
                .hint_text("search G-code")
                .desired_width(260.0),
        );
        if app.program.focus_editor {
            response.request_focus();
            app.program.focus_editor = false;
        }
        let needle = app.program.search_needle.clone();
        if response.changed() {
            app.program.search_matches = app.program.editor.find_all(&needle);
            app.program.search_index = 0;
        }
        if ui.input(|i| i.key_pressed(egui::Key::Enter)) && !needle.is_empty() {
            app.program.search_matches = app.program.editor.find_all(&needle);
            if !app.program.search_matches.is_empty() {
                let index = app.program.search_index % app.program.search_matches.len();
                app.program.selected_line = app.program.search_matches[index];
                app.program.code_scroll_to_line = Some(app.program.selected_line);
                app.program.search_index = index + 1;
            }
        }
        if theme::action_button(ui, "NEXT", !app.program.search_matches.is_empty()).clicked() {
            if !app.program.search_matches.is_empty() {
                let index = app.program.search_index % app.program.search_matches.len();
                app.program.selected_line = app.program.search_matches[index];
                app.program.code_scroll_to_line = Some(app.program.selected_line);
                app.program.search_index = index + 1;
            }
        }
        ui.label(
            RichText::new(format!(
                "{} MATCH{}",
                app.program.search_matches.len(),
                if app.program.search_matches.len() == 1 {
                    ""
                } else {
                    "ES"
                }
            ))
            .monospace()
            .size(theme::font_size(10.0))
            .color(theme::MUTED),
        );
        if theme::action_button(ui, "CLOSE", true).clicked() {
            app.program.search_open = false;
            app.program.focus_editor = false;
        }
    });
}

fn split(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_size();
    let main_height = (available.y - 88.0).max(220.0);
    let editor_width = (available.x * 0.58).max(480.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(editor_width, main_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| editor_pane(app, ui, main_height - 4.0),
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new((available.x - editor_width - 8.0).max(300.0), main_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                graph_card(app, ui, (main_height - 12.0).max(240.0), ViewKind::Full);
            },
        );
    });
    ui.add_space(4.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(available.x * 0.58, 88.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| common::current_next_cards(app, ui),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            ui.label(
                RichText::new("SPLIT / LIVE PATH + PROGRAM PATH")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
    });
}

fn code(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_size();
    let editor_width = (available.x * 0.75).max(620.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(editor_width, available.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| editor_pane(app, ui, available.y - 4.0),
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new((available.x - editor_width - 8.0).max(250.0), available.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                graph_card(app, ui, 150.0, ViewKind::MiniXy);
                egui::ScrollArea::vertical()
                    .id_salt("program-info-scroll")
                    .max_height((available.y - 178.0).max(120.0))
                    .show(ui, |ui| info_card(app, ui));
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
                let clear_label = if matches!(kind, ViewKind::MiniXy) {
                    "CLEAR"
                } else {
                    "CLEAR TRAIL"
                };
                if theme::action_button(ui, clear_label, true).clicked() {
                    app.clear_trail();
                }
                if theme::action_button(ui, "FIT", true).clicked() {
                    app.program.graph_zoom = 1.0;
                    app.program.graph_pan = Vec2::ZERO;
                    app.program.fit_requested = true;
                }
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
        );
        app.program.graph_zoom = zoom;
        app.program.graph_pan = pan;
        app.program.fit_requested = false;
    });
}

fn info_card(app: &mut LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "PROGRAM INFO");
        if let Some(error) = &app.program.preview.error {
            syntax_issue(ui, error);
        }
        let line = app.snapshot.program.current_line.max(1);
        ui.label(
            RichText::new(format!(
                "CURSOR LN {} / {}",
                app.program.selected_line,
                app.program.editor.line_count()
            ))
            .monospace()
            .size(theme::font_size(10.0))
            .color(theme::MUTED),
        );
        common::token_ui(
            ui,
            &app.program.editor.line(app.program.selected_line),
            12.0,
        );
        theme::separator(ui);
        ui.label(
            RichText::new("CURRENT")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        common::token_ui(ui, &app.snapshot.program.current_block, 11.0);
        if let Some(next) = &app.snapshot.program.next_line {
            ui.label(
                RichText::new("NEXT")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            common::token_ui(ui, next, 11.0);
        }
        theme::separator(ui);
        common::modal_summary(app, ui);
        theme::kv_row(
            ui,
            "BUFFER",
            if app.program.editor.dirty {
                "DIRTY"
            } else {
                "SAVED"
            },
        );
        theme::kv_row(ui, "LINES", app.program.editor.line_count().to_string());
        theme::kv_row(
            ui,
            "PREVIEW",
            if app.program.preview_due.is_some() {
                "DEBOUNCED".into()
            } else if app.program.preview.error.is_some() {
                format!(
                    "ERROR LN {}",
                    app.program.preview.error.as_ref().unwrap().line
                )
            } else {
                "VALID".into()
            },
        );
        let _ = line;
    });
}

fn syntax_issue(ui: &mut Ui, issue: &SyntaxIssue) {
    ui.label(
        RichText::new(format!("SYNTAX ERROR LN {}: {}", issue.line, issue.message))
            .size(theme::font_size(11.0))
            .color(theme::WARNING),
    );
}

fn editor_pane(app: &mut LincApp, ui: &mut Ui, height: f32) {
    let editable = app.editing_allowed();
    theme::card(ui, |ui| {
        ui.set_min_height(height);
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
        scroll_editor(app, ui, height - 92.0, editable);
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
            ui.label(
                RichText::new("MODAL")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            if app.snapshot.program.modal.is_empty() {
                ui.label(
                    RichText::new("—")
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
            } else {
                for (key, value) in app.snapshot.program.modal.iter() {
                    ui.label(
                        RichText::new(common::modal_text(key, value))
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::FG),
                    );
                }
            }
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
