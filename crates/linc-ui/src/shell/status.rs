//! Pure readouts: the strings and colours the bars paint. Nothing here mutates
//! the app, which is what lets these be tested without a window.

use super::*;

/// Exponential moving average of the frame rate, biased so the first samples
/// settle quickly without flickering.
pub fn fps_step(previous: f32, dt: f32) -> f32 {
    let dt = dt.clamp(1.0 / 240.0, 1.0);
    let sample = 1.0 / dt;
    if previous <= 0.0 {
        sample
    } else {
        previous * 0.9 + sample * 0.1
    }
}

/// The bottom bar's machine field.
///
/// A machine that is off is a fault, not a quiet state: it is painted in the
/// error colour and the bar blinks it through [`blink_fault_color`], so a bar
/// full of green does not hide it. Stale data claims neither, and asks.
pub(super) fn machine_status_field(on: bool, stale: bool) -> (String, egui::Color32) {
    if stale {
        ("MACHINE ?".to_string(), theme::WARNING)
    } else if on {
        ("MACHINE ON".to_string(), theme::SUCCESS)
    } else {
        ("MACHINE OFF".to_string(), theme::ERROR)
    }
}

/// Whether the probe field belongs on the bar: a probe is configured **and**
/// the probe tool is the one in the spindle.
///
/// A configured probe only says the profile has one; the machine is another
/// matter, and the bar's right-hand group implies a fitted probe. A tool change
/// that pulls the probe out (or fits a cutter) takes the field away entirely,
/// so nothing dims and nothing shifts.
pub(super) fn probe_fitted(configured: bool, probe_tool: u16, current_tool: u16) -> bool {
    configured && probe_tool == current_tool
}

/// The bottom bar's probe field: the state of the wired probe input.
///
/// `Some(_)` becomes READY or TRIPPED and `None` asks, the way the machine
/// field does. Stale data asks too: the input may have moved since the last
/// poll, and claiming READY while the link is down is the one answer that
/// costs a broken probe. MUTED for the unknown case: it is not a fault and not
/// a good state either.
pub(super) fn probe_input_field(
    input_tripped: Option<bool>,
    stale: bool,
) -> (String, egui::Color32) {
    match (stale, input_tripped) {
        (false, Some(false)) => ("PROBE READY".to_string(), theme::SUCCESS),
        (false, Some(true)) => ("PROBE TRIPPED".to_string(), theme::WARNING),
        _ => ("PROBE ?".to_string(), theme::MUTED),
    }
}

/// True on the bright half of the bottom bar's fault blink.
///
/// Two alternations a second: half a second bright, half a second dim. The
/// text is painted on both halves - only the colour alternates - so a capture
/// or a test always finds it.
pub(super) fn blink_is_bright(time: f64) -> bool {
    ((time * 2.0) as i64).rem_euclid(2) == 0
}

/// Alternating fault colour for the bottom bar, requesting the next half.
pub(super) fn blink_fault_color(ui: &egui::Ui) -> egui::Color32 {
    ui.ctx()
        .request_repaint_after(std::time::Duration::from_millis(250));
    if blink_is_bright(ui.input(|input| input.time)) {
        theme::ERROR
    } else {
        theme::ERROR.gamma_multiply(0.45)
    }
}

/// Blinking red in the status line: the limit switches are being ignored.
///
/// The jog screens have no override button any more, so this is the only sign
/// that the machine is free to drive through a switch. The text is painted on
/// both halves of the blink - only the colour alternates - so a capture or a
/// test always finds it.
pub fn limits_override_indicator(ui: &mut egui::Ui) {
    ui.label(
        RichText::new("LIMITS OVERRIDE ON")
            .monospace()
            .size(theme::font_size(11.0))
            .color(blink_fault_color(ui)),
    );
}

/// Bottom-bar override readout.
///
/// The order is the physical panel's — FEED, RAPID, SPINDLE — so the numbers
/// under the F, R and S keys read in the order the operator's hand finds them.
pub fn override_readout(feed: f64, rapid: f64, spindle: f64) -> String {
    format!(
        "OVR F{:.0} R{:.0} S{:.0}",
        feed * 100.0,
        rapid * 100.0,
        spindle * 100.0
    )
}

pub fn active_offset(snapshot: &MachineSnapshot) -> Option<&linc_core::model::AxisMap<f64>> {
    let index = snapshot.offsets.active_index?;
    snapshot
        .offsets
        .offsets
        .iter()
        .find(|offset| offset.index == index)
        .map(|offset| &offset.values)
}

