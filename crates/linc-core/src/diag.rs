//! Diagnostic state shared with `linc-mcp`.
//!
//! This module is pure data: no sockets, no threads, no MCP. The UI publishes a
//! [`MachineSnapshot`] and a [`UiSnapshot`] into a [`Bus`] on every frame, and
//! the diagnostic server (in `linc-diag`) answers protocol requests from it.
//!
//! Two things live here that must not be duplicated elsewhere:
//!
//! - the **gate report** ([`gates::blockers`]), so "why is this action blocked"
//!   has one answer for the UI, the MCP server and the tests;
//! - the **envelope**, so every response carries the same freshness and drop
//!   bookkeeping and an agent can refuse to act on stale state.

use std::collections::VecDeque;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::model::MachineSnapshot;

/// Independent of the crate version: additive fields do not bump it, removing
/// or renaming one does.
pub const SCHEMA_VERSION: u32 = 1;

/// A snapshot older than this is reported stale unless the caller says
/// otherwise.
pub const DEFAULT_STALE_MS: i64 = 2_000;

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `1..=9` to `G54`..`G59.3`, the way the controller numbers them.
pub fn wcs_name(index: u8) -> String {
    match index {
        1 => "G54".into(),
        2 => "G55".into(),
        3 => "G56".into(),
        4 => "G57".into(),
        5 => "G58".into(),
        6 => "G59".into(),
        7 => "G59.1".into(),
        8 => "G59.2".into(),
        9 => "G59.3".into(),
        other => format!("G5{other}"),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// One reason an action is blocked, or would be.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Blocker {
    /// Stable, greppable identifier such as `PANEL_OFFLINE`.
    pub code: String,
    pub message: String,
    pub severity: Severity,
    /// `panel`, `machine`, `program`, `config` — which layer to look at first.
    pub source: String,
    /// What the operator can do, when there is something.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

impl Blocker {
    fn new(
        code: &str,
        message: impl Into<String>,
        severity: Severity,
        source: &str,
        remediation: Option<&str>,
    ) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            severity,
            source: source.into(),
            remediation: remediation.map(str::to_string),
        }
    }
}

/// The UI state an agent needs to reason about what the operator is seeing.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct UiSnapshot {
    /// Screen name as shown in the navigation, e.g. `MACHINE`.
    pub page: String,
    /// Layout variant of the page, e.g. `mill` / `lathe`.
    pub layout: String,
    pub mode: String,
    /// `SPLIT` or `CODE` on the Program page.
    pub program_view: String,
    /// Toolpath view label, e.g. `3D / ISO`.
    pub view: String,
    pub zoom: f32,
    pub selected_line: Option<usize>,
    pub current_line: Option<usize>,
    pub selected_axis: Option<String>,
    pub program_name: String,
    pub program_dirty: bool,
    pub panel_online: bool,
    /// Render rate the UI measured, for diagnosing a sluggish panel.
    pub fps: f32,
    pub toasts: Vec<String>,
    /// Window size in points and scale, so a layout bug can be reproduced.
    #[serde(default)]
    pub viewport: ViewportInfo,
    /// Named screen regions from the shell, in points, top-left origin.
    #[serde(default)]
    pub regions: Vec<ScreenRegion>,
    /// The frame time the UI measured when it published this snapshot.
    #[serde(default)]
    pub frame_time_ms: f32,
}

/// Window size and scale of the HMI, measured from the frame.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ViewportInfo {
    pub width: f32,
    pub height: f32,
    pub pixels_per_point: f32,
    pub fullscreen: bool,
}

/// One named rectangle the shell laid out, for the screen model tool.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ScreenRegion {
    pub name: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl ScreenRegion {
    pub fn new(name: &str, x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            name: name.into(),
            x,
            y,
            width,
            height,
        }
    }
}

/// An HMI intent an agent may trigger through the diagnostic socket.
///
/// These are view/state operations, not machine commands: pressing a page key or
/// showing the bounds cannot move an axis. Anything that can is a separate
/// command with its own gate.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum UiAction {
    /// Go to a page by its navigation name (`MACHINE`, `PROGRAM`, ...).
    Page { page: String },
    /// Cycle the toolpath view ISO -> X -> Y -> Z.
    CycleView,
    /// Show or hide the program bounds (technical drawing).
    ShowBounds { visible: bool },
    /// Advance the `Ctrl+B` cycle: dimensions, + machine, + program box, off.
    CycleBounds,
    /// Frame the program in the toolpath.
    Fit,
    /// Clear the live trail.
    ClearTrail,
    /// Select and reveal a program line (1-based).
    SelectLine { line: usize },
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LogRecord {
    pub sequence: u64,
    pub at_ms: i64,
    pub level: String,
    pub target: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EventRecord {
    pub sequence: u64,
    pub at_ms: i64,
    pub kind: String,
    pub severity: Severity,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dropped {
    pub events: u64,
    pub logs: u64,
    pub snapshots: u64,
    pub panel_frames: u64,
    #[serde(default)]
    pub motion: u64,
}

impl Dropped {
    pub fn any(&self) -> bool {
        self.events > 0
            || self.logs > 0
            || self.snapshots > 0
            || self.panel_frames > 0
            || self.motion > 0
    }
}

/// One panel state transition, kept in the panel service's own ring.
///
/// `from` and `to` are display strings rather than typed values: the ring
/// carries fields of different shapes (a mode label, a button mask, an
/// override), and text keeps every record readable without a union type. They
/// are formatted for a reader, not decoded again.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PanelEventRecord {
    pub sequence: u64,
    pub at_ms: i64,
    pub kind: String,
    pub from: String,
    pub to: String,
}

/// A simulator panel fault an agent can inject.
///
/// These are link and input faults the panel service can produce on request.
/// Controller-side faults (a lost NML session, a missing bridge snapshot) are
/// not panel faults: the panel service does not own the bridge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelFault {
    /// The link is down: the panel reports offline and the snapshot stops.
    Disconnect,
    /// A read timed out; the panel reports a communication error.
    Timeout,
    /// The reply arrived corrupted.
    CrcError,
    /// The mode field decodes to zero or several inputs.
    InvalidMode,
    /// The protect input cannot be trusted, so it fails closed.
    ProtectUnknown,
    /// The panel snapshot stops advancing; its age grows past the stale limit.
    Stale,
}

