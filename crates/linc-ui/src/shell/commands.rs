//! Everything that reaches the backend: sending a command, draining events,
//! writing to the UI log, and the move-progress arithmetic the bars read.

use super::*;

/// Fraction of the current move already travelled, from the remaining distance.
///
/// `remaining` is the magnitude of the DTG — the sign only says which way the
/// move goes — and `total` is the largest remainder seen since the axis last
/// stopped, i.e. the length of the move. The bar starts empty, fills as the
/// remainder shrinks, and sits full once the axis arrives. A move that turns out
/// to be longer than the one recorded resets the total, so the bar can never go
/// past empty.
///
/// Returns `(fraction, total)`, where `total` is what the caller should keep for
/// the next observation.
pub fn move_progress(remaining: f64, total: f64) -> (f64, f64) {
    const AT_REST: f64 = 1e-6;
    let remaining = remaining.abs();
    if remaining <= AT_REST {
        // Nothing left to travel: the move is complete and the next one starts
        // from a fresh total.
        return (1.0, 0.0);
    }
    let total = total.max(remaining);
    ((1.0 - remaining / total).clamp(0.0, 1.0), total)
}

impl LincApp {
    /// Send without polling the backend back.
    ///
    /// `send` polls, so anything called from `poll_backend` must use this or it
    /// recurses until the stack overflows.
    pub(super) fn send_quiet(&mut self, command: MachineCommand) {
        if let Err(error) = self.backend.send(command) {
            self.log_ui(
                AlarmSeverity::Error,
                AlarmSource::Ui,
                "UI-CMD",
                error.to_string(),
            );
        }
    }

    pub fn send(&mut self, command: MachineCommand) {
        if let Err(error) = self.backend.send(command) {
            self.toast(error.to_string(), theme::ERROR);
            self.log_ui(
                AlarmSeverity::Error,
                AlarmSource::Ui,
                "UI-CMD",
                error.to_string(),
            );
        }
        self.poll_backend();
    }

    /// `send`, reporting whether the backend accepted the command.
    ///
    /// The reporting is identical, but a caller that owns a dialog needs the
    /// answer: closing the dialog over a refused write makes the save look like
    /// it happened. Fires the same poll, so the snapshot is current either way.
    pub(super) fn try_send(&mut self, command: MachineCommand) -> bool {
        let accepted = match self.backend.send(command) {
            Ok(()) => true,
            Err(error) => {
                self.toast(error.to_string(), theme::ERROR);
                self.log_ui(
                    AlarmSeverity::Error,
                    AlarmSource::Ui,
                    "UI-CMD",
                    error.to_string(),
                );
                false
            }
        };
        self.poll_backend();
        accepted
    }

    /// Turn each axis's remaining distance into a fraction of its current move.
    pub(super) fn update_move_progress(&mut self) {
        let mut updates = Vec::new();
        for (axis, position) in self.snapshot.positions.axes.iter() {
            let total = self.view.move_distance.get(axis).copied().unwrap_or(0.0);
            let (fraction, total) = move_progress(position.dtg, total);
            updates.push((axis.to_string(), total, fraction));
        }
        for (axis, total, fraction) in updates {
            self.view.move_distance.insert(axis.clone(), total);
            self.view.move_progress.insert(axis, fraction);
        }
    }

