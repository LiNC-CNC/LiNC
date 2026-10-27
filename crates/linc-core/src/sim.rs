//! Standalone simulation backend.
//!
//! This is not the real machine backend and is only used when LinuxCNC is not
//! running (for development, UI tests and `linc --sim`). It follows the same
//! command validation and snapshot contract as the real bridge.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::backend::MachineBackend;
use crate::command::{HalValue, LincOutput, MachineCommand, MachineError, MdiOrigin, ProbeTarget};
use crate::config::Profile;
use crate::gcode::{parse_preview, SegmentKind};
use crate::model::*;
use crate::tooltable::ToolTable;

const DEMO_MILL: &str = include_str!("../assets/demo_mill.ngc");
const DEMO_LATHE: &str = include_str!("../assets/demo_lathe.ngc");

pub struct SimBackend {
    profile: Profile,
    snapshot: MachineSnapshot,
    tool_table: ToolTable,
    next_alarm: u64,
    trail_limit: usize,
    demo_text: String,
    program_revision: u64,
    program_path: Option<PathBuf>,
    last_probe: Option<ProbeResult>,
    panel_ipc: Option<crate::sim_ipc::SimPanelIpc>,
    panel_seen: Instant,
}

impl SimBackend {
    pub fn new(profile: Profile, initial_mode: PanelMode) -> Self {
        Self::new_with_ipc(profile, initial_mode, None)
    }

    /// Simulation HMI connected to the companion panel process through the
    /// local runtime state bridge. Tests use `new` and stay isolated.
    pub fn new_connected(profile: Profile, initial_mode: PanelMode) -> Self {
        Self::new_with_ipc(
            profile,
            initial_mode,
            Some(crate::sim_ipc::SimPanelIpc::from_env()),
        )
    }

    fn new_with_ipc(
        profile: Profile,
        initial_mode: PanelMode,
        panel_ipc: Option<crate::sim_ipc::SimPanelIpc>,
    ) -> Self {
        let ini = &profile.machine_ini;
        let mut snapshot = MachineSnapshot::empty(profile.variant);
        snapshot.machine_name = profile.config.machine.name.clone();
        snapshot.simulated = profile.is_sim();

        for axis in &ini.axes {
            snapshot.positions.axes.insert(
                axis.name.clone(),
                PositionSet {
                    work: 0.0,
                    machine: 0.0,
                    dtg: 0.0,
                    velocity: 0.0,
                },
            );
            snapshot.machine.homed.insert(axis.name.clone(), true);
            snapshot.machine.homing.insert(axis.name.clone(), false);
            snapshot
                .machine
                .following_error
                .insert(axis.name.clone(), 0.0);
            snapshot.machine.servo_load.insert(
                axis.name.clone(),
                match axis.name.as_str() {
                    "X" => 32.0,
                    "Y" => 18.0,
                    "Z" => 12.0,
                    "C" => 4.0,
                    _ => 2.0,
                },
            );
            snapshot.machine.commanded.insert(axis.name.clone(), 0.0);
            snapshot.machine.actual.insert(axis.name.clone(), 0.0);
        }
        snapshot.positions.units = ini.linear_units.clone();
        snapshot.positions.angular_units = ini.angular_units.clone();
        snapshot.machine.homing_sequence = ini
            .home_order()
            .iter()
            .map(|axis| axis.home_sequence)
            .collect();
        snapshot.machine.force_homing = ini.no_force_homing;
        snapshot.machine.on = true;
        snapshot.machine.estop = false;
        snapshot.task.power = MachinePower::On;
        snapshot.task.interp = InterpState::Idle;
        snapshot.task.mode = task_mode_for(initial_mode);

        // Work offsets G54…G59.3: nine systems, as LinuxCNC numbers them.
        for index in 1..=9 {
            let mut values = AxisMap::new();
            for axis in &ini.axes {
                values.insert(axis.name.clone(), 0.0);
            }
            snapshot.offsets.offsets.push(WorkOffset {
                index,
                values,
                rotation: Some(0.0),
            });
        }
        snapshot.offsets.active_index = Some(1);
        for axis in &ini.axes {
            snapshot.offsets.g92.insert(axis.name.clone(), 0.0);
        }

        // Spindles.
        for index in 0..ini.spindles {
            let live = profile.variant == MachineVariant::Lathe && index > 0;
            snapshot.spindles.push(SpindleState {
                index,
                name: if live {
                    "LIVE TOOL".into()
                } else if index == 0 {
                    "MAIN SPINDLE".into()
                } else {
                    format!("SPINDLE {index}")
                },
                commanded_speed: if live { 1200.0 } else { 4200.0 },
                actual_speed: if live { 1180.0 } else { 4185.0 },
                load: Some(if live { 21.0 } else { 38.0 }),
                override_factor: 1.0,
                direction: 1,
                on: true,
                live_tool: live,
                live_tool_axial: live,
                live_tool_max_rpm: profile
                    .config
                    .lathe
                    .as_ref()
                    .and_then(|lathe| lathe.live_tool_max_rpm)
                    .unwrap_or(6000.0),
            });
        }
        snapshot.feed.feed = 850.0;
        snapshot.feed.units = ini.linear_units.clone();
        snapshot.feed.rapid_override = 1.0;
        snapshot.feed.spindle_override = 1.0;
        snapshot.feed.feed_override = 1.0;

        // Panel.
        snapshot.panel.connected = true;
        snapshot.panel.communication_error = false;
        snapshot.panel.mode = PanelModeValidity::Valid(initial_mode);
        snapshot.panel.feed_override = 1.0;
        snapshot.panel.rapid_override = 1.0;
        snapshot.panel.spindle_override = 1.0;
        snapshot.panel.program_protect = false;
        snapshot.panel.program_protect_known = true;
        snapshot.panel.estop = false;
        snapshot.panel.sseg_tool = 1;
        snapshot.panel.sseg_next_tool = 0;
        snapshot.panel.sseg_status = 54;
        snapshot.panel.link = PanelLinkStats {
            port: profile
                .config
                .panel
                .link
                .port
                .clone()
                .unwrap_or_else(|| "virtual".into()),
            baud: profile.config.panel.link.baud.unwrap_or(115200),
            cycle_time_ms: profile.config.panel.link.cycle_time_ms.unwrap_or(20) as f64,
            available: profile.config.diagnostics.panel_link_stats != "none",
            ..Default::default()
        };
        snapshot.panel.capabilities = PanelCapabilities {
            transport: profile
                .config
                .panel
                .link
                .kind
                .clone()
                .unwrap_or_else(|| "virtual".into()),
            ..Default::default()
        };
        snapshot.panel.jog = JogState {
            mode: initial_mode.label().into(),
            active_axis: Some("X".into()),
            speed: 250.0,
            rapid: false,
            increment: Some(1.0),
            remaining_step: None,
            handwheel_scale: Some(0.01),
        };
        snapshot.pendant = PendantState {
            enabled: profile.config.pendant.enabled,
            connected: profile.config.pendant.enabled,
            active_axis: Some("X".into()),
            handwheel_scale: Some(0.01),
            step_mode: Some("x1".into()),
            ..Default::default()
        };

        // Tool table.
        let (tool_table_state, tool_table, tool_table_error) =
            match ToolTable::load(&ini.tool_table, &ini.tool_table_columns, profile.changer) {
                Ok(table) => (table.state(), Some(table), None),
                Err(error) => {
                    let mut state = ToolTableState {
                        path: ini.tool_table.clone(),
                        columns: ini
                            .tool_table_columns
                            .chars()
                            .filter_map(crate::model::ToolColumn::parse)
                            .collect(),
                        changer: profile.changer,
                        ..Default::default()
                    };
                    state.error = Some(error.to_string());
                    (state, None, Some(error.to_string()))
                }
            };
        snapshot.tool_table = tool_table_state;
        let tool_table = tool_table.unwrap_or_else(|| ToolTable {
            path: ini.tool_table.clone(),
            columns: snapshot.tool_table.columns.clone(),
            rows: Vec::new(),
            revision: 0,
            changer: profile.changer,
        });
        let initial_tool = if profile.variant == MachineVariant::Lathe {
            4
        } else {
            1
        };
        snapshot.tool_change.current_tool = initial_tool;
        snapshot.tool_change.prepared_tool = 0;
        snapshot.panel.current_tool = initial_tool;
        snapshot.panel.next_tool = 0;
        snapshot.panel.sseg_tool = initial_tool as i32;

        // Probe state.
        snapshot.probe = ProbeState {
            enabled: profile.config.probe.enabled,
            tool_number: profile.config.probe.tool_number,
            selected_wcs: profile.config.probe.wcs.clone(),
            calibration_offset: Some(0.0),
            calibration_diameter: Some(profile.config.probe.calibration_diameter),
            input_tripped: Some(false),
            ..Default::default()
        };

        // Lathe ATC.
        if profile.variant == MachineVariant::Lathe {
            snapshot.atc = Some(AtcState {
                pockets: profile.turret_stations().unwrap_or(12),
                current_pocket: Some(4),
                prepared_pocket: None,
                clamped: Some(true),
                indexing: false,
                station: Some(4),
                live_tool_rpm: Some(1180.0),
                live_tool_direction: 1,
                live_tool_axial: Some(true),
                fault: None,
            });
        } else if profile.changer.uses_pockets() {
            snapshot.atc = Some(AtcState {
                pockets: ini.atc_pockets.unwrap_or(12),
                current_pocket: Some(0),
                prepared_pocket: None,
                clamped: Some(true),
                indexing: false,
                station: None,
                live_tool_rpm: None,
                live_tool_direction: 0,
                live_tool_axial: None,
                fault: None,
            });
        }

        let demo_text = if profile.variant == MachineVariant::Lathe {
            DEMO_LATHE.to_string()
        } else {
            DEMO_MILL.to_string()
        };
        let mut backend = Self {
            profile,
            snapshot,
            tool_table,
            next_alarm: 1,
            trail_limit: 100_000,
            demo_text,
            program_revision: 1,
            program_path: None,
            last_probe: None,
            panel_ipc,
            panel_seen: Instant::now(),
        };
        backend.trail_limit = backend.profile.config.toolpath.trail_max_points;
        backend.initialize_program();
        backend.add_alarm(
            AlarmSeverity::Info,
            AlarmSource::Panel,
            "P-ONLINE",
            "Panel link established",
        );
        if let Some(error) = tool_table_error {
            backend.add_alarm(
                AlarmSeverity::Warning,
                AlarmSource::Tool,
                "T-LOAD",
                format!("Tool table: {error}"),
            );
        }
        backend.add_alarm(
            AlarmSeverity::Info,
            AlarmSource::Program,
            "P-READY",
            "Demonstration program loaded",
        );
        backend
    }

    pub fn profile(&self) -> &Profile {
        &self.profile
    }

    fn initialize_program(&mut self) {
        let text = self.demo_text.clone();
        let display_name = if self.profile.variant == MachineVariant::Lathe {
            "DEMO-LATHE.NC"
        } else {
            "DEMO.NC"
        };
        self.apply_program(None, display_name, text, 1);
    }

