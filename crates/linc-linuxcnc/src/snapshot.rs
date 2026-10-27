//! Build the typed `MachineSnapshot` from an NML status poll and HAL reads.

use std::collections::{BTreeSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use linuxcnc_nml::{
    ExecState, MessageKind, Status, TaskMode as NmlTaskMode, TaskState as NmlTaskState,
};

use linc_core::config::Profile;
use linc_core::faults::ProbeTripState;
use linc_core::model::*;
use linc_core::probe::{
    direct_written_axes, ProbeCategory, ProbeMode, ProbeOperation, ProbeParameters,
};
use linc_core::tooltable::ToolTable;

use crate::panel::PanelState;
use crate::parameters::ParameterFile;

const MAX_TRAIL_POINTS: usize = 200_000;
const MAX_PROBE_HISTORY: usize = 32;

/// Difference accepted between an intended and a live work offset.
const OFFSET_TOLERANCE: f64 = 1e-4;

/// The active work offset row as it stood when the MDI block was issued.
///
/// DIRECT routines write `G10 L2` into that row themselves, so a poll taken
/// after the run no longer describes the origin the result was computed from.
/// Provenance is therefore captured in the dispatch path, before the block
/// reaches the controller, and the commit uses it for POSITION and DIRECT
/// alike.
#[derive(Clone, Debug, Default)]
pub(crate) struct ProbeProvenance {
    pub index: Option<u8>,
    pub wcs: String,
    pub source_offsets: AxisMap<f64>,
    pub rotation: f64,
}

impl ProbeProvenance {
    pub(crate) fn from_snapshot(snapshot: &MachineSnapshot) -> Self {
        let index = snapshot
            .offsets
            .active_index
            .filter(|index| (1..=9).contains(index));
        let row = index.and_then(|index| {
            snapshot
                .offsets
                .offsets
                .iter()
                .find(|offset| offset.index == index)
        });
        Self {
            index,
            wcs: row
                .map(|row| wcs_name(row.index))
                .unwrap_or_else(|| "--".into()),
            source_offsets: row.map(|row| row.values.clone()).unwrap_or_default(),
            rotation: row.and_then(|row| row.rotation).unwrap_or(0.0),
        }
    }
}

/// Probe Basic calibration writes `#3032` twice: an initial clear to zero
/// before the measurements and the AVG/X/Y result after them. Only a message
/// that arrives after every measurement slot is the controller's result, so
/// the initial clear can never satisfy completion.
#[derive(Clone, Debug, Default)]
struct CalibrationTrack {
    messages: u32,
    /// Last `#3032` seen before all measurement slots had arrived.
    preliminary: Option<f64>,
    /// Last `#3032` seen after all measurement slots had arrived.
    result: Option<f64>,
}

impl CalibrationTrack {
    fn record(&mut self, value: f64, complete: bool) {
        self.messages += 1;
        if complete {
            self.result = Some(value);
        } else {
            self.preliminary = Some(value);
        }
    }
}

struct LiveProbeTransaction {
    operation: &'static ProbeOperation,
    parameters: ProbeParameters,
    provenance: ProbeProvenance,
    values: [f64; 12],
    updated: BTreeSet<u16>,
    seen_active: bool,
    failed: bool,
    calibration: CalibrationTrack,
}

impl LiveProbeTransaction {
    fn new(
        operation: &'static ProbeOperation,
        parameters: ProbeParameters,
        provenance: ProbeProvenance,
    ) -> Self {
        Self {
            operation,
            parameters,
            provenance,
            values: [0.0; 12],
            updated: BTreeSet::new(),
            seen_active: false,
            failed: false,
            calibration: CalibrationTrack::default(),
        }
    }

    fn measurements_complete(&self) -> bool {
        self.operation
            .result_parameters()
            .iter()
            .all(|parameter| self.updated.contains(parameter))
    }
}

pub struct SnapshotBuilder {
    pub(crate) profile: Profile,
    pub(crate) tool_table: ToolTable,
    tool_table_checked: Instant,
    program_text: Option<Arc<String>>,
    program_path: Option<PathBuf>,
    program_mtime: Option<std::time::SystemTime>,
    program_revision: u64,
    program_loaded_at_ms: u64,
    live_trail: Vec<TrailPoint>,
    last_tool_tip: Option<[f64; 3]>,
    next_alarm_id: u64,
    last_alarm_texts: VecDeque<String>,
    pub(crate) alarms: VecDeque<Alarm>,
    probe_trip: ProbeTripState,
    probe_transaction: Option<LiveProbeTransaction>,
    probe_last_result: Option<ProbeResult>,
    probe_history: VecDeque<ProbeResult>,
    probe_error: Option<String>,
    warned_axis_mismatch: bool,
    tool_change_serial: u64,
    tool_change_active: bool,
    tool_confirm: bool,
    calibration_offset: Option<f64>,
    offsets_last: Option<Vec<WorkOffset>>,
    offsets_revision: u64,
}

impl SnapshotBuilder {
    pub fn new(profile: Profile, tool_table: ToolTable) -> Self {
        Self {
            profile,
            tool_table,
            tool_table_checked: Instant::now() - Duration::from_secs(2),
            program_text: None,
            program_path: None,
            program_mtime: None,
            program_revision: 1,
            program_loaded_at_ms: now_ms(),
            live_trail: Vec::new(),
            last_tool_tip: None,
            next_alarm_id: 1,
            last_alarm_texts: VecDeque::new(),
            alarms: VecDeque::new(),
            probe_trip: ProbeTripState::new(),
            probe_transaction: None,
            probe_last_result: None,
            probe_history: VecDeque::new(),
            probe_error: None,
            warned_axis_mismatch: false,
            tool_change_serial: 0,
            tool_change_active: false,
            tool_confirm: false,
            calibration_offset: None,
            offsets_last: None,
            offsets_revision: 1,
        }
    }

    /// Start tracking the transaction a probe MDI block describes.
    ///
    /// `provenance` is the active work offset row read *before* the block was
    /// issued; DIRECT routines rewrite that row themselves.
    pub(crate) fn note_probe_block(&mut self, block: Option<String>, provenance: ProbeProvenance) {
        let Some(block) = block else {
            return;
        };
        let operation = probe_operation(&block);
        let reset = block.to_ascii_lowercase().contains("reset");
        if reset {
            self.probe_transaction = None;
            self.probe_last_result = None;
            self.probe_error = None;
        }
        let Some(operation) = operation else {
            // Parameter-only updates do not invalidate the previous result or
            // start collecting a new one.
            return;
        };

        self.probe_last_result = None;
        self.probe_error = None;
        self.probe_transaction = None;
        if operation.id == "cal_reset" {
            // Its #3032 EVAL is handled by push_probe_value. There are no
            // #1001..#1012 results to collect for a calibration reset.
            return;
        }
        let parameters = ProbeParameters::from_mdi(&block).unwrap_or_default();
        self.probe_transaction = Some(LiveProbeTransaction::new(operation, parameters, provenance));
    }

    pub(crate) fn push_probe_value(&mut self, parameter: u32, value: f64) {
        if !value.is_finite() {
            return;
        }
        match parameter {
            3032 => {
                // Only the controller's final write is the calibration
                // result. An intermediate one (the routine's initial clear to
                // zero) is recorded but never published as the offset, so the
                // displayed value stays the last confirmed one.
                let mut publish = true;
                if let Some(transaction) = self.probe_transaction.as_mut() {
                    let complete = transaction.measurements_complete();
                    transaction.calibration.record(value, complete);
                    publish = complete;
                }
                if publish {
                    self.calibration_offset = Some(value);
                }
            }
            1001..=1012 => {
                if let Some(transaction) = self.probe_transaction.as_mut() {
                    transaction.values[(parameter - 1001) as usize] = value;
                    transaction.updated.insert(parameter as u16);
                }
            }
            _ => {}
        }
    }

    pub(crate) fn fail_probe(&mut self) {
        if let Some(transaction) = self.probe_transaction.as_mut() {
            transaction.failed = true;
        }
    }

    pub fn confirm_tool_change(&mut self) {
        self.tool_confirm = true;
    }

    pub fn cancel_tool_confirm(&mut self) {
        self.tool_confirm = false;
    }

    /// Drop a refusal-to-claim message once its recovery has succeeded.
    pub(crate) fn clear_probe_error(&mut self) {
        self.probe_error = None;
    }

    /// Record that an explicit Apply put this exact result's values live, so a
    /// repeated Apply is refused instead of writing a second time.
    pub(crate) fn mark_probe_result_applied(&mut self, result: &ProbeResult) {
        let matches = |candidate: &ProbeResult| {
            candidate.timestamp_ms == result.timestamp_ms && candidate.operation == result.operation
        };
        if let Some(current) = self.probe_last_result.as_mut() {
            if matches(current) {
                current.applied = true;
            }
        }
        for entry in self.probe_history.iter_mut() {
            if matches(entry) {
                entry.applied = true;
            }
        }
    }

    pub fn clear_trail(&mut self) {
        let tip = self.last_tool_tip.unwrap_or([0.0; 3]);
        self.live_trail.clear();
        self.live_trail.push(TrailPoint::new(tip, false));
        self.last_tool_tip = Some(tip);
    }

    pub fn push_message(&mut self, kind: MessageKind, text: &str) {
        // The Messages page shows the controller's text verbatim, so trim the
        // transport's trailing newline but keep internal spacing.
        let message = text.trim().to_string();
        if message.is_empty() {
            return;
        }
        if kind.is_error() {
            // A probe trip is a latching operator fault, not just a log line.
            // Latch before the duplicate check so a repeated trip after a
            // clear raises the fault even when the text is still in the
            // dedupe window.
            if let Some(trip) = self.probe_trip.raise(&message, now_ms()) {
                log::error!("probe trip latched ({:?}): {}", trip, message);
            }
        }
        if self
            .last_alarm_texts
            .iter()
            .any(|previous| previous == &message)
        {
            return;
        }
        let severity = if kind.is_error() {
            AlarmSeverity::Error
        } else {
            AlarmSeverity::Info
        };
        self.alarms.push_front(Alarm::new(
            self.next_alarm_id,
            severity,
            AlarmSource::Nml,
            "NML",
            message.clone(),
        ));
        self.next_alarm_id += 1;
        self.last_alarm_texts.push_back(message);
        while self.last_alarm_texts.len() > 100 {
            self.last_alarm_texts.pop_front();
        }
        while self.alarms.len() > MAX_EVENT_LOG {
            self.alarms.pop_back();
        }
    }

    /// Clear the latched probe trip after the controller reset was accepted.
    ///
    /// The controller no longer reports the error once it is consumed, so
    /// nothing else can clear this: no poll, age or reconnect does. Forgetting
    /// the text in the dedupe window lets the next trip appear as a fresh
    /// Messages entry.
    pub fn clear_probe_trip(&mut self) {
        if let Some(fault) = self.probe_trip.clear() {
            self.last_alarm_texts.retain(|text| text != &fault.message);
        }
    }

    pub fn refresh_tool_table(&mut self) {
        if self.tool_table_checked.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.tool_table_checked = Instant::now();
        if let Ok(table) = ToolTable::load(
            &self.profile.machine_ini.tool_table,
            &self.profile.machine_ini.tool_table_columns,
            self.profile.changer,
        ) {
            self.tool_table = table;
        }
    }

    pub fn build(
        &mut self,
        status: &Status,
        panel: &PanelState,
        spindles: Vec<SpindleState>,
        parameters: &ParameterFile,
    ) -> MachineSnapshot {
        self.refresh_tool_table();
        let mut snapshot = initial_snapshot(&self.profile);
        snapshot.meta.captured_at_ms = now_ms();
        snapshot.meta.stale = false;

        snapshot.task.power = match status.task.state {
            NmlTaskState::Estop => MachinePower::Estop,
            NmlTaskState::EstopReset => MachinePower::EstopReset,
            NmlTaskState::Off => MachinePower::Off,
            NmlTaskState::On if status.motion.enabled => MachinePower::On,
            NmlTaskState::On => MachinePower::Off,
            NmlTaskState::Unknown(_) => MachinePower::Unknown,
        };
        snapshot.task.mode = match status.task.mode {
            NmlTaskMode::Auto => TaskMode::Auto,
            NmlTaskMode::Mdi => TaskMode::Mdi,
            NmlTaskMode::Manual => TaskMode::Manual,
            NmlTaskMode::Unknown(_) => TaskMode::Unknown,
        };
        snapshot.task.interp = match status.task.interp_state {
            linuxcnc_nml::InterpState::Idle => InterpState::Idle,
            linuxcnc_nml::InterpState::Reading => InterpState::Reading,
            linuxcnc_nml::InterpState::Paused => InterpState::Paused,
            linuxcnc_nml::InterpState::Waiting => InterpState::Waiting,
            linuxcnc_nml::InterpState::Unknown(_) => InterpState::Unknown,
        };
        snapshot.task.paused = status.task.paused;
        snapshot.task.running =
            status.task.mode == NmlTaskMode::Auto && status.task.exec_state != ExecState::Done;
        snapshot.task.program_started = snapshot.task.running;
        snapshot.task.motion_active = motion_active(status);

        snapshot.machine.on = status.motion.enabled;
        // A hardwired mushroom reaches the task status through the machine HAL;
        // a panel that reports its own bit adds to that unless the profile says
        // the bit is not evidence (see `panel.estop_from_panel`).
        let panel_estop = panel.estop && self.profile.config.panel.estop_from_panel;
        snapshot.machine.estop = status.io.estop || panel_estop;
        snapshot.machine.fault = status.io.fault || panel_estop;
        snapshot.machine.force_homing = self.profile.machine_ini.no_force_homing;
        snapshot.machine.coolant_flood = status.io.flood;
        snapshot.machine.coolant_mist = status.io.mist;
        snapshot.machine.lube = status.io.lube;
        snapshot.machine.lube_level_ok = status.io.lube_level;
        snapshot.machine.door_closed = panel.door_closed;
        snapshot.machine.tool_change_active = panel.tool_change_active;
        snapshot.machine.echo_serial = status.echo_serial_number.max(0) as u64;
        snapshot.machine.nml_command_connected = true;
        snapshot.machine.nml_status_connected = true;

        let axes = &self.profile.machine_ini.axes;
        for (index, axis) in axes.iter().enumerate() {
            // Pose components are indexed by axis letter (X=0, Y=1, Z=2, ...),
            // not by position in the INI's coordinate list: a lathe's list is
            // [X, Z], so using the list index read Z as Y and the Z DRO stayed
            // at 0 while the joint moved.
            let pose_index = axis_pose_index(&axis.name).unwrap_or(index);
            let joint = status.joints.get(index);
            snapshot.machine.homed.insert(
                axis.name.clone(),
                joint.map(|joint| joint.homed).unwrap_or(false),
            );
            snapshot.machine.homing.insert(
                axis.name.clone(),
                joint.map(|joint| joint.homing).unwrap_or(false),
            );
            snapshot.machine.following_error.insert(
                axis.name.clone(),
                joint.map(|joint| joint.ferror_current).unwrap_or(0.0),
            );
            snapshot.machine.servo_load.insert(axis.name.clone(), 0.0);
            snapshot.machine.commanded.insert(
                axis.name.clone(),
                pose_component(&status.positions.commanded, pose_index),
            );
            snapshot.machine.actual.insert(
                axis.name.clone(),
                pose_component(&status.positions.actual, pose_index),
            );
        }
        snapshot.machine.limits = limit_labels(status, axes);

        let g5x = status.offsets.g5x_offset;
        let g92 = status.offsets.g92_offset;
        let tool_offset = status.offsets.tool_offset;
        let dtg = status.positions.dtg;
        snapshot.positions.axes = AxisMap::new();
        for (index, axis) in axes.iter().enumerate() {
            let pose_index = axis_pose_index(&axis.name).unwrap_or(index);
            let machine = pose_component(&status.positions.actual, pose_index);
            let work = machine
                - pose_component(&g5x, pose_index)
                - pose_component(&g92, pose_index)
                - pose_component(&tool_offset, pose_index);
            snapshot.positions.axes.insert(
                axis.name.clone(),
                PositionSet {
                    work,
                    machine,
                    dtg: pose_component(&dtg, pose_index),
                    velocity: status
                        .joints
                        .get(index)
                        .map(|joint| joint.velocity)
                        .unwrap_or(0.0),
                },
            );
        }
        if !self.warned_axis_mismatch && !snapshot.positions.axes.is_empty() {
            let expected = self.profile.machine_ini.axis_names();
            let missing: Vec<&String> = expected
                .iter()
                .filter(|axis| snapshot.positions.axes.get(axis).is_none())
                .collect();
            if !missing.is_empty() {
                log::warn!(
                    "LinuxCNC reported positions for {:?} but the INI declares {expected:?}; \
                     check [TRAJ] COORDINATES. Missing: {missing:?}",
                    snapshot.positions.axes.keys().collect::<Vec<_>>()
                );
                self.warned_axis_mismatch = true;
            }
        }

        let active_index = (status.offsets.g5x_index as u8).clamp(1, 9);
        snapshot.offsets.active_index = Some(active_index);
        let axis_names = self.axis_names();
        snapshot.offsets.offsets = (1..=9)
            .map(|index| {
                let mut values = parameters.work_offset(index, &axis_names);
                let mut rotation = parameters.rotation(index);
                if index == active_index {
                    for (axis_index, axis) in axes.iter().enumerate() {
                        values.insert(
                            axis.name.clone(),
                            pose_component(&g5x, axis_pose_index(&axis.name).unwrap_or(axis_index)),
                        );
                    }
                    rotation = status.offsets.rotation_xy;
                }
                WorkOffset {
                    index,
                    values,
                    rotation: Some(rotation),
                }
            })
            .collect();
        snapshot.offsets.g92 = {
            let mut values = AxisMap::new();
            for (index, axis) in axes.iter().enumerate() {
                values.insert(
                    axis.name.clone(),
                    pose_component(&g92, axis_pose_index(&axis.name).unwrap_or(index)),
                );
            }
            values
        };
        if self.offsets_last.as_ref() != Some(&snapshot.offsets.offsets) {
            self.offsets_last = Some(snapshot.offsets.offsets.clone());
            self.offsets_revision += 1;
        }
        snapshot.offsets.revision = self.offsets_revision;

        snapshot.spindles = if spindles.is_empty() {
            initial_snapshot(&self.profile).spindles
        } else {
            spindles
        };

        snapshot.feed = FeedState {
            feed: feed_word(status, &self.profile.machine_ini.linear_units),
            feed_override: panel.feed_override,
            rapid_override: panel.rapid_override,
            spindle_override: panel.spindle_override,
            units: self.profile.machine_ini.linear_units.clone(),
            feed_mode: feed_mode(status),
            distance_mode: distance_mode(status),
            feed_override_enabled: status.motion.feed_override_enabled,
            spindle_override_enabled: panel.spindle_override_enabled,
            adaptive_feed: status.motion.adaptive_feed_enabled,
        };

        snapshot.limits_overridden = panel.override_limits;
        snapshot.panel = panel.to_snapshot(&self.profile, status.io.tool_in_spindle.max(0) as u16);
        snapshot.pendant = snapshot.panel.pendant.clone();

        snapshot.tool_table = self.tool_table.state();
        let current_tool = status.io.tool_in_spindle.max(0) as u16;
        let prepared = panel.next_tool;
        snapshot.tool_change.current_tool = current_tool;
        snapshot.tool_change.prepared_tool = prepared;
        snapshot.tool_change.target_tool = Some(prepared).filter(|tool| *tool > 0);
        if panel.tool_change_active && !self.tool_change_active {
            self.tool_change_serial += 1;
            self.tool_change_active = true;
        } else if !panel.tool_change_active {
            self.tool_change_active = false;
            self.tool_confirm = false;
        }
        if panel.tool_change_active {
            if self.profile.changer == ToolChangerKind::Manual
                && self.profile.config.tools.manual_confirm
            {
                snapshot.tool_change.status = ToolChangeStatus::ManualPrompt;
                snapshot.tool_change.confirm_available = true;
                snapshot.tool_change.token = Some(ToolChangeToken(
                    (self.tool_change_serial << 16) | prepared as u64,
                ));
                snapshot.tool_change.message =
                    format!("REMOVE T{current_tool} / INSTALL T{prepared}");
            } else {
                snapshot.tool_change.status = ToolChangeStatus::Issuing;
                snapshot.tool_change.message = format!("Changing to T{prepared}");
            }
            snapshot.tool_change.confirmed = self.tool_confirm;
            snapshot.hal.tool_change_confirm = self.tool_confirm;
        } else if current_tool != 0 {
            snapshot.tool_change.status = ToolChangeStatus::Idle;
        }

        snapshot.atc = atc_state(&self.profile, &snapshot.spindles);

        self.update_program(status, &mut snapshot);
        self.update_probe(status, parameters, &mut snapshot);
        snapshot.alarms = self.alarms.clone();
        snapshot.probe_trip = self.probe_trip.fault().cloned();

        let tip = ["X", "Y", "Z"].map(|axis| {
            snapshot
                .positions
                .axes
                .get(axis)
                .map(|position| position.machine)
                .unwrap_or(0.0)
        });
        if self
            .last_tool_tip
            .map(|last| distance(last, tip) > 1e-4)
            .unwrap_or(false)
            || self.live_trail.is_empty()
        {
            let rapid = trail_hop_is_rapid(status.motion.motion_type);
            self.live_trail.push(TrailPoint::new(tip, rapid));
            let max = self
                .profile
                .config
                .toolpath
                .trail_max_points
                .clamp(2, MAX_TRAIL_POINTS);
            while self.live_trail.len() > max {
                self.live_trail.remove(0);
            }
        }
        self.last_tool_tip = Some(tip);
        snapshot.tool_tip = Some(tip);
        snapshot.live_trail = Arc::new(self.live_trail.clone());
        snapshot
    }

    fn axis_names(&self) -> Vec<String> {
        self.profile
            .machine_ini
            .axes
            .iter()
            .map(|axis| axis.name.clone())
            .collect()
    }

    fn update_program(&mut self, status: &Status, snapshot: &mut MachineSnapshot) {
        let path = if status.task.file.trim().is_empty() {
            None
        } else {
            Some(PathBuf::from(status.task.file.trim()))
        };
        if path != self.program_path {
            self.program_path = path.clone();
            self.program_revision += 1;
            self.program_loaded_at_ms = now_ms();
            self.live_trail.clear();
            self.program_text = None;
            self.program_mtime = None;
        }
        let mtime = self
            .program_path
            .as_ref()
            .and_then(|path| std::fs::metadata(path).ok())
            .and_then(|metadata| metadata.modified().ok());
        if mtime != self.program_mtime {
            self.program_mtime = mtime;
            if let Some(path) = &self.program_path {
                match std::fs::read_to_string(path) {
                    Ok(text) => {
                        self.program_text = Some(Arc::new(text));
                        self.program_revision += 1;
                    }
                    Err(error) => {
                        log::warn!("cannot read loaded program {}: {error}", path.display());
                        self.program_text = None;
                    }
                }
            }
        }

        let current_line = status.task.current_line.max(0) as usize;
        let motion_line = status.task.motion_line.max(0) as usize;
        let executed = if motion_line > 0 {
            motion_line
        } else {
            current_line
        };
        let text = self.program_text.clone();
        let line_count = text.as_ref().map(|text| text.lines().count()).unwrap_or(0);
        snapshot.program = ProgramState {
            path: self.program_path.clone(),
            display_name: self
                .program_path
                .as_ref()
                .and_then(|path| path.file_name())
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
            revision: self.program_revision,
            content: text.clone(),
            line_count,
            current_line,
            motion_line,
            next_line: text
                .as_ref()
                .and_then(|text| text.lines().nth(executed).map(str::to_string)),
            current_block: text
                .as_ref()
                .and_then(|text| {
                    text.lines()
                        .nth(executed.saturating_sub(1))
                        .map(str::to_string)
                })
                .unwrap_or_default(),
            modal: modal_lines(&status.active_gcodes, &status.active_mcodes),
            running: snapshot.task.running,
            paused: snapshot.task.paused,
            dirty: false,
            syntax_error: None,
            progress: if line_count > 0 {
                executed as f32 / line_count as f32
            } else {
                0.0
            },
            loaded_at_ms: self.program_loaded_at_ms,
        };
    }

    fn update_probe(
        &mut self,
        status: &Status,
        parameters: &ParameterFile,
        snapshot: &mut MachineSnapshot,
    ) {
        if self.calibration_offset.is_none() && parameters.has(3032) {
            self.calibration_offset = Some(parameters.get(3032));
        }
        snapshot.probe.enabled = self.profile.config.probe.enabled;
        snapshot.probe.tool_number = self.profile.config.probe.tool_number;
        snapshot.probe.selected_wcs = snapshot
            .offsets
            .active_index
            .map(wcs_name)
            .unwrap_or_else(|| "--".into());
        snapshot.probe.calibration_diameter = Some(self.profile.config.probe.calibration_diameter);
        snapshot.probe.input_tripped = Some(status.motion.probe_val != 0);

        let idle = probe_is_idle(status);
        let finalize = if let Some(transaction) = self.probe_transaction.as_mut() {
            if !idle {
                transaction.seen_active = true;
                false
            } else if transaction.failed {
                // Unlike a quiet just-issued block, an NML error is terminal
                // evidence and must not leave the transaction pending.
                true
            } else {
                // An idle poll immediately after issuing MDI can precede the
                // queued command. Wait in that case, but accept a run that
                // started and finished between polls if its updates arrived.
                transaction.seen_active || !transaction.updated.is_empty()
            }
        } else {
            false
        };
        if finalize {
            let transaction = self
                .probe_transaction
                .take()
                .expect("finalized probe transaction exists");
            if transaction.failed {
                self.probe_error = Some(format!(
                    "{} failed in LinuxCNC; no result was committed",
                    transaction.operation.name
                ));
            } else {
                let missing = transaction
                    .operation
                    .result_parameters()
                    .into_iter()
                    .filter(|parameter| !transaction.updated.contains(parameter))
                    .collect::<Vec<_>>();
                // A calibration result exists only once the controller has
                // published its final `#3032` after the measurements. Without
                // it the run is unconfirmed, and no locally computed value is
                // substituted.
                let calibration_confirmed = transaction.operation.category
                    != ProbeCategory::Calibrate
                    || transaction.calibration.result.is_some();
                if !missing.is_empty() {
                    let missing = missing
                        .iter()
                        .map(|parameter| format!("#{parameter}"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.probe_error = Some(format!(
                        "{} ended without fresh live updates for {missing}; no result was committed",
                        transaction.operation.name
                    ));
                } else if !calibration_confirmed {
                    self.probe_error = Some(format!(
                        "{} ended without the controller's final #3032 calibration message; \
                         no calibration was committed",
                        transaction.operation.name
                    ));
                } else {
                    self.commit_probe_transaction(transaction, snapshot);
                }
            }
        }

        snapshot.probe.running = self.probe_transaction.is_some() || status.motion.probing;
        snapshot.probe.calibration_offset = self.calibration_offset;
        snapshot.probe.last_result = self.probe_last_result.clone();
        snapshot.probe.history = self.probe_history.clone();
        snapshot.probe.error = self.probe_error.clone();
        snapshot.probe.last_routine = self
            .probe_transaction
            .as_ref()
            .map(|transaction| transaction.operation.id.to_string())
            .or_else(|| {
                self.probe_last_result
                    .as_ref()
                    .map(|result| result.operation.clone())
            });
    }

    fn commit_probe_transaction(
        &mut self,
        transaction: LiveProbeTransaction,
        snapshot: &MachineSnapshot,
    ) {
        let operation = transaction.operation;
        let parameters = &transaction.parameters;
        let direct = parameters.probe_mode == ProbeMode::ApplyWcs;
        let results = linc_core::probe::ParameterResults::from_values(&transaction.values);
        let provenance = transaction.provenance;
        let rotation = (operation.category == ProbeCategory::EdgeAngle && parameters.wcs_rotation)
            .then_some(transaction.values[11]);
        let updated = transaction.updated.iter().copied().collect::<Vec<_>>();
        let mut result = ProbeResult::from_operation(
            operation,
            &results,
            provenance.source_offsets.clone(),
            provenance.wcs.clone(),
            if direct { "DIRECT WCO" } else { "POSITION" }.into(),
            updated,
            std::collections::BTreeMap::new(),
            rotation,
            // Direct-WCO is only claimed applied once the live row confirms
            // it; see the check below.
            false,
        );

        if operation.category == ProbeCategory::Calibrate {
            // The controller's own final `#3032` is the calibrated offset.
            // A locally recomputed value is never published in its place.
            let confirmed = transaction.calibration.result;
            result.error = confirmed;
            if let Some(offset) = confirmed {
                self.calibration_offset = Some(offset);
            }
        }

        if direct && !operation.apply_terms().is_empty() {
            match direct_write_check(
                operation,
                &results,
                &parameters,
                &transaction.values,
                &provenance,
                snapshot,
            ) {
                DirectWriteCheck::Confirmed => result.applied = true,
                DirectWriteCheck::Mismatch(problems) => {
                    self.probe_error = Some(format!(
                        "{} reported DIRECT WCO, but the controller row does not match the \
                         result: {}; it is not applied — press A to apply it explicitly",
                        operation.name,
                        problems.join(", ")
                    ));
                }
                DirectWriteCheck::Unverified {
                    anchors,
                    uncaptured,
                } => {
                    let reason = match (anchors.is_empty(), uncaptured.is_empty()) {
                        (false, true) => format!(
                            "{} is a controller-side start/corner anchor that cannot be read back",
                            anchors.join("/")
                        ),
                        (true, false) => format!(
                            "{} had no captured origin when the run started",
                            uncaptured.join("/")
                        ),
                        _ => format!(
                            "{} is a controller-side start/corner anchor and {} had no captured \
                             origin",
                            anchors.join("/"),
                            uncaptured.join("/")
                        ),
                    };
                    self.probe_error = Some(format!(
                        "{} DIRECT WCO wrote the controller row, but {reason}; the write is not \
                         numerically verified and is not claimed applied — press A to apply the \
                         result explicitly",
                        operation.name
                    ));
                }
            }
        }
        if let Some(message) = &self.probe_error {
            log::warn!("probe {}: {message}", operation.id);
        }

        self.probe_last_result = Some(result.clone());
        self.probe_history.push_front(result);
        while self.probe_history.len() > MAX_PROBE_HISTORY {
            self.probe_history.pop_back();
        }
    }
}

/// Outcome of comparing a DIRECT routine's live row with what the
/// transaction intended before it ran.
enum DirectWriteCheck {
    /// Every intended axis, and the requested rotation, are live.
    Confirmed,
    /// The row disagrees with the result, or the target row is no longer the
    /// active one.
    Mismatch(Vec<String>),
    /// Every compared value matched, but these axes are only known inside the
    /// interpreter: an Edge Angle start/corner anchor, or an origin that was
    /// never captured. The write is on the controller; no numeric claim is
    /// made, and the state is deliberately not a mismatch.
    Unverified {
        anchors: Vec<&'static str>,
        uncaptured: Vec<&'static str>,
    },
}

/// Only a live controller row can confirm a DIRECT write: completion of the
/// block, or the routine's own claim, is not evidence that `G10 L2` ran.
fn direct_write_check(
    operation: &ProbeOperation,
    results: &linc_core::probe::ParameterResults,
    parameters: &ProbeParameters,
    values: &[f64; 12],
    provenance: &ProbeProvenance,
    snapshot: &MachineSnapshot,
) -> DirectWriteCheck {
    let Some(index) = provenance.index else {
        return DirectWriteCheck::Mismatch(vec![
            "no active work offset when the run started".into()
        ]);
    };
    if snapshot.offsets.active_index != Some(index) {
        return DirectWriteCheck::Mismatch(vec![format!(
            "the active work offset is now {}, but the run wrote {}",
            snapshot
                .offsets
                .active_index
                .map(wcs_name)
                .unwrap_or_else(|| "--".into()),
            wcs_name(index)
        )]);
    }
    let Some(row) = snapshot
        .offsets
        .offsets
        .iter()
        .find(|offset| offset.index == index)
    else {
        return DirectWriteCheck::Mismatch(vec![format!(
            "{} is missing from the status poll",
            wcs_name(index)
        )]);
    };

    // Compare exactly the axes this operation's DIRECT branch writes, and
    // require every other axis to be untouched.
    let written = direct_written_axes(operation);
    if written.is_empty() {
        return DirectWriteCheck::Mismatch(vec![format!(
            "{} writes no work offset in DIRECT mode",
            operation.name
        )]);
    }
    let auxiliary = std::collections::BTreeMap::new();
    let expected = expected_row(operation, results, &provenance.source_offsets, &auxiliary);
    let mut problems = Vec::new();
    let mut anchors = Vec::new();
    let mut uncaptured = Vec::new();
    for axis in written.iter().copied() {
        match expected.axes.get(axis).copied() {
            Some(want) => match row.values.get(axis).copied() {
                Some(actual) if (actual - want).abs() > OFFSET_TOLERANCE => {
                    problems.push(format!("{axis}{actual:.6} (expected {want:.6})"));
                }
                Some(_) => {}
                None => problems.push(format!("{axis} is missing from the live row")),
            },
            None if is_anchor_axis(operation, axis) => anchors.push(axis),
            // No captured origin: not an anchor, and still unverifiable.
            None => uncaptured.push(axis),
        }
    }
    for (axis, before) in provenance.source_offsets.iter() {
        if written.contains(&axis) {
            continue;
        }
        let Some(actual) = row.values.get(axis).copied() else {
            continue;
        };
        if (actual - before).abs() > OFFSET_TOLERANCE {
            problems.push(format!(
                "{axis}{actual:.6} changed although {} does not write {axis} (was {before:.6})",
                operation.name
            ));
        }
    }
    if operation.category == ProbeCategory::EdgeAngle {
        // Every Edge Angle run clears rotation with `R0`; the DIRECT branch
        // adds the requested angle, so the rotation is checkable even when the
        // anchor-dependent axes are not.
        let intended = if parameters.wcs_rotation {
            values[11]
        } else {
            0.0
        };
        let actual = row.rotation.unwrap_or(0.0);
        if (actual - intended).abs() > OFFSET_TOLERANCE {
            problems.push(format!("R{actual:.4} (expected {intended:.4})"));
        }
    } else if (row.rotation.unwrap_or(0.0) - provenance.rotation).abs() > OFFSET_TOLERANCE {
        problems.push(format!(
            "the row rotation changed to {:.4}, but {} does not write one",
            row.rotation.unwrap_or(0.0),
            operation.name
        ));
    }

    if !problems.is_empty() {
        DirectWriteCheck::Mismatch(problems)
    } else if !anchors.is_empty() || !uncaptured.is_empty() {
        DirectWriteCheck::Unverified {
            anchors,
            uncaptured,
        }
    } else {
        DirectWriteCheck::Confirmed
    }
}

/// Does this operation's Apply read `axis` from a controller-side anchor?
fn is_anchor_axis(operation: &ProbeOperation, axis: &str) -> bool {
    operation.apply_terms().iter().any(|term| {
        term.axis == axis
            && matches!(
                term.source,
                linc_core::probe::ApplySource::StartX
                    | linc_core::probe::ApplySource::StartY
                    | linc_core::probe::ApplySource::CornerX
                    | linc_core::probe::ApplySource::CornerY
            )
    })
}

/// Which axes an Apply intends to write, and which of them depend on
/// controller-side anchors that are only evaluable inside the interpreter.
pub(crate) struct ExpectedRow {
    pub axes: AxisMap<f64>,
    pub unverified: Vec<&'static str>,
}

pub(crate) fn expected_row(
    operation: &ProbeOperation,
    results: &linc_core::probe::ParameterResults,
    source_offsets: &AxisMap<f64>,
    auxiliary: &std::collections::BTreeMap<String, f64>,
) -> ExpectedRow {
    let mut axes = AxisMap::new();
    let mut unverified = Vec::new();
    for term in operation.apply_terms() {
        match (
            term.source.value(results, auxiliary),
            source_offsets.get(term.axis).copied(),
        ) {
            (Some(measured), Some(source)) => {
                axes.insert(term.axis.to_string(), source + measured);
            }
            // A term whose value is only known to the interpreter, or whose
            // origin was never captured, is never reported as verified.
            _ => unverified.push(term.axis),
        }
    }
    ExpectedRow { axes, unverified }
}

/// Numeric disagreements between an intended row and the live one.
pub(crate) fn compare_row(
    expected: &ExpectedRow,
    rotation: Option<f64>,
    live: &WorkOffset,
) -> Vec<String> {
    let mut problems = Vec::new();
    for (axis, want) in expected.axes.iter() {
        match live.values.get(axis).copied() {
            Some(actual) if (actual - want).abs() > OFFSET_TOLERANCE => {
                problems.push(format!("{axis}{actual:.6} (expected {want:.6})"));
            }
            Some(_) => {}
            None => problems.push(format!("{axis} is missing from the live row")),
        }
    }
    if let Some(want) = rotation {
        let actual = live.rotation.unwrap_or(0.0);
        if (actual - want).abs() > OFFSET_TOLERANCE {
            problems.push(format!("R{actual:.6} (expected {want:.6})"));
        }
    }
    problems
}

pub(crate) fn probe_operation(block: &str) -> Option<&'static ProbeOperation> {
    // One exact-token resolver: the same one the simulator and the UI use.
    linc_core::probe::operation_for_mdi(block)
}

fn probe_is_idle(status: &Status) -> bool {
    status.task.interp_state == linuxcnc_nml::InterpState::Idle
        && status.task.exec_state == ExecState::Done
        && status.task.queued_mdi_commands == 0
        && status.motion.inpos
        && !status.motion.probing
}

pub fn initial_snapshot(profile: &Profile) -> MachineSnapshot {
    let mut snapshot = MachineSnapshot::empty(profile.variant);
    snapshot.machine_name = profile.config.machine.name.clone();
    snapshot.simulated = profile.is_sim();
    for axis in &profile.machine_ini.axes {
        snapshot
            .positions
            .axes
            .insert(axis.name.clone(), PositionSet::default());
        snapshot.machine.homed.insert(axis.name.clone(), false);
        snapshot.machine.homing.insert(axis.name.clone(), false);
        snapshot
            .machine
            .following_error
            .insert(axis.name.clone(), 0.0);
        snapshot.machine.servo_load.insert(axis.name.clone(), 0.0);
        snapshot.machine.commanded.insert(axis.name.clone(), 0.0);
        snapshot.machine.actual.insert(axis.name.clone(), 0.0);
        snapshot.offsets.g92.insert(axis.name.clone(), 0.0);
    }
    snapshot.positions.units = profile.machine_ini.linear_units.clone();
    snapshot.positions.angular_units = profile.machine_ini.angular_units.clone();
    for index in 1..=9 {
        let mut values = AxisMap::new();
        for axis in &profile.machine_ini.axes {
            values.insert(axis.name.clone(), 0.0);
        }
        snapshot.offsets.offsets.push(WorkOffset {
            index,
            values,
            rotation: Some(0.0),
        });
    }
    snapshot.offsets.active_index = Some(1);
    for index in 0..profile.machine_ini.spindles {
        snapshot.spindles.push(SpindleState {
            index,
            name: if index == 0 {
                "MAIN SPINDLE".into()
            } else if profile.variant == MachineVariant::Lathe {
                "LIVE TOOL".into()
            } else {
                format!("SPINDLE {index}")
            },
            commanded_speed: 0.0,
            actual_speed: 0.0,
            load: None,
            override_factor: 1.0,
            direction: 0,
            on: false,
            live_tool: profile.variant == MachineVariant::Lathe && index > 0,
            live_tool_axial: false,
            live_tool_max_rpm: profile
                .config
                .lathe
                .as_ref()
                .and_then(|lathe| lathe.live_tool_max_rpm)
                .unwrap_or(0.0),
        });
    }
    snapshot.feed.units = profile.machine_ini.linear_units.clone();
    snapshot
}

fn atc_state(profile: &Profile, spindles: &[SpindleState]) -> Option<AtcState> {
    if profile.variant == MachineVariant::Lathe {
        let pockets = profile.turret_stations().unwrap_or(0);
        Some(AtcState {
            pockets,
            current_pocket: None,
            prepared_pocket: None,
            clamped: Some(true),
            indexing: false,
            station: None,
            live_tool_rpm: spindles
                .iter()
                .find(|spindle| spindle.live_tool)
                .map(|spindle| spindle.actual_speed),
            live_tool_direction: spindles
                .iter()
                .find(|spindle| spindle.live_tool)
                .map(|spindle| spindle.direction)
                .unwrap_or(0),
            live_tool_axial: None,
            fault: None,
        })
    } else if profile.changer.uses_pockets() {
        Some(AtcState {
            pockets: profile.machine_ini.atc_pockets.unwrap_or(0),
            current_pocket: None,
            prepared_pocket: None,
            clamped: None,
            indexing: false,
            station: None,
            live_tool_rpm: None,
            live_tool_direction: 0,
            live_tool_axial: None,
            fault: None,
        })
    } else {
        None
    }
}

fn motion_active(status: &Status) -> bool {
    status.motion.current_vel.abs() > 1e-6 || status.motion.probing
}

/// Pose slot of an axis letter. INI coordinate lists are machine-specific
/// (`X Z` on a lathe), while the NML pose is always X,Y,Z,A,B,C,U,V,W.
pub(crate) fn axis_pose_index(name: &str) -> Option<usize> {
    match name.to_ascii_uppercase().as_str() {
        "X" => Some(0),
        "Y" => Some(1),
        "Z" => Some(2),
        "A" => Some(3),
        "B" => Some(4),
        "C" => Some(5),
        "U" => Some(6),
        "V" => Some(7),
        "W" => Some(8),
        _ => None,
    }
}

fn pose_component(pose: &linuxcnc_nml::Pose, index: usize) -> f64 {
    match index {
        0 => pose.x,
        1 => pose.y,
        2 => pose.z,
        3 => pose.a,
        4 => pose.b,
        5 => pose.c,
        6 => pose.u,
        7 => pose.v,
        8 => pose.w,
        _ => 0.0,
    }
}

fn limit_labels(status: &Status, axes: &[linc_core::ini::AxisIni]) -> Vec<String> {
    let mut labels = Vec::new();
    for (index, mask) in status.limits.iter().enumerate() {
        let Some(axis) = axes.get(index) else {
            continue;
        };
        if mask & 1 != 0 {
            labels.push(format!("{}-", axis.name));
        }
        if mask & 2 != 0 {
            labels.push(format!("{}+", axis.name));
        }
        if mask & 4 != 0 {
            labels.push(format!("{} MIN", axis.name));
        }
        if mask & 8 != 0 {
            labels.push(format!("{} MAX", axis.name));
        }
    }
    labels
}

fn feed_word(status: &Status, linear_units: &str) -> f64 {
    let word = status.active_settings.get(1).copied().unwrap_or(0.0);
    let program_inch = status.active_gcodes.contains(&200);
    let machine_inch = linear_units.trim().to_ascii_lowercase().starts_with("in");
    if program_inch && !machine_inch {
        word * 25.4
    } else if !program_inch && machine_inch {
        word / 25.4
    } else {
        word
    }
}

fn feed_mode(status: &Status) -> String {
    if status.active_gcodes.contains(&950) {
        "G95".into()
    } else if status.active_gcodes.contains(&930) {
        "G93".into()
    } else {
        "G94".into()
    }
}

fn distance_mode(status: &Status) -> String {
    if status.active_gcodes.contains(&910) {
        "G91".into()
    } else {
        "G90".into()
    }
}

fn modal_lines(gcodes: &[i32], mcodes: &[i32]) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut gs = gcodes
        .iter()
        .skip(1)
        .copied()
        .filter(|value| *value != -1)
        .collect::<Vec<_>>();
    gs.sort_unstable();
    for raw in gs {
        entries.push((format!("G{}", raw as f64 / 10.0), String::new()));
    }
    let mut ms = mcodes
        .iter()
        .skip(1)
        .copied()
        .filter(|value| *value != -1)
        .collect::<Vec<_>>();
    ms.sort_unstable();
    for raw in ms {
        entries.push((format!("M{raw}"), String::new()));
    }
    entries
}

/// Whether the move that just reached a new tool-tip sample is a rapid.
///
/// The trail keeps one point per poll, so the hop that reached the point is the
/// motion the controller is executing. The status carries that directly as the
/// motion type: LinuxCNC's EMC_MOTION_TYPE_TRAVERSE (1) is G0, while feed,
/// arc, probing and rotary indexing are cuts. Reading active_gcodes instead is
/// wrong here: that array is not ordered with the motion mode first, and most
/// hops were classified as cuts.
const MOTION_TYPE_TRAVERSE: i32 = 1;

fn trail_hop_is_rapid(motion_type: i32) -> bool {
    motion_type == MOTION_TYPE_TRAVERSE
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Copy HMI-owned HAL selections across a rebuilt snapshot. The snapshot is
/// rebuilt from controller state on every poll, so without this the jog
/// selections and the manual tool-change level would be lost.
pub fn carry_hal_selections(converted: &mut MachineSnapshot, stored: &MachineSnapshot) {
    converted.hal.jog_increment = stored.hal.jog_increment;
    converted.hal.jog_speed = stored.hal.jog_speed;
    // The tool-change confirmation is owned by `SnapshotBuilder::tool_confirm`
    // and cleared when `iocontrol.0.tool-change` drops. Carrying the previous
    // level here would overwrite the first confirmation with `false`.
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_gcodes_match_probe_basic_text() {
        let gcodes = [0, 800, -1, 170, 400, 210, 900, 940, 540, 80];
        let mcodes = [0, -1, 5, -1, 9];
        let lines = modal_lines(&gcodes, &mcodes);
        let text = lines
            .iter()
            .map(|(key, value)| format!("{key}{value}"))
            .collect::<Vec<_>>();
        assert_eq!(
            text,
            vec!["G8", "G17", "G21", "G40", "G54", "G80", "G90", "G94", "M5", "M9"]
        );
    }

    #[test]
    fn only_traverse_trail_hops_are_rapids() {
        // EMC_MOTION_TYPE_TRAVERSE is G0; every other motion type is a cut.
        assert!(trail_hop_is_rapid(1), "G0 traverses");
        assert!(!trail_hop_is_rapid(2), "G1 feeds");
        assert!(!trail_hop_is_rapid(3), "G2/G3 arc");
        assert!(!trail_hop_is_rapid(4), "tool change");
        assert!(!trail_hop_is_rapid(5), "probing");
        assert!(!trail_hop_is_rapid(0), "no motion");
    }

    #[test]
    fn axis_letters_map_to_pose_slots_not_coordinate_order() {
        // A lathe's coordinate list is [X, Z]; Z must read pose.z, not pose.y.
        assert_eq!(axis_pose_index("X"), Some(0));
        assert_eq!(axis_pose_index("Z"), Some(2));
        assert_eq!(axis_pose_index("z"), Some(2));
        assert_eq!(axis_pose_index("A"), Some(3));
        assert_eq!(axis_pose_index("W"), Some(8));
        assert_eq!(axis_pose_index("9"), None);
    }

    #[test]
    fn jog_selections_are_carried_across_a_poll() {
        let mut stored = MachineSnapshot::empty(MachineVariant::Mill);
        stored.hal.jog_speed = Some(42.0);
        stored.hal.jog_increment = Some(0.5);
        let mut converted = MachineSnapshot::empty(MachineVariant::Mill);
        carry_hal_selections(&mut converted, &stored);
        assert_eq!(converted.hal.jog_speed, Some(42.0));
        assert_eq!(converted.hal.jog_increment, Some(0.5));
    }

    #[test]
    fn every_probe_trip_wording_latches_and_survives_rebuilds() {
        use linc_core::faults::ProbeTripKind;

        let mut builder = probe_builder();
        let mut status = idle_status();
        for kind in ProbeTripKind::ALL {
            builder.clear_probe_trip();
            builder.push_message(MessageKind::OperatorError, kind.message());

            let first = poll_probe(&mut builder, &status);
            let fault = first.probe_trip.as_ref().expect("trip latched");
            assert_eq!(fault.kind, kind);
            assert_eq!(fault.message, kind.message());

            // Unrelated controller errors and further status polls are not a
            // clear: only the reset path clears the fault.
            builder.push_message(MessageKind::OperatorError, "spindle amplifier fault");
            for _ in 0..10 {
                status.motion.heartbeat = status.motion.heartbeat.wrapping_add(1);
                let later = poll_probe(&mut builder, &status);
                assert_eq!(later.probe_trip.as_ref().map(|f| f.kind), Some(kind));
            }
        }
    }

    #[test]
    fn the_clear_path_is_the_only_clear() {
        let mut builder = probe_builder();
        let status = idle_status();
        builder.push_message(
            MessageKind::OperatorError,
            "Probe tripped during a coordinate jog.",
        );
        assert!(poll_probe(&mut builder, &status).probe_trip_active());

        builder.clear_probe_trip();
        assert!(poll_probe(&mut builder, &status).probe_trip.is_none());

        // A later non-trip message cannot resurrect it.
        builder.push_message(MessageKind::OperatorText, "program end");
        assert!(poll_probe(&mut builder, &status).probe_trip.is_none());

        // The same wording after a clear latches again and is a new Messages
        // entry, because the clear also forgets the dedupe text.
        builder.push_message(
            MessageKind::OperatorError,
            "Probe tripped during a coordinate jog.",
        );
        let tripped = poll_probe(&mut builder, &status);
        assert!(tripped.probe_trip_active());
        assert_eq!(
            tripped
                .alarms
                .iter()
                .filter(|alarm| alarm.message == "Probe tripped during a coordinate jog.")
                .count(),
            2
        );
    }

    #[test]
    fn probe_trip_text_reaches_messages_verbatim() {
        let mut builder = probe_builder();
        let status = idle_status();
        builder.push_message(
            MessageKind::OperatorError,
            "  Probe tripped during a joint jog.\n",
        );
        let snapshot = poll_probe(&mut builder, &status);
        assert_eq!(
            snapshot.alarms.front().map(|alarm| alarm.message.as_str()),
            Some("Probe tripped during a joint jog.")
        );
        assert!(snapshot.probe_trip_active());
    }

    fn probe_builder() -> SnapshotBuilder {
        let profile_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../configs/sim.toml");
        let profile = Profile::load(&profile_path, None).expect("test profile");
        let tool_table = ToolTable::load(
            &profile.machine_ini.tool_table,
            &profile.machine_ini.tool_table_columns,
            profile.changer,
        )
        .expect("test tool table");
        SnapshotBuilder::new(profile, tool_table)
    }

    fn idle_status() -> Status {
        let mut status = Status::from_raw(Default::default());
        status.valid = true;
        status.task.mode = NmlTaskMode::Mdi;
        status.task.state = NmlTaskState::On;
        status.task.exec_state = ExecState::Done;
        status.task.interp_state = linuxcnc_nml::InterpState::Idle;
        status.motion.enabled = true;
        status.motion.inpos = true;
        status.offsets.g5x_index = 1;
        status
    }

    fn make_idle(status: &mut Status) {
        status.task.exec_state = ExecState::Done;
        status.task.interp_state = linuxcnc_nml::InterpState::Idle;
        status.task.queued_mdi_commands = 0;
        status.motion.inpos = true;
        status.motion.probing = false;
    }

    fn poll_probe(builder: &mut SnapshotBuilder, status: &Status) -> MachineSnapshot {
        let panel = PanelState::offline(&builder.profile);
        builder.build(status, &panel, Vec::new(), &ParameterFile::default())
    }

    /// The active row as the dispatch path records it, before the block runs.
    fn provenance(index: u8, x: f64, y: f64, z: f64) -> ProbeProvenance {
        let mut source_offsets = AxisMap::new();
        source_offsets.insert("X".to_string(), x);
        source_offsets.insert("Y".to_string(), y);
        source_offsets.insert("Z".to_string(), z);
        ProbeProvenance {
            index: Some(index),
            wcs: wcs_name(index),
            source_offsets,
            rotation: 0.0,
        }
    }

    fn direct_parameters() -> ProbeParameters {
        let mut parameters = ProbeParameters::default();
        parameters.probe_mode = ProbeMode::ApplyWcs;
        parameters
    }

    fn start_direct(
        builder: &mut SnapshotBuilder,
        operation: &'static ProbeOperation,
        provenance: ProbeProvenance,
    ) {
        let parameters = direct_parameters();
        builder.note_probe_block(Some(parameters.mdi_text(operation)), provenance);
    }

    fn push_calibration_slots(builder: &mut SnapshotBuilder, operation: &ProbeOperation) {
        for parameter in operation.result_parameters() {
            let value = match parameter {
                1001 => 50.04,
                1002 => 1.0,
                1003 => 49.98,
                1004 => 2.0,
                1010 => 50.01,
                _ => 0.0,
            };
            builder.push_probe_value(parameter as u32, value);
        }
    }

    #[test]
    fn live_probe_commits_once_from_exact_updates() {
        let mut builder = probe_builder();
        let operation = linc_core::probe::operation("edge_x_plus").unwrap();
        let parameters = ProbeParameters::default();
        builder.note_probe_block(
            Some(parameters.mdi_text(operation)),
            provenance(2, 10.0, 0.0, 0.0),
        );
        let mut status = idle_status();
        status.motion.probe_val = 1;
        status.offsets.g5x_index = 2;
        status.offsets.g5x_offset.x = 10.0;

        // A just-issued transaction can be polled before LinuxCNC queues it.
        let pending = poll_probe(&mut builder, &status);
        assert!(pending.probe.running);
        assert!(pending.probe.last_result.is_none());
        assert!(pending.probe.error.is_none());
        assert_eq!(pending.probe.selected_wcs, "G55");
        assert_eq!(pending.probe.input_tripped, Some(true));

        status.task.interp_state = linuxcnc_nml::InterpState::Reading;
        status.task.exec_state = ExecState::WaitingForMotion;
        status.motion.probing = true;
        builder.push_probe_value(1006, 12.5);
        builder.push_probe_value(1005, f64::NAN);
        builder.push_probe_value(999, 99.0);
        let active = poll_probe(&mut builder, &status);
        assert!(active.probe.last_result.is_none());

        // POSITION does not write the row, but provenance stays the value
        // captured when the block was issued.
        status.offsets.g5x_offset.x = 99.0;
        make_idle(&mut status);
        let completed = poll_probe(&mut builder, &status);
        let result = completed.probe.last_result.as_ref().unwrap();
        assert_eq!(result.operation, "edge_x_plus");
        assert_eq!(result.updated, vec![1006]);
        assert_eq!(result.value(1006), Some(12.5));
        assert_eq!(result.value(1005), None);
        assert_eq!(result.mode, "POSITION");
        assert_eq!(result.wcs, "G55");
        assert_eq!(result.source_offsets.get("X"), Some(&10.0));
        assert!(!result.applied);
        assert!(!completed.probe.running);
        assert_eq!(completed.probe.history.len(), 1);

        let timestamp = result.timestamp_ms;
        let repeated = poll_probe(&mut builder, &status);
        assert_eq!(repeated.probe.history.len(), 1);
        assert_eq!(
            repeated.probe.last_result.as_ref().unwrap().timestamp_ms,
            timestamp
        );

        builder.note_probe_block(
            Some(parameters.parameter_update_block()),
            ProbeProvenance::default(),
        );
        let parameter_update = poll_probe(&mut builder, &status);
        assert_eq!(parameter_update.probe.history.len(), 1);
        assert_eq!(
            parameter_update
                .probe
                .last_result
                .as_ref()
                .unwrap()
                .timestamp_ms,
            timestamp
        );
    }

    #[test]
    fn failed_and_incomplete_transactions_discard_pending_values() {
        let mut builder = probe_builder();
        let operation = linc_core::probe::operation("edge_x_plus").unwrap();
        let parameters = ProbeParameters::default();
        let mut status = idle_status();

        builder.note_probe_block(
            Some(parameters.mdi_text(operation)),
            ProbeProvenance::default(),
        );
        builder.fail_probe();
        let failed = poll_probe(&mut builder, &status);
        assert!(failed.probe.last_result.is_none());
        assert!(failed.probe.history.is_empty());
        assert!(failed.probe.error.as_deref().unwrap().contains("failed"));
        assert!(!failed.probe.running);

        builder.note_probe_block(
            Some(parameters.mdi_text(operation)),
            ProbeProvenance::default(),
        );
        assert!(poll_probe(&mut builder, &status).probe.error.is_none());
        status.task.queued_mdi_commands = 1;
        poll_probe(&mut builder, &status);
        builder.push_probe_value(1001, 999.0);
        make_idle(&mut status);
        let incomplete = poll_probe(&mut builder, &status);
        assert!(incomplete.probe.last_result.is_none());
        assert!(incomplete.probe.history.is_empty());
        assert!(incomplete.probe.error.as_deref().unwrap().contains("#1006"));
        assert!(!incomplete.probe.running);
    }

    #[test]
    fn direct_write_is_applied_only_when_the_live_row_confirms_it() {
        let mut builder = probe_builder();
        let operation = linc_core::probe::operation("edge_x_plus").unwrap();
        start_direct(&mut builder, operation, provenance(1, 10.0, 0.0, 0.0));
        builder.push_probe_value(1006, 2.0);

        // The routine's own G10 took effect: raw source 10 + measured 2.
        let mut status = idle_status();
        status.offsets.g5x_offset.x = 12.0;
        let completed = poll_probe(&mut builder, &status);
        let result = completed.probe.last_result.as_ref().unwrap();
        assert_eq!(result.mode, "DIRECT WCO");
        assert!(result.applied);
        assert!(completed.probe.error.is_none());
        assert_eq!(result.source_offsets.get("X"), Some(&10.0));
        assert_eq!(completed.probe.history.len(), 1);

        // Polling again must not apply or record it a second time.
        let timestamp = result.timestamp_ms;
        let repeated = poll_probe(&mut builder, &status);
        assert_eq!(repeated.probe.history.len(), 1);
        let repeated_result = repeated.probe.last_result.as_ref().unwrap();
        assert_eq!(repeated_result.timestamp_ms, timestamp);
        assert!(repeated_result.applied);
        assert!(repeated_result.apply_error(Some(1)).is_err());
    }

    #[test]
    fn direct_commit_keeps_the_pre_run_origin() {
        let mut builder = probe_builder();
        let operation = linc_core::probe::operation("edge_x_plus").unwrap();
        start_direct(&mut builder, operation, provenance(3, -5.0, 0.0, 0.0));
        builder.push_probe_value(1006, 1.25);

        // The routine wrote the row itself; provenance must still be the row
        // as it stood when the block was issued.
        let mut status = idle_status();
        status.offsets.g5x_index = 3;
        status.offsets.g5x_offset.x = -3.75;
        let completed = poll_probe(&mut builder, &status);
        let result = completed.probe.last_result.as_ref().unwrap();
        assert!(result.applied);
        assert_eq!(result.wcs, "G56");
        assert_eq!(result.source_offsets.get("X"), Some(&-5.0));
    }

    #[test]
    fn direct_write_without_a_g10_is_not_applied() {
        let mut builder = probe_builder();
        let operation = linc_core::probe::operation("edge_x_plus").unwrap();
        start_direct(&mut builder, operation, provenance(1, 10.0, 0.0, 0.0));
        builder.push_probe_value(1006, 2.0);

        // No write at all: the live row still holds the pre-run origin.
        let mut status = idle_status();
        status.offsets.g5x_offset.x = 10.0;
        let completed = poll_probe(&mut builder, &status);
        let result = completed.probe.last_result.as_ref().unwrap();
        assert!(!result.applied);
        assert!(completed
            .probe
            .error
            .as_deref()
            .unwrap()
            .contains("does not match"));
        // The measurement is still committed, and explicit Apply remains the
        // recovery path for an unconfirmed DIRECT write.
        assert_eq!(completed.probe.history.len(), 1);
        assert!(result.apply_error(Some(1)).is_ok());
    }

    #[test]
    fn wrong_direct_write_is_not_applied() {
        let mut builder = probe_builder();
        let operation = linc_core::probe::operation("edge_x_plus").unwrap();
        start_direct(&mut builder, operation, provenance(1, 10.0, 0.0, 0.0));
        builder.push_probe_value(1006, 2.0);

        // A PR-02-style write that omits the source origin: the routine wrote
        // the measured value instead of source + measured.
        let mut status = idle_status();
        status.offsets.g5x_offset.x = 2.0;
        let completed = poll_probe(&mut builder, &status);
        let result = completed.probe.last_result.as_ref().unwrap();
        assert!(!result.applied);
        let error = completed.probe.error.as_deref().unwrap();
        assert!(error.contains("X2.000000"), "{error}");
        assert!(error.contains("expected 12.000000"), "{error}");
        assert!(result.apply_error(Some(1)).is_ok());
    }

    #[test]
    fn partial_direct_write_is_not_applied() {
        let mut builder = probe_builder();
        let operation = linc_core::probe::operation("front_left_top_corner").unwrap();
        start_direct(&mut builder, operation, provenance(1, 10.0, 20.0, 30.0));
        builder.push_probe_value(1006, 1.0);
        builder.push_probe_value(1008, 2.0);
        builder.push_probe_value(1009, -1.0);

        let mut status = idle_status();
        status.offsets.g5x_offset.x = 11.0;
        status.offsets.g5x_offset.y = 20.0;
        // X and the helper's Z were written; Y was not.
        status.offsets.g5x_offset.z = 29.0;
        let completed = poll_probe(&mut builder, &status);
        let result = completed.probe.last_result.as_ref().unwrap();
        assert!(!result.applied);
        let error = completed.probe.error.as_deref().unwrap();
        assert!(error.contains("Y20.000000"), "{error}");
        assert!(error.contains("expected 22.000000"), "{error}");
        assert!(!error.contains("X11"), "{error}");
        assert!(!error.contains("Z"), "{error}");
        assert!(result.apply_error(Some(1)).is_ok());
    }

    #[test]
    fn direct_corner_write_is_confirmed_and_applied_exactly_once() {
        let operation = linc_core::probe::operation("front_left_top_corner").unwrap();
        let mut builder = probe_builder();
        start_direct(&mut builder, operation, provenance(1, 10.0, 20.0, 30.0));
        builder.push_probe_value(1006, 1.0);
        builder.push_probe_value(1008, 2.0);
        builder.push_probe_value(1009, -1.0);
        let mut status = idle_status();
        status.offsets.g5x_offset.x = 11.0;
        status.offsets.g5x_offset.y = 22.0;
        // The helper (probe_z_minus_sub, called with #3030) writes Z too.
        status.offsets.g5x_offset.z = 29.0;
        let completed = poll_probe(&mut builder, &status);
        assert!(
            completed.probe.error.is_none(),
            "{:?}",
            completed.probe.error
        );
        let result = completed.probe.last_result.as_ref().unwrap();
        assert!(result.applied);
        assert_eq!(result.mode, "DIRECT WCO");
        // Polling again neither re-applies nor records a second result.
        let timestamp = result.timestamp_ms;
        let repeated = poll_probe(&mut builder, &status);
        assert_eq!(repeated.probe.history.len(), 1);
        assert_eq!(
            repeated.probe.last_result.as_ref().unwrap().timestamp_ms,
            timestamp
        );

        // A wrong X is still caught.
        let mut builder = probe_builder();
        start_direct(&mut builder, operation, provenance(1, 10.0, 20.0, 30.0));
        builder.push_probe_value(1006, 1.0);
        builder.push_probe_value(1008, 2.0);
        builder.push_probe_value(1009, -1.0);
        let mut status = idle_status();
        status.offsets.g5x_offset.x = 10.5;
        status.offsets.g5x_offset.y = 22.0;
        status.offsets.g5x_offset.z = 29.0;
        let completed = poll_probe(&mut builder, &status);
        assert!(!completed.probe.last_result.as_ref().unwrap().applied);
        let error = completed.probe.error.as_deref().unwrap();
        assert!(error.contains("X10.500000"), "{error}");
        assert!(error.contains("expected 11.000000"), "{error}");
    }

    #[test]
    fn direct_x_plus_helper_z_write_leaves_y_alone() {
        let operation = linc_core::probe::operation("right_top_side").unwrap();
        let mut builder = probe_builder();
        start_direct(&mut builder, operation, provenance(1, 10.0, 20.0, 30.0));
        builder.push_probe_value(1005, 0.5);
        builder.push_probe_value(1009, -1.0);
        let mut status = idle_status();
        status.offsets.g5x_offset.x = 10.5;
        status.offsets.g5x_offset.y = 20.0;
        // X inline plus the helper's Z; Y is never written.
        status.offsets.g5x_offset.z = 29.0;
        let completed = poll_probe(&mut builder, &status);
        assert!(
            completed.probe.error.is_none(),
            "{:?}",
            completed.probe.error
        );
        assert!(completed.probe.last_result.as_ref().unwrap().applied);

        let mut builder = probe_builder();
        start_direct(&mut builder, operation, provenance(1, 10.0, 20.0, 30.0));
        builder.push_probe_value(1005, 0.5);
        builder.push_probe_value(1009, -1.0);
        let mut status = idle_status();
        status.offsets.g5x_offset.x = 10.0;
        status.offsets.g5x_offset.y = 20.0;
        status.offsets.g5x_offset.z = 29.0;
        let completed = poll_probe(&mut builder, &status);
        assert!(!completed.probe.last_result.as_ref().unwrap().applied);
        assert!(completed
            .probe
            .error
            .as_deref()
            .unwrap()
            .contains("X10.000000"));
    }

    #[test]
    fn direct_y_plus_helper_z_write_leaves_x_alone() {
        let operation = linc_core::probe::operation("front_top_side").unwrap();
        let mut builder = probe_builder();
        start_direct(&mut builder, operation, provenance(1, 10.0, 20.0, 30.0));
        builder.push_probe_value(1008, 2.0);
        builder.push_probe_value(1009, -1.0);
        let mut status = idle_status();
        status.offsets.g5x_offset.x = 10.0;
        status.offsets.g5x_offset.y = 22.0;
        // Y inline plus the helper's Z; X is never written.
        status.offsets.g5x_offset.z = 29.0;
        let completed = poll_probe(&mut builder, &status);
        assert!(
            completed.probe.error.is_none(),
            "{:?}",
            completed.probe.error
        );
        assert!(completed.probe.last_result.as_ref().unwrap().applied);

        let mut builder = probe_builder();
        start_direct(&mut builder, operation, provenance(1, 10.0, 20.0, 30.0));
        builder.push_probe_value(1008, 2.0);
        builder.push_probe_value(1009, -1.0);
        let mut status = idle_status();
        status.offsets.g5x_offset.x = 10.0;
        status.offsets.g5x_offset.y = 21.0;
        status.offsets.g5x_offset.z = 29.0;
        let completed = poll_probe(&mut builder, &status);
        assert!(!completed.probe.last_result.as_ref().unwrap().applied);
        assert!(completed
            .probe
            .error
            .as_deref()
            .unwrap()
            .contains("Y21.000000"));
    }

    #[test]
    fn direct_write_into_an_unwritten_axis_is_a_mismatch() {
        // right_top_side writes X and the helper's Z; a stray Y write is
        // still a write the operator did not ask for.
        let operation = linc_core::probe::operation("right_top_side").unwrap();
        let mut builder = probe_builder();
        start_direct(&mut builder, operation, provenance(1, 10.0, 20.0, 30.0));
        builder.push_probe_value(1005, 0.5);
        builder.push_probe_value(1009, -1.0);
        let mut status = idle_status();
        status.offsets.g5x_offset.x = 10.5;
        status.offsets.g5x_offset.y = 20.5;
        status.offsets.g5x_offset.z = 29.0;
        let completed = poll_probe(&mut builder, &status);
        assert!(!completed.probe.last_result.as_ref().unwrap().applied);
        let error = completed.probe.error.as_deref().unwrap();
        assert!(error.contains("Y20.500000"), "{error}");
        assert!(error.contains("does not write Y"), "{error}");
    }

    #[test]
    fn angle_direct_write_is_never_claimed_applied() {
        let mut builder = probe_builder();
        let operation = linc_core::probe::operation("top_front_edge_angle").unwrap();
        start_direct(&mut builder, operation, provenance(1, 10.0, 20.0, 30.0));
        builder.push_probe_value(1008, 25.0);
        builder.push_probe_value(1009, -2.0);
        builder.push_probe_value(1011, 1.0);
        builder.push_probe_value(1012, 0.5);

        let mut status = idle_status();
        // Y = source + #1008 - #1011, Z = source + #1009 (the helper writes
        // it); X is a controller-side start anchor and has no readback.
        status.offsets.g5x_offset.y = 20.0 + 25.0 - 1.0;
        status.offsets.g5x_offset.z = 30.0 - 2.0;
        status.offsets.rotation_xy = 0.0;
        let completed = poll_probe(&mut builder, &status);
        let result = completed.probe.last_result.as_ref().unwrap();
        assert!(!result.applied);
        let error = completed.probe.error.as_deref().unwrap();
        // The correct Z write must not be reported as a row mismatch; the only
        // reason is the unreadable anchor.
        assert!(error.contains("start/corner anchor"), "{error}");
        assert!(error.contains("X is a controller-side"), "{error}");
        assert!(!error.contains("does not match"), "{error}");
        assert!(!error.contains("does not write"), "{error}");
        assert!(!error.contains("changed although"), "{error}");
        assert_eq!(result.measured.get("Y"), Some(&24.0));
        assert_eq!(result.measured.get("Z"), Some(&-2.0));
        assert_eq!(result.measured.get("X"), None);
        assert!(result.apply_error(Some(1)).is_ok());
    }

    #[test]
    fn angle_direct_write_with_a_stale_rotation_is_a_mismatch() {
        let mut builder = probe_builder();
        let operation = linc_core::probe::operation("top_front_edge_angle").unwrap();
        start_direct(&mut builder, operation, provenance(1, 10.0, 20.0, 30.0));
        builder.push_probe_value(1008, 25.0);
        builder.push_probe_value(1009, -2.0);
        builder.push_probe_value(1011, 1.0);
        builder.push_probe_value(1012, 0.5);

        let mut status = idle_status();
        status.offsets.g5x_offset.y = 44.0;
        status.offsets.g5x_offset.z = 28.0;
        // Every Edge Angle run clears rotation with R0; this one did not.
        status.offsets.rotation_xy = 3.0;
        let completed = poll_probe(&mut builder, &status);
        let result = completed.probe.last_result.as_ref().unwrap();
        assert!(!result.applied);
        let error = completed.probe.error.as_deref().unwrap();
        assert!(error.contains("R3.0000"), "{error}");
        assert!(error.contains("expected 0.0000"), "{error}");
    }

    #[test]
    fn missing_helper_z_write_is_a_mismatch() {
        let mut builder = probe_builder();
        let operation = linc_core::probe::operation("front_left_top_corner").unwrap();
        start_direct(&mut builder, operation, provenance(1, 10.0, 20.0, 30.0));
        builder.push_probe_value(1006, 1.0);
        builder.push_probe_value(1008, 2.0);
        builder.push_probe_value(1009, -1.0);

        // X and Y were written; the helper's Z write did not happen even
        // though #1009 is -1.
        let mut status = idle_status();
        status.offsets.g5x_offset.x = 11.0;
        status.offsets.g5x_offset.y = 22.0;
        status.offsets.g5x_offset.z = 30.0;
        let completed = poll_probe(&mut builder, &status);
        let result = completed.probe.last_result.as_ref().unwrap();
        assert!(!result.applied);
        let error = completed.probe.error.as_deref().unwrap();
        assert!(error.contains("Z30.000000"), "{error}");
        assert!(error.contains("expected 29.000000"), "{error}");
        // Explicit Apply remains the recovery path.
        assert!(result.apply_error(Some(1)).is_ok());
    }

    #[test]
    fn wrong_helper_z_write_is_a_mismatch() {
        let mut builder = probe_builder();
        let operation = linc_core::probe::operation("front_left_top_corner").unwrap();
        start_direct(&mut builder, operation, provenance(1, 10.0, 20.0, 30.0));
        builder.push_probe_value(1006, 1.0);
        builder.push_probe_value(1008, 2.0);
        builder.push_probe_value(1009, -1.0);

        let mut status = idle_status();
        status.offsets.g5x_offset.x = 11.0;
        status.offsets.g5x_offset.y = 22.0;
        // 27.5 instead of the intended 29.0: no applied claim.
        status.offsets.g5x_offset.z = 27.5;
        let completed = poll_probe(&mut builder, &status);
        let result = completed.probe.last_result.as_ref().unwrap();
        assert!(!result.applied);
        let error = completed.probe.error.as_deref().unwrap();
        assert!(error.contains("Z27.500000"), "{error}");
        assert!(error.contains("expected 29.000000"), "{error}");
    }

    #[test]
    fn position_run_with_a_helper_never_writes_the_row() {
        let mut builder = probe_builder();
        let operation = linc_core::probe::operation("front_left_top_corner").unwrap();
        let parameters = ProbeParameters::default();
        builder.note_probe_block(
            Some(parameters.mdi_text(operation)),
            provenance(1, 10.0, 20.0, 30.0),
        );
        builder.push_probe_value(1006, 1.0);
        builder.push_probe_value(1008, 2.0);
        builder.push_probe_value(1009, -1.0);
        // The row is untouched by a measure-only run, including Z.
        let mut status = idle_status();
        status.offsets.g5x_offset.x = 10.0;
        status.offsets.g5x_offset.y = 20.0;
        status.offsets.g5x_offset.z = 30.0;
        let completed = poll_probe(&mut builder, &status);
        let result = completed.probe.last_result.as_ref().unwrap();
        assert_eq!(result.mode, "POSITION");
        assert!(!result.applied);
        assert!(
            completed.probe.error.is_none(),
            "{:?}",
            completed.probe.error
        );
        assert_eq!(result.measured.get("Z"), Some(&-1.0));
    }

    #[test]
    fn position_result_can_be_applied_once_by_hand() {
        let mut builder = probe_builder();
        let operation = linc_core::probe::operation("edge_x_plus").unwrap();
        let parameters = ProbeParameters::default();
        builder.note_probe_block(
            Some(parameters.mdi_text(operation)),
            provenance(1, 10.0, 0.0, 0.0),
        );
        builder.push_probe_value(1006, 2.0);
        let status = idle_status();
        let completed = poll_probe(&mut builder, &status);
        let result = completed.probe.last_result.clone().unwrap();
        assert_eq!(result.apply_error(Some(1)), Ok(()));

        // The explicit Apply path marks the exact result once the live row
        // confirms it, so a repeated A cannot write again.
        builder.mark_probe_result_applied(&result);
        let after = poll_probe(&mut builder, &status);
        let current = after.probe.last_result.clone().unwrap();
        assert!(current.applied);
        assert!(current.apply_error(Some(1)).is_err());
        assert_eq!(after.probe.history.len(), 1);
        assert!(after.probe.history[0].applied);
    }

    #[test]
    fn calibration_requires_the_final_controller_message() {
        let mut builder = probe_builder();
        let operation = linc_core::probe::operation("cal_round_boss").unwrap();
        let mut parameters = ProbeParameters::default();
        parameters.calibration_diameter = 50.0;
        parameters.square_cal_axis = 0;
        builder.note_probe_block(
            Some(parameters.mdi_text(operation)),
            ProbeProvenance::default(),
        );

        // A stub that reports the initial clear-to-zero and every measurement
        // slot but never the controller's final #3032.
        builder.push_probe_value(3032, 0.0);
        push_calibration_slots(&mut builder, operation);

        let status = idle_status();
        let unconfirmed = poll_probe(&mut builder, &status);
        assert!(unconfirmed.probe.last_result.is_none());
        assert!(unconfirmed.probe.history.is_empty());
        assert!(unconfirmed
            .probe
            .error
            .as_deref()
            .unwrap()
            .contains("#3032"));
        // Nothing is synthesized in its place, and the intermediate clear is
        // not published as the calibration value.
        assert_eq!(unconfirmed.probe.calibration_offset, None);
        assert!(!unconfirmed.probe.running);
    }

    #[test]
    fn calibration_publishes_the_controller_value() {
        let mut builder = probe_builder();
        let operation = linc_core::probe::operation("cal_round_boss").unwrap();
        let mut parameters = ProbeParameters::default();
        parameters.calibration_diameter = 50.0;
        parameters.square_cal_axis = 0;
        builder.note_probe_block(
            Some(parameters.mdi_text(operation)),
            ProbeProvenance::default(),
        );
        builder.push_probe_value(3032, 0.0);
        push_calibration_slots(&mut builder, operation);
        // The final message arrives after every measurement slot. The local
        // recomputation for these widths would be 0.015.
        builder.push_probe_value(3032, 0.031);

        let status = idle_status();
        let completed = poll_probe(&mut builder, &status);
        let result = completed.probe.last_result.as_ref().unwrap();
        assert_eq!(result.updated, (1001..=1010).collect::<Vec<_>>());
        assert_eq!(result.error, Some(0.031));
        assert_eq!(completed.probe.calibration_offset, Some(0.031));
        assert!(!result.applied);
        assert!(operation.apply_terms().is_empty());
        assert!(result.measured.is_empty());

        // A later controller message is still authoritative outside a run.
        builder.push_probe_value(3032, 0.025);
        let authoritative = poll_probe(&mut builder, &status);
        assert_eq!(authoritative.probe.calibration_offset, Some(0.025));
    }

    #[test]
    fn unconfirmed_reset_keeps_the_controller_value() {
        let mut builder = probe_builder();
        builder.push_probe_value(3032, 0.02);
        let status = idle_status();
        assert_eq!(
            poll_probe(&mut builder, &status).probe.calibration_offset,
            Some(0.02)
        );

        // The reset is issued; no #3032 message has arrived yet.
        builder.note_probe_block(
            Some("o<probe_cal_reset> call".into()),
            ProbeProvenance::default(),
        );
        let waiting = poll_probe(&mut builder, &status);
        assert_eq!(waiting.probe.calibration_offset, Some(0.02));
        assert!(waiting.probe.last_result.is_none());

        builder.push_probe_value(3032, 0.0);
        let reset = poll_probe(&mut builder, &status);
        assert_eq!(reset.probe.calibration_offset, Some(0.0));
        assert!(reset.probe.last_result.is_none());
        assert!(!reset.probe.running);
    }
}