pub fn machine_bounds(profile: &Profile) -> Option<Bounds3> {
    let mut bounds = Bounds3 {
        min: [f64::INFINITY; 3],
        max: [f64::NEG_INFINITY; 3],
    };
    let mut any = false;
    for axis in &profile.machine_ini.axes {
        let index = match axis.name.as_str() {
            "X" => 0,
            "Y" => 1,
            "Z" => 2,
            _ => continue,
        };
        bounds.min[index] = axis.min_limit;
        bounds.max[index] = axis.max_limit;
        any = true;
    }
    if !any {
        return None;
    }
    for index in 0..3 {
        if !bounds.min[index].is_finite() {
            bounds.min[index] = 0.0;
            bounds.max[index] = 0.0;
        }
    }
    Some(bounds)
}

/// The program state word for the top bar.
///
/// Paused is the state that needs attention - the machine is waiting with a
/// line in hand - so it is the only one that earns a warning colour. Idle and
/// running are both normal and stay quiet.
pub(super) fn program_state_word(program: &ProgramState) -> (&'static str, Color32) {
    if program.paused {
        ("PAUSED", theme::WARNING)
    } else if program.running {
        ("RUNNING", theme::SUCCESS)
    } else {
        ("IDLE", theme::MUTED)
    }
}

/// Homing progress for the status line.
///
/// Moves fail closed until every axis is homed, so a partly homed machine is a
/// warning that names the axes it already trusts; the fully homed case is the
/// normal one and stays muted.
pub(super) fn homing_field(homed: &AxisMap<bool>) -> Option<(String, Color32)> {
    if homed.is_empty() {
        return None;
    }
    let total = homed.len();
    let done: Vec<&str> = homed
        .iter()
        .filter(|(_, is_homed)| **is_homed)
        .map(|(axis, _)| axis)
        .collect();
    if done.len() == total {
        Some(("HOMED".into(), theme::MUTED))
    } else if done.is_empty() {
        Some(("NOT HOMED".into(), theme::WARNING))
    } else {
        Some((format!("HOMED {}", done.join(" ")), theme::WARNING))
    }
}

/// The axis with the most left to go, for the status line's DTG field.
///
/// A settled machine has no field; during a move or a jog the largest distance
/// wins, which is the axis the operator is watching.
pub(super) fn moving_axis(axes: &AxisMap<PositionSet>) -> Option<(&str, f64)> {
    axes.iter()
        .filter(|(_, position)| position.dtg.abs() > 0.001)
        .max_by(|a, b| a.1.dtg.abs().total_cmp(&b.1.dtg.abs()))
        .map(|(axis, position)| (axis, position.dtg))
}

/// The field for a manual tool change that is waiting on the operator.
pub(super) fn tool_change_field(tool_change: &ToolChangeState) -> Option<(String, Color32)> {
    if tool_change.status != ToolChangeStatus::ManualPrompt {
        return None;
    }
    let target = tool_change.target_tool.unwrap_or(tool_change.current_tool);
    Some((format!("TC T{target} PENDING"), theme::WARNING))
}

pub fn state_label(power: MachinePower) -> &'static str {
    power.label()
}

impl LincApp {
    pub fn is_sim(&self) -> bool {
        self.profile.is_sim() || self.snapshot.simulated
    }

    pub fn active_offset(&self) -> Option<&linc_core::model::AxisMap<f64>> {
        active_offset(&self.snapshot)
    }

    pub fn machine_bounds(&self) -> Option<Bounds3> {
        machine_bounds(&self.profile)
    }

    pub fn axes(&self) -> Vec<String> {
        self.profile.machine_ini.axis_names()
    }

    pub fn wcs_label(&self, index: Option<u8>) -> String {
        match index {
            Some(index) if (1..=9).contains(&index) => linc_core::model::wcs_name(index),
            _ => "--".into(),
        }
    }

    pub fn active_wcs_label(&self) -> String {
        self.wcs_label(self.snapshot.offsets.active_index)
    }