    fn apply_program(
        &mut self,
        path: Option<PathBuf>,
        display_name: impl Into<String>,
        text: String,
        start_line: usize,
    ) {
        let line_count = text.lines().count().max(1);
        self.program_revision += 1;
        let text = Arc::new(text);
        self.snapshot.program = ProgramState {
            path: path.clone(),
            display_name: display_name.into(),
            revision: self.program_revision,
            line_count,
            current_line: start_line.min(line_count),
            motion_line: 0,
            next_line: None,
            current_block: text
                .lines()
                .nth(start_line.saturating_sub(1))
                .unwrap_or_default()
                .to_string(),
            modal: Vec::new(),
            running: false,
            paused: false,
            dirty: false,
            syntax_error: None,
            progress: 0.0,
            loaded_at_ms: now_ms(),
            content: Some(text),
        };
        self.program_path = path;
        self.update_program_preview();
        self.seed_trail_from_program();
    }

    fn update_program_preview(&mut self) {
        let Some(content) = self.snapshot.program.content.clone() else {
            return;
        };
        let axis_names: Vec<String> = self
            .snapshot
            .positions
            .axes
            .keys()
            .map(str::to_string)
            .collect();
        let active_offset = self.snapshot.offsets.active_index.and_then(|index| {
            self.snapshot
                .offsets
                .offsets
                .iter()
                .find(|o| o.index == index)
        });
        let preview = parse_preview(
            content.as_str(),
            &axis_names,
            active_offset.map(|offset| &offset.values),
            self.current_tool_tip(),
        );
        self.snapshot.program.syntax_error = preview.error.clone();
        self.snapshot.program.modal = preview.modal.lines();
        self.snapshot.program.current_block = self
            .snapshot
            .program
            .content
            .as_ref()
            .and_then(|text| {
                text.lines()
                    .nth(self.snapshot.program.current_line.saturating_sub(1))
            })
            .unwrap_or_default()
            .to_string();
        self.snapshot.program.next_line = self
            .snapshot
            .program
            .content
            .as_ref()
            .and_then(|text| text.lines().nth(self.snapshot.program.current_line))
            .map(str::to_string);
    }

    fn current_tool_tip(&self) -> [f64; 3] {
        let get = |axis: &str| {
            self.snapshot
                .positions
                .axes
                .get(axis)
                .map(|position| position.work)
                .unwrap_or(0.0)
        };
        [get("X"), get("Y"), get("Z")]
    }

    fn seed_trail_from_program(&mut self) {
        let Some(_) = self.snapshot.program.content.as_ref() else {
            return;
        };
        let axis_names: Vec<String> = self
            .snapshot
            .positions
            .axes
            .keys()
            .map(str::to_string)
            .collect();
        let active_offset = self.snapshot.offsets.active_index.and_then(|index| {
            self.snapshot
                .offsets
                .offsets
                .iter()
                .find(|o| o.index == index)
        });
        let preview = parse_preview(
            self.snapshot
                .program
                .content
                .as_ref()
                .map(|text| text.as_str())
                .unwrap_or_default(),
            &axis_names,
            active_offset.map(|offset| &offset.values),
            self.current_tool_tip(),
        );
        let limit = self.snapshot.program.current_line.max(1);
        let mut points: Vec<TrailPoint> = Vec::new();
        for segment in preview
            .segments
            .iter()
            .filter(|segment| segment.line <= limit)
        {
            let rapid = segment.kind == SegmentKind::Rapid;
            if points.last().map(|point| point.position) != Some(segment.start) {
                points.push(TrailPoint::new(segment.start, rapid));
            }
            points.push(TrailPoint::new(segment.end, rapid));
        }
        if points.is_empty() {
            points.push(TrailPoint::new(self.current_tool_tip(), false));
        }
        let max = self.trail_limit.max(2);
        if points.len() > max {
            points = points.split_off(points.len() - max);
        }
        self.snapshot.tool_tip = points.last().map(|point| point.position);
        self.snapshot.live_trail = Arc::new(points);
    }

    /// Write a probe result into a work offset row.
    ///
    /// `axes` narrows the write to the axes a DIRECT branch actually touches
    /// (`crate::probe::direct_written_axes`); `None` writes every apply term,
    /// which is what an explicit Apply the operator asked for does.
    fn write_probe_raw(
        &mut self,
        result: &ProbeResult,
        index: u8,
        axes: Option<&[&'static str]>,
    ) -> Result<(), MachineError> {
        let operation = crate::probe::operation(&result.operation)
            .ok_or_else(|| MachineError::Invalid("unknown probe operation".into()))?;
        let results = crate::probe::ParameterResults::from_values(&result.values);
        let raw = results
            .raw_offsets(operation, &result.source_offsets, &result.auxiliary)
            .map_err(MachineError::Invalid)?;
        let raw = match axes {
            Some(axes) => {
                let mut kept = AxisMap::new();
                for (axis, value) in raw.iter() {
                    if axes.contains(&axis) {
                        kept.insert(axis, *value);
                    }
                }
                kept
            }
            None => raw,
        };
        for (axis, value) in raw.iter() {
            self.profile
                .machine_ini
                .validate_axis_value(axis, *value)
                .map_err(MachineError::Invalid)?;
        }
        let offset = self
            .snapshot
            .offsets
            .offsets
            .iter_mut()
            .find(|offset| offset.index == index)
            .ok_or_else(|| MachineError::Invalid("unknown work offset".into()))?;
        for (axis, value) in raw.iter() {
            offset.values.insert(axis.to_string(), *value);
        }
        if let Some(rotation) = result.rotation {
            offset.rotation = Some(rotation);
        }
        self.snapshot.offsets.revision += 1;
        Ok(())
    }

    fn add_alarm(
        &mut self,
        severity: AlarmSeverity,
        source: AlarmSource,
        code: &str,
        message: impl Into<String>,
    ) {
        let alarm = Alarm::new(self.next_alarm, severity, source, code, message);
        self.next_alarm += 1;
        self.snapshot.alarms.push_front(alarm);
        while self.snapshot.alarms.len() > MAX_EVENT_LOG {
            self.snapshot.alarms.pop_back();
        }
    }

    fn append_trail(&mut self, point: [f64; 3], rapid: bool) {
        let mut trail = (*self.snapshot.live_trail).clone();
        if trail.last().map(|last| last.position) != Some(point) {
            trail.push(TrailPoint::new(point, rapid));
            while trail.len() > self.trail_limit.max(2) {
                trail.remove(0);
            }
        }
        self.snapshot.tool_tip = Some(point);
        self.snapshot.live_trail = Arc::new(trail);
    }

    /// Demo/testing hook. In production the panel service owns the mode.
    pub fn set_panel_mode(&mut self, mode: PanelMode) {
        self.snapshot.panel.mode = PanelModeValidity::Valid(mode);
        self.snapshot.panel.jog.mode = mode.label().into();
        self.snapshot.task.mode = task_mode_for(mode);
        if mode != PanelMode::Mdi {
            self.snapshot.task.interp = InterpState::Idle;
        }
    }

    pub fn set_program_protect(&mut self, on: bool, known: bool) {
        self.snapshot.panel.program_protect = on;
        self.snapshot.panel.program_protect_known = known;
    }

    pub fn set_panel_connected(&mut self, connected: bool) {
        self.snapshot.panel.connected = connected;
        self.snapshot.panel.communication_error = !connected;
    }

    pub fn set_power(&mut self, power: MachinePower) {
        self.snapshot.task.power = power;
        self.snapshot.machine.on = power == MachinePower::On;
        self.snapshot.machine.estop =
            matches!(power, MachinePower::Estop | MachinePower::EstopReset);
    }

    pub fn clear_trail(&mut self) {
        let tip = self.current_tool_tip();
        self.snapshot.live_trail = Arc::new(vec![TrailPoint::new(tip, false)]);
        self.snapshot.tool_tip = Some(tip);
    }

    fn sync_panel(&mut self) {
        let Some(mut ipc) = self.panel_ipc.take() else {
            return;
        };
        let state = ipc.read_state();
        let mut received = false;
        if let Some(panel) = state {
            received = true;
            self.panel_seen = Instant::now();
            self.apply_panel_state(panel);
        }
        if !received && self.panel_seen.elapsed() > std::time::Duration::from_secs(1) {
            self.mark_panel_offline();
        }
        let display = self.panel_display();
        let _ = ipc.write_display(&display);
        self.panel_ipc = Some(ipc);
    }

    fn apply_panel_state(&mut self, panel: PanelSnapshot) {
        let estop = panel.estop;
        let power_on = panel.power_on;
        self.snapshot.machine.estop = estop;
        self.snapshot.task.power = if estop {
            MachinePower::Estop
        } else {
            match power_on {
                Some(true) => MachinePower::On,
                Some(false) => MachinePower::Off,
                None => self.snapshot.task.power,
            }
        };
        self.snapshot.machine.on = self.snapshot.task.power == MachinePower::On;
        self.snapshot.task.mode = panel
            .mode
            .mode()
            .map(task_mode_for)
            .unwrap_or(TaskMode::Unknown);
        self.snapshot.feed.feed_override = panel.feed_override;
        self.snapshot.feed.rapid_override = panel.rapid_override;
        self.snapshot.feed.spindle_override = panel.spindle_override;
        for spindle in &mut self.snapshot.spindles {
            spindle.override_factor = panel.spindle_override;
            // The simulator has no encoder; the feedback follows the commanded
            // speed through the override, as a connected loopback would.
            spindle.actual_speed = spindle.commanded_speed * panel.spindle_override;
        }
        self.snapshot.panel = panel;
    }

    fn mark_panel_offline(&mut self) {
        if !self.snapshot.panel.connected {
            return;
        }
        self.snapshot.panel.connected = false;
        self.snapshot.panel.communication_error = true;
        self.snapshot.panel.program_protect_known = false;
        self.snapshot.panel.mode = PanelModeValidity::Unknown;
        self.snapshot.panel.power_on = None;
        self.snapshot.task.mode = TaskMode::Unknown;
    }

    fn panel_display(&self) -> PanelDisplay {
        let status = self
            .snapshot
            .status_display_value(self.profile.status_source)
            .unwrap_or(-1);
        PanelDisplay {
            leds: 0,
            sseg_tool: self.snapshot.tool_change.current_tool as i32,
            sseg_next_tool: self.snapshot.tool_change.prepared_tool as i32,
            sseg_status: status,
            vel: self
                .snapshot
                .positions
                .axes
                .iter()
                .take(3)
                .map(|(_, position)| position.velocity)
                .collect::<Vec<_>>()
                .try_into()
                .unwrap_or([0.0; 3]),
            homing: self
                .snapshot
                .machine
                .homing
                .iter()
                .any(|(_, homing)| *homing),
        }
    }

    fn require_ready(&self) -> Result<(), MachineError> {
        if !self.snapshot.panel.connected || self.snapshot.panel.communication_error {
            return Err(MachineError::PanelOffline);
        }
        if !self.snapshot.panel.mode.is_valid() {
            return Err(MachineError::InvalidPanelMode);
        }
        if self.snapshot.task.power != MachinePower::On {
            return Err(MachineError::MachineOff);
        }
        if self.snapshot.machine.homed.is_empty()
            || !self.snapshot.machine.homed.iter().all(|(_, homed)| *homed)
        {
            return Err(MachineError::NotHomed);
        }
        if self.snapshot.task.running {
            return Err(MachineError::ProgramRunning);
        }
        if self.snapshot.task.interp != InterpState::Idle {
            return Err(MachineError::NotIdle);
        }
        if self.snapshot.probe_trip_active() {
            return Err(MachineError::ProbeTripped);
        }
        Ok(())
    }

    fn require_issue(&self, origin: MdiOrigin) -> Result<(), MachineError> {
        self.require_ready()?;
        if origin == MdiOrigin::Mdi && self.snapshot.panel.mode.mode() != Some(PanelMode::Mdi) {
            return Err(MachineError::WrongPanelMode);
        }
        Ok(())
    }

    fn apply_mdi_line(&mut self, line: &str) -> Result<(), MachineError> {
        let upper = line.to_ascii_uppercase();
        // Tool change.
        if let Some(position) = upper.find("M6") {
            let prefix = &upper[..position];
            if let Some(t_index) = prefix.find('T') {
                let digits: String = prefix[t_index + 1..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                if let Ok(tool) = digits.parse::<u16>() {
                    self.complete_tool_change(tool);
                }
            }
        }
        if let Some(index) = upper.find("M61") {
            let digits: String = upper[index + 3..]
                .chars()
                .skip_while(|ch| *ch != 'Q')
                .skip(1)
                .take_while(char::is_ascii_digit)
                .collect();
            if let Ok(tool) = digits.parse::<u16>() {
                self.snapshot.tool_change.current_tool = tool;
                self.snapshot.panel.current_tool = tool;
            }
        }
        // G10 L2 Pn X.. Y.. Z.. R..
        if upper.contains("G10") && upper.contains("L2") {
            if let Some(index) = upper.find('P') {
                let digits: String = upper[index + 1..]
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect();
                if let Ok(index) = digits.parse::<u8>() {
                    let mut values = AxisMap::new();
                    for axis in self.snapshot.positions.axes.keys() {
                        let letter = axis.chars().next().unwrap_or('X').to_ascii_uppercase();
                        if let Some(value) = word_number(&upper, letter) {
                            values.insert(axis.to_string(), value);
                        }
                    }
                    if let Some(offset) = self
                        .snapshot
                        .offsets
                        .offsets
                        .iter_mut()
                        .find(|o| o.index == index)
                    {
                        for (axis, value) in values.iter() {
                            offset.values.insert(axis, *value);
                        }
                        self.snapshot.offsets.revision += 1;
                    }
                }
            }
        }
        // Motion: update displayed position and the live trail from the
        // bounded preview of the block. This is a simulation, not a motion
        // controller.
        let axis_names: Vec<String> = self
            .snapshot
            .positions
            .axes
            .keys()
            .map(str::to_string)
            .collect();
        // The active work system applies here as well: parse_preview returns
        // machine coordinates (program words + the WCS row), which is the frame
        // the seeded trail is already in. Stamping the program value into both
        // position fields left a trail mixing two frames whenever a WCS was set.
        let active_offset = self.snapshot.offsets.active_index.and_then(|index| {
            self.snapshot
                .offsets
                .offsets
                .iter()
                .find(|o| o.index == index)
        });
        let preview = parse_preview(
            line,
            &axis_names,
            active_offset.map(|offset| &offset.values),
            self.current_tool_tip(),
        );
        if let Some(segment) = preview.segments.last() {
            let start = segment.start;
            let end = segment.end;
            for (axis, index) in [("X", 0usize), ("Y", 1), ("Z", 2)] {
                let offset = active_offset
                    .and_then(|offset| offset.values.get(axis).copied())
                    .unwrap_or(0.0);
                if let Some(position) = self.snapshot.positions.axes.get_mut(axis) {
                    position.machine = end[index];
                    position.work = end[index] - offset;
                    position.dtg = 0.0;
                }
                if let Some(commanded) = self.snapshot.machine.commanded.get_mut(axis) {
                    *commanded = end[index];
                }
                if let Some(actual) = self.snapshot.machine.actual.get_mut(axis) {
                    *actual = end[index];
                }
            }
            // Simple straight-line trail sampling keeps the graph lively.
            let rapid = segment.kind == SegmentKind::Rapid;
            let steps = if rapid { 3 } else { 12 };
            for step in 1..=steps {
                let t = step as f64 / steps as f64;
                let point = [
                    start[0] + (end[0] - start[0]) * t,
                    start[1] + (end[1] - start[1]) * t,
                    start[2] + (end[2] - start[2]) * t,
                ];
                self.append_trail(point, rapid);
            }
        }
        if upper.contains("M30") || upper.contains("M2") {
            self.snapshot.task.running = false;
            self.snapshot.program.running = false;
        }
        Ok(())
    }

    fn complete_tool_change(&mut self, tool: u16) {
        self.snapshot.tool_change.status = ToolChangeStatus::Issuing;
        self.snapshot.tool_change.target_tool = Some(tool);
        self.snapshot.tool_change.prepared_tool = tool;
        self.snapshot.panel.next_tool = tool;
        self.snapshot.tool_change.current_tool = tool;
        self.snapshot.panel.current_tool = tool;
        self.snapshot.tool_change.message = format!("Tool T{tool} changed");
        self.snapshot.tool_change.status = ToolChangeStatus::Complete;
    }
}

fn task_mode_for(mode: PanelMode) -> TaskMode {
    match mode {
        PanelMode::Auto | PanelMode::Edit => TaskMode::Auto,
        PanelMode::Mdi => TaskMode::Mdi,
        PanelMode::Jog
        | PanelMode::Inc
        | PanelMode::Mpg
        | PanelMode::Home
        | PanelMode::ProgHome => TaskMode::Manual,
    }
}

fn word_number(text: &str, letter: char) -> Option<f64> {
    let upper = text.to_ascii_uppercase();
    let index = upper.find(letter)?;
    let rest = &upper[index + 1..];
    let end = rest
        .find(|ch: char| !(ch.is_ascii_digit() || ch == '.' || ch == '-' || ch == '+'))
        .unwrap_or(rest.len());
    rest[..end].parse::<f64>().ok()
}

/// Select the routine that the MDI block really calls. The catalogue holds both
/// `probe_round_pocket` and `probe_round_pocket_center_start`, so substring
/// containment resolves a centre-start call to the shorter routine; only an
/// exact `o<ID> call` token may select an operation. The native transaction
/// parser (`linc-linuxcnc`'s `snapshot::probe_operation`) keeps its own copy of
/// this match because it cannot depend on the simulator.
fn probe_operation_for(block: &str) -> Option<&'static crate::probe::ProbeOperation> {
    crate::probe::operation_for_mdi(block)
}

/// True when one block line is the subroutine call `o<call> call`, optionally
/// followed by bracket arguments such as `[#3017]`. Whitespace around the
/// `o`, the name and the keyword is tolerated; a line that merely mentions the
/// name is not a call.
fn canonical_under(root: &Path, candidate: &Path) -> Result<PathBuf, MachineError> {
    let root = root.canonicalize().map_err(|_| MachineError::UnsafePath)?;
    let candidate = candidate
        .canonicalize()
        .map_err(|_| MachineError::UnsafePath)?;
    if !candidate.starts_with(&root) {
        return Err(MachineError::UnsafePath);
    }
    if !candidate.is_file() {
        return Err(MachineError::UnsafePath);
    }
    Ok(candidate)
}

impl MachineBackend for SimBackend {
    fn snapshot(&mut self) -> MachineSnapshot {
        self.sync_panel();
        self.snapshot.clone()
    }

