//! Machine dashboard, mill and lathe variants.

use egui::{RichText, Ui, Vec2};
use linc_core::model::MachineVariant;
use linc_view::ViewMode;

use crate::screens::common;
use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    // Machine is the default page, so it needs a focus stop for arrow-key
    // navigation to start from; it does not need a visible title.
    common::focus_anchor(ui, "machine-page-focus");
    if app.profile.variant == MachineVariant::Lathe {
        lathe(app, ui);
    } else {
        mill(app, ui);
    }
}

fn mill(app: &mut LincApp, ui: &mut Ui) {
    // The alternate dashboard is the coordinate matrix: it replaces the axis
    // readouts and the toolpath, and every other card stays where it was.
    let alternate = alternate_active(app);
    if !alternate {
        common::axis_cards(app, ui);
        ui.add_space(6.0);
    }

    let available = ui.available_width();
    let page_height = (ui.ctx().input(|input| input.viewport_rect().bottom())
        - theme::BOTTOM_BAR_HEIGHT
        - ui.cursor().min.y)
        .max(200.0);
    // The bottom cards are sized to their contents, not to a fixed strip: the
    // graph gets every pixel the feed/servo rows do not need.
    let bottom_height = bottom_cards_height(app, alternate);
    let graph_height = (page_height - bottom_height - 6.0).max(200.0);
    let graph_width = (available * 0.70).max(480.0);
    let right_width = (available - graph_width - 8.0).max(280.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(graph_width, page_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                if alternate {
                    coordinate_matrix(app, ui, graph_width, graph_height);
                } else {
                    ui.allocate_ui_with_layout(
                        Vec2::new(graph_width, graph_height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| graph_panel(app, ui, graph_height),
                    );
                }
                ui.add_space(6.0);
                // The square key swaps the feed/servo pair for the
                // following-error traces: one row of cards, one subject.
                if app.following.active {
                    bottom_card(
                        app,
                        ui,
                        bottom_row_card_width(graph_width, 1.0, 180.0),
                        bottom_height,
                        "following",
                        |app, ui, _h| common::following_error_card(app, ui, bottom_height),
                    );
                } else {
                    // The second card is the load meters when the machine
                    // publishes load, the cut's own numbers when it does not,
                    // or whatever `[ui] second_card` forces: a position-only
                    // controller has no load to meter, and three bars at 0 %
                    // stand for a light cut that was never measured.
                    let second = second_card(
                        &app.profile.config.ui.second_card,
                        !app.servo_load_rows().is_empty(),
                    );
                    // CUT does not need half the strip: it carries three short
                    // rows, so it shares the second slot with TLO and the pair
                    // reads at a glance without the width the meters want. The
                    // alternate dashboard already shows those offsets in its own
                    // card, so its strip keeps CUT at the half the meters use.
                    let with_tlo = second == Some(Second::Cut) && !alternate;
                    // FEED / SPINDLE keeps half the strip; CUT and TOOL OFFSETS
                    // take only the width their figures need, measured here so a
                    // card with three short rows is not given a third of the bar
                    // to fill with air.
                    let (feed_width, second_width, tlo_width) = bottom_strip_widths(
                        graph_width,
                        second,
                        with_tlo,
                        if second == Some(Second::Cut) {
                            crate::screens::cut::natural_width(ui, app)
                        } else {
                            0.0
                        },
                        if with_tlo {
                            crate::screens::tlo::natural_width(ui, app)
                        } else {
                            0.0
                        },
                        common::feed_speed_natural_width(ui, app),
                    );
                    ui.horizontal_top(|ui| {
                        // The explicit gap below is the whole gap: the default
                        // item spacing would add 8 more per card and push the
                        // servo card past the graph's right edge.
                        ui.spacing_mut().item_spacing.x = 0.0;
                        bottom_card(app, ui, feed_width, bottom_height, "feed", |app, ui, _h| {
                            common::feed_speed_card(app, ui, bottom_height);
                        });
                        let Some(second) = second else {
                            return;
                        };
                        ui.add_space(CARD_GAP);
                        bottom_card(
                            app,
                            ui,
                            second_width,
                            bottom_height,
                            second.id(),
                            |app, ui, _h| second.draw(app, ui, bottom_height),
                        );
                        if with_tlo {
                            ui.add_space(CARD_GAP);
                            bottom_card(app, ui, tlo_width, bottom_height, "tlo", |app, ui, _h| {
                                crate::screens::tlo::tlo_card(app, ui, bottom_height);
                            });
                        }
                    });
                }
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new(right_width, page_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                let pane_top = ui.cursor().min.y;
                let viewport = ui.ctx().input(|input| input.viewport_rect());
                let pane_bottom = viewport.bottom() - theme::BOTTOM_BAR_HEIGHT + 15.0;
                let pane_width = (viewport.right() - 16.0 - ui.cursor().min.x).max(120.0);
                ui.set_width(pane_width);
                let card_height = (pane_bottom - pane_top - 2.0).max(200.0);
                // The panel position decides which card owns this slot: while
                // it sits in MDI the program card is replaced by the pane.
                if app.mdi_pane_visible() {
                    crate::screens::mdi::pane(app, ui, card_height);
                } else {
                    common::program_card_sized(app, ui, card_height);
                }
            },
        );
    });
}

