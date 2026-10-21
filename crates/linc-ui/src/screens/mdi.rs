//! Mode-driven MDI screen.

use egui::{Align, FontId, RichText, TextFormat, Ui, Vec2};
use linc_core::command::{MachineCommand, MdiOrigin};
use linc_core::gcode::{lex_line, parse_preview, TokenKind};
use linc_core::model::{format_clock, MachinePower, PanelMode};

use crate::screens::common;
use crate::shell::LincApp;
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    // No page title: the navigation names the page.
    common::focus_anchor(ui, "mdi-page-focus");
    let mode = app.snapshot.panel.mode.mode();
    let in_mdi = mode == Some(PanelMode::Mdi);
    ui.horizontal(|ui| {
        ui.label(
            RichText::new(format!("PANEL MODE: {}", common::mode_label(app)))
                .monospace()
                .size(theme::font_size(12.0))
                .color(if in_mdi {
                    theme::ACCENT
                } else {
                    theme::WARNING
                }),
        );
        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
            ui.label(
                RichText::new(if in_mdi {
                    "READY TO ISSUE"
                } else {
                    "MDI SCREEN INACTIVE"
                })
                .monospace()
                .size(theme::font_size(9.0))
                .color(if in_mdi {
                    theme::SUCCESS
                } else {
                    theme::WARNING
                }),
            );
        });
    });
    if !in_mdi {
        theme::banner(
            ui,
            theme::WARNING,
            "Move the physical mode selector to MDI to issue commands.",
        );
        ui.add_space(4.0);
    }
    ui.add_space(4.0);
    let available = ui.available_size();
    let top_height = (available.y - 118.0).max(250.0);
    let bottom_height = (available.y - top_height - 14.0).max(112.0);
    let left_width = (available.x * 0.64).max(560.0);
    let right_width = (available.x - left_width - 8.0).max(280.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(left_width, top_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("mdi-command")
                    .max_height(top_height)
                    .show(ui, |ui| command_card(app, ui, in_mdi));
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new(right_width, top_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_max_width((right_width - 24.0).max(220.0));
                egui::ScrollArea::vertical()
                    .id_salt("mdi-right")
                    .max_height(top_height)
                    .show(ui, |ui| {
                        mdi_telemetry(app, ui);
                    });
            },
        );
    });
    ui.add_space(6.0);
    ui.horizontal_top(|ui| {
        ui.allocate_ui_with_layout(
            Vec2::new(left_width, bottom_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("mdi-result")
                    .max_height(bottom_height)
                    .show(ui, |ui| last_result_card(app, ui));
            },
        );
        ui.add_space(8.0);
        ui.allocate_ui_with_layout(
            Vec2::new(right_width, bottom_height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                ui.set_max_width((right_width - 24.0).max(220.0));
                history_card(app, ui);
            },
        );
    });
}

fn command_card(app: &mut LincApp, ui: &mut Ui, in_mdi: bool) {
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, "MDI COMMAND");
            ui.label(
                RichText::new("ONE BLOCK / LINUXCNC")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format!(
                        "{} / {}",
                        app.mdi.input.len(),
                        app.profile.config.mdi.max_command_length
                    ))
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
                );
            });
        });
        let response = highlighted_input(app, ui, in_mdi);
        if app.profile.config.mdi.auto_focus && in_mdi && app.mdi.focus {
            response.request_focus();
            app.mdi.focus = false;
        }
        let syntax = parse_preview(&app.mdi.input, &app.axes(), None, [0.0; 3]);
        app.mdi.syntax_ok = app.mdi.input.trim().is_empty() || syntax.error.is_none();
        ui.horizontal(|ui| {
            let (text, color) = if app.mdi.input.trim().is_empty() {
                ("ENTER ISSUES / MDI".to_string(), theme::MUTED)
            } else if let Some(error) = &syntax.error {
                (format!("SYNTAX ERROR: {}", error.message), theme::WARNING)
            } else {
                (
                    "SYNTAX OK / ENTER ISSUES VIA ISSUE_MDI".to_string(),
                    theme::SUCCESS,
                )
            };
            ui.label(
                RichText::new(text)
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(color),
            );
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                if theme::action_button(ui, "CLEAR", true).clicked() {
                    app.mdi.input.clear();
                    app.mdi.last_error = None;
                }
                if theme::accent_button(ui, "ISSUE MDI", in_mdi).clicked() {
                    issue(app);
                }
            });
        });
        theme::separator(ui);
        theme::section_header(ui, "QUICK INSERT");
        ui.horizontal_wrapped(|ui| {
            let quick = app.mdi.quick_commands.clone();
            for code in quick {
                if theme::chip(ui, &code, false).clicked() {
                    insert(app, &code);
                }
            }
        });
        ui.add_space(4.0);
        theme::section_header(ui, "REQUIREMENTS");
        let requirements = [
            ("MACHINE ON", app.snapshot.task.power == MachinePower::On),
            (
                "ALL AXES HOMED",
                !app.snapshot.machine.homed.is_empty()
                    && app.snapshot.machine.homed.iter().all(|(_, homed)| *homed),
            ),
            (
                "INTERPRETER IDLE",
                app.snapshot.task.interp == linc_core::model::InterpState::Idle,
            ),
            ("NO ACTIVE PROGRAM", !app.snapshot.task.running),
            (
                "PANEL LINK",
                app.snapshot.panel.connected && !app.snapshot.panel.communication_error,
            ),
            ("PANEL MODE MDI", in_mdi),
        ];
        ui.horizontal_wrapped(|ui| {
            for (label, ok) in requirements {
                ui.label(
                    RichText::new(format!("[{}] {}", if ok { "OK" } else { "--" }, label))
                        .size(theme::font_size(10.0))
                        .color(if ok { theme::SUCCESS } else { theme::ERROR }),
                );
            }
        });
        if let Some(error) = &app.mdi.last_error {
            ui.label(RichText::new(error).color(theme::ERROR));
        }
        ui.label(
            RichText::new(
                "Leaving MDI discards unissued input; issued commands are not cancelled.",
            )
            .size(theme::font_size(9.0))
            .color(theme::MUTED),
        );
    });
}

