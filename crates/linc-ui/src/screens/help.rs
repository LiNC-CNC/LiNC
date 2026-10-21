//! Offline LinuxCNC G-code reference browser.

use egui::{Align, RichText, Ui, Vec2};
use linc_core::help::{HelpBlock, HelpBundle, HelpGroup, HelpItem};

use crate::screens::common;
use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    app.help.ensure_loaded();
    let Some(bundle) = app.help.bundle.clone() else {
        ui.label(RichText::new("HELP BUNDLE UNAVAILABLE").color(theme::ERROR));
        return;
    };
    // No page title: the navigation names the page.
    common::focus_anchor(ui, "help-page-focus");
    let filtered = bundle.filter(&app.help.query);
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
        let search_active = response.has_focus();
        let anchors: Vec<String> = filtered
            .iter()
            .flat_map(|group| group.rows.iter().map(|row| row.anchor.clone()))
            .collect();
        if !search_active {
            let mut delta: i32 = 0;
            let mut activate = false;
            let mut back = false;
            ui.input_mut(|input| {
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown) {
                    delta = 1;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp) {
                    delta = -1;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::Enter) {
                    activate = true;
                }
                if input.consume_key(egui::Modifiers::NONE, egui::Key::Backspace) {
                    back = true;
                }
            });
            if delta != 0 && !anchors.is_empty() {
                let current = app.help.selected_row.min(anchors.len().saturating_sub(1)) as i32;
                app.help.selected_row =
                    (current + delta).clamp(0, anchors.len().saturating_sub(1) as i32) as usize;
            }
            if activate {
                if let Some(anchor) = anchors.get(app.help.selected_row) {
                    app.help.open(anchor);
                }
            }
            if back {
                app.help.back();
            }
        }
        if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
            app.help.query.clear();
        }
        ui.label(
            RichText::new(format!(
                "{} SECTIONS / {} INDEX ROWS / {} FIGURES",
                bundle.counts.sections, bundle.counts.rows, bundle.counts.figures
            ))
            .monospace()
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
        );
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            if theme::action_button(ui, "BACK", !app.help.history.is_empty()).clicked() {
                app.help.back();
            }
        });
    });
    ui.add_space(4.0);
    let available = ui.available_width();
    let left_width = (available * 0.42).max(340.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(left_width, available_height(ui)),
            egui::Layout::top_down(egui::Align::Min),
            |ui| index_pane(app, ui, &bundle, &filtered),
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new(
                (available - left_width - 8.0).max(320.0),
                available_height(ui),
            ),
            egui::Layout::top_down(egui::Align::Min),
            |ui| detail_pane(app, ui, &bundle),
        );
    });
}

fn available_height(ui: &Ui) -> f32 {
    ui.available_height().max(300.0)
}

fn index_pane(app: &mut LincApp, ui: &mut Ui, bundle: &HelpBundle, groups: &[HelpGroup]) {
    let selected_anchor = app.help.selected_anchor.clone();
    let mut open: Option<String> = None;
    theme::card(ui, |ui| {
        egui::ScrollArea::vertical()
            .id_salt("help-index")
            .max_height(ui.available_height() - 10.0)
            .show(ui, |ui| {
                let mut index = 0usize;
                for group in groups {
                    ui.label(
                        RichText::new(&group.name)
                            .size(theme::font_size(13.0))
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
                    for row in &group.rows {
                        let selected = selected_anchor.as_deref() == Some(row.anchor.as_str());
                        let response = ui.add(
                            egui::Button::new(
                                RichText::new(format!("{:<18}  {}", row.code, row.desc))
                                    .monospace()
                                    .size(theme::font_size(11.0))
                                    .color(if selected { theme::ACCENT } else { theme::FG }),
                            )
                            .fill(if selected {
                                theme::ACCENT_DARK
                            } else {
                                theme::PANEL
                            })
                            .stroke(egui::Stroke::new(
                                1.0,
                                if selected {
                                    theme::ACCENT
                                } else {
                                    egui::Color32::TRANSPARENT
                                },
                            ))
                            .min_size(Vec2::new(ui.available_width(), 26.0)),
                        );
                        if response.clicked() {
                            open = Some(row.anchor.clone());
                            app.help.selected_row = index;
                        }
                        if app.help.selected_row == index {
                            let rect = response.rect;
                            ui.painter().rect_stroke(
                                rect,
                                3,
                                egui::Stroke::new(2.0, theme::ACCENT),
                                egui::StrokeKind::Inside,
                            );
                        }
                        index += 1;
                    }
                    ui.add_space(6.0);
                }
                app.help.selected_row = app
                    .help
                    .selected_row
                    .min(bundle.counts.rows.saturating_sub(1));
            });
    });
    if let Some(anchor) = open {
        app.help.open(&anchor);
    }
}

fn detail_pane(app: &mut LincApp, ui: &mut Ui, bundle: &HelpBundle) {
    let Some(anchor) = app.help.selected_anchor.clone() else {
        theme::card(ui, |ui| {
            ui.label(
                RichText::new("SELECT AN INDEX ROW OR PRESS ENTER")
                    .size(theme::font_size(14.0))
                    .color(theme::MUTED),
            );
            ui.label(
                RichText::new("Code search ignores spaces and commas, so g2g3 finds G2, G3.")
                    .size(theme::font_size(10.0))
                    .color(theme::MUTED),
            );
        });
        return;
    };
    let Some(section) = bundle.section(&anchor) else {
        theme::card(ui, |ui| {
            ui.label(RichText::new("SECTION NOT FOUND").color(theme::ERROR))
        });
        return;
    };
    let mut scroll_to_top = app.help.scroll_to_anchor;
    let mut related = None;
    theme::card(ui, |ui| {
        let mut area = egui::ScrollArea::vertical()
            .id_salt("help-detail")
            .max_height(ui.available_height() - 10.0);
        if scroll_to_top {
            area = area.vertical_scroll_offset(0.0);
        }
        scroll_to_top = false;
        area.show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new(&section.title)
                        .size(theme::font_size(22.0))
                        .strong()
                        .color(theme::FG),
                );
                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(bundle.version.clone())
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                });
            });
            ui.add_space(4.0);
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
                            RichText::new(text)
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
                            "help-table-{anchor}-{block_index}-{}",
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
                                            RichText::new(text)
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
                        theme::banner(ui, theme::WARNING, text);
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
                    for related_anchor in &section.related {
                        let title = bundle
                            .section(related_anchor)
                            .map(|section| section.title.clone())
                            .unwrap_or_else(|| related_anchor.clone());
                        if theme::chip(ui, title, false).clicked() {
                            related = Some(related_anchor.clone());
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
        });
    });
    app.help.scroll_to_anchor = false;
    if let Some(related) = related {
        app.help.open(&related);
    } else if scroll_to_top {
        app.help.scroll_to_anchor = true;
    }
}

fn show_figure(app: &mut LincApp, ui: &mut Ui, image: &str, caption: &str) {
    let ctx = ui.ctx().clone();
    if let Some(texture) = app.help.texture(&ctx, image) {
        let max = (ui.available_width() - 20.0).min(620.0);
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
