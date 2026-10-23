//! Settings: display preferences, keyboard reference, profile and panel info.

use egui::{RichText, Ui, Vec2};

use crate::screens::common;
use crate::shell::LincApp;
use crate::state::SettingsUi;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    // No page title: the navigation names the page.
    common::focus_anchor(ui, "settings-page-focus");
    let tabs = ["DISPLAY", "KEYBOARD", "MACHINE", "PANEL", "ABOUT"];
    ui.horizontal(|ui| {
        for (index, tab) in tabs.iter().enumerate() {
            if common::page_tab(ui, tab, app.settings.page == index).clicked() {
                app.settings.page = index;
            }
        }
    });
    ui.add_space(4.0);
    egui::ScrollArea::vertical()
        .id_salt("settings-body")
        .max_height(ui.available_height())
        .show(ui, |ui| match app.settings.page {
            0 => display(app, ui),
            1 => keyboard(app, ui),
            2 => machine(app, ui),
            3 => panel(app, ui),
            _ => about(app, ui),
        });
}

fn display(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_width();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(available * 0.5, 380.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "THEME / UNITS / DENSITY");
                    theme::kv_row(ui, "THEME", app.profile.config.ui.theme.clone());
                    theme::kv_row(ui, "LINEAR UNITS", app.snapshot.positions.units.clone());
                    theme::kv_row(
                        ui,
                        "ANGULAR UNITS",
                        app.snapshot.positions.angular_units.clone(),
                    );
                    ui.label(
                        RichText::new("Units come from the LinuxCNC INI and are read-only here.")
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                    ui.add_space(6.0);
                    theme::kv_row(ui, "CONFIG SCALE", app.profile.config.ui.scale.clone());
                    ui.label(
                        RichText::new("UI SCALE (runtime override)")
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                    let mut scale = app.design_scale;
                    if ui
                        .add(egui::Slider::new(&mut scale, 0.8..=1.5).text("scale"))
                        .changed()
                    {
                        app.design_scale = scale;
                        app.settings.density = scale;
                    }
                    theme::kv_row(ui, "WINDOW", app.profile.config.ui.window_title.clone());
                    theme::kv_row(ui, "START PAGE", app.profile.config.ui.start_page.clone());
                    theme::kv_row(
                        ui,
                        "PREFERENCES",
                        app.profile.preferences_path.display().to_string(),
                    );
                });
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new((available - available * 0.5 - 8.0).max(300.0), 380.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "VIEW SELECTION");
                    theme::kv_row(
                        ui,
                        "PROGRAM START",
                        app.profile.config.program.default_view.clone(),
                    );
                    theme::kv_row(
                        ui,
                        "AFTER LOAD",
                        app.profile.config.program.load_view.clone(),
                    );
                    theme::kv_row(
                        ui,
                        "EDIT VIEW",
                        app.profile.config.program.edit_view.clone(),
                    );
                    theme::kv_row(
                        ui,
                        "LIVE PREVIEW",
                        if app.profile.config.program.live_preview {
                            "ON"
                        } else {
                            "OFF"
                        },
                    );
                    theme::kv_row(
                        ui,
                        "PREVIEW DEBOUNCE",
                        format!("{} ms", app.profile.config.program.preview_debounce_ms),
                    );
                    theme::kv_row(
                        ui,
                        "AUTOSAVE",
                        if app.profile.config.program.autosave {
                            "ON"
                        } else {
                            "OFF"
                        },
                    );
                });
                theme::card(ui, |ui| {
                    theme::section_header(ui, "SAFETY POLICY (IMMUTABLE)");
                    for (key, value) in [
                        ("MACHINE CONTROL", "PANEL + EXPLICIT MDI"),
                        ("SOFTWARE E-STOP", "FORBIDDEN"),
                        ("SOFTWARE CYCLE", "FORBIDDEN"),
                        ("SOFTWARE JOG", "FORBIDDEN"),
                        ("SOFTWARE OVERRIDES", "FORBIDDEN"),
                        ("PROGRAM PROTECT", "PANEL / FAIL CLOSED"),
                        ("PENDANT", "JOG ONLY"),
                    ] {
                        theme::kv_row(ui, key, value);
                    }
                });
            },
        );
    });
}

