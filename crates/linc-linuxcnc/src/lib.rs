//! LinuxCNC backend.
//!
//! The official `linuxcnc` module is used from a supervised Python helper.
//! The HMI only sees the typed `MachineBackend` boundary; the helper protocol
//! has no arbitrary HAL or NML escape hatch. PyO3 can replace this transport
//! without changing the UI.

use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use linc_core::backend::MachineBackend;
use linc_core::command::{
    HalValue, LincOutput, MachineCommand, MachineError, MdiOrigin, ProbeTarget,
};
use linc_core::config::Profile;
use linc_core::diag::BridgeInfo;
use linc_core::gcode::parse_preview;
use linc_core::model::*;
use linc_core::tooltable::ToolTable;
use serde::Deserialize;
use thiserror::Error;

const HELPER: &str = include_str!("../python/linc_bridge.py");

#[derive(Debug, Error)]
pub enum BridgeError {
    #[error("cannot spawn python3: {0}")]
    Spawn(String),
    #[error("bridge process is not running")]
    NotRunning,
    #[error("bridge protocol error: {0}")]
    Protocol(String),
    #[error("bridge command timed out")]
    Timeout,
    #[error("bridge command failed: {0}")]
    Command(String),
    #[error("I/O: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum BridgeMessage {
    Hello {
        #[serde(default)]
        ini: String,
    },
    Snapshot(BridgeSnapshot),
    Result(BridgeReply),
    Error {
        #[serde(default)]
        error: String,
    },
    Fatal {
        #[serde(default)]
        error: String,
    },
}

