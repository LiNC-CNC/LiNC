//! Read-only diagnostics: overview, logs, HAL/NML, turret.

use egui::{RichText, Ui};
use linc_core::model::{format_clock, format_elapsed, Alarm, AlarmSeverity, AlarmSource};
use linc_hal::browser::{HalBrowser, HalPin, HalSignal, HalSnapshot};

use crate::screens::common;
use crate::shell::LincApp;
use crate::state::{severity_index, source_index, DiagnosticsTab, HalPane};
use crate::theme;

pub fn show(app: &mut LincApp, ui: &mut Ui) {
    let tabs = DiagnosticsTab::visible(&app.profile);
    // A tab remembered or restored from an earlier profile can name one this
    // profile hides; nothing below may draw its body.
    if !tabs.contains(&app.diagnostics.tab) {
        app.diagnostics.tab = DiagnosticsTab::Overview;
    }
    // No page title: the navigation names the page.
    common::focus_anchor(ui, "diagnostics-page-focus");
    ui.horizontal(|ui| {
        // PROBE's strip: words, not buttons. The active tab is the accent one
        // with a rule under it.
        ui.spacing_mut().item_spacing.x = 16.0;
        for tab in tabs {
            if common::text_tab(ui, tab.label(), app.diagnostics.tab == tab).clicked() {
                app.diagnostics.tab = tab;
            }
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new("READ-ONLY")
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(theme::MUTED),
            );
        });
    });
    // The hairline PROBE draws under its strip, so the strip reads as a header
    // and not as the first row of the page.
    let rule = ui.cursor().top();
    ui.painter().line_segment(
        [
            egui::Pos2::new(ui.min_rect().left(), rule),
            egui::Pos2::new(ui.min_rect().right(), rule),
        ],
        egui::Stroke::new(1.0, theme::BORDER),
    );
    ui.add_space(4.0);
    // Entering HAL / NML or LOGS puts the keyboard on the list, not in the
    // search line: the single-letter toggles are the point of those tabs, and a
    // focused field swallows every one of them. `/` goes to the search line
    // from there. A first frame on the page counts as entering it.
    // The frame number is read before the store is locked: asking the context
    // inside `data_mut` re-enters the same lock.
    let frame = ui.ctx().cumulative_frame_nr();
    let page_opened = ui.data_mut(|data| {
        let key = egui::Id::unique("linc-diagnostics-last-frame");
        let opened = data
            .get_temp::<u64>(key)
            .map(|last| last + 1 != frame)
            .unwrap_or(true);
        data.insert_temp(key, frame);
        opened
    });
    if page_opened || app.diagnostics.tab != app.diagnostics.last_tab {
        let searchable = matches!(
            app.diagnostics.tab,
            DiagnosticsTab::HalNml | DiagnosticsTab::Logs
        );
        app.diagnostics.list_focused = searchable;
        app.diagnostics.focus_list = searchable;
        app.diagnostics.last_tab = app.diagnostics.tab;
    }
    egui::ScrollArea::vertical()
        .id_salt("diagnostics-body")
        .max_height(ui.available_height())
        .show(ui, |ui| match app.diagnostics.tab {
            DiagnosticsTab::Overview => overview(app, ui),
            DiagnosticsTab::HalNml => hal_nml(app, ui),
            DiagnosticsTab::Turret => turret(app, ui),
            DiagnosticsTab::Logs => logs(app, ui),
        });
}

/// `theme::card`'s frame margin, on each side.
const CARD_PAD: f32 = 12.0;

/// Draw a card pinned to `width`, and to `height` when one is given.
///
/// A frame takes its size from its content, so a card inside a column of fixed
/// height used to end on its last row and leave the column's remainder empty.
/// The body is laid out top-down here instead of inheriting the caller's layout,
/// and the frame content is given the exact rect the column reserved.
fn pinned_card(ui: &mut Ui, width: f32, height: Option<f32>, add: impl FnOnce(&mut Ui)) {
    theme::card(ui, |ui| {
        ui.set_width((width - 2.0 * CARD_PAD).max(0.0));
        if let Some(height) = height {
            ui.set_height((height - 2.0 * CARD_PAD).max(0.0));
        }
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), add);
    });
}

/// The width of one of `remaining` equal columns left in the row, so rounding
/// never accumulates and the last card still ends on the page margin.
fn even_width(ui: &Ui, remaining: f32, gap: f32) -> f32 {
    (ui.available_width() - gap * (remaining - 1.0)) / remaining
}

/// A card as wide as `width` whose height follows its rows.
fn width_pinned_card(ui: &mut Ui, width: f32, add: impl FnOnce(&mut Ui)) {
    pinned_card(ui, width, None, add);
}

/// A card that occupies exactly `width` x `height`.
fn sized_card(ui: &mut Ui, width: f32, height: f32, add: impl FnOnce(&mut Ui)) {
    pinned_card(ui, width, Some(height), add);
}