/// Which card sits beside FEED / SPINDLE on the Machine page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Second {
    Load,
    Cut,
}

impl Second {
    fn id(self) -> &'static str {
        match self {
            Self::Load => "servo",
            Self::Cut => "cut",
        }
    }

    fn draw(self, app: &mut LincApp, ui: &mut Ui, height: f32) {
        match self {
            Self::Load => common::servo_load_card(&app.servo_load_rows(), ui, height),
            Self::Cut => crate::screens::cut::cut_card(app, ui, height),
        }
    }
}

/// Resolve `[ui] second_card`. `auto` follows what the machine publishes: the
/// meters when there are rows to draw, the cut's numbers when there are none.
/// `load` keeps the meters even with nothing to show, and the card then says
/// that the machine publishes no load rather than drawing an empty frame.
fn second_card(policy: &str, has_load: bool) -> Option<Second> {
    match policy {
        "load" => Some(Second::Load),
        "cut" => Some(Second::Cut),
        "none" => None,
        // The profile is validated at load, so an unknown value here is an
        // "auto" spelled another way.
        _ => Some(if has_load { Second::Load } else { Second::Cut }),
    }
}

/// Height that just holds the bottom row: the feed/spindle block needs a fixed
/// body, and every servo row adds its meter height above the card's title and
/// margins. A machine with no load rows draws the CUT card, which fits the feed
/// block's own height — until TLO joins it, since that card carries a row per
/// offset the tool table declares and has its own height to ask for.
///
/// The alternate dashboard has no TLO in the strip: its own `TOOL LENGTH
/// OFFSET` card already carries the same readout, so the strip stays the feed
/// block's height there.
fn bottom_cards_height(app: &LincApp, alternate: bool) -> f32 {
    const FEED: f32 = 100.0;
    // Three rows and room to read them; the rows divide whatever is left after
    // the card's own header, so a shorter strip is not clipped.
    const FOLLOWING: f32 = 132.0;
    if app.following.active {
        return FOLLOWING;
    }
    let rows = app.servo_load_rows().len();
    let mut height = FEED.max(common::servo_load_card_height(rows));
    let second = second_card(&app.profile.config.ui.second_card, rows > 0);
    // The lathe keeps the feed/CUT pair: TLO is the mill's, so its rows do not
    // grow the lathe's strip.
    if !alternate && second == Some(Second::Cut) && app.profile.variant != MachineVariant::Lathe {
        let lengths = crate::screens::tlo::read(
            &app.snapshot,
            &app.profile.machine_ini.tool_offset_columns(),
            app.profile.variant,
        );
        height = height.max(crate::screens::tlo::card_height(lengths.line_count()));
    }
    height
}

/// Width of one bottom card so that `cards` cards and the 8 px gaps between
/// them fill `column` exactly. `min` is the smallest usable card.
fn bottom_row_card_width(column: f32, cards: f32, min: f32) -> f32 {
    ((column - 8.0 * (cards - 1.0)) / cards).max(min)
}

/// Gap the strip leaves between two cards.
const CARD_GAP: f32 = 8.0;

/// Share of the strip FEED / SPINDLE keeps when it is alone with one other card.
const FEED_SHARE: f32 = 0.5;

