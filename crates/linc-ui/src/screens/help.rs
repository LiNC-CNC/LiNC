//! Offline LinuxCNC G-code reference browser.
//!
//! One quick-reference table in the page; Enter opens the full section in a
//! near-fullscreen dialog with its own back history.
//!
//! The dialog renders the generated documentation natively: inline bold,
//! italic, code and link runs from the bundle, source-highlighted code blocks,
//! typed admonitions and documentation tables. There is no webview.

use std::borrow::Cow;

use egui::text::{LayoutJob, TextFormat, TextWrapping};
use egui::{
    Align, Color32, CornerRadius, Direction, FontFamily, FontId, Margin, Rect, RichText, Sense,
    Stroke, Ui, Vec2,
};
use linc_core::help::{
    HelpAdmonition, HelpBlock, HelpBundle, HelpItem, HelpRow, HelpSection, HelpSpan,
};

use crate::screens::common;
use crate::shell::LincApp;
use crate::theme;

// ---------------------------------------------------------------------------
// Probe Basic documentation palette.
//
// gcode_help.py (user_tabs/gcode_help) remaps the light-theme colours that
// source-highlight baked into the generated gcode_docs.py. Its _SYNTAX_REMAP is
// the source of the code-block colours below:
//
//   #0000FF  G/M words and keywords   ->  #7d8cf6
//   #000080  o-words                  ->  #a3b2ee
//   #000000  operators and other text ->  #d9d9d9
//   #FF6600  axis letters             ->  #ffb36b
//   #993399  numbers                  ->  #d99bd9
//   #9A1900  comments (italic)        ->  #e89f92
//   #009900  parameter references     ->  #93d493
//
// Its table constants are HEADER_BG, ZEBRA_BG and LINK_COLOR, and its generated
// index table uses cellpadding 3 with 10/22/28 % columns.
// ---------------------------------------------------------------------------

const SYNTAX_WORD: Color32 = Color32::from_rgb(0x7d, 0x8c, 0xf6);
const SYNTAX_OWORD: Color32 = Color32::from_rgb(0xa3, 0xb2, 0xee);
const SYNTAX_PLAIN: Color32 = Color32::from_rgb(0xd9, 0xd9, 0xd9);
const SYNTAX_AXIS: Color32 = Color32::from_rgb(0xff, 0xb3, 0x6b);
const SYNTAX_NUMBER: Color32 = Color32::from_rgb(0xd9, 0x9b, 0xd9);
const SYNTAX_COMMENT: Color32 = Color32::from_rgb(0xe8, 0x9f, 0x92);
const SYNTAX_PARAMETER: Color32 = Color32::from_rgb(0x93, 0xd4, 0x93);

/// Probe Basic HEADER_BG: the index table's header band and the table headers
/// of a documentation table.
const DOC_HEADER_BG: Color32 = Color32::from_rgb(0x36, 0x3d, 0x5e);
/// Probe Basic ZEBRA_BG: every other body row.
const DOC_ZEBRA_BG: Color32 = Color32::from_rgb(0x26, 0x2b, 0x2d);
/// Probe Basic LINK_COLOR, kept legible on the darker LiNC panel.
const DOC_LINK: Color32 = Color32::from_rgb(0x9a, 0xa3, 0xf7);
/// cellpadding 3 with the 1 px borders the generated docs use.
const DOC_CELL_PAD: f32 = 6.0;

const CODE_BG: Color32 = Color32::from_rgb(0x0f, 0x11, 0x13);
const INLINE_CODE_BG: Color32 = Color32::from_rgb(0x24, 0x28, 0x2e);

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
    if search_had_focus
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
    {
        anchor.request_focus();
    }
    let mut search_active = search_had_focus;
    // The search bar is the table's title bar: one text line, not the 44 px
    // touch-target row the theme would otherwise reserve.
    let one_line = ui.text_style_height(&egui::TextStyle::Body);
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = one_line + 2.0;
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
                    .margin(egui::Margin::symmetric(6, 2)),
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
    // The table is text rows, not touch targets; the theme's 44 px
    // interact_size made the column-title row taller than its captions.
    ui.spacing_mut().interact_size.y = 18.0;
    egui::Frame::new()
        .fill(theme::PANEL)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(egui::CornerRadius::same(6))
        .inner_margin(Margin {
            left: 12,
            right: 12,
            top: 4,
            bottom: 12,
        })
        .show(ui, |ui| {
            ui.set_min_height(ui.available_height());
            ui.spacing_mut().item_spacing.y = 0.0;
            let width = ui.available_width();
            let columns = Columns::new(width);
            header_row(ui, &columns);
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
                    ui.spacing_mut().item_spacing.y = 0.0;
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
/// meaning the rest, with one cell pad on each side of every column.
struct Columns {
    code: f32,
    params: f32,
    desc: f32,
    meaning: f32,
}

impl Columns {
    fn new(width: f32) -> Self {
        let span = (width - DOC_CELL_PAD * 5.0).max(240.0);
        let code = (span * 0.10).max(80.0);
        let params = (span * 0.22).max(150.0);
        let desc = (span * 0.28).max(170.0);
        let meaning = (span - code - params - desc).max(120.0);
        Self {
            code,
            params,
            desc,
            meaning,
        }
    }
}

/// One table row: the documentation's own padding, so the header band, the
/// group bands and the body rows share a left edge and a row rhythm.
fn doc_row_frame(fill: Color32) -> egui::Frame {
    egui::Frame::new()
        .fill(fill)
        .inner_margin(Margin {
            left: DOC_CELL_PAD as i8,
            right: DOC_CELL_PAD as i8,
            top: 2,
            bottom: 2,
        })
}

fn header_row(ui: &mut Ui, columns: &Columns) {
    // The column titles are one 18 px row; without this the theme's 44 px
    // interact_size makes the header band taller than the table it labels.
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = 18.0;
        doc_row_frame(DOC_HEADER_BG).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().item_spacing.x = DOC_CELL_PAD;
            ui.horizontal(|ui| {
                header_cell(ui, columns.code, "CODE", Align::Center);
                header_cell(ui, columns.params, "PARAMETERS", Align::Min);
                header_cell(ui, columns.desc, "DESCRIPTION", Align::Min);
                header_cell(ui, columns.meaning, "MEANING", Align::Min);
            });
        });
    });
}

