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

        // Work offsets G54…G59.
        for index in 1..=6 {
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
        let mut points: Vec<[f64; 3]> = Vec::new();
        for segment in preview
            .segments
            .iter()
            .filter(|segment| segment.line <= limit)
        {
            if points.last().copied() != Some(segment.start) {
                points.push(segment.start);
            }
            points.push(segment.end);
        }
        if points.is_empty() {
            points.push(self.current_tool_tip());
        }
        let max = self.trail_limit.max(2);
        if points.len() > max {
            points = points.split_off(points.len() - max);
        }
        self.snapshot.tool_tip = points.last().copied();
        self.snapshot.live_trail = Arc::new(points);
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

    fn append_trail(&mut self, point: [f64; 3]) {
        let mut trail = (*self.snapshot.live_trail).clone();
        if trail.last().copied() != Some(point) {
            trail.push(point);
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
        self.snapshot.live_trail = Arc::new(vec![tip]);
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
        if let Some(spindle) = self.snapshot.spindles.first_mut() {
            spindle.override_factor = panel.spindle_override;
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

    fn require_issue(&self, origin: MdiOrigin) -> Result<(), MachineError> {
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
        let preview = parse_preview(line, &axis_names, None, self.current_tool_tip());
        if let Some(segment) = preview.segments.last() {
            let start = segment.start;
            let end = segment.end;
            for (axis, index) in [("X", 0usize), ("Y", 1), ("Z", 2)] {
                if let Some(position) = self.snapshot.positions.axes.get_mut(axis) {
                    position.work = end[index];
                    position.machine = end[index];
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
            let steps = if segment.kind == SegmentKind::Rapid {
                3
            } else {
                12
            };
            for step in 1..=steps {
                let t = step as f64 / steps as f64;
                let point = [
                    start[0] + (end[0] - start[0]) * t,
                    start[1] + (end[1] - start[1]) * t,
                    start[2] + (end[2] - start[2]) * t,
                ];
                self.append_trail(point);
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
            MachineCommand::IssueMdi { block, origin } => {
                self.require_issue(origin)?;
                if block.trim().is_empty() {
                    return Err(MachineError::Invalid("empty MDI block".into()));
                }
                if matches!(origin, MdiOrigin::Probe) && !self.snapshot.probe.enabled {
                    return Err(MachineError::Invalid("touch probing is disabled".into()));
                }
                for line in block.lines().filter(|line| !line.trim().is_empty()) {
                    self.apply_mdi_line(line)?;
                }
                // Probe simulation: complete with a plausible measured result.
                if origin == MdiOrigin::Probe {
                    let measured = self.current_tool_tip();
                    let mut values = AxisMap::new();
                    for axis in self.snapshot.positions.axes.keys() {
                        let value = match axis {
                            "X" => measured[0],
                            "Y" => measured[1],
                            "Z" => measured[2],
                            _ => 0.0,
                        };
                        values.insert(axis.to_string(), value);
                    }
                    let result = ProbeResult {
                        routine: "simulated probe".into(),
                        measured: values.clone(),
                        error: Some(0.0),
                        wcs: self.snapshot.probe.selected_wcs.clone(),
                        mode: "SIM".into(),
                        timestamp_ms: now_ms(),
                        values: vec![
                            values.get("X").copied().unwrap_or(0.0),
                            values.get("Y").copied().unwrap_or(0.0),
                            values.get("Z").copied().unwrap_or(0.0),
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                            0.0,
                        ],
                    };
                    self.snapshot.probe.last_result = Some(result.clone());
                    self.snapshot.probe.history.push_front(result.clone());
                    while self.snapshot.probe.history.len() > 20 {
                        self.snapshot.probe.history.pop_back();
                    }
                    self.last_probe = Some(result);
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
                if !(1..=6).contains(&index) {
                    return Err(MachineError::Invalid("work offset must be G54-G59".into()));
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
                    return Err(MachineError::ProgramProtected);
                }
                match target {
                    ProbeTarget::WorkOffset(index) => {
                        if let Some(offset) = self
                            .snapshot
                            .offsets
                            .offsets
                            .iter_mut()
                            .find(|o| o.index == index)
                        {
                            offset.values = result.measured.clone();
                            self.snapshot.offsets.revision += 1;
                        }
                    }
                    ProbeTarget::Calibration => {
                        let value = result.measured.values().first().copied().unwrap_or(0.0);
                        self.snapshot.probe.calibration_offset = Some(value);
                    }
                }
                Ok(())
            }
            MachineCommand::ResetProbeCalibration => {
                if !self.snapshot.writes_allowed() {
                    return Err(MachineError::ProgramProtected);
                }
                self.snapshot.probe.calibration_offset = None;
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
                }
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

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
}