fn highlighted_input(app: &mut LincApp, ui: &mut Ui, in_mdi: bool) -> egui::Response {
    let mut text = app.mdi.input.clone();
    let mut layouter = |ui: &egui::Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| {
        let mut job = egui::text::LayoutJob::default();
        for token in lex_line(buffer.as_str()) {
            let format = TextFormat {
                font_id: FontId::monospace(theme::font_size(19.0)),
                color: token_color(token.kind),
                italics: token.kind == TokenKind::Comment,
                ..Default::default()
            };
            job.append(&token.display(), 0.0, format);
        }
        job.wrap.max_width = wrap_width;
        ui.fonts_mut(|fonts| fonts.layout_job(job))
    };
    let response = ui.add_enabled(
        in_mdi,
        egui::TextEdit::singleline(&mut text)
            .font(FontId::monospace(theme::font_size(19.0)))
            .desired_width(ui.available_width())
            .hint_text("G1 X10 Y5 F300")
            .layouter(&mut layouter),
    );
    if text != app.mdi.input {
        app.mdi.input = text;
        app.mdi.last_error = None;
    }
    response
}

fn insert(app: &mut LincApp, code: &str) {
    if !app.mdi.input.is_empty() && !app.mdi.input.ends_with(' ') {
        app.mdi.input.push(' ');
    }
    app.mdi.input.push_str(code);
}

pub fn issue(app: &mut LincApp) {
    app.mdi.last_error = None;
    let command = app.mdi.input.trim().to_string();
    issue_block(app, command);
}

fn issue_block(app: &mut LincApp, command: String) {
    if command.is_empty() {
        app.mdi.last_error = Some("MDI command is empty".into());
        return;
    }
    if command.len() > app.profile.config.mdi.max_command_length {
        app.mdi.last_error = Some(format!(
            "command exceeds {} characters",
            app.profile.config.mdi.max_command_length
        ));
        return;
    }
    if app.snapshot.panel.mode.mode() != Some(PanelMode::Mdi) {
        app.mdi.last_error = Some("panel mode is not MDI".into());
        return;
    }
    if let Err(error) = app.snapshot.issue_allowed() {
        app.mdi.last_error = Some(error);
        return;
    }
    let preview = parse_preview(&command, &app.axes(), None, [0.0; 3]);
    if let Some(error) = preview.error {
        app.mdi.last_error = Some(format!("line {}: {}", error.line, error.message));
        return;
    }
    app.send(MachineCommand::IssueMdi {
        block: command.clone(),
        origin: MdiOrigin::Mdi,
    });
    app.mdi
        .push_history(app.profile.config.mdi.history_limit, command.clone());
    app.mdi.last_result = Some(command);
    app.mdi.input.clear();
    app.mdi.focus = true;
}

fn issue_history(app: &mut LincApp, index: usize) {
    let Some((command, _)) = app.mdi.history.get(index).cloned() else {
        return;
    };
    app.mdi.input = command.clone();
    app.mdi.history_index = Some(index);
    app.mdi.last_error = None;
    issue_block(app, command);
}

fn last_result_card(app: &LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "LAST ISSUED / RESULT");
        if let Some(result) = &app.mdi.last_result {
            common::token_ui(ui, result, 12.0);
        } else {
            ui.label(
                RichText::new("NO COMMAND ISSUED")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        }
        if let Some(error) = &app.mdi.last_error {
            ui.label(
                RichText::new(error)
                    .size(theme::font_size(10.0))
                    .color(theme::ERROR),
            );
        }
    });
}