fn header_cell(ui: &mut Ui, width: f32, text: &str, align: Align) {
    let label = RichText::new(text)
        .size(theme::font_size(9.0))
        .strong()
        .color(theme::FG);
    doc_cell(ui, width, align, label);
}

fn group_row(ui: &mut Ui, group: &linc_core::help::HelpGroup) {
    doc_row_frame(theme::ACCENT_DARK).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.spacing_mut().item_spacing.x = DOC_CELL_PAD;
        ui.horizontal_wrapped(|ui| {
            ui.label(
                RichText::new(&group.name)
                    .size(theme::font_size(10.0))
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
        DOC_ZEBRA_BG
    } else {
        Color32::TRANSPARENT
    };
    let inner = doc_row_frame(fill).show(ui, |ui| {
        ui.spacing_mut().item_spacing.x = DOC_CELL_PAD;
        ui.horizontal_top(|ui| {
            doc_cell(
                ui,
                columns.code,
                Align::Center,
                RichText::new(&row.code)
                    .monospace()
                    .size(theme::font_size(11.0))
                    .color(theme::ACCENT),
            );
            doc_cell(
                ui,
                columns.params,
                Align::Min,
                RichText::new(&row.params)
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(theme::FG),
            );
            doc_cell(
                ui,
                columns.desc,
                Align::Min,
                RichText::new(&row.desc)
                    .size(theme::font_size(11.0))
                    .color(theme::FG),
            );
            doc_cell(
                ui,
                columns.meaning,
                Align::Min,
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

/// One table cell: fixed width so columns line up between rows, wrapped text.
fn doc_cell(ui: &mut Ui, width: f32, align: Align, text: RichText) {
    let layout = match align {
        Align::Max => egui::Layout::right_to_left(Align::Min),
        Align::Center => egui::Layout::centered_and_justified(Direction::LeftToRight),
        _ => egui::Layout::left_to_right(Align::Min),
    };
    ui.allocate_ui_with_layout(Vec2::new(width, 0.0), layout, |ui| {
        ui.set_min_width(width);
        ui.add(egui::Label::new(text).wrap());
    });
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
    let mut clicked_link: Option<String> = None;
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
                    // A dialog opens with its safe button under the Enter key:
                    // CLOSE, unless the operator has moved the ring to BACK.
                    let holds_ring = close_button.has_focus()
                        || back_button.as_ref().is_some_and(|button| button.has_focus());
                    if !holds_ring {
                        close_button.request_focus();
                    }
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
                clicked_link = render_section(app, ui, &bundle, section, related_selection);
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
    } else if let Some(anchor) = clicked_link {
        // Inline links and the RELATED chips share one navigation path.
        app.help.dialog_link(&anchor);
    } else if let Some(dialog) = app.help.dialog.as_mut() {
        if dialog.scroll_to_top {
            dialog.scroll_to_top = false;
        }
    }
}

/// The section body without its title; returns a clicked anchor, if any.
fn render_section(
    app: &mut LincApp,
    ui: &mut Ui,
    bundle: &HelpBundle,
    section: &HelpSection,
    selected_related: Option<usize>,
) -> Option<String> {
    let mut clicked = None;
    ui.set_max_width(ui.available_width());
    for block in &section.blocks {
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
            HelpBlock::Para { text, spans } => {
                let spans = display_spans(bundle, text, spans);
                // Every block is drawn; only the first clicked link navigates.
                let hit = prose(ui, bundle, &spans, theme::font_size(12.0));
                if clicked.is_none() {
                    clicked = hit;
                }
            }
            HelpBlock::Code { text } => show_code(ui, text),
            HelpBlock::Table { rows, header } => show_table(ui, rows, *header),
            HelpBlock::List { items } => {
                for item in items {
                    match item {
                        HelpItem::ListItem { text, spans } => {
                            ui.horizontal_top(|ui| {
                                ui.label(RichText::new("•").color(theme::ACCENT));
                                let spans = display_spans(bundle, text, spans);
                                let hit = prose(ui, bundle, &spans, theme::font_size(12.0));
                                if clicked.is_none() {
                                    clicked = hit;
                                }
                            });
                        }
                        HelpItem::Image { image, text } => show_figure(app, ui, image, text),
                    }
                }
            }
            HelpBlock::Image { image, text } => show_figure(app, ui, image, text),
            HelpBlock::Admonition {
                admonition,
                text,
                spans,
            } => {
                ui.add_space(2.0);
                let hit = show_admonition(ui, bundle, *admonition, text, spans);
                if clicked.is_none() {
                    clicked = hit;
                }
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
                    clicked = Some(anchor.clone());
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
    clicked
}

/// Reflow prose from the generated HTML: single newlines there are source-line
/// breaks, not intentional line breaks.
fn flow(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A block's spans, or its plain text when the bundle predates them.
///
/// The generated docs link to sections this pinned bundle does not carry; such
/// a run keeps its text but loses the link, because a run that cannot navigate
/// must not be drawn as one.
fn display_spans<'a>(
    bundle: &HelpBundle,
    text: &'a str,
    spans: &'a [HelpSpan],
) -> Cow<'a, [HelpSpan]> {
    if spans.is_empty() {
        return Cow::Owned(vec![HelpSpan::plain(flow(text))]);
    }
    let dead = |span: &HelpSpan| {
        span.link
            .as_deref()
            .is_some_and(|anchor| bundle.section(anchor).is_none())
    };
    if !spans.iter().any(dead) {
        return Cow::Borrowed(spans);
    }
    Cow::Owned(
        spans
            .iter()
            .cloned()
            .map(|mut span| {
                if dead(&span) {
                    span.link = None;
                }
                span
            })
            .collect(),
    )
}

/// A clickable run of a laid-out paragraph, in character offsets.
#[derive(Clone, Debug, PartialEq, Eq)]
struct LinkRange {
    start: usize,
    end: usize,
    anchor: String,
}

/// Lay one paragraph out as a single wrapped job, recording where the link runs
/// sit so a pointer position can be resolved back to its anchor.
fn prose_job(spans: &[HelpSpan], size: f32, max_width: f32) -> (LayoutJob, Vec<LinkRange>) {
    let mut job = LayoutJob::default();
    job.wrap = TextWrapping::wrap_at_width(max_width);
    let mut ranges = Vec::new();
    let mut chars = 0usize;
    for span in spans {
        let mut format = TextFormat {
            font_id: FontId::new(size, FontFamily::Proportional),
            color: theme::FG,
            ..Default::default()
        };
        if span.bold {
            // egui ships no bold face; brightness plus tracking carries weight.
            format.color = Color32::WHITE;
            format.extra_letter_spacing = 0.35;
        }
        if span.italic {
            format.italics = true;
        }
        if span.code {
            format.font_id = FontId::new(size - 1.0, FontFamily::Monospace);
            format.color = SYNTAX_PLAIN;
            format.background = INLINE_CODE_BG;
        }
        if span.link.is_some() {
            format.color = DOC_LINK;
            format.underline = Stroke::new(1.0, DOC_LINK);
        }
        let count = span.text.chars().count();
        job.append(&span.text, 0.0, format);
        if let Some(anchor) = &span.link {
            ranges.push(LinkRange {
                start: chars,
                end: chars + count,
                anchor: anchor.clone(),
            });
        }
        chars += count;
    }
    (job, ranges)
}

/// The anchor of the link run containing a character index, if any.
fn link_at(ranges: &[LinkRange], index: usize) -> Option<&str> {
    ranges
        .iter()
        .find(|range| index >= range.start && index < range.end)
        .map(|range| range.anchor.as_str())
}

/// Resolve a clicked character index into an anchor this bundle can open.
///
/// The generated docs link to sections the pinned bundle does not carry; a dead
/// anchor must not navigate, because the dialog closes on an unknown section.
fn route_inline_link(bundle: &HelpBundle, ranges: &[LinkRange], index: usize) -> Option<String> {
    let anchor = link_at(ranges, index)?;
    bundle.section(anchor).map(|_| anchor.to_string())
}

/// A paragraph or list item: wrapped prose with bold, italic, code and clickable
/// internal links. Returns the anchor of an inline link that was clicked.
fn prose(ui: &mut Ui, bundle: &HelpBundle, spans: &[HelpSpan], size: f32) -> Option<String> {
    let (job, ranges) = prose_job(spans, size, ui.available_width());
    let galley = ui.ctx().fonts_mut(|fonts| fonts.layout_job(job));
    let sense = if ranges.is_empty() {
        Sense::hover()
    } else {
        Sense::click()
    };
    let response = ui.add(egui::Label::new(galley.clone()).wrap().sense(sense));
    let hit = |pos: egui::Pos2| {
        let cursor = galley.cursor_from_pos(pos - response.rect.min);
        route_inline_link(bundle, &ranges, cursor.index.0)
    };
    if response.hover_pos().and_then(hit).is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    if response.clicked() {
        response.interact_pointer_pos().and_then(hit)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Code blocks
// ---------------------------------------------------------------------------

/// The syntax classes Probe Basic highlights in a code block.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CodeClass {
    /// A G or M word; Probe Basic gives both the keyword blue.
    Word,
    /// An o-word label.
    Oword,
    /// An axis or parameter letter (X Y Z A B C U V W I J K R P Q).
    Axis,
    Number,
    /// A numbered or named parameter reference (#5070, #<feed>).
    Parameter,
    /// A parenthesized or semicolon comment.
    Comment,
    /// Operators, punctuation and unclassified text.
    Plain,
}

fn code_color(class: CodeClass) -> Color32 {
    match class {
        CodeClass::Word => SYNTAX_WORD,
        CodeClass::Oword => SYNTAX_OWORD,
        CodeClass::Axis => SYNTAX_AXIS,
        CodeClass::Number => SYNTAX_NUMBER,
        CodeClass::Parameter => SYNTAX_PARAMETER,
        CodeClass::Comment => SYNTAX_COMMENT,
        CodeClass::Plain => SYNTAX_PLAIN,
    }
}

fn code_format(size: f32, class: CodeClass) -> TextFormat {
    TextFormat {
        font_id: FontId::new(size, FontFamily::Monospace),
        color: code_color(class),
        italics: class == CodeClass::Comment,
        ..Default::default()
    }
}

/// The letters the docs colour as axis and parameter words.
fn is_axis_letter(letter: char) -> bool {
    "XYZABCUVWIJKRPQ".contains(letter)
}

/// Classify a documentation code block into coloured runs.
///
/// This is the docs reading of source-highlight's tokens, not the program
/// lexer: a reference code sample mixes G-code with plain words, so a letter is
/// a word only where the docs show one. A G/M/o word is a letter plus a number,
/// and an axis letter is an uppercase one that no other letter follows.
fn code_runs(text: &str) -> Vec<(CodeClass, &str)> {
    let mut runs: Vec<(CodeClass, &str)> = Vec::new();
    let mut plain: Option<usize> = None;
    let mut i = 0usize;

    macro_rules! flush_plain {
        ($end:expr) => {
            if let Some(start) = plain.take() {
                runs.push((CodeClass::Plain, &text[start..$end]));
            }
        };
    }

    while i < text.len() {
        let rest = &text[i..];
        let ch = rest.chars().next().unwrap();
        match ch {
            '(' => {
                // Nested parentheses stay part of the comment.
                let mut depth = 0i32;
                let mut end = text.len();
                for (offset, c) in rest.char_indices() {
                    match c {
                        '(' => depth += 1,
                        ')' => {
                            depth -= 1;
                            if depth == 0 {
                                end = i + offset + c.len_utf8();
                                break;
                            }
                        }
                        _ => {}
                    }
                }
                flush_plain!(i);
                runs.push((CodeClass::Comment, &text[i..end]));
                i = end;
            }
            ';' => {
                let end = rest.find('\n').map_or(text.len(), |offset| i + offset);
                flush_plain!(i);
                runs.push((CodeClass::Comment, &text[i..end]));
                i = end;
            }
            '#' => {
                let mut end = i + 1;
                let mut chars = text[end..].chars();
                if chars.next() == Some('<') {
                    for c in text[end..].chars() {
                        end += c.len_utf8();
                        if c == '>' {
                            break;
                        }
                    }
                } else {
                    for c in text[end..].chars() {
                        if !c.is_ascii_digit() {
                            break;
                        }
                        end += c.len_utf8();
                    }
                }
                flush_plain!(i);
                runs.push((CodeClass::Parameter, &text[i..end]));
                i = end;
            }
            c if c.is_ascii_alphabetic() => {
                let upper = c.to_ascii_uppercase();
                let next = text[i + c.len_utf8()..].chars().next();
                if matches!(upper, 'G' | 'M' | 'O') && next.is_some_and(|n| n.is_ascii_digit()) {
                    let mut end = i + c.len_utf8();
                    for n in text[end..].chars() {
                        if !(n.is_ascii_digit() || n == '.') {
                            break;
                        }
                        end += n.len_utf8();
                    }
                    flush_plain!(i);
                    let class = if upper == 'O' {
                        CodeClass::Oword
                    } else {
                        CodeClass::Word
                    };
                    runs.push((class, &text[i..end]));
                    i = end;
                } else if c.is_ascii_uppercase()
                    && is_axis_letter(c)
                    && !next.is_some_and(|n| n.is_ascii_alphabetic())
                {
                    flush_plain!(i);
                    runs.push((CodeClass::Axis, &text[i..i + c.len_utf8()]));
                    i += c.len_utf8();
                } else {
                    plain.get_or_insert(i);
                    i += c.len_utf8();
                }
            }
            c if c.is_ascii_digit() => {
                let mut end = i;
                for n in text[i..].chars() {
                    if !(n.is_ascii_digit() || n == '.') {
                        break;
                    }
                    end += n.len_utf8();
                }
                flush_plain!(i);
                runs.push((CodeClass::Number, &text[i..end]));
                i = end;
            }
            c => {
                plain.get_or_insert(i);
                i += c.len_utf8();
            }
        }
    }
    flush_plain!(text.len());
    runs
}

fn show_code(ui: &mut Ui, text: &str) {
    egui::Frame::new()
        .fill(CODE_BG)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(4)
        .inner_margin(8)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let mut job = LayoutJob::default();
            job.wrap = TextWrapping::wrap_at_width(ui.available_width());
            for (class, run) in code_runs(text) {
                job.append(run, 0.0, code_format(theme::font_size(11.0), class));
            }
            ui.add(egui::Label::new(job).wrap());
        });
}

// ---------------------------------------------------------------------------
// Admonitions
// ---------------------------------------------------------------------------

/// One admonition type's label, accent and background.
#[derive(Clone, Copy, Debug)]
struct AdmonitionStyle {
    label: &'static str,
    accent: Color32,
    fill: Color32,
}

/// Probe Basic's _ADMON_REMAP backgrounds, one per type, with a LiNC accent
/// each so the five read differently instead of sharing one warning banner.
///
/// Warning, Note and Tip are its exact fills. Probe Basic gives Important and
/// Caution near-identical browns (#443b2e and #463c2c); they are separated here
/// so the two are distinguishable, which is the point of the type.
fn admonition_style(kind: HelpAdmonition) -> AdmonitionStyle {
    match kind {
        HelpAdmonition::Warning => AdmonitionStyle {
            label: "WARNING",
            accent: theme::ERROR,
            fill: Color32::from_rgb(0x45, 0x30, 0x30),
        },
        HelpAdmonition::Note => AdmonitionStyle {
            label: "NOTE",
            accent: theme::ACCENT,
            fill: Color32::from_rgb(0x3f, 0x3c, 0x2c),
        },
        HelpAdmonition::Tip => AdmonitionStyle {
            label: "TIP",
            accent: theme::SUCCESS,
            fill: Color32::from_rgb(0x2f, 0x3f, 0x32),
        },
        HelpAdmonition::Important => AdmonitionStyle {
            label: "IMPORTANT",
            accent: theme::WARNING,
            fill: Color32::from_rgb(0x3a, 0x33, 0x42),
        },
        HelpAdmonition::Caution => AdmonitionStyle {
            label: "CAUTION",
            accent: theme::C5,
            fill: Color32::from_rgb(0x4b, 0x38, 0x28),
        },
    }
}

fn show_admonition(
    ui: &mut Ui,
    bundle: &HelpBundle,
    kind: HelpAdmonition,
    text: &str,
    spans: &[HelpSpan],
) -> Option<String> {
    let style = admonition_style(kind);
    let mut clicked = None;
    let inner = egui::Frame::new()
        .fill(style.fill)
        .stroke(Stroke::new(1.0, style.accent.linear_multiply(0.55)))
        .corner_radius(4)
        .inner_margin(Margin {
            left: 12,
            right: 10,
            top: 6,
            bottom: 6,
        })
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.label(
                RichText::new(style.label)
                    .size(theme::font_size(9.0))
                    .strong()
                    .color(style.accent),
            );
            let spans = display_spans(bundle, text, spans);
            clicked = prose(ui, bundle, &spans, theme::font_size(11.5));
        });
    let rect = inner.response.rect;
    ui.painter().rect_filled(
        Rect::from_min_size(rect.left_top(), Vec2::new(3.0, rect.height())),
        CornerRadius::same(1),
        style.accent,
    );
    clicked
}

// ---------------------------------------------------------------------------
// Documentation tables
// ---------------------------------------------------------------------------

/// Header band, alternating zebra rows and transparent otherwise.
fn table_row_fill(index: usize, header: bool) -> Color32 {
    if header && index == 0 {
        return DOC_HEADER_BG;
    }
    let body = index - usize::from(header);
    if body % 2 == 1 {
        DOC_ZEBRA_BG
    } else {
        Color32::TRANSPARENT
    }
}

/// Equal columns that fit the width, with one cell pad between each pair.
fn table_column_widths(width: f32, columns: usize) -> Vec<f32> {
    let columns = columns.max(1);
    let gaps = DOC_CELL_PAD * columns.saturating_sub(1) as f32;
    let cell = ((width - gaps) / columns as f32).max(80.0);
    vec![cell; columns]
}

fn is_numeric_cell(text: &str) -> bool {
    let text = text.trim();
    !text.is_empty()
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | '%'))
}

/// Header cells centre; numeric cells line up on the right so a column of
/// figures reads as one column.
fn cell_align(header: bool, text: &str) -> Align {
    if header {
        Align::Center
    } else if is_numeric_cell(text) {
        Align::Max
    } else {
        Align::Min
    }
}

fn show_table(ui: &mut Ui, rows: &[Vec<String>], header: bool) {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let widths = table_column_widths(ui.available_width(), columns);
    ui.scope(|ui| {
        ui.spacing_mut().item_spacing.y = 0.0;
        for (index, row) in rows.iter().enumerate() {
            let is_header = header && index == 0;
            doc_row_frame(table_row_fill(index, header)).show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.spacing_mut().item_spacing.x = DOC_CELL_PAD;
                ui.horizontal_top(|ui| {
                    for (column, cell) in row.iter().enumerate() {
                        let width = widths.get(column).copied().unwrap_or(80.0);
                        let text = RichText::new(cell)
                            .size(theme::font_size(10.5))
                            .color(theme::FG);
                        let text = if is_header { text.strong() } else { text };
                        doc_cell(ui, width, cell_align(is_header, cell), text);
                    }
                });
            });
        }
        ui.add_space(6.0);
    });
}

fn show_figure(app: &mut LincApp, ui: &mut Ui, image: &str, caption: &str) {
    let ctx = ui.ctx().clone();
    if let Some(texture) = app.help.texture(&ctx, image) {
        // The documentation figures are dark-on-transparent; the dialog is
        // dark too. Put them on a soft light plate so the drawing is legible
        // without leaving the graphite palette.
        egui::Frame::new()
            .fill(Color32::from_rgb(0xe4, 0xe8, 0xed))
            .stroke(egui::Stroke::new(1.0, theme::BORDER))
            .corner_radius(6)
            .inner_margin(10)
            .show(ui, |ui| {
                let max = (ui.available_width()).min(720.0);
                let initial = texture.size_vec2();
                let scale = (max / initial.x).min(1.0);
                let size = initial * scale;
                ui.add(
                    egui::Image::new(egui::load::SizedTexture::new(texture.id(), size))
                        .max_width(max),
                );
            });
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::HelpUi;

    fn link_span(text: &str, anchor: &str) -> HelpSpan {
        HelpSpan {
            text: text.to_string(),
            link: Some(anchor.to_string()),
            ..HelpSpan::default()
        }
    }

    /// Count the link runs the bundle can open and the ones it cannot.
    fn count_links(bundle: &HelpBundle, spans: &[HelpSpan], live: &mut usize, dead: &mut usize) {
        for span in spans {
            if let Some(anchor) = span.link.as_deref() {
                if bundle.section(anchor).is_some() {
                    *live += 1;
                } else {
                    *dead += 1;
                }
            }
        }
    }

    /// The spans of the first paragraph in a section that carries a link.
    fn linked_paragraph<'a>(bundle: &'a HelpBundle, anchor: &str) -> &'a [HelpSpan] {
        bundle
            .section(anchor)
            .unwrap()
            .blocks
            .iter()
            .find_map(|block| match block {
                HelpBlock::Para { spans, .. }
                    if spans.iter().any(|span| span.link.is_some()) =>
                {
                    Some(spans.as_slice())
                }
                _ => None,
            })
            .expect("a paragraph with an inline link")
    }

    #[test]
    fn inline_ranges_follow_the_span_text() {
        let spans = vec![
            HelpSpan::plain("See "),
            HelpSpan {
                text: "G2".to_string(),
                italic: true,
                ..HelpSpan::default()
            },
            HelpSpan::plain(" and "),
            link_span("the feed rate", "sec:set-feed-rate"),
            HelpSpan::plain(" section."),
        ];
        let (job, ranges) = prose_job(&spans, 12.0, 600.0);
        assert_eq!(job.text, "See G2 and the feed rate section.");
        assert_eq!(job.sections.len(), 5, "every span keeps its own format");
        assert!(job.sections[1].format.italics);
        assert_ne!(job.sections[3].format.underline, Stroke::NONE);
        assert_eq!(job.sections[3].format.color, DOC_LINK);
        assert_eq!(
            ranges,
            vec![LinkRange {
                start: 11,
                end: 24,
                anchor: "sec:set-feed-rate".to_string(),
            }]
        );
        assert_eq!(link_at(&ranges, 11), Some("sec:set-feed-rate"));
        assert_eq!(link_at(&ranges, 23), Some("sec:set-feed-rate"));
        assert_eq!(link_at(&ranges, 10), None);
        assert_eq!(link_at(&ranges, 24), None);
    }

    #[test]
    fn inline_link_clicks_route_through_the_dialog() {
        let bundle = HelpBundle::load_bundled().unwrap();
        let spans = linked_paragraph(&bundle, "gcode:g2-g3");
        let (_, ranges) = prose_job(spans, 12.0, 600.0);
        let range = ranges
            .iter()
            .find(|range| range.anchor == "sec:set-feed-rate")
            .expect("the feed rate link");
        let anchor = route_inline_link(&bundle, &ranges, range.start + 2).expect("open anchor");
        assert_eq!(anchor, "sec:set-feed-rate");
        // The click path is the same one the RELATED chips use.
        let mut help = HelpUi::default();
        help.open_dialog("gcode:g2-g3");
        help.dialog_link(&anchor);
        let dialog = help.dialog.expect("the dialog stays open");
        assert_eq!(dialog.anchor, "sec:set-feed-rate");
        assert_eq!(dialog.position, 1);
        assert_eq!(help.selected_anchor.as_deref(), Some("sec:set-feed-rate"));
    }

    #[test]
    fn links_outside_the_bundle_do_not_navigate() {
        let bundle = HelpBundle::load_bundled().unwrap();
        let spans = linked_paragraph(&bundle, "sec:set-feed-rate");
        let (_, ranges) = prose_job(spans, 12.0, 600.0);
        let range = ranges
            .iter()
            .find(|range| range.anchor == "sub:feed-rate")
            .expect("a link the bundle does not carry");
        assert!(bundle.section("sub:feed-rate").is_none());
        assert_eq!(route_inline_link(&bundle, &ranges, range.start), None);
    }

    #[test]
    fn blocks_without_spans_fall_back_to_their_text() {
        let bundle = HelpBundle::load_bundled().unwrap();
        let spans = display_spans(&bundle, "  reflow\n  sentence ", &[]);
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "reflow sentence");
        let existing = [HelpSpan::plain("kept")];
        assert_eq!(
            display_spans(&bundle, "ignored", &existing).as_ref(),
            &existing[..]
        );
    }

    #[test]
    fn inline_links_to_missing_sections_draw_as_plain_text() {
        let bundle = HelpBundle::load_bundled().unwrap();
        let spans = [
            HelpSpan::plain("see "),
            link_span("feed rate", "sec:set-feed-rate"),
            HelpSpan::plain(" and "),
            link_span("this dropped section", "gcode:g33-tech-info"),
        ];
        let shown = display_spans(&bundle, "", &spans);
        assert!(shown[1].link.is_some(), "a live anchor keeps its link");
        assert!(shown[3].link.is_none(), "a dead anchor loses its link");
        assert_eq!(shown[3].text, "this dropped section");
        let (job, ranges) = prose_job(&shown, 12.0, 600.0);
        assert_eq!(job.text, "see feed rate and this dropped section");
        assert_eq!(ranges.len(), 1, "only the live run is clickable");
        // A block with no dead links is borrowed, not copied.
        assert!(matches!(
            display_spans(&bundle, "", &spans[..2]),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn the_bundle_links_resolve_through_the_dialog_path() {
        let bundle = HelpBundle::load_bundled().unwrap();
        let mut live = 0;
        let mut dead = 0;
        for section in bundle.sections.values() {
            for block in &section.blocks {
                let spans: &[HelpSpan] = match block {
                    HelpBlock::Para { spans, .. } | HelpBlock::Admonition { spans, .. } => spans,
                    HelpBlock::List { items } => {
                        for item in items {
                            if let HelpItem::ListItem { spans, .. } = item {
                                count_links(&bundle, spans, &mut live, &mut dead);
                            }
                        }
                        continue;
                    }
                    _ => continue,
                };
                count_links(&bundle, spans, &mut live, &mut dead);
            }
        }
        assert!(live > 100, "expected the docs cross-references, saw {live}");
        assert!(dead > 0, "the pinned bundle drops some target sections");
        // Every dead anchor is stripped before layout, so no run is a dead link.
        for section in bundle.sections.values() {
            for block in &section.blocks {
                if let HelpBlock::Para { text, spans } = block {
                    let shown = display_spans(&bundle, text, spans);
                    for span in shown.iter() {
                        if let Some(anchor) = span.link.as_deref() {
                            assert!(bundle.section(anchor).is_some(), "{anchor}");
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn syntax_classes_match_the_probe_basic_remap() {
        assert_eq!(code_color(CodeClass::Word), Color32::from_rgb(0x7d, 0x8c, 0xf6));
        assert_eq!(code_color(CodeClass::Oword), Color32::from_rgb(0xa3, 0xb2, 0xee));
        assert_eq!(code_color(CodeClass::Axis), Color32::from_rgb(0xff, 0xb3, 0x6b));
        assert_eq!(code_color(CodeClass::Number), Color32::from_rgb(0xd9, 0x9b, 0xd9));
        assert_eq!(
            code_color(CodeClass::Comment),
            Color32::from_rgb(0xe8, 0x9f, 0x92)
        );
        assert_eq!(
            code_color(CodeClass::Parameter),
            Color32::from_rgb(0x93, 0xd4, 0x93)
        );
        assert_eq!(code_color(CodeClass::Plain), Color32::from_rgb(0xd9, 0xd9, 0xd9));
        assert!(code_format(11.0, CodeClass::Comment).italics);
        assert!(!code_format(11.0, CodeClass::Word).italics);
        assert_eq!(
            code_format(11.0, CodeClass::Axis).font_id.family,
            FontFamily::Monospace
        );
    }

    #[test]
    fn syntax_classification_follows_the_docs_markup() {
        assert_eq!(code_runs("G90.1"), vec![(CodeClass::Word, "G90.1")]);
        assert_eq!(code_runs("M6"), vec![(CodeClass::Word, "M6")]);
        assert_eq!(
            code_runs("o100 sub"),
            vec![(CodeClass::Oword, "o100"), (CodeClass::Plain, " sub")]
        );
        assert_eq!(
            code_runs("X10.5"),
            vec![(CodeClass::Axis, "X"), (CodeClass::Number, "10.5")]
        );
        assert_eq!(code_runs("#5070"), vec![(CodeClass::Parameter, "#5070")]);
        assert_eq!(code_runs("#<feed>"), vec![(CodeClass::Parameter, "#<feed>")]);
        assert_eq!(
            code_runs("(set unit)"),
            vec![(CodeClass::Comment, "(set unit)")]
        );
        assert_eq!(
            code_runs("G1 X0 ; home"),
            vec![
                (CodeClass::Word, "G1"),
                (CodeClass::Plain, " "),
                (CodeClass::Axis, "X"),
                (CodeClass::Number, "0"),
                (CodeClass::Plain, " "),
                (CodeClass::Comment, "; home"),
            ]
        );
        // A comment absorbs the words inside it.
        assert_eq!(
            code_runs("(go G0 home) G1"),
            vec![
                (CodeClass::Comment, "(go G0 home)"),
                (CodeClass::Plain, " "),
                (CodeClass::Word, "G1"),
            ]
        );
        // Prose inside a code sample is not G-code.
        assert_eq!(
            code_runs("axes offsets"),
            vec![(CodeClass::Plain, "axes offsets")]
        );
        assert_eq!(
            code_runs("Center Format Arcs"),
            vec![(CodeClass::Plain, "Center Format Arcs")]
        );
    }

    #[test]
    fn code_runs_cover_every_documentation_code_block() {
        let bundle = HelpBundle::load_bundled().unwrap();
        let mut blocks = 0;
        for section in bundle.sections.values() {
            for block in &section.blocks {
                if let HelpBlock::Code { text } = block {
                    let joined: String = code_runs(text).iter().map(|(_, run)| *run).collect();
                    assert_eq!(&joined, text, "{}", section.title);
                    blocks += 1;
                }
            }
        }
        assert!(blocks > 100, "expected the documentation code blocks");
    }

    #[test]
    fn every_admonition_type_has_its_own_treatment() {
        let kinds = [
            HelpAdmonition::Warning,
            HelpAdmonition::Note,
            HelpAdmonition::Tip,
            HelpAdmonition::Important,
            HelpAdmonition::Caution,
        ];
        let styles: Vec<AdmonitionStyle> = kinds.iter().map(|kind| admonition_style(*kind)).collect();
        for (index, style) in styles.iter().enumerate() {
            assert!(!style.label.is_empty());
            for other in &styles[index + 1..] {
                assert_ne!(style.label, other.label);
                assert_ne!(style.accent, other.accent);
                assert_ne!(style.fill, other.fill);
            }
        }
        assert_eq!(styles[0].label, "WARNING");
        assert_eq!(styles[1].label, "NOTE");
        // No two fills may read as the same tint: a distinct type needs a
        // visible difference, not merely an unequal one.
        for (index, style) in styles.iter().enumerate() {
            for other in &styles[index + 1..] {
                let delta = [
                    (style.fill.r() as i32 - other.fill.r() as i32).abs(),
                    (style.fill.g() as i32 - other.fill.g() as i32).abs(),
                    (style.fill.b() as i32 - other.fill.b() as i32).abs(),
                ];
                assert!(
                    delta.iter().any(|delta| *delta >= 8),
                    "{} and {} share one tint: {delta:?}",
                    style.label,
                    other.label
                );
            }
        }
    }

    #[test]
    fn the_bundle_carries_typed_admonitions() {
        let bundle = HelpBundle::load_bundled().unwrap();
        let mut warnings = 0;
        let mut notes = 0;
        for section in bundle.sections.values() {
            for block in &section.blocks {
                match block {
                    HelpBlock::Admonition { admonition, .. } => match admonition {
                        HelpAdmonition::Warning => warnings += 1,
                        HelpAdmonition::Note => notes += 1,
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
        assert!(warnings > 0, "no warning admonitions in the bundle");
        assert!(notes > 0, "no note admonitions in the bundle");
    }

    /// Rect fills and laid-out text colours a fixture paints, in draw order.
    fn painted(
        draw: impl FnOnce(&mut Ui),
    ) -> (Vec<Color32>, Vec<(String, Vec<Color32>)>) {
        fn collect(
            shape: &egui::Shape,
            fills: &mut Vec<Color32>,
            texts: &mut Vec<(String, Vec<Color32>)>,
        ) {
            match shape {
                egui::Shape::Rect(rect) => fills.push(rect.fill),
                egui::Shape::Text(text) => texts.push((
                    text.galley.job.text.clone(),
                    text.galley
                        .job
                        .sections
                        .iter()
                        .map(|section| section.format.color)
                        .collect(),
                )),
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        collect(shape, fills, texts);
                    }
                }
                _ => {}
            }
        }
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, Vec2::new(900.0, 700.0))),
            ..Default::default()
        };
        let mut fills = Vec::new();
        let mut texts = Vec::new();
        let mut draw = Some(draw);
        let mut output = ctx.run_ui(input, |ui| {
            if let Some(draw) = draw.take() {
                draw(ui);
            }
        });
        output.textures_delta.clear();
        for clipped in &output.shapes {
            collect(&clipped.shape, &mut fills, &mut texts);
        }
        (fills, texts)
    }

    #[test]
    fn a_documentation_table_paints_its_header_band_and_zebra_rows() {
        let rows = vec![
            vec!["P".to_string(), "1".to_string()],
            vec!["X".to_string(), "0.0".to_string()],
            vec!["Y".to_string(), "12.5".to_string()],
        ];
        let (fills, _) = painted(|ui| show_table(ui, &rows, true));
        assert!(fills.contains(&DOC_HEADER_BG), "no header band painted");
        assert!(fills.contains(&DOC_ZEBRA_BG), "no zebra row painted");
        // Without the flag no row claims the header band.
        let (fills, _) = painted(|ui| show_table(ui, &rows, false));
        assert!(!fills.contains(&DOC_HEADER_BG));
        assert!(fills.contains(&DOC_ZEBRA_BG));
    }

    #[test]
    fn a_code_block_paints_the_probe_basic_syntax_colours() {
        let (_, texts) = painted(|ui| show_code(ui, "G1 X10.5 (go home) #5070"));
        let (text, colours) = texts
            .iter()
            .find(|(text, _)| text.contains("G1 X10.5"))
            .expect("the code block is laid out");
        assert_eq!(text, "G1 X10.5 (go home) #5070");
        for (class, colour) in [
            (CodeClass::Word, SYNTAX_WORD),
            (CodeClass::Axis, SYNTAX_AXIS),
            (CodeClass::Number, SYNTAX_NUMBER),
            (CodeClass::Comment, SYNTAX_COMMENT),
            (CodeClass::Parameter, SYNTAX_PARAMETER),
            (CodeClass::Plain, SYNTAX_PLAIN),
        ] {
            assert!(
                colours.contains(&colour),
                "{class:?} colour {colour:?} is not in the painted runs {colours:?}"
            );
        }
    }

    #[test]
    fn an_admonition_paints_its_own_band() {
        let bundle = HelpBundle::load_bundled().unwrap();
        let mut seen = Vec::new();
        for kind in [
            HelpAdmonition::Warning,
            HelpAdmonition::Note,
            HelpAdmonition::Tip,
            HelpAdmonition::Important,
            HelpAdmonition::Caution,
        ] {
            let style = admonition_style(kind);
            let (fills, texts) = painted(|ui| {
                show_admonition(ui, &bundle, kind, "body text", &[]);
            });
            assert!(fills.contains(&style.fill), "{kind:?} band not painted");
            assert!(texts.iter().any(|(text, _)| text == style.label));
            seen.push(style.fill);
        }
        let unique: std::collections::BTreeSet<[u8; 4]> =
            seen.iter().map(|fill| fill.to_array()).collect();
        assert_eq!(unique.len(), seen.len(), "two types share one band");
    }

    #[test]
    fn table_header_band_is_distinct_from_the_body() {
        assert_eq!(table_row_fill(0, true), DOC_HEADER_BG);
        assert_eq!(table_row_fill(1, true), Color32::TRANSPARENT);
        assert_eq!(table_row_fill(2, true), DOC_ZEBRA_BG);
        assert_eq!(table_row_fill(0, false), Color32::TRANSPARENT);
        assert_eq!(table_row_fill(1, false), DOC_ZEBRA_BG);
        assert_ne!(DOC_HEADER_BG, DOC_ZEBRA_BG);
        assert_ne!(DOC_ZEBRA_BG, Color32::TRANSPARENT);
    }

    #[test]
    fn table_columns_leave_room_for_padding() {
        let widths = table_column_widths(600.0, 4);
        assert_eq!(widths.len(), 4);
        let gaps = DOC_CELL_PAD * 3.0;
        assert!((widths.iter().sum::<f32>() + gaps - 600.0).abs() < 0.01);
        assert!(widths.iter().all(|width| *width >= 80.0));
        assert_eq!(table_column_widths(100.0, 3).len(), 3);
        assert_eq!(table_column_widths(100.0, 0).len(), 1);
    }

    #[test]
    fn numeric_cells_align_right_and_headers_centre() {
        assert_eq!(cell_align(true, "12"), Align::Center);
        assert_eq!(cell_align(true, "G2"), Align::Center);
        assert_eq!(cell_align(false, "12.5"), Align::Max);
        assert_eq!(cell_align(false, "-3"), Align::Max);
        assert_eq!(cell_align(false, "G2"), Align::Min);
        assert_eq!(cell_align(false, "12 mm"), Align::Min);
    }

    #[test]
    fn index_columns_fit_the_table_width() {
        for width in [660.0f32, 1000.0, 1600.0] {
            let columns = Columns::new(width);
            let used =
                columns.code + columns.params + columns.desc + columns.meaning + DOC_CELL_PAD * 5.0;
            assert!(used <= width + 0.01, "{used} wider than {width}");
        }
    }
}