fn overview(app: &mut LincApp, ui: &mut Ui) {
    // `C` clears the view: the counters go to zero and the rows below start
    // after this moment. Nothing is acknowledged or reset on the controller.
    if !ui.ctx().text_edit_focused()
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::C))
    {
        app.clear_log_view();
    }
    // The page's card rhythm: 12 px between the cards across a row.
    const COLUMN_GAP: f32 = 12.0;
    let available = ui.available_width();
    // Each card is as tall as its three lines and pins its own width: a fixed
    // height left a blank strip under the detail line of all four.
    ui.horizontal_top(|ui| {
        ui.spacing_mut().item_spacing.x = COLUMN_GAP;
        health_card(
            ui,
            even_width(ui, 4.0, COLUMN_GAP),
            "CONTROL",
            if app.snapshot.machine.estop {
                "E-STOP"
            } else {
                "HEALTHY"
            },
            app.snapshot.machine.estop,
            &format!(
                "{} / {}",
                app.snapshot.task.mode.label(),
                app.snapshot.task.interp.label()
            ),
        );
        health_card(
            ui,
            even_width(ui, 3.0, COLUMN_GAP),
            "PANEL LINK",
            // The error latch counts: a link that is connected but reporting
            // errors is not online, and the card must not say otherwise.
            if app.panel_offline() {
                "OFFLINE"
            } else {
                "ONLINE"
            },
            app.panel_offline(),
            &if app.snapshot.panel.link.available {
                format!(
                    "{} {} / CRC {} / TO {}",
                    app.snapshot.panel.capabilities.transport.to_uppercase(),
                    app.snapshot.panel.link.port,
                    app.snapshot.panel.link.crc_errors,
                    app.snapshot.panel.link.timeouts
                )
            } else {
                format!(
                    "{} / COUNTERS UNAVAILABLE",
                    app.snapshot.panel.capabilities.transport.to_uppercase()
                )
            },
        );
        health_card(
            ui,
            even_width(ui, 2.0, COLUMN_GAP),
            "NML / COMMAND",
            if app.snapshot.machine.nml_command_connected {
                "CONNECTED"
            } else {
                "LOST"
            },
            !app.snapshot.machine.nml_command_connected,
            &format!(
                "STATUS {} ms / ECHO {}",
                app.snapshot.machine.status_age_ms, app.snapshot.machine.echo_serial
            ),
        );
        let following = max_following_error(app);
        let servo = max_servo_load(app);
        health_card(
            ui,
            even_width(ui, 1.0, COLUMN_GAP),
            "MOTION",
            if following > 50.0 {
                "ELEVATED"
            } else {
                "NOMINAL"
            },
            following > 50.0,
            &format!("FOLLOW {following:.1} µm / SERVO {servo:.0}%"),
        );
    });
    ui.add_space(6.0);
    // The link strip, then the log. One card each: the strip is two lines of
    // polled counter state, the log takes the rest of the page at full width.
    width_pinned_card(ui, available, |ui| link_strip(app, ui));
    ui.add_space(6.0);
    // The log runs down to the status line, less a small pad, so the page has
    // no dead band under the card at any window height.
    const ROW_PAD: f32 = 8.0;
    let row_height = (ui.available_height() - ROW_PAD).max(240.0);
    sized_card(ui, available, row_height, |ui| {
        let room = ui.available_height();
        let top = ui.cursor().top();
        ui.horizontal(|ui| {
            theme::section_header(ui, "ACTIVE ALARMS & MESSAGES");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let logs = app.logs();
                let alarms = logs.iter().filter(|a| a.severity.is_alarm()).count();
                let messages = logs.len().saturating_sub(alarms);
                for (label, count) in [
                    ("MESSAGES", messages),
                    ("ALARMS", alarms),
                    ("ALL", logs.len()),
                ] {
                    ui.label(
                        RichText::new(format!("{label} {count}"))
                            .monospace()
                            .size(theme::font_size(8.0))
                            .color(theme::MUTED),
                    );
                    ui.add_space(8.0);
                }
            });
        });
        // The whole card is the table now, so it scrolls through the log
        // rather than cutting it at five rows.
        let header = ui.cursor().top() - top;
        let logs = app.logs();
        egui::ScrollArea::vertical()
            .id_salt("overview-alarms-scroll")
            .max_height((room - header).max(60.0))
            .show(ui, |ui| log_table(ui, "overview-alarms", &logs, usize::MAX));
    });
}

/// The RS-485 / virtual panel link as one strip on the overview.
///
/// Every value is the polled panel snapshot's own, painted in the same frame the
/// header row reads, so the strip cannot outlive the link it describes: `LAST
/// RX` and `PANEL AGE` are the two clocks that show a feed going quiet. The
/// byte counters appear only on a transport that keeps them.
fn link_strip(app: &LincApp, ui: &mut Ui) {
    let link = &app.snapshot.panel.link;
    let transport = app.snapshot.panel.capabilities.transport.to_uppercase();
    compact_rows(ui);
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 14.0;
        theme::section_header(ui, "PANEL");
        pair(ui, "TRANSPORT", transport.clone());
        // A virtual or HAL transport names itself twice; one field is enough.
        let port = link.port.trim();
        if !port.is_empty() && !port.eq_ignore_ascii_case(&transport) {
            pair(ui, "PORT", port);
        }
        if link.available {
            pair(ui, "BAUD", link.baud.to_string());
            pair(ui, "CYCLE", format!("{:.1} ms", link.cycle_time_ms));
        }
        pair(
            ui,
            "LAST RX",
            link.last_rx_age_ms
                .map(format_elapsed)
                .unwrap_or_else(|| "—".into()),
        );
        pair(ui, "AGE", panel_age(app));
        if link.available {
            pair(
                ui,
                "RX / TX",
                format!("{} / {}", link.rx_bytes, link.tx_bytes),
            );
            pair(ui, "RECONN", link.reconnects.to_string());
        }
    });
    if !link.available {
        ui.label(
            RichText::new(
                "This transport keeps no byte counters; the values above are live, byte counts are not tracked.",
            )
            .size(theme::font_size(8.5))
            .color(theme::MUTED),
        );
    }
}

/// A label and its value, inline, for the strip inside a card.
fn pair(ui: &mut Ui, label: &str, value: impl Into<String>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            RichText::new(label)
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        ui.label(
            RichText::new(value.into())
                .monospace()
                .size(theme::font_size(11.0))
                .color(theme::FG),
        );
    });
}

/// How long ago the panel produced the snapshot this page is drawing.
///
/// The field is a wall-clock stamp, not a duration: subtract it from now. A
/// backend that does not stamp its snapshots leaves it at zero, and an age
/// measured from the epoch would be absurd, so zero reads as unknown.
fn panel_age(app: &LincApp) -> String {
    let stamped = app.snapshot.panel.snapshot_age_ms;
    if stamped == 0 {
        return "—".into();
    }
    format_elapsed(linc_core::model::now_ms().saturating_sub(stamped))
}

/// Compact a card's rows so each key/value line reads as one line.
///
/// `interact_size.y` sizes a `ui.horizontal` row and the minimum height of a
/// grid row, so at `MIN_TARGET` every row carries a blank line of 44 pt plus
/// `item_spacing.y`: eight rows overflowed a summary card, and the same pitch
/// spaced the diagnostics panes apart. The spacing is set on the card's own
/// `Ui`, so it does not leak to the rest of the page.
fn compact_rows(ui: &mut Ui) {
    ui.spacing_mut().interact_size.y = 14.0;
    ui.spacing_mut().item_spacing.y = 2.0;
}