/// Width a value card needs for its rows and its title, margins included.
///
/// A row puts its label at the left, its value and units at the right, so the
/// card is as wide as the widest of `label + value + units`, or as its title if
/// that is wider. Anything more is empty border.
pub(super) fn value_card_width(ui: &Ui, title: &str, rows: &[common::ValueRow]) -> f32 {
    let label_font = egui::FontId::monospace(theme::font_size(9.0));
    let value_font = egui::FontId::monospace(theme::font_size(11.0));
    let title_font = egui::FontId::proportional(theme::font_size(12.0));
    let width = |text: &str, font: &egui::FontId| {
        ui.fonts_mut(|fonts| {
            fonts
                .layout_no_wrap(text.to_string(), font.clone(), theme::FG)
                .size()
                .x
        })
    };
    // The badge beside the title (CUT has none, TOOL OFFSETS its tool number)
    // takes the title's own right end; adding it to the title's width keeps the
    // header from crowding the first row.
    let mut widest = width(title, &title_font) + 28.0;
    for row in rows {
        let mut row_width = width(&row.label, &label_font) + 10.0;
        if let Some(lead) = &row.lead {
            row_width += width(lead, &label_font) + 5.0;
        }
        row_width += width(&row.value, &value_font);
        if !row.units.is_empty() {
            row_width += width(&row.units, &label_font) + 5.0;
        }
        widest = widest.max(row_width);
    }
    (widest + 2.0 * theme::CARD_MARGIN).ceil()
}

/// The strip's three widths: FEED / SPINDLE, the second card, and TOOL OFFSETS.
///
/// The row fills the column and ends where the toolpath does, because the offsets
/// card is pinned to that right edge - it is the card the eye compares against
/// the graph above, so the two must end together. CUT takes the width its three
/// short rows need and no more. FEED / SPINDLE absorbs the remainder: its bars
/// read as bars, and it is the one card that gets better with the space.
fn bottom_strip_widths(
    column: f32,
    second: Option<Second>,
    with_tlo: bool,
    cut_width: f32,
    tlo_width: f32,
    feed_width_needed: f32,
) -> (f32, f32, f32) {
    let gaps = match (second, with_tlo) {
        (None, _) => 0.0,
        (Some(_), true) => 2.0 * CARD_GAP,
        (Some(_), false) => CARD_GAP,
    };
    let usable = (column - gaps).max(0.0);
    // Alone in the row, the feed card fills it.
    if second.is_none() {
        return (usable.max(180.0), 0.0, 0.0);
    }
    // FEED / SPINDLE keeps what its padded figures need; the other two give way
    // to it rather than the reverse, because its numbers are the ones that grow.
    let feed_floor = feed_width_needed.max(150.0);
    // The offsets card keeps its own width, but never so much that the cards
    // beside it fall below a readable minimum.
    let tlo = if with_tlo {
        tlo_width.clamp(0.0, (usable - feed_floor - 150.0).max(0.0))
    } else {
        0.0
    };
    let shared = (usable - tlo).max(0.0);
    match second {
        // The load meters are bars, not rows: they keep the even split.
        Some(Second::Load) => {
            let feed = (shared * FEED_SHARE).max(150.0).min(shared);
            (feed, shared - feed, tlo)
        }
        // CUT takes what its rows need; the feed card gets the rest of the row.
        Some(Second::Cut) => {
            let cut = cut_width.clamp(0.0, (shared - feed_floor).max(0.0));
            (shared - cut, cut, tlo)
        }
        None => (shared, 0.0, tlo),
    }
}

fn bottom_card(
    app: &mut LincApp,
    ui: &mut Ui,
    width: f32,
    height: f32,
    id: &str,
    add: impl FnOnce(&mut LincApp, &mut Ui, f32),
) {
    ui.allocate_ui_with_layout(
        Vec2::new(width, height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_min_width(width);
            ui.set_max_width(width);
            let _ = id;
            add(app, ui, height);
        },
    );
}

/// True when the alternate dashboard is both asked for and offered: the
/// operator pressed MAIN on MAIN, and the mode is one whose screen MAIN is.
pub(super) fn alternate_active(app: &LincApp) -> bool {
    app.main_matrix_active()
}

