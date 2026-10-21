//! Machine dashboard, mill and lathe variants.

use egui::{RichText, Ui, Vec2};
use linc_core::model::MachineVariant;
use linc_view::ViewMode;

use crate::screens::common;
use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    // The page header is the stable keyboard focus stop. Machine is the
    // default page, so without it arrow-key navigation could never start
    // here (egui only bootstraps focus from Tab).
    common::page_header(
        ui,
        if app.profile.variant == MachineVariant::Lathe {
            "MACHINE / LATHE"
        } else {
            "MACHINE / MILL"
        },
        "Read-only dashboard. Machine control stays on the physical panel.",
    );
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
    let bottom_height = 150.0;
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
                let card_width = ((graph_width - 16.0) / 3.0 - 11.4).max(180.0);
                ui.horizontal_top(|ui| {
                    bottom_card(app, ui, card_width, bottom_height, "feed", |app, ui, _h| {
                        common::feed_spindle_card(app, ui, bottom_height);
                    });
                    ui.add_space(8.0);
                    bottom_card(
                        app,
                        ui,
                        card_width,
                        bottom_height,
                        "health",
                        |app, ui, _h| common::health_card(app, ui, bottom_height),
                    );
                    ui.add_space(8.0);
                    ui.allocate_ui_with_layout(
                        Vec2::new(card_width, bottom_height),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_min_width(card_width - 1.0);
                            common::alarm_card(app, ui, bottom_height);
                        },
                    );
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
                common::program_card_sized(app, ui, (pane_bottom - pane_top - 2.0).max(200.0));
            },
        );
    });
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
            ui.set_min_width(width - 1.0);
            ui.set_max_width(width - 1.0);
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
            ui.horizontal(|ui| {
                ui.label(
                    RichText::new("TOOLPATH")
                        .size(theme::font_size(10.0))
                        .strong()
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new(view.label())
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new("— CUT")
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::ACCENT),
                );
                ui.label(
                    RichText::new("-- RAPID")
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
                ui.label(
                    RichText::new("— LIVE")
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(theme::SUCCESS),
                );
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        RichText::new(format!("ZOOM {:.1}x", app.program.graph_zoom))
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                });
            });
            let mut zoom = app.program.graph_zoom;
            let mut pan = app.program.graph_pan;
            let mut fit = app.program.fit_requested;
            common::trajectory(
                app,
                ui,
                &preview,
                (height - 36.0).max(120.0),
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
                    let mut fit = app.program.fit_requested;
                    common::trajectory(
                        app,
                        ui,
                        &preview,
                        (main_height - 58.0).max(140.0),
                        ViewMode::LatheXz,
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
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new(right_width, main_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("lathe-program")
                    .max_height(main_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| common::program_card(app, ui));
            },
        );
    });
    ui.add_space(6.0);
    let card_width = ((available - 16.0) / 3.0).max(220.0);
    ui.horizontal_top(|ui| {
        bottom_card(app, ui, card_width, bottom_height, "feed", |app, ui, _h| {
            common::feed_spindle_card(app, ui, bottom_height);
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
        ui.add_space(8.0);
        bottom_card(
            app,
            ui,
            card_width,
            bottom_height,
            "health",
            |app, ui, _h| common::health_card(app, ui, bottom_height),
        );
    });
}