fn health_card(ui: &mut Ui, width: f32, title: &str, value: &str, fault: bool, detail: &str) {
    width_pinned_card(ui, width, |ui| {
        theme::section_header(ui, title);
        ui.label(
            RichText::new(value)
                .size(theme::font_size(20.0))
                .strong()
                .color(if fault { theme::ERROR } else { theme::SUCCESS }),
        );
        ui.label(
            RichText::new(detail)
                .monospace()
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
    });
}

/// How old a HAL snapshot may be before the browser calls it stale, as a
/// multiple of the poll period: a slow poll is not a dead one, so the threshold
/// has to follow the interval the operator chose.
const HAL_STALE_POLLS: u64 = 3;

/// The NML channels as one line.
///
/// The question this tab answers about NML is "is the bridge talking", not
/// "what does every channel say"; the channel detail lives in the HAL pins the
/// browser lists below.
fn nml_status_line(app: &LincApp, ui: &mut Ui) {
    let machine = &app.snapshot.machine;
    // One text line, not the 44 pt touch-target row the theme reserves: the
    // strip is a readout, and the reserved height pushed the browser down.
    ui.scope(|ui| {
        ui.spacing_mut().interact_size.y = 16.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 10.0;
            theme::status_dot(
                ui,
                if machine.nml_command_connected {
                    theme::SUCCESS
                } else {
                    theme::ERROR
                },
                "CMD",
                if machine.nml_command_connected {
                    "command channel connected"
                } else {
                    "command channel lost"
                },
            );
            theme::status_dot(
                ui,
                if machine.nml_status_connected {
                    theme::SUCCESS
                } else {
                    theme::ERROR
                },
                "STATUS",
                if machine.nml_status_connected {
                    "status channel connected"
                } else {
                    "status channel lost"
                },
            );
            for (text, color) in [
                (
                    format!("STATUS {}", format_elapsed(machine.status_age_ms)),
                    theme::MUTED,
                ),
                (
                    format!("COMMAND {}", format_elapsed(machine.command_age_ms)),
                    theme::MUTED,
                ),
                (format!("ECHO {}", machine.echo_serial), theme::MUTED),
                (
                    if machine.hal_thread_ok {
                        "HAL THREAD OK".to_string()
                    } else {
                        "HAL THREAD FAULT".to_string()
                    },
                    if machine.hal_thread_ok {
                        theme::MUTED
                    } else {
                        theme::ERROR
                    },
                ),
            ] {
                ui.label(
                    RichText::new(text)
                        .monospace()
                        .size(theme::font_size(10.0))
                        .color(color),
                );
            }
        });
    });
}

fn hal_nml(app: &mut LincApp, ui: &mut Ui) {
    nml_status_line(app, ui);
    ui.add_space(4.0);
    hal_browser(app, ui);
}

/// One drawn row of the pin tree: a component header or one of its pins.
enum HalRow<'a> {
    Component(&'a str, usize),
    Pin(&'a HalPin),
}

/// Flatten the polled tree into the rows the scroll area pages through.
fn hal_rows<'a>(snapshot: &'a HalSnapshot, filter: &str) -> Vec<HalRow<'a>> {
    let mut rows = Vec::new();
    for component in snapshot.components() {
        let matching: Vec<&HalPin> = snapshot
            .pins_of(component)
            .filter(|pin| {
                filter.is_empty()
                    || pin.name.to_ascii_lowercase().contains(filter)
                    || pin
                        .signal
                        .as_deref()
                        .is_some_and(|signal| signal.to_ascii_lowercase().contains(filter))
            })
            .collect();
        if matching.is_empty() {
            continue;
        }
        rows.push(HalRow::Component(component, matching.len()));
        rows.extend(matching.into_iter().map(HalRow::Pin));
    }
    rows
}