/// One line of a matrix card: the axis name and the figure beside it.
struct CoordLine {
    label: String,
    value: String,
    /// An axis with no number to show is dimmed rather than dropped.
    muted: bool,
}

/// The gap between the matrix's cards. The manual page's third column takes it
/// too, so the toolpath beside the matrix lines up with the cells.
pub(super) const MATRIX_GAP: f32 = 8.0;

/// One cell of a matrix drawn into a `width` × `height` column.
///
/// The manual page reads it to size the toolpath card it draws in the column
/// beside the matrix: two cells across the matrix column plus the gap, one cell
/// across the graph column, and all three cards on the top row are the same
/// size.
pub(super) fn matrix_cell(width: f32, height: f32) -> Vec2 {
    Vec2::new(
        ((width - MATRIX_GAP) / 2.0).max(160.0),
        ((height - MATRIX_GAP) / 2.0).max(120.0),
    )
}

/// The alternate dashboard: four coordinate cards in the place the axis
/// readouts and the toolpath take on the default one.
///
/// The WORK COORDINATES card is the one the operator may write: it is the active
/// WCS, so it takes the OFFSETS page's in-place edit — a click on a row, or in
/// the manual modes the axis's letter key, Enter to write, Escape to drop — and
/// writes through the same `SetWorkPosition` path the page's DRO does.
pub(super) fn coordinate_matrix(app: &mut LincApp, ui: &mut Ui, width: f32, height: f32) {
    let cell = matrix_cell(width, height);
    let axes = app.axes();
    let diameter = app.reads_x_as_diameter();
    // The OFFSETS page's own handlers, against the same active WCS: the letters
    // open the edit, Enter writes it, Escape drops it. Called here rather than by
    // the page because the card only exists with the matrix in front — on the
    // dashboard face those letters belong to nothing else either, and the zeroing
    // they used to do on the manual page is gone.
    //
    // The letters are the manual modes' only: in AUTO and MDI the thing being
    // typed is a block, and `X` there is the block's X word rather than a
    // shortcut into the offset. Enter and Escape still reach a field the operator
    // opened with the mouse, in every mode.
    if app.matrix_dro_keys_active() {
        crate::screens::offsets::handle_axis_keys(app, ui);
    }
    crate::screens::offsets::handle_dro_keys(app, ui);
    // One read of the snapshot for all four cards, so the matrix cannot show two
    // different frames.
    let positions: Vec<(f64, f64, f64)> = axes
        .iter()
        .map(|axis| common::axis_display(app, axis).unwrap_or((0.0, 0.0, 0.0)))
        .collect();
    // Every axis name carries the lathe's DIA mark, as it does on the DROs the
    // matrix replaces.
    let labels: Vec<String> = axes
        .iter()
        .map(|axis| common::dro_axis_label(axis, diameter))
        .collect();
    let rows_of = |pick: fn(&(f64, f64, f64)) -> f64| -> Vec<CoordLine> {
        positions
            .iter()
            .zip(labels.iter())
            .map(|(position, label)| CoordLine {
                label: label.clone(),
                value: theme::format_position(pick(position), 3),
                muted: false,
            })
            .collect()
    };
    // The X of a lathe is already a diameter on the DRO row; the distance to go
    // is a distance, so it is never doubled.
    let work = rows_of(|position| position.0);
    let machine = rows_of(|position| position.1);
    let to_go = rows_of(|position| position.2);
    let lengths = crate::screens::tlo::read(
        &app.snapshot,
        &app.profile.machine_ini.tool_offset_columns(),
        app.profile.variant,
    );
    let tool: Vec<CoordLine> = lengths
        .lines(&app.snapshot)
        .into_iter()
        .map(|(label, value, muted)| CoordLine {
            label,
            value,
            muted,
        })
        .collect();
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        ui.horizontal_top(|ui| {
            work_coordinates_card(app, ui, cell, &work, &axes);
            ui.add_space(MATRIX_GAP);
            matrix_card(ui, cell, "MACHINE COORDINATES", None, &machine);
        });
        ui.add_space(MATRIX_GAP);
        ui.horizontal_top(|ui| {
            matrix_card(ui, cell, "DISTANCE TO GO", None, &to_go);
            ui.add_space(MATRIX_GAP);
            matrix_card(
                ui,
                cell,
                "TOOL OFFSETS",
                Some(format!("T{}", lengths.tool)),
                &tool,
            );
        });
    });
}

