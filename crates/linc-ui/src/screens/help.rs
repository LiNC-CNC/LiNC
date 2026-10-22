//! Offline LinuxCNC G-code reference browser.
//!
//! One quick-reference table in the page; Enter opens the full section in a
//! near-fullscreen dialog with its own back history.

use egui::{Align, Color32, CornerRadius, Direction, Margin, Rect, RichText, Sense, Ui, Vec2};
use linc_core::help::{HelpBlock, HelpBundle, HelpItem, HelpRow, HelpSection};

use crate::screens::common;
use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    app.help.ensure_loaded();
    let Some(bundle) = app.help.bundle.clone() else {
        ui.label(RichText::new("HELP BUNDLE UNAVAILABLE").color(theme::ERROR));
        return;
    };
    common::focus_anchor(ui, "help-page-focus");
    let filtered = bundle.filter(&app.help.query);
    let anchors: Vec<String> = filtered
        .iter()
        .flat_map(|group| group.rows.iter().map(|row| row.anchor.clone()))
        .collect();
    if anchors.is_empty() {
        app.help.selected_row = 0;
        app.help.selected_anchor = None;
    } else {
        app.help.selected_row = app.help.selected_row.min(anchors.len() - 1);
        let current = app.help.selected_anchor.as_deref();
        if current.map_or(true, |anchor| !anchors.iter().any(|a| a == anchor)) {
            app.help.selected_anchor = Some(anchors[app.help.selected_row].clone());
        }
    }

    let mut search_active = false;
    ui.horizontal(|ui| {
        ui.label(
            RichText::new("SEARCH")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        let response = ui.add(
            egui::TextEdit::singleline(&mut app.help.query)
                .hint_text("G2, G3, M6, o sub, arc …  ( / )")
                .desired_width(360.0),
        );
        if app.help.focus_search {
            response.request_focus();
            app.help.focus_search = false;
        }
        search_active = response.has_focus();
        ui.label(
            RichText::new(format!(
                "{} SECTIONS / {} INDEX ROWS / {} FIGURES",
                bundle.counts.sections, bundle.counts.rows, bundle.counts.figures
            ))
            .monospace()
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
        );
    });

    let mut open_anchor: Option<String> = None;
    if !search_active && !app.overlay_open() {
        let mut delta: i32 = 0;
        let mut read = false;
        ui.input_mut(|input| {
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                delta = 1;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                delta = -1;
            }
            if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                read = true;
            }
        });
        if delta != 0 && !anchors.is_empty() {
            let current = app.help.selected_row.min(anchors.len() - 1) as i32;
            app.help.selected_row =
                (current + delta).clamp(0, anchors.len() as i32 - 1) as usize;
            app.help.selected_anchor = Some(anchors[app.help.selected_row].clone());
            app.help.scroll_to_selection = true;
        }
        if read {
            open_anchor = anchors.get(app.help.selected_row).cloned();
        }
        if ui.input(|input| input.key_pressed(egui::Key::Escape)) {
            app.help.query.clear();
        }
    }

    let scroll_selection = app.help.scroll_to_selection;
    theme::card(ui, |ui| {
        ui.set_min_height(ui.available_height());
        let width = ui.available_width();
        let columns = Columns::new(width);
        header_row(ui, &columns);
        ui.add_space(2.0);
        theme::separator(ui);
        if filtered.is_empty() {
            ui.add_space(12.0);
            ui.label(
                RichText::new("NO CODES MATCH THIS SEARCH")
                    .size(theme::font_size(12.0))
                    .color(theme::MUTED),
            );
            return;
        }
        egui::ScrollArea::vertical()
            .id_salt("help-table")
            .max_height(ui.available_height() - 10.0)
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 3.0;
                let mut index = 0usize;
                let mut zebra = false;
                for group in &filtered {
                    group_row(ui, group);
                    for row in &group.rows {
                        let selected = index == app.help.selected_row;
                        let response = table_row(ui, &columns, row, selected, zebra, index);
                        if selected && scroll_selection {
                            ui.scroll_to_rect(response.rect, Some(Align::Center));
                        }
                        if response.clicked() {
                            app.help.selected_row = index;
                            app.help.selected_anchor = Some(row.anchor.clone());
                        }
                        if response.double_clicked() {
                            open_anchor = Some(row.anchor.clone());
                        }
                        index += 1;
                        zebra = !zebra;
                    }
                }
            });
    });
    app.help.scroll_to_selection = false;
    if let Some(anchor) = open_anchor {
        app.help.open_dialog(&anchor);
    }
}