/// The haltui-style HAL browser: pins grouped by owning component on the left,
/// signals on the right, both from the same `halcmd` poll.
///
/// `halcmd` is the only enumeration path this build links, so a session without
/// it gets an explicit unavailable state: the pane never falls back to the
/// machine snapshot's pin shorthand, which would show a plausible tree that HAL
/// never reported.
fn hal_browser(app: &mut LincApp, ui: &mut Ui) {
    // The poller starts the first time this tab is drawn: no thread runs while
    // nobody is looking at HAL.
    let browser = app.diagnostics.hal.get_or_insert_with(HalBrowser::default);
    let snapshot = browser.snapshot();
    let age = snapshot.age_ms();
    // Three missed polls, and never less than three seconds: a 10 s period must
    // not read as stale while it is on schedule.
    let stale_ms = (browser.interval().as_millis() as u64 * HAL_STALE_POLLS).max(3_000);

    // Keys are read before the filter is drawn: a singleline `TextEdit` takes
    // Enter for itself, so a handler after it would never see the key.
    hal_list_keys(app, ui, (ui.available_height() - 8.0).max(240.0));
    // One keyboard target for both panes: the cursors choose the pane, the
    // page keys scroll it. Two focusable panes fought egui's own arrow focus
    // walk, which runs before the page can claim the key.
    let anchor = list_anchor(ui, "linc-hal-list");
    take_entry_focus(app, &anchor);

    ui.horizontal(|ui| {
        // One line: the theme's 44 pt touch target would make the filter box
        // two text lines tall on a strip that holds a single one.
        ui.spacing_mut().interact_size.y = 20.0;
        theme::section_header(ui, "HAL BROWSER");
        ui.add_space(6.0);
        let search = ui.add(
            egui::TextEdit::singleline(app.diagnostics.search_for_mut(DiagnosticsTab::HalNml))
                .id(egui::Id::unique("linc-diagnostics-hal-search"))
                .hint_text("filter component, pin or signal")
                .desired_width(260.0),
        );
        take_clear_focus(app, &search);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let (text, color) = match (snapshot.available, age) {
                (false, _) => ("HALCMD UNAVAILABLE".to_string(), theme::ERROR),
                (true, Some(age)) if age > stale_ms => (
                    format!(
                        "{:.1} s · {} PINS · {} SIGNALS · STALE",
                        age as f64 / 1000.0,
                        snapshot.pins.len(),
                        snapshot.signals.len()
                    ),
                    theme::WARNING,
                ),
                (true, Some(age)) => (
                    format!(
                        "{:.1} s · {} PINS · {} SIGNALS",
                        age as f64 / 1000.0,
                        snapshot.pins.len(),
                        snapshot.signals.len()
                    ),
                    theme::MUTED,
                ),
                (true, None) => ("POLLING".to_string(), theme::MUTED),
            };
            ui.label(
                RichText::new(text)
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(color),
            );
        });
    });
    ui.add_space(4.0);

    if !snapshot.available {
        theme::banner(ui, theme::WARNING, "HAL NOT AVAILABLE");
        if let Some(error) = &snapshot.error {
            ui.label(
                RichText::new(error)
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(theme::ERROR),
            );
        }
        ui.label(
            RichText::new(
                "The browser runs `halcmd -s show pin` and `halcmd -s show sig`, so it needs a running LinuxCNC session with halcmd on PATH. No pin values are filled in from the machine snapshot.",
            )
            .size(theme::font_size(10.0))
            .color(theme::MUTED),
        );
        // The list stays the keyboard's owner: an empty browser is still the
        // pane Esc returns from, and a focus model that disagreed with the
        // widget holding the caret would lose the next key.
        return;
    }

    let filter = app
        .diagnostics
        .search_for(DiagnosticsTab::HalNml)
        .to_ascii_lowercase();
    let rows = hal_rows(&snapshot, &filter);
    let signals: Vec<&HalSignal> = snapshot
        .signals
        .iter()
        .filter(|signal| {
            filter.is_empty()
                || signal.name.to_ascii_lowercase().contains(&filter)
                || signal
                    .pins
                    .iter()
                    .any(|pin| pin.to_ascii_lowercase().contains(&filter))
        })
        .collect();
    let height = (ui.available_height() - 8.0).max(240.0);

    ui.horizontal_top(|ui| {
        let gap = 8.0;
        let left_width = ((ui.available_width() - gap) * 0.62).max(320.0);
        ui.allocate_ui_with_layout(
            egui::Vec2::new(left_width, height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    pane_marker(
                        ui,
                        app.diagnostics.list_focused && app.diagnostics.hal_pane == HalPane::Pins,
                    );
                    if rows.is_empty() {
                        // `halcmd` exits 0 with no rows when no HAL session is
                        // running, so an empty tree is a different report from
                        // a filter that matched nothing.
                        let text = if snapshot.pins.is_empty() {
                            "HAL reports no pins: no LinuxCNC session is running"
                        } else {
                            "no pin matches the filter"
                        };
                        ui.label(
                            RichText::new(text)
                                .size(theme::font_size(10.0))
                                .color(theme::MUTED),
                        );
                        return;
                    }
                    ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
                    let mut table = hal_table(ui, "hal-pins-table", height - 32.0);
                    if let Some(offset) = app.diagnostics.hal_pins_scroll.take() {
                        table = table.vertical_scroll_offset(offset);
                    }
                    let out = table
                        .header(browser_row_height(), |mut header| {
                            for label in ["PIN", "TYPE", "DIR", "VALUE", "SIGNAL"] {
                                header.col(|ui| browser_header(ui, label));
                            }
                        })
                        .body(|body| {
                            body.rows(browser_row_height(), rows.len(), |mut row| {
                                let index = row.index();
                                match rows[index] {
                                    HalRow::Component(name, count) => {
                                        row.col(|ui| {
                                            ui.label(
                                                RichText::new(name)
                                                    .monospace()
                                                    .size(theme::font_size(11.0))
                                                    .strong()
                                                    .color(theme::ACCENT),
                                            );
                                            browser_dim(ui, &format!("{count} pins"), theme::MUTED);
                                        });
                                    }
                                    HalRow::Pin(pin) => pin_cells(&mut row, pin),
                                }
                            });
                        });
                    app.diagnostics.hal_pins_offset = out.state.clamped_offset().y;
                    app.diagnostics.hal_pins_scrollable =
                        out.content_size.y > out.inner_rect.height() + 1.0;
                });
            },
        );
        ui.add_space((gap - ui.spacing().item_spacing.x).max(0.0));
        let right_width = ui.available_width().max(240.0);
        ui.allocate_ui_with_layout(
            egui::Vec2::new(right_width, height),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                theme::card(ui, |ui| {
                    pane_marker(
                        ui,
                        app.diagnostics.list_focused
                            && app.diagnostics.hal_pane == HalPane::Signals,
                    );
                    theme::section_header(ui, "SIGNALS");
                    if signals.is_empty() {
                        let text = if snapshot.signals.is_empty() {
                            "HAL reports no signals"
                        } else {
                            "no signal matches the filter"
                        };
                        ui.label(
                            RichText::new(text)
                                .size(theme::font_size(10.0))
                                .color(theme::MUTED),
                        );
                        return;
                    }
                    ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
                    let mut table = signal_table(ui, height - 56.0);
                    if let Some(offset) = app.diagnostics.hal_signals_scroll.take() {
                        table = table.vertical_scroll_offset(offset);
                    }
                    let out = table.body(|body| {
                        body.rows(browser_row_height(), signals.len(), |mut row| {
                            let signal = signals[row.index()];
                            row.col(|ui| {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(&signal.name)
                                            .monospace()
                                            .size(theme::font_size(10.0))
                                            .color(theme::FG),
                                    )
                                    .truncate(),
                                )
                                .on_hover_text(
                                    if signal.pins.is_empty() {
                                        "no pin is linked to this signal".to_string()
                                    } else {
                                        signal.pins.join("\n")
                                    },
                                );
                            });
                            row.col(|ui| {
                                ui.label(
                                    RichText::new(&signal.value)
                                        .monospace()
                                        .size(theme::font_size(10.0))
                                        .color(hal_value_color(&signal.value)),
                                );
                            });
                            row.col(|ui| {
                                browser_dim(ui, &signal.ty, theme::MUTED);
                            });
                            row.col(|ui| {
                                browser_dim(
                                    ui,
                                    &format!("{} pins", signal.pins.len()),
                                    theme::MUTED,
                                );
                            });
                        });
                    });
                    app.diagnostics.hal_signals_offset = out.state.clamped_offset().y;
                    app.diagnostics.hal_signals_scrollable =
                        out.content_size.y > out.inner_rect.height() + 1.0;
                });
            },
        );
    });
}