/// One card of the matrix: a title, an optional badge at its right end (the
/// active WCS, the loaded tool) and the lines under it.///
/// The title is painted by [`common::value_card`], not laid out in a row: a
/// laid-out title sat 14 px below the card's own margin, and that blank band
/// was the first thing the eye met inside every card.
fn matrix_card(ui: &mut Ui, cell: Vec2, title: &str, badge: Option<String>, lines: &[CoordLine]) {
    ui.allocate_ui_with_layout(cell, egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_min_width(cell.x);
        ui.set_max_width(cell.x);
        let badge = badge.as_deref().map(|badge| (badge, theme::ACCENT));
        let body = common::value_card(ui, cell.y, title, badge);
        paint_coordinate_lines(ui, body, lines, None);
    });
}

/// The matrix's one writable card: the active WCS.
///
/// A click on a row opens the same in-place edit the OFFSETS page's DRO uses,
/// seeded from the controller's own work position, and Enter writes it back
/// through `SetWorkPosition`; the keys are that page's (`handle_axis_keys`,
/// `handle_dro_keys`), so the two cannot drift. The letter keys are the manual
/// modes' (`matrix_dro_keys_active`); the click is the pointer's own request, so
/// it opens the edit in every mode the card is drawn in.
fn work_coordinates_card(
    app: &mut LincApp,
    ui: &mut Ui,
    cell: Vec2,
    lines: &[CoordLine],
    axes: &[String],
) {
    let writable = app.snapshot.writes_allowed() && app.snapshot.offsets.active_index.is_some();
    let skip = app
        .offsets
        .dro_edit
        .as_ref()
        .and_then(|edit| axes.iter().position(|axis| *axis == edit.axis));
    let mut start: Option<String> = None;

    ui.allocate_ui_with_layout(cell, egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_min_width(cell.x);
        ui.set_max_width(cell.x);
        let wcs = app.active_wcs_label();
        let badge = Some((wcs.as_str(), theme::ACCENT));
        let body = common::value_card(ui, cell.y, "WORK COORDINATES", badge);
        paint_coordinate_lines(ui, body, lines, skip);
        let rows = lines.len().max(1);
        let row_height = body.height() / rows as f32;
        let size = theme::font_size(line_base(rows, body.height()));
        for (index, axis) in axes.iter().enumerate() {
            if index >= lines.len() {
                break;
            }
            let row = egui::Rect::from_min_size(
                egui::pos2(body.left(), body.top() + index as f32 * row_height),
                egui::vec2(body.width(), row_height),
            );
            if skip == Some(index) {
                matrix_edit_field(app, ui, row, size);
                continue;
            }
            let response = ui.interact(
                row,
                egui::Id::unique(("matrix-work-row", index)),
                egui::Sense::click(),
            );
            if writable && response.hovered() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::Text);
            }
            if writable && response.clicked() {
                start = Some(axis.clone());
            }
        }
    });

    let Some(axis) = start else { return };
    let work = common::axis_display(app, &axis)
        .map(|(work, _, _)| work)
        .unwrap_or(0.0);
    app.offsets.dro_edit = Some(crate::state::DroEdit {
        axis,
        buffer: format!("{work:.3}"),
        focus: true,
    });
}