    pub fn poll_backend(&mut self) {
        let mut snapshot = self.backend.snapshot();
        if self.view.trail_cleared {
            if cleared_trail_pending(&snapshot, self.view.trail_cleared_tip) {
                let tip = snapshot.tool_tip.unwrap_or([0.0; 3]);
                snapshot.live_trail =
                    std::sync::Arc::new(vec![linc_core::model::TrailPoint::new(tip, false)]);
            } else {
                self.view.trail_cleared = false;
            }
        }
        self.snapshot = snapshot;
        self.sample_following_error();
        // Keep the last known work zero: loading a program can leave the
        // snapshot without an active system for a poll, and the cross must not
        // jump to the machine origin when that happens.
        if let Some(offset) = active_offset(&self.snapshot) {
            self.view.origin = [
                offset.get("X").copied().unwrap_or(0.0),
                offset.get("Y").copied().unwrap_or(0.0),
                offset.get("Z").copied().unwrap_or(0.0),
            ];
        }
        self.update_move_progress();
        // A touch-off or a DRO entry moves the origin the preview is drawn
        // against. The backends bump `offsets.revision` on a write, but the
        // first snapshots arrive before the offset rows do and carry revision 0,
        // so a preview parsed with no offset is rebuilt once rows appear.
        //
        // The rows themselves are compared as well, because the controller moves
        // them without any HMI write: the startup publication issues its own
        // `G10 L2`, and a program may too. A preview left parsed against the
        // zero row it saw before the publication does not just draw the part in
        // the wrong place - `live_trail_shift` then shifts the tool marker by the
        // whole offset, which is the "toolhead sits at machine home" report.
        let rows = offsets_signature(&self.snapshot.offsets.offsets);
        let offsets_changed = self.snapshot.offsets.revision != self.program.preview_offsets_seen
            || rows != self.program.preview_offsets_rows;
        let offset_now_available =
            self.program.preview_offset.is_none() && !self.snapshot.offsets.offsets.is_empty();
        if offsets_changed || offset_now_available {
            self.refresh_preview();
        }
        // RAPID changes the jog ceiling, so the published speed follows it.
        // Compared against what we last sent, not against the snapshot: the
        // snapshot is rebuilt every poll, and comparing with it published on
        // every frame and starved the controller.
        let wanted = self.jog_speed_value();
        // A fault blocks new jog commands; keep the last published value
        // untouched so a slider change is published once the reset clears the
        // fault.
        if !self.snapshot.probe_trip_active()
            && self
                .jog
                .speed_published
                .map(|value| (value - wanted).abs() > 1e-9)
                .unwrap_or(true)
        {
            self.jog.speed_published = Some(wanted);
            self.send_quiet(linc_core::command::MachineCommand::SetHalOutput {
                output: linc_core::command::LincOutput::JogSpeed,
                value: linc_core::command::HalValue::Float(wanted * 60.0),
            });
        }
        // Tell the operator about controller errors wherever they are. An
        // aborted program used to leave the console as the only witness.
        let newest = self
            .snapshot
            .alarms
            .iter()
            .map(|alarm| alarm.id)
            .max()
            .unwrap_or(0);
        if self.chrome.alarm_id_seen == 0 {
            // First poll: the controller may already hold a backlog. Show it in
            // the message strip, but do not fire a toast for every entry.
            self.chrome.alarm_id_seen = newest;
        } else if newest > self.chrome.alarm_id_seen {
            let fresh: Vec<linc_core::model::Alarm> = self
                .snapshot
                .alarms
                .iter()
                .filter(|alarm| alarm.id > self.chrome.alarm_id_seen)
                .cloned()
                .collect();
            for alarm in fresh {
                let colour = match alarm.severity {
                    linc_core::model::AlarmSeverity::Critical
                    | linc_core::model::AlarmSeverity::Error => theme::ERROR,
                    linc_core::model::AlarmSeverity::Warning => theme::WARNING,
                    linc_core::model::AlarmSeverity::Info => theme::MUTED,
                };
                // An error that stopped the program is worth more than the five
                // seconds a status toast lives: the operator is still reading
                // the console when it appears, and a message that is gone by the
                // time they look reads as one that never arrived.
                let life = if alarm.severity.is_alarm()
                    && !matches!(alarm.severity, linc_core::model::AlarmSeverity::Warning)
                {
                    ALARM_TOAST_SECONDS
                } else {
                    TOAST_SECONDS
                };
                self.toast_for(alarm.message, colour, life);
            }
            self.chrome.alarm_id_seen = newest;
        }
        let mut loaded = false;
        let mut load_failed = false;
        self.chrome.last_snapshot_ms = now_ms();
        let events = self.backend.drain_events();
        for event in events {
            match event {
                BackendEvent::Command(CommandResult {
                    command, result, ..
                }) => match result {
                    Ok(()) => {
                        if command == "Load program" {
                            loaded = true;
                        }
                        self.log_ui(
                            AlarmSeverity::Info,
                            AlarmSource::Ui,
                            "UI-OK",
                            format!("{command} accepted"),
                        );
                    }
                    Err(error) => {
                        if command == "Load program" {
                            load_failed = true;
                        }
                        self.toast(error.to_string(), theme::ERROR);
                        self.log_ui(
                            AlarmSeverity::Error,
                            AlarmSource::Ui,
                            "UI-ERR",
                            format!("{command}: {error}"),
                        );
                    }
                },
                BackendEvent::Log(alarm) => self.diagnostics.log.push_front(alarm),
                BackendEvent::SnapshotLost => {
                    self.toast("status snapshot lost", theme::WARNING);
                }
            }
        }
        while self.diagnostics.log.len() > 1000 {
            self.diagnostics.log.pop_back();
        }
        if self.program.pending_load {
            if loaded {
                self.commit_load();
            } else if load_failed {
                // Stay in Files so the operator sees the error next to the file.
                self.program.pending_load = false;
            }
        }
    }