    fn send(&mut self, command: MachineCommand) -> Result<(), MachineError> {
        self.sync_panel();
        match command {
            MachineCommand::ToggleLimitsOverride => {
                self.snapshot.limits_overridden = !self.snapshot.limits_overridden;
                Ok(())
            }
            MachineCommand::IssueMdi { block, origin } => {
                self.require_issue(origin)?;
                if block.trim().is_empty() {
                    return Err(MachineError::Invalid("empty MDI block".into()));
                }
                if matches!(origin, MdiOrigin::Probe) && !self.snapshot.probe.enabled {
                    return Err(MachineError::Invalid("touch probing is disabled".into()));
                }
                let operation = (origin == MdiOrigin::Probe)
                    .then(|| probe_operation_for(&block))
                    .flatten();
                let parameters = (origin == MdiOrigin::Probe)
                    .then(|| crate::probe::ProbeParameters::from_mdi(&block))
                    .flatten();
                let direct = parameters
                    .as_ref()
                    .is_some_and(|params| params.probe_mode == crate::probe::ProbeMode::ApplyWcs);
                // Fail closed on every action that writes controller state,
                // before anything executes. This is the same classifier the
                // native backend and the UI use, so the three cannot drift:
                // measurement-only POSITION runs stay allowed, while DIRECT,
                // Edge Angle, calibration runs (including spindle_nose),
                // the calibration reset, the U parameter update and every
                // result reset require PROTECT off.
                if origin == MdiOrigin::Probe
                    && crate::probe::probe_write_action(&block).is_some()
                    && !self.snapshot.writes_allowed()
                {
                    return Err(MachineError::ProgramProtected);
                }
                let tip = self.current_tool_tip();
                let active_index = self.snapshot.offsets.active_index;
                let source_offsets = active_index
                    .and_then(|index| {
                        self.snapshot
                            .offsets
                            .offsets
                            .iter()
                            .find(|offset| offset.index == index)
                            .map(|offset| offset.values.clone())
                    })
                    .unwrap_or_default();
                // The reference angle routines clear rotation before their
                // measure-only branch, including when no later Apply occurs.
                if operation.is_some_and(|op| op.category == crate::probe::ProbeCategory::EdgeAngle)
                {
                    if let Some(offset) = active_index.and_then(|index| {
                        self.snapshot
                            .offsets
                            .offsets
                            .iter_mut()
                            .find(|offset| offset.index == index)
                    }) {
                        offset.rotation = Some(0.0);
                        self.snapshot.offsets.revision += 1;
                    }
                }
                for line in block.lines().filter(|line| !line.trim().is_empty()) {
                    self.apply_mdi_line(line)?;
                }
                if let Some(operation) = operation {
                    if operation.id == "cal_reset" {
                        self.snapshot.probe.calibration_offset = Some(0.0);
                        self.snapshot.probe.last_result = None;
                        return Ok(());
                    }
                    let defaults = parameters.clone().unwrap_or_default();
                    let round = operation.call.contains("round");
                    let expected_x = match operation.category {
                        crate::probe::ProbeCategory::Calibrate if round => {
                            defaults.calibration_diameter
                        }
                        crate::probe::ProbeCategory::Calibrate => defaults.x_cal_width,
                        crate::probe::ProbeCategory::BossAndPocket if round => {
                            defaults.diameter_hint
                        }
                        crate::probe::ProbeCategory::BossAndPocket => defaults.x_hint_boss_pocket,
                        crate::probe::ProbeCategory::RidgeAndValley => defaults.x_hint_ridge_valley,
                        _ => 20.0,
                    };
                    let expected_y = match operation.category {
                        crate::probe::ProbeCategory::Calibrate if round => {
                            defaults.calibration_diameter
                        }
                        crate::probe::ProbeCategory::Calibrate => defaults.y_cal_width,
                        crate::probe::ProbeCategory::BossAndPocket if round => {
                            defaults.diameter_hint
                        }
                        crate::probe::ProbeCategory::BossAndPocket => defaults.y_hint_boss_pocket,
                        crate::probe::ProbeCategory::RidgeAndValley => defaults.y_hint_ridge_valley,
                        _ => 20.0,
                    };
                    let expected_x = expected_x.max(1.0);
                    let expected_y = expected_y.max(1.0);
                    let calibration = operation.calibration;
                    let width_x = expected_x + if calibration { 0.02 } else { 0.0 };
                    let width_y = expected_y - if calibration { 0.01 } else { 0.0 };
                    let updated = operation.result_parameters();
                    let has_width_x = updated.contains(&1001);
                    let has_width_y = updated.contains(&1003);
                    let mut values = [0.0; 12];
                    for parameter in &updated {
                        let value = match parameter {
                            1001 => width_x,
                            1002 => tip[0],
                            1003 => width_y,
                            1004 => tip[1],
                            1005 => tip[0] - if has_width_x { width_x / 2.0 } else { 0.0 },
                            1006 => tip[0] + if has_width_x { width_x / 2.0 } else { 0.0 },
                            1007 => tip[1] - if has_width_y { width_y / 2.0 } else { 0.0 },
                            1008 => tip[1] + if has_width_y { width_y / 2.0 } else { 0.0 },
                            1009 => tip[2],
                            1010 => (width_x + width_y) / 2.0,
                            1011 | 1012 => 0.0,
                            _ => continue,
                        };
                        values[(parameter - 1001) as usize] = value;
                    }
                    let results = crate::probe::ParameterResults::from_values(&values);
                    let mut auxiliary = std::collections::BTreeMap::new();
                    auxiliary.insert("start_x".into(), tip[0]);
                    auxiliary.insert("start_y".into(), tip[1]);
                    auxiliary.insert("corner_x".into(), tip[0]);
                    auxiliary.insert("corner_y".into(), tip[1]);
                    let rotation = (defaults.wcs_rotation
                        && operation.category == crate::probe::ProbeCategory::EdgeAngle)
                        .then_some(values[11]);
                    let wcs = active_index.map(wcs_name).unwrap_or_else(|| "NONE".into());
                    let mut result = ProbeResult::from_operation(
                        operation,
                        &results,
                        source_offsets,
                        wcs,
                        if direct { "DIRECT WCO" } else { "POSITION" }.into(),
                        updated,
                        auxiliary,
                        rotation,
                        false,
                    );
                    if let Some(offset) = results.calibration_error(operation, &defaults) {
                        result.error = Some(offset);
                        self.snapshot.probe.calibration_offset = Some(offset);
                    }
                    // A DIRECT run writes only the axes its branch's `G10 L2`
                    // names. Publishing a wider row here would rehearse offsets
                    // the machine never touches (`surface_z` writes Z only, the
                    // corner routines leave Z alone, `spindle_nose` writes
                    // nothing at all).
                    let written = crate::probe::direct_written_axes(operation);
                    let touches_row = operation
                        .apply_terms()
                        .iter()
                        .any(|term| written.contains(&term.axis));
                    if direct && touches_row {
                        if let Some(index) = active_index {
                            self.write_probe_raw(&result, index, Some(written))?;
                        }
                        result.applied = true;
                    }
                    self.snapshot.probe.last_result = Some(result.clone());
                    self.snapshot.probe.history.push_front(result.clone());
                    while self.snapshot.probe.history.len() > 20 {
                        self.snapshot.probe.history.pop_back();
                    }
                    self.last_probe = Some(result);
                } else if origin == MdiOrigin::Probe && block.contains("reset") {
                    self.snapshot.probe.last_result = None;
                }
                Ok(())
            }
            MachineCommand::IssueToolChange { tool } => {
                self.require_issue(MdiOrigin::ToolChange)?;
                self.snapshot.tool_change.status = ToolChangeStatus::Issuing;
                self.snapshot.tool_change.target_tool = Some(tool);
                self.snapshot.tool_change.prepared_tool = tool;
                self.snapshot.tool_change.message = format!("Issuing T{tool} M6");
                self.snapshot.panel.next_tool = tool;
                if self.profile.changer == ToolChangerKind::Manual
                    && self.profile.config.tools.manual_confirm
                {
                    self.snapshot.tool_change.status = ToolChangeStatus::ManualPrompt;
                    self.snapshot.tool_change.token = Some(ToolChangeToken(now_ms()));
                    self.snapshot.tool_change.confirm_available = true;
                    self.snapshot.tool_change.message = format!(
                        "REMOVE T{} / INSTALL T{tool}",
                        self.snapshot.tool_change.current_tool
                    );
                } else {
                    self.complete_tool_change(tool);
                }
                Ok(())
            }
            MachineCommand::LoadTool { tool } => {
                self.require_issue(MdiOrigin::ToolChange)?;
                self.snapshot.tool_change.status = ToolChangeStatus::Issuing;
                self.snapshot.tool_change.target_tool = Some(tool);
                self.snapshot.tool_change.prepared_tool = tool;
                self.snapshot.tool_change.message = format!("Loading T{tool} with G43 H{tool}");
                self.snapshot.panel.next_tool = tool;
                // The simulator has no length compensation of its own; the load
                // and the confirmation flow match a plain tool change.
                if self.profile.changer == ToolChangerKind::Manual
                    && self.profile.config.tools.manual_confirm
                {
                    self.snapshot.tool_change.status = ToolChangeStatus::ManualPrompt;
                    self.snapshot.tool_change.token = Some(ToolChangeToken(now_ms()));
                    self.snapshot.tool_change.confirm_available = true;
                    self.snapshot.tool_change.message = format!(
                        "REMOVE T{} / INSTALL T{tool} (G43 H{tool} after)",
                        self.snapshot.tool_change.current_tool
                    );
                } else {
                    self.complete_tool_change(tool);
                }
                Ok(())
            }
            MachineCommand::ConfirmManualToolChange { token } => {
                if !self.snapshot.panel.connected {
                    return Err(MachineError::PanelOffline);
                }
                if !self.snapshot.panel.mode.is_valid() {
                    return Err(MachineError::InvalidPanelMode);
                }
                if self.snapshot.task.interp != InterpState::Idle || self.snapshot.task.running {
                    return Err(MachineError::NotIdle);
                }
                if self.snapshot.tool_change.status != ToolChangeStatus::ManualPrompt {
                    return Err(MachineError::NoPendingToolChange);
                }
                if self.snapshot.tool_change.token.as_ref() != Some(&token) {
                    return Err(MachineError::StaleToolChangeToken);
                }
                let tool = self.snapshot.tool_change.target_tool.unwrap_or(0);
                self.complete_tool_change(tool);
                self.snapshot.tool_change.token = None;
                self.snapshot.tool_change.confirm_available = false;
                self.snapshot.hal.tool_change_confirm_seq += 1;
                Ok(())
            }
            MachineCommand::SetCurrentTool { tool } => {
                if !self.snapshot.panel.connected {
                    return Err(MachineError::PanelOffline);
                }
                if !self.snapshot.panel.mode.is_valid() {
                    return Err(MachineError::InvalidPanelMode);
                }
                if self.snapshot.task.interp != InterpState::Idle || self.snapshot.task.running {
                    return Err(MachineError::NotIdle);
                }
                self.snapshot.tool_change.current_tool = tool;
                self.snapshot.panel.current_tool = tool;
                self.snapshot.tool_change.message =
                    format!("Current tool set to T{tool} (M61, not a tool change)");
                Ok(())
            }
            MachineCommand::LoadProgram {
                path,
                expected_revision,
            } => {
                if self.snapshot.task.running {
                    return Err(MachineError::ProgramRunning);
                }
                if !self.snapshot.panel.connected {
                    return Err(MachineError::PanelOffline);
                }
                if !self.profile.config.files.copy_while_running && self.snapshot.task.running {
                    return Err(MachineError::ProgramRunning);
                }
                if let Some(expected) = expected_revision {
                    if expected != self.snapshot.program.revision {
                        return Err(MachineError::RevisionConflict);
                    }
                }
                let canonical = canonical_under(self.profile.nc_files_root(), &path)?;
                let text = std::fs::read_to_string(&canonical)?;
                let name = canonical
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| canonical.display().to_string());
                self.apply_program(Some(canonical), name, text, 1);
                Ok(())
            }
            MachineCommand::SaveProgram {
                path,
                expected_revision,
                content,
            } => {
                if !self.snapshot.panel.connected {
                    return Err(MachineError::PanelOffline);
                }
                if self.snapshot.panel.protect_effective() {
                    return Err(MachineError::ProgramProtected);
                }
                if self.snapshot.task.running || self.snapshot.task.interp != InterpState::Idle {
                    return Err(MachineError::NotIdle);
                }
                if self.snapshot.panel.mode.mode() != Some(PanelMode::Edit) {
                    return Err(MachineError::WrongPanelMode);
                }
                if expected_revision != self.snapshot.program.revision {
                    return Err(MachineError::RevisionConflict);
                }
                let canonical = canonical_under(self.profile.nc_files_root(), &path)?;
                let parent = canonical.parent().ok_or(MachineError::UnsafePath)?;
                let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
                use std::io::Write as _;
                temporary.write_all(content.as_bytes())?;
                temporary.as_file().sync_all()?;
                temporary
                    .persist(&canonical)
                    .map_err(|error| MachineError::Io(error.error.to_string()))?;
                let name = canonical
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_else(|| canonical.display().to_string());
                self.apply_program(
                    Some(canonical),
                    name,
                    content,
                    self.snapshot.program.current_line,
                );
                Ok(())
            }
            MachineCommand::SetWorkOffset {
                index,
                values,
                rotation,
            } => {
                if !self.snapshot.writes_allowed() {
                    if self.snapshot.panel.protect_effective() {
                        return Err(MachineError::ProgramProtected);
                    }
                    return Err(MachineError::NotIdle);
                }
                if !(1..=9).contains(&index) {
                    return Err(MachineError::Invalid(
                        "work offset must be G54-G59.3".into(),
                    ));
                }
                for (axis, value) in values.iter() {
                    self.profile
                        .machine_ini
                        .validate_axis_value(axis, *value)
                        .map_err(MachineError::Invalid)?;
                }
                if let Some(offset) = self
                    .snapshot
                    .offsets
                    .offsets
                    .iter_mut()
                    .find(|o| o.index == index)
                {
                    offset.values = values;
                    offset.rotation = rotation;
                    self.snapshot.offsets.revision += 1;
                    Ok(())
                } else {
                    Err(MachineError::Invalid("unknown work offset".into()))
                }
            }
            MachineCommand::ActivateWorkOffset { index } => {
                self.require_ready()?;
                if !(1..=9).contains(&index) {
                    return Err(MachineError::Invalid(
                        "work offset must be G54-G59.3".into(),
                    ));
                }
                let Some(values) = self
                    .snapshot
                    .offsets
                    .offsets
                    .iter()
                    .find(|offset| offset.index == index)
                    .map(|offset| offset.values.clone())
                else {
                    return Err(MachineError::Invalid("unknown work offset".into()));
                };
                self.snapshot.offsets.active_index = Some(index);
                self.snapshot.offsets.revision += 1;
                // The simulator keeps the DRO's work values directly. Rebase
                // them from the machine position so the display follows the
                // newly active system, as the native backend's poll does.
                let axes: Vec<String> = self
                    .snapshot
                    .positions
                    .axes
                    .keys()
                    .map(str::to_string)
                    .collect();
                for axis in axes {
                    if let Some(offset) = values.get(&axis) {
                        if let Some(position) = self.snapshot.positions.axes.get_mut(&axis) {
                            position.work = position.machine - offset;
                        }
                    }
                }
                Ok(())
            }
            MachineCommand::RunFromLine { line } => {
                if !self.snapshot.writes_allowed() {
                    if self.snapshot.panel.protect_effective() {
                        return Err(MachineError::ProgramProtected);
                    }
                    return Err(MachineError::NotIdle);
                }
                // The panel must be in AUTO: a line number can only come from
                // the HMI, but whether the machine may run is the panel's call.
                if self.snapshot.panel.mode.mode() != Some(crate::model::PanelMode::Auto) {
                    return Err(MachineError::Invalid("panel mode is not AUTO".into()));
                }
                if self.snapshot.program.line_count == 0 {
                    return Err(MachineError::Invalid("no program is loaded".into()));
                }
                if line == 0 || line as usize > self.snapshot.program.line_count {
                    return Err(MachineError::Invalid(format!(
                        "line {line} is outside the program"
                    )));
                }
                self.snapshot.task.running = true;
                self.snapshot.task.interp = InterpState::Reading;
                self.snapshot.task.mode = TaskMode::Auto;
                self.snapshot.program.current_line = line as usize;
                Ok(())
            }
            MachineCommand::ZeroWorkOffset { index, axes } => {
                if !self.snapshot.writes_allowed() {
                    if self.snapshot.panel.protect_effective() {
                        return Err(MachineError::ProgramProtected);
                    }
                    return Err(MachineError::NotIdle);
                }
                if !(1..=9).contains(&index) {
                    return Err(MachineError::Invalid(
                        "work offset must be G54-G59.3".into(),
                    ));
                }
                // `G10 L20` makes the current position read zero, so the new
                // offset is the machine coordinate for each selected axis.
                let machines: Vec<(String, f64)> = axes
                    .iter()
                    .map(|axis| {
                        let machine = self
                            .snapshot
                            .positions
                            .axes
                            .get(axis)
                            .map(|position| position.machine)
                            .unwrap_or(0.0);
                        (axis.clone(), machine)
                    })
                    .collect();
                if let Some(offset) = self
                    .snapshot
                    .offsets
                    .offsets
                    .iter_mut()
                    .find(|offset| offset.index == index)
                {
                    let selected: Vec<String> = machines.iter().map(|(a, _)| a.clone()).collect();
                    for (axis, machine) in machines {
                        offset.values.insert(axis, machine);
                    }
                    self.snapshot.offsets.revision += 1;
                    // After a touch-off the work coordinate is zero by
                    // definition, so the DRO reads zero immediately.
                    for axis in &selected {
                        if let Some(position) = self.snapshot.positions.axes.get_mut(axis) {
                            position.work = 0.0;
                        }
                    }
                    Ok(())
                } else {
                    Err(MachineError::Invalid("unknown work offset".into()))
                }
            }
            MachineCommand::SetWorkPosition { index, values } => {
                if !self.snapshot.writes_allowed() {
                    if self.snapshot.panel.protect_effective() {
                        return Err(MachineError::ProgramProtected);
                    }
                    return Err(MachineError::NotIdle);
                }
                if !(1..=9).contains(&index) {
                    return Err(MachineError::Invalid(
                        "work offset must be G54-G59.3".into(),
                    ));
                }
                // `G10 L20` stores the offset that makes the position read the
                // given coordinates, so the new offset is machine - value.
                let derived: Vec<(String, f64, f64)> = values
                    .iter()
                    .map(|(axis, value)| {
                        let machine = self
                            .snapshot
                            .positions
                            .axes
                            .get(axis)
                            .map(|position| position.machine)
                            .unwrap_or(0.0);
                        (axis.to_string(), machine - value, *value)
                    })
                    .collect();
                if derived.is_empty() {
                    return Err(MachineError::Invalid("no axes given".into()));
                }
                if let Some(offset) = self
                    .snapshot
                    .offsets
                    .offsets
                    .iter_mut()
                    .find(|offset| offset.index == index)
                {
                    for (axis, new_offset, _) in &derived {
                        offset.values.insert(axis.clone(), *new_offset);
                    }
                    self.snapshot.offsets.revision += 1;
                    for (axis, _, wanted) in &derived {
                        if let Some(position) = self.snapshot.positions.axes.get_mut(axis) {
                            position.work = *wanted;
                        }
                    }
                    Ok(())
                } else {
                    Err(MachineError::Invalid("unknown work offset".into()))
                }
            }
            MachineCommand::SetG92Offset { values } => {
                if !self.snapshot.writes_allowed() {
                    return Err(MachineError::NotIdle);
                }
                self.snapshot.offsets.g92 = values;
                self.snapshot.offsets.revision += 1;
                Ok(())
            }
            MachineCommand::SetToolTableRow {
                row,
                expected_revision,
            } => {
                if !self.snapshot.writes_allowed() {
                    return Err(MachineError::NotIdle);
                }
                if expected_revision != self.snapshot.tool_table.revision {
                    return Err(MachineError::RevisionConflict);
                }
                self.tool_table
                    .upsert(row)
                    .map_err(|error| MachineError::Invalid(error.to_string()))?;
                self.tool_table
                    .save(expected_revision)
                    .map_err(|error| match error {
                        crate::tooltable::ToolTableError::RevisionConflict => {
                            MachineError::RevisionConflict
                        }
                        other => MachineError::Io(other.to_string()),
                    })?;
                self.snapshot.tool_table = self.tool_table.state();
                Ok(())
            }
            MachineCommand::ApplyProbeResult { result, target } => {
                if !self.snapshot.writes_allowed() {
                    if self.snapshot.panel.protect_effective() {
                        return Err(MachineError::ProgramProtected);
                    }
                    return Err(MachineError::NotIdle);
                }
                let ProbeTarget::WorkOffset(index) = target else {
                    return Err(MachineError::Invalid(
                        "calibration is stored by the calibration routine, not Apply".into(),
                    ));
                };
                result
                    .apply_error(self.snapshot.offsets.active_index)
                    .map_err(MachineError::Invalid)?;
                let current = self
                    .snapshot
                    .probe
                    .last_result
                    .as_ref()
                    .ok_or_else(|| MachineError::Invalid("no current probe result".into()))?;
                if current != &result {
                    return Err(MachineError::Invalid(
                        "stale probe result; run the probe again".into(),
                    ));
                }
                if self.snapshot.offsets.active_index != Some(index) {
                    return Err(MachineError::Invalid(
                        "Apply target must be the active work offset".into(),
                    ));
                }
                // An explicit Apply writes the complete result, exactly as
                // before: only the DIRECT path is limited to its branch.
                self.write_probe_raw(&result, index, None)
            }
            MachineCommand::ResetProbeCalibration => {
                if !self.snapshot.writes_allowed() {
                    return Err(MachineError::ProgramProtected);
                }
                self.snapshot.probe.calibration_offset = Some(0.0);
                Ok(())
            }
            MachineCommand::AbortProgram => {
                // K4 is halui.program.stop: no mode or idle gate, because a
                // reset has to be able to stop whatever is running. The same
                // reset clears a latched probe trip.
                self.snapshot.probe_trip = None;
                self.snapshot.task.running = false;
                self.snapshot.task.paused = false;
                self.snapshot.task.motion_active = false;
                self.snapshot.task.interp = InterpState::Idle;
                self.snapshot.program.paused = false;
                Ok(())
            }
            MachineCommand::ClearTrail => {
                self.clear_trail();
                Ok(())
            }
            MachineCommand::SetHalOutput { output, value } => {
                if !value.matches(output) {
                    return Err(MachineError::Invalid("wrong HAL value type".into()));
                }
                if matches!(output, LincOutput::JogIncrement | LincOutput::JogSpeed)
                    && self.snapshot.probe_trip_active()
                {
                    return Err(MachineError::ProbeTripped);
                }
                match output {
                    LincOutput::ToolChangeConfirm => {
                        if self.snapshot.tool_change.status != ToolChangeStatus::ManualPrompt
                            && !matches!(value, HalValue::Bit(false))
                        {
                            return Err(MachineError::NoPendingToolChange);
                        }
                        self.snapshot.hal.tool_change_confirm_seq += 1;
                    }
                    LincOutput::HmiAlive => {}
                    LincOutput::StatusDisplay => {
                        // The worker writes the status display from the WCS.
                    }
                    LincOutput::JogIncrement => {
                        if let HalValue::Float(value) = value {
                            self.snapshot.hal.jog_increment =
                                if value > 0.0 { Some(value) } else { None };
                        }
                    }
                    LincOutput::JogSpeed => {
                        if let HalValue::Float(value) = value {
                            self.snapshot.hal.jog_speed = Some(value);
                        }
                    }
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_fixture() -> (tempfile::TempDir, Profile) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::write(
            root.join("base.toml"),
            include_str!("../../../configs/base.toml"),
        )
        .unwrap();
        std::fs::create_dir_all(root.join("gcode")).unwrap();
        std::fs::write(
            root.join("tool.tbl"),
            "T1 P0 Z0 D6 ; end mill\nT12 P0 Z-56 D10 ; current\nT99 P0 Z0 D4 ; probe\n",
        )
        .unwrap();
        std::fs::write(
            root.join("probe_basic.ini"),
            "[EMC]\nMACHINE = Test\n[DISPLAY]\nPROGRAM_PREFIX = gcode\nTOOL_TABLE_COLUMNS = TZDR\n[KINS]\nJOINTS = 3\nKINEMATICS = trivkins coordinates=XYZ\n[TRAJ]\nCOORDINATES = XYZ\nLINEAR_UNITS = mm\nSPINDLES = 1\n[EMCIO]\nTOOL_TABLE = tool.tbl\n[AXIS_X]\nMIN_LIMIT = -372\nMAX_LIMIT = 5\n[AXIS_Y]\nMIN_LIMIT = -346\nMAX_LIMIT = 4\n[AXIS_Z]\nMIN_LIMIT = -253\nMAX_LIMIT = 19\n",
        ).unwrap();
        std::fs::write(
            root.join("sim.toml"),
            r#"schema_version = 1
profile = "sim"
extends = "base.toml"
[machine]
name = "Test"
kind = "sim"
gui = "mill"
linuxcnc_ini = "probe_basic.ini"
[panel]
kind = "simulated"
driver = "cnc-panel-sim"
[panel.link]
kind = "virtual"
[simulation]
enabled = true
"#,
        )
        .unwrap();
        let profile = Profile::load(&root.join("sim.toml"), None).unwrap();
        (dir, profile)
    }

    #[test]
    fn sim_starts_with_valid_panel_and_demo_program() {
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Auto);
        let snapshot = backend.snapshot();
        assert!(snapshot.panel.connected);
        assert!(snapshot.panel.mode.is_valid());
        assert_eq!(snapshot.panel.mode.mode(), Some(PanelMode::Auto));
        assert!(snapshot.program.content.is_some());
        assert!(snapshot.program.line_count > 10);
        assert!(!snapshot.live_trail.is_empty());
    }

    #[test]
    fn all_nine_work_systems_exist_and_accept_writes() {
        use crate::command::MachineCommand;
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Auto);
        let snapshot = backend.snapshot();
        let indices: Vec<u8> = snapshot.offsets.offsets.iter().map(|o| o.index).collect();
        assert_eq!(indices, (1..=9).collect::<Vec<u8>>(), "G54..G59.3");

        let mut values = AxisMap::new();
        values.insert("X", -10.0);
        backend
            .send(MachineCommand::SetWorkOffset {
                index: 9,
                values: values.clone(),
                rotation: Some(0.0),
            })
            .expect("G59.3 accepts an offset write");
        backend
            .send(MachineCommand::SetWorkPosition { index: 8, values })
            .expect("G59.2 accepts a position write");
        backend
            .send(MachineCommand::ZeroWorkOffset {
                index: 7,
                axes: vec!["X".into()],
            })
            .expect("G59.1 accepts a touch-off");

        let snapshot = backend.snapshot();
        let g59_3 = snapshot
            .offsets
            .offsets
            .iter()
            .find(|offset| offset.index == 9)
            .expect("G59.3 row");
        assert_eq!(g59_3.values.get("X").copied(), Some(-10.0));
    }