/// A focusable 1 px strip: the keyboard target of a list.
///
/// The arrows stay on the list, so egui does not walk the focus into another
/// widget when the operator is moving between the panes or scrolling them.
fn list_anchor(ui: &mut Ui, salt: &str) -> egui::Response {
    let (rect, _) = ui.allocate_exact_size(
        egui::Vec2::new(ui.available_width(), 1.0),
        egui::Sense::hover(),
    );
    let response = ui.interact(
        rect,
        egui::Id::unique(salt),
        egui::Sense::focusable_noninteractive(),
    );
    ui.memory_mut(|memory| {
        memory.set_focus_lock_filter(
            response.id,
            egui::EventFilter {
                tab: true,
                horizontal_arrows: true,
                vertical_arrows: true,
                escape: false,
            },
        )
    });
    response
}

/// A page step in points: what one PageUp/PageDown moves a list by.
fn page_step(height: f32) -> f32 {
    (height - 40.0).max(browser_row_height() * 3.0)
}

/// The keys that walk the HAL browser: Enter leaves the filter for the list,
/// the cursors choose the pane, the page keys scroll it, and Esc goes back to
/// the filter.
fn hal_list_keys(app: &mut LincApp, ui: &mut Ui, height: f32) {
    let search_id = egui::Id::unique("linc-diagnostics-hal-search");
    let list_id = egui::Id::unique("linc-hal-list");
    let search_focused = ui.memory(|memory| memory.has_focus(search_id));
    let enter = search_focused
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    if enter {
        app.diagnostics.list_focused = true;
        ui.memory_mut(|memory| memory.request_focus(list_id));
        return;
    }
    if !app.diagnostics.list_focused {
        return;
    }
    if search_key(ui) {
        app.diagnostics.list_focused = false;
        ui.memory_mut(|memory| memory.request_focus(search_id));
        return;
    }
    // A click into the filter is the operator leaving the list.
    if search_focused {
        app.diagnostics.list_focused = false;
        return;
    }
    let (escape, left, right, page_up, page_down, faster, slower) = ui.input_mut(|input| {
        (
            input.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft),
            input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight),
            input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp),
            input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown),
            input.consume_key(egui::Modifiers::NONE, egui::Key::F),
            input.consume_key(egui::Modifiers::NONE, egui::Key::S),
        )
    });
    if escape {
        app.diagnostics.list_focused = false;
        ui.memory_mut(|memory| memory.request_focus(search_id));
        return;
    }
    // egui walks the focus on an arrow key before the page can claim it, when
    // the lock filter is not yet on the widget. Take the anchor back: the keys
    // are the list's either way.
    if !ui.memory(|memory| memory.has_focus(list_id)) {
        ui.memory_mut(|memory| memory.request_focus(list_id));
    }
    if faster || slower {
        // The poll rate is live: halcmd is asked again on the next turn.
        let current = app
            .diagnostics
            .hal
            .as_ref()
            .map(|browser| browser.interval())
            .unwrap_or(linc_hal::browser::MIN_INTERVAL);
        let doubled = if faster { current / 2 } else { current * 2 };
        let next = doubled
            .max(linc_hal::browser::MIN_INTERVAL)
            .min(linc_hal::browser::MAX_INTERVAL);
        if let Some(browser) = &app.diagnostics.hal {
            browser.set_interval(next);
        }
        app.toast(
            format!("HAL POLL {:.1} s", next.as_secs_f32()),
            theme::SUCCESS,
        );
    }
    if left || right {
        app.diagnostics.hal_pane = if left {
            HalPane::Pins
        } else {
            HalPane::Signals
        };
    }
    if page_up || page_down {
        let (offset, scrollable) = match app.diagnostics.hal_pane {
            HalPane::Pins => (
                app.diagnostics.hal_pins_offset,
                app.diagnostics.hal_pins_scrollable,
            ),
            HalPane::Signals => (
                app.diagnostics.hal_signals_offset,
                app.diagnostics.hal_signals_scrollable,
            ),
        };
        // A pane that fits has nothing to scroll: the key keeps its page
        // meaning there instead of doing nothing.
        if scrollable {
            let step = page_step(height);
            let delta = if page_down { step } else { -step };
            let target = (offset + delta).max(0.0);
            match app.diagnostics.hal_pane {
                HalPane::Pins => app.diagnostics.hal_pins_scroll = Some(target),
                HalPane::Signals => app.diagnostics.hal_signals_scroll = Some(target),
            }
        } else if page_down {
            app.page_down();
        } else {
            app.page_up();
        }
    }
}

/// After Esc cleared the search text, put the caret back in the field.
fn take_clear_focus(app: &mut LincApp, field: &egui::Response) {
    if app.diagnostics.focus_search {
        field.request_focus();
        app.diagnostics.focus_search = false;
    }
}

/// Take the keyboard into the list when its tab was just entered.
fn take_entry_focus(app: &mut LincApp, anchor: &egui::Response) {
    if app.diagnostics.focus_list {
        anchor.request_focus();
        app.diagnostics.focus_list = false;
    }
}

/// `/` moves the keyboard from the list to the search line — the same key the
/// Help page uses — and reports whether it did.
///
/// The key's own text event is dropped with it: the caret moves in this frame,
/// so a surviving `Text("/")` would be typed into the field that just took it.
fn search_key(ui: &mut Ui) -> bool {
    let hit = ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Slash));
    if hit {
        ui.input_mut(|input| {
            input
                .events
                .retain(|event| !matches!(event, egui::Event::Text(text) if text == "/"))
        });
    }
    hit
}

/// A 2 px accent rule on the pane the cursors act on.
fn pane_marker(ui: &mut Ui, active: bool) {
    let (rect, _) = ui.allocate_exact_size(
        egui::Vec2::new(ui.available_width(), 2.0),
        egui::Sense::hover(),
    );
    if active {
        ui.painter().rect_filled(rect, 1.0, theme::ACCENT);
    }
}

