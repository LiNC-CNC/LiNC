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

use std::path::{Component, Path, PathBuf};
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
    axis_pose_index, carry_hal_selections, compare_row, expected_row, initial_snapshot,
    ProbeProvenance, SnapshotBuilder,
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
    /// Said once per session: the active row is unpublished, so the stored one
    /// is kept instead of being replaced by zeros.
    offset_unpublished_logged: bool,
    /// Last state of the PROG.HOME keys, for the rising edge that issues the
    /// block. One bool per entry of `linc_core::homing::proghome_keys`.
    proghome_prev: Vec<bool>,
    confirm_seq: u64,
    tool_table: ToolTable,
    /// The last nonzero `motion.program-line`: the line the trajectory planner
    /// is cutting. The pin reads 0 for a cycle whenever the queue drains, so the
    /// last value is kept until the cut moves on - and dropped when a different
    /// program is loaded, whose first move has a line of its own.
    program_line: usize,
    program_line_revision: u64,
    /// Said once per session: which source drives the displayed line.
    program_line_logged: bool,
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
            offset_unpublished_logged: false,
            proghome_prev: Vec::new(),
            confirm_seq: 0,
            program_line: 0,
            program_line_revision: 0,
            program_line_logged: false,
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

        // The trajectory planner's executing id, published by motion as a HAL
        // pin: the one exact answer to "which line is being cut". Measured on
        // the mill, `task.motion_line` is 0 through a whole program, and
        // `task.current_line` runs ahead of the cut by the motion queue's depth,
        // so a long program sat at its own end instead of following the tool.
        let revision = self.builder.program_revision();
        if self.program_line_revision != revision {
            self.program_line = 0;
            self.program_line_revision = revision;
        }
        if let Some(hal) = self.hal.as_ref() {
            match hal.s32("motion.program-line") {
                Some(line) if line > 0 => {
                    if !self.program_line_logged {
                        self.program_line_logged = true;
                        log::info!(
                            "following motion.program-line: the line the planner is cutting"
                        );
                    }
                    self.program_line = line as usize;
                }
                Some(_) => {}
                None => {
                    if !self.program_line_logged {
                        self.program_line_logged = true;
                        log::warn!(
                            "no motion.program-line pin; the displayed line comes from                              task.current_line, which runs ahead of the cut"
                        );
                    }
                }
            }
        }
        let executing_line = (self.program_line > 0).then_some(self.program_line);
        let mut snapshot =
            self.builder
                .build(&status, &panel, spindles, &self.parameters, executing_line);
        if let Some(stored) = self.last_snapshot.as_mut() {
            carry_hal_selections(&mut snapshot, stored);
        }
        // A subroutine runs from its own file. The controller then reports that
        // file's line numbers (`task.callLevel` is above zero and the
        // interpreter's line is in the routine), but it publishes only the
        // loaded program's name. Keep the program's own line so the listing does
        // not jump to a line number that means something else, and publish the
        // reported line separately: the UI names the routine from the preview's
        // subroutine bodies.
        let call_level = status.task.call_level.max(0) as usize;
        snapshot.program.call_level = call_level;
        if call_level > 0 {
            snapshot.program.sub_line = executing_line
                .unwrap_or(snapshot.program.current_line)
                .max(1);
            if let Some(previous) = self.last_snapshot.as_ref() {
                snapshot.program.current_line = previous.program.current_line;
                snapshot.program.motion_line = previous.program.motion_line;
                snapshot.program.current_block = previous.program.current_block.clone();
            }
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
    /// Publish the parameter file's work offsets when the controller does not
    /// have them.
    ///
    /// The invariant is not "we published once" but "the controller still has
    /// the stored row": a startup race - the G10 sent before the task was ready,
    /// which timed out on the first session and left G54 at all zeros - shows up
    /// minutes later as a machine cutting in the wrong coordinate system. So a
    /// synced session keeps watching, slowly, and publishes again if the row has
    /// been lost.
    ///
    /// It does not fight the operator: a row that merely *differs* is their
    /// touch-off, mirrored into the parameter file by the UI. Only a row that
    /// has lost every stored value is republished.
    fn sync_work_offsets(&mut self, status: &NmlStatus, panel: &PanelState) {
        if Instant::now() < self.offset_sync_next {
            return;
        }
        if !self.offsets_synced && self.offset_sync_attempts >= MAX_OFFSET_SYNCS {
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
                // Keep an eye on it, slowly.
                self.offset_sync_next = Instant::now() + OFFSET_WATCH;
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
                self.offset_sync_next = Instant::now() + OFFSET_WATCH;
            }
            OffsetSync::Publish {
                index,
                block,
                expected,
                rotation,
            } => {
                let lost = live_row_lost(&inputs.live, inputs.live_rotation, &axes, &expected);
                if self.offsets_synced && !lost {
                    // The operator's row, not ours: the parameter file follows
                    // the controller through the UI, so there is nothing to do.
                    log::debug!(
                        "the live {} row differs from the stored one; leaving it to the operator",
                        wcs_name(index)
                    );
                    self.offset_sync_next = Instant::now() + OFFSET_WATCH;
                    return;
                }
                let attempt = self.offset_sync_attempts + 1;
                if lost {
                    log::warn!(
                        "the controller has lost the stored {} row; publishing it again",
                        wcs_name(index)
                    );
                    // A lost row is not an operator edit, so it does not consume
                    // the publish budget: keep correcting it, slowly.
                    self.offset_sync_next = Instant::now() + OFFSET_WATCH;
                } else {
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
                }
                if let Err(error) = self.issue_blocks(&[block]) {
                    log::warn!(
                        "could not publish stored {} offsets: {error}",
                        wcs_name(index)
                    );
                    // The command may have landed late: ask the controller before
                    // deciding it failed. Returning here is what let the first
                    // session claim a row that was never published.
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
                    if !lost && self.offset_sync_attempts >= MAX_OFFSET_SYNCS {
                        log::warn!(
                            "giving up on publishing the stored {} row; the table shows the controller's row, and the stored one is kept in the parameter file",
                            wcs_name(index)
                        );
                    }
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
        contained_nc_file(self.profile.nc_files_root(), path).ok_or(MachineError::UnsafePath)
    }

    fn validate_ready(&mut self) -> Result<MachineSnapshot, MachineError> {
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
        if snapshot.probe_trip_active() {
            return Err(MachineError::ProbeTripped);
        }
        Ok(snapshot)
    }

    fn validate_issue(&mut self, origin: MdiOrigin) -> Result<(), MachineError> {
        let snapshot = self.validate_ready()?;
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

    /// A controller write that must also fail closed on a latched probe trip.
    ///
    /// `SaveProgram` keeps using `validate_write`: writing the file and
    /// reopening it does not move the machine, and losing an operator's buffer
    /// would be worse than letting them save it while the fault is latched.
    fn validate_controller_write(&mut self) -> Result<MachineSnapshot, MachineError> {
        let snapshot = self.validate_write()?;
        if snapshot.probe_trip_active() {
            return Err(MachineError::ProbeTripped);
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
                pose_component(
                    &status.offsets.g5x_offset,
                    axis_pose_index(axis).unwrap_or(axis_index),
                ),
            );
        }
        let stored = self.parameters.work_offset(index, &axes);
        let stored_rotation = self.parameters.rotation(index);
        if unpublished_live_row(
            &values,
            status.offsets.rotation_xy,
            &stored,
            stored_rotation,
        ) {
            if !self.offset_unpublished_logged {
                log::warn!(
                    "{} is not published by the interpreter; keeping the stored row ({}) instead of mirroring zeros",
                    wcs_name(index),
                    stored
                        .iter()
                        .map(|(axis, value)| format!("{axis}{value:.4}"))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
                self.offset_unpublished_logged = true;
            }
            // Not a capture: ask the startup path to publish the stored row
            // again, as it does on a fresh controller. It converges on the next
            // poll where the machine is idle, ready and unprotected.
            self.offsets_synced = false;
            self.offset_sync_attempts = 0;
            self.offset_sync_next = Instant::now();
            return;
        }
        self.parameters.record_offset_axes(index, &values, &axes);
        self.parameters
            .record_rotation(index, status.offsets.rotation_xy);
    }

    /// One block per PROG.HOME key press.
    ///
    /// The keys are not jogs: each asks for the block the homing table names for
    /// its pin. halui's `mdi-command` pins would run them, but they need the task
    /// in MDI, and asking for MDI from the dial position stranded jogging and
    /// homing in every other mode. So the HMI issues the block itself, on the
    /// key's rising edge, through the same path as its other MDI writes:
    /// `issue_blocks` switches to MDI and puts the panel's mode back.
    ///
    /// A refused press is logged and dropped, never queued: a key pressed with
    /// the machine off or a program running must not fire later.
    fn service_proghome_keys(&mut self) {
        let keys = linc_core::homing::proghome_keys();
        if self.proghome_prev.len() != keys.len() {
            self.proghome_prev = vec![false; keys.len()];
        }
        let pressed: Vec<bool> = {
            let Some(hal) = self.hal.as_ref() else {
                return;
            };
            keys.iter()
                .map(|(pin, _)| hal.bit(pin).unwrap_or(false))
                .collect()
        };
        let rising = rising_edges(&self.proghome_prev, &pressed);
        self.proghome_prev = pressed;
        for index in rising {
            let (pin, block) = keys[index];
            if let Err(error) = self.validate_ready() {
                log::warn!("{pin} ignored: {error}");
                continue;
            }
            log::info!("{pin}: {block}");
            if let Err(error) = self.issue_blocks(&[block.to_string()]) {
                log::warn!("{pin} could not start: {error}");
            }
        }
    }

    fn save_parameters(&mut self) -> Result<(), MachineError> {
        self.parameters.save().map_err(MachineError::Backend)
    }

    /// Open `canonical` in the interpreter, publishing the live offsets first.
    ///
    /// `ProgramOpen` resets the interpreter, and the active `G5X` row it leaves
    /// behind is all zero until the program publishes one. The live row is
    /// written to the parameter file before the reset; opening without that
    /// step replaced a stored row with zeros.
    fn open_program(&mut self, canonical: PathBuf) -> Result<(), MachineError> {
        self.capture_live_offsets();
        self.save_parameters()?;
        self.nml_send(&NmlCommand::ProgramOpen(canonical))?;
        self.wait_complete(Duration::from_secs(2))?;
        Ok(())
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
                        pose_component(
                            &status.offsets.g5x_offset,
                            axis_pose_index(axis).unwrap_or(axis_index),
                        ),
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
                    pose_component(
                        &status.offsets.g5x_offset,
                        axis_pose_index(axis).unwrap_or(axis_index),
                    ),
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
                    let pose_index = axis_pose_index(axis).unwrap_or(axis_index);
                    let machine = pose_component(&status.positions.actual, pose_index);
                    let g92 = pose_component(&status.offsets.g92_offset, pose_index);
                    let tool = pose_component(&status.offsets.tool_offset, pose_index);
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
                pose_component(
                    &status.offsets.g92_offset,
                    axis_pose_index(axis).unwrap_or(axis_index),
                ),
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
                if self.refresh().probe_trip_active() {
                    return Err(MachineError::ProbeTripped);
                }
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
                self.open_program(canonical)?;
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
                let canonical = contained_nc_write_target(self.profile.nc_files_root(), &path)
                    .ok_or(MachineError::UnsafePath)?;
                let parent = canonical.parent().ok_or(MachineError::UnsafePath)?;
                let mut temp = tempfile::NamedTempFile::new_in(parent)?;
                use std::io::Write;
                temp.write_all(content.as_bytes())?;
                temp.as_file().sync_all()?;
                temp.persist(&canonical)
                    .map_err(|error| MachineError::Io(error.error.to_string()))?;
                // The interpreter keeps the program it opened: the write
                // changed the file on disk, not what CYCLE START runs. Open
                // the file just written so the save is the program AUTO
                // starts, the way the simulator's save replaces its program.
                self.open_program(canonical)?;
                Ok(())
            }
            MachineCommand::SetWorkOffset {
                index,
                values,
                rotation,
            } => {
                self.validate_controller_write()?;
                self.write_work_offset(index, &values, rotation)
            }
            MachineCommand::ActivateWorkOffset { index } => {
                // Selecting a coordinate system is not an offset write, so it
                // does not need PROTECT off; it does need a ready, idle
                // machine. `issue_blocks` switches to MDI and restores the
                // panel's mode, as a tool change does.
                self.validate_ready()?;
                if !(1..=9).contains(&index) {
                    return Err(MachineError::Invalid(
                        "work offset must be G54-G59.3".into(),
                    ));
                }
                self.issue_blocks(&[wcs_name(index)])?;
                let _ = self.client.wait_complete(Duration::from_secs(2));
                let status = self.poll_status()?;
                if (status.offsets.g5x_index as u8).clamp(1, 9) != index {
                    return Err(MachineError::Backend(format!(
                        "the controller did not activate {}",
                        wcs_name(index)
                    )));
                }
                self.parameters.record_active_index(index);
                self.save_parameters()?;
                Ok(())
            }
            MachineCommand::RunFromLine { line } => {
                self.validate_controller_write()?;
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
                self.validate_controller_write()?;
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
                self.validate_controller_write()?;
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
                self.validate_controller_write()?;
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
                old_tool,
                expected_revision,
            } => {
                self.validate_controller_write()?;
                if expected_revision != self.tool_table.revision {
                    return Err(MachineError::RevisionConflict);
                }
                let status = self.poll_status()?;
                let active_tool = status.io.tool_in_spindle.max(0) as u16;
                if let Some(old) = old_tool.filter(|old| *old != row.tool) {
                    if old == self.profile.config.probe.tool_number {
                        return Err(MachineError::Invalid(format!(
                            "T{old} is the probe tool and cannot be renumbered"
                        )));
                    }
                    if old == active_tool {
                        return Err(MachineError::Invalid(format!(
                            "T{old} is the active tool and cannot be renumbered"
                        )));
                    }
                }
                let tool = row.tool;
                let mut table = self.tool_table.clone();
                if let Some(old) = old_tool.filter(|old| *old != row.tool) {
                    table
                        .remove(old)
                        .map_err(|error| MachineError::Invalid(error.to_string()))?;
                }
                table
                    .upsert(row)
                    .map_err(|error| MachineError::Invalid(error.to_string()))?;
                table.save(expected_revision).map_err(|error| match error {
                    linc_core::tooltable::ToolTableError::RevisionConflict => {
                        MachineError::RevisionConflict
                    }
                    other => MachineError::Io(other.to_string()),
                })?;
                self.tool_table = table;
                self.builder.tool_table = self.tool_table.clone();
                self.nml_send(&NmlCommand::LoadToolTable(Some(
                    self.profile.machine_ini.tool_table.clone(),
                )))?;
                if active_tool == tool {
                    self.issue_blocks(&[format!("G43 H{tool}")])?;
                }
                Ok(())
            }
            MachineCommand::DeleteToolTableRow {
                tool,
                expected_revision,
            } => {
                self.validate_controller_write()?;
                if expected_revision != self.tool_table.revision {
                    return Err(MachineError::RevisionConflict);
                }
                if tool == self.profile.config.probe.tool_number {
                    return Err(MachineError::Invalid(format!(
                        "T{tool} is the probe tool and cannot be deleted"
                    )));
                }
                let active_tool = self.poll_status()?.io.tool_in_spindle.max(0) as u16;
                if tool == active_tool {
                    return Err(MachineError::Invalid(format!(
                        "T{tool} is the active tool and cannot be deleted"
                    )));
                }
                self.tool_table
                    .remove(tool)
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
                Ok(())
            }
            MachineCommand::ApplyProbeResult { result, target } => {
                self.validate_controller_write()?;
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
                self.validate_controller_write()?;
                // Never pre-publish the reset. The displayed offset stays
                // controller-owned until the routine's own #3032 message
                // confirms the new value; an unconfirmed reset changes nothing.
                self.issue_blocks(&["o<probe_cal_reset> call".to_string()])
            }
            MachineCommand::AbortProgram => {
                self.nml_send(&NmlCommand::Abort)?;
                // EMC_TASK_ABORT is the controller reset that clears the motion
                // error a probe trip sets. This is the only clear path.
                self.builder.clear_probe_trip();
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
                if matches!(output, LincOutput::JogIncrement | LincOutput::JogSpeed)
                    && self
                        .last_snapshot
                        .as_ref()
                        .is_some_and(MachineSnapshot::probe_trip_active)
                {
                    return Err(MachineError::ProbeTripped);
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
                    // The worker owns these: it publishes them from its own
                    // snapshot and nothing here has state to mirror.
                    LincOutput::HmiAlive | LincOutput::StatusDisplay | LincOutput::MachineOff => {}
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
        // The PROG.HOME keys ride on the same poll the HMI already makes: one
        // block per press, no HAL pin and no mode request.
        self.service_proghome_keys();
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
/// How often a synced session re-checks that the controller still has the
/// stored work offsets. Slow on purpose: it is a safety net, not a heartbeat.
const OFFSET_WATCH: Duration = Duration::from_secs(5);

const MAX_OFFSET_SYNCS: u32 = 5;

/// Is this live row the interpreter's *unpublished* row rather than an offset
/// of zero?
///
/// After a reset, after a blank `OPEN_FILE` start and after an aborted
/// program, the interpreter leaves the active `G5X` row at zero while the
/// parameter file still holds the operator's numbers. The two are numerically
/// identical, so the only evidence is the file: all-zero live over a stored row
/// that is set means the row has not been published yet.
///
/// Mirroring that zero into the file is what lost the active system's offset:
/// `LoadProgram` captures the live row just before it opens a file, so opening
/// a program after such a reset replaced the stored row with zeros.
fn unpublished_live_row(
    live: &AxisMap<f64>,
    live_rotation: f64,
    stored: &AxisMap<f64>,
    stored_rotation: f64,
) -> bool {
    let live_is_zero =
        live.iter().all(|(_, value)| value.abs() < 1e-9) && live_rotation.abs() < 1e-9;
    let stored_is_set =
        stored.iter().any(|(_, value)| value.abs() >= 1e-9) || stored_rotation.abs() >= 1e-9;
    live_is_zero && stored_is_set
}

/// What startup offset publication needs from one status poll.
pub(crate) struct OffsetSyncInputs {
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
    if !parameters.has_work_offset(index) {
        return OffsetSync::Done("no stored offsets");
    }

    let mut parts = vec![format!("G10 L2 P{index}")];
    let mut expected = AxisMap::new();
    for axis in axes {
        let Some(value) = parameters.work_axis(index, axis) else {
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
/// Which keys went from released to pressed since the last poll.
///
/// A press is the edge, not the level: a held PROG.HOME key must issue its block
/// once, not on every poll.
fn rising_edges(previous: &[bool], now: &[bool]) -> Vec<usize> {
    now.iter()
        .enumerate()
        .filter(|(index, pressed)| **pressed && !previous.get(*index).copied().unwrap_or(false))
        .map(|(index, _)| index)
        .collect()
}

/// Has the controller lost every stored value in the live row?
///
/// Not "does it differ": a touch-off (G10 L20 P1 X0 after setting an edge) is the
/// operator's row and legitimately differs. All stored axes reading zero is the
/// signature of a row the controller never received - a G10 that timed out
/// because the task was not ready yet - and that row must be published again.
fn live_row_lost(
    live: &linuxcnc_nml::Pose,
    _live_rotation: f64,
    axes: &[String],
    expected: &AxisMap<f64>,
) -> bool {
    let mut lost = false;
    for (axis_index, axis) in axes.iter().enumerate() {
        let Some(want) = expected.get(axis) else {
            continue;
        };
        if want.abs() <= 1e-9 {
            continue;
        }
        let index = axis_pose_index(axis).unwrap_or(axis_index);
        if pose_component(live, index).abs() > 1e-6 {
            return false;
        }
        lost = true;
    }
    lost
}

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
        if (pose_component(live, axis_pose_index(axis).unwrap_or(axis_index)) - want).abs() > 1e-6 {
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

/// A file inside `NC_FILES`, without resolving symbolic links.
///
/// Canonicalising the request was wrong: a standard LinuxCNC nc_files tree is
/// full of symlinks (examples, ngcgui_lib, remap_lib -> /usr/share/linuxcnc/
/// ncfiles), so every path reached through one resolved outside the root and
/// was rejected as unsafe - and LinuxCNC itself loads those paths happily.
/// Containment is decided lexically instead: `.` and `..` are folded without
/// touching the filesystem, so a path cannot escape the root by spelling, while
/// a symlinked directory inside it is allowed.
fn contained_nc_file(root: &Path, requested: &Path) -> Option<PathBuf> {
    let root = normalize_lexically(root);
    let candidate = normalize_lexically(requested);
    if !candidate.starts_with(&root) || !candidate.is_file() {
        return None;
    }
    Some(candidate)
}

/// The path a save writes to.
///
/// A save may create the file, so the candidate itself need not exist; the same
/// lexical containment rule as `contained_nc_file` applies, plus the parent must
/// be an existing directory. An entry that already exists must be a regular
/// file and not a symlink: writing through a link would land wherever it points
/// rather than in `NC_FILES`.
fn contained_nc_write_target(root: &Path, requested: &Path) -> Option<PathBuf> {
    let root = normalize_lexically(root);
    let candidate = normalize_lexically(requested);
    if !candidate.starts_with(&root) || candidate == root {
        return None;
    }
    if !candidate.parent()?.is_dir() {
        return None;
    }
    match std::fs::symlink_metadata(&candidate) {
        Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => return None,
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return None,
    }
    Some(candidate)
}

/// Fold `.` and `..` segments without resolving symlinks or touching the disk.
fn normalize_lexically(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                // Only pop a normal segment; `..` above `/` stays put.
                if out
                    .components()
                    .next_back()
                    .is_some_and(|last| matches!(last, Component::Normal(_)))
                {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod nc_containment_tests {
    use super::{contained_nc_file, contained_nc_write_target, normalize_lexically};
    use std::path::PathBuf;

    fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("linc-nc-{}", std::process::id()));
        let root = dir.join("nc_files");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(
            root.join("sub").join("part.ngc"),
            "G0 X0
",
        )
        .unwrap();
        std::fs::write(
            root.join("top.ngc"),
            "G0 X0
",
        )
        .unwrap();
        root
    }

    #[test]
    fn a_file_in_a_subdirectory_is_allowed() {
        let root = temp_root();
        let found = contained_nc_file(&root, &root.join("sub").join("part.ngc"));
        assert!(found.is_some(), "files in a subdirectory must load");
    }

    #[test]
    fn a_symlinked_directory_inside_the_root_is_allowed() {
        let root = temp_root();
        // A standard nc_files tree has these: examples -> /usr/share/linuxcnc/ncfiles
        let outside = root.parent().unwrap().join("shared");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(
            outside.join("demo.ngc"),
            "G0 X0
",
        )
        .unwrap();
        let link = root.join("examples");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let found = contained_nc_file(&root, &link.join("demo.ngc"));
        assert!(
            found.is_some(),
            "paths reached through a symlink inside NC_FILES must load"
        );
    }

    #[test]
    fn a_path_that_escapes_the_root_is_rejected() {
        let root = temp_root();
        let outside = root.parent().unwrap().join("elsewhere.ngc");
        std::fs::write(
            &outside, "G0 X0
",
        )
        .unwrap();
        assert!(contained_nc_file(&root, &outside).is_none());
        let dotted = root.join("sub").join("..").join("..").join("elsewhere.ngc");
        assert!(
            contained_nc_file(&root, &dotted).is_none(),
            ".. must not escape the root"
        );
    }

    #[test]
    fn lexical_folding_keeps_the_path_spelling() {
        assert_eq!(
            normalize_lexically(&PathBuf::from("/a/b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(
            normalize_lexically(&PathBuf::from("/a/./b")),
            PathBuf::from("/a/b")
        );
    }

    /// A save names a file that does not exist yet: the target must be accepted
    /// inside NC_FILES and still refused outside it.
    #[test]
    fn a_save_target_may_be_new_but_must_stay_inside_the_root() {
        let root = temp_root();
        let fresh = root.join("new-part.ngc");
        assert!(!fresh.exists());
        assert_eq!(
            contained_nc_write_target(&root, &fresh),
            Some(fresh.clone())
        );

        // An existing regular file is still a valid target.
        let existing = root.join("top.ngc");
        assert_eq!(contained_nc_write_target(&root, &existing), Some(existing));

        // Outside the root, and the root itself, are not targets.
        let outside = root.parent().unwrap().join("new-escape.ngc");
        assert!(contained_nc_write_target(&root, &outside).is_none());
        assert!(
            contained_nc_write_target(&root, &root.join("sub").join("..").join("..").join("x.ngc"))
                .is_none(),
            ".. must not escape the root"
        );
        assert!(contained_nc_write_target(&root, &root).is_none());

        // A directory is not a file to write, and a symlink is not followed.
        assert!(contained_nc_write_target(&root, &root.join("sub")).is_none());
        let link = root.join("link.ngc");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(
            contained_nc_write_target(&root, &link).is_none(),
            "writing through a link would leave NC_FILES"
        );
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

    fn row(values: &[(&str, f64)]) -> AxisMap<f64> {
        AxisMap::from_entries(
            values
                .iter()
                .map(|(axis, value)| (axis.to_string(), *value)),
        )
    }

    /// An unpublished live row is zeros, and zeros are also a legitimate
    /// offset, so the file is the only evidence: all-zero live over a stored
    /// row that is set is an unpublished row, not an offset that became zero.
    /// Mirroring it is what replaced the active system's offset with zeros.
    #[test]
    fn an_unpublished_live_row_is_not_mirrored_over_the_stored_one() {
        let stored = row(&[("X", -10.0), ("Y", -20.0), ("Z", -30.0)]);
        let live_zero = row(&[("X", 0.0), ("Y", 0.0), ("Z", 0.0)]);
        assert!(
            unpublished_live_row(&live_zero, 0.0, &stored, 0.0),
            "the interpreter has not published the row"
        );

        // The published row mirrors as before: this is the probe path, where
        // the interpreter itself moved the offset.
        let live_set = row(&[("X", -10.0), ("Y", -20.0), ("Z", -30.0)]);
        assert!(!unpublished_live_row(&live_set, 0.0, &stored, 0.0));

        // A published row with a zero axis is an offset, not an unpublished
        // row: one non-zero axis is enough.
        let partly_zero = row(&[("X", 0.0), ("Y", -20.0), ("Z", 0.0)]);
        assert!(!unpublished_live_row(&partly_zero, 0.0, &stored, 0.0));

        // Nothing stored: writing zeros over zeros loses nothing.
        let stored_zero = row(&[("X", 0.0), ("Y", 0.0), ("Z", 0.0)]);
        assert!(!unpublished_live_row(&live_zero, 0.0, &stored_zero, 0.0));

        // A rotation-only offset is the same hazard as an axis one.
        assert!(
            unpublished_live_row(&live_zero, 0.0, &stored_zero, 30.0),
            "the stored rotation would be replaced by zero"
        );
        assert!(!unpublished_live_row(&live_zero, 30.0, &stored_zero, 30.0));
    }

    /// A PROG.HOME key issues one block per press: holding it must not walk the
    /// axis to zero one block per poll.
    #[test]
    fn a_proghome_key_issues_once_per_press() {
        assert!(rising_edges(&[false, false], &[false, false]).is_empty());
        assert_eq!(rising_edges(&[false, false], &[true, false]), vec![0]);
        assert!(
            rising_edges(&[true, false], &[true, false]).is_empty(),
            "a held key does not repeat"
        );
        assert!(rising_edges(&[true, true], &[false, false]).is_empty());
        assert_eq!(
            rising_edges(&[true, false], &[true, true]),
            vec![1],
            "the other key rising is its own press"
        );
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

    /// A *loaded* program is not a reason to leave the active row unpublished.
    ///
    /// It used to end the startup path outright ("the open path restores it
    /// itself"), so until the program ran, the interpreter still had G54 at
    /// zero: in manual mode a `G0 X0 Y0` then went to machine zero instead of
    /// work zero, and the graph drew the work origin on top of machine home.
    /// The OPEN_FILE transient is excluded by the idle check, not by a file.
    #[test]
    fn a_loaded_program_does_not_block_the_publication() {
        let parameters = parameters_with(&g54_row());
        let mut inputs = offset_inputs();
        inputs.idle = true;
        let sync = plan_offset_sync(&inputs, &parameters, &axes3());
        let (index, block) = published(&sync);
        assert_eq!(index, 1);
        assert_eq!(
            block,
            "G10 L2 P1 X-10.000000 Y-20.000000 Z-30.000000 R0.000000"
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
    /// A startup race leaves all zeros where the stored row should be, and a
    /// row that merely differs is the operator's touch-off. Only the first is
    /// republished.
    #[test]
    fn a_live_row_of_zeros_has_lost_the_stored_one() {
        let axes = vec!["X".to_string(), "Y".to_string(), "Z".to_string()];
        let mut expected = AxisMap::new();
        expected.insert("X".to_string(), -156.381004);
        expected.insert("Y".to_string(), -67.696033);
        expected.insert("Z".to_string(), -89.943998);

        let zeros = linuxcnc_nml::Pose::default();
        assert!(super::live_row_lost(&zeros, 0.0, &axes, &expected));

        // One axis holding its value means the row arrived.
        let mut partial = linuxcnc_nml::Pose::default();
        partial.x = -156.381004;
        assert!(!super::live_row_lost(&partial, 0.0, &axes, &expected));

        // An operator's touch-off is a different row, not a lost one.
        let mut touched = linuxcnc_nml::Pose::default();
        touched.x = -10.0;
        touched.y = 5.0;
        assert!(!super::live_row_lost(&touched, 0.0, &axes, &expected));

        // Nothing stored means nothing to lose.
        assert!(!super::live_row_lost(&zeros, 0.0, &axes, &AxisMap::new()));
    }
}