fn keyboard(app: &mut LincApp, ui: &mut Ui) {
    let keys = &app.profile.config.keys;
    let rows = [
        ("MAIN", keys.machine.as_str(), "Machine dashboard"),
        ("FILE", keys.program.as_str(), "Program / cycle SPLIT-CODE"),
        ("TOOL", keys.tools.as_str(), "Tools"),
        ("OFFSET", keys.offsets.as_str(), "Offsets"),
        ("Ctrl+D", keys.diagnostics.as_str(), "Diagnostics"),
        ("Ctrl+H", keys.help.as_str(), "G-code Help"),
        ("M", keys.messages.as_str(), "Messages dialog"),
        ("ESC", keys.back.as_str(), "Back / cancel / close detail"),
        ("CTRL+Q", keys.quit.as_str(), "Guarded quit"),
        ("CTRL+O", keys.open.as_str(), "Open Files"),
        ("CTRL+S", keys.save.as_str(), "Save program"),
        ("CTRL+Z/Y", "Ctrl+Z / Ctrl+Y", "Undo / redo"),
        ("CTRL+F", keys.find.as_str(), "Focus Help search"),
        ("CTRL+R", keys.refresh.as_str(), "Refresh Files"),
        (
            "ENTER",
            keys.activate.as_str(),
            "Select / apply / issue MDI",
        ),
        ("UP/DOWN", "Arrows", "Focus, lists, scroll, history"),
        ("PGUP/PGDN", "PageUp / PageDown", "Tabs / pages"),
        ("HOME/END", "Home / End", "First / last row"),
        ("/", keys.help_search.as_str(), "Focus Help search"),
        ("MO(1)", "Symbol layer", "Symbols and mouse"),
    ];
    theme::card(ui, |ui| {
        theme::section_header(ui, "QMK CNC KEYBOARD");
        egui::Grid::new("keyboard-grid")
            .num_columns(3)
            .striped(true)
            .spacing([18.0, 4.0])
            .show(ui, |ui| {
                for header in ["KEYCAP", "USB / ACTION", "HMI ACTION"] {
                    ui.label(
                        RichText::new(header)
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                }
                ui.end_row();
                for (keycap, action, description) in rows {
                    ui.label(
                        RichText::new(keycap)
                            .monospace()
                            .size(theme::font_size(11.0))
                            .color(theme::C3),
                    );
                    ui.label(
                        RichText::new(action)
                            .monospace()
                            .size(theme::font_size(11.0))
                            .color(theme::FG),
                    );
                    ui.label(
                        RichText::new(description)
                            .size(theme::font_size(10.0))
                            .color(theme::MUTED),
                    );
                    ui.end_row();
                }
            });
        ui.add_space(6.0);
        ui.label(
            RichText::new("Hard limits: no F1-F4, F9, F11, F12; no Tab, Shift or Delete. Letters arrive lowercase; G-code words display uppercase.")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        ui.label(
            RichText::new("On-screen actions are focusable with arrows/Enter and clickable with the MO(1) mouse layer.")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
    });
}

fn machine(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_width();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(available * 0.56, 460.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "MACHINE PROFILE / INI SUMMARY");
                    for (key, value) in app.profile.summary() {
                        theme::kv_row(ui, &key, value);
                    }
                });
                theme::card(ui, |ui| {
                    theme::section_header(ui, "AXES");
                    egui::Grid::new("settings-axes")
                        .num_columns(5)
                        .striped(true)
                        .show(ui, |ui| {
                            for header in ["AXIS", "MIN", "MAX", "MAX VEL", "HOME SEQ"] {
                                ui.label(RichText::new(header).size(theme::font_size(9.0)).strong().color(theme::MUTED));
                            }
                            ui.end_row();
                            for axis in &app.profile.machine_ini.axes {
                                ui.label(RichText::new(&axis.name).monospace().color(theme::C3));
                                ui.label(RichText::new(format!("{:.3}", axis.min_limit)).monospace().size(theme::font_size(10.0)));
                                ui.label(RichText::new(format!("{:.3}", axis.max_limit)).monospace().size(theme::font_size(10.0)));
                                ui.label(RichText::new(format!("{:.2}", axis.max_velocity)).monospace().size(theme::font_size(10.0)));
                                ui.label(RichText::new(axis.home_sequence.to_string()).monospace().size(theme::font_size(10.0)));
                                ui.end_row();
                            }
                        });
                });
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new((available - available * 0.56 - 8.0).max(300.0), 460.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "TOOLING / PROGRAM PATHS");
                    theme::kv_row(ui, "TOOL TABLE", app.profile.machine_ini.tool_table.display().to_string());
                    theme::kv_row(ui, "COLUMNS", app.profile.machine_ini.tool_table_columns.clone());
                    theme::kv_row(ui, "CHANGER", app.profile.changer.label().to_string());
                    theme::kv_row(ui, "ATC POCKETS", app.profile.machine_ini.atc_pockets.map(|p| p.to_string()).unwrap_or_else(|| "—".into()));
                    theme::kv_row(ui, "PROGRAM PREFIX", app.profile.machine_ini.program_prefix.display().to_string());
                    theme::kv_row(ui, "SUBROUTINES", app.profile.machine_ini.subroutine_paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(":"));
                });
                theme::card(ui, |ui| {
                    theme::section_header(ui, "CONFIGURATION RULES");
                    ui.label(RichText::new("The INI remains authoritative for travel, velocities, units, spindle/ATC data, tool table and program prefix. The HMI refuses to start without an explicit profile.").size(theme::font_size(10.0)).color(theme::MUTED));
                    ui.label(RichText::new("Unknown profile keys are errors; arrays replace; safety flags cannot be weakened.").size(theme::font_size(10.0)).color(theme::MUTED));
                });
            },
        );
    });
}