    pub fn log_ui(
        &mut self,
        severity: AlarmSeverity,
        source: AlarmSource,
        code: &str,
        message: impl Into<String>,
    ) {
        let id = self
            .diagnostics
            .log
            .front()
            .map(|alarm| alarm.id + 1)
            .unwrap_or(1);
        let mut alarm = Alarm::new(id, severity, source, code, message);
        alarm.timestamp_ms = now_ms();
        self.diagnostics.log.push_front(alarm);
    }

    pub fn toast(&mut self, text: impl Into<String>, color: Color32) {
        self.toast_for(text, color, TOAST_SECONDS);
    }

    /// A toast that stays up for `seconds`: errors live longer than status
    /// notes, so a stopped program's reason is still readable after the
    /// operator has looked away from the console.
    pub fn toast_for(&mut self, text: impl Into<String>, color: Color32, seconds: u64) {
        self.overlay.toasts.push(toast_for(text, color, seconds));
        if self.overlay.toasts.len() > 6 {
            self.overlay.toasts.remove(0);
        }
    }
}

#[cfg(test)]
mod move_progress_tests {
    use super::*;

    #[test]
    pub(super) fn a_move_fills_the_bar_from_empty_to_full() {
        let (fraction, total) = move_progress(-100.0, 0.0);
        assert_eq!(fraction, 0.0);
        assert_eq!(total, 100.0);

        let (fraction, total) = move_progress(-50.0, total);
        assert!((fraction - 0.5).abs() < 1e-9, "{fraction}");

        let (fraction, total) = move_progress(-0.0, total);
        assert_eq!(fraction, 1.0);
        assert_eq!(total, 0.0);
    }

    #[test]
    pub(super) fn the_sign_of_the_remaining_distance_is_ignored() {
        let (plus, _) = move_progress(100.0, 0.0);
        let (minus, _) = move_progress(-100.0, 0.0);
        assert_eq!(plus, 0.0);
        assert_eq!(minus, 0.0);

        let (plus, _) = move_progress(40.0, 100.0);
        let (minus, _) = move_progress(-40.0, 100.0);
        assert!((plus - 0.6).abs() < 1e-9, "{plus}");
        assert_eq!(plus, minus);
    }

    #[test]
    pub(super) fn a_longer_move_resets_the_total_instead_of_going_negative() {
        let (fraction, total) = move_progress(-200.0, 50.0);
        assert_eq!(fraction, 0.0);
        assert_eq!(total, 200.0);
    }

    #[test]
    pub(super) fn an_idle_axis_shows_a_full_bar() {
        let (fraction, total) = move_progress(0.0, 0.0);
        assert_eq!(fraction, 1.0);
        assert_eq!(total, 0.0);
    }
}