impl PanelFault {
    pub const ALL: [PanelFault; 6] = [
        PanelFault::Disconnect,
        PanelFault::Timeout,
        PanelFault::CrcError,
        PanelFault::InvalidMode,
        PanelFault::ProtectUnknown,
        PanelFault::Stale,
    ];

    /// Parse the wire name (`disconnect`, `crc_error`, ...).
    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|fault| fault.label() == value.trim())
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Disconnect => "disconnect",
            Self::Timeout => "timeout",
            Self::CrcError => "crc_error",
            Self::InvalidMode => "invalid_mode",
            Self::ProtectUnknown => "protect_unknown",
            Self::Stale => "stale",
        }
    }
}

/// A change an agent asks a simulated panel to make.
///
/// Every field is optional: an absent field is left alone, which is what makes
/// a patch safe to apply on a panel an operator is also using. Unless stated
/// otherwise the values are the operator's own controls, so `estop` and
/// `power` are the same inputs the simulator window's buttons raise.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SimPanelPatch {
    pub connected: Option<bool>,
    /// `EDIT`, `AUTO`, `MDI`, `JOG`, `INC`, `MPG`, `HOME`, `PROG.HOME`, or
    /// `INVALID`/`UNKNOWN` for the fail-closed states.
    pub mode: Option<String>,
    pub feed_override: Option<f64>,
    pub rapid_override: Option<f64>,
    pub spindle_override: Option<f64>,
    pub program_protect: Option<bool>,
    pub estop: Option<bool>,
    /// Simulation power state: `cnc_panel.power_on`, not the POWER ON key edge.
    pub power: Option<bool>,
    /// Button names from the panel table (`cycle_start`, `estop`, ...).
    pub press: Option<Vec<String>>,
    /// Button names to release.
    pub release: Option<Vec<String>>,
}

/// The HMI's view of the LinuxCNC bridge process behind the backend.
///
/// Filled by `linc-linuxcnc`; a simulator backend has none. `restarts` is
/// always zero today because the bridge is not restarted in place: a dead
/// process sets `alive=false` and `fatal`, and the operator restarts the HMI.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BridgeInfo {
    pub kind: String,
    pub pid: u32,
    pub alive: bool,
    pub restarts: u64,
    pub requests: u64,
    pub replies: u64,
    pub errors: u64,
    pub last_latency_ms: u64,
    pub max_latency_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_request: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reply: Option<String>,
    #[serde(default)]
    pub stderr_tail: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fatal: Option<String>,
}

/// One axis at a motion sample.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AxisMotion {
    pub axis: String,
    pub work: f64,
    pub machine: f64,
    pub dtg: f64,
    pub velocity: f64,
}

/// A motion sample retained for the motion-history tool.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct MotionSample {
    pub at_ms: i64,
    pub sequence: u64,
    pub positions: Vec<AxisMotion>,
    pub commanded: Vec<f64>,
    pub actual: Vec<f64>,
    pub following_error: Vec<f64>,
    pub servo_load: Vec<f64>,
    pub spindle_speed: f64,
    pub spindle_load: f64,
    pub spindle_override: f64,
    pub feed: f64,
    pub feed_override: f64,
    pub rapid_override: f64,
    pub task_mode: String,
    pub interp: String,
    pub program_line: usize,
    pub motion_line: usize,
    pub estop: bool,
    pub power: String,
    pub panel_mode: String,
}

impl MotionSample {
    pub fn from_snapshot(snapshot: &MachineSnapshot, sequence: u64) -> Self {
        let axis_entries = |map: &crate::model::AxisMap<f64>| -> Vec<(String, f64)> {
            map.iter().map(|(axis, value)| (axis.to_string(), *value)).collect()
        };
        let commanded = axis_entries(&snapshot.machine.commanded);
        let positions = snapshot
            .positions
            .axes
            .iter()
            .map(|(axis, set)| AxisMotion {
                axis: axis.into(),
                work: set.work,
                machine: set.machine,
                dtg: set.dtg,
                velocity: set.velocity,
            })
            .collect();
        let spindle = snapshot.spindles.first();
        Self {
            at_ms: now_ms(),
            sequence,
            positions,
            commanded: commanded.iter().map(|(_, value)| *value).collect(),
            actual: axis_entries(&snapshot.machine.actual)
                .iter()
                .map(|(_, value)| *value)
                .collect(),
            following_error: axis_entries(&snapshot.machine.following_error)
                .iter()
                .map(|(_, value)| *value)
                .collect(),
            servo_load: axis_entries(&snapshot.machine.servo_load)
                .iter()
                .map(|(_, value)| *value)
                .collect(),
            spindle_speed: spindle.map(|spindle| spindle.actual_speed).unwrap_or(0.0),
            spindle_load: spindle.and_then(|spindle| spindle.load).unwrap_or(0.0),
            spindle_override: snapshot.feed.spindle_override,
            feed: snapshot.feed.feed,
            feed_override: snapshot.feed.feed_override,
            rapid_override: snapshot.feed.rapid_override,
            task_mode: format!("{:?}", snapshot.task.mode),
            interp: format!("{:?}", snapshot.task.interp),
            program_line: snapshot.program.current_line,
            motion_line: snapshot.program.motion_line,
            estop: snapshot.machine.estop,
            power: format!("{:?}", snapshot.task.power),
            panel_mode: snapshot.panel.mode.label().into(),
        }
    }
}