/// The in-place field over the edited row, pre-selecting the reading the way the
/// OFFSETS page does: the operator is replacing a number, not appending to it.
fn matrix_edit_field(app: &mut LincApp, ui: &mut Ui, row: egui::Rect, size: f32) {
    let height = (row.height() * 0.8).clamp(20.0, 34.0);
    let field = egui::Rect::from_min_max(
        egui::pos2(
            row.left() + row.width() * 0.28,
            row.center().y - height * 0.5,
        ),
        egui::pos2(row.right(), row.center().y + height * 0.5),
    );
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(field)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    let mut buffer = app
        .offsets
        .dro_edit
        .as_ref()
        .map(|edit| edit.buffer.clone())
        .unwrap_or_default();
    let id = egui::Id::unique(crate::state::DRO_FIELD_ID);
    let edit = child.add_sized(
        [field.width(), height],
        egui::TextEdit::singleline(&mut buffer)
            .id(id)
            .align(egui::Align2::RIGHT_CENTER)
            .font(egui::FontId::monospace(size))
            .margin(egui::Margin::symmetric(4, 0)),
    );
    if app
        .offsets
        .dro_edit
        .as_ref()
        .map(|edit| edit.focus)
        .unwrap_or(false)
    {
        edit.request_focus();
        if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), id) {
            state
                .cursor
                .set_char_range(Some(egui::text::CCursorRange::two(
                    egui::text::CCursor::new(0),
                    egui::text::CCursor::new(buffer.chars().count()),
                )));
            state.store(ui.ctx(), id);
        }
        if let Some(slot) = app.offsets.dro_edit.as_mut() {
            slot.focus = false;
        }
    }
    if edit.changed() {
        if let Some(slot) = app.offsets.dro_edit.as_mut() {
            slot.buffer = buffer;
        }
    }
}

/// Paint the lines of a matrix card: the axis name in the DRO's label colour at
/// the left inset, its figure at the right edge. `skip` leaves one row's figure
/// unpainted: the edit field covers it while that axis is being typed.
fn paint_coordinate_lines(ui: &Ui, rect: egui::Rect, lines: &[CoordLine], skip: Option<usize>) {
    if lines.is_empty() {
        return;
    }
    let font = egui::FontId::monospace(theme::font_size(line_base(lines.len(), rect.height())));
    let row = rect.height() / lines.len() as f32;
    let painter = ui.painter_at(rect);
    for (index, line) in lines.iter().enumerate() {
        let y = rect.top() + (index as f32 + 0.5) * row;
        painter.text(
            egui::pos2(rect.left(), y),
            egui::Align2::LEFT_CENTER,
            &line.label,
            font.clone(),
            theme::C3,
        );
        if skip == Some(index) {
            continue;
        }
        painter.text(
            egui::pos2(rect.right(), y),
            egui::Align2::RIGHT_CENTER,
            &line.value,
            font.clone(),
            if line.muted { theme::MUTED } else { theme::FG },
        );
    }
}

/// Base type size for a matrix card's lines: the largest on the ramp whose row
/// still holds the DRO line's air, so a five-axis machine stays readable and a
/// three-axis one gets the big figures the matrix exists for.
fn line_base(rows: usize, height: f32) -> f32 {
    // Bases on the type ramp, largest first.
    const CANDIDATES: [f32; 5] = [30.0, 26.0, 22.0, 18.0, 16.0];
    let per_row = height / rows.max(1) as f32;
    CANDIDATES
        .into_iter()
        .find(|base| theme::font_size(*base) * 1.45 <= per_row)
        .unwrap_or(CANDIDATES[CANDIDATES.len() - 1])
}

