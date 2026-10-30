//! Fixed chrome: the top bar, the bottom bar, the messages strip, banners and
//! the icon and metric helpers they paint with.

use super::*;

pub(super) fn metric(ui: &mut egui::Ui, label: &str, value: String, size: f32) {
    metric_colored(ui, label, value, size, theme::FG);
}

/// A top-bar metric whose value carries a state colour: the label stays quiet,
/// the value is what has to be read at a glance.
pub(super) fn metric_colored(
    ui: &mut egui::Ui,
    label: &str,
    value: String,
    size: f32,
    color: Color32,
) {
    ui.add_space(6.0);
    ui.vertical(|ui| {
        ui.label(
            RichText::new(label)
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        ui.label(RichText::new(value).monospace().size(size).color(color));
    });
}

impl LincApp {
    pub(super) fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("LiNC")
                            .size(theme::font_size(16.0))
                            .strong()
                            .color(theme::FG),
                    );
                    if self.is_sim() {
                        ui.add_space(4.0);
                        let (rect, _) =
                            ui.allocate_exact_size(Vec2::new(38.0, 18.0), Sense::hover());
                        ui.painter().rect_filled(rect, 3, theme::WARNING);
                        ui.painter().text(
                            rect.center(),
                            Align2::CENTER_CENTER,
                            "SIM",
                            FontId::monospace(theme::font_size(10.0)),
                            Color32::from_rgb(0x1a, 0x12, 0x04),
                        );
                    }
                    ui.label(
                        RichText::new(self.profile.config.machine.name.clone())
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(format!("PANEL {}", self.snapshot.panel.mode.label()))
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(if self.snapshot.panel.mode.is_valid() {
                                theme::ACCENT
                            } else {
                                theme::ERROR
                            }),
                    );
                    ui.label(
                        RichText::new(format!("TASK {}", self.snapshot.task.mode.label()))
                            .monospace()
                            .size(theme::font_size(9.0))
                            .color(theme::MUTED),
                    );
                });
            });
            ui.add_space(18.0);
            metric(ui, "PROGRAM", self.program_label(), 13.0);
            let (state_word, state_color) = program_state_word(&self.snapshot.program);
            metric_colored(ui, "STATE", state_word.into(), 13.0, state_color);
            metric(
                ui,
                "TOOL",
                format!("T{}", self.snapshot.tool_change.current_tool),
                13.0,
            );
            metric(
                ui,
                "SPINDLE",
                self.snapshot
                    .spindles
                    .first()
                    .map(|spindle| format!("S {:.0}", spindle.actual_speed))
                    .unwrap_or_else(|| "S --".into()),
                13.0,
            );
            metric(
                ui,
                "FEED",
                format!("F {:.0}", self.snapshot.feed.feed),
                13.0,
            );
            metric(ui, "WORK", self.active_wcs_label(), 13.0);
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(
                    RichText::new(format_clock(now_ms()))
                        .monospace()
                        .size(theme::font_size(13.0))
                        .color(theme::MUTED),
                );
                // Render rate beside the clock: the first number to ask about
                // when the panel feels slow.
                ui.add_space(8.0);
                ui.label(
                    RichText::new(format!("{:.0} FPS", self.fps))
                        .monospace()
                        .size(theme::font_size(9.0))
                        .color(if self.fps < 30.0 {
                            theme::WARNING
                        } else {
                            theme::MUTED
                        }),
                );
                ui.add_space(14.0);
                let (messages, alarms) = self.log_counts();
                let alarm_color = if alarms > 0 {
                    theme::ERROR
                } else {
                    theme::MUTED
                };
                ui.label(
                    RichText::new(format!("ALARM {alarms}"))
                        .monospace()
                        .size(theme::font_size(12.0))
                        .color(alarm_color),
                );
                ui.label(
                    RichText::new(format!("MSG {messages}"))
                        .monospace()
                        .size(theme::font_size(12.0))
                        .color(if messages > 0 {
                            theme::FG
                        } else {
                            theme::MUTED
                        }),
                );
                ui.add_space(14.0);
                theme::status_dot(
                    ui,
                    if self.snapshot.panel.connected && !self.snapshot.panel.communication_error {
                        theme::SUCCESS
                    } else {
                        theme::ERROR
                    },
                    if self.snapshot.panel.connected && !self.snapshot.panel.communication_error {
                        "PANEL ONLINE"
                    } else {
                        "PANEL OFFLINE"
                    },
                );
            });
        });
    }

    /// Controller messages, one line above the hint row.
    ///
    /// The controller is the only thing that reports why a program stopped, and
    /// those messages used to live only on the Machine page. The newest few are
    /// shown on every page, newest first, coloured by severity; the row
    /// disappears when the controller has said nothing.
    pub(super) fn messages_strip(&self, ui: &mut egui::Ui) {
        if self.snapshot.alarms.is_empty() {
            return;
        }
        let size = theme::font_size(9.5);
        let errors = self
            .snapshot
            .alarms
            .iter()
            .filter(|alarm| {
                matches!(
                    alarm.severity,
                    linc_core::model::AlarmSeverity::Error
                        | linc_core::model::AlarmSeverity::Critical
                )
            })
            .count();
        egui::containers::Panel::bottom("linc-messages")
            .exact_size(20.0)
            .frame(
                Frame::new()
                    .fill(theme::PANEL)
                    .inner_margin(Margin::symmetric(16, 2)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new(if errors > 0 {
                            format!("MESSAGES · {errors} ERROR")
                        } else {
                            "MESSAGES".to_string()
                        })
                        .size(size)
                        .strong()
                        .color(if errors > 0 {
                            theme::ERROR
                        } else {
                            theme::MUTED
                        }),
                    );
                    ui.add_space(8.0);
                    for alarm in self.snapshot.alarms.iter().take(3) {
                        let colour = match alarm.severity {
                            linc_core::model::AlarmSeverity::Critical
                            | linc_core::model::AlarmSeverity::Error => theme::ERROR,
                            linc_core::model::AlarmSeverity::Warning => theme::WARNING,
                            linc_core::model::AlarmSeverity::Info => theme::MUTED,
                        };
                        // The strip is one row high: long messages are cut here
                        // and read in full in the messages dialog (M).
                        let text: String = alarm.message.chars().take(64).collect();
                        ui.label(RichText::new(text).size(size).color(colour));
                        ui.add_space(12.0);
                    }
                });
            });
    }

    /// The centre field of the bottom bar: the work position of every axis,
    /// then the same axes with the distance still to go.
    ///
    /// Two groups rather than interleaved pairs: X/Y/Z in one block reads as the
    /// position, and the DTG block lines up under it axis for axis. Painted on
    /// the Program page it replaces the single moving-axis DTG field the bar
    /// otherwise carries, so the distance is never shown twice.
    pub(super) fn coordinate_status_line(&self) -> String {
        let width = self.coordinate_field_width();
        let mut position = String::new();
        let mut dtg = String::new();
        for axis in self.axes() {
            // Same frame as the DRO cards: the bar and the cards disagreeing
            // about commanded vs actual would read as a position glitch.
            let Some((work, _machine, entry_dtg)) = screens::common::axis_display(self, &axis)
            else {
                continue;
            };
            position.push_str(&format!("{axis}{:+width$.3} ", work, width = width));
            dtg.push_str(&format!("{axis}{:+width$.3} ", entry_dtg, width = width));
        }
        format!("{}  DTG: {}", position.trim_end(), dtg.trim_end())
    }

    /// Characters in one coordinate field, sign and decimals included.
    ///
    /// Sized by the largest number that can appear - the machine envelope and
    /// the loaded program's extents - never by what is on screen: a field that
    /// widened when an axis crossed 10.000 made the whole line shuffle while the
    /// machine moved and the distance-to-go counted down.
    pub(super) fn coordinate_field_width(&self) -> usize {
        const DECIMALS: usize = 3;
        let mut extent: f64 = 0.0;
        for bounds in [
            self.machine_bounds(),
            self.program.last_valid_preview.bounds,
        ]
        .into_iter()
        .flatten()
        {
            for axis in 0..3 {
                extent = extent
                    .max(bounds.min[axis].abs())
                    .max(bounds.max[axis].abs());
            }
        }
        // Sign, the integer digits of the extent, the point, the decimals.
        1 + format!("{:.*}", DECIMALS, extent).len()
    }

    /// True where the bar shows the full coordinate line, so the single
    /// moving-axis DTG field steps aside instead of repeating it.
    pub(super) fn shows_coordinate_status(&self) -> bool {
        self.nav.page == Page::Program
    }

    /// Bottom bar: keyboard commands on the left, read-only machine status on
    /// the right. It carries no buttons.
    pub(super) fn bottom_bar(&mut self, ui: &mut egui::Ui) {
        let size = theme::font_size(11.0);
        ui.horizontal(|ui| {
            let field = |ui: &mut egui::Ui, text: String, color: egui::Color32| {
                ui.label(RichText::new(text).monospace().size(size).color(color));
            };
            // Left: what the machine is doing, in fields that appear only when
            // they have something to say. The bottom bar carries live machine
            // state alone; nothing here is an action.
            for (text, color) in self.status_fields() {
                field(ui, text, color);
                ui.add_space(16.0);
            }

            // The program's own numbers - the work position it is cutting to
            // and the distance left to get there - in the left-hand flow, after
            // the machine fields. Only on the Program page; the jog screens
            // carry the same figures in their own readout, and the bar is
            // read-only everywhere.
            if self.shows_coordinate_status() {
                let bar = ui.max_rect();
                let text = self.coordinate_status_line();
                let galley = ui
                    .painter()
                    .layout_no_wrap(text, FontId::monospace(size), theme::FG);
                // Never let it run into the left or the right block: the bar is
                // 1280 px wide at the minimum and those two need about 600.
                if galley.size().x + 24.0 < bar.width() * 0.5 {
                    // Left-aligned in the flow, right after the machine fields:
                    // the operator reads the bar left to right, and a centred
                    // block floats away from the line it belongs to.
                    let pos = egui::pos2(ui.cursor().min.x, bar.center().y - galley.size().y / 2.0);
                    ui.painter().galley(pos, galley, theme::FG);
                }
            }

            // Right: the read-only machine state.
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                // Never claim OK on stale data: the controller may be latched
                // while the last snapshot still says otherwise.
                let (estop_text, estop_color) = if self.snapshot.meta.stale {
                    ("E-STOP ?".to_string(), theme::WARNING)
                } else if self.snapshot.machine.estop {
                    ("E-STOP ACTIVE".to_string(), theme::ERROR)
                } else {
                    ("E-STOP OK".to_string(), theme::SUCCESS)
                };
                field(ui, estop_text, estop_color);
                ui.add_space(16.0);
                // Machine on/off sits immediately left of the E-stop state.
                // `halui.machine.is-on` is the authority; a stale snapshot
                // reports `?` rather than guessing. A running machine ON stays
                // steady; MACHINE OFF blinks so it cannot fade into a bar full
                // of green.
                let machine_is_off = !self.snapshot.machine.on && !self.snapshot.meta.stale;
                let (machine_text, machine_color) =
                    machine_status_field(self.snapshot.machine.on, self.snapshot.meta.stale);
                let machine_color = if machine_is_off {
                    blink_fault_color(ui)
                } else {
                    machine_color
                };
                field(ui, machine_text, machine_color);
                ui.add_space(16.0);
                // The wired probe input, right beside the machine state: the
                // operator proves the input by touching the probe with a
                // finger, which is a fault (`PROBE TRIPPED`) and not a
                // reading on the page that owns the probe. A profile without
                // a probe gets no dead field, and neither does a machine whose
                // spindle holds something else: the field claims a probe is
                // fitted, so it is drawn only while the probe tool is in the
                // spindle. Absent, not dimmed - the group is laid out from the
                // right, so the rest of the bar does not move.
                if probe_fitted(
                    self.profile.config.probe.enabled,
                    self.profile.config.probe.tool_number,
                    self.snapshot.tool_change.current_tool,
                ) {
                    let (probe_text, probe_color) = probe_input_field(
                        self.snapshot.probe.input_tripped,
                        self.snapshot.meta.stale,
                    );
                    field(ui, probe_text, probe_color);
                    ui.add_space(16.0);
                }
                // Protect is only worth a field when it is engaged: the off
                // state is the normal one and the row is space-limited.
                if self.program_protected() {
                    field(ui, "PROTECT ON".to_string(), theme::WARNING);
                    ui.add_space(16.0);
                }
                field(
                    ui,
                    override_readout(
                        self.snapshot.feed.feed_override,
                        self.snapshot.feed.rapid_override,
                        self.snapshot.feed.spindle_override,
                    ),
                    theme::MUTED,
                );
                if self.snapshot.limits_overridden {
                    ui.add_space(16.0);
                    limits_override_indicator(ui);
                }
                ui.add_space(16.0);
                field(
                    ui,
                    self.snapshot.panel.mode.label().to_string(),
                    if self.snapshot.panel.mode.is_valid() {
                        theme::ACCENT
                    } else {
                        theme::ERROR
                    },
                );
            });
        });
    }

    pub(super) fn banners(&mut self, ui: &mut egui::Ui) {
        // The E-stop is the most severe state and is never hidden behind a tab.
        if self.snapshot.machine.estop {
            theme::banner(
                ui,
                theme::ERROR,
                "EMERGENCY STOP ACTIVE — MOTION AND MACHINE ACTIONS BLOCKED",
            );
            ui.add_space(4.0);
        }
        if self.snapshot.limits_overridden {
            theme::banner(
                ui,
                theme::WARNING,
                "LIMITS OVERRIDE ACTIVE — LIMIT SWITCHES ARE IGNORED",
            );
            ui.add_space(4.0);
        }
        if self.panel_offline() {
            theme::banner(ui, theme::ERROR, "PANEL OFFLINE — MACHINE ACTIONS BLOCKED");
            ui.add_space(4.0);
        } else if self.invalid_mode() {
            theme::banner(ui, theme::ERROR, "PANEL MODE INVALID — FAILS CLOSED");
            ui.add_space(4.0);
        }
        if let Some(fault) = self.snapshot.probe_trip.clone() {
            let text = probe_trip_banner_text(&fault);
            if theme::banner_action(ui, theme::ERROR, &text, PROBE_TRIP_ACTION) {
                self.clear_probe_trip();
            }
            ui.add_space(4.0);
        }
        if self.program_protected()
            && matches!(
                self.nav.page,
                Page::Program | Page::Offsets | Page::Tools | Page::Probe | Page::Files
            )
        {
            theme::banner(
                ui,
                theme::WARNING,
                "PROTECT ON — EDITING LOCKED. Buffer preserved; writes, imports and applies are blocked.",
            );
            ui.add_space(4.0);
        }
        if self.nav.page == Page::Program && self.program.external_change {
            theme::banner(
                ui,
                theme::WARNING,
                "PROGRAM CHANGED EXTERNALLY — unsaved buffer preserved. Save will use the current revision.",
            );
            ui.add_space(4.0);
        }
    }
}

/// State the bars paint: connection and protect edges, the lathe readout mode,
/// and the counters the alarm strip compares frame to frame.
pub(crate) struct ChromeUi {
    pub quit_requested: bool,
    pub last_snapshot_ms: u64,
    /// Highest alarm id the operator has been told about. Controller errors used
    /// to appear only in the Machine page's message card, so a program that
    /// aborted looked like it had simply jumped to the end.
    pub alarm_id_seen: u64,
    pub last_connection: Option<bool>,
    pub last_protect: Option<bool>,
    /// Lathe only: read the X position as a diameter (`2X`) instead of a
    /// radius. A display factor — the controller keeps working in radius, and
    /// no command is issued from it.
    pub lathe_diameter: bool,
}
