//! Everything that reaches the backend: sending a command, draining events,
//! writing to the UI log, and the move-progress arithmetic the bars read.

use super::*;

/// How far through its move an axis is, from the controller's distance to go.
///
/// The status carries only the remainder — the distance still to go, signed by
/// the direction of travel — so the length of the move is learned from the
/// largest remainder seen for it. That length belongs to one move only. A
/// program runs block after block, and a shorter block measured against the
/// previous, longer one started its bar part filled, from a scale that no longer
/// meant anything (measured on LinuxCNC with a G64 program: 100 % down to 51 %,
/// 25 %, 0 % at block starts). A remainder that grows beyond the blend overshoot
/// is therefore a new move, and the bar starts from empty again.
#[derive(Clone, Copy, Debug, Default)]
pub struct MoveProgress {
    /// Length of the move in flight: the largest remainder seen for it.
    length: f64,
    /// The remainder at the last observation, sign discarded.
    remaining: f64,
}

impl MoveProgress {
    /// Below this the axis has arrived and the move is over.
    const AT_REST: f64 = 1e-6;
    /// Growth of the remainder that is more than a blend overshoot. The planner
    /// can carry a little past a block's endpoint before the next block is
    /// reported, and that is still the same move.
    const NEW_MOVE: f64 = 1e-3;

    /// Feed one observation of the controller's distance to go, either sign, and
    /// return the fraction of the move already travelled.
    pub fn observe(&mut self, dtg: f64) -> f64 {
        let remaining = dtg.abs();
        if remaining <= Self::AT_REST {
            // Nothing left to travel: the move is complete, and the next one
            // brings its own length.
            *self = Self::default();
            return 1.0;
        }
        if remaining > self.remaining + Self::NEW_MOVE {
            self.length = remaining;
        }
        self.length = self.length.max(remaining);
        self.remaining = remaining;
        (1.0 - remaining / self.length).clamp(0.0, 1.0)
    }

    /// Fraction of the move already travelled, as last observed.
    pub fn fraction(&self) -> f64 {
        if self.length <= 0.0 {
            // Standing still: the bar sits full.
            1.0
        } else {
            (1.0 - self.remaining / self.length).clamp(0.0, 1.0)
        }
    }
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

    /// Feed each axis's remaining distance to its move bar.
    pub(super) fn update_move_progress(&mut self) {
        let mut updates = Vec::new();
        for (axis, position) in self.snapshot.positions.axes.iter() {
            updates.push((axis.to_string(), position.dtg));
        }
        for (axis, dtg) in updates {
            if self.view.move_progress.get(axis.as_str()).is_none() {
                self.view
                    .move_progress
                    .insert(axis.clone(), MoveProgress::default());
            }
            if let Some(progress) = self.view.move_progress.get_mut(axis.as_str()) {
                progress.observe(dtg);
            }
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
                        // The recovery dialog is async: the backend's refusal
                        // arrives here, not from `send`. Put it on the dialog,
                        // where the operator is looking, and end the attempt so
                        // the next frame does not re-issue the same step.
                        if matches!(command.as_str(), "Recover limit" | "Arm limits override") {
                            if let Some(Modal::LimitRecovery(dialog)) = &mut self.overlay.modal {
                                dialog.fail(error.to_string().to_ascii_uppercase());
                            }
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
        let mut progress = MoveProgress::default();
        assert_eq!(progress.observe(-100.0), 0.0);
        assert!((progress.observe(-50.0) - 0.5).abs() < 1e-9);
        assert_eq!(progress.observe(-0.0), 1.0);
    }

    #[test]
    pub(super) fn the_sign_of_the_remaining_distance_is_ignored() {
        let mut plus = MoveProgress::default();
        let mut minus = MoveProgress::default();
        for step in [100.0, 40.0, 5.0] {
            assert_eq!(plus.observe(step), minus.observe(-step));
        }
    }

    /// A program runs block after block. The length of one block must not become
    /// the scale of the next: a shorter block then started part filled, and the
    /// bar read as full and draining (measured: 100 % -> 51 %, 25 %, 0 %).
    #[test]
    pub(super) fn a_shorter_block_starts_empty_instead_of_part_filled() {
        let mut progress = MoveProgress::default();
        assert_eq!(progress.observe(-200.0), 0.0);
        assert_eq!(progress.observe(-0.0), 1.0);
        // The next block is a tenth of the last one and goes the other way.
        assert_eq!(progress.observe(20.0), 0.0);
        assert!((progress.observe(10.0) - 0.5).abs() < 1e-9);
    }

    #[test]
    pub(super) fn a_longer_block_starts_empty_as_well() {
        let mut progress = MoveProgress::default();
        assert_eq!(progress.observe(50.0), 0.0);
        assert_eq!(progress.observe(-200.0), 0.0);
        assert!((progress.observe(-100.0) - 0.5).abs() < 1e-9);
    }

    /// The planner can carry a few microns past a block's endpoint before the
    /// next block is reported; that is the same move, not a fresh one.
    #[test]
    pub(super) fn a_blend_overshoot_keeps_the_move() {
        let mut progress = MoveProgress::default();
        assert_eq!(progress.observe(-20.0), 0.0);
        let late = progress.observe(-0.0004);
        assert!(late > 0.99, "{late}");
        let overshoot = progress.observe(0.0005);
        assert!(overshoot > 0.99, "still the same move: {overshoot}");
    }

    /// The measured shape of a running program: blocks of different lengths and
    /// directions, each remainder shrinking to nothing (blended, so the planner
    /// carries a few microns past the endpoint). The bar sweeps once per block
    /// and never runs backwards.
    #[test]
    pub(super) fn a_program_sweeps_once_per_block_and_never_backwards() {
        let mut progress = MoveProgress::default();
        for length in [-20.0_f64, 10.0, -30.0, 15.0] {
            assert_eq!(progress.observe(length), 0.0, "block {length} starts empty");
            let mut last = 0.0;
            for step in 1..=20 {
                let remaining = length * (1.0 - step as f64 / 20.0);
                let fraction = progress.observe(remaining);
                assert!(
                    fraction >= last - 1e-9,
                    "block {length}: {fraction} after {last}"
                );
                last = fraction;
            }
            assert_eq!(progress.observe(0.0), 1.0, "block {length} arrives full");
        }
    }

    #[test]
    pub(super) fn an_idle_axis_shows_a_full_bar() {
        assert_eq!(MoveProgress::default().fraction(), 1.0);
        let mut progress = MoveProgress::default();
        assert_eq!(progress.observe(0.0), 1.0);
        assert_eq!(progress.fraction(), 1.0);
    }
}
