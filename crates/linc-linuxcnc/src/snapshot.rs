//! Build the typed `MachineSnapshot` from an NML status poll and HAL reads.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use linuxcnc_nml::{
    ExecState, MessageKind, Status, TaskMode as NmlTaskMode, TaskState as NmlTaskState,
};

use linc_core::config::Profile;
use linc_core::model::*;
use linc_core::tooltable::ToolTable;

use crate::panel::PanelState;
use crate::parameters::ParameterFile;

const MAX_TRAIL_POINTS: usize = 200_000;

pub struct SnapshotBuilder {
    pub(crate) profile: Profile,
    pub(crate) tool_table: ToolTable,
    tool_table_checked: Instant,
    program_text: Option<Arc<String>>,
    program_path: Option<PathBuf>,
    program_mtime: Option<std::time::SystemTime>,
    program_revision: u64,
    program_loaded_at_ms: u64,
    live_trail: Vec<[f64; 3]>,
    last_tool_tip: Option<[f64; 3]>,
    next_alarm_id: u64,
    last_alarm_texts: VecDeque<String>,
    pub(crate) alarms: VecDeque<Alarm>,
    last_probe_signature: Option<String>,
    probe_block: Option<String>,
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
            last_probe_signature: None,
            probe_block: None,
            warned_axis_mismatch: false,
            tool_change_serial: 0,
            tool_change_active: false,
            tool_confirm: false,
            calibration_offset: None,
            offsets_last: None,
            offsets_revision: 1,
        }
    }

    pub fn note_probe_block(&mut self, block: Option<String>) {
        self.probe_block = block;
    }

    pub fn confirm_tool_change(&mut self) {
        self.tool_confirm = true;
    }

    pub fn cancel_tool_confirm(&mut self) {
        self.tool_confirm = false;
    }

    pub fn set_calibration_offset(&mut self, offset: Option<f64>) {
        self.calibration_offset = offset;
    }

    pub fn clear_trail(&mut self) {
        let tip = self.last_tool_tip.unwrap_or([0.0; 3]);
        self.live_trail.clear();
        self.live_trail.push(tip);
        self.last_tool_tip = Some(tip);
    }

    pub fn push_message(&mut self, kind: MessageKind, text: &str) {
        let message = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if message.is_empty() {
            return;
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
        snapshot.machine.estop = status.io.estop || panel.estop;
        snapshot.machine.fault = status.io.fault || panel.estop;
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
                pose_component(&status.positions.commanded, index),
            );
            snapshot.machine.actual.insert(
                axis.name.clone(),
                pose_component(&status.positions.actual, index),
            );
        }
        snapshot.machine.limits = limit_labels(status, axes);

        let g5x = status.offsets.g5x_offset;
        let g92 = status.offsets.g92_offset;
        let tool_offset = status.offsets.tool_offset;
        let dtg = status.positions.dtg;
        snapshot.positions.axes = AxisMap::new();
        for (index, axis) in axes.iter().enumerate() {
            let machine = pose_component(&status.positions.actual, index);
            let work = machine
                - pose_component(&g5x, index)
                - pose_component(&g92, index)
                - pose_component(&tool_offset, index);
            snapshot.positions.axes.insert(
                axis.name.clone(),
                PositionSet {
                    work,
                    machine,
                    dtg: pose_component(&dtg, index),
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
                        values.insert(axis.name.clone(), pose_component(&g5x, axis_index));
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
                values.insert(axis.name.clone(), pose_component(&g92, index));
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
            self.live_trail.push(tip);
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
        snapshot.probe.enabled = self.profile.config.probe.enabled;
        snapshot.probe.tool_number = self.profile.config.probe.tool_number;
        snapshot.probe.selected_wcs = self.profile.config.probe.wcs.clone();
        snapshot.probe.running = status.motion.probing;
        snapshot.probe.calibration_offset = self.calibration_offset;

        // The interpreter does not expose numbered parameters over NML. The
        // parameter file is the only readable copy, and LinuxCNC writes it on
        // interpreter exit; a probe that runs to M2 is therefore visible on
        // the next poll only when the controller has written the file.
        let mut values = [0.0; 9];
        for (index, key) in (1001..=1009).enumerate() {
            values[index] = parameters.get(key);
        }
        let signature = format!("{values:?}");
        if self.last_probe_signature.as_deref() != Some(signature.as_str())
            && (status.motion.probe_tripped || values.iter().any(|value| *value != 0.0))
        {
            let results = linc_core::probe::ParameterResults::from_values(&values);
            let operation = self
                .probe_block
                .as_deref()
                .and_then(|block| {
                    linc_core::probe::operations()
                        .iter()
                        .find(|operation| block.contains(operation.call))
                })
                .or_else(|| {
                    snapshot.probe.last_routine.as_deref().and_then(|id| {
                        linc_core::probe::operations()
                            .iter()
                            .find(|operation| operation.id == id)
                    })
                });
            let family = operation
                .map(|operation| operation.result_family())
                .unwrap_or_default();
            let mut measured = AxisMap::new();
            if let Some(operation) = operation {
                for (axis, value) in results.wcs_offsets(operation) {
                    measured.insert(axis, value);
                }
            }
            let error = operation.and_then(|operation| {
                results.error(operation, Some(self.profile.config.probe.probe_diameter))
            });
            let routine = operation
                .map(|operation| operation.name.to_string())
                .unwrap_or_else(|| "probe result".into());
            let result = ProbeResult {
                routine,
                measured,
                error,
                wcs: snapshot
                    .offsets
                    .active_index
                    .map(linc_core::model::wcs_name)
                    .unwrap_or_else(|| "--".into()),
                mode: "measured".into(),
                timestamp_ms: now_ms(),
                values: values.to_vec(),
                family,
            };
            snapshot.probe.last_result = Some(result.clone());
            snapshot.probe.history.push_front(result);
            while snapshot.probe.history.len() > 32 {
                snapshot.probe.history.pop_back();
            }
            self.last_probe_signature = Some(signature);
        }
    }
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
    fn jog_selections_are_carried_across_a_poll() {
        let mut stored = MachineSnapshot::empty(MachineVariant::Mill);
        stored.hal.jog_speed = Some(42.0);
        stored.hal.jog_increment = Some(0.5);
        let mut converted = MachineSnapshot::empty(MachineVariant::Mill);
        carry_hal_selections(&mut converted, &stored);
        assert_eq!(converted.hal.jog_speed, Some(42.0));
        assert_eq!(converted.hal.jog_increment, Some(0.5));
    }
}