/// Probe Basic's index columns: code 10 %, parameters 22 %, description 28 %,
/// meaning the rest.
struct Columns {
    code: f32,
    params: f32,
    desc: f32,
    meaning: f32,
}

impl Columns {
    fn new(width: f32) -> Self {
        let code = (width * 0.10).max(80.0);
        let params = (width * 0.22).max(150.0);
        let desc = (width * 0.28).max(170.0);
        let meaning = (width - code - params - desc).max(160.0);
        Self {
            code,
            params,
            desc,
            meaning,
        }
    }
}

fn header_row(ui: &mut Ui, columns: &Columns) {
    ui.spacing_mut().item_spacing.x = 0.0;
    ui.horizontal(|ui| {
        header_cell(ui, columns.code, "CODE", true);
        header_cell(ui, columns.params, "PARAMETERS", false);
        header_cell(ui, columns.desc, "DESCRIPTION", false);
        header_cell(ui, columns.meaning, "MEANING", false);
    });
}

fn header_cell(ui: &mut Ui, width: f32, text: &str, centered: bool) {
    let label = RichText::new(text)
        .size(theme::font_size(9.0))
        .strong()
        .color(theme::MUTED);
    if centered {
        ui.allocate_ui_with_layout(
            Vec2::new(width, 18.0),
            egui::Layout::centered_and_justified(Direction::LeftToRight),
            |ui| {
                ui.set_min_width(width);
                ui.label(label);
            },
        );
    } else {
        ui.allocate_ui_with_layout(
            Vec2::new(width, 18.0),
            egui::Layout::left_to_right(Align::Center),
            |ui| {
                ui.set_min_width(width);
                ui.add_space(6.0);
                ui.label(label);
            },
        );
    }
}

fn group_row(ui: &mut Ui, group: &linc_core::help::HelpGroup) {
    egui::Frame::new()
        .fill(theme::PANEL_RAISED)
        .inner_margin(Margin::symmetric(8, 5))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(&group.name)
                        .size(theme::font_size(11.5))
                        .strong()
                        .color(theme::FG),
                );
                if !group.note.is_empty() {
                    ui.label(
                        RichText::new(&group.note)
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                }
            });
        });
}

fn table_row(
    ui: &mut Ui,
    columns: &Columns,
    row: &HelpRow,
    selected: bool,
    striped: bool,
    index: usize,
) -> egui::Response {
    let fill = if selected {
        theme::tint(theme::ACCENT, theme::SELECTION_ALPHA)
    } else if striped {
        theme::PANEL_RAISED
    } else {
        Color32::TRANSPARENT
    };
    let inner = egui::Frame::new()
        .fill(fill)
        .inner_margin(Margin::symmetric(0, 2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.horizontal_top(|ui| {
                cell(
                    ui,
                    columns.code,
                    true,
                    RichText::new(&row.code)
                        .monospace()
                        .size(theme::font_size(11.0))
                        .color(theme::ACCENT),
                );
                cell(
                    ui,
                    columns.params,
                    false,
                    RichText::new(&row.params)
                        .monospace()
                        .size(theme::font_size(10.0))
                        .color(theme::FG),
                );
                cell(
                    ui,
                    columns.desc,
                    false,
                    RichText::new(&row.desc)
                        .size(theme::font_size(11.0))
                        .color(theme::FG),
                );
                cell(
                    ui,
                    columns.meaning,
                    false,
                    RichText::new(&row.meaning)
                        .size(theme::font_size(10.5))
                        .color(theme::MUTED),
                );
            });
        });
    let rect = inner.response.rect;
    if selected {
        ui.painter().rect_filled(
            Rect::from_min_size(
                rect.left_top(),
                Vec2::new(theme::SELECTION_BAR, rect.height()),
            ),
            CornerRadius::same(1),
            theme::ACCENT,
        );
    }
    ui.interact(rect, egui::Id::unique(("help-row", index)), Sense::click())
}

