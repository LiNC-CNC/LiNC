//! Native LinuxCNC backend.
//!
//! The HMI worker owns one [`NmlClient`] and reads panel/spindle pins through
//! [`linc_hal::HalReader`]. There is no helper process, no JSON protocol and no
//! Python runtime on the control path. The only authority remains the typed
//! [`MachineCommand`] set: this crate has no generic NML or HAL escape hatch.
//!
//! NML is the same transport AXIS and the official Python module use. The
//! native client links `liblinuxcnc` and `libnml`, so this crate is Linux-only
//! and needs the LinuxCNC development files at build time.

mod panel;
mod parameters;
mod snapshot;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use linuxcnc_nml::{
    Command as NmlCommand, MessageKind, NmlClient, NmlError, Status as NmlStatus,
    TaskMode as NmlTaskMode, TaskState as NmlTaskState,
};

use linc_core::backend::MachineBackend;
use linc_core::command::{
    HalValue, LincOutput, MachineCommand, MachineError, MdiOrigin, ProbeTarget,
};
use linc_core::config::Profile;
use linc_core::diag::BridgeInfo;
use linc_core::gcode::parse_preview;
use linc_core::model::*;
use linc_core::tooltable::ToolTable;
use linc_hal::HalReader;
use thiserror::Error;

use panel::{PanelReader, PanelState};
use parameters::ParameterFile;
use snapshot::{carry_hal_selections, initial_snapshot, SnapshotBuilder};

