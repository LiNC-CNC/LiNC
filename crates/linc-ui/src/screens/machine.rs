//! Machine dashboard, mill and lathe variants.

use egui::{RichText, Ui, Vec2};
use linc_core::model::MachineVariant;
use linc_view::ViewMode;

use crate::screens::common;
use crate::shell::LincApp;
use crate::state::MainView;
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
                    let cards = match second {
                        None => 1.0,
                        Some(Second::Load) => 2.0,
                        Some(Second::Cut) if with_tlo => 3.0,
                        Some(Second::Cut) => 2.0,
                    };
                    let card_width = bottom_row_card_width(graph_width, cards, 150.0);
                    ui.horizontal_top(|ui| {
                        // The explicit gap below is the whole gap: the default
                        // item spacing would add 8 more per card and push the
                        // servo card past the graph's right edge.
                        ui.spacing_mut().item_spacing.x = 0.0;
                        bottom_card(app, ui, card_width, bottom_height, "feed", |app, ui, _h| {
                            common::feed_speed_card(app, ui, bottom_height);
                        });
                        let Some(second) = second else {
                            return;
                        };
                        ui.add_space(8.0);
                        bottom_card(
                            app,
                            ui,
                            card_width,
                            bottom_height,
                            second.id(),
                            |app, ui, _h| second.draw(app, ui, bottom_height),
                        );
                        if with_tlo {
                            ui.add_space(8.0);
                            bottom_card(
                                app,
                                ui,
                                card_width,
                                bottom_height,
                                "tlo",
                                |app, ui, _h| {
                                    crate::screens::tlo::tlo_card(app, ui, bottom_height);
                                },
                            );
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
fn alternate_active(app: &LincApp) -> bool {
    app.main_view == MainView::Coordinates && app.main_alternate_available()
}

/// One line of a matrix card: the axis name and the figure beside it.
struct CoordLine {
    label: String,
    value: String,
    /// An axis with no number to show is dimmed rather than dropped.
    muted: bool,
}

/// The alternate dashboard: four coordinate cards in the place the axis
/// readouts and the toolpath take on the default one.
fn coordinate_matrix(app: &LincApp, ui: &mut Ui, width: f32, height: f32) {
    const GAP: f32 = 8.0;
    let cell = Vec2::new(
        ((width - GAP) / 2.0).max(160.0),
        ((height - GAP) / 2.0).max(120.0),
    );
    let axes = app.axes();
    let diameter = app.reads_x_as_diameter();
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
            matrix_card(
                ui,
                cell,
                "WORK COORDINATES",
                Some(app.active_wcs_label()),
                &work,
            );
            ui.add_space(GAP);
            matrix_card(ui, cell, "MACHINE COORDINATES", None, &machine);
        });
        ui.add_space(GAP);
        ui.horizontal_top(|ui| {
            matrix_card(ui, cell, "DISTANCE TO GO", None, &to_go);
            ui.add_space(GAP);
            matrix_card(
                ui,
                cell,
                "TOOL LENGTH OFFSET",
                Some(format!("T{}", lengths.tool)),
                &tool,
            );
        });
    });
}

/// One card of the matrix: a title, an optional badge at its right end (the
/// active WCS, the loaded tool) and the lines under it.
///
/// The title is painted by [`common::value_card`], not laid out in a row: a
/// laid-out title sat 14 px below the card's own margin, and that blank band
/// was the first thing the eye met inside every card.
fn matrix_card(ui: &mut Ui, cell: Vec2, title: &str, badge: Option<String>, lines: &[CoordLine]) {
    ui.allocate_ui_with_layout(cell, egui::Layout::top_down(egui::Align::Min), |ui| {
        ui.set_min_width(cell.x);
        ui.set_max_width(cell.x);
        let badge = badge.as_deref().map(|badge| (badge, theme::ACCENT));
        let body = common::value_card(ui, cell.y, title, badge);
        paint_coordinate_lines(ui, body, lines);
    });
}

/// Paint the lines of a matrix card: the axis name in the DRO's label colour at
/// the left inset, its figure at the right edge.
fn paint_coordinate_lines(ui: &Ui, rect: egui::Rect, lines: &[CoordLine]) {
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
    let preview = app.program.last_valid_preview.clone();
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
                Some(app.program.selected_line),
                app.snapshot.program.highlight_line(),
                false,
                false,
                false,
                false,
                false,
                None,
            );
            app.program.graph_zoom = zoom;
            app.program.graph_pan = pan;
            app.program.graph_yaw = yaw;
            app.program.graph_pitch = pitch;
            app.program.fit_requested = false;
            app.program.graph_auto_fit = auto_fit;
            common::toolpath_status(ui, header, view.label(), drawn.span_mm);
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
                    &app.program.preview,
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
                    let preview = app.program.last_valid_preview.clone();
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
                        Some(app.program.selected_line),
                        app.snapshot.program.highlight_line(),
                        false,
                        false,
                        false,
                        false,
                        false,
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
                let cards = if second.is_some() { 2.0 } else { 1.0 };
                let card_width = bottom_row_card_width(ui.available_width(), cards, 180.0);
                ui.horizontal_top(|ui| {
                    ui.spacing_mut().item_spacing.x = 0.0;
                    bottom_card(app, ui, card_width, bottom_height, "feed", |app, ui, _h| {
                        common::feed_speed_card(app, ui, bottom_height);
                    });
                    let Some(second) = second else {
                        return;
                    };
                    ui.add_space(8.0);
                    bottom_card(
                        app,
                        ui,
                        card_width,
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