fn graph_panel(app: &mut LincApp, ui: &mut Ui, height: f32) {
    let preview = app.program.loaded().last_valid_preview.clone();
    let mut view = app.program.graph_view;
    if view == ViewMode::LatheXz {
        view = ViewMode::Iso3d;
    }
    egui::Frame::new()
        .fill(theme::PANEL)
        .stroke(egui::Stroke::new(1.0, theme::BORDER))
        .corner_radius(6)
        .inner_margin(egui::Margin::same(4))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.spacing_mut().interact_size.y = 14.0;
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.set_max_height(height);
            // One legend implementation for every toolpath card: the
            // main window had its own copy, which is why its tool key was a
            // circle while the others were already a triangle.
            let header = common::toolpath_header(ui, app.graph_style.remaining);
            let mut zoom = app.program.graph_zoom;
            let mut pan = app.program.graph_pan;
            let mut yaw = app.program.graph_yaw;
            let mut pitch = app.program.graph_pitch;
            let mut fit = app.program.fit_requested;
            let mut auto_fit = app.program.graph_auto_fit;
            let drawn = common::trajectory(
                app,
                ui,
                &preview,
                (height - 36.0).max(120.0),
                view,
                &mut zoom,
                &mut pan,
                &mut yaw,
                &mut pitch,
                &mut fit,
                &mut auto_fit,
                Some(app.program.loaded().selected_line),
                app.snapshot.program.highlight_line(),
                false,
                false,
                false,
                false,
                false,
                true,
                None,
            );
            app.program.graph_zoom = zoom;
            app.program.graph_pan = pan;
            app.program.graph_yaw = yaw;
            app.program.graph_pitch = pitch;
            app.program.fit_requested = false;
            app.program.graph_auto_fit = auto_fit;
            common::toolpath_status(ui, header, view.label(), drawn.span_mm, None);
            // The block the MDI pane is composing, drawn over the program in
            // the pane's own colour. Nothing is drawn for an empty or
            // unparsable block, and the pane is only up in MDI.
            crate::screens::mdi::draw_overlay(
                app,
                ui,
                drawn.response.rect,
                view,
                zoom,
                pan,
                yaw,
                pitch,
            );
            ui.horizontal(|ui| {
                let program = if app.snapshot.program.display_name.is_empty() {
                    "NO PROGRAM".to_string()
                } else {
                    app.snapshot.program.display_name.clone()
                };
                ui.label(
                    RichText::new(program)
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                // The loop the run is in, in the same words as the PROGRAM
                // page's status line. Dropped first when the card is narrow:
                // the listing carries it too.
                if let Some(readout) = crate::state::LoopReadout::current(
                    &app.program.loaded().preview,
                    &app.program.loop_progress,
                ) {
                    ui.label(
                        RichText::new("\u{00b7}")
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                    // One line: a narrow card cuts the innermost loop rather
                    // than wrapping the status row into a second line.
                    ui.add(
                        egui::Label::new(
                            RichText::new(readout.line())
                                .monospace()
                                .size(theme::font_size(9.0))
                                .color(theme::ACCENT),
                        )
                        .truncate(),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let spindle = app
                        .snapshot
                        .spindles
                        .first()
                        .map(|spindle| spindle.actual_speed)
                        .unwrap_or(0.0);
                    ui.label(
                        RichText::new(format!(
                            "F {:.0} / S {:.0}",
                            app.snapshot.feed.feed, spindle
                        ))
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                    );
                });
            });
        });
}

fn lathe(app: &mut LincApp, ui: &mut Ui) {
    common::axis_cards(app, ui);
    ui.add_space(6.0);
    let available = ui.available_width();
    let page_height = (ui.ctx().input(|input| input.viewport_rect().bottom())
        - theme::BOTTOM_BAR_HEIGHT
        - ui.cursor().min.y)
        .max(200.0);
    let bottom_height = 170.0;
    let main_height = (page_height - bottom_height - 10.0).max(200.0);
    let graph_width = (available * 0.62).max(460.0);
    let right_width = (available - graph_width - 8.0).max(280.0);
    let second = second_card(
        &app.profile.config.ui.second_card,
        !app.servo_load_rows().is_empty(),
    );
    // The lathe dashboard keeps feed/spindle and a second card, which is the
    // load meters or the cut's own numbers per `[ui] second_card`; the turret
    // pane belongs to the mill layout.
    let full_height = (page_height - 10.0).max(main_height);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(graph_width, main_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        theme::section_header(ui, "TOOLPATH  XZ / TURN");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new("LIVE TOOL READY")
                                    .monospace()
                                    .size(theme::font_size(9.0))
                                    .color(theme::MUTED),
                            );
                        });
                    });
                    let preview = app.program.loaded().last_valid_preview.clone();
                    let mut zoom = app.program.graph_zoom;
                    let mut pan = app.program.graph_pan;
                    let mut yaw = app.program.graph_yaw;
                    let mut pitch = app.program.graph_pitch;
                    let mut fit = app.program.fit_requested;
                    let mut auto_fit = app.program.graph_auto_fit;
                    let response = common::trajectory(
                        app,
                        ui,
                        &preview,
                        (main_height - 58.0).max(140.0),
                        ViewMode::LatheXz,
                        &mut zoom,
                        &mut pan,
                        &mut yaw,
                        &mut pitch,
                        &mut fit,
                        &mut auto_fit,
                        Some(app.program.loaded().selected_line),
                        app.snapshot.program.highlight_line(),
                        false,
                        false,
                        false,
                        false,
                        false,
                        true,
                        None,
                    )
                    .response;
                    app.program.graph_zoom = zoom;
                    app.program.graph_pan = pan;
                    app.program.graph_yaw = yaw;
                    app.program.graph_pitch = pitch;
                    app.program.fit_requested = false;
                    // Persist auto-fit too: dropping it here made every frame
                    // reload the default true, refitting and discarding the
                    // operator's zoom immediately.
                    app.program.graph_auto_fit = auto_fit;
                    crate::screens::mdi::draw_overlay(
                        app,
                        ui,
                        response.rect,
                        ViewMode::LatheXz,
                        zoom,
                        pan,
                        yaw,
                        pitch,
                    );
                });
                ui.add_space(6.0);
                // The bottom cards share the TOOLPATH column's width, so the
                // servo card's right edge lines up with the graph's. Measure
                // the column itself, not the pre-layout estimate.
                let column = ui.available_width();
                let (feed_width, second_width, _) = bottom_strip_widths(
                    column,
                    second,
                    false,
                    if second == Some(Second::Cut) {
                        crate::screens::cut::natural_width(ui, app)
                    } else {
                        0.0
                    },
                    0.0,
                    common::feed_speed_natural_width(ui, app),
                );
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    bottom_card(app, ui, feed_width, bottom_height, "feed", |app, ui, _h| {
                        common::feed_speed_card(app, ui, bottom_height);
                    });
                    let Some(second) = second else {
                        return;
                    };
                    ui.add_space(CARD_GAP);
                    bottom_card(
                        app,
                        ui,
                        second_width,
                        bottom_height,
                        second.id(),
                        |app, ui, _h| second.draw(app, ui, bottom_height),
                    );
                });
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new(right_width, full_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                if app.mdi_pane_visible() {
                    crate::screens::mdi::pane(app, ui, full_height);
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("lathe-program")
                        .max_height(full_height)
                        .auto_shrink([false, false])
                        .show(ui, |ui| common::program_card(app, ui));
                }
            },
        );
    });
}

