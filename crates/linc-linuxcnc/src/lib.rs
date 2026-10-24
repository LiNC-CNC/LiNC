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
mod probe_messages;
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
use snapshot::{
    carry_hal_selections, compare_row, expected_row, initial_snapshot, ProbeProvenance,
    SnapshotBuilder,
};

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
    offset_sync_protect_logged: bool,
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
            offset_sync_protect_logged: false,
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
        self.sync_work_offsets(&status, &panel);

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
                Ok(Some(message)) => {
                    if let Some((parameter, value)) =
                        probe_messages::parse_probe_message(message.kind, &message.text)
                    {
                        self.builder.push_probe_value(parameter, value);
                    } else {
                        if message.kind.is_error() {
                            self.builder.fail_probe();
                        }
                        self.builder.push_message(message.kind, &message.text);
                    }
                }
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

    /// Publish the parameter file's row for the active system when the live
    /// row does not match it.
    ///
    /// A blank-`OPEN_FILE` start leaves the interpreter's restored `G5X`
    /// row unpublished, and neither an interpreter reset nor selecting the
    /// already-active origin reloads it. A complete stored-row `G10 L2` (every
    /// stored axis plus the stored rotation) is the deliberate write that
    /// publishes it, and the live row is read back to confirm the result.
    ///
    /// That write changes machine state, so it is refused, and retried later,
    /// while PROTECT is on or unknown. It never runs with a program open, so
    /// the separate program-open restoration path cannot be disturbed.
    fn sync_work_offsets(&mut self, status: &NmlStatus, panel: &PanelState) {
        if self.offsets_synced || self.offset_sync_attempts >= MAX_OFFSET_SYNCS {
            return;
        }
        if Instant::now() < self.offset_sync_next {
            return;
        }
        let axes = self.axis_names();
        let protect = !panel.program_protect_known || panel.program_protect;
        let inputs =
            OffsetSyncInputs::from_status(status, self.profile.machine_ini.axes.len(), protect);
        match plan_offset_sync(&inputs, &self.parameters, &axes) {
            OffsetSync::Done(reason) => {
                if !self.offsets_synced {
                    log::info!("stored work offsets: {reason}");
                }
                self.offsets_synced = true;
            }
            OffsetSync::Wait(reason) => {
                if protect && !self.offset_sync_protect_logged {
                    log::warn!(
                        "stored work offsets are not published while PROTECT is on or unknown"
                    );
                    self.offset_sync_protect_logged = true;
                } else {
                    log::debug!("stored work offsets are not published yet: {reason}");
                }
            }
            OffsetSync::Publish {
                index,
                block,
                expected,
                rotation,
            } => {
                let attempt = self.offset_sync_attempts + 1;
                if attempt == 1 {
                    if self.parameters.active_index() != index {
                        // The parameter file records another system as active.
                        // Restoring it is LinuxCNC's business, not a silent
                        // WCS change by the HMI.
                        log::info!(
                            "the parameter file records {} as the active system; the controller is in {}",
                            wcs_name(self.parameters.active_index()),
                            wcs_name(index)
                        );
                    }
                    log::info!("publishing stored {} offsets: {block}", wcs_name(index));
                }
                self.offset_sync_attempts = attempt;
                self.offset_sync_next = Instant::now() + Duration::from_secs(2);
                if let Err(error) = self.issue_blocks(&[block]) {
                    log::warn!(
                        "could not publish stored {} offsets: {error}",
                        wcs_name(index)
                    );
                    return;
                }
                let _ = self.client.wait_complete(Duration::from_secs(2));
                let Ok(now) = self.client.poll_status() else {
                    return;
                };
                if row_matches(
                    &now.offsets.g5x_offset,
                    now.offsets.rotation_xy,
                    &axes,
                    &expected,
                    rotation,
                ) {
                    self.offsets_synced = true;
                    log::info!("stored {} offsets are now live", wcs_name(index));
                } else {
                    log::warn!(
                        "stored {} offsets did not reach the live row (attempt {} of {MAX_OFFSET_SYNCS})",
                        wcs_name(index),
                        self.offset_sync_attempts
                    );
                }
            }
        }
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
                    // Fail closed before the block reaches the controller.
                    // The UI applies the same action rules client-side; both
                    // gates are independent and neither replaces the other.
                    let snapshot = self.refresh();
                    if probe_write_blocked(&block, snapshot.writes_allowed()) {
                        return Err(MachineError::ProgramProtected);
                    }
                    // Provenance is read here, before the block can run: a
                    // DIRECT routine rewrites the active row itself.
                    self.builder.note_probe_block(
                        Some(block.clone()),
                        ProbeProvenance::from_snapshot(&snapshot),
                    );
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
                let ProbeTarget::WorkOffset(index) = target else {
                    return Err(MachineError::Invalid(
                        "calibration is stored by the calibration routine, not Apply".into(),
                    ));
                };
                let snapshot = self.refresh();
                result
                    .apply_error(snapshot.offsets.active_index)
                    .map_err(MachineError::Invalid)?;
                let current = snapshot
                    .probe
                    .last_result
                    .as_ref()
                    .ok_or_else(|| MachineError::Invalid("no current probe result".into()))?;
                if current != &result {
                    return Err(MachineError::Invalid(
                        "stale probe result; run the probe again".into(),
                    ));
                }
                if snapshot.offsets.active_index != Some(index) {
                    return Err(MachineError::Invalid(
                        "Apply target must be the active work offset".into(),
                    ));
                }
                let operation = linc_core::probe::operation(&result.operation)
                    .ok_or_else(|| MachineError::Invalid("unknown probe operation".into()))?;
                let results = linc_core::probe::ParameterResults::from_values(&result.values);
                let block = operation
                    .apply_block(index, &results, result.rotation)
                    .map_err(MachineError::Invalid)?;
                self.issue_blocks(&[block])?;
                self.wait_complete(Duration::from_secs(2))?;

                // A completed block is not evidence. Read the row back and
                // compare it with what the Apply intended.
                let snapshot = self.refresh();
                if snapshot.offsets.active_index != Some(index) {
                    return Err(MachineError::Backend(format!(
                        "the controller is no longer in {}; the apply was not verified",
                        wcs_name(index)
                    )));
                }
                let Some(live) = snapshot
                    .offsets
                    .offsets
                    .iter()
                    .find(|offset| offset.index == index)
                else {
                    return Err(MachineError::Backend(
                        "no live work offset row to verify the apply against".into(),
                    ));
                };
                let expected = expected_row(
                    operation,
                    &results,
                    &result.source_offsets,
                    &result.auxiliary,
                );
                let problems = compare_row(&expected, result.rotation, live);
                if !problems.is_empty() {
                    return Err(MachineError::Backend(format!(
                        "controller did not take probe apply: {}",
                        problems.join(", ")
                    )));
                }
                // Edge Angle corner/start anchors exist only inside the
                // interpreter, so those axes are reported as unverified rather
                // than described as numerically read back.
                if !expected.unverified.is_empty() {
                    log::info!(
                        "probe apply {}: {} not numerically verified (controller-side anchors)",
                        operation.id,
                        expected.unverified.join("/")
                    );
                }
                self.builder.mark_probe_result_applied(&result);
                self.builder.clear_probe_error();
                self.capture_live_offsets();
                self.save_parameters()
            }
            MachineCommand::ResetProbeCalibration => {
                self.validate_write()?;
                // Never pre-publish the reset. The displayed offset stays
                // controller-owned until the routine's own #3032 message
                // confirms the new value; an unconfirmed reset changes nothing.
                self.issue_blocks(&["o<probe_cal_reset> call".to_string()])
            }
            MachineCommand::AbortProgram => {
                self.nml_send(&NmlCommand::Abort)?;
                self.builder.fail_probe();
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

/// Number of startup attempts at publishing the stored work offsets.
const MAX_OFFSET_SYNCS: u32 = 5;

/// What startup offset publication needs from one status poll.
pub(crate) struct OffsetSyncInputs {
    pub program_open: bool,
    pub estop: bool,
    pub task_on: bool,
    pub idle: bool,
    pub probing: bool,
    pub homed: bool,
    pub live_index: i32,
    pub live: linuxcnc_nml::Pose,
    pub live_rotation: f64,
    pub protect: bool,
}

impl OffsetSyncInputs {
    pub(crate) fn from_status(status: &NmlStatus, axis_count: usize, protect: bool) -> Self {
        Self {
            program_open: !status.task.file.trim().is_empty(),
            estop: status.io.estop,
            task_on: status.task.state == NmlTaskState::On,
            idle: status.task.interp_state == linuxcnc_nml::InterpState::Idle,
            probing: status.motion.probing,
            homed: status.joints.len() >= axis_count
                && status
                    .joints
                    .iter()
                    .take(axis_count)
                    .all(|joint| joint.homed),
            live_index: status.offsets.g5x_index,
            live: status.offsets.g5x_offset,
            live_rotation: status.offsets.rotation_xy,
            protect,
        }
    }
}

/// What one startup poll should do about the stored work offsets.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum OffsetSync {
    /// Nothing to publish, or the live row already matches; stop checking.
    Done(&'static str),
    /// Not ready yet; check again on a later poll.
    Wait(&'static str),
    /// Publish this complete stored row, then verify the live result.
    Publish {
        index: u8,
        block: String,
        expected: AxisMap<f64>,
        rotation: f64,
    },
}

/// Decide how to publish the stored row for the active work system.
///
/// The target is the active index the controller reports; an invalid index is
/// read as G54, which is where LinuxCNC starts and therefore the row its live
/// `g5x_offset` field describes. The stored `#5220` value is not used to
/// switch coordinate systems behind the operator's back.
pub(crate) fn plan_offset_sync(
    inputs: &OffsetSyncInputs,
    parameters: &ParameterFile,
    axes: &[String],
) -> OffsetSync {
    // The program-open path restores the parameter file and publishes its G5X
    // offsets itself. Never touch a session that has a file open.
    if inputs.program_open {
        return OffsetSync::Done("a program is open");
    }
    if inputs.estop || !inputs.task_on {
        return OffsetSync::Wait("the machine is not ON");
    }
    if !inputs.idle || inputs.probing {
        return OffsetSync::Wait("the interpreter is busy");
    }
    if !inputs.homed {
        return OffsetSync::Wait("axes are not homed");
    }
    if inputs.protect {
        return OffsetSync::Wait("PROTECT is on");
    }
    let index = if (1..=9).contains(&inputs.live_index) {
        inputs.live_index as u8
    } else {
        1
    };
    if !parameters.has_work_offset(index, axes.len()) {
        return OffsetSync::Done("no stored offsets");
    }

    let mut parts = vec![format!("G10 L2 P{index}")];
    let mut expected = AxisMap::new();
    for (axis_index, axis) in axes.iter().enumerate() {
        let Some(value) = parameters.work_axis(index, axis_index) else {
            continue;
        };
        parts.push(format!("{axis}{value:.6}"));
        expected.insert(axis.clone(), value);
    }
    // The rotation belongs to the row: publishing only the axes would leave a
    // stale angle behind.
    let rotation = parameters.rotation(index);
    parts.push(format!("R{rotation:.6}"));
    if row_matches(
        &inputs.live,
        inputs.live_rotation,
        axes,
        &expected,
        rotation,
    ) {
        return OffsetSync::Done("the live row already matches");
    }
    OffsetSync::Publish {
        index,
        block: parts.join(" "),
        expected,
        rotation,
    }
}

/// Does the live row carry every stored value for `index`?
pub(crate) fn row_matches(
    live: &linuxcnc_nml::Pose,
    live_rotation: f64,
    axes: &[String],
    expected: &AxisMap<f64>,
    rotation: f64,
) -> bool {
    if expected.iter().next().is_none() {
        return false;
    }
    for (axis_index, axis) in axes.iter().enumerate() {
        let Some(want) = expected.get(axis) else {
            continue;
        };
        if (pose_component(live, axis_index) - want).abs() > 1e-6 {
            return false;
        }
    }
    (live_rotation - rotation).abs() <= 1e-6
}

/// Is this probe block a write that `writes_allowed` forbids?
///
/// The action rules live in [`linc_core::probe::probe_write_action`] and are
/// shared with the simulator (and meant to be shared with the UI), so the
/// backends cannot drift. Measurement-only POSITION runs stay allowed in every
/// state; every classified action is refused while PROTECT is on or unknown,
/// before any MDI reaches the controller.
pub(crate) fn probe_write_blocked(block: &str, writes_allowed: bool) -> bool {
    linc_core::probe::probe_write_action(block).is_some() && !writes_allowed
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
    use linc_core::probe::ProbeParameters;

    #[test]
    fn helper_script_is_gone() {
        assert!(!Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/python/linc_bridge.py"
        ))
        .exists());
    }

    fn run_block(operation: &str, mode: linc_core::probe::ProbeMode) -> String {
        let mut parameters = ProbeParameters::default();
        parameters.probe_mode = mode;
        parameters.mdi_text(linc_core::probe::operation(operation).unwrap())
    }

    fn all_probe_blocks() -> Vec<(&'static str, String)> {
        let mut blocks = vec![
            (
                "direct",
                run_block("edge_x_plus", linc_core::probe::ProbeMode::ApplyWcs),
            ),
            (
                "direct-angle",
                run_block(
                    "top_front_edge_angle",
                    linc_core::probe::ProbeMode::ApplyWcs,
                ),
            ),
            (
                "edge-angle-position",
                run_block(
                    "top_front_edge_angle",
                    linc_core::probe::ProbeMode::MeasureOnly,
                ),
            ),
            (
                "calibration-run",
                run_block("cal_round_boss", linc_core::probe::ProbeMode::MeasureOnly),
            ),
            (
                "spindle-nose-position",
                run_block("spindle_nose", linc_core::probe::ProbeMode::MeasureOnly),
            ),
            ("calibration-reset", "o<probe_cal_reset> call".to_string()),
            (
                "parameter-update",
                ProbeParameters::default().parameter_update_block(),
            ),
            ("reset-all", "o<reset_all_data> call".to_string()),
            ("reset-x", "o<x_data_reset> call".to_string()),
            ("reset-y", "o<y_data_reset> call".to_string()),
            ("unknown-mode", "o<probe_x_plus_wco> call".to_string()),
        ];
        // Controls that must stay allowed.
        blocks.push((
            "position-measure-only",
            run_block("edge_x_plus", linc_core::probe::ProbeMode::MeasureOnly),
        ));
        blocks.push((
            "position-boss",
            run_block("round_boss", linc_core::probe::ProbeMode::MeasureOnly),
        ));
        blocks.push(("context-only", "o<linc_probe_context> call".to_string()));
        blocks
    }

    const WRITE_BLOCKS: [&str; 11] = [
        "direct",
        "direct-angle",
        "edge-angle-position",
        "calibration-run",
        "spindle-nose-position",
        "calibration-reset",
        "parameter-update",
        "reset-all",
        "reset-x",
        "reset-y",
        "unknown-mode",
    ];

    #[test]
    fn native_gate_refuses_every_write_action_and_allows_measure_only() {
        use linc_core::probe::{probe_write_action, ProbeWriteAction};
        let blocks = all_probe_blocks();
        for (name, block) in &blocks {
            let is_write = WRITE_BLOCKS.contains(name);
            // PROTECT on and PROTECT unknown both land on writes_allowed=false.
            assert_eq!(probe_write_blocked(block, false), is_write, "{name}");
            // With writes allowed the gate never refuses.
            assert!(!probe_write_blocked(block, true), "{name}");
            assert_eq!(probe_write_action(block).is_some(), is_write, "{name}");
        }
        // Every action has an explicit classification, so a future routine
        // cannot silently fall into "measure-only".
        let action = |block: &str| probe_write_action(block);
        assert_eq!(
            action(&run_block(
                "edge_x_plus",
                linc_core::probe::ProbeMode::ApplyWcs
            )),
            Some(ProbeWriteAction::DirectWco)
        );
        assert_eq!(
            action(&run_block(
                "top_front_edge_angle",
                linc_core::probe::ProbeMode::ApplyWcs
            )),
            Some(ProbeWriteAction::DirectWco),
            "DIRECT wins over the angle label"
        );
        assert_eq!(
            action(&run_block(
                "top_front_edge_angle",
                linc_core::probe::ProbeMode::MeasureOnly
            )),
            Some(ProbeWriteAction::EdgeAngleRotation)
        );
        assert_eq!(
            action(&run_block(
                "cal_round_boss",
                linc_core::probe::ProbeMode::MeasureOnly
            )),
            Some(ProbeWriteAction::CalibrationRun)
        );
        // spindle_nose is calibration-flagged even though its category is
        // SurfaceZ, because probe_spindle_nose.ngc:65-68 writes #3010/#3007.
        assert_eq!(
            action(&run_block(
                "spindle_nose",
                linc_core::probe::ProbeMode::MeasureOnly
            )),
            Some(ProbeWriteAction::CalibrationRun),
            "spindle_nose writes controller parameters in POSITION"
        );
        assert_eq!(
            action("o<probe_cal_reset> call"),
            Some(ProbeWriteAction::CalibrationReset)
        );
        assert_eq!(
            action(&ProbeParameters::default().parameter_update_block()),
            Some(ProbeWriteAction::ParameterUpdate)
        );
        for block in [
            "o<reset_all_data> call",
            "o<x_data_reset> call",
            "o<y_data_reset> call",
        ] {
            assert_eq!(
                action(block),
                Some(ProbeWriteAction::ResultReset),
                "{block}"
            );
        }
        assert_eq!(
            action("o<probe_x_plus_wco> call"),
            Some(ProbeWriteAction::UnknownRun)
        );
        assert_eq!(action("G0 X0"), None);
        assert_eq!(action("M2"), None);
    }

    /// Both backends must answer exactly the same way for the same block with
    /// the same PROTECT state, so the two gates cannot drift apart.
    #[test]
    fn native_and_simulator_agree_on_probe_write_gating() {
        use linc_core::command::MdiOrigin;
        use linc_core::sim::SimBackend;
        let profile_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../configs/sim.toml");
        let profile = Profile::load(&profile_path, None).expect("test profile");
        for (protect_on, protect_known) in [(true, true), (false, false), (false, true)] {
            let mut sim = SimBackend::new(profile.clone(), PanelMode::Mdi);
            sim.set_program_protect(protect_on, protect_known);
            let writes_allowed = !protect_on && protect_known;
            for (name, block) in all_probe_blocks() {
                let native_refuses = probe_write_blocked(&block, writes_allowed);
                let refused = matches!(
                    sim.send(MachineCommand::IssueMdi {
                        block: block.clone(),
                        origin: MdiOrigin::Probe,
                    }),
                    Err(MachineError::ProgramProtected)
                );
                if WRITE_BLOCKS.contains(&name) && !writes_allowed {
                    assert!(refused, "{name}: simulator let a write through PROTECT");
                    assert!(native_refuses, "{name}: native gate disagrees");
                } else {
                    assert!(!refused, "{name}: refused although writes are allowed");
                    assert!(!native_refuses, "{name}: native gate disagrees");
                }
            }
        }
    }

    fn offset_inputs() -> OffsetSyncInputs {
        OffsetSyncInputs {
            program_open: false,
            estop: false,
            task_on: true,
            idle: true,
            probing: false,
            homed: true,
            live_index: 1,
            live: linuxcnc_nml::Pose::default(),
            live_rotation: 0.0,
            protect: false,
        }
    }

    fn parameters_with(rows: &[(u32, f64)]) -> ParameterFile {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rs274ngc.var");
        let text = rows
            .iter()
            .map(|(key, value)| format!("{key}\t{value:.6}"))
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(&path, text).unwrap();
        ParameterFile::load(Some(path))
    }

    fn g54_row() -> Vec<(u32, f64)> {
        vec![(5220, 1.0), (5221, -10.0), (5222, -20.0), (5223, -30.0)]
    }

    fn g55_row() -> Vec<(u32, f64)> {
        vec![
            (5220, 2.0),
            (5241, -11.5),
            (5242, -22.5),
            (5243, -0.25),
            (5250, 30.0),
        ]
    }

    fn axes3() -> Vec<String> {
        vec!["X".to_string(), "Y".to_string(), "Z".to_string()]
    }

    fn published(sync: &OffsetSync) -> (u8, &str) {
        match sync {
            OffsetSync::Publish { index, block, .. } => (*index, block.as_str()),
            other => panic!("expected a publication, got {other:?}"),
        }
    }

    #[test]
    fn startup_publication_writes_the_complete_stored_row() {
        let parameters = parameters_with(&g54_row());
        let sync = plan_offset_sync(&offset_inputs(), &parameters, &axes3());
        let (index, block) = published(&sync);
        assert_eq!(index, 1);
        assert_eq!(
            block,
            "G10 L2 P1 X-10.000000 Y-20.000000 Z-30.000000 R0.000000"
        );
    }

    #[test]
    fn invalid_live_index_publishes_g54() {
        let parameters = parameters_with(&g54_row());
        let mut inputs = offset_inputs();
        inputs.live_index = 0;
        let sync = plan_offset_sync(&inputs, &parameters, &axes3());
        let (index, block) = published(&sync);
        // The controller's live row is G54's; the stored #5220 value does not
        // silently switch coordinate systems.
        assert_eq!(index, 1);
        assert!(block.starts_with("G10 L2 P1 "), "{block}");
    }

    #[test]
    fn stale_live_values_of_the_active_system_are_republished() {
        // G55 with stored offsets, live row still at zero.
        let parameters = parameters_with(&g55_row());
        let mut inputs = offset_inputs();
        inputs.live_index = 2;
        let sync = plan_offset_sync(&inputs, &parameters, &axes3());
        let (index, block) = published(&sync);
        assert_eq!(index, 2);
        assert_eq!(
            block,
            "G10 L2 P2 X-11.500000 Y-22.500000 Z-0.250000 R30.000000"
        );
    }

    #[test]
    fn matching_live_row_needs_no_write() {
        let parameters = parameters_with(&g55_row());
        let mut inputs = offset_inputs();
        inputs.live_index = 2;
        inputs.live.x = -11.5;
        inputs.live.y = -22.5;
        inputs.live.z = -0.25;
        inputs.live_rotation = 30.0;
        assert_eq!(
            plan_offset_sync(&inputs, &parameters, &axes3()),
            OffsetSync::Done("the live row already matches")
        );
    }

    #[test]
    fn a_stale_rotation_alone_republishes_the_row() {
        let parameters = parameters_with(&g55_row());
        let mut inputs = offset_inputs();
        inputs.live_index = 2;
        inputs.live.x = -11.5;
        inputs.live.y = -22.5;
        inputs.live.z = -0.25;
        // Axes match, the stored rotation does not.
        let sync = plan_offset_sync(&inputs, &parameters, &axes3());
        let (_, block) = published(&sync);
        assert!(block.ends_with("R30.000000"), "{block}");
    }

    #[test]
    fn protect_holds_the_publication() {
        let parameters = parameters_with(&g54_row());
        let mut inputs = offset_inputs();
        inputs.protect = true;
        assert_eq!(
            plan_offset_sync(&inputs, &parameters, &axes3()),
            OffsetSync::Wait("PROTECT is on")
        );
        // Releasing PROTECT resumes the normal path.
        inputs.protect = false;
        assert!(matches!(
            plan_offset_sync(&inputs, &parameters, &axes3()),
            OffsetSync::Publish { .. }
        ));
    }

    #[test]
    fn an_open_program_keeps_the_startup_path_out_of_the_session() {
        let parameters = parameters_with(&g54_row());
        let mut inputs = offset_inputs();
        inputs.program_open = true;
        assert_eq!(
            plan_offset_sync(&inputs, &parameters, &axes3()),
            OffsetSync::Done("a program is open")
        );
    }

    #[test]
    fn startup_waits_until_the_machine_can_take_a_write() {
        let parameters = parameters_with(&g54_row());
        let cases: [(fn(&mut OffsetSyncInputs), &str); 3] = [
            (|inputs| inputs.estop = true, "the machine is not ON"),
            (|inputs| inputs.homed = false, "axes are not homed"),
            (|inputs| inputs.probing = true, "the interpreter is busy"),
        ];
        for (mutate, reason) in cases {
            let mut inputs = offset_inputs();
            mutate(&mut inputs);
            assert_eq!(
                plan_offset_sync(&inputs, &parameters, &axes3()),
                OffsetSync::Wait(reason)
            );
        }
    }

    #[test]
    fn a_parameter_file_without_the_active_row_is_left_alone() {
        let parameters = parameters_with(&[(5220, 1.0), (1030, 5.0)]);
        assert_eq!(
            plan_offset_sync(&offset_inputs(), &parameters, &axes3()),
            OffsetSync::Done("no stored offsets")
        );
    }
}