fn cell(ui: &mut Ui, width: f32, centered: bool, text: RichText) {
    if centered {
        ui.allocate_ui_with_layout(
            Vec2::new(width, 0.0),
            egui::Layout::centered_and_justified(Direction::LeftToRight),
            |ui| {
                ui.set_min_width(width);
                ui.add(egui::Label::new(text).wrap());
            },
        );
    } else {
        ui.allocate_ui_with_layout(
            Vec2::new(width, 0.0),
            egui::Layout::left_to_right(Align::Min),
            |ui| {
                ui.set_min_width(width);
                ui.add_space(6.0);
                ui.add(egui::Label::new(text).wrap());
            },
        );
    }
}

/// Full-screen reference dialog, drawn over every page.
pub fn draw_dialog(app: &mut LincApp, ctx: &egui::Context) {
    let Some(dialog) = app.help.dialog.clone() else {
        return;
    };
    let Some(bundle) = app.help.bundle.clone() else {
        app.help.close_dialog();
        return;
    };
    let Some(section) = bundle.section(&dialog.anchor) else {
        app.help.close_dialog();
        return;
    };
    let screen = ctx.content_rect();
    let size = (screen.size() - Vec2::new(48.0, 48.0)).max(Vec2::new(640.0, 360.0));
    let mut close = false;
    let mut back = false;
    let mut related: Option<String> = None;
    let scroll_top = dialog.scroll_to_top;
    let modal = egui::Modal::new(egui::Id::unique("linc-help-dialog"))
        .frame(
            egui::Frame::new()
                .fill(theme::PANEL)
                .stroke(egui::Stroke::new(1.0, theme::BORDER))
                .corner_radius(8)
                .inner_margin(16),
        )
        .show(ctx, |ui| {
            ui.set_min_size(size);
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&section.title)
                        .size(theme::font_size(22.0))
                        .strong()
                        .color(theme::FG),
                );
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    if theme::action_button(ui, "CLOSE", true).clicked() {
                        close = true;
                    }
                    if dialog.position > 0
                        && theme::action_button(ui, "BACK", true).clicked()
                    {
                        back = true;
                    }
                    ui.label(
                        RichText::new(&bundle.version)
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                });
            });
            theme::separator(ui);
            let mut area = egui::ScrollArea::vertical()
                .id_salt("help-dialog-scroll")
                .max_height(ui.available_height() - 8.0);
            if scroll_top {
                area = area.vertical_scroll_offset(0.0);
            }
            area.show(ui, |ui| {
                related = render_section(app, ui, &bundle, section);
            });
        });
    if modal.should_close() {
        close = true;
    }
    if ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Backspace)) {
        back = true;
    }
    if close {
        app.help.close_dialog();
    } else if back {
        app.help.dialog_back();
    } else if let Some(anchor) = related {
        app.help.dialog_link(&anchor);
    } else if let Some(dialog) = app.help.dialog.as_mut() {
        if dialog.scroll_to_top {
            dialog.scroll_to_top = false;
        }
    }
}

