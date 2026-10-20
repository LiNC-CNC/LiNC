//! Read-only Manual screen shared by JOG, INC and MPG panel modes.

use egui::{RichText, Ui, Vec2};
use linc_core::model::MachineVariant;
use linc_view::ViewMode;

use crate::screens::common;
use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    let mode = app
        .snapshot
        .panel
        .mode
        .mode()
        .filter(|mode| mode.is_manual());
    let mode_label = mode.map(|mode| mode.label()).unwrap_or("UNKNOWN");
    common::page_header(
        ui,
        "MANUAL",
        "JOG / INC / MPG share this read-only screen. Input stays on the physical panel.",
    );
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("PANEL MODE: {mode_label}"))
                .monospace()
                .size(14.0)
                .color(if mode.is_some() {
                    theme::ACCENT
                } else {
                    theme::WARNING
                }),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new("NO ON-SCREEN JOG OR MODE CONTROLS")
                    .monospace()
                    .size(10.0)
                    .color(theme::MUTED),
            );
        });
    });
    ui.add_space(4.0);
    let available = ui.available_size();
    let graph_width = (available.x * 0.68).max(560.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(graph_width, available.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    ui.horizontal(|ui| {
                        theme::section_header(ui, "TOOLPATH / LIVE POSITION");
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if theme::action_button(ui, "CLEAR TRAIL", true).clicked() {
                                app.clear_trail();
                            }
                            if theme::action_button(ui, "FIT", true).clicked() {
                                app.manual.fit = true;
                            }
                            let label = app.manual.view.label();
                            if theme::action_button(ui, label, true).clicked() {
                                app.manual.view = if app.profile.variant == MachineVariant::Lathe {
                                    ViewMode::LatheXz
                                } else {
                                    match app.manual.view {
                                        ViewMode::Iso3d => ViewMode::TopXy,
                                        ViewMode::TopXy => ViewMode::SideXz,
                                        _ => ViewMode::Iso3d,
                                    }
                                };
                            }
                            if app.snapshot.machine.estop {
                                ui.label(RichText::new("E-STOP").color(theme::ERROR));
                            }
                        });
                    });
                    let preview = app.program.last_valid_preview.clone();
                    let mut zoom = app.manual.zoom;
                    let mut pan = app.manual.pan;
                    let mut fit = app.manual.fit;
                    common::trajectory(
                        app,
                        ui,
                        &preview,
                        (available.y - 90.0).max(320.0),
                        app.manual.view,
                        &mut zoom,
                        &mut pan,
                        &mut fit,
                        None,
                        Some(app.snapshot.program.current_line),
                    );
                    app.manual.zoom = zoom;
                    app.manual.pan = pan;
                    app.manual.fit = fit;
                });
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new((available.x - graph_width - 8.0).max(320.0), available.y),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("manual-right")
                    .max_height(available.y - 10.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        coordinates_card(app, ui);
                        mode_context_card(app, ui);
                        theme::card(ui, |ui| {
                            theme::section_header(ui, "INPUT");
                            theme::kv_row(ui, "PANEL", if app.snapshot.panel.connected { "ONLINE" } else { "OFFLINE" });
                            theme::kv_row(ui, "PROTECT", if app.program_protected() { "ON" } else { "OFF" });
                            theme::kv_row(ui, "PENDANT", if app.snapshot.pendant.connected { "WHB04B-6 / JOG ONLY" } else { "NOT CONNECTED" });
                            theme::kv_row(ui, "ACTIVE AXIS", app.snapshot.panel.jog.active_axis.clone().unwrap_or_else(|| "—".into()));
                            ui.label(
                                RichText::new("MPG encoder counts are not carried by the RS-485 link and are never fabricated.")
                                    .size(9.0)
                                    .color(theme::MUTED),
                            );
                        });
                    });
            },
        );
    });
}