/// A panic written by `linc-diag`'s crash hook; `linc-mcp` reads the files.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CrashRecord {
    pub at_ms: i64,
    pub pid: u32,
    pub role: String,
    #[serde(default)]
    pub build: BuildInfo,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backtrace: Option<String>,
}

/// Process identity attached to every response.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BuildInfo {
    pub version: String,
    pub git_commit: String,
    pub git_dirty: bool,
    pub profile: String,
    pub backend: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessInfo {
    pub pid: u32,
    pub role: String,
    pub build: BuildInfo,
}

/// The response wrapper from the handoff document §7.2.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub schema_version: u32,
    pub source: String,
    pub process: ProcessInfo,
    pub sequence: u64,
    pub captured_at_ms: i64,
    pub age_ms: i64,
    pub stale: bool,
    pub dropped: Dropped,
    pub data: Value,
}

impl Envelope {
    /// Build a response, computing `age_ms` from the snapshot's timestamp and
    /// taking `stale` from that age, not from a guess.
    pub fn new(
        source: &str,
        process: ProcessInfo,
        sequence: u64,
        captured_at_ms: i64,
        stale_after_ms: i64,
        dropped: Dropped,
        data: Value,
    ) -> Self {
        let age_ms = (now_ms() - captured_at_ms).max(0);
        Self {
            schema_version: SCHEMA_VERSION,
            source: source.into(),
            process,
            sequence,
            captured_at_ms,
            age_ms,
            stale: age_ms > stale_after_ms,
            dropped,
            data,
        }
    }
}

/// Protocol error, shaped like §7.4.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DiagError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
    pub retryable: bool,
}

impl DiagError {
    pub fn new(code: &str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: Value::Null,
            retryable,
        }
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new("INVALID_ARGUMENT", message, false)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("NOT_FOUND", message, false)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new("INTERNAL", message, false)
    }
}

impl std::fmt::Display for DiagError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for DiagError {}

/// The shared state both the diagnostic server and the HUD read.
///
/// `sequence` advances whenever the machine or the UI state changes; the
/// timestamp is the moment the state was captured, which is what `age_ms` in the
/// envelope measures.
pub struct Bus {
    machine: MachineSnapshot,
    ui: UiSnapshot,
    sequence: u64,
    captured_at_ms: i64,
    logs: VecDeque<LogRecord>,
    events: VecDeque<EventRecord>,
    dropped: Dropped,
    log_limit: usize,
    event_limit: usize,
    history: VecDeque<HistoryEntry>,
    history_limit: usize,
    last_history_ms: i64,
    motion: VecDeque<MotionSample>,
    motion_limit: usize,
    motion_interval_ms: i64,
    last_motion_ms: i64,
    screenshot_requested: bool,
    screenshot: Option<Screenshot>,
    ui_actions: VecDeque<UiAction>,
}

/// A PNG the UI thread captured for `diag.screenshot`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Screenshot {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub at_ms: i64,
}

/// One retained snapshot for `diag.snapshot_at` and `diag.snapshot_diff`.
///
/// Kept at 1 Hz: enough to answer "what changed just before this error" without
/// holding a copy of the trail more often than needed.
#[derive(Clone, Debug)]
pub struct HistoryEntry {
    pub sequence: u64,
    pub captured_at_ms: i64,
    pub snapshot: MachineSnapshot,
}

impl Bus {
    pub fn new(machine: MachineSnapshot) -> Self {
        Self {
            captured_at_ms: now_ms(),
            machine,
            ui: UiSnapshot::default(),
            sequence: 0,
            logs: VecDeque::new(),
            events: VecDeque::new(),
            dropped: Dropped::default(),
            log_limit: 2_000,
            event_limit: 1_000,
            history: VecDeque::new(),
            history_limit: 60,
            last_history_ms: 0,
            motion: VecDeque::new(),
            motion_limit: 1_200,
            motion_interval_ms: 100,
            last_motion_ms: 0,
            screenshot_requested: false,
            screenshot: None,
            ui_actions: VecDeque::new(),
        }
    }

    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn captured_at_ms(&self) -> i64 {
        self.captured_at_ms
    }

    pub fn dropped(&self) -> Dropped {
        self.dropped
    }

    pub fn machine(&self) -> &MachineSnapshot {
        &self.machine
    }

    pub fn ui(&self) -> &UiSnapshot {
        &self.ui
    }

    /// Publish a machine snapshot. Cheap enough to call at status rate: the
    /// trail is inside the snapshot and is an `Arc`.
    pub fn publish_machine(&mut self, snapshot: MachineSnapshot) {
        self.record_transitions(&snapshot);
        self.machine = snapshot;
        self.sequence += 1;
        self.captured_at_ms = now_ms();
        self.record_history();
        self.record_motion();
    }

    /// Retain a snapshot at 1 Hz; `history_limit` entries is one minute.
    fn record_history(&mut self) {
        if self.captured_at_ms - self.last_history_ms < 1_000 && !self.history.is_empty() {
            return;
        }
        self.last_history_ms = self.captured_at_ms;
        if self.history.len() >= self.history_limit {
            self.history.pop_front();
        }
        self.history.push_back(HistoryEntry {
            sequence: self.sequence,
            captured_at_ms: self.captured_at_ms,
            snapshot: self.machine.clone(),
        });
    }

    pub fn history_len(&self) -> usize {
        self.history.len()
    }

    /// Retain a motion sample at `motion_interval_ms` (10 Hz by default).
    fn record_motion(&mut self) {
        if self.captured_at_ms - self.last_motion_ms < self.motion_interval_ms
            && !self.motion.is_empty()
        {
            return;
        }
        self.last_motion_ms = self.captured_at_ms;
        if self.motion.len() >= self.motion_limit {
            self.motion.pop_front();
            self.dropped.motion += 1;
        }
        self.motion.push_back(MotionSample::from_snapshot(&self.machine, self.sequence));
    }