#[derive(Debug, Deserialize)]
struct BridgeReply {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct BridgeSnapshot {
    #[serde(default)]
    task: BridgeTask,
    #[serde(default)]
    machine: BridgeMachine,
    #[serde(default)]
    positions: Vec<BridgePosition>,
    #[serde(default)]
    offsets: BridgeOffsets,
    #[serde(default)]
    spindles: Vec<BridgeSpindle>,
    #[serde(default)]
    feed: BridgeFeed,
    #[serde(default)]
    program: BridgeProgram,
    #[serde(default)]
    panel: BridgePanel,
    #[serde(default)]
    tool: BridgeTool,
    #[serde(default)]
    probe: BridgeProbe,
    #[serde(default)]
    alarms: Vec<BridgeAlarm>,
    #[serde(default)]
    pendant: BridgePendant,
}

#[derive(Debug, Default, Deserialize)]
struct BridgeTask {
    #[serde(default)]
    power: String,
    #[serde(default)]
    mode: String,
    #[serde(default)]
    interp: String,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    motion_active: bool,
}

#[derive(Debug, Default, Deserialize)]
struct BridgeMachine {
    #[serde(default)]
    on: bool,
    #[serde(default)]
    estop: bool,
    #[serde(default)]
    homed: HashMap<String, bool>,
    #[serde(default)]
    homing: HashMap<String, bool>,
    #[serde(default)]
    limits: Vec<String>,
    #[serde(default)]
    fault: bool,
    #[serde(default)]
    force_homing: bool,
    #[serde(default)]
    following_error: HashMap<String, f64>,
    #[serde(default)]
    servo_load: HashMap<String, f64>,
    #[serde(default)]
    commanded: HashMap<String, f64>,
    #[serde(default)]
    actual: HashMap<String, f64>,
    #[serde(default)]
    coolant_flood: bool,
    #[serde(default)]
    coolant_mist: bool,
    #[serde(default)]
    lube: bool,
    #[serde(default)]
    lube_level_ok: bool,
    #[serde(default)]
    door_closed: bool,
    #[serde(default)]
    tool_change_active: bool,
    #[serde(default)]
    echo_serial: u64,
    #[serde(default)]
    nml_command_connected: bool,
    #[serde(default)]
    nml_status_connected: bool,
}

#[derive(Debug, Default, Deserialize)]
struct BridgePosition {
    #[serde(default)]
    axis: String,
    #[serde(default)]
    work: f64,
    #[serde(default)]
    machine: f64,
    #[serde(default)]
    dtg: f64,
    #[serde(default)]
    velocity: f64,
}

#[derive(Debug, Default, Deserialize)]
struct BridgeOffsets {
    #[serde(default)]
    active: u8,
    #[serde(default)]
    offsets: Vec<BridgeOffset>,
    #[serde(default)]
    g92: HashMap<String, f64>,
}

#[derive(Debug, Default, Deserialize)]
struct BridgeOffset {
    #[serde(default)]
    index: u8,
    #[serde(default)]
    values: HashMap<String, f64>,
    #[serde(default)]
    rotation: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct BridgeSpindle {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    speed: f64,
    #[serde(default)]
    override_factor: f64,
    #[serde(default)]
    direction: i8,
    #[serde(default)]
    on: bool,
    #[serde(default)]
    load: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct BridgeFeed {
    #[serde(default)]
    feed: f64,
    #[serde(default)]
    feed_override: f64,
    #[serde(default)]
    rapid_override: f64,
    #[serde(default)]
    spindle_override: f64,
    #[serde(default)]
    units: String,
    #[serde(default)]
    feed_mode: String,
    #[serde(default)]
    distance_mode: String,
    #[serde(default)]
    feed_override_enabled: bool,
    #[serde(default)]
    spindle_override_enabled: bool,
    #[serde(default)]
    adaptive_feed: bool,
}

#[derive(Debug, Default, Deserialize)]
struct BridgeProgram {
    #[serde(default)]
    path: String,
    #[serde(default)]
    current_line: usize,
    #[serde(default)]
    motion_line: usize,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    gcodes: Vec<f64>,
    #[serde(default)]
    mcodes: Vec<f64>,
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    running: bool,
}

#[derive(Debug, Default, Deserialize)]
struct BridgePanel {
    #[serde(default)]
    connected: bool,
    /// `halui.joint.0.override-limits`: the controller is ignoring limits.
    #[serde(default)]
    override_limits: bool,
    #[serde(default)]
    rapid: bool,
    #[serde(default)]
    error: bool,
    #[serde(default)]
    mode: BridgeMode,
    #[serde(default)]
    protect: bool,
    #[serde(default)]
    protect_known: bool,
    #[serde(default)]
    feed_override: f64,
    #[serde(default)]
    rapid_override: f64,
    #[serde(default)]
    spindle_override: f64,
    #[serde(default)]
    sseg_status: i32,
    #[serde(default)]
    estop: bool,
    #[serde(default)]
    next_tool: u16,
    #[serde(default)]
    buttons: u64,
    #[serde(default)]
    leds: u64,
    #[serde(default)]
    sseg_tool: i32,
    #[serde(default)]
    sseg_next: i32,
}

#[derive(Debug, Default, Deserialize)]
struct BridgeMode {
    #[serde(default)]
    valid: bool,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    active: Vec<String>,
}

#[derive(Debug, Default, Deserialize)]
struct BridgeTool {
    #[serde(default)]
    current: u16,
    #[serde(default)]
    prepared: u16,
    #[serde(default)]
    change_active: bool,
}

#[derive(Debug, Default, Deserialize)]
struct BridgeProbe {
    #[serde(default)]
    running: bool,
    #[serde(default)]
    tripped: bool,
    #[serde(default)]
    values: HashMap<String, f64>,
}

#[derive(Debug, Default, Deserialize)]
struct BridgeAlarm {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    text: String,
}

#[derive(Debug, Default, Deserialize)]
struct BridgePendant {
    #[serde(default)]
    connected: bool,
    #[serde(default)]
    jog_enable: bool,
    #[serde(default)]
    active_axis: Option<String>,
    #[serde(default)]
    handwheel_scale: Option<f64>,
    #[serde(default)]
    step_mode: Option<String>,
}

struct Shared {
    snapshot: Mutex<MachineSnapshot>,
    clear_trail: std::sync::atomic::AtomicBool,
    results: Mutex<HashMap<u64, Result<(), String>>>,
    condvar: Condvar,
    alive: Mutex<bool>,
    last_update: Mutex<Instant>,
    /// Last MDI block issued for a probe routine, so the result converter can
    /// interpret `#1001`-`#1009` with the routine that actually ran.
    last_probe_block: Mutex<Option<String>>,
    /// Request/reply counters, stderr tail and liveness for `linc_get_nml`.
    bridge: Mutex<BridgeInfo>,
}

/// LinuxCNC backend backed by the supervised Python helper.
pub struct PythonBridgeBackend {
    child: Child,
    stdin: Option<ChildStdin>,
    shared: Arc<Shared>,
    next_id: u64,
    profile: Profile,
    reader: Option<thread::JoinHandle<()>>,
    tool_table: ToolTable,
    tool_table_checked: Instant,
    program_revision: u64,
    confirm_seq: u64,
    last_mdi: Option<String>,
    last_mdi_result: Option<String>,
}

impl PythonBridgeBackend {
    pub fn spawn(profile: Profile) -> Result<Self, BridgeError> {
        let (tool_table_state, tool_table) = load_tool_table(&profile);
        let mut initial = initial_snapshot(&profile);
        initial.tool_table = tool_table_state;
        let shared = Arc::new(Shared {
            snapshot: Mutex::new(initial),
            clear_trail: std::sync::atomic::AtomicBool::new(false),
            results: Mutex::new(HashMap::new()),
            condvar: Condvar::new(),
            alive: Mutex::new(true),
            last_update: Mutex::new(Instant::now()),
            last_probe_block: Mutex::new(None),
            bridge: Mutex::new(BridgeInfo {
                kind: "python".into(),
                alive: true,
                ..BridgeInfo::default()
            }),
        });
        let mut child = Command::new("python3")
            .arg("-u")
            .arg("-c")
            .arg(HELPER)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("PYTHONUNBUFFERED", "1")
            .spawn()
            .map_err(|error| BridgeError::Spawn(error.to_string()))?;
        if let Ok(mut bridge) = shared.bridge.lock() {
            bridge.pid = child.id();
        }
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let shared_reader = shared.clone();
        let profile_reader = profile.clone();
        let reader = thread::Builder::new()
            .name("linc-linuxcnc-reader".into())
            .spawn(move || {
                if let Some(stdout) = stdout {
                    read_loop(stdout, shared_reader.clone(), profile_reader);
                }
                *shared_reader.alive.lock().unwrap() = false;
                if let Ok(mut bridge) = shared_reader.bridge.lock() {
                    bridge.alive = false;
                    bridge.fatal = Some("bridge process exited".into());
                }
                let mut snapshot = shared_reader.snapshot.lock().unwrap();
                snapshot.panel.connected = false;
                snapshot.panel.communication_error = true;
                snapshot.panel.mode = PanelModeValidity::Unknown;
                snapshot.alarms.push_front(Alarm::new(
                    9_999_999,
                    AlarmSeverity::Error,
                    AlarmSource::Nml,
                    "NML-LOST",
                    "LinuxCNC bridge process exited",
                ));
            })
            .map_err(|error| BridgeError::Spawn(error.to_string()))?;
        if let Some(stderr) = stderr {
            let shared_stderr = shared.clone();
            thread::Builder::new()
                .name("linc-linuxcnc-stderr".into())
                .spawn(move || {
                    for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                        if let Ok(mut bridge) = shared_stderr.bridge.lock() {
                            bridge.stderr_tail.push(line.clone());
                            if bridge.stderr_tail.len() > 20 {
                                bridge.stderr_tail.remove(0);
                            }
                        }
                        // The helper's stderr carries both routine reports and
                        // failures. Logging everything at warn made a normal
                        // startup ("the file has X, the controller Y") look like
                        // an error on the console.
                        if bridge_line_is_failure(&line) {
                            log::warn!("linuxcnc bridge: {line}");
                        } else {
                            log::info!("linuxcnc bridge: {line}");
                        }
                    }
                })
                .ok();
        }
        Ok(Self {
            child,
            stdin,
            shared,
            next_id: 1,
            profile,
            reader: Some(reader),
            tool_table,
            tool_table_checked: Instant::now(),
            program_revision: 1,
            confirm_seq: 0,
            last_mdi: None,
            last_mdi_result: None,
        })
    }

    fn refresh_tool_table(&mut self) {
        if self.tool_table_checked.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.tool_table_checked = Instant::now();
        if let Ok(table) = ToolTable::load(
            &self.profile.machine_ini.tool_table,
            &self.profile.machine_ini.tool_table_columns,
            self.profile.changer,
        ) {
            if table.revision != self.tool_table.revision {
                self.tool_table = table;
                let mut snapshot = self.shared.snapshot.lock().unwrap();
                snapshot.tool_table = self.tool_table.state();
            }
        }
    }

    fn request(
        &mut self,
        payload: serde_json::Value,
        timeout: Duration,
    ) -> Result<(), MachineError> {
        let id = self.next_id;
        self.next_id += 1;
        let mut payload = payload;
        payload["id"] = serde_json::json!(id);
        let command = payload
            .get("cmd")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown")
            .to_string();
        let started = Instant::now();
        self.update_bridge(|bridge| {
            bridge.requests += 1;
            bridge.last_request = Some(command);
        });
        if self.stdin.is_none() {
            self.update_bridge(|bridge| {
                bridge.errors += 1;
                bridge.last_reply = Some("bridge stdin is closed".into());
            });
            return Err(MachineError::NotConnected);
        }
        let line = serde_json::to_string(&payload)
            .map_err(|error| MachineError::Backend(error.to_string()))?;
        // The borrow of the pipe ends before the bridge counters are updated, so
        // a write failure is visible in `linc_get_nml` like a reply failure.
        let written = {
            let stdin = self.stdin.as_mut().expect("checked above");
            stdin
                .write_all(line.as_bytes())
                .and_then(|_| stdin.write_all(b"\n"))
                .and_then(|_| stdin.flush())
        };
        if let Err(error) = written {
            self.update_bridge(|bridge| {
                bridge.errors += 1;
                bridge.last_reply = Some(format!("write failed: {error}"));
            });
            return Err(MachineError::Backend(error.to_string()));
        }
        let deadline = Instant::now() + timeout;
        let mut results = self.shared.results.lock().unwrap();
        loop {
            if let Some(result) = results.remove(&id) {
                let latency_ms = started.elapsed().as_millis() as u64;
                self.update_bridge(|bridge| match &result {
                    Ok(()) => {
                        bridge.replies += 1;
                        bridge.last_latency_ms = latency_ms;
                        bridge.max_latency_ms = bridge.max_latency_ms.max(latency_ms);
                        bridge.last_reply = Some("ok".into());
                    }
                    Err(message) => {
                        bridge.errors += 1;
                        bridge.last_reply = Some(message.clone());
                    }
                });
                return result.map_err(MachineError::Backend);
            }
            let now = Instant::now();
            if now >= deadline {
                self.update_bridge(|bridge| {
                    bridge.errors += 1;
                    bridge.last_reply = Some("bridge command timed out".into());
                });
                return Err(MachineError::Backend("bridge command timed out".into()));
            }
            let (guard, _) = self
                .shared
                .condvar
                .wait_timeout(results, deadline - now)
                .unwrap();
            results = guard;
        }
    }

    fn update_bridge(&self, update: impl FnOnce(&mut BridgeInfo)) {
        if let Ok(mut bridge) = self.shared.bridge.lock() {
            update(&mut bridge);
        }
    }

    fn current(&self) -> MachineSnapshot {
        let mut snapshot = self.shared.snapshot.lock().unwrap().clone();
        if let Ok(mut bridge) = self.shared.bridge.lock() {
            bridge.alive = self.shared.alive.lock().map(|alive| *alive).unwrap_or(false);
            snapshot.bridge = Some(bridge.clone());
        }
        let stale = self
            .shared
            .last_update
            .lock()
            .map(|last| last.elapsed() > Duration::from_secs(1))
            .unwrap_or(true);
        if stale {
            snapshot.meta.stale = true;
            snapshot.panel.connected = false;
            snapshot.panel.communication_error = true;
            snapshot.panel.program_protect_known = false;
            snapshot.panel.mode = PanelModeValidity::Unknown;
        }
        snapshot
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

    fn validate_issue(&self, origin: MdiOrigin) -> Result<(), MachineError> {
        let snapshot = self.current();
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

    fn validate_write(&self) -> Result<MachineSnapshot, MachineError> {
        let snapshot = self.current();
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
}

impl Drop for PythonBridgeBackend {
    fn drop(&mut self) {
        if let Some(mut stdin) = self.stdin.take() {
            let _ = stdin.write_all(b"{\"cmd\":\"shutdown\"}\n");
            let _ = stdin.flush();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

impl MachineBackend for PythonBridgeBackend {
    fn snapshot(&mut self) -> MachineSnapshot {
        self.refresh_tool_table_snapshot();
        let mut snapshot = self.current();
        snapshot.hal.tool_change_confirm_seq = self.confirm_seq;
        snapshot
    }

    fn send(&mut self, command: MachineCommand) -> Result<(), MachineError> {
        self.refresh_tool_table();
        match command {
            MachineCommand::IssueMdi { block, origin } => {
                self.validate_issue(origin)?;
                let blocks: Vec<&str> = block
                    .lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .collect();
                if blocks.is_empty() {
                    return Err(MachineError::Invalid("empty MDI block".into()));
                }
                for line in &blocks {
                    // Syntax-gate the command with the bounded preview parser.
                    let axes: Vec<String> = self.profile.machine_ini.axis_names();
                    let preview = parse_preview(line, &axes, None, [0.0; 3]);
                    if let Some(error) = preview.error {
                        return Err(MachineError::Invalid(format!(
                            "line {}: {}",
                            error.line, error.message
                        )));
                    }
                }
                self.last_mdi = Some(block.clone());
                if origin == MdiOrigin::Probe {
                    if let Ok(mut last) = self.shared.last_probe_block.lock() {
                        *last = Some(block.clone());
                    }
                }
                let result = self.request(
                    serde_json::json!({
                        "cmd": "issue_mdi",
                        "origin": match origin {
                            MdiOrigin::Mdi => "mdi",
                            MdiOrigin::Probe => "probe",
                            MdiOrigin::ToolChange => "tool-change",
                            MdiOrigin::ManualToolChange => "manual-tool-change",
                            MdiOrigin::ProgHome => "prog-home",
                        },
                        "blocks": blocks,
                    }),
                    Duration::from_secs(5),
                );
                self.last_mdi_result = Some(format!("{result:?}"));
                result
            }
            MachineCommand::IssueToolChange { tool } => {
                self.validate_issue(MdiOrigin::ToolChange)?;
                self.last_mdi = Some(format!("T{tool} M6"));
                self.request(
                    serde_json::json!({ "cmd": "issue_tool_change", "tool": tool }),
                    Duration::from_secs(5),
                )
            }
            MachineCommand::LoadTool { tool } => {
                self.validate_issue(MdiOrigin::ToolChange)?;
                self.last_mdi = Some(format!("T{tool} M6\nG43 H{tool}"));
                self.request(
                    serde_json::json!({ "cmd": "load_tool", "tool": tool }),
                    Duration::from_secs(5),
                )
            }
            MachineCommand::ConfirmManualToolChange { token } => {
                let snapshot = self.current();
                if !snapshot.panel.connected {
                    return Err(MachineError::PanelOffline);
                }
                if !snapshot.panel.mode.is_valid() {
                    return Err(MachineError::InvalidPanelMode);
                }
                // No idle or running gate: the operator is changing the tool by
                // hand while the interpreter waits, and this only satisfies the
                // M6 handshake.
                if snapshot.tool_change.status != ToolChangeStatus::ManualPrompt {
                    return Err(MachineError::NoPendingToolChange);
                }
                if snapshot.tool_change.token.as_ref() != Some(&token) {
                    return Err(MachineError::StaleToolChangeToken);
                }
                self.confirm_seq += 1;
                let mut state = self.shared.snapshot.lock().unwrap();
                // Keep the prompt and the token: the request is still up, and
                // `iocontrol` clears it. The flag tells the operator the
                // confirmation has been sent.
                state.tool_change.confirmed = true;
                state.hal.tool_change_confirm = true;
                state.hal.tool_change_confirm_seq = self.confirm_seq;
                Ok(())
            }
            MachineCommand::ToggleLimitsOverride => self.request(
                serde_json::json!({ "cmd": "toggle_limits_override" }),
                Duration::from_secs(5),
            ),
            MachineCommand::SetCurrentTool { tool } => {
                self.validate_issue(MdiOrigin::ToolChange)?;
                self.request(
                    serde_json::json!({ "cmd": "set_current_tool", "tool": tool }),
                    Duration::from_secs(5),
                )
            }
            MachineCommand::LoadProgram {
                path,
                expected_revision,
            } => {
                if self.current().task.running {
                    return Err(MachineError::ProgramRunning);
                }
                let canonical = self.canonical_nc_file(&path)?;
                if let Some(expected) = expected_revision {
                    if expected != self.current().program.revision {
                        return Err(MachineError::RevisionConflict);
                    }
                }
                self.request(
                    serde_json::json!({
                        "cmd": "load_program",
                        "path": canonical.to_string_lossy(),
                    }),
                    Duration::from_secs(5),
                )
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
                temp.write_all(content.as_bytes())?;
                temp.as_file().sync_all()?;
                temp.persist(&canonical)
                    .map_err(|error| MachineError::Io(error.error.to_string()))?;
                self.program_revision += 1;
                Ok(())
            }
            MachineCommand::SetWorkOffset {
                index,
                values,
                rotation,
            } => {
                self.validate_write()?;
                if !(1..=9).contains(&index) {
                    return Err(MachineError::Invalid("work offset must be G54-G59.3".into()));
                }
                for (axis, value) in values.iter() {
                    self.profile
                        .machine_ini
                        .validate_axis_value(axis, *value)
                        .map_err(MachineError::Invalid)?;
                }
                let map: HashMap<&str, f64> =
                    values.iter().map(|(axis, value)| (axis, *value)).collect();
                self.request(
                    serde_json::json!({
                        "cmd": "set_work_offset",
                        "index": index,
                        "values": map,
                        "rotation": rotation,
                    }),
                    Duration::from_secs(5),
                )
            }
            MachineCommand::RunFromLine { line } => {
                self.validate_write()?;
                if line == 0 {
                    return Err(MachineError::Invalid("line must be 1 or greater".into()));
                }
                self.request(
                    serde_json::json!({ "cmd": "run_from_line", "line": line }),
                    Duration::from_secs(5),
                )
            }
            MachineCommand::ZeroWorkOffset { index, axes } => {
                self.validate_write()?;
                if !(1..=9).contains(&index) {
                    return Err(MachineError::Invalid("work offset must be G54-G59.3".into()));
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
                self.request(
                    serde_json::json!({
                        "cmd": "zero_work_offset",
                        "index": index,
                        "axes": axes,
                    }),
                    Duration::from_secs(5),
                )
            }
            MachineCommand::SetWorkPosition { index, values } => {
                self.validate_write()?;
                if !(1..=9).contains(&index) {
                    return Err(MachineError::Invalid("work offset must be G54-G59.3".into()));
                }
                if values.iter().next().is_none() {
                    return Err(MachineError::Invalid("no axes given".into()));
                }
                // The entered value is a work coordinate, not a machine
                // position: with a negative offset the work reading is routinely
                // outside the travel range, so only the axis name is checked.
                for axis in values.iter().map(|(axis, _)| axis) {
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
                let map: HashMap<&str, f64> =
                    values.iter().map(|(axis, value)| (axis, *value)).collect();
                self.request(
                    serde_json::json!({
                        "cmd": "set_work_position",
                        "index": index,
                        "values": map,
                    }),
                    Duration::from_secs(5),
                )
            }
            MachineCommand::SetG92Offset { values } => {
                self.validate_write()?;
                let map: HashMap<&str, f64> =
                    values.iter().map(|(axis, value)| (axis, *value)).collect();
                self.request(
                    serde_json::json!({ "cmd": "set_g92", "values": map }),
                    Duration::from_secs(5),
                )
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
                let mut snapshot = self.shared.snapshot.lock().unwrap();
                snapshot.tool_table = self.tool_table.state();
                drop(snapshot);
                // Reload the table into the controller, and re-apply the length
                // compensation when the edited row is the tool in the spindle:
                // G43 reads the table, so without this the DRO keeps the old
                // height until the next tool change.
                self.request(
                    serde_json::json!({ "cmd": "load_tool_table", "reapply": tool }),
                    Duration::from_secs(5),
                )
            }
            MachineCommand::ApplyProbeResult { result, target } => {
                self.validate_write()?;
                match target {
                    ProbeTarget::WorkOffset(index) => {
                        let map: HashMap<&str, f64> = result
                            .measured
                            .iter()
                            .map(|(axis, value)| (axis, *value))
                            .collect();
                        self.request(
                            serde_json::json!({
                                "cmd": "set_work_offset",
                                "index": index,
                                "values": map,
                                "rotation": null,
                            }),
                            Duration::from_secs(5),
                        )
                    }
                    ProbeTarget::Calibration => {
                        let mut snapshot = self.shared.snapshot.lock().unwrap();
                        snapshot.probe.calibration_offset =
                            result.measured.values().first().copied().or(Some(0.0));
                        Ok(())
                    }
                }
            }
            MachineCommand::ResetProbeCalibration => {
                self.validate_write()?;
                self.request(
                    serde_json::json!({ "cmd": "reset_probe_cal" }),
                    Duration::from_secs(5),
                )
            }
            MachineCommand::AbortProgram => self.request(
                serde_json::json!({ "cmd": "abort" }),
                Duration::from_secs(5),
            ),
            MachineCommand::ClearTrail => {
                self.shared
                    .clear_trail
                    .store(true, std::sync::atomic::Ordering::Relaxed);
                Ok(())
            }
            MachineCommand::SetHalOutput { output, value } => {
                if !value.matches(output) {
                    return Err(MachineError::Invalid("wrong HAL value type".into()));
                }
                match output {
                    LincOutput::ToolChangeConfirm => {
                        let snapshot = self.current();
                        if snapshot.tool_change.status != ToolChangeStatus::ManualPrompt
                            && !matches!(value, HalValue::Bit(false))
                        {
                            return Err(MachineError::NoPendingToolChange);
                        }
                        self.confirm_seq += 1;
                        self.shared
                            .snapshot
                            .lock()
                            .unwrap()
                            .hal
                            .tool_change_confirm_seq = self.confirm_seq;
                    }
                    LincOutput::JogIncrement => {
                        if let HalValue::Float(value) = value {
                            self.shared.snapshot.lock().unwrap().hal.jog_increment =
                                if value > 0.0 { Some(value) } else { None };
                        }
                    }
                    LincOutput::JogSpeed => {
                        if let HalValue::Float(value) = value {
                            self.shared.snapshot.lock().unwrap().hal.jog_speed = Some(value);
                        }
                    }
                    LincOutput::HmiAlive | LincOutput::StatusDisplay => {}
                }
                Ok(())
            }
        }
    }
}

impl PythonBridgeBackend {
    fn refresh_tool_table_snapshot(&self) {
        // Called from the UI thread only through `snapshot`; the file is read
        // at most once per second by the backend's own command path. Keep this
        // read-free to honor the no-blocking-I/O rule.
    }
}

/// Does the file carry a `P` word on any tool line?
fn tool_table_has_p(path: &std::path::Path) -> bool {
    std::fs::read_to_string(path)
        .map(|text| {
            text.lines().any(|line| {
                line.split(';')
                    .next()
                    .unwrap_or("")
                    .split_whitespace()
                    .any(|word| {
                        word.chars()
                            .next()
                            .is_some_and(|ch| ch == 'P' || ch == 'p')
                    })
            })
        })
        .unwrap_or(false)
}

fn load_tool_table(profile: &Profile) -> (ToolTableState, ToolTable) {
    match ToolTable::load(
        &profile.machine_ini.tool_table,
        &profile.machine_ini.tool_table_columns,
        profile.changer,
    ) {
        Ok(mut table) => {
            // Older LiNC versions wrote manual tables without P; LinuxCNC then
            // keeps only the last tool, so every `T` word fails as "not in the
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

fn initial_snapshot(profile: &Profile) -> MachineSnapshot {
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
            } else {
                format!("SPINDLE {index}")
            },
            commanded_speed: 0.0,
            actual_speed: 0.0,
            load: None,
            override_factor: 1.0,
            direction: 0,
            on: false,
            live_tool: false,
            live_tool_axial: false,
            live_tool_max_rpm: 0.0,
        });
    }
    snapshot.feed.units = profile.machine_ini.linear_units.clone();
    snapshot.panel.capabilities.transport = profile
        .config
        .panel
        .link
        .kind
        .clone()
        .unwrap_or_else(|| "hal".into());
    snapshot.panel.link.port = profile.config.panel.link.port.clone().unwrap_or_default();
    snapshot.panel.link.baud = profile.config.panel.link.baud.unwrap_or(115200);
    snapshot
}

fn read_loop(stdout: impl std::io::Read, shared: Arc<Shared>, profile: Profile) {
    let mut converter = Converter::new(profile);
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let message: BridgeMessage = match serde_json::from_str(&line) {
            Ok(message) => message,
            Err(error) => {
                log::warn!("linuxcnc bridge protocol: {error}: {line}");
                continue;
            }
        };
        match message {
            BridgeMessage::Hello { ini } => {
                log::info!("linuxcnc bridge connected: {ini}");
            }
            BridgeMessage::Snapshot(snapshot) => {
                if let Ok(mut last) = shared.last_update.lock() {
                    *last = Instant::now();
                }
                let probe_block = shared
                    .last_probe_block
                    .lock()
                    .ok()
                    .and_then(|block| block.clone());
                let mut converted = converter.convert(snapshot, probe_block);
                if shared
                    .clear_trail
                    .swap(false, std::sync::atomic::Ordering::Relaxed)
                {
                    let tip = converted.tool_tip.unwrap_or([0.0; 3]);
                    converted.live_trail = Arc::new(vec![tip]);
                    // The converter appends to its own buffer and rebuilds the
                    // snapshot from it on every poll, so patching the snapshot
                    // alone held only until the next conversion: the trail came
                    // straight back. Clear the buffer too.
                    converter.live_trail.clear();
                    converter.live_trail.push(tip);
                    converter.last_tool_tip = converted.tool_tip;
                }
                // The bridge rebuilds the snapshot from the panel service on
                // every poll, so the jog selections the HMI owns have to be
                // carried across or jogging drops back to zero speed.
                {
                    let mut stored = shared.snapshot.lock().unwrap();
                    carry_hal_selections(&mut converted, &stored);
                    *stored = converted;
                }
            }
            BridgeMessage::Result(reply) => {
                if let Some(id) = reply.id {
                    shared.results.lock().unwrap().insert(
                        id,
                        if reply.ok {
                            Ok(())
                        } else {
                            Err(reply.error.unwrap_or_else(|| "command failed".into()))
                        },
                    );
                    shared.condvar.notify_all();
                }
            }
            BridgeMessage::Error { error } => {
                log::error!("linuxcnc bridge error: {error}");
            }
            BridgeMessage::Fatal { error } => {
                log::error!("linuxcnc bridge fatal: {error}");
                break;
            }
        }
    }
    *shared.alive.lock().unwrap() = false;
}

struct Converter {
    profile: Profile,
    tool_table: ToolTable,
    tool_table_checked: Instant,
    program_text: Option<Arc<String>>,
    program_path: Option<PathBuf>,
    program_revision: u64,
    live_trail: Vec<[f64; 3]>,
    last_tool_tip: Option<[f64; 3]>,
    next_alarm_id: u64,
    last_alarm_texts: VecDeque<String>,
    last_probe_signature: Option<String>,
    /// One-shot guard for the axis-name mismatch warning below.
    warned_axis_mismatch: bool,
    /// Increments on the rising edge of `iocontrol.0.tool-change`, so the
    /// token identifies one change episode instead of the clock.
    tool_change_serial: u64,
    tool_change_active: bool,
    /// Controller messages, newest first. The snapshot is rebuilt from the
    /// bridge on every poll, so without this the log held only the messages
    /// discovered in the current poll and every page looked silent one frame
    /// after a message arrived.
    alarms: std::collections::VecDeque<Alarm>,
    /// Last offsets handed to the snapshot, plus a counter that changes with
    /// them. The HMI rebuilds its editable drafts when the counter moves, so a
    /// constant value left the table showing stale numbers after a `G10` write.
    offsets_last: Option<Vec<WorkOffset>>,
    offsets_revision: u64,
}

impl Converter {
    fn new(profile: Profile) -> Self {
        let (_state, tool_table) = load_tool_table(&profile);
        Self {
            profile,
            tool_table,
            tool_table_checked: Instant::now(),
            program_text: None,
            program_path: None,
            program_revision: 1,
            live_trail: Vec::new(),
            last_tool_tip: None,
            next_alarm_id: 1,
            last_alarm_texts: VecDeque::new(),
            last_probe_signature: None,
            warned_axis_mismatch: false,
            tool_change_serial: 0,
            tool_change_active: false,
            alarms: std::collections::VecDeque::new(),
            offsets_last: None,
            offsets_revision: 1,
        }
    }

    fn refresh_tool_table(&mut self) {
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

    fn convert(&mut self, dto: BridgeSnapshot, probe_block: Option<String>) -> MachineSnapshot {
        self.refresh_tool_table();
        let mut snapshot = initial_snapshot(&self.profile);
        snapshot.machine_name = self.profile.config.machine.name.clone();
        snapshot.simulated = self.profile.is_sim();
        snapshot.meta.captured_at_ms = now_ms();
        snapshot.meta.stale = false;

        snapshot.task.power = match dto.task.power.as_str() {
            "on" => MachinePower::On,
            "off" => MachinePower::Off,
            "estop" => MachinePower::Estop,
            "estop_reset" => MachinePower::EstopReset,
            _ => MachinePower::Unknown,
        };
        snapshot.task.mode = match dto.task.mode.as_str() {
            "auto" => TaskMode::Auto,
            "mdi" => TaskMode::Mdi,
            "manual" => TaskMode::Manual,
            _ => TaskMode::Unknown,
        };
        snapshot.task.interp = match dto.task.interp.as_str() {
            "idle" => InterpState::Idle,
            "reading" => InterpState::Reading,
            "paused" => InterpState::Paused,
            "waiting" => InterpState::Waiting,
            _ => InterpState::Unknown,
        };
        snapshot.task.paused = dto.task.paused;
        snapshot.task.running = dto.task.running;
        snapshot.task.motion_active = dto.task.motion_active;
        snapshot.machine.on = dto.machine.on;
        // E-stop is the union of the controller's latched state and the panel
        // mushroom. Reading only LinuxCNC's flag let the HMI show `E-STOP OK`
        // while the controller was still latched (the panel had been released
        // but `halui.estop.reset` had not cleared it yet), so the operator was
        // told the machine was ready when it was not. Failing towards ACTIVE is
        // the only safe direction.
        snapshot.machine.estop = dto.machine.estop || dto.panel.estop;
        snapshot.machine.fault = dto.machine.fault || dto.panel.estop;
        snapshot.machine.force_homing = dto.machine.force_homing;
        snapshot.machine.limits = dto.machine.limits;
        snapshot.machine.coolant_flood = dto.machine.coolant_flood;
        snapshot.machine.coolant_mist = dto.machine.coolant_mist;
        snapshot.machine.lube = dto.machine.lube;
        snapshot.machine.lube_level_ok = dto.machine.lube_level_ok;
        snapshot.machine.door_closed = dto.machine.door_closed;
        snapshot.machine.tool_change_active = dto.machine.tool_change_active;
        snapshot.machine.echo_serial = dto.machine.echo_serial;
        snapshot.machine.nml_command_connected = dto.machine.nml_command_connected;
        snapshot.machine.nml_status_connected = dto.machine.nml_status_connected;
        for (axis, value) in dto.machine.homed {
            snapshot.machine.homed.insert(axis, value);
        }
        for (axis, value) in dto.machine.homing {
            snapshot.machine.homing.insert(axis, value);
        }
        for (axis, value) in dto.machine.following_error {
            snapshot.machine.following_error.insert(axis, value);
        }
        for (axis, value) in dto.machine.servo_load {
            snapshot.machine.servo_load.insert(axis, value);
        }
        for (axis, value) in dto.machine.commanded {
            snapshot.machine.commanded.insert(axis, value);
        }
        for (axis, value) in dto.machine.actual {
            snapshot.machine.actual.insert(axis, value);
        }

        snapshot.positions.axes = AxisMap::new();
        for position in dto.positions {
            snapshot.positions.axes.insert(
                position.axis,
                PositionSet {
                    work: position.work,
                    machine: position.machine,
                    dtg: position.dtg,
                    velocity: position.velocity,
                },
            );
        }
        // A silent axis-name mismatch (for example an INI whose
        // `[TRAJ] COORDINATES` is the trivkins shorthand `XYZ`) makes every
        // position lookup miss, which shows up as a frozen 0.000 DRO and a
        // tool marker stuck at the origin. Say so instead of failing quietly.
        if !self.warned_axis_mismatch && !snapshot.positions.axes.is_empty() {
            let expected = self.profile.machine_ini.axis_names();
            let missing: Vec<&String> = expected
                .iter()
                .filter(|axis| snapshot.positions.axes.get(axis).is_none())
                .collect();
            if !missing.is_empty() {
                log::warn!(
                    "LinuxCNC bridge reported positions for {:?} but the INI declares {expected:?}; \
                     check [TRAJ] COORDINATES. Missing: {missing:?}",
                    snapshot.positions.axes.keys().collect::<Vec<_>>()
                );
                self.warned_axis_mismatch = true;
            }
        }
        snapshot.offsets.offsets = dto
            .offsets
            .offsets
            .into_iter()
            .map(|offset| WorkOffset {
                index: offset.index,
                values: map_values(offset.values),
                rotation: offset.rotation,
            })
            .collect();
        snapshot.offsets.g92 = map_values(dto.offsets.g92);
        snapshot.offsets.active_index = if (1..=9).contains(&dto.offsets.active) {
            Some(dto.offsets.active)
        } else {
            None
        };
        if self.offsets_last.as_ref() != Some(&snapshot.offsets.offsets) {
            self.offsets_last = Some(snapshot.offsets.offsets.clone());
            self.offsets_revision += 1;
        }
        snapshot.offsets.revision = self.offsets_revision;

        snapshot.spindles = dto
            .spindles
            .into_iter()
            .map(|spindle| SpindleState {
                index: spindle.index,
                name: if spindle.index == 0 {
                    "MAIN SPINDLE".into()
                } else if self.profile.variant == MachineVariant::Lathe && spindle.index > 0 {
                    "LIVE TOOL".into()
                } else {
                    format!("SPINDLE {}", spindle.index)
                },
                commanded_speed: spindle.speed,
                actual_speed: spindle.speed,
                load: spindle.load,
                override_factor: spindle.override_factor,
                direction: spindle.direction,
                on: spindle.on,
                live_tool: self.profile.variant == MachineVariant::Lathe && spindle.index > 0,
                live_tool_axial: false,
                live_tool_max_rpm: self
                    .profile
                    .config
                    .lathe
                    .as_ref()
                    .and_then(|lathe| lathe.live_tool_max_rpm)
                    .unwrap_or(0.0),
            })
            .collect();
        if snapshot.spindles.is_empty() {
            snapshot.spindles.push(SpindleState {
                index: 0,
                name: "MAIN SPINDLE".into(),
                commanded_speed: 0.0,
                actual_speed: 0.0,
                load: None,
                override_factor: 1.0,
                direction: 0,
                on: false,
                live_tool: false,
                live_tool_axial: false,
                live_tool_max_rpm: 0.0,
            });
        }

        snapshot.feed = FeedState {
            feed: dto.feed.feed,
            feed_override: dto.feed.feed_override,
            rapid_override: dto.feed.rapid_override,
            spindle_override: dto.feed.spindle_override,
            units: if dto.feed.units.is_empty() {
                self.profile.machine_ini.linear_units.clone()
            } else {
                dto.feed.units
            },
            feed_mode: dto.feed.feed_mode,
            distance_mode: dto.feed.distance_mode,
            feed_override_enabled: dto.feed.feed_override_enabled,
            spindle_override_enabled: dto.feed.spindle_override_enabled,
            adaptive_feed: dto.feed.adaptive_feed,
        };

        snapshot.panel.connected = dto.panel.connected;
        snapshot.panel.communication_error = dto.panel.error;
        snapshot.panel.mode = map_panel_mode(&dto.panel.mode);
        snapshot.panel.feed_override = dto.panel.feed_override;
        snapshot.panel.rapid_override = dto.panel.rapid_override;
        snapshot.panel.rapid = dto.panel.rapid;
        snapshot.limits_overridden = dto.panel.override_limits;
        snapshot.panel.spindle_override = dto.panel.spindle_override;
        snapshot.panel.program_protect = dto.panel.protect;
        snapshot.panel.program_protect_known = dto.panel.protect_known;
        snapshot.panel.sseg_status = dto.panel.sseg_status;
        snapshot.panel.estop = dto.panel.estop;
        snapshot.panel.current_tool = dto.tool.current;
        snapshot.panel.next_tool = dto.panel.next_tool;
        snapshot.panel.buttons = dto.panel.buttons;
        snapshot.panel.leds = dto.panel.leds;
        snapshot.panel.sseg_tool = dto.panel.sseg_tool;
        snapshot.panel.sseg_next_tool = dto.panel.sseg_next;
        snapshot.panel.capabilities = profile_capabilities(&self.profile);
        snapshot.panel.link = PanelLinkStats {
            port: self
                .profile
                .config
                .panel
                .link
                .port
                .clone()
                .unwrap_or_else(|| "hal".into()),
            baud: self.profile.config.panel.link.baud.unwrap_or(115200),
            cycle_time_ms: self.profile.config.panel.link.cycle_time_ms.unwrap_or(0) as f64,
            // The current cnc_panel driver does not expose serial counters.
            available: false,
            ..Default::default()
        };
        snapshot.panel.jog = JogState {
            mode: snapshot.panel.mode.label().into(),
            active_axis: dto.pendant.active_axis.clone(),
            speed: 0.0,
            rapid: false,
            increment: None,
            remaining_step: None,
            handwheel_scale: dto.pendant.handwheel_scale,
        };
        snapshot.pendant = PendantState {
            enabled: self.profile.config.pendant.enabled,
            connected: dto.pendant.connected,
            jog_only: true,
            jog_enable: dto.pendant.jog_enable,
            active_axis: dto.pendant.active_axis,
            handwheel_scale: dto.pendant.handwheel_scale,
            step_mode: dto.pendant.step_mode,
            ignored_functions: self.profile.config.pendant.ignored_functions.clone(),
        };

        snapshot.tool_table = self.tool_table.state();
        snapshot.tool_change.current_tool = dto.tool.current;
        snapshot.tool_change.prepared_tool = dto.tool.prepared;
        snapshot.tool_change.target_tool = Some(dto.tool.prepared).filter(|tool| *tool > 0);
        if dto.tool.change_active && !self.tool_change_active {
            // Rising edge: a new episode, and a new token to acknowledge.
            self.tool_change_serial += 1;
            self.tool_change_active = true;
        } else if !dto.tool.change_active {
            self.tool_change_active = false;
        }
        if dto.tool.change_active {
            if self.profile.changer == ToolChangerKind::Manual
                && self.profile.config.tools.manual_confirm
            {
                snapshot.tool_change.status = ToolChangeStatus::ManualPrompt;
                snapshot.tool_change.confirm_available = true;
                snapshot.tool_change.token = Some(ToolChangeToken(
                    (self.tool_change_serial << 16) | dto.tool.prepared as u64,
                ));
                snapshot.tool_change.message = format!(
                    "REMOVE T{} / INSTALL T{}",
                    dto.tool.current, dto.tool.prepared
                );
            } else {
                snapshot.tool_change.status = ToolChangeStatus::Issuing;
                snapshot.tool_change.message = format!("Changing to T{}", dto.tool.prepared);
            }
        } else if snapshot.tool_change.current_tool != 0 {
            snapshot.tool_change.status = ToolChangeStatus::Idle;
        }

        if self.profile.variant == MachineVariant::Lathe {
            let pockets = self.profile.turret_stations().unwrap_or(0);
            snapshot.atc = Some(AtcState {
                pockets,
                current_pocket: None,
                prepared_pocket: None,
                clamped: Some(true),
                indexing: false,
                station: None,
                live_tool_rpm: snapshot
                    .spindles
                    .iter()
                    .find(|spindle| spindle.live_tool)
                    .map(|spindle| spindle.actual_speed),
                live_tool_direction: snapshot
                    .spindles
                    .iter()
                    .find(|spindle| spindle.live_tool)
                    .map(|spindle| spindle.direction)
                    .unwrap_or(0),
                live_tool_axial: None,
                fault: None,
            });
        } else if self.profile.changer.uses_pockets() {
            snapshot.atc = Some(AtcState {
                pockets: self.profile.machine_ini.atc_pockets.unwrap_or(0),
                current_pocket: None,
                prepared_pocket: None,
                clamped: None,
                indexing: false,
                station: None,
                live_tool_rpm: None,
                live_tool_direction: 0,
                live_tool_axial: None,
                fault: None,
            });
        }

        // Program content: retain unless the helper sent a fresh copy.
        if let Some(text) = dto.program.text {
            if self
                .program_text
                .as_deref()
                .map(|current| current != text.as_str())
                .unwrap_or(true)
            {
                self.program_revision += 1;
            }
            self.program_text = Some(Arc::new(text));
        }
        let path = if dto.program.path.is_empty() {
            None
        } else {
            Some(PathBuf::from(&dto.program.path))
        };
        if path != self.program_path {
            self.program_path = path;
            self.program_revision += 1;
            self.live_trail.clear();
        }
        let text = self.program_text.clone();
        let line_count = text.as_ref().map(|text| text.lines().count()).unwrap_or(0);
        let current_line = dto.program.current_line;
        let motion_line = dto.program.motion_line;
        // The line to show as current: motion's line lags the interpreter's
        // read-ahead, so it is the one the tool is actually cutting.
        let executed = if motion_line > 0 {
            motion_line
        } else {
            current_line
        };
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
            modal: modal_lines(&dto.program.gcodes, &dto.program.mcodes),
            running: dto.program.running,
            paused: dto.program.paused,
            dirty: false,
            syntax_error: None,
            progress: if line_count > 0 {
                executed as f32 / line_count as f32
            } else {
                0.0
            },
            loaded_at_ms: now_ms(),
        };

        // Probe result from parameters #1001..#1009 and probed position.
        let signature = format!("{:?}", dto.probe.values);
        if self.last_probe_signature.as_deref() != Some(signature.as_str())
            && (dto.probe.tripped || dto.probe.values.values().any(|value| *value != 0.0))
        {
            let mut values = [0.0; 9];
            for (index, key) in (1001..=1009).enumerate() {
                values[index] = *dto.probe.values.get(&key.to_string()).unwrap_or(&0.0);
            }
            let results = linc_core::probe::ParameterResults::from_values(&values);
            // `#1001`-`#1009` are named measurements, not axis positions: for
            // the boss/pocket family `#1001` is an X width and `#1002` an X
            // centre. Interpret them with the routine that was actually issued.
            let operation = probe_block
                .as_deref()
                .and_then(|block| {
                    linc_core::probe::operations()
                        .iter()
                        .find(|op| block.contains(op.call))
                })
                .or_else(|| {
                    snapshot
                        .probe
                        .last_routine
                        .as_deref()
                        .and_then(|id| linc_core::probe::operations().iter().find(|op| op.id == id))
                });
            let family = operation.map(|op| op.result_family()).unwrap_or_default();
            let mut measured = AxisMap::new();
            if let Some(operation) = operation {
                for (axis, value) in results.wcs_offsets(operation) {
                    measured.insert(axis, value);
                }
            }
            let error = operation
                .and_then(|op| results.error(op, Some(self.profile.config.probe.probe_diameter)));
            let routine = operation
                .map(|op| op.name.to_string())
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
            snapshot.probe.history.push_front(result.clone());
            while snapshot.probe.history.len() > 32 {
                snapshot.probe.history.pop_back();
            }
            self.last_probe_signature = Some(signature);
        }
        snapshot.probe.enabled = self.profile.config.probe.enabled;
        snapshot.probe.tool_number = self.profile.config.probe.tool_number;
        snapshot.probe.selected_wcs = self.profile.config.probe.wcs.clone();
        snapshot.probe.running = dto.probe.running;

        for alarm in dto.alarms {
            if self.last_alarm_texts.iter().any(|text| text == &alarm.text) {
                continue;
            }
            let severity = if alarm.kind == "error" {
                AlarmSeverity::Error
            } else {
                AlarmSeverity::Info
            };
            // Belt and braces: an older bridge may still send the newlines that
            // made every entry look triple-spaced.
            let message = alarm.text.split_whitespace().collect::<Vec<_>>().join(" ");
            if message.is_empty() {
                continue;
            }
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
        }
        while self.alarms.len() > MAX_EVENT_LOG {
            self.alarms.pop_back();
        }
        snapshot.alarms = self.alarms.clone();

        // Live tool trail sampled at the status rate, capped by config.
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
            let max = self.profile.config.toolpath.trail_max_points.max(2);
            while self.live_trail.len() > max {
                self.live_trail.remove(0);
            }
        }
        self.last_tool_tip = Some(tip);
        snapshot.tool_tip = Some(tip);
        snapshot.live_trail = Arc::new(self.live_trail.clone());
        snapshot
    }
}

fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

fn map_values(map: HashMap<String, f64>) -> AxisMap<f64> {
    AxisMap::from_entries(map)
}

fn map_panel_mode(mode: &BridgeMode) -> PanelModeValidity {
    if mode.valid {
        if let Some(mode) = mode.mode.as_deref().and_then(parse_panel_mode) {
            return PanelModeValidity::Valid(mode);
        }
    }
    let active: Vec<PanelMode> = mode
        .active
        .iter()
        .filter_map(|name| parse_panel_mode(name))
        .collect();
    if active.is_empty() {
        PanelModeValidity::Unknown
    } else {
        PanelModeValidity::Invalid(active)
    }
}

fn parse_panel_mode(name: &str) -> Option<PanelMode> {
    Some(match name {
        "edit" => PanelMode::Edit,
        "auto" => PanelMode::Auto,
        "mdi" => PanelMode::Mdi,
        "jog" => PanelMode::Jog,
        "inc" => PanelMode::Inc,
        "mpg" => PanelMode::Mpg,
        "home" => PanelMode::Home,
        "proghome" => PanelMode::ProgHome,
        _ => return None,
    })
}

/// Carry the HMI-owned HAL selections across a poll.
///
/// The bridge rebuilds the snapshot from the panel service on every poll, so
/// anything the HMI wrote into `hal` would otherwise be reset — jogging then
/// runs at zero speed and INC steps by nothing.
fn carry_hal_selections(converted: &mut MachineSnapshot, stored: &MachineSnapshot) {
    converted.hal.jog_increment = stored.hal.jog_increment;
    converted.hal.jog_speed = stored.hal.jog_speed;
    // A manual tool-change confirmation is a level held until `iocontrol`
    // drops the request: the prompt is rebuilt from the bridge every poll, so
    // the level and the "confirmed" flag have to be carried while it stands.
    let prompt = converted.tool_change.status == ToolChangeStatus::ManualPrompt;
    converted.hal.tool_change_confirm = stored.hal.tool_change_confirm && prompt;
    converted.tool_change.confirmed = stored.tool_change.confirmed && prompt;
}

fn modal_lines(gcodes: &[f64], mcodes: &[f64]) -> Vec<(String, String)> {
    // Match Probe Basic/QtPyVCP `status:gcodes?text` and
    // `status:mcodes?text`: the first slot is not part of the modal display,
    // G-codes are encoded as 10x their printed value, and -1 slots are empty.
    let mut entries = Vec::new();
    let mut gs = gcodes
        .iter()
        .skip(1)
        .filter(|value| **value != -1.0 && value.is_finite())
        .map(|value| *value as i64)
        .collect::<Vec<_>>();
    gs.sort_unstable();
    for raw in gs {
        let value = raw as f64 / 10.0;
        entries.push((format!("G{value}"), String::new()));
    }
    let mut ms = mcodes
        .iter()
        .skip(1)
        .filter(|value| **value != -1.0 && value.is_finite())
        .map(|value| *value as i64)
        .collect::<Vec<_>>();
    ms.sort_unstable();
    for raw in ms {
        entries.push((format!("M{raw}"), String::new()));
    }
    entries
}

fn profile_capabilities(profile: &Profile) -> PanelCapabilities {
    PanelCapabilities {
        buttons: 46,
        leds: 56,
        displays: 3,
        has_status_display: true,
        mode_encoding: "one_hot_steady".into(),
        override_encoding: "analog_steady".into(),
        pendant_supported: profile.config.pendant.enabled,
        transport: profile
            .config
            .panel
            .link
            .kind
            .clone()
            .unwrap_or_else(|| "hal".into()),
    }
}

/// Does a helper stderr line report a failure (as opposed to a routine report)?
fn bridge_line_is_failure(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "error",
        "failed",
        "cannot",
        "could not",
        "refused",
        "mismatch",
        "not available",
        "will not persist",
        "no [rs274ngc]",
    ]
        .iter()
        .any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod bridge_log_tests {
    use super::bridge_line_is_failure;

    #[test]
    fn routine_reports_are_not_failures() {
        assert!(!bridge_line_is_failure(
            "linc bridge: G54 X file -11.1000, controller not applied yet 0.0000 (/x/sim.var)"
        ));
        assert!(!bridge_line_is_failure("linc bridge: applied the stored G54 offsets"));
    }

    #[test]
    fn real_failures_stay_warnings() {
        assert!(bridge_line_is_failure("linc bridge: could not apply stored G54 offsets"));
        assert!(bridge_line_is_failure("linc bridge: G54 -> controller ... (mismatch)"));
        assert!(bridge_line_is_failure("no [RS274NGC] PARAMETER_FILE in '/x/sim.ini'"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jog_selections_are_carried_across_a_poll() {
        // Regression: these used to live in `panel`, which the bridge rebuilds
        // from the panel service, so jogging ran at zero speed.
        let mut stored = MachineSnapshot::empty(MachineVariant::Mill);
        stored.hal.jog_speed = Some(42.0);
        stored.hal.jog_increment = Some(0.5);
        // A fresh poll has no idea about them.
        let mut converted = MachineSnapshot::empty(MachineVariant::Mill);
        assert_eq!(converted.hal.jog_speed, None);
        carry_hal_selections(&mut converted, &stored);
        assert_eq!(converted.hal.jog_speed, Some(42.0));
        assert_eq!(converted.hal.jog_increment, Some(0.5));
    }

    #[test]
    fn active_gcodes_match_probe_basic_text() {
        let gcodes = [
            0.0, 800.0, -1.0, 170.0, 400.0, 210.0, 900.0, 940.0, 540.0, 80.0,
        ];
        let mcodes = [0.0, -1.0, 5.0, -1.0, 9.0];
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
    fn maps_valid_and_invalid_panel_modes() {
        let valid = BridgeMode {
            valid: true,
            mode: Some("auto".into()),
            active: vec!["auto".into()],
        };
        assert_eq!(
            map_panel_mode(&valid),
            PanelModeValidity::Valid(PanelMode::Auto)
        );
        let invalid = BridgeMode {
            valid: false,
            mode: None,
            active: vec!["auto".into(), "mdi".into()],
        };
        assert!(matches!(
            map_panel_mode(&invalid),
            PanelModeValidity::Invalid(_)
        ));
        let unknown = BridgeMode::default();
        assert_eq!(map_panel_mode(&unknown), PanelModeValidity::Unknown);
    }

    #[test]
    fn helper_script_is_embedded_and_declares_no_writes() {
        assert!(HELPER.contains("o<probe_cal_reset>"));
        assert!(HELPER.contains("cnc_panel.proghome-all"));
        assert!(HELPER.contains("G0 X0Y0Z0"));
        assert!(HELPER.contains("G0 X0"));
        assert!(!HELPER.contains("serialport"));
        assert!(!HELPER.contains("/dev/tty"));
        assert!(!HELPER.contains("hal.set_p"));
        // The bridge owns a read-only HAL component (no pins), required by the
        // Python binding before hal.get_value() can read other components.
        assert!(HELPER.contains("hal.component"));
        assert!(!HELPER.contains("newpin"));
        assert!(!HELPER.contains("newparam"));
    }
}