/// The section body without its title; returns a related anchor if clicked.
fn render_section(
    app: &mut LincApp,
    ui: &mut Ui,
    bundle: &HelpBundle,
    section: &HelpSection,
) -> Option<String> {
    let mut related = None;
    for (block_index, block) in section.blocks.iter().enumerate() {
        match block {
            HelpBlock::Heading { text, level } => {
                ui.add_space(6.0);
                ui.label(
                    RichText::new(text)
                        .size(match level {
                            1 => 18.0,
                            2 => 16.0,
                            3 => 14.0,
                            _ => 12.0,
                        })
                        .strong()
                        .color(theme::FG),
                );
            }
            HelpBlock::Para { text } => {
                ui.label(
                    RichText::new(flow(text))
                        .size(theme::font_size(12.0))
                        .color(theme::FG),
                );
            }
            HelpBlock::Code { text } => {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(0x0f, 0x11, 0x13))
                    .stroke(egui::Stroke::new(1.0, theme::BORDER))
                    .corner_radius(4)
                    .inner_margin(8)
                    .show(ui, |ui| {
                        ui.label(
                            RichText::new(text)
                                .monospace()
                                .size(theme::font_size(11.0))
                                .color(theme::FG),
                        );
                    });
            }
            HelpBlock::Table { rows } => {
                egui::Grid::new(format!(
                    "help-table-{}-{block_index}-{}",
                    section.title,
                    rows.len()
                ))
                .striped(true)
                .spacing([10.0, 4.0])
                .show(ui, |ui| {
                    for row in rows {
                        for cell in row {
                            ui.label(
                                RichText::new(cell)
                                    .size(theme::font_size(10.5))
                                    .color(theme::FG),
                            );
                        }
                        ui.end_row();
                    }
                });
            }
            HelpBlock::List { items } => {
                for item in items {
                    match item {
                        HelpItem::ListItem { text } => {
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("•").color(theme::ACCENT));
                                ui.label(
                                    RichText::new(flow(text))
                                        .size(theme::font_size(12.0))
                                        .color(theme::FG),
                                );
                            });
                        }
                        HelpItem::Image { image, text } => {
                            show_figure(app, ui, image, text);
                        }
                    }
                }
            }
            HelpBlock::Image { image, text } => show_figure(app, ui, image, text),
            HelpBlock::Note { text } => {
                theme::banner(ui, theme::WARNING, &flow(text));
            }
        }
    }
    if !section.related.is_empty() {
        ui.add_space(10.0);
        theme::separator(ui);
        ui.label(
            RichText::new("RELATED")
                .size(theme::font_size(10.0))
                .strong()
                .color(theme::MUTED),
        );
        ui.horizontal_wrapped(|ui| {
            for anchor in &section.related {
                let title = bundle
                    .section(anchor)
                    .map(|section| section.title.clone())
                    .unwrap_or_else(|| anchor.clone());
                if theme::chip(ui, title, false).clicked() {
                    related = Some(anchor.clone());
                }
            }
        });
    }
    ui.add_space(8.0);
    ui.label(
        RichText::new(format!("{}  •  {}", bundle.attribution, bundle.source))
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
    );
    related
}

/// Reflow prose from the generated HTML: single newlines there are source-line
/// breaks, not intentional line breaks.
fn flow(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn show_figure(app: &mut LincApp, ui: &mut Ui, image: &str, caption: &str) {
    let ctx = ui.ctx().clone();
    if let Some(texture) = app.help.texture(&ctx, image) {
        let max = (ui.available_width() - 20.0).min(720.0);
        let initial = texture.size_vec2();
        let scale = (max / initial.x).min(1.0);
        let size = initial * scale;
        ui.add(egui::Image::new(egui::load::SizedTexture::new(texture.id(), size)).max_width(max));
    } else {
        ui.label(
            RichText::new(format!("[figure: {image}]"))
                .monospace()
                .size(theme::font_size(10.0))
                .color(theme::MUTED),
        );
    }
    if !caption.is_empty() {
        ui.label(
            RichText::new(caption)
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
    }
}
