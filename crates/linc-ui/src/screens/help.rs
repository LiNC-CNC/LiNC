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
    let anchor = common::focus_anchor_locked(ui, "help-page-focus", true);
    if ui.memory(|memory| memory.focused().is_none()) {
        anchor.request_focus();
    }
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

    let search_id = egui::Id::unique("linc-help-search");
    let search_had_focus = ui.memory(|memory| memory.has_focus(search_id));
    // Enter in the search bar belongs to the search: it hands the keyboard
    // back to the table and must not open the selected row in the same frame.
    if search_had_focus && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
        anchor.request_focus();
    }
    let mut search_active = search_had_focus;
    // The search bar is the table's title bar: one text line, not the 44 px
    // touch-target row the theme would otherwise reserve.
    let one_line = ui.text_style_height(&egui::TextStyle::Body);
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = one_line;
        ui.horizontal(|ui| {
            ui.label(
                RichText::new("SEARCH")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            let response = ui.add(
                egui::TextEdit::singleline(&mut app.help.query)
                    .id(search_id)
                    .hint_text("G2, G3, M6, o sub, arc …  ( / )")
                    .desired_width(360.0)
                    .margin(egui::Margin::symmetric(6, 1)),
            );
            if app.help.focus_search {
                response.request_focus();
                app.help.focus_search = false;
            }
            search_active = search_active || response.has_focus();
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
            let next = (current + delta).clamp(0, anchors.len() as i32 - 1) as usize;
            // At either end the key is a no-op: re-requesting the scroll there
            // made the first/last row flicker on every extra key press.
            if next != app.help.selected_row {
                app.help.selected_row = next;
                app.help.selected_anchor = Some(anchors[next].clone());
                app.help.scroll_to_selection = true;
            }
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
                            ui.memory_mut(|memory| memory.request_focus(anchor.id));
                        }
                        if response.double_clicked() {
                            ui.memory_mut(|memory| memory.request_focus(anchor.id));
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
    // The column titles are one 18 px row; without this the theme's 44 px
    // interact_size makes the header band taller than the table it labels.
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = 18.0;
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.horizontal(|ui| {
            header_cell(ui, columns.code, "CODE", true);
            header_cell(ui, columns.params, "PARAMETERS", false);
            header_cell(ui, columns.desc, "DESCRIPTION", false);
            header_cell(ui, columns.meaning, "MEANING", false);
        });
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
        .fill(Color32::BLACK)
        .inner_margin(Margin::symmetric(8, 2))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    RichText::new(&group.name)
                        .size(theme::font_size(10.5))
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
    let mut clicked_related: Option<String> = None;
    let mut open_related: Option<String> = None;
    let scroll_top = dialog.scroll_to_top;
    let mut related_selection = dialog.related;
    let related_count = section.related.len();
    // The dialog owns the keyboard while it is open: arrows and page keys
    // scroll it; once the bottom is reached, Down enters the RELATED chips,
    // which are then chosen with the cursor keys. Enter opens the chosen chip,
    // or closes when no chip is chosen; Escape always closes.
    let page = (size.y - 140.0).max(200.0);
    let mut scroll_delta = 0.0;
    let mut at_bottom = dialog.at_bottom;
    ctx.input_mut(|input| {
        let down = input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown);
        let up = input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp);
        let left = input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft);
        let right = input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight);
        let page_down = input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown);
        let page_up = input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp);
        let enter = input.consume_key(egui::Modifiers::NONE, egui::Key::Enter);
        if let Some(index) = related_selection {
            if left || up {
                // Stepping left/up from the first chip returns to the document.
                related_selection = index.checked_sub(1);
            }
            if right || down {
                related_selection = Some((index + 1).min(related_count.saturating_sub(1)));
            }
            if page_down {
                related_selection = None;
                scroll_delta += page;
            }
            if page_up {
                related_selection = None;
                scroll_delta -= page;
            }
            if enter {
                open_related = section.related.get(index).cloned();
            }
        } else {
            if down {
                if dialog.at_bottom && related_count > 0 {
                    related_selection = Some(0);
                } else {
                    scroll_delta += 40.0;
                }
            }
            if up {
                scroll_delta -= 40.0;
            }
            if page_down {
                scroll_delta += page;
            }
            if page_up {
                scroll_delta -= page;
            }
            if enter {
                close = true;
            }
        }
    });
    if let Some(dialog) = app.help.dialog.as_mut() {
        dialog.related = related_selection;
    }
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
                    let close_button = theme::action_button(ui, "CLOSE", true);
                    if close_button.clicked() {
                        close = true;
                    }
                    let back_button = if dialog.position > 0 {
                        let button = theme::action_button(ui, "BACK", true);
                        if button.clicked() {
                            back = true;
                        }
                        Some(button)
                    } else {
                        None
                    };
                    // Arrows belong to the document scroll, not to focus
                    // navigation between these two buttons.
                    for button in [Some(close_button), back_button].into_iter().flatten() {
                        ui.memory_mut(|memory| {
                            memory.set_focus_lock_filter(
                                button.id,
                                egui::EventFilter {
                                    tab: true,
                                    horizontal_arrows: true,
                                    vertical_arrows: true,
                                    escape: false,
                                },
                            )
                        });
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
            let output = area.show(ui, |ui| {
                if scroll_delta != 0.0 {
                    ui.scroll_with_delta(Vec2::new(0.0, -scroll_delta));
                }
                clicked_related =
                    render_section(app, ui, &bundle, section, related_selection);
            });
            at_bottom = output.state.clamped_offset().y + 2.0
                >= (output.content_size.y - output.inner_rect.height()).max(0.0);
        });
    if let Some(dialog) = app.help.dialog.as_mut() {
        dialog.at_bottom = at_bottom;
    }
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
    } else if let Some(anchor) = open_related {
        app.help.dialog_link(&anchor);
    } else if let Some(anchor) = clicked_related {
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
    selected_related: Option<usize>,
) -> Option<String> {
    let mut related = None;
    ui.set_max_width(ui.available_width());
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
                ui.add(
                    egui::Label::new(
                        RichText::new(flow(text))
                            .size(theme::font_size(12.0))
                            .color(theme::FG),
                    )
                    .wrap(),
                );
            }
            HelpBlock::Code { text } => {
                egui::Frame::new()
                    .fill(egui::Color32::from_rgb(0x0f, 0x11, 0x13))
                    .stroke(egui::Stroke::new(1.0, theme::BORDER))
                    .corner_radius(4)
                    .inner_margin(8)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        ui.add(
                            egui::Label::new(
                                RichText::new(text)
                                    .monospace()
                                    .size(theme::font_size(11.0))
                                    .color(theme::FG),
                            )
                            .wrap(),
                        );
                    });
            }
            HelpBlock::Table { rows } => {
                let columns = rows
                    .iter()
                    .map(Vec::len)
                    .max()
                    .unwrap_or(1)
                    .max(1);
                let cell_width = (ui.available_width() / columns as f32 - 8.0).max(80.0);
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
                            ui.allocate_ui_with_layout(
                                Vec2::new(cell_width, 0.0),
                                egui::Layout::top_down(Align::Min),
                                |ui| {
                                    ui.set_min_width(cell_width);
                                    ui.add(
                                        egui::Label::new(
                                            RichText::new(cell)
                                                .size(theme::font_size(10.5))
                                                .color(theme::FG),
                                        )
                                        .wrap(),
                                    );
                                },
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
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(flow(text))
                                            .size(theme::font_size(12.0))
                                            .color(theme::FG),
                                    )
                                    .wrap(),
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
            for (index, anchor) in section.related.iter().enumerate() {
                let title = bundle
                    .section(anchor)
                    .map(|section| section.title.clone())
                    .unwrap_or_else(|| anchor.clone());
                if theme::chip(ui, title, selected_related == Some(index)).clicked() {
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