fn mdi_telemetry(app: &LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "POSITION / MODAL");
        ui.spacing_mut().interact_size.y = 22.0;
        egui::Grid::new("mdi-positions")
            .num_columns(4)
            .spacing([10.0, 2.0])
            .show(ui, |ui| {
                for header in ["AXIS", "WORK", "MACHINE", "DTG"] {
                    ui.label(
                        RichText::new(header)
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                }
                ui.end_row();
                for axis in app.axes() {
                    let position = app.snapshot.positions.axes.get(&axis);
                    ui.label(
                        RichText::new(&axis)
                            .monospace()
                            .size(theme::font_size(11.0))
                            .color(theme::C3),
                    );
                    ui.label(
                        RichText::new(format!("{:+.3}", position.map(|p| p.work).unwrap_or(0.0)))
                            .monospace()
                            .size(theme::font_size(10.0))
                            .color(theme::FG),
                    );
                    ui.label(
                        RichText::new(format!(
                            "{:+.3}",
                            position.map(|p| p.machine).unwrap_or(0.0)
                        ))
                        .monospace()
                        .size(theme::font_size(10.0))
                        .color(theme::MUTED),
                    );
                    ui.label(
                        RichText::new(format!("{:.3}", position.map(|p| p.dtg).unwrap_or(0.0)))
                            .monospace()
                            .size(theme::font_size(10.0))
                            .color(theme::MUTED),
                    );
                    ui.end_row();
                }
            });
        theme::separator(ui);
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            if app.snapshot.program.modal.is_empty() {
                ui.label(RichText::new("NO MODAL STATE").color(theme::MUTED));
            } else {
                for (key, value) in &app.snapshot.program.modal {
                    ui.label(
                        RichText::new(common::modal_text(key, value))
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::FG),
                    );
                }
            }
            ui.label(
                RichText::new(format!("WCS {}", app.active_wcs_label()))
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::ACCENT),
            );
        });
    });
}

fn history_card(app: &mut LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        ui.horizontal(|ui| {
            theme::section_header(ui, "MDI HISTORY");
            ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new("LOCAL / BOUNDED")
                        .size(theme::font_size(9.0))
                        .color(theme::MUTED),
                );
            });
        });
        let mut recall: Option<String> = None;
        let mut run_history = None;
        egui::ScrollArea::vertical()
            .id_salt("mdi-history")
            .max_height(120.0)
            .show(ui, |ui| {
                for (index, (command, timestamp)) in app.mdi.history.iter().enumerate() {
                    let selected = app.mdi.history_index == Some(index);
                    let response = ui.selectable_label(
                        selected,
                        RichText::new(format!("{}  {}", format_clock(*timestamp), command))
                            .monospace()
                            .size(theme::font_size(10.0))
                            .color(if selected {
                                theme::ACCENT
                            } else {
                                theme::MUTED
                            }),
                    );
                    if response.clicked() {
                        recall = Some(command.clone());
                        app.mdi.history_index = Some(index);
                    }
                    if response.has_focus()
                        && ui.input_mut(|input| {
                            input.consume_key(egui::Modifiers::NONE, egui::Key::Enter)
                        })
                    {
                        app.mdi.history_index = Some(index);
                        recall = Some(command.clone());
                        run_history = Some(index);
                    }
                }
            });
        if let Some(command) = recall {
            app.mdi.input = command;
            app.mdi.focus = true;
        }
        let selected = app.mdi.history_index;
        ui.horizontal(|ui| {
            if theme::accent_button(ui, "RUN SELECTED", selected.is_some()).clicked() {
                run_history = selected;
            }
            ui.label(
                RichText::new("ENTER ON A ROW OR CTRL+ENTER")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
        if ui.input_mut(|input| input.consume_key(egui::Modifiers::CTRL, egui::Key::Enter)) {
            run_history = selected;
        }
        if let Some(index) = run_history {
            issue_history(app, index);
        }
        ui.horizontal(|ui| {
            let up =
                ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp));
            let down = ui
                .input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown));
            if up && !app.mdi.history.is_empty() {
                let index = match app.mdi.history_index {
                    Some(index) => (index + 1).min(app.mdi.history.len() - 1),
                    None => 0,
                };
                app.mdi.history_index = Some(index);
                app.mdi.input = app.mdi.history[index].0.clone();
            }
            if down {
                if let Some(index) = app.mdi.history_index {
                    if index == 0 {
                        app.mdi.history_index = None;
                        app.mdi.input.clear();
                    } else {
                        app.mdi.history_index = Some(index - 1);
                        app.mdi.input = app.mdi.history[index - 1].0.clone();
                    }
                }
            }
            ui.label(
                RichText::new("UP/DOWN RECALL")
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
    });
}

fn token_color(kind: TokenKind) -> egui::Color32 {
    match kind {
        TokenKind::GCode => theme::C1,
        TokenKind::MCode => theme::C2,
        TokenKind::Axis => theme::C3,
        TokenKind::Number => theme::FG,
        TokenKind::FeedSpeed => theme::C4,
        TokenKind::Tool => theme::C5,
        TokenKind::Parameter => theme::C6,
        TokenKind::Variable => theme::C6,
        TokenKind::Comment => theme::COMMENT,
        TokenKind::ProgramNumber => theme::C6,
        TokenKind::LineNumber => theme::LINE_NUMBER,
        TokenKind::OWord => theme::C2,
        TokenKind::Other => theme::FG,
    }
}