    pub fn motion_len(&self) -> usize {
        self.motion.len()
    }

    /// The retained motion samples, optionally limited to the last `seconds`
    /// and evenly downsampled to `max_samples`.
    pub fn motion_history(&self, seconds: Option<f64>, max_samples: usize) -> Value {
        let max = max_samples.clamp(1, 1_200);
        let cutoff = seconds.map(|seconds| {
            self.captured_at_ms - (seconds.max(0.0) * 1000.0).round() as i64
        });
        let samples: Vec<&MotionSample> = self
            .motion
            .iter()
            .filter(|sample| cutoff.map(|cutoff| sample.at_ms >= cutoff).unwrap_or(true))
            .collect();
        let available = samples.len();
        let step = (available / max).max(1);
        let picked: Vec<Value> = samples
            .iter()
            .step_by(step)
            .take(max)
            .map(|sample| serde_json::to_value(sample).unwrap_or(Value::Null))
            .collect();
        json!({
            "samples": picked,
            "count": picked.len(),
            "available": available,
            "retained": self.motion.len(),
            "sample_interval_ms": self.motion_interval_ms,
            "downsampled_by": step,
        })
    }

    /// The snapshot at `sequence`, or the newest one at or before it.
    ///
    /// A sequence at or after the live state returns the live snapshot, so a
    /// caller can diff "one second ago" against "now" without waiting for the
    /// sampler to record again.
    pub fn snapshot_at(&self, sequence: u64) -> Option<HistoryEntry> {
        if sequence >= self.sequence {
            return Some(HistoryEntry {
                sequence: self.sequence,
                captured_at_ms: self.captured_at_ms,
                snapshot: self.machine.clone(),
            });
        }
        self.history
            .iter()
            .rev()
            .find(|entry| entry.sequence <= sequence)
            .cloned()
    }

    /// Field-level differences between two retained snapshots.
    pub fn snapshot_diff(&self, from_sequence: u64, to_sequence: u64) -> Result<Value, String> {
        let from = self
            .snapshot_at(from_sequence)
            .ok_or_else(|| format!("no snapshot at or before {from_sequence}"))?;
        let to = self
            .snapshot_at(to_sequence)
            .ok_or_else(|| format!("no snapshot at or before {to_sequence}"))?;
        let mut changes = Vec::new();
        let from_value = serde_json::to_value(&from.snapshot).map_err(|e| e.to_string())?;
        let to_value = serde_json::to_value(&to.snapshot).map_err(|e| e.to_string())?;
        diff_json(&from_value, &to_value, "", &mut changes, 0);
        Ok(json!({
            "from": { "sequence": from.sequence, "captured_at_ms": from.captured_at_ms },
            "to": { "sequence": to.sequence, "captured_at_ms": to.captured_at_ms },
            "changes": changes,
        }))
    }