fn coordinates_card(app: &LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, "WORK COORDINATES");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} / {}{}",
                        app.active_wcs_label(),
                        app.snapshot.positions.units,
                        if app.profile.variant == MachineVariant::Lathe {
                            " / RADIUS"
                        } else {
                            ""
                        }
                    ))
                    .monospace()
                    .size(10.0)
                    .color(theme::MUTED),
                );
            });
        });
        for axis in app.axes() {
            let position = app.snapshot.positions.axes.get(&axis);
            let active = app
                .snapshot
                .panel
                .jog
                .active_axis
                .as_deref()
                .map(|name| name == axis)
                .unwrap_or(false);
            let work = position.map(|p| p.work).unwrap_or(0.0);
            let machine = position.map(|p| p.machine).unwrap_or(0.0);
            let dtg = position.map(|p| p.dtg).unwrap_or(0.0);
            let frame = egui::Frame::new()
                .fill(if active {
                    theme::ACCENT_DARK
                } else {
                    theme::PANEL
                })
                .stroke(egui::Stroke::new(
                    1.0,
                    if active { theme::ACCENT } else { theme::BORDER },
                ))
                .corner_radius(3)
                .inner_margin(egui::Margin::symmetric(8, 4));
            frame.show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(&axis)
                            .monospace()
                            .size(13.0)
                            .strong()
                            .color(theme::C3),
                    );
                    if active {
                        ui.label(RichText::new("ACTIVE").size(8.0).color(theme::ACCENT));
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new(format!("DTG {dtg:.3}"))
                                .monospace()
                                .size(9.0)
                                .color(theme::MUTED),
                        );
                        ui.label(
                            RichText::new(crate::theme::format_position(work, 3))
                                .monospace()
                                .size(22.0)
                                .color(if active { theme::FG } else { theme::FG }),
                        );
                    });
                });
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(format!("MACH {machine:+.3}"))
                            .monospace()
                            .size(8.5)
                            .color(theme::MUTED),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            RichText::new("WORK / MACHINE / DTG")
                                .size(8.0)
                                .color(theme::MUTED),
                        );
                    });
                });
            });
        }
    });
}

fn mode_context_card(app: &LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "MODE CONTEXT");
        let mode = app.snapshot.panel.mode.mode();
        let active = app
            .snapshot
            .panel
            .jog
            .active_axis
            .clone()
            .unwrap_or_else(|| "—".into());
        theme::kv_row(ui, "ACTIVE AXIS", active);
        match mode {
            Some(linc_core::model::PanelMode::Jog) => {
                theme::kv_row(ui, "MODE", "CONTINUOUS");
                theme::kv_row(
                    ui,
                    "JOG SPEED",
                    format!(
                        "{:.0} {}",
                        app.snapshot.panel.jog.speed, app.snapshot.positions.units
                    ),
                );
                theme::kv_row(
                    ui,
                    "RAPID",
                    if app.snapshot.panel.jog.rapid {
                        "ON"
                    } else {
                        "OFF"
                    },
                );
            }
            Some(linc_core::model::PanelMode::Inc) => {
                theme::kv_row(ui, "MODE", "INCREMENT");
                theme::kv_row(
                    ui,
                    "INCREMENT",
                    app.snapshot
                        .panel
                        .jog
                        .increment
                        .map(|value| format!("{value} {}", app.snapshot.positions.units))
                        .unwrap_or_else(|| "UNKNOWN".into()),
                );
                theme::kv_row(
                    ui,
                    "REMAINING STEP",
                    app.snapshot
                        .panel
                        .jog
                        .remaining_step
                        .map(|value| format!("{value:.3}"))
                        .unwrap_or_else(|| "—".into()),
                );
            }
            Some(linc_core::model::PanelMode::Mpg) => {
                theme::kv_row(ui, "MODE", "HANDWHEEL");
                theme::kv_row(
                    ui,
                    "SCALE",
                    app.snapshot
                        .panel
                        .jog
                        .handwheel_scale
                        .map(|value| format!("{value}"))
                        .unwrap_or_else(|| "UNKNOWN".into()),
                );
                theme::kv_row(ui, "STEP MODE", app.snapshot.panel.jog.mode.clone());
            }
            _ => {
                theme::kv_row(ui, "MODE", "UNKNOWN");
            }
        }
        ui.label(
            RichText::new("When the panel mode or axis selection is unknown after reconnect, it stays UNKNOWN until a fresh state arrives.")
                .size(9.0)
                .color(theme::MUTED),
        );
    });
}
