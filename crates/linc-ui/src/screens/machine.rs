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
    common::axis_cards(app, ui);
    ui.add_space(6.0);

    let available = ui.available_width();
    let page_height = (ui.ctx().input(|input| input.viewport_rect().bottom())
        - theme::BOTTOM_BAR_HEIGHT
        - ui.cursor().min.y)
        .max(200.0);
    // The bottom cards are sized to their contents, not to a fixed strip: the
    // graph gets every pixel the feed/servo rows do not need.
    let bottom_height = bottom_cards_height(app);
    let graph_height = (page_height - bottom_height - 6.0).max(200.0);
    let graph_width = (available * 0.70).max(480.0);
    let right_width = (available - graph_width - 8.0).max(280.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(graph_width, page_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.allocate_ui_with_layout(
                    Vec2::new(graph_width, graph_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| graph_panel(app, ui, graph_height),
                );
                ui.add_space(6.0);
                let servo = !app.servo_load_rows().is_empty();
                let cards = if servo { 2.0 } else { 1.0 };
                let card_width = bottom_row_card_width(graph_width, cards, 180.0);
                ui.horizontal_top(|ui| {
                    // The explicit gap below is the whole gap: the default
                    // item spacing would add 8 more per card and push the
                    // servo card past the graph's right edge.
                    ui.spacing_mut().item_spacing.x = 0.0;
                    bottom_card(app, ui, card_width, bottom_height, "feed", |app, ui, _h| {
                        common::feed_speed_card(app, ui, bottom_height);
                    });
                    if servo {
                        ui.add_space(8.0);
                        bottom_card(
                            app,
                            ui,
                            card_width,
                            bottom_height,
                            "servo",
                            |app, ui, _h| common::servo_load_card(app, ui, bottom_height),
                        );
                    }
                });
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

/// Height that just holds the bottom row: the feed/spindle block needs a fixed
/// body, and every servo row adds 16 px above the card's title and margins.
fn bottom_cards_height(app: &LincApp) -> f32 {
    const FEED: f32 = 100.0;
    let rows = app.servo_load_rows().len();
    let servo = if rows == 0 {
        0.0
    } else {
        44.0 + rows as f32 * 16.0
    };
    FEED.max(servo)
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
            common::toolpath_header(ui, view.label(), app.program.graph_zoom);
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
                (height - 36.0).max(120.0),
                view,
                &mut zoom,
                &mut pan,
                &mut yaw,
                &mut pitch,
                &mut fit,
                &mut auto_fit,
                Some(app.program.selected_line),
                Some(app.snapshot.program.current_line),
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
            // The block the MDI pane is composing, drawn over the program in
            // the pane's own colour. Nothing is drawn for an empty or
            // unparsable block, and the pane is only up in MDI.
            crate::screens::mdi::draw_overlay(app, ui, response.rect, view, zoom, pan, yaw, pitch);
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
                        Some(app.snapshot.program.current_line),
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
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new(right_width, main_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                if app.mdi_pane_visible() {
                    crate::screens::mdi::pane(app, ui, main_height);
                } else {
                    egui::ScrollArea::vertical()
                        .id_salt("lathe-program")
                        .max_height(main_height)
                        .auto_shrink([false, false])
                        .show(ui, |ui| common::program_card(app, ui));
                }
            },
        );
    });
    ui.add_space(6.0);
    let servo = !app.servo_load_rows().is_empty();
    let cards = if servo { 3.0 } else { 2.0 };
    let card_width = bottom_row_card_width(available, cards, 220.0);
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        bottom_card(app, ui, card_width, bottom_height, "feed", |app, ui, _h| {
            common::feed_speed_card(app, ui, bottom_height);
        });
        ui.add_space(8.0);
        bottom_card(
            app,
            ui,
            card_width,
            bottom_height,
            "turret",
            |app, ui, h| {
                egui::ScrollArea::vertical()
                    .id_salt("lathe-turret")
                    .max_height(h - 50.0)
                    .show(ui, |ui| common::turret_card(app, ui));
            },
        );
        if servo {
            ui.add_space(8.0);
            bottom_card(
                app,
                ui,
                card_width,
                bottom_height,
                "servo",
                |app, ui, _h| common::servo_load_card(app, ui, bottom_height),
            );
        }
    });
}

#[cfg(test)]
mod tests {
    use super::bottom_row_card_width;

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
}