    /// Events and logs merged by time, around a moment or after a sequence.
    pub fn timeline(
        &self,
        around_ms: Option<i64>,
        window_ms: i64,
        since_sequence: Option<u64>,
        limit: usize,
    ) -> Value {
        let limit = limit.clamp(1, 1_000);
        let low = around_ms.map(|center| center - window_ms);
        let high = around_ms.map(|center| center + window_ms);
        let mut entries: Vec<(i64, &'static str, Value)> = Vec::new();
        for log in self.logs.iter().filter(|log| {
            low.map(|low| log.at_ms >= low).unwrap_or(true)
                && high.map(|high| log.at_ms <= high).unwrap_or(true)
                && since_sequence
                    .map(|since| log.sequence > since)
                    .unwrap_or(true)
        }) {
            entries.push((log.at_ms, "log", serde_json::to_value(log).unwrap_or(Value::Null)));
        }
        for event in self.events.iter().filter(|event| {
            low.map(|low| event.at_ms >= low).unwrap_or(true)
                && high.map(|high| event.at_ms <= high).unwrap_or(true)
                && since_sequence
                    .map(|since| event.sequence > since)
                    .unwrap_or(true)
        }) {
            entries.push((event.at_ms, "event", serde_json::to_value(event).unwrap_or(Value::Null)));
        }
        entries.sort_by_key(|(at, _, _)| *at);
        let entries: Vec<Value> = entries
            .into_iter()
            .take(limit)
            .map(|(at, kind, record)| json!({ "at_ms": at, "kind": kind, "record": record }))
            .collect();
        json!({ "timeline": entries, "next_cursor": Value::Null })
    }

    /// Publish UI state. An unchanged snapshot does not advance the sequence, so
    /// a waiting agent is not woken by repaint churn.
    pub fn publish_ui(&mut self, ui: UiSnapshot) {
        if ui == self.ui {
            return;
        }
        self.ui = ui;
        self.sequence += 1;
        self.captured_at_ms = now_ms();
    }

    pub fn push_log(&mut self, level: &str, target: &str, message: impl Into<String>) {
        if self.logs.len() >= self.log_limit {
            self.logs.pop_front();
            self.dropped.logs += 1;
        }
        self.logs.push_back(LogRecord {
            sequence: self.sequence,
            at_ms: now_ms(),
            level: level.into(),
            target: target.into(),
            message: message.into(),
        });
    }

    pub fn push_event(&mut self, kind: &str, severity: Severity, message: impl Into<String>) {
        if self.events.len() >= self.event_limit {
            self.events.pop_front();
            self.dropped.events += 1;
        }
        self.events.push_back(EventRecord {
            sequence: self.sequence,
            at_ms: now_ms(),
            kind: kind.into(),
            severity,
            message: message.into(),
        });
    }

    /// Events are derived from transitions rather than sprinkled at call sites,
    /// so nothing can be forgotten when a new field starts changing.
    fn record_transitions(&mut self, next: &MachineSnapshot) {
        let mut transitions: Vec<(&'static str, Severity, String)> = Vec::new();
        {
            let previous = &self.machine;
            if previous.task.power != next.task.power {
                transitions.push((
                    "power",
                    Severity::Info,
                    format!("machine power {:?} -> {:?}", previous.task.power, next.task.power),
                ));
            }
            if previous.machine.estop != next.machine.estop {
                let severity = if next.machine.estop {
                    Severity::Error
                } else {
                    Severity::Info
                };
                transitions.push(("estop", severity, format!("e-stop {}", next.machine.estop)));
            }
            if previous.panel.connected != next.panel.connected {
                let severity = if next.panel.connected {
                    Severity::Info
                } else {
                    Severity::Warning
                };
                transitions.push((
                    "panel",
                    severity,
                    format!("panel connected {}", next.panel.connected),
                ));
            }
            if previous.task.mode != next.task.mode {
                transitions.push((
                    "mode",
                    Severity::Info,
                    format!("task mode {:?} -> {:?}", previous.task.mode, next.task.mode),
                ));
            }
            if previous.task.interp != next.task.interp {
                transitions.push((
                    "interp",
                    Severity::Info,
                    format!("interpreter {:?} -> {:?}", previous.task.interp, next.task.interp),
                ));
            }
            if previous.program.display_name != next.program.display_name {
                transitions.push((
                    "program",
                    Severity::Info,
                    format!(
                        "program {:?} -> {:?}",
                        previous.program.display_name, next.program.display_name
                    ),
                ));
            }
            if previous.alarms.len() != next.alarms.len() {
                let severity = if next.alarms.len() > previous.alarms.len() {
                    Severity::Error
                } else {
                    Severity::Info
                };
                transitions.push((
                    "alarms",
                    severity,
                    format!("alarms {} -> {}", previous.alarms.len(), next.alarms.len()),
                ));
            }
        }
        for (kind, severity, message) in transitions {
            self.push_event(kind, severity, message);
        }
    }

    /// Queue an intent for the UI thread. The queue is capped: a burst of
    /// requests must not replay a minute of keystrokes later.
    pub fn request_ui_action(&mut self, action: UiAction) {
        if self.ui_actions.len() >= 32 {
            self.ui_actions.pop_front();
        }
        self.ui_actions.push_back(action);
    }

    pub fn take_ui_actions(&mut self) -> Vec<UiAction> {
        self.ui_actions.drain(..).collect()
    }

    /// Ask the UI thread to capture the next frame.
    pub fn request_screenshot(&mut self) {
        self.screenshot_requested = true;
        self.screenshot = None;
    }

    /// The UI thread takes the request; `None` means nothing was asked for.
    pub fn take_screenshot_request(&mut self) -> bool {
        std::mem::take(&mut self.screenshot_requested)
    }

    pub fn put_screenshot(&mut self, screenshot: Screenshot) {
        self.screenshot = Some(screenshot);
    }

    pub fn take_screenshot(&mut self) -> Option<Screenshot> {
        self.screenshot.take()
    }

    /// `{ machine, ui }`, with the live trail trimmed to what was asked for.
    pub fn state(&self, include_trail: bool, trail_points: usize) -> Value {
        let mut machine = self.machine.clone();
        if include_trail {
            let limit = trail_points.clamp(1, 5_000);
            if machine.live_trail.len() > limit {
                let start = machine.live_trail.len() - limit;
                machine.live_trail = std::sync::Arc::new(machine.live_trail[start..].to_vec());
            }
        } else {
            machine.live_trail = std::sync::Arc::new(Vec::new());
        }
        json!({
            "machine": serde_json::to_value(&machine).unwrap_or(Value::Null),
            "ui": serde_json::to_value(&self.ui).unwrap_or(Value::Null),
            "trail_included": include_trail,
        })
    }

    /// The operator summary from §9.2.
    pub fn summary(&self) -> Value {
        let m = &self.machine;
        let line = m.program.executing_line();
        json!({
            "machine": m.machine_name,
            "simulated": m.simulated,
            "power": format!("{:?}", m.task.power),
            "mode": format!("{:?}", m.task.mode),
            "interp": format!("{:?}", m.task.interp),
            "estop": m.machine.estop,
            "fault": m.machine.fault,
            "panel_mode": format!("{:?}", m.panel.mode),
            "panel_online": m.panel.connected,
            "protect": m.panel.protect_effective(),
            "homed": m
                .positions
                .axes
                .iter()
                .all(|(axis, _)| m.machine.homed.get(axis).copied().unwrap_or(false)),
            "program": m.program.display_name,
            "loaded": m.program.path.is_some(),
            "line": line,
            "progress": m.program.progress,
            "tool": m.tool_change.current_tool,
            "next_tool": m.tool_change.prepared_tool,
            "wcs": m.offsets.active_index.map(wcs_name),
            "alarms": m.alarms.len(),
            "dirty": m.program.dirty,
            "ui": {
                "page": self.ui.page,
                "mode": self.ui.mode,
                "view": self.ui.view,
            },
            "blockers": gates::blockers(m, &self.ui),
        })
    }

    pub fn why_blocked(&self) -> Value {
        let blockers = gates::blockers(&self.machine, &self.ui);
        json!({
            "blocked": !blockers.is_empty(),
            "blockers": blockers,
        })
    }

    pub fn offsets(&self) -> Value {
        let m = &self.machine;
        json!({
            "active": m.offsets.active_index.map(wcs_name),
            "active_index": m.offsets.active_index,
            "revision": m.offsets.revision,
            "systems": m.offsets.offsets.iter().map(|o| json!({
                "name": wcs_name(o.index),
                "index": o.index,
                "values": o.values,
                "rotation": o.rotation,
            })).collect::<Vec<_>>(),
            "g92": m.offsets.g92,
        })
    }

    pub fn tools(&self) -> Value {
        let m = &self.machine;
        json!({
            "path": m.tool_table.path,
            "changer": format!("{:?}", m.tool_table.changer),
            "revision": m.tool_table.revision,
            "error": m.tool_table.error,
            "columns": m.tool_table.columns.iter().map(|c| format!("{c:?}")).collect::<Vec<_>>(),
            "rows": m.tool_table.rows,
            "current": m.tool_change.current_tool,
            "next": m.tool_change.prepared_tool,
            "state": format!("{:?}", m.tool_change.status),
            "atc": m.atc.as_ref().map(|atc| serde_json::to_value(atc).unwrap_or(Value::Null)),
        })
    }

    pub fn program(&self, line_start: usize, line_count: usize) -> Value {
        let m = &self.machine;
        let content = m.program.content.as_deref().cloned().unwrap_or_default();
        let start = line_start.saturating_sub(1);
        let lines: Vec<Value> = content
            .lines()
            .skip(start)
            .take(line_count.clamp(1, 2_000))
            .enumerate()
            .map(|(index, text)| json!({ "line": start + index + 1, "text": text }))
            .collect();
        json!({
            "path": m.program.path,
            "name": m.program.display_name,
            "revision": m.program.revision,
            "line_count": m.program.line_count,
            "line_start": line_start,
            "current_line": m.program.current_line,
            "motion_line": m.program.motion_line,
            "next_line": m.program.next_line,
            "block": m.program.current_block,
            "running": m.program.running,
            "paused": m.program.paused,
            "dirty": m.program.dirty,
            "progress": m.program.progress,
            "syntax_error": m.program.syntax_error,
            "modal": m.program.modal,
            "lines": lines,
        })
    }

    pub fn logs(&self, since_ms: Option<i64>, level: Option<&str>, limit: usize) -> Value {
        let limit = limit.clamp(1, 1_000);
        let records: Vec<Value> = self
            .logs
            .iter()
            .filter(|record| since_ms.map(|since| record.at_ms > since).unwrap_or(true))
            .filter(|record| {
                level
                    .map(|wanted| record.level.eq_ignore_ascii_case(wanted))
                    .unwrap_or(true)
            })
            .take(limit)
            .map(|record| serde_json::to_value(record).unwrap_or(Value::Null))
            .collect();
        json!({ "logs": records, "next_cursor": Value::Null })
    }

    pub fn events(
        &self,
        since_sequence: Option<u64>,
        kind: Option<&str>,
        limit: usize,
    ) -> Value {
        let limit = limit.clamp(1, 1_000);
        let records: Vec<Value> = self
            .events
            .iter()
            .filter(|record| {
                since_sequence
                    .map(|since| record.sequence > since)
                    .unwrap_or(true)
            })
            .filter(|record| kind.map(|wanted| record.kind == wanted).unwrap_or(true))
            .take(limit)
            .map(|record| serde_json::to_value(record).unwrap_or(Value::Null))
            .collect();
        json!({ "events": records, "next_cursor": Value::Null })
    }
}

/// Paths that change on every sample, or that are too large to diff usefully.
const DIFF_IGNORE: &[&str] = &[
    "meta.captured_at_ms",
    "meta.age_ms",
    "meta.sequence",
    "meta.stale",
    "live_trail",
    "tool_tip",
    "hal.heartbeat_seq",
    "program.content",
    "program.progress",
    "program.loaded_at_ms",
];

/// Collect changed leaf paths between two JSON values.
///
/// Objects recurse; arrays and scalars are reported whole. Paths in
/// [`DIFF_IGNORE`] are skipped: a diff where every entry is `meta.age_ms` is
/// noise, not a diagnosis.
fn diff_json(from: &Value, to: &Value, prefix: &str, out: &mut Vec<Value>, depth: usize) {
    if depth > 8 {
        return;
    }
    let (Some(from_object), Some(to_object)) = (from.as_object(), to.as_object()) else {
        return;
    };
    let mut keys: Vec<&String> = from_object.keys().chain(to_object.keys()).collect();
    keys.sort();
    keys.dedup();
    for key in keys {
        let path = if prefix.is_empty() {
            key.clone()
        } else {
            format!("{prefix}.{key}")
        };
        if DIFF_IGNORE
            .iter()
            .any(|ignore| path == *ignore || path.starts_with(&format!("{ignore}.")))
        {
            continue;
        }
        let before = from_object.get(key).unwrap_or(&Value::Null);
        let after = to_object.get(key).unwrap_or(&Value::Null);
        if before == after {
            continue;
        }
        if before.is_object() && after.is_object() {
            diff_json(before, after, &path, out, depth + 1);
            continue;
        }
        out.push(json!({ "path": path, "from": before, "to": after }));
    }
}

/// The shared answer to "why can't I do this?".
///
/// Every check is derived from state the HMI already trusts; a check that needs
/// a live controller round trip does not belong here.
pub mod gates {
    use super::{wcs_name, Blocker, MachineSnapshot, Severity, UiSnapshot};
    use crate::model::{InterpState, MachinePower, PanelMode};

    /// All blocking conditions, in the order an operator should read them:
    /// panel first (everything else is stale without it), then machine safety,
    /// then program preconditions.
    pub fn blockers(machine: &MachineSnapshot, ui: &UiSnapshot) -> Vec<Blocker> {
        let mut out = Vec::new();

        if !machine.panel.connected {
            out.push(Blocker::new(
                "PANEL_OFFLINE",
                "the panel service is not connected; machine state is not authoritative",
                Severity::Error,
                "panel",
                Some("check the panel link and the panel service"),
            ));
        } else if machine.panel.communication_error {
            out.push(Blocker::new(
                "PANEL_COMM_ERROR",
                "the panel reports a communication error; its inputs are ignored",
                Severity::Error,
                "panel",
                Some("clear the link error from the panel"),
            ));
        } else if !machine.panel.mode.is_valid() {
            out.push(Blocker::new(
                "PANEL_MODE_UNKNOWN",
                format!("the panel mode {:?} is not known yet", machine.panel.mode),
                Severity::Warning,
                "panel",
                Some("wait for a complete panel frame or cycle the panel mode"),
            ));
        }

        if machine.machine.estop {
            out.push(Blocker::new(
                "ESTOP_LATCHED",
                "the emergency stop is latched",
                Severity::Error,
                "machine",
                Some("release the e-stop and reset the machine"),
            ));
        }
        if machine.task.power != MachinePower::On {
            out.push(Blocker::new(
                "MACHINE_OFF",
                "machine power is off",
                Severity::Error,
                "machine",
                Some("power on the machine"),
            ));
        }
        if machine.machine.fault {
            out.push(Blocker::new(
                "MACHINE_FAULT",
                "the controller reports a machine fault",
                Severity::Error,
                "machine",
                Some("read the alarms and clear the fault"),
            ));
        }
        if machine.panel.protect_effective() {
            out.push(Blocker::new(
                "PROTECT_ACTIVE",
                "protection is on: program, offset and tool writes are locked",
                Severity::Warning,
                "panel",
                Some("turn PROTECT off"),
            ));
        }

        let unhomed: Vec<String> = machine
            .positions
            .axes
            .iter()
            .filter(|(axis, _)| !machine.machine.homed.get(axis).copied().unwrap_or(false))
            .map(|(axis, _)| axis.to_string())
            .collect();
        if !unhomed.is_empty() {
            out.push(Blocker::new(
                "AXES_UNHOMED",
                format!("axes not homed: {}", unhomed.join(", ")),
                Severity::Warning,
                "machine",
                Some("home every axis before running a program"),
            ));
        }

        if machine.task.interp != InterpState::Idle || machine.task.running {
            out.push(Blocker::new(
                "NOT_IDLE",
                format!(
                    "the interpreter is {:?}{}",
                    machine.task.interp,
                    if machine.task.running { " and a program is running" } else { "" }
                ),
                Severity::Warning,
                "program",
                None,
            ));
        }

        if machine.tool_table.error.is_some() {
            out.push(Blocker::new(
                "TOOL_TABLE_ERROR",
                format!(
                    "the tool table did not load: {}",
                    machine.tool_table.error.as_deref().unwrap_or("unknown")
                ),
                Severity::Error,
                "config",
                Some("fix the tool table path or its columns"),
            ));
        }

        if machine.program.path.is_none() && machine.program.display_name.is_empty() {
            out.push(Blocker::new(
                "NO_PROGRAM",
                "no program is loaded",
                Severity::Info,
                "program",
                Some("load a program from the Program page"),
            ));
        }

        // Mode-specific preconditions. These mirror `MachineSnapshot`'s own
        // predicates, so a write the UI would refuse is refused here with the
        // same reason.
        let panel_mode = machine.panel.mode.mode();
        if let Some(panel_mode) = panel_mode {
            if machine.task.mode != crate::model::TaskMode::Unknown
                && !panel_mode_matches(panel_mode, machine.task.mode)
            {
                out.push(Blocker::new(
                    "MODE_MISMATCH",
                    format!(
                        "the panel is in {:?} but the controller is in {:?}",
                        panel_mode, machine.task.mode
                    ),
                    Severity::Warning,
                    "panel",
                    None,
                ));
            }
        }

        if let Some(page) = (!ui.page.is_empty()).then_some(ui.page.as_str()) {
            if page == "PROGRAM" && machine.task.mode == crate::model::TaskMode::Mdi {
                out.push(Blocker::new(
                    "WRONG_MODE",
                    "the Program page cannot start a program in MDI mode",
                    Severity::Info,
                    "panel",
                    Some("select AUTO on the panel"),
                ));
            }
        }

        if let Some(index) = machine.offsets.active_index {
            if !machine.offsets.offsets.iter().any(|o| o.index == index) {
                out.push(Blocker::new(
                    "WCS_MISSING",
                    format!("the active system {} has no row", wcs_name(index)),
                    Severity::Warning,
                    "config",
                    None,
                ));
            }
        }

        out
    }

    fn panel_mode_matches(panel: PanelMode, task: crate::model::TaskMode) -> bool {
        use crate::model::TaskMode;
        matches!(
            (panel, task),
            (PanelMode::Auto, TaskMode::Auto)
                | (PanelMode::Mdi, TaskMode::Mdi)
                | (PanelMode::Jog, TaskMode::Manual)
                | (PanelMode::Inc, TaskMode::Manual)
                | (PanelMode::Mpg, TaskMode::Manual)
                | (PanelMode::Home, TaskMode::Manual)
                | (PanelMode::ProgHome, TaskMode::Manual)
                | (PanelMode::Edit, TaskMode::Manual)
        )
    }

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::model::{MachineSnapshot, MachineVariant, PanelSnapshot};

    fn snapshot() -> MachineSnapshot {
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.panel = PanelSnapshot {
            connected: true,
            program_protect_known: true,
            ..PanelSnapshot::default()
        };
        snapshot
    }

    #[test]
    fn envelope_reports_age_and_staleness_from_the_capture_time() {
        let process = ProcessInfo::default();
        let captured = now_ms() - 5_000;
        let envelope = Envelope::new("linc", process, 7, captured, 2_000, Dropped::default(), json!({}));
        assert_eq!(envelope.schema_version, SCHEMA_VERSION);
        assert!(envelope.age_ms >= 5_000, "{}", envelope.age_ms);
        assert!(envelope.stale);

        let fresh = Envelope::new(
            "linc",
            ProcessInfo::default(),
            8,
            now_ms(),
            2_000,
            Dropped::default(),
            json!({}),
        );
        assert!(!fresh.stale);
    }

    #[test]
    fn gates_name_the_panel_machine_and_program_preconditions() {
        let mut snapshot = snapshot();
        snapshot.panel.connected = false;
        let blockers = gates::blockers(&snapshot, &UiSnapshot::default());
        assert!(blockers.iter().any(|b| b.code == "PANEL_OFFLINE"));
        // The report is exhaustive: the program precondition is still listed, so
        // an agent sees the whole picture instead of one reason at a time.
        assert!(blockers.iter().any(|b| b.code == "NO_PROGRAM"));

        let mut snapshot = self::snapshot();
        snapshot.machine.estop = true;
        let blockers = gates::blockers(&snapshot, &UiSnapshot::default());
        assert!(blockers.iter().any(|b| b.code == "ESTOP_LATCHED"));
        assert!(blockers.iter().any(|b| b.code == "MACHINE_OFF"), "{blockers:?}");
        assert!(blockers.iter().any(|b| b.code == "NO_PROGRAM"));

        // An unhomed axis is a warning, not an error: jogging in manual still works.
        let mut snapshot_with_axis = self::snapshot();
        snapshot_with_axis
            .positions
            .axes
            .insert("X".to_string(), crate::model::PositionSet::default());
        snapshot_with_axis.machine.homed.insert("X".to_string(), false);
        let blockers = gates::blockers(&snapshot_with_axis, &UiSnapshot::default());
        let blocker = blockers.iter().find(|b| b.code == "AXES_UNHOMED").cloned();
        assert_eq!(blocker.map(|b| b.severity), Some(Severity::Warning));

        // A tool table that did not load is an error from the config layer.
        let mut snapshot = self::snapshot();
        snapshot.tool_table.error = Some("missing column P".into());
        assert!(gates::blockers(&snapshot, &UiSnapshot::default())
            .iter()
            .any(|b| b.code == "TOOL_TABLE_ERROR"));
    }

    #[test]
    fn publishing_advances_the_sequence_only_when_state_changes() {
        let mut bus = Bus::new(snapshot());
        let start = bus.sequence();
        bus.publish_machine(snapshot());
        assert!(bus.sequence() > start, "a new snapshot is a new sequence");

        let ui = UiSnapshot {
            page: "MACHINE".into(),
            ..UiSnapshot::default()
        };
        bus.publish_ui(ui.clone());
        let after_ui = bus.sequence();
        bus.publish_ui(ui);
        assert_eq!(bus.sequence(), after_ui, "a repeated UI snapshot is a no-op");
    }

    #[test]
    fn ui_actions_are_queued_capped_and_drained() {
        let mut bus = Bus::new(snapshot());
        bus.request_ui_action(UiAction::Fit);
        bus.request_ui_action(UiAction::Page {
            page: "PROGRAM".into(),
        });
        let actions = bus.take_ui_actions();
        assert_eq!(actions.len(), 2);
        assert!(bus.take_ui_actions().is_empty(), "draining empties the queue");

        for _ in 0..40 {
            bus.request_ui_action(UiAction::CycleView);
        }
        assert_eq!(bus.take_ui_actions().len(), 32, "the queue is capped");
    }

    #[test]
    fn transitions_are_recorded_as_events() {
        let mut bus = Bus::new(snapshot());
        let mut next = snapshot();
        next.task.power = crate::model::MachinePower::On;
        bus.publish_machine(next);
        let events = bus.events(None, Some("power"), 10);
        let records = events.get("events").and_then(Value::as_array).unwrap();
        assert_eq!(records.len(), 1, "{events}");
        bus.push_event("estop", Severity::Error, "e-stop true".to_string());
        let events = bus.events(None, Some("estop"), 10);
        let records = events.get("events").and_then(Value::as_array).unwrap();
        assert_eq!(records[0].get("severity").and_then(Value::as_str), Some("error"));
    }

    #[test]
    fn the_summary_carries_the_operator_fields() {
        let mut bus = Bus::new(snapshot());
        bus.publish_machine(snapshot());
        let summary = bus.summary();
        assert_eq!(summary.get("mode").and_then(Value::as_str), Some("Unknown"));
        assert!(summary.get("blockers").and_then(Value::as_array).is_some());
    }

    #[test]
    fn motion_samples_are_retained_at_ten_hertz_and_downsampled() {
        let mut bus = Bus::new(snapshot());
        bus.publish_machine(snapshot());
        assert_eq!(bus.motion_len(), 1);
        // Rewind the sampler instead of sleeping 100 ms per sample.
        bus.last_motion_ms -= 200;
        let mut next = snapshot();
        next.feed.feed = 321.0;
        next.program.current_line = 7;
        bus.publish_machine(next);
        assert_eq!(bus.motion_len(), 2);

        let history = bus.motion_history(None, 10);
        assert_eq!(history["count"], 2);
        assert_eq!(history["samples"][1]["feed"], 321.0);
        assert_eq!(history["samples"][1]["program_line"], 7);
        assert_eq!(history["sample_interval_ms"], 100);

        let sampled = bus.motion_history(None, 1);
        assert_eq!(sampled["count"], 1, "max_samples caps the page");
        assert_eq!(sampled["available"], 2, "the page still reports what was there");
    }

    #[test]
    fn the_motion_ring_caps_and_counts_drops() {
        let mut bus = Bus::new(snapshot());
        bus.motion_limit = 2;
        for _ in 0..3 {
            bus.last_motion_ms -= 200;
            bus.publish_machine(snapshot());
        }
        assert_eq!(bus.motion_len(), 2);
        assert_eq!(bus.dropped().motion, 1);
    }
}
}