    #[test]
    fn activating_a_work_system_changes_the_active_row_and_rebases_the_dro() {
        use crate::command::MachineCommand;
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Auto);
        {
            let g55 = backend
                .snapshot
                .offsets
                .offsets
                .iter_mut()
                .find(|offset| offset.index == 2)
                .expect("G55 row");
            g55.values.insert("X", 12.5);
        }
        backend
            .send(MachineCommand::ActivateWorkOffset { index: 2 })
            .expect("G55 activates");
        let snapshot = backend.snapshot();
        assert_eq!(snapshot.offsets.active_index, Some(2));
        assert_eq!(
            snapshot
                .positions
                .axes
                .get("X")
                .map(|position| position.work),
            Some(-12.5),
            "the DRO follows the newly active system"
        );

        let error = backend
            .send(MachineCommand::ActivateWorkOffset { index: 10 })
            .unwrap_err();
        assert!(matches!(error, MachineError::Invalid(message) if message.contains("G54")));
    }

    #[test]
    fn jog_selections_survive_a_poll() {
        // Regression: the jog selections were stored in `panel`, which is
        // rebuilt from the panel service on every poll, so they were reset to
        // zero and jogging ran at no speed at all.
        use crate::command::{HalValue, LincOutput, MachineCommand};
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Jog);
        backend
            .send(MachineCommand::SetHalOutput {
                output: LincOutput::JogSpeed,
                value: HalValue::Float(42.0),
            })
            .unwrap();
        backend
            .send(MachineCommand::SetHalOutput {
                output: LincOutput::JogIncrement,
                value: HalValue::Float(0.5),
            })
            .unwrap();
        let snapshot = backend.snapshot();
        assert_eq!(snapshot.hal.jog_speed, Some(42.0));
        assert_eq!(snapshot.hal.jog_increment, Some(0.5));
    }

    #[test]
    fn mdi_is_gated_by_panel_mode_and_idle() {
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Auto);
        let error = backend
            .send(MachineCommand::IssueMdi {
                block: "G0 X0".into(),
                origin: MdiOrigin::Mdi,
            })
            .unwrap_err();
        assert_eq!(error, MachineError::WrongPanelMode);
        backend.set_panel_mode(PanelMode::Mdi);
        backend
            .send(MachineCommand::IssueMdi {
                block: "G0 X0".into(),
                origin: MdiOrigin::Mdi,
            })
            .unwrap();
    }

    #[test]
    fn a_work_offset_moves_the_machine_position_but_not_the_work_one() {
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Mdi);

        // G54 is active by default; give it a non-zero origin.
        let g54 = backend
            .snapshot
            .offsets
            .offsets
            .iter_mut()
            .find(|offset| offset.index == 1)
            .expect("G54 exists");
        g54.values.insert(String::from("X"), -50.0);
        g54.values.insert(String::from("Y"), -20.0);
        g54.values.insert(String::from("Z"), -10.0);

        backend
            .send(MachineCommand::IssueMdi {
                block: "G54 G0 X10 Y5 Z1".into(),
                origin: MdiOrigin::Mdi,
            })
            .unwrap();

        let position = |axis: &str| backend.snapshot.positions.axes.get(axis).cloned().unwrap();
        // Program words are the work position; the machine moves by the origin.
        assert_eq!(position("X").work, 10.0);
        assert_eq!(position("X").machine, -40.0);
        assert_eq!(position("Y").work, 5.0);
        assert_eq!(position("Y").machine, -15.0);
        assert_eq!(position("Z").work, 1.0);
        assert_eq!(position("Z").machine, -9.0);

        // The trail and the preview share the machine frame; a trail in work
        // coordinates would sit 50/20/10 mm away from the path it is cutting.
        let tip = backend
            .snapshot
            .live_trail
            .last()
            .map(|point| point.position)
            .expect("the trail has a tip");
        assert_eq!(tip, [-40.0, -15.0, -9.0]);
    }

    #[test]
    fn probe_trip_refuses_commands_until_the_reset_clears_it() {
        use crate::faults::{ProbeTripFault, ProbeTripKind};

        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Mdi);
        backend.snapshot.probe_trip = Some(ProbeTripFault::new(
            ProbeTripKind::CoordinateJog,
            ProbeTripKind::CoordinateJog.message(),
            1,
        ));

        let refused = backend
            .send(MachineCommand::IssueMdi {
                block: "G0 X0".into(),
                origin: MdiOrigin::Mdi,
            })
            .unwrap_err();
        assert_eq!(refused, MachineError::ProbeTripped);
        assert_eq!(
            backend.snapshot.positions.axes.get("X").map(|p| p.machine),
            Some(0.0)
        );

        // Jog parameters are motion commands too; the fault stops them before
        // the controller sees a new value.
        let refused = backend
            .send(MachineCommand::SetHalOutput {
                output: LincOutput::JogSpeed,
                value: HalValue::Float(10.0),
            })
            .unwrap_err();
        assert_eq!(refused, MachineError::ProbeTripped);

        // The controller reset is the only clear path.
        backend.send(MachineCommand::AbortProgram).unwrap();
        assert!(backend.snapshot().probe_trip.is_none());
        backend
            .send(MachineCommand::IssueMdi {
                block: "G0 X0".into(),
                origin: MdiOrigin::Mdi,
            })
            .unwrap();
    }

    #[test]
    fn protect_fails_closed_and_blocks_save() {
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Edit);
        backend.set_program_protect(false, false);
        let path = backend.profile().nc_files_root().join("PART.NC");
        std::fs::write(&path, "G0 X0\n").unwrap();
        let revision = backend.snapshot().program.revision;
        let error = backend
            .send(MachineCommand::SaveProgram {
                path,
                expected_revision: revision,
                content: "G0 X1\n".into(),
            })
            .unwrap_err();
        assert_eq!(error, MachineError::ProgramProtected);
    }

    #[test]
    fn loading_a_tool_runs_the_change_flow() {
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Auto);
        backend
            .send(MachineCommand::LoadTool { tool: 1 })
            .expect("load tool");
        // The fixture uses a manual changer, so the load waits for the operator
        // exactly as a plain M6 does; G43 is issued after the confirmation.
        assert_eq!(
            backend.snapshot().tool_change.status,
            ToolChangeStatus::ManualPrompt
        );
        let token = backend.snapshot().tool_change.token.unwrap();
        backend
            .send(MachineCommand::ConfirmManualToolChange { token })
            .expect("confirm");
        assert_eq!(
            backend.snapshot().tool_change.status,
            ToolChangeStatus::Complete
        );
        assert_eq!(backend.snapshot().tool_change.current_tool, 1);
    }

    #[test]
    fn manual_tool_change_requires_matching_token() {
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Auto);
        backend
            .send(MachineCommand::IssueToolChange { tool: 1 })
            .unwrap();
        let token = backend.snapshot().tool_change.token.unwrap();
        assert_eq!(
            backend.snapshot().tool_change.status,
            ToolChangeStatus::ManualPrompt
        );
        let error = backend
            .send(MachineCommand::ConfirmManualToolChange {
                token: ToolChangeToken(token.0 + 1),
            })
            .unwrap_err();
        assert_eq!(error, MachineError::StaleToolChangeToken);
        backend
            .send(MachineCommand::ConfirmManualToolChange { token })
            .unwrap();
        assert_eq!(
            backend.snapshot().tool_change.status,
            ToolChangeStatus::Complete
        );
        assert_eq!(backend.snapshot().hal.tool_change_confirm_seq, 1);
    }

    #[test]
    fn save_writes_atomically_and_updates_revision() {
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Edit);
        let root = backend.profile().nc_files_root().to_path_buf();
        let path = root.join("PART.NC");
        std::fs::write(&path, "G0 X0\n").unwrap();
        backend
            .send(MachineCommand::LoadProgram {
                path: path.clone(),
                expected_revision: None,
            })
            .unwrap();
        let revision = backend.snapshot().program.revision;
        backend
            .send(MachineCommand::SaveProgram {
                path,
                expected_revision: revision,
                content: "G0 X1\nG1 X2\n".into(),
            })
            .unwrap();
        let text = std::fs::read_to_string(root.join("PART.NC")).unwrap();
        assert_eq!(text, "G0 X1\nG1 X2\n");
    }

    fn run_probe(
        backend: &mut SimBackend,
        id: &str,
        parameters: &crate::probe::ProbeParameters,
    ) -> ProbeResult {
        let operation = crate::probe::operation(id).unwrap();
        backend
            .send(MachineCommand::IssueMdi {
                block: parameters.mdi_text(operation),
                origin: MdiOrigin::Probe,
            })
            .expect("run probe");
        backend
            .snapshot()
            .probe
            .last_result
            .clone()
            .expect("result")
    }

    /// A simulator whose tool tip sits at a known point, so the generated
    /// result slots are checkable, plus the fixture directory owning the profile.
    fn positioned_backend() -> (tempfile::TempDir, SimBackend) {
        let (dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Mdi);
        for (axis, value) in [("X", 12.5), ("Y", -4.25), ("Z", 1.5)] {
            if let Some(position) = backend.snapshot.positions.axes.get_mut(axis) {
                position.work = value;
            }
        }
        (dir, backend)
    }

    fn slot(result: &ProbeResult, parameter: u16) -> f64 {
        result
            .value(parameter)
            .unwrap_or_else(|| panic!("#{parameter} is not an updated slot"))
    }

    /// A probe routine is only selected by the exact `o<ID> call` token the
    /// parameter block generator emits, never by a contained name.
    fn assert_selected(result: &ProbeResult, id: &str) {
        let operation = crate::probe::operation(id).expect("catalogue operation");
        assert_eq!(result.operation, id);
        assert_eq!(result.routine, operation.name);
        assert_eq!(
            crate::probe::operation(&result.operation).map(|found| found.call),
            Some(operation.call),
            "the result must resolve back to the operation it was generated from"
        );
        assert_eq!(result.updated, operation.result_parameters());
    }

    #[test]
    fn probe_apply_converts_coordinates_merges_axes_and_is_idempotent() {
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Mdi);
        let source =
            AxisMap::from_entries([("X".into(), 1.0), ("Y".into(), 2.0), ("Z".into(), 3.0)]);
        backend
            .send(MachineCommand::SetWorkOffset {
                index: 1,
                values: source,
                rotation: Some(0.0),
            })
            .unwrap();
        let result = run_probe(
            &mut backend,
            "edge_x_minus",
            &crate::probe::ProbeParameters::default(),
        );
        let expected_x = 1.0 + result.measured.get("X").copied().unwrap();
        for _ in 0..2 {
            backend
                .send(MachineCommand::ApplyProbeResult {
                    result: result.clone(),
                    target: ProbeTarget::WorkOffset(1),
                })
                .expect("Apply");
            let offset = &backend.snapshot().offsets.offsets[0];
            assert!((offset.values.get("X").copied().unwrap() - expected_x).abs() < 1e-9);
            assert_eq!(offset.values.get("Y"), Some(&2.0));
            assert_eq!(offset.values.get("Z"), Some(&3.0));
        }

        backend.snapshot.offsets.active_index = Some(2);
        let error = backend
            .send(MachineCommand::ApplyProbeResult {
                result,
                target: ProbeTarget::WorkOffset(2),
            })
            .unwrap_err();
        assert!(matches!(error, MachineError::Invalid(message) if message.contains("G54")));
    }

    #[test]
    fn calibration_computes_selected_offset_and_has_no_generic_apply() {
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Mdi);
        let mut parameters = crate::probe::ProbeParameters::default();
        parameters.calibration_diameter = 40.0;
        let result = run_probe(&mut backend, "cal_round_boss", &parameters);
        assert!((result.error.unwrap() - 0.0075).abs() < 1e-12);
        assert!((backend.snapshot().probe.calibration_offset.unwrap() - 0.0075).abs() < 1e-12);
        assert!(backend
            .send(MachineCommand::ApplyProbeResult {
                result: result.clone(),
                target: ProbeTarget::WorkOffset(1),
            })
            .is_err());
        assert!((backend.snapshot().probe.calibration_offset.unwrap() - 0.0075).abs() < 1e-12);

        parameters.square_cal_axis = 1;
        let result = run_probe(&mut backend, "cal_round_boss", &parameters);
        assert!((result.error.unwrap() - 0.01).abs() < 1e-12);
        parameters.square_cal_axis = 2;
        let result = run_probe(&mut backend, "cal_round_boss", &parameters);
        assert!((result.error.unwrap() - 0.005).abs() < 1e-12);
    }

    #[test]
    fn direct_mode_applies_once_and_angle_run_clears_old_rotation() {
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Mdi);
        backend.snapshot.offsets.offsets[0].values =
            AxisMap::from_entries([("X".into(), 1.0), ("Y".into(), 2.0), ("Z".into(), 3.0)]);
        backend.snapshot.offsets.offsets[0].rotation = Some(15.0);
        let mut parameters = crate::probe::ProbeParameters::default();
        let angle = run_probe(&mut backend, "top_front_edge_angle", &parameters);
        assert_eq!(backend.snapshot().offsets.offsets[0].rotation, Some(0.0));
        assert_eq!(angle.rotation, None);

        parameters.probe_mode = crate::probe::ProbeMode::ApplyWcs;
        let result = run_probe(&mut backend, "edge_x_plus", &parameters);
        assert!(result.applied);
        let expected = 1.0 + result.measured.get("X").copied().unwrap();
        assert_eq!(
            backend.snapshot().offsets.offsets[0].values.get("X"),
            Some(&expected)
        );
        assert!(backend
            .send(MachineCommand::ApplyProbeResult {
                result,
                target: ProbeTarget::WorkOffset(1),
            })
            .is_err());
    }

    #[test]
    fn direct_corner_write_includes_the_helper_z() {
        let (_dir, mut backend) = positioned_backend();
        backend.snapshot.offsets.offsets[0].values =
            AxisMap::from_entries([("X".into(), -10.0), ("Y".into(), 2.0), ("Z".into(), 7.5)]);
        let mut parameters = crate::probe::ProbeParameters::default();
        parameters.probe_mode = crate::probe::ProbeMode::ApplyWcs;

        // The corner routine records X and Y inline and the helper
        // (probe_z_minus_sub, called with #3030) records Z, so a DIRECT run
        // touches all three axes (crate::probe::direct_written_axes).
        let result = run_probe(&mut backend, "front_left_top_corner", &parameters);
        assert!(result.applied);
        assert_eq!(
            crate::probe::direct_written_axes(
                crate::probe::operation("front_left_top_corner").unwrap()
            ),
            &["X", "Y", "Z"]
        );
        let source_x = -10.0;
        let source_y = 2.0;
        let source_z = 7.5;
        let row = &backend.snapshot().offsets.offsets[0];
        assert_eq!(
            row.values.get("X"),
            Some(&(source_x + result.measured.get("X").copied().unwrap()))
        );
        assert_eq!(
            row.values.get("Y"),
            Some(&(source_y + result.measured.get("Y").copied().unwrap()))
        );
        assert_eq!(
            row.values.get("Z"),
            Some(&(source_z + result.measured.get("Z").copied().unwrap()))
        );
    }

    #[test]
    fn direct_angle_write_includes_the_helper_z() {
        let (_dir, mut backend) = positioned_backend();
        backend.snapshot.offsets.offsets[0].values =
            AxisMap::from_entries([("X".into(), -10.0), ("Y".into(), 2.0), ("Z".into(), 7.5)]);
        let mut parameters = crate::probe::ProbeParameters::default();
        parameters.probe_mode = crate::probe::ProbeMode::ApplyWcs;

        // The Edge Angle routine records X and Y inline and the helper records
        // Z, so a DIRECT run touches all three axes.
        let result = run_probe(&mut backend, "top_front_edge_angle", &parameters);
        assert!(result.applied);
        assert_eq!(
            crate::probe::direct_written_axes(
                crate::probe::operation("top_front_edge_angle").unwrap()
            ),
            &["X", "Y", "Z"]
        );
        let row = &backend.snapshot().offsets.offsets[0];
        for (axis, source) in [("X", -10.0), ("Y", 2.0), ("Z", 7.5)] {
            assert_eq!(
                row.values.get(axis),
                Some(&(source + result.measured.get(axis).copied().unwrap())),
                "{axis}"
            );
        }
    }

    #[test]
    fn position_run_with_a_helper_leaves_the_row_untouched() {
        let (_dir, mut backend) = positioned_backend();
        let source =
            AxisMap::from_entries([("X".into(), -10.0), ("Y".into(), 2.0), ("Z".into(), 7.5)]);
        backend.snapshot.offsets.offsets[0].values = source.clone();
        backend.snapshot.offsets.offsets[0].rotation = Some(5.0);
        let parameters = crate::probe::ProbeParameters::default();

        // A POSITION run measures; the helper's DIRECT Z branch must not fire.
        let result = run_probe(&mut backend, "front_left_top_corner", &parameters);
        assert!(!result.applied);
        assert_eq!(result.mode, "POSITION");
        let row = &backend.snapshot().offsets.offsets[0];
        assert_eq!(row.values, source);
        assert_eq!(row.rotation, Some(5.0));
    }

    #[test]
    fn direct_surface_z_write_touches_only_z() {
        let (_dir, mut backend) = positioned_backend();
        backend.snapshot.offsets.offsets[0].values =
            AxisMap::from_entries([("X".into(), -9.0), ("Y".into(), -8.0), ("Z".into(), 3.0)]);
        let mut parameters = crate::probe::ProbeParameters::default();
        parameters.probe_mode = crate::probe::ProbeMode::ApplyWcs;

        let result = run_probe(&mut backend, "surface_z", &parameters);
        assert!(result.applied);
        let row = &backend.snapshot().offsets.offsets[0];
        assert_eq!(
            row.values.get("Z"),
            Some(&(3.0 + result.measured.get("Z").copied().unwrap()))
        );
        assert_eq!(row.values.get("X"), Some(&-9.0));
        assert_eq!(row.values.get("Y"), Some(&-8.0));
    }

    #[test]
    fn direct_spindle_nose_is_not_applied_and_changes_nothing() {
        let (_dir, mut backend) = positioned_backend();
        let source =
            AxisMap::from_entries([("X".into(), 1.0), ("Y".into(), 2.0), ("Z".into(), 3.0)]);
        backend.snapshot.offsets.offsets[0].values = source.clone();
        backend.snapshot.offsets.offsets[0].rotation = Some(12.0);
        let mut parameters = crate::probe::ProbeParameters::default();
        parameters.probe_mode = crate::probe::ProbeMode::ApplyWcs;

        // probe_spindle_nose.ngc has no G10 at all, so a DIRECT run there is a
        // measurement, not an applied offset.
        let result = run_probe(&mut backend, "spindle_nose", &parameters);
        assert!(!result.applied);
        let row = &backend.snapshot().offsets.offsets[0];
        assert_eq!(row.values, source);
        assert_eq!(row.rotation, Some(12.0));
    }

    fn probe_block(id: &str, mode: crate::probe::ProbeMode) -> String {
        let mut parameters = crate::probe::ProbeParameters::default();
        parameters.probe_mode = mode;
        parameters.mdi_text(crate::probe::operation(id).expect("operation"))
    }

    /// Every action the shared classifier calls a write, plus the labels the
    /// gating test checks them against.
    fn probe_write_blocks() -> Vec<(&'static str, String)> {
        use crate::probe::ProbeMode;
        vec![
            ("direct", probe_block("edge_x_plus", ProbeMode::ApplyWcs)),
            (
                "direct-angle",
                probe_block("top_front_edge_angle", ProbeMode::ApplyWcs),
            ),
            (
                "edge-angle",
                probe_block("top_front_edge_angle", ProbeMode::MeasureOnly),
            ),
            (
                "calibration",
                probe_block("cal_round_boss", ProbeMode::MeasureOnly),
            ),
            (
                "spindle-nose",
                probe_block("spindle_nose", ProbeMode::MeasureOnly),
            ),
            ("calibration-reset", "o<probe_cal_reset> call".into()),
            (
                "parameter-update",
                crate::probe::ProbeParameters::default().parameter_update_block(),
            ),
            ("reset-all", "o<reset_all_data> call".into()),
            ("reset-x", "o<x_data_reset> call".into()),
            ("reset-y", "o<y_data_reset> call".into()),
            ("unknown-mode", "o<probe_x_plus_wco> call".into()),
        ]
    }

    #[test]
    fn protect_refuses_every_probe_write_action_in_the_simulator() {
        // PROTECT on, and PROTECT unknown (a stale/absent pin) both fail closed.
        for (protect_on, known) in [(true, true), (false, false)] {
            let (_dir, profile) = profile_fixture();
            let mut backend = SimBackend::new(profile, PanelMode::Mdi);
            let row =
                AxisMap::from_entries([("X".into(), 1.0), ("Y".into(), 2.0), ("Z".into(), 3.0)]);
            backend.snapshot.offsets.offsets[0].values = row.clone();
            backend.snapshot.offsets.offsets[0].rotation = Some(5.0);
            backend.snapshot.probe.calibration_offset = Some(0.25);
            backend.set_program_protect(protect_on, known);
            for (name, block) in probe_write_blocks() {
                assert!(
                    crate::probe::probe_write_action(&block).is_some(),
                    "{name} is not classified as a write"
                );
                let error = backend
                    .send(MachineCommand::IssueMdi {
                        block,
                        origin: MdiOrigin::Probe,
                    })
                    .expect_err("PROTECT let a probe write execute");
                assert_eq!(error, MachineError::ProgramProtected, "{name}");
                // Nothing executed: no row, rotation, calibration or result
                // change, and no fresh result.
                assert_eq!(backend.snapshot().offsets.offsets[0].values, row, "{name}");
                assert_eq!(
                    backend.snapshot().offsets.offsets[0].rotation,
                    Some(5.0),
                    "{name}"
                );
                assert_eq!(
                    backend.snapshot().probe.calibration_offset,
                    Some(0.25),
                    "{name}"
                );
                assert!(backend.snapshot().probe.last_result.is_none(), "{name}");
                assert!(backend.snapshot().probe.history.is_empty(), "{name}");
            }
        }
    }

    #[test]
    fn protect_off_runs_every_probe_write_action() {
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Mdi);
        backend.set_program_protect(false, true);
        // DIRECT writes the row and records a fresh result.
        let mut direct = crate::probe::ProbeParameters::default();
        direct.probe_mode = crate::probe::ProbeMode::ApplyWcs;
        let result = run_probe(&mut backend, "edge_x_plus", &direct);
        assert!(result.applied);
        assert_eq!(backend.snapshot().probe.history.len(), 1);
        // Calibration commits again.
        let parameters = crate::probe::ProbeParameters::default();
        let calibration = run_probe(&mut backend, "cal_round_boss", &parameters);
        assert_eq!(calibration.operation, "cal_round_boss");
        assert!(backend.snapshot().probe.calibration_offset.is_some());
        // The reset, U update, data resets and spindle nose reach the controller.
        for (name, block) in [
            ("parameter-update", parameters.parameter_update_block()),
            (
                "spindle-nose",
                probe_block("spindle_nose", crate::probe::ProbeMode::MeasureOnly),
            ),
            ("reset-all", "o<reset_all_data> call".to_string()),
            ("reset-x", "o<x_data_reset> call".to_string()),
            ("reset-y", "o<y_data_reset> call".to_string()),
            ("calibration-reset", "o<probe_cal_reset> call".to_string()),
        ] {
            backend
                .send(MachineCommand::IssueMdi {
                    block,
                    origin: MdiOrigin::Probe,
                })
                .unwrap_or_else(|error| panic!("{name}: {error}"));
        }
        assert_eq!(backend.snapshot().probe.calibration_offset, Some(0.0));
    }

    #[test]
    fn spindle_nose_write_requires_protect_off() {
        use crate::probe::{probe_write_action, ProbeMode, ProbeWriteAction};
        let block = probe_block("spindle_nose", ProbeMode::MeasureOnly);
        // It is calibration-flagged even though its category is SurfaceZ.
        assert_eq!(
            probe_write_action(&block),
            Some(ProbeWriteAction::CalibrationRun)
        );
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Mdi);
        backend.set_program_protect(true, true);
        let error = backend
            .send(MachineCommand::IssueMdi {
                block: block.clone(),
                origin: MdiOrigin::Probe,
            })
            .expect_err("spindle nose ran under PROTECT");
        assert_eq!(error, MachineError::ProgramProtected);
        assert!(backend.snapshot().probe.last_result.is_none());
        backend.set_program_protect(false, true);
        backend
            .send(MachineCommand::IssueMdi {
                block,
                origin: MdiOrigin::Probe,
            })
            .expect("spindle nose with PROTECT off");
        assert_eq!(backend.snapshot().probe.history.len(), 1);
    }

    #[test]
    fn position_measure_only_runs_stay_allowed_with_protect_on() {
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Mdi);
        backend.set_program_protect(true, true);
        let parameters = crate::probe::ProbeParameters::default();
        for id in ["edge_x_plus", "round_boss"] {
            // The incidental parameter preamble is not a write; run_probe
            // panics if the send is refused.
            let result = run_probe(&mut backend, id, &parameters);
            assert_eq!(result.mode, "POSITION", "{id}");
            assert!(!result.applied, "{id}");
        }
        assert_eq!(backend.snapshot().probe.history.len(), 2);
    }

    #[test]
    fn protect_blocks_angle_rotation_reset_even_in_position_mode() {
        let (_dir, profile) = profile_fixture();
        let mut backend = SimBackend::new(profile, PanelMode::Mdi);
        backend.snapshot.offsets.offsets[0].rotation = Some(15.0);
        backend.set_program_protect(true, true);
        let parameters = crate::probe::ProbeParameters::default();
        let operation = crate::probe::operation("top_front_edge_angle").unwrap();
        let error = backend
            .send(MachineCommand::IssueMdi {
                block: parameters.mdi_text(operation),
                origin: MdiOrigin::Probe,
            })
            .unwrap_err();
        assert_eq!(error, MachineError::ProgramProtected);
        assert_eq!(backend.snapshot().offsets.offsets[0].rotation, Some(15.0));
        assert!(backend.snapshot().probe.last_result.is_none());

        backend.set_program_protect(false, true);
        run_probe(&mut backend, "top_front_edge_angle", &parameters);
        assert_eq!(backend.snapshot().offsets.offsets[0].rotation, Some(0.0));
    }

    #[test]
    fn round_pocket_centre_start_selects_its_own_operation() {
        let (_dir, mut backend) = positioned_backend();
        let mut parameters = crate::probe::ProbeParameters::default();
        parameters.diameter_hint = 30.0;
        let operation = crate::probe::operation("round_pocket_center").expect("operation");
        assert_eq!(
            parameters.mdi_text(operation).lines().last(),
            Some("o<probe_round_pocket_center_start> call")
        );

        let result = run_probe(&mut backend, "round_pocket_center", &parameters);
        assert_selected(&result, "round_pocket_center");
        // The shorter catalogue name must not win: it writes #1009 and applies Z.
        assert_ne!(result.operation, "round_pocket");
        assert_eq!(
            result.updated,
            vec![1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008, 1010]
        );
        assert_eq!(slot(&result, 1001), 30.0);
        assert_eq!(slot(&result, 1002), 12.5);
        assert_eq!(slot(&result, 1003), 30.0);
        assert_eq!(slot(&result, 1004), -4.25);
        assert_eq!(slot(&result, 1005), -2.5);
        assert_eq!(slot(&result, 1006), 27.5);
        assert_eq!(slot(&result, 1007), -19.25);
        assert_eq!(slot(&result, 1008), 10.75);
        assert_eq!(slot(&result, 1010), 30.0);
        assert!(!result.has_parameter(1009));
        assert_eq!(result.measured.get("X").copied(), Some(12.5));
        assert_eq!(result.measured.get("Y").copied(), Some(-4.25));
        assert_eq!(result.measured.get("Z").copied(), None);
    }

    #[test]
    fn rect_pocket_centre_start_selects_its_own_operation() {
        let (_dir, mut backend) = positioned_backend();
        let mut parameters = crate::probe::ProbeParameters::default();
        parameters.x_hint_boss_pocket = 40.0;
        parameters.y_hint_boss_pocket = 20.0;
        let operation = crate::probe::operation("rect_pocket_center").expect("operation");
        assert_eq!(
            parameters.mdi_text(operation).lines().last(),
            Some("o<probe_rect_pocket_center_start> call")
        );

        let result = run_probe(&mut backend, "rect_pocket_center", &parameters);
        assert_selected(&result, "rect_pocket_center");
        assert_ne!(result.operation, "rect_pocket");
        assert_eq!(
            result.updated,
            vec![1001, 1002, 1003, 1004, 1005, 1006, 1007, 1008]
        );
        assert_eq!(slot(&result, 1001), 40.0);
        assert_eq!(slot(&result, 1002), 12.5);
        assert_eq!(slot(&result, 1003), 20.0);
        assert_eq!(slot(&result, 1004), -4.25);
        assert_eq!(slot(&result, 1005), -7.5);
        assert_eq!(slot(&result, 1006), 32.5);
        assert_eq!(slot(&result, 1007), -14.25);
        assert_eq!(slot(&result, 1008), 5.75);
        assert!(!result.has_parameter(1009));
        assert_eq!(result.measured.get("X").copied(), Some(12.5));
        assert_eq!(result.measured.get("Y").copied(), Some(-4.25));
        assert_eq!(result.measured.get("Z").copied(), None);
    }

    #[test]
    fn valley_x_centre_start_selects_its_own_operation() {
        let (_dir, mut backend) = positioned_backend();
        let mut parameters = crate::probe::ProbeParameters::default();
        parameters.x_hint_ridge_valley = 8.0;
        let operation = crate::probe::operation("valley_x_center").expect("operation");
        assert_eq!(
            parameters.mdi_text(operation).lines().last(),
            Some("o<probe_valley_x_center_start> call")
        );

        let result = run_probe(&mut backend, "valley_x_center", &parameters);
        assert_selected(&result, "valley_x_center");
        assert_ne!(result.operation, "valley_x");
        assert_eq!(result.updated, vec![1001, 1002, 1005, 1006]);
        assert_eq!(slot(&result, 1001), 8.0);
        assert_eq!(slot(&result, 1002), 12.5);
        assert_eq!(slot(&result, 1005), 8.5);
        assert_eq!(slot(&result, 1006), 16.5);
        assert!(!result.has_parameter(1009));
        assert_eq!(result.measured.get("X").copied(), Some(12.5));
        assert_eq!(result.measured.get("Y").copied(), None);
    }

    #[test]
    fn valley_y_centre_start_selects_its_own_operation() {
        let (_dir, mut backend) = positioned_backend();
        let mut parameters = crate::probe::ProbeParameters::default();
        parameters.y_hint_ridge_valley = 8.0;
        let operation = crate::probe::operation("valley_y_center").expect("operation");
        assert_eq!(
            parameters.mdi_text(operation).lines().last(),
            Some("o<probe_valley_y_center_start> call")
        );

        let result = run_probe(&mut backend, "valley_y_center", &parameters);
        assert_selected(&result, "valley_y_center");
        assert_ne!(result.operation, "valley_y");
        assert_eq!(result.updated, vec![1003, 1004, 1007, 1008]);
        assert_eq!(slot(&result, 1003), 8.0);
        assert_eq!(slot(&result, 1004), -4.25);
        assert_eq!(slot(&result, 1007), -8.25);
        assert_eq!(slot(&result, 1008), -0.25);
        assert!(!result.has_parameter(1009));
        assert_eq!(result.measured.get("X").copied(), None);
        assert_eq!(result.measured.get("Y").copied(), Some(-4.25));
    }

    #[test]
    fn operation_calls_match_exactly_after_whitespace_and_arguments() {
        let id = |block: &str| probe_operation_for(block).map(|operation| operation.id);
        assert_eq!(
            id("o<probe_valley_x_center_start> call"),
            Some("valley_x_center")
        );
        assert_eq!(
            id("  O<PROBE_VALLEY_Y_CENTER_START>\tCall"),
            Some("valley_y_center")
        );
        assert_eq!(
            id("o<probe_rect_pocket> call [1][#3017]"),
            Some("rect_pocket")
        );
        assert_eq!(
            id("o <probe_round_pocket_center_start> call"),
            Some("round_pocket_center")
        );
        // A missing keyword, a bare mention and a prefix-only name are not calls.
        assert_eq!(id("o<probe_round_pocket_center_start>"), None);
        assert_eq!(id("M118 (probe_round_pocket_center_start)"), None);
        assert_eq!(id("o<probe_round_pocket_center> call"), None);
    }

    #[test]
    fn a_name_mention_is_not_a_call_and_the_plain_routine_still_matches() {
        let (_dir, mut backend) = positioned_backend();
        backend
            .send(MachineCommand::IssueMdi {
                block: "M118 (probe_round_pocket_center_start)".into(),
                origin: MdiOrigin::Probe,
            })
            .expect("comment-only block");
        assert!(backend.snapshot().probe.last_result.is_none());

        let mut parameters = crate::probe::ProbeParameters::default();
        parameters.diameter_hint = 30.0;
        let result = run_probe(&mut backend, "round_pocket", &parameters);
        assert_selected(&result, "round_pocket");
        assert_ne!(result.operation, "round_pocket_center");
        assert!(result.has_parameter(1009));
        assert_eq!(result.measured.get("Z").copied(), Some(1.5));
    }
}