    /// Why "run from line" is unavailable, or `None` when it may run.
    ///
    /// A line number is the one program-start input the panel cannot supply,
    /// so the HMI issues it; everything else that guards a program start is
    /// still checked here rather than assumed.
    pub fn run_from_line_block(&self) -> Option<&'static str> {
        self.start_gate(PanelMode::Auto, true)
    }

    /// The MDI pane's "run the selected block" gate.
    ///
    /// Every check `run_from_line_block` makes except the AUTO-mode one and
    /// the loaded-program one: the pane starts a buffer block in MDI, and the
    /// issue path re-checks the mode before it sends anything.
    pub fn mdi_run_block(&self) -> Option<&'static str> {
        self.start_gate(PanelMode::Mdi, false)
    }

    /// MAIN shows the MDI pane while the panel sits in MDI.
    ///
    /// Keyed to the panel mode, not to page entry: returning to MAIN from
    /// another page while the panel still sits in MDI shows the pane again.
    pub fn mdi_pane_visible(&self) -> bool {
        self.nav.page == Page::Machine && self.snapshot.panel.mode.mode() == Some(PanelMode::Mdi)
    }

    /// Why the MDI buffer may not be written to disk, or `None` when it may.
    ///
    /// The same policy `save_program` applies, minus the EDIT-mode one: a save
    /// needs an idle interpreter (`safety.save_requires_idle`), a live panel
    /// and no program protect, not the editor.
    pub fn mdi_save_block(&self) -> Option<&'static str> {
        if self.program_protected() {
            return Some("PROTECT ON — SAVE BLOCKED");
        }
        if self.panel_offline() {
            return Some("PANEL OFFLINE — SAVE BLOCKED");
        }
        if self.snapshot.task.running
            || self.snapshot.task.interp != linc_core::model::InterpState::Idle
        {
            return Some("not idle — save blocked");
        }
        None
    }

    /// The worst and mean absolute following error for `axis` since the last
    /// program start.
    pub fn following_error_summary(&self, axis: &str) -> (f64, f64) {
        let samples = self.following.run_samples.get(axis).copied().unwrap_or(0);
        if samples == 0 {
            return (0.0, 0.0);
        }
        let worst = self.following.run_max.get(axis).copied().unwrap_or(0.0);
        let sum = self.following.run_abs_sum.get(axis).copied().unwrap_or(0.0);
        (worst, sum / samples as f64)
    }

    pub fn active_offset_values(&self) -> Option<&linc_core::model::AxisMap<f64>> {
        active_offset(&self.snapshot)
    }

    /// The tool tip in machine coordinates, matching the preview and the trail.
    pub fn tool_tip(&self) -> [f64; 3] {
        ["X", "Y", "Z"].map(|axis| {
            self.snapshot
                .positions
                .axes
                .get(axis)
                .map(|position| position.machine)
                .unwrap_or(0.0)
        })
    }

    /// The log the operator sees: the controller's alarms plus the HMI's own
    /// notices, newest first. Diagnostics and the status bars share this list
    /// so one of them cannot report messages the other shows as zero.
    pub fn logs(&self) -> Vec<Alarm> {
        let mut logs: Vec<Alarm> = self.snapshot.alarms.iter().cloned().collect();
        logs.extend(self.diagnostics.log.iter().cloned());
        logs.sort_by(|a, b| b.timestamp_ms.cmp(&a.timestamp_ms));
        logs
    }

    /// A program is loaded and not idle at the interpreter: running, paused, or
    /// waiting for a tool change. This is the state in which Esc resets the
    /// program and the sidebar offers the shortcut.
    pub fn program_active(&self) -> bool {
        self.snapshot.task.running
            || self.snapshot.program.paused
            // `Unknown` before the first status poll is not a running program,
            // so require a loaded program for the interpreter-state test.
            || (self.snapshot.program.line_count > 0
                && self.snapshot.task.interp != linc_core::model::InterpState::Idle)
    }

    /// Rows for the SERVO LOAD pane, in the order `[ui] servo_load` lists them.
    /// Entries whose axis or spindle is absent are skipped, so a shared profile
    /// can name an axis the machine does not have.
    pub fn servo_load_rows(&self) -> Vec<(String, f64)> {
        let mut rows = Vec::new();
        for entry in &self.profile.config.ui.servo_load {
            let label = entry.trim().to_ascii_uppercase();
            if label == "S" {
                if let Some(load) = self
                    .snapshot
                    .spindles
                    .first()
                    .and_then(|spindle| spindle.load)
                {
                    rows.push((label, load));
                }
            } else if let Some(load) = self.snapshot.machine.servo_load.get(&label) {
                rows.push((label, *load));
            }
        }
        rows
    }

    /// The newest `limit` entries of [`Self::logs`], for cards that show a few
    /// rows every frame: bounded work per frame instead of cloning the whole
    /// log. Each source is already newest-first, so only `limit` entries of
    /// each are copied before the merge.
    pub fn recent_logs(&self, limit: usize) -> Vec<Alarm> {
        let mut logs: Vec<Alarm> = self.diagnostics.log.iter().take(limit).cloned().collect();
        logs.extend(self.snapshot.alarms.iter().take(limit).cloned());
        logs.sort_by(|a, b| b.timestamp_ms.cmp(&a.timestamp_ms));
        logs.truncate(limit);
        logs
    }

    /// `(messages, alarms)` over [`Self::logs`], split once by
    /// `AlarmSeverity::is_alarm`: a warning is an alarm in the top bar and in
    /// the diagnostics table alike.
    pub fn log_counts(&self) -> (usize, usize) {
        let mut alarms = 0;
        let mut messages = 0;
        for entry in self
            .snapshot
            .alarms
            .iter()
            .chain(self.diagnostics.log.iter())
        {
            if entry.severity.is_alarm() {
                alarms += 1;
            } else {
                messages += 1;
            }
        }
        (messages, alarms)
    }

    /// The live fields the bottom bar shows on its left, in reading order:
    /// program progress, homing, limits, a pending manual tool change and the
    /// axis with the most left to go. Empty entries are omitted rather than
    /// drawn as placeholders, so a quiet machine gives a quiet row.
    pub(super) fn status_fields(&self) -> Vec<(String, Color32)> {
        let mut fields: Vec<(String, Color32)> = Vec::new();
        let program = &self.snapshot.program;
        if program.line_count > 0 {
            let line = program.executing_line().max(1);
            let percent = (program.progress.clamp(0.0, 1.0) * 100.0).round() as i32;
            fields.push((
                format!("LN {line}/{} {percent}%", program.line_count),
                theme::MUTED,
            ));
        }
        if let Some(field) = homing_field(&self.snapshot.machine.homed) {
            fields.push(field);
        }
        // A held limit switch is never quiet: it blocks motion and says why.
        if !self.snapshot.machine.limits.is_empty() {
            fields.push((
                format!("LIMIT {}", self.snapshot.machine.limits.join(" ")),
                theme::ERROR,
            ));
        }
        if let Some(field) = tool_change_field(&self.snapshot.tool_change) {
            fields.push(field);
        }
        if !self.shows_coordinate_status() {
            if let Some((axis, dtg)) = moving_axis(&self.snapshot.positions.axes) {
                fields.push((format!("DTG {axis} {dtg:+.3}"), theme::MUTED));
            }
        }
        fields
    }

    pub(super) fn program_label(&self) -> String {
        let name = self
            .snapshot
            .program
            .display_name
            .rsplit('/')
            .next()
            .unwrap_or("");
        if name.is_empty() {
            "--".into()
        } else {
            name.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    pub(super) fn the_program_state_word_reads_the_controller() {
        let mut program = ProgramState::default();
        assert_eq!(program_state_word(&program).0, "IDLE");
        program.running = true;
        assert_eq!(program_state_word(&program).0, "RUNNING");
        program.paused = true;
        assert_eq!(program_state_word(&program).0, "PAUSED");
    }

    #[test]
    pub(super) fn homing_names_the_axes_it_cannot_trust() {
        let mut homed = AxisMap::new();
        assert!(homing_field(&homed).is_none(), "no axes, no field");
        homed.insert("X", false);
        homed.insert("Y", false);
        assert_eq!(homing_field(&homed).unwrap().0, "NOT HOMED");
        homed.insert("X", true);
        assert_eq!(homing_field(&homed).unwrap().0, "HOMED X");
        homed.insert("Y", true);
        assert_eq!(homing_field(&homed).unwrap().0, "HOMED");
    }

    #[test]
    pub(super) fn the_status_line_names_the_axis_that_is_moving() {
        let mut axes = AxisMap::new();
        axes.insert(
            "X",
            PositionSet {
                dtg: 0.0,
                ..PositionSet::default()
            },
        );
        axes.insert(
            "Y",
            PositionSet {
                dtg: 0.0,
                ..PositionSet::default()
            },
        );
        assert!(moving_axis(&axes).is_none(), "a settled machine is quiet");
        axes.insert(
            "Y",
            PositionSet {
                dtg: -12.34,
                ..PositionSet::default()
            },
        );
        assert_eq!(moving_axis(&axes), Some(("Y", -12.34)));
        axes.insert(
            "X",
            PositionSet {
                dtg: 40.0,
                ..PositionSet::default()
            },
        );
        assert_eq!(moving_axis(&axes).map(|(axis, _)| axis), Some("X"));
    }

    #[test]
    pub(super) fn a_pending_tool_change_is_named() {
        let mut tool_change = ToolChangeState::default();
        assert!(tool_change_field(&tool_change).is_none());
        tool_change.status = ToolChangeStatus::ManualPrompt;
        tool_change.target_tool = Some(7);
        assert_eq!(tool_change_field(&tool_change).unwrap().0, "TC T7 PENDING");
        // Without a target the current tool is the best name available.
        tool_change.target_tool = None;
        tool_change.current_tool = 3;
        assert_eq!(tool_change_field(&tool_change).unwrap().0, "TC T3 PENDING");
    }
}
