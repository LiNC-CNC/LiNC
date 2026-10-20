//! Machine dashboard, mill and lathe variants.

use egui::{RichText, Ui, Vec2};
use linc_core::model::MachineVariant;
use linc_view::ViewMode;

use crate::screens::common;
use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    if app.profile.variant == MachineVariant::Lathe {
        lathe(app, ui);
    } else {
        mill(app, ui);
    }
}

fn mill(app: &mut LincApp, ui: &mut Ui) {
    common::page_header(
        ui,
        "MACHINE",
        &format!(
            "{}  /  {}  /  {}",
            app.profile.config.machine.name,
            common::mode_label(app),
            if app.is_sim() { "SIMULATION" } else { "LIVE" }
        ),
    );
    common::axis_cards(app, ui);
    ui.add_space(6.0);

    let available = ui.available_width();
    // Fill the page at the 720p baseline and above instead of leaving unused space.
    let main_height = (ui.available_height() - 12.0).max(220.0);
    let graph_width = (available * 0.62).max(460.0);
    let right_width = (available - graph_width - 8.0).max(300.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(graph_width, main_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| graph_panel(app, ui, main_height),
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new(right_width, main_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_max_width((right_width - 24.0).max(220.0));
                egui::ScrollArea::vertical()
                    .id_salt("machine-right")
                    .max_height(main_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        common::program_card(app, ui);
                        common::feed_spindle_card(app, ui);
                        common::health_card(app, ui);
                        common::alarm_card(app, ui, 180.0);
                    });
            },
        );
    });
}

fn graph_panel(app: &mut LincApp, ui: &mut Ui, height: f32) {
    let preview = app.program.last_valid_preview.clone();
    let mut view = app.program.graph_view;
    if view == ViewMode::LatheXz {
        view = ViewMode::Iso3d;
    }
    theme::card(ui, |ui| {
        ui.set_max_height(height);
        ui.horizontal(|ui| {
            theme::section_header(ui, "TOOLPATH");
            ui.add_space(8.0);
            if theme::action_button(ui, "CLEAR TRAIL", true).clicked() {
                app.clear_trail();
            }
            if theme::action_button(ui, "FIT", true).clicked() {
                app.program.graph_zoom = 1.0;
                app.program.graph_pan = Vec2::ZERO;
                app.program.fit_requested = true;
            }
            if theme::action_button(ui, view.label(), true).clicked() {
                app.program.graph_view = match view {
                    ViewMode::Iso3d => ViewMode::TopXy,
                    ViewMode::TopXy => ViewMode::SideXz,
                    _ => ViewMode::Iso3d,
                };
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "CURRENT LN {} / {}",
                        app.snapshot.program.current_line, app.snapshot.program.line_count
                    ))
                    .monospace()
                    .size(theme::font_size(10.0))
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
            (height - 62.0).max(140.0),
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

fn lathe(app: &mut LincApp, ui: &mut Ui) {
    common::page_header(
        ui,
        "LATHE",
        &format!(
            "{}  /  {}  /  {}",
            app.profile.config.machine.name,
            common::mode_label(app),
            if app.is_sim() { "SIMULATION" } else { "LIVE" }
        ),
    );
    common::axis_cards(app, ui);
    ui.add_space(6.0);
    let available = ui.available_width();
    let main_height = (ui.available_height() - 12.0).max(200.0);
    let graph_width = (available * 0.60).max(480.0);
    let right_width = (available - graph_width - 8.0).max(300.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(graph_width, main_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        theme::section_header(ui, "TOOLPATH");
                        ui.add_space(8.0);
                        if theme::action_button(ui, "CLEAR TRAIL", true).clicked() {
                            app.clear_trail();
                        }
                        if theme::action_button(ui, "FIT", true).clicked() {
                            app.program.graph_zoom = 1.0;
                            app.program.graph_pan = Vec2::ZERO;
                            app.program.fit_requested = true;
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            ui.label(
                                RichText::new("XZ / TURN   LIVE TOOL READY")
                                    .monospace()
                                    .size(theme::font_size(10.0))
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
                        240.0,
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
                ui.set_max_width((right_width - 24.0).max(220.0));
                egui::ScrollArea::vertical()
                    .id_salt("machine-right")
                    .max_height(main_height)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        common::turret_card(app, ui);
                        common::program_card(app, ui);
                        common::feed_spindle_card(app, ui);
                        common::health_card(app, ui);
                        common::alarm_card(app, ui, 180.0);
                    });
            },
        );
    });
}
