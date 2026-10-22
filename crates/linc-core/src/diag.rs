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
    pub toasts: Vec<String>,
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
}

impl Dropped {
    pub fn any(&self) -> bool {
        self.events > 0 || self.logs > 0 || self.snapshots > 0 || self.panel_frames > 0
    }
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
}
}