fn panel(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_width();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(available * 0.5, 380.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "PANEL LINK");
                    theme::kv_row(ui, "OWNER", app.profile.config.panel.owner.clone());
                    theme::kv_row(ui, "KIND", app.profile.config.panel.kind.clone());
                    theme::kv_row(ui, "DRIVER", app.profile.config.panel.driver.clone());
                    theme::kv_row(ui, "TRANSPORT", app.snapshot.panel.capabilities.transport.clone());
                    theme::kv_row(ui, "PORT", app.snapshot.panel.link.port.clone());
                    theme::kv_row(ui, "BAUD", app.snapshot.panel.link.baud.to_string());
                    theme::kv_row(ui, "CYCLE", format!("{:.1} ms", app.snapshot.panel.link.cycle_time_ms));
                    theme::kv_row(ui, "CONNECTED", app.snapshot.panel.connected.to_string());
                    theme::kv_row(ui, "MODE", app.snapshot.panel.mode.label().to_string());
                });
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new((available - available * 0.5 - 8.0).max(300.0), 380.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "HAL OUTPUT POLICY");
                    theme::kv_row(ui, "WRITE POLICY", app.profile.config.hal.write_policy.clone());
                    theme::kv_row(ui, "PREFIX", app.profile.config.hal.output_prefix.clone());
                    for output in &app.profile.config.hal.allowed_outputs {
                        theme::kv_row(ui, "ALLOWED", format!("{}{output}", app.profile.config.hal.output_prefix));
                    }
                    ui.label(RichText::new("The HMI never opens the panel serial port and never writes arbitrary cnc_panel.* or machine-motion pins.").size(theme::font_size(9.0)).color(theme::MUTED));
                });
                theme::card(ui, |ui| {
                    theme::section_header(ui, "PENDANT");
                    theme::kv_row(ui, "KIND", app.profile.config.pendant.kind.clone());
                    theme::kv_row(ui, "JOG ONLY", app.profile.config.pendant.jog_only.to_string());
                    theme::kv_row(ui, "IGNORED", app.profile.config.pendant.ignored_functions.join(", "));
                });
            },
        );
    });
}

fn about(app: &mut LincApp, ui: &mut Ui) {
    let available = ui.available_width();
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(available * 0.5, 320.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    ui.label(RichText::new("LiNC").size(theme::font_size(28.0)).strong().color(theme::FG));
                    ui.label(RichText::new("Rust egui HMI for LinuxCNC").size(theme::font_size(13.0)).color(theme::MUTED));
                    ui.add_space(8.0);
                    theme::kv_row(ui, "VERSION", env!("CARGO_PKG_VERSION").to_string());
                    theme::kv_row(ui, "PROFILE", app.profile.config.profile.clone());
                    theme::kv_row(ui, "MACHINE", app.profile.config.machine.name.clone());
                    theme::kv_row(ui, "KIND", app.profile.config.machine.kind.clone());
                    theme::kv_row(ui, "SIMULATION", app.is_sim().to_string());
                    theme::kv_row(ui, "BACKEND", app.profile.config.backend.kind.clone());
                    theme::kv_row(ui, "TRANSPORT", app.profile.config.backend.transport.clone());
                    ui.label(RichText::new("The hardware E-stop chain is the only safety-rated stop. The HMI is outside the safety path.").size(theme::font_size(9.0)).color(theme::MUTED));
                });
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new((available - available * 0.5 - 8.0).max(300.0), 320.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    theme::section_header(ui, "SUPPORT");
                    ui.label(RichText::new("G-code Help is bundled offline with LinuxCNC documentation attribution.").size(theme::font_size(10.0)).color(theme::MUTED));
                    ui.label(RichText::new("Diagnostics exposes panel link, NML, motion, I/O and event history without machine controls.").size(theme::font_size(10.0)).color(theme::MUTED));
                    ui.label(RichText::new("Manual modes and homing are read-only; all motion input stays on the physical panel.").size(theme::font_size(10.0)).color(theme::MUTED));
                });
            },
        );
    });
}

pub fn _unused(_: SettingsUi) {}