#[derive(Debug, Error)]
pub enum BackendError {
    #[error("cannot open LinuxCNC NML: {0}")]
    Connect(String),
    #[error("NML error: {0}")]
    Nml(#[from] NmlError),
    #[error("HAL error: {0}")]
    Hal(String),
    #[error("tool table: {0}")]
    ToolTable(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

/// Native in-process backend.
pub struct NmlBackend {
    client: NmlClient,
    hal: Option<HalReader>,
    profile: Profile,
    panel: PanelReader,
    builder: SnapshotBuilder,
    parameters: ParameterFile,
    last_snapshot: Option<MachineSnapshot>,
    bridge: BridgeInfo,
    nml_alive: bool,
    nml_lost_reported: bool,
    pending_restore: Option<NmlTaskMode>,
    pending_restore_at: Instant,
    pending_height_comp: Option<(u16, Instant)>,
    offsets_synced: bool,
    offset_sync_attempts: u32,
    offset_sync_next: Instant,
    confirm_seq: u64,
    tool_table: ToolTable,
}

impl NmlBackend {
    pub fn connect(profile: Profile) -> Result<Self, BackendError> {
        let timeout = Duration::from_millis(profile.config.backend.connect_timeout_ms.max(1));
        let deadline = Instant::now() + timeout;
        let client = loop {
            match NmlClient::open_default() {
                Ok(client) => break client,
                Err(error) => {
                    if Instant::now() >= deadline {
                        return Err(BackendError::Connect(error.to_string()));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        };

        let hal = match HalReader::new() {
            Ok(reader) => Some(reader),
            Err(error) => {
                log::warn!("HAL pin reads unavailable: {error}");
                None
            }
        };
        if hal.is_none() {
            log::warn!(
                "the native backend is running without HAL; panel, spindle and tool-change pins will read as offline"
            );
        }

        let (tool_table_state, tool_table) = load_tool_table(&profile);
        let mut initial = initial_snapshot(&profile);
        initial.tool_table = tool_table_state;
        let parameters = ParameterFile::load(profile.machine_ini.parameter_file.clone());
        let builder = SnapshotBuilder::new(profile.clone(), tool_table.clone());
        let bridge = BridgeInfo {
            kind: "nml".into(),
            pid: std::process::id(),
            alive: true,
            ..BridgeInfo::default()
        };
        Ok(Self {
            client,
            hal,
            profile,
            panel: PanelReader::new(),
            builder,
            parameters,
            last_snapshot: Some(initial),
            bridge,
            nml_alive: true,
            nml_lost_reported: false,
            pending_restore: None,
            pending_restore_at: Instant::now(),
            pending_height_comp: None,
            offsets_synced: false,
            offset_sync_attempts: 0,
            offset_sync_next: Instant::now(),
            confirm_seq: 0,
            tool_table,
        })
    }

    fn refresh(&mut self) -> MachineSnapshot {
        let status = match self.client.poll_status() {
            Ok(status) => {
                self.nml_alive = true;
                if self.nml_lost_reported {
                    log::info!("LinuxCNC NML status link is back");
                    self.nml_lost_reported = false;
                }
                self.bridge.fatal = None;
                status
            }
            Err(error) => return self.lost_status(error),
        };
        self.poll_messages();

        let panel = self.panel.read(self.hal.as_ref(), &self.profile);
        let spindles = self.read_spindles(&status, &panel);
        self.poll_restore();
        self.poll_height_comp(&panel);
        self.sync_work_offsets(&status);

        let mut snapshot = self
            .builder
            .build(&status, &panel, spindles, &self.parameters);
        if let Some(stored) = self.last_snapshot.as_mut() {
            carry_hal_selections(&mut snapshot, stored);
        }
        snapshot.hal.tool_change_confirm_seq = self.confirm_seq;
        snapshot.machine.hal_thread_ok = self.hal.is_some();
        snapshot.bridge = Some(self.bridge_info());
        self.last_snapshot = Some(snapshot.clone());
        snapshot
    }

    fn lost_status(&mut self, error: NmlError) -> MachineSnapshot {
        self.nml_alive = false;
        self.bridge.errors += 1;
        self.bridge.alive = false;
        self.bridge.fatal = Some(error.to_string());
        if !self.nml_lost_reported {
            self.nml_lost_reported = true;
            log::error!("LinuxCNC NML status link lost: {error}");
            self.builder
                .push_message(MessageKind::NmlError, "LinuxCNC NML status link lost");
        }
        let mut snapshot = self
            .last_snapshot
            .clone()
            .unwrap_or_else(|| initial_snapshot(&self.profile));
        snapshot.meta.stale = true;
        snapshot.meta.captured_at_ms = now_ms();
        snapshot.panel.connected = false;
        snapshot.panel.communication_error = true;
        snapshot.panel.program_protect_known = false;
        snapshot.panel.mode = PanelModeValidity::Unknown;
        snapshot.machine.nml_status_connected = false;
        snapshot.machine.nml_command_connected = false;
        snapshot.hal.tool_change_confirm = false;
        snapshot.tool_change.confirmed = false;
        snapshot.bridge = Some(self.bridge_info());
        snapshot
    }

    fn bridge_info(&self) -> BridgeInfo {
        let mut info = self.bridge.clone();
        info.kind = "nml".into();
        info.pid = std::process::id();
        info.alive = self.nml_alive;
        info
    }

    fn poll_messages(&mut self) {
        loop {
            match self.client.poll_error() {
                Ok(Some(message)) => self.builder.push_message(message.kind, &message.text),
                Ok(None) => break,
                Err(error) => {
                    log::warn!("LinuxCNC error channel: {error}");
                    break;
                }
            }
        }
    }

    fn read_spindles(&self, status: &NmlStatus, panel: &PanelState) -> Vec<SpindleState> {
        let configured = self.profile.machine_ini.spindles.max(1);
        let count = configured.min(status.spindles.len()).min(4);
        let mut rows = Vec::with_capacity(count);
        for index in 0..count {
            let spindle = &status.spindles[index];
            let primary = (index == 0)
                .then(|| {
                    self.hal
                        .as_ref()?
                        .float(&self.profile.config.hal.pins.spindle_speed)
                })
                .flatten();
            let commanded_rps = self
                .hal
                .as_ref()
                .and_then(|hal| hal.float(&format!("spindle.{index}.speed-out-rps")));
            let feedback = primary.filter(|value| value.abs() > 1e-9).or(commanded_rps);
            let override_factor = panel.spindle_override;
            let actual_speed = match feedback {
                Some(rps) => rps.abs() * 60.0,
                None if spindle.enabled => spindle.speed.abs() * override_factor,
                None => 0.0,
            };
            let load_pin = if index == 0 {
                self.profile.config.hal.pins.spindle_load.clone()
            } else {
                format!("rio.spindle{index}-modbus.spindle_load-abs")
            };
            let load = self.hal.as_ref().and_then(|hal| hal.float(&load_pin));
            let name = if index == 0 {
                "MAIN SPINDLE".to_string()
            } else if self.profile.variant == MachineVariant::Lathe {
                "LIVE TOOL".to_string()
            } else {
                format!("SPINDLE {index}")
            };
            rows.push(SpindleState {
                index,
                name,
                commanded_speed: spindle.speed,
                actual_speed,
                load,
                override_factor,
                direction: spindle.direction.clamp(-1, 1) as i8,
                on: spindle.enabled,
                live_tool: self.profile.variant == MachineVariant::Lathe && index > 0,
                live_tool_axial: false,
                live_tool_max_rpm: self
                    .profile
                    .config
                    .lathe
                    .as_ref()
                    .and_then(|lathe| lathe.live_tool_max_rpm)
                    .unwrap_or(0.0),
            });
        }
        rows
    }

    fn poll_restore(&mut self) {
        let Some(previous) = self.pending_restore else {
            return;
        };
        if self.pending_restore_at.elapsed() < Duration::from_millis(400) {
            return;
        }
        let Ok(status) = self.client.poll_status() else {
            return;
        };
        if status.task.interp_state == linuxcnc_nml::InterpState::Idle && !status.motion.probing {
            if let Err(error) = self.client.send(&NmlCommand::SetMode(previous)) {
                log::warn!("could not restore the panel task mode: {error}");
            }
            self.pending_restore = None;
        }
    }

    fn poll_height_comp(&mut self, panel: &PanelState) {
        let Some((tool, started)) = self.pending_height_comp else {
            return;
        };
        if started.elapsed() < Duration::from_millis(1500) || panel.tool_change_active {
            return;
        }
        let Ok(status) = self.client.poll_status() else {
            return;
        };
        if status.io.tool_in_spindle.max(0) as u16 != tool
            || status.task.interp_state != linuxcnc_nml::InterpState::Idle
        {
            return;
        }
        self.pending_height_comp = None;
        if let Err(error) = self.issue_blocks(&[format!("G43 H{tool}")]) {
            log::warn!("could not apply G43 H{tool}: {error}");
        }
    }

    fn sync_work_offsets(&mut self, status: &NmlStatus) {
        if self.offsets_synced || self.offset_sync_attempts >= 5 {
            return;
        }
        if Instant::now() < self.offset_sync_next {
            return;
        }
        if status.io.estop
            || status.task.state != NmlTaskState::On
            || status.task.interp_state != linuxcnc_nml::InterpState::Idle
            || status.motion.probing
        {
            return;
        }
        // `G54` is an MDI block, and LinuxCNC refuses MDI until every axis is
        // homed. A start-up sync that runs before homing would burn every
        // attempt on rejections and never apply the stored offsets.
        if status.joints.len() < self.profile.machine_ini.axes.len()
            || status
                .joints
                .iter()
                .take(self.profile.machine_ini.axes.len())
                .any(|joint| !joint.homed)
        {
            return;
        }
        let active = if (1..=9).contains(&status.offsets.g5x_index) {
            status.offsets.g5x_index as u8
        } else {
            self.parameters.active_index()
        };
        let axes = self.axis_names();
        if !self.parameters.has_work_offset(active, axes.len()) {
            self.offsets_synced = true;
            return;
        }
        if self.live_offsets_match(status, active, &axes) {
            self.offsets_synced = true;
            return;
        }
        self.offset_sync_attempts += 1;
        self.offset_sync_next = Instant::now() + Duration::from_secs(2);

        // The interpreter restores the parameter file at startup but only
        // queues the G5X offset; reinitializing it while the task is running
        // makes the queue drain. If that does not take, a G54..G59.3 MDI block
        // is the documented fallback.
        let _ = self.client.send(&NmlCommand::ResetInterpreter);
        let _ = self.client.wait_complete(Duration::from_secs(2));
        std::thread::sleep(Duration::from_millis(250));
        if let Ok(now) = self.client.poll_status() {
            if self.live_offsets_match(&now, active, &axes) {
                self.offsets_synced = true;
                log::info!("stored work offsets applied by the interpreter restart");
                return;
            }
        }
        let code = wcs_name(active);
        if let Err(error) = self.issue_blocks(&[code.clone()]) {
            log::warn!("could not apply stored {code} offsets: {error}");
            return;
        }
        let _ = self.client.wait_complete(Duration::from_secs(2));
        if let Ok(now) = self.client.poll_status() {
            if self.live_offsets_match(&now, active, &axes) {
                self.offsets_synced = true;
                log::info!("stored work offsets applied by selecting {code}");
            }
        }
    }

    fn live_offsets_match(&self, status: &NmlStatus, index: u8, axes: &[String]) -> bool {
        let live = status.offsets.g5x_offset;
        let mut compared = 0;
        for (axis_index, _axis) in axes.iter().enumerate() {
            let Some(stored) = self.parameters_work_value(index, axis_index) else {
                continue;
            };
            let actual = pose_component(&live, axis_index);
            if (actual - stored).abs() > 1e-6 {
                return false;
            }
            compared += 1;
        }
        compared > 0
    }

    fn parameters_work_value(&self, index: u8, axis_index: usize) -> Option<f64> {
        let key = 5220 + index as u32 * 20 + 1 + axis_index as u32;
        self.parameters.has(key).then(|| self.parameters.get(key))
    }

    fn axis_names(&self) -> Vec<String> {
        self.profile
            .machine_ini
            .axes
            .iter()
            .map(|axis| axis.name.clone())
            .collect()
    }

    fn poll_status(&mut self) -> Result<NmlStatus, MachineError> {
        self.client
            .poll_status()
            .map_err(|error| MachineError::Backend(error.to_string()))
    }

    fn nml_send(&mut self, command: &NmlCommand) -> Result<(), MachineError> {
        self.client
            .send(command)
            .map_err(|error| MachineError::Backend(error.to_string()))
    }

    fn wait_complete(&mut self, timeout: Duration) -> Result<(), MachineError> {
        match self.client.wait_complete(timeout) {
            Ok(true) => Ok(()),
            Ok(false) => Err(MachineError::Backend(format!(
                "NML command timed out after {} ms",
                timeout.as_millis()
            ))),
            Err(error) => Err(MachineError::Backend(error.to_string())),
        }
    }

    fn issue_blocks(&mut self, blocks: &[String]) -> Result<(), MachineError> {
        let status = self.poll_status()?;
        let previous = status.task.mode;
        if previous != NmlTaskMode::Mdi {
            self.nml_send(&NmlCommand::SetMode(NmlTaskMode::Mdi))?;
            self.wait_complete(Duration::from_secs(2))?;
            self.pending_restore = match previous {
                NmlTaskMode::Manual | NmlTaskMode::Auto => Some(previous),
                _ => None,
            };
            self.pending_restore_at = Instant::now();
        }
        for block in blocks {
            self.nml_send(&NmlCommand::Mdi(block.clone()))?;
        }
        Ok(())
    }

    fn canonical_nc_file(&self, path: &Path) -> Result<PathBuf, MachineError> {
        let root = self
            .profile
            .nc_files_root()
            .canonicalize()
            .map_err(|_| MachineError::UnsafePath)?;
        let candidate = path.canonicalize().map_err(|_| MachineError::UnsafePath)?;
        if !candidate.starts_with(&root) || !candidate.is_file() {
            return Err(MachineError::UnsafePath);
        }
        Ok(candidate)
    }

    fn validate_issue(&mut self, origin: MdiOrigin) -> Result<(), MachineError> {
        let snapshot = self.refresh();
        if !snapshot.panel.connected || snapshot.panel.communication_error {
            return Err(MachineError::PanelOffline);
        }
        if !snapshot.panel.mode.is_valid() {
            return Err(MachineError::InvalidPanelMode);
        }
        if snapshot.task.power != MachinePower::On {
            return Err(MachineError::MachineOff);
        }
        if snapshot.task.interp != InterpState::Idle || snapshot.task.running {
            return Err(MachineError::NotIdle);
        }
        if snapshot.machine.homed.is_empty()
            || !snapshot.machine.homed.iter().all(|(_, homed)| *homed)
        {
            return Err(MachineError::NotHomed);
        }
        if origin == MdiOrigin::Mdi && snapshot.panel.mode.mode() != Some(PanelMode::Mdi) {
            return Err(MachineError::WrongPanelMode);
        }
        Ok(())
    }

    fn validate_write(&mut self) -> Result<MachineSnapshot, MachineError> {
        let snapshot = self.refresh();
        if !snapshot.panel.connected || snapshot.panel.communication_error {
            return Err(MachineError::PanelOffline);
        }
        if !snapshot.panel.mode.is_valid() {
            return Err(MachineError::InvalidPanelMode);
        }
        if snapshot.panel.protect_effective() {
            return Err(MachineError::ProgramProtected);
        }
        if snapshot.task.power != MachinePower::On {
            return Err(MachineError::MachineOff);
        }
        if snapshot.task.interp != InterpState::Idle || snapshot.task.running {
            return Err(MachineError::NotIdle);
        }
        Ok(snapshot)
    }

    fn capture_live_offsets(&mut self) {
        if !self.offsets_synced {
            return;
        }
        let Ok(status) = self.client.poll_status() else {
            return;
        };
        let index = status.offsets.g5x_index.clamp(1, 9) as u8;
        let axes = self.axis_names();
        let mut values = AxisMap::new();
        for (axis_index, axis) in axes.iter().enumerate() {
            values.insert(
                axis.clone(),
                pose_component(&status.offsets.g5x_offset, axis_index),
            );
        }
        self.parameters.record_offset_axes(index, &values, &axes);
        self.parameters
            .record_rotation(index, status.offsets.rotation_xy);
    }

    fn save_parameters(&mut self) -> Result<(), MachineError> {
        self.parameters.save().map_err(MachineError::Backend)
    }

    fn write_work_offset(
        &mut self,
        index: u8,
        values: &AxisMap<f64>,
        rotation: Option<f64>,
    ) -> Result<(), MachineError> {
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
        let axes = self.axis_names();
        let mut parts = vec![format!("G10 L2 P{index}")];
        for (axis, value) in values.iter() {
            parts.push(format!("{axis}{value:.6}"));
        }
        if let Some(rotation) = rotation {
            parts.push(format!("R{rotation:.6}"));
        }
        self.issue_blocks(&[parts.join(" ")])?;
        self.wait_complete(Duration::from_secs(2))?;
        if index == self.poll_status()?.offsets.g5x_index.clamp(1, 9) as u8 {
            let status = self.poll_status()?;
            let mut kept = AxisMap::new();
            for (axis_index, axis) in axes.iter().enumerate() {
                if values.get(axis).is_some() {
                    kept.insert(
                        axis.clone(),
                        pose_component(&status.offsets.g5x_offset, axis_index),
                    );
                }
            }
            let wrong: Vec<String> = values
                .iter()
                .filter_map(|(axis, asked)| {
                    kept.get(axis).and_then(|kept| {
                        ((kept - asked).abs() > 1e-4)
                            .then(|| format!("{axis}={kept:.4} (asked {asked:.4})"))
                    })
                })
                .collect();
            if !wrong.is_empty() {
                return Err(MachineError::Backend(format!(
                    "controller did not take the offset: {}",
                    wrong.join(", ")
                )));
            }
        }
        self.parameters
            .record_work_offset(index, values, &axes, rotation);
        self.save_parameters()
    }

    fn touch_off(
        &mut self,
        index: u8,
        values: &AxisMap<f64>,
        axes: &[String],
    ) -> Result<(), MachineError> {
        if !(1..=9).contains(&index) {
            return Err(MachineError::Invalid(
                "work offset must be G54-G59.3".into(),
            ));
        }
        if axes.is_empty() {
            return Err(MachineError::Invalid("no axes selected".into()));
        }
        let parts = values
            .iter()
            .map(|(axis, value)| format!("{axis}{value:.6}"))
            .collect::<Vec<_>>()
            .join(" ");
        self.issue_blocks(&[format!("G10 L20 P{index} {parts}")])?;
        self.wait_complete(Duration::from_secs(2))?;
        let status = self.poll_status()?;
        let live_index = status.offsets.g5x_index.clamp(1, 9) as u8;
        if live_index == index {
            let mut computed = AxisMap::new();
            for (axis_index, axis) in axes.iter().enumerate() {
                computed.insert(
                    axis.clone(),
                    pose_component(&status.offsets.g5x_offset, axis_index),
                );
            }
            self.parameters.record_offset_axes(index, &computed, axes);
            self.parameters
                .record_rotation(index, status.offsets.rotation_xy);
        } else {
            // G10 L20 computes the offset from the machine position. For a
            // non-active system the live G5X field still describes the active
            // one, so derive the new row from the machine pose and the global
            // G92/tool offsets.
            let mut computed = AxisMap::new();
            for (axis_index, axis) in axes.iter().enumerate() {
                if let Some(requested) = values.get(axis) {
                    let machine = pose_component(&status.positions.actual, axis_index);
                    let g92 = pose_component(&status.offsets.g92_offset, axis_index);
                    let tool = pose_component(&status.offsets.tool_offset, axis_index);
                    computed.insert(axis.clone(), machine - g92 - tool - requested);
                }
            }
            self.parameters.record_offset_axes(index, &computed, axes);
            self.parameters
                .record_rotation(index, status.offsets.rotation_xy);
        }
        self.save_parameters()
    }

    fn persist_g92(&mut self) -> Result<(), MachineError> {
        let status = self.poll_status()?;
        let axes = self.axis_names();
        let mut values = AxisMap::new();
        for (axis_index, axis) in axes.iter().enumerate() {
            values.insert(
                axis.clone(),
                pose_component(&status.offsets.g92_offset, axis_index),
            );
        }
        self.parameters.record_g92(&values, &axes);
        self.save_parameters()
    }

    fn dispatch(&mut self, command: MachineCommand) -> Result<(), MachineError> {
        match command {
            MachineCommand::IssueMdi { block, origin } => {
                self.validate_issue(origin)?;
                let blocks: Vec<String> = block
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect();
                if blocks.is_empty() {
                    return Err(MachineError::Invalid("empty MDI block".into()));
                }
                let axes = self.profile.machine_ini.axis_names();
                for line in &blocks {
                    let preview = parse_preview(line, &axes, None, [0.0; 3]);
                    if let Some(error) = preview.error {
                        return Err(MachineError::Invalid(format!(
                            "line {}: {}",
                            error.line, error.message
                        )));
                    }
                }
                if origin == MdiOrigin::Probe {
                    self.builder.note_probe_block(Some(block.clone()));
                }
                self.issue_blocks(&blocks)
            }
            MachineCommand::IssueToolChange { tool } => {
                self.validate_issue(MdiOrigin::ToolChange)?;
                self.issue_blocks(&[format!("T{tool} M6")])
            }
            MachineCommand::LoadTool { tool } => {
                self.validate_issue(MdiOrigin::ToolChange)?;
                self.nml_send(&NmlCommand::LoadToolTable(Some(
                    self.profile.machine_ini.tool_table.clone(),
                )))?;
                self.issue_blocks(&[format!("T{tool} M6")])?;
                self.pending_height_comp = Some((tool, Instant::now()));
                Ok(())
            }
            MachineCommand::ConfirmManualToolChange { token } => {
                let snapshot = self.refresh();
                if !snapshot.panel.connected {
                    return Err(MachineError::PanelOffline);
                }
                if !snapshot.panel.mode.is_valid() {
                    return Err(MachineError::InvalidPanelMode);
                }
                if snapshot.tool_change.status != ToolChangeStatus::ManualPrompt {
                    return Err(MachineError::NoPendingToolChange);
                }
                if snapshot.tool_change.token.as_ref() != Some(&token) {
                    return Err(MachineError::StaleToolChangeToken);
                }
                self.confirm_seq += 1;
                self.builder.confirm_tool_change();
                if let Some(stored) = self.last_snapshot.as_mut() {
                    stored.tool_change.confirmed = true;
                    stored.hal.tool_change_confirm = true;
                    stored.hal.tool_change_confirm_seq = self.confirm_seq;
                }
                Ok(())
            }
            MachineCommand::SetCurrentTool { tool } => {
                self.validate_issue(MdiOrigin::ToolChange)?;
                self.issue_blocks(&[format!("M61 Q{tool}")])
            }
            MachineCommand::ToggleLimitsOverride => {
                self.nml_send(&NmlCommand::OverrideLimits)?;
                self.wait_complete(Duration::from_secs(1))?;
                if self.poll_status()?.task.state != NmlTaskState::On {
                    self.nml_send(&NmlCommand::SetState(NmlTaskState::On))?;
                }
                Ok(())
            }
            MachineCommand::LoadProgram {
                path,
                expected_revision,
            } => {
                if self.refresh().task.running {
                    return Err(MachineError::ProgramRunning);
                }
                let canonical = self.canonical_nc_file(&path)?;
                if let Some(expected) = expected_revision {
                    if expected != self.refresh().program.revision {
                        return Err(MachineError::RevisionConflict);
                    }
                }
                self.capture_live_offsets();
                self.save_parameters()?;
                self.nml_send(&NmlCommand::ProgramOpen(canonical))?;
                self.wait_complete(Duration::from_secs(2))?;
                Ok(())
            }
            MachineCommand::SaveProgram {
                path,
                expected_revision,
                content,
            } => {
                let snapshot = self.validate_write()?;
                if snapshot.panel.mode.mode() != Some(PanelMode::Edit) {
                    return Err(MachineError::WrongPanelMode);
                }
                if expected_revision != snapshot.program.revision {
                    return Err(MachineError::RevisionConflict);
                }
                let canonical = self.canonical_nc_file(&path)?;
                let parent = canonical.parent().ok_or(MachineError::UnsafePath)?;
                let mut temp = tempfile::NamedTempFile::new_in(parent)?;
                use std::io::Write;
                temp.write_all(content.as_bytes())?;
                temp.as_file().sync_all()?;
                temp.persist(&canonical)
                    .map_err(|error| MachineError::Io(error.error.to_string()))?;
                Ok(())
            }
            MachineCommand::SetWorkOffset {
                index,
                values,
                rotation,
            } => {
                self.validate_write()?;
                self.write_work_offset(index, &values, rotation)
            }
            MachineCommand::RunFromLine { line } => {
                self.validate_write()?;
                if line == 0 {
                    return Err(MachineError::Invalid("line must be 1 or greater".into()));
                }
                let status = self.poll_status()?;
                if status.task.mode != NmlTaskMode::Auto {
                    self.nml_send(&NmlCommand::SetMode(NmlTaskMode::Auto))?;
                    self.wait_complete(Duration::from_secs(2))?;
                }
                self.nml_send(&NmlCommand::AutoRun { line: line as i32 })
            }
            MachineCommand::ZeroWorkOffset { index, axes } => {
                self.validate_write()?;
                if !(1..=9).contains(&index) {
                    return Err(MachineError::Invalid(
                        "work offset must be G54-G59.3".into(),
                    ));
                }
                if axes.is_empty() {
                    return Err(MachineError::Invalid("no axes selected".into()));
                }
                for axis in &axes {
                    self.profile
                        .machine_ini
                        .validate_axis_value(axis, 0.0)
                        .map_err(MachineError::Invalid)?;
                }
                let mut values = AxisMap::new();
                for axis in &axes {
                    values.insert(axis.clone(), 0.0);
                }
                self.touch_off(index, &values, &axes)
            }
            MachineCommand::SetWorkPosition { index, values } => {
                self.validate_write()?;
                if !(1..=9).contains(&index) {
                    return Err(MachineError::Invalid(
                        "work offset must be G54-G59.3".into(),
                    ));
                }
                if values.iter().next().is_none() {
                    return Err(MachineError::Invalid("no axes given".into()));
                }
                for axis in values.keys() {
                    if !self
                        .profile
                        .machine_ini
                        .axes
                        .iter()
                        .any(|config| config.name == axis)
                    {
                        return Err(MachineError::Invalid(format!("unknown axis {axis}")));
                    }
                }
                let axes: Vec<String> = values.keys().map(str::to_string).collect();
                self.touch_off(index, &values, &axes)
            }
            MachineCommand::SetG92Offset { values } => {
                self.validate_write()?;
                let parts = values
                    .iter()
                    .map(|(axis, value)| format!("{axis}{value:.6}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                self.issue_blocks(&[format!("G92 {parts}")])?;
                self.wait_complete(Duration::from_secs(2))?;
                self.persist_g92()
            }
            MachineCommand::SetToolTableRow {
                row,
                expected_revision,
            } => {
                self.validate_write()?;
                if expected_revision != self.tool_table.revision {
                    return Err(MachineError::RevisionConflict);
                }
                let tool = row.tool;
                self.tool_table
                    .upsert(row)
                    .map_err(|error| MachineError::Invalid(error.to_string()))?;
                self.tool_table
                    .save(expected_revision)
                    .map_err(|error| match error {
                        linc_core::tooltable::ToolTableError::RevisionConflict => {
                            MachineError::RevisionConflict
                        }
                        other => MachineError::Io(other.to_string()),
                    })?;
                self.builder.tool_table = self.tool_table.clone();
                self.nml_send(&NmlCommand::LoadToolTable(Some(
                    self.profile.machine_ini.tool_table.clone(),
                )))?;
                let status = self.poll_status()?;
                if status.io.tool_in_spindle.max(0) as u16 == tool {
                    self.issue_blocks(&[format!("G43 H{tool}")])?;
                }
                Ok(())
            }
            MachineCommand::ApplyProbeResult { result, target } => {
                self.validate_write()?;
                match target {
                    ProbeTarget::WorkOffset(index) => {
                        self.write_work_offset(index, &result.measured, None)
                    }
                    ProbeTarget::Calibration => {
                        let offset = result.measured.values().first().copied().or(Some(0.0));
                        self.builder.set_calibration_offset(offset);
                        if let Some(stored) = self.last_snapshot.as_mut() {
                            stored.probe.calibration_offset = offset;
                        }
                        Ok(())
                    }
                }
            }
            MachineCommand::ResetProbeCalibration => {
                self.validate_write()?;
                self.builder.set_calibration_offset(None);
                if let Some(stored) = self.last_snapshot.as_mut() {
                    stored.probe.calibration_offset = None;
                }
                self.issue_blocks(&["o<probe_cal_reset> call".to_string()])
            }
            MachineCommand::AbortProgram => {
                self.nml_send(&NmlCommand::Abort)?;
                self.pending_restore = None;
                self.pending_height_comp = None;
                Ok(())
            }
            MachineCommand::ClearTrail => {
                self.builder.clear_trail();
                Ok(())
            }
            MachineCommand::SetHalOutput { output, value } => {
                if !value.matches(output) {
                    return Err(MachineError::Invalid("wrong HAL value type".into()));
                }
                match output {
                    LincOutput::ToolChangeConfirm => {
                        let snapshot = self.last_snapshot.clone().unwrap_or_else(|| self.refresh());
                        if snapshot.tool_change.status != ToolChangeStatus::ManualPrompt
                            && !matches!(value, HalValue::Bit(false))
                        {
                            return Err(MachineError::NoPendingToolChange);
                        }
                        if let HalValue::Bit(true) = value {
                            self.confirm_seq += 1;
                            self.builder.confirm_tool_change();
                        } else {
                            self.builder.cancel_tool_confirm();
                        }
                    }
                    LincOutput::JogIncrement => {
                        if let HalValue::Float(value) = value {
                            if let Some(snapshot) = self.last_snapshot.as_mut() {
                                snapshot.hal.jog_increment =
                                    if value > 0.0 { Some(value) } else { None };
                            }
                        }
                    }
                    LincOutput::JogSpeed => {
                        if let HalValue::Float(value) = value {
                            if let Some(snapshot) = self.last_snapshot.as_mut() {
                                snapshot.hal.jog_speed = Some(value);
                            }
                        }
                    }
                    LincOutput::HmiAlive | LincOutput::StatusDisplay => {}
                }
                Ok(())
            }
        }
    }
}

impl Drop for NmlBackend {
    fn drop(&mut self) {
        // The interpreter only writes this file when it exits. Persisting here
        // keeps a normal HMI shutdown from losing the last offset changes.
        if let Err(error) = self.parameters.save() {
            log::warn!("could not persist work offsets: {error}");
        }
    }
}

impl MachineBackend for NmlBackend {
    fn snapshot(&mut self) -> MachineSnapshot {
        self.refresh()
    }

    fn send(&mut self, command: MachineCommand) -> Result<(), MachineError> {
        let started = Instant::now();
        self.bridge.requests += 1;
        self.bridge.last_request = Some(command.label().to_string());
        let result = self.dispatch(command);
        let elapsed = started.elapsed().as_millis() as u64;
        self.bridge.last_latency_ms = elapsed;
        self.bridge.max_latency_ms = self.bridge.max_latency_ms.max(elapsed);
        match &result {
            Ok(()) => {
                self.bridge.replies += 1;
                self.bridge.last_reply = Some("ok".into());
            }
            Err(error) => {
                self.bridge.errors += 1;
                self.bridge.last_reply = Some(error.to_string());
                log::warn!(
                    "{} rejected: {error}",
                    self.bridge.last_request.as_deref().unwrap_or("command")
                );
            }
        }
        result
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

/// Does the file carry a `P` word on any tool line?
fn tool_table_has_p(path: &Path) -> bool {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines().any(|line| {
                line.split(';')
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .any(|word| word.chars().next().is_some_and(|ch| ch == 'P' || ch == 'p'))
            })
        })
        .unwrap_or(false)
}

pub(crate) fn load_tool_table(profile: &Profile) -> (ToolTableState, ToolTable) {
    match ToolTable::load(
        &profile.machine_ini.tool_table,
        &profile.machine_ini.tool_table_columns,
        profile.changer,
    ) {
        Ok(mut table) => {
            // Older LiNC versions wrote manual tables without P; LinuxCNC then
            // keeps only the last tool, so every T word fails as "not in the
            // tool table". Rewrite it once with the column present.
            if !profile.changer.uses_pockets() && !tool_table_has_p(&table.path) {
                let revision = table.revision;
                match table.save(revision) {
                    Ok(()) => log::info!(
                        "tool table {}: added the P column LinuxCNC needs",
                        table.path.display()
                    ),
                    Err(error) => log::warn!(
                        "tool table {}: could not add the P column: {error}",
                        table.path.display()
                    ),
                }
            }
            (table.state(), table)
        }
        Err(error) => {
            let state = ToolTableState {
                path: profile.machine_ini.tool_table.clone(),
                columns: profile
                    .machine_ini
                    .tool_table_columns
                    .chars()
                    .filter_map(ToolColumn::parse)
                    .collect(),
                changer: profile.changer,
                error: Some(error.to_string()),
                ..Default::default()
            };
            (
                state,
                ToolTable {
                    path: profile.machine_ini.tool_table.clone(),
                    columns: Vec::new(),
                    rows: Vec::new(),
                    revision: 0,
                    changer: profile.changer,
                },
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_script_is_gone() {
        assert!(!Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/python/linc_bridge.py"
        ))
        .exists());
    }
}