/// The same walk for the log list:/// The same walk for the log list: Enter leaves the search line for the table,
/// the page keys scroll it, Esc returns to the search line.
fn logs_list_keys(app: &mut LincApp, ui: &mut Ui, height: f32) {
    let search_id = egui::Id::unique("linc-diagnostics-search");
    let list_id = egui::Id::unique("linc-logs-list");
    let search_focused = ui.memory(|memory| memory.has_focus(search_id));
    let enter = search_focused
        && ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Enter));
    if enter {
        app.diagnostics.list_focused = true;
        ui.memory_mut(|memory| memory.request_focus(list_id));
        return;
    }
    if !app.diagnostics.list_focused {
        return;
    }
    if search_key(ui) {
        app.diagnostics.list_focused = false;
        ui.memory_mut(|memory| memory.request_focus(search_id));
        return;
    }
    if search_focused {
        app.diagnostics.list_focused = false;
        return;
    }
    let (escape, page_up, page_down) = ui.input_mut(|input| {
        (
            input.consume_key(egui::Modifiers::NONE, egui::Key::Escape),
            input.consume_key(egui::Modifiers::NONE, egui::Key::PageUp),
            input.consume_key(egui::Modifiers::NONE, egui::Key::PageDown),
        )
    });
    if escape {
        app.diagnostics.list_focused = false;
        ui.memory_mut(|memory| memory.request_focus(search_id));
        return;
    }
    if !ui.memory(|memory| memory.has_focus(list_id)) {
        ui.memory_mut(|memory| memory.request_focus(list_id));
    }
    log_toggle_keys(app, ui);
    if page_up || page_down {
        // A short log fits the card: the page keys fall through to the tabs
        // rather than doing nothing.
        if app.diagnostics.logs_scrollable {
            let step = page_step(height);
            let delta = if page_down { step } else { -step };
            let target = (app.diagnostics.logs_offset + delta).max(0.0);
            app.diagnostics.logs_scroll = Some(target);
        } else if page_down {
            app.page_down();
        } else {
            app.page_up();
        }
    }
}

/// The pins table, so both halves of the browser share one column rhythm./// The pins table, so both halves of the browser share one column rhythm.
fn hal_table<'a>(ui: &'a mut Ui, id: &str, max_height: f32) -> egui_extras::TableBuilder<'a> {
    egui_extras::TableBuilder::new(ui)
        .id_salt(id)
        .column(egui_extras::Column::remainder().at_least(150.0).clip(true))
        .column(egui_extras::Column::exact(42.0))
        .column(egui_extras::Column::exact(30.0))
        .column(egui_extras::Column::exact(72.0))
        .column(egui_extras::Column::remainder().at_least(90.0).clip(true))
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .striped(true)
        .max_scroll_height(max_height)
}

fn signal_table<'a>(ui: &'a mut Ui, max_height: f32) -> egui_extras::TableBuilder<'a> {
    egui_extras::TableBuilder::new(ui)
        .id_salt("hal-signals-table")
        .column(egui_extras::Column::remainder().at_least(110.0).clip(true))
        .column(egui_extras::Column::exact(62.0))
        .column(egui_extras::Column::exact(40.0))
        .column(egui_extras::Column::exact(52.0))
        .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
        .striped(true)
        .max_scroll_height(max_height)
}

/// Row height for the browser tables: one monospace line plus air.
fn browser_row_height() -> f32 {
    (theme::font_size(11.0) * 1.35).max(15.0)
}

fn browser_header(ui: &mut Ui, label: &str) {
    ui.label(
        RichText::new(label)
            .size(theme::font_size(9.0))
            .strong()
            .color(theme::MUTED),
    );
}

fn browser_dim(ui: &mut Ui, text: &str, color: egui::Color32) {
    ui.label(
        RichText::new(text)
            .monospace()
            .size(theme::font_size(9.0))
            .color(color),
    );
}

/// One pin row: name, type, direction, live value and the signal it rides.
fn pin_cells(row: &mut egui_extras::TableRow<'_, '_>, pin: &HalPin) {
    let dir_color = match pin.dir.as_str() {
        "IN" => theme::MUTED,
        "OUT" => theme::C1,
        _ => theme::WARNING,
    };
    row.col(|ui| {
        ui.add(
            egui::Label::new(
                RichText::new(&pin.name)
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(theme::FG),
            )
            .truncate(),
        );
    });
    row.col(|ui| browser_dim(ui, &pin.ty, theme::MUTED));
    row.col(|ui| browser_dim(ui, &pin.dir, dir_color));
    row.col(|ui| {
        ui.label(
            RichText::new(&pin.value)
                .monospace()
                .size(theme::font_size(10.0))
                .color(hal_value_color(&pin.value)),
        );
    });
    let (signal, color) = match &pin.signal {
        Some(signal) => (signal.as_str(), theme::C3),
        None => ("—", theme::MUTED),
    };
    row.col(|ui| {
        ui.add(
            egui::Label::new(
                RichText::new(signal)
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(color),
            )
            .truncate(),
        );
    });
}
/// Bit pins print TRUE/FALSE; colour those, leave numbers plain.
fn hal_value_color(value: &str) -> egui::Color32 {
    match value {
        "TRUE" => theme::SUCCESS,
        "FALSE" => theme::MUTED,
        _ => theme::FG,
    }
}
fn turret(app: &mut LincApp, ui: &mut Ui) {
    theme::card(ui, |ui| {
        theme::section_header(ui, "TURRET / LIVE TOOL");
        compact_rows(ui);
        let Some(atc) = app.snapshot.atc.as_ref() else {
            ui.label(
                RichText::new("No turret/ATC configured for this profile.").color(theme::MUTED),
            );
            return;
        };
        theme::kv_row(ui, "STATIONS", atc.pockets.to_string());
        theme::kv_row(
            ui,
            "STATION",
            atc.station
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
        );
        theme::kv_row(
            ui,
            "CURRENT POCKET",
            atc.current_pocket
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
        );
        theme::kv_row(
            ui,
            "PREPARED",
            atc.prepared_pocket
                .map(|v| v.to_string())
                .unwrap_or_else(|| "—".into()),
        );
        theme::kv_row(
            ui,
            "CLAMP",
            atc.clamped
                .map(|v| if v { "CLAMPED" } else { "UNCLAMPED" })
                .unwrap_or("UNKNOWN"),
        );
        theme::kv_row(ui, "INDEX", if atc.indexing { "INDEXING" } else { "READY" });
        theme::kv_row(
            ui,
            "LIVE TOOL RPM",
            atc.live_tool_rpm
                .map(|v| format!("{v:.0}"))
                .unwrap_or_else(|| "OFF".into()),
        );
        theme::kv_row(
            ui,
            "LIVE TOOL DIR",
            if atc.live_tool_direction >= 0 {
                "CW"
            } else {
                "CCW"
            },
        );
        theme::kv_row(
            ui,
            "ORIENTATION",
            match atc.live_tool_axial {
                Some(true) => "AXIAL",
                Some(false) => "RADIAL",
                None => "UNKNOWN",
            },
        );
        if let Some(fault) = &atc.fault {
            ui.label(RichText::new(fault).color(theme::ERROR));
        }
    });
}