#[cfg(test)]
mod tests {
    use super::{bottom_row_card_width, line_base, second_card, Second};

    #[test]
    fn second_card_follows_the_policy_then_the_machine() {
        // `auto` is the only policy that reads the machine, and it follows what
        // the machine publishes in both directions.
        assert_eq!(second_card("auto", true), Some(Second::Load));
        assert_eq!(second_card("auto", false), Some(Second::Cut));
        // The forced policies ignore what the machine publishes: `load` on a
        // machine with nothing to meter draws the card that says so.
        assert_eq!(second_card("load", false), Some(Second::Load));
        assert_eq!(second_card("cut", true), Some(Second::Cut));
        assert_eq!(second_card("none", true), None);
        assert_eq!(second_card("none", false), None);
        // A value the profile validator would have rejected behaves as auto
        // rather than silently dropping the card.
        assert_eq!(second_card("", true), Some(Second::Load));
    }

    #[test]
    fn the_bottom_row_ends_at_the_column_edge() {
        for (column, cards, min, expected) in [
            (750.0, 2.0, 180.0, 750.0),
            (1098.0, 3.0, 220.0, 1098.0),
            (480.0, 1.0, 180.0, 480.0),
        ] {
            let width = bottom_row_card_width(column, cards, min);
            let total = width * cards + 8.0 * (cards - 1.0);
            assert!(
                (total - expected).abs() < 1e-3,
                "{cards} cards of {width} span {total}, expected {expected}"
            );
        }
    }

    /// The matrix card's figures are the DRO size, chosen from the room the rows
    /// have: a three-axis card gets the biggest on the ramp, a five-axis one
    /// steps down rather than painting over the card's edge.
    #[test]
    fn the_matrix_lines_shrink_to_fit_their_card() {
        // A three-axis card has room for the largest figure.
        assert_eq!(line_base(3, 190.0), 30.0);
        // A five-axis one has to step down, and still gets a size.
        assert!(line_base(5, 190.0) < line_base(3, 190.0));
        // A card too short for any candidate keeps the smallest: the lines are
        // still painted rather than dropped.
        assert_eq!(line_base(5, 40.0), 16.0);
        // A machine that reports no axes still has a size to paint at.
        assert_eq!(line_base(0, 190.0), line_base(1, 190.0));
    }
}