/// The severity toggles, in the order their keys follow.
const LOG_SEVERITIES: [(AlarmSeverity, &str, &str); 4] = [
    (AlarmSeverity::Info, "I", "INFO"),
    (AlarmSeverity::Warning, "W", "WARN"),
    (AlarmSeverity::Error, "E", "ERROR"),
    (AlarmSeverity::Critical, "C", "CRIT"),
];

/// The source toggles and the key that flips each one. Every letter is free on
/// this page: the shell's single-letter commands are page-local.
const LOG_SOURCES: [(AlarmSource, &str, &str); 10] = [
    (AlarmSource::Panel, "P", "PANEL"),
    (AlarmSource::Program, "G", "PROGRAM"),
    (AlarmSource::Probe, "B", "PROBE"),
    (AlarmSource::Tool, "T", "TOOL"),
    (AlarmSource::Hal, "H", "HAL"),
    (AlarmSource::Nml, "N", "NML"),
    (AlarmSource::Motion, "M", "MOTION"),
    (AlarmSource::Io, "O", "I/O"),
    (AlarmSource::Ui, "U", "UI"),
    (AlarmSource::System, "Y", "SYSTEM"),
];

fn logs(app: &mut LincApp, ui: &mut Ui) {
    // Before the search line: the field takes Enter for itself.
    logs_list_keys(app, ui, (ui.available_height() - 40.0).max(120.0));
    ui.spacing_mut().interact_size.y = 18.0;
    // Wrapped: a narrow window drops the status line to its own row instead of
    // clipping the source names off the right edge.
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new("SEARCH")
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        let search = ui.add(
            egui::TextEdit::singleline(app.diagnostics.search_for_mut(DiagnosticsTab::Logs))
                .id(egui::Id::unique("linc-diagnostics-search"))
                .hint_text("message, code, source")
                .desired_width(240.0),
        );
        take_clear_focus(app, &search);
        ui.add_space(10.0);
        // The toggles are state, not buttons: the status line reports which
        // ones are on, and the legend names the keys that flip them.
        // The bits, not `shows_*`: an empty group means "no filter", and the
        // strip reports that as ALL rather than as every value selected.
        let severities: Vec<(&str, bool)> = LOG_SEVERITIES
            .iter()
            .map(|(severity, _, label)| {
                (
                    *label,
                    app.diagnostics.filter.severities & (1 << severity_index(*severity)) != 0,
                )
            })
            .collect();
        log_toggle_group(ui, "SEV", &severities);
        ui.add_space(10.0);
        let sources: Vec<(&str, bool)> = LOG_SOURCES
            .iter()
            .map(|(source, _, label)| {
                (
                    *label,
                    app.diagnostics.filter.sources & (1 << source_index(*source)) != 0,
                )
            })
            .collect();
        log_toggle_group(ui, "SRC", &sources);
    });
    ui.add_space(4.0);
    theme::card(ui, |ui| {
        let anchor = list_anchor(ui, "linc-logs-list");
        take_entry_focus(app, &anchor);
        let all = app.logs();
        let total = all.len();
        let logs: Vec<Alarm> = all
            .into_iter()
            .filter(|alarm| app.diagnostics.filter.shows_severity(alarm.severity))
            .filter(|alarm| app.diagnostics.filter.shows_source(alarm.source))
            .filter(|alarm| {
                let query = app
                    .diagnostics
                    .search_for(DiagnosticsTab::Logs)
                    .to_ascii_lowercase();
                query.is_empty()
                    || alarm.message.to_ascii_lowercase().contains(&query)
                    || alarm.code.to_ascii_lowercase().contains(&query)
                    || alarm.source.label().to_ascii_lowercase().contains(&query)
            })
            .collect();
        // Solid bars: a list longer than the card must show it, not fade out
        // the bar until the pointer happens to pass over it.
        ui.style_mut().spacing.scroll = egui::style::ScrollStyle::solid();
        let mut area = egui::ScrollArea::vertical()
            .id_salt("log-scroll")
            .max_height(480.0);
        if let Some(offset) = app.diagnostics.logs_scroll.take() {
            area = area.vertical_scroll_offset(offset);
        }
        let out = area.show(ui, |ui| log_table(ui, "log-grid", &logs, usize::MAX));
        app.diagnostics.logs_offset = out.state.clamped_offset().y;
        app.diagnostics.logs_scrollable = out.content_size.y > out.inner_rect.height() + 1.0;
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(format!("{} / {} EVENTS", logs.len(), total))
                    .monospace()
                    .size(theme::font_size(9.0))
                    .color(theme::MUTED),
            );
        });
    });
}

/// The letter toggles: one per severity, one per source, and `A` for none.
///
/// They are read only while the list holds the keyboard, so a letter typed in
/// the search line stays text.
fn log_toggle_keys(app: &mut LincApp, ui: &mut Ui) {
    let mut changed = false;
    let mut cleared = false;
    ui.input_mut(|input| {
        for (severity, key, label) in LOG_SEVERITIES {
            if input.consume_key(egui::Modifiers::NONE, letter_key(key)) {
                let on = app.diagnostics.filter.toggle_severity(severity);
                changed = true;
                app.toast(
                    format!("{} {label}", if on { "FILTER ON" } else { "FILTER OFF" }),
                    if on { theme::ACCENT } else { theme::MUTED },
                );
            }
        }
        for (source, key, label) in LOG_SOURCES {
            if input.consume_key(egui::Modifiers::NONE, letter_key(key)) {
                let on = app.diagnostics.filter.toggle_source(source);
                changed = true;
                app.toast(
                    format!("{} {label}", if on { "FILTER ON" } else { "FILTER OFF" }),
                    if on { theme::ACCENT } else { theme::MUTED },
                );
            }
        }
        if input.consume_key(egui::Modifiers::NONE, egui::Key::A) {
            cleared = true;
        }
    });
    if cleared {
        app.diagnostics.filter.clear();
        app.toast("FILTERS CLEARED", theme::MUTED);
    } else if !changed {
        return;
    }
    // A new filter starts at the top of the list: the rows the reader was
    // looking at are not the rows they asked for.
    app.diagnostics.logs_scroll = Some(0.0);
}

/// The `egui::Key` for one of the toggle letters.
fn letter_key(letter: &str) -> egui::Key {
    match letter {
        "I" => egui::Key::I,
        "W" => egui::Key::W,
        "E" => egui::Key::E,
        "C" => egui::Key::C,
        "P" => egui::Key::P,
        "G" => egui::Key::G,
        "B" => egui::Key::B,
        "T" => egui::Key::T,
        "H" => egui::Key::H,
        "N" => egui::Key::N,
        "M" => egui::Key::M,
        "O" => egui::Key::O,
        "U" => egui::Key::U,
        "Y" => egui::Key::Y,
        _ => egui::Key::A,
    }
}

/// One group of the status line: its name, then every value either accented
/// (its toggle is on) or dimmed (off). A group with nothing on shows `ALL`,
/// which is what an empty filter means.
fn log_toggle_group(ui: &mut Ui, name: &str, entries: &[(&str, bool)]) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(
            RichText::new(name)
                .size(theme::font_size(9.0))
                .color(theme::MUTED),
        );
        if !entries.iter().any(|(_, on)| *on) {
            ui.label(
                RichText::new("ALL")
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(theme::ACCENT),
            );
            return;
        }
        for (label, on) in entries {
            ui.label(
                RichText::new(*label)
                    .monospace()
                    .size(theme::font_size(10.0))
                    .color(if *on { theme::ACCENT } else { theme::MUTED }),
            );
        }
    });
}

pub fn copy_log(app: &mut LincApp, ui: &mut Ui) {
    let logs = app.logs();
    let text = logs
        .iter()
        .map(|alarm| {
            format!(
                "{} {} {} {} {}",
                format_clock(alarm.timestamp_ms),
                alarm.severity.label(),
                alarm.source.label(),
                alarm.code,
                alarm.message
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    ui.ctx().copy_text(text);
    app.toast("log copied to clipboard", theme::SUCCESS);
}

/// The alarm table.
///
/// Not an `egui::Grid`: a grid row cannot size itself around a wrapped cell, so
/// a long message spilled into the row below. Each entry is one horizontal line
/// whose last cell is a width-bounded scope, so the message wraps inside its
/// column and the row grows to fit it, with the metadata aligned to the top.
fn log_table(ui: &mut Ui, _id: &str, logs: &[Alarm], limit: usize) {
    const TIME: f32 = 66.0;
    const SEVERITY: f32 = 74.0;
    const SOURCE: f32 = 56.0;
    const CODE: f32 = 64.0;
    const GAP: f32 = 12.0;
    let tight = ui.spacing().item_spacing.y;
    ui.spacing_mut().item_spacing.y = 2.0;
    log_row(ui, [TIME, SEVERITY, SOURCE, CODE], GAP, None);
    for alarm in logs.iter().take(limit) {
        log_row(ui, [TIME, SEVERITY, SOURCE, CODE], GAP, Some(alarm));
        ui.add_space(3.0);
    }
    ui.spacing_mut().item_spacing.y = tight;
}

fn log_column(ui: &mut Ui, width: f32, text: &str, colour: egui::Color32, strong: bool) {
    ui.allocate_ui_with_layout(
        egui::vec2(width, 0.0),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            let mut rich = RichText::new(text)
                .monospace()
                .size(theme::font_size(9.0))
                .color(colour);
            if strong {
                rich = rich.strong();
            }
            ui.add(egui::Label::new(rich).wrap());
        },
    );
}

fn log_row(ui: &mut Ui, widths: [f32; 4], gap: f32, alarm: Option<&Alarm>) {
    let header = ["TIME", "SEVERITY", "SOURCE", "CODE"];
    ui.horizontal_top(|ui| {
        for (index, width) in widths.into_iter().enumerate() {
            let (text, colour, strong) = match alarm {
                None => (header[index].to_string(), theme::MUTED, true),
                Some(alarm) => {
                    let text = match index {
                        0 => format_clock(alarm.timestamp_ms),
                        1 => alarm.severity.label().to_string(),
                        2 => alarm.source.label().to_string(),
                        _ => {
                            if alarm.code.is_empty() {
                                "—".to_string()
                            } else {
                                alarm.code.clone()
                            }
                        }
                    };
                    let colour = if index == 1 {
                        severity_color(alarm.severity)
                    } else {
                        theme::MUTED
                    };
                    (text, colour, false)
                }
            };
            log_column(ui, width, &text, colour, strong);
        }
        ui.add_space(gap);
        // Whatever is left of the row is the message column.
        let rest = ui.available_width().max(80.0);
        ui.scope(|ui| {
            ui.set_max_width(rest);
            match alarm {
                None => {
                    ui.label(
                        RichText::new("MESSAGE")
                            .size(theme::font_size(9.0))
                            .strong()
                            .color(theme::MUTED),
                    );
                }
                Some(alarm) => {
                    ui.add(
                        egui::Label::new(
                            RichText::new(&alarm.message)
                                .size(theme::font_size(10.0))
                                .color(if alarm.severity == AlarmSeverity::Info {
                                    theme::MUTED
                                } else {
                                    severity_color(alarm.severity)
                                }),
                        )
                        .wrap(),
                    );
                }
            }
        });
    });
}

fn severity_color(severity: AlarmSeverity) -> egui::Color32 {
    match severity {
        AlarmSeverity::Info => theme::MUTED,
        AlarmSeverity::Warning => theme::WARNING,
        AlarmSeverity::Error | AlarmSeverity::Critical => theme::ERROR,
    }
}

fn max_following_error(app: &LincApp) -> f64 {
    app.snapshot
        .machine
        .following_error
        .iter()
        .map(|(_, value)| value.abs())
        .fold(0.0, f64::max)
}

fn max_servo_load(app: &LincApp) -> f64 {
    app.snapshot
        .machine
        .servo_load
        .iter()
        .map(|(_, value)| *value)
        .fold(0.0, f64::max)
}
