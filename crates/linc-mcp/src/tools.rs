//! The MCP tool catalog and dispatch.
//!
//! Every tool is a thin, typed step over a `diag.*` method. Composition stays
//! here so the socket protocol remains a flat state reader: an agent asking for
//! "health" gets findings assembled from the snapshot and the gate report.

use linc_core::diag::DiagError;
use linc_diag::Client;
use serde_json::{json, Value};

/// What a tool is allowed to do. `Export` writes a file under a fixed directory
/// and needs no opt-in; `Write` can move the machine and is gated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolClass {
    Read,
    /// Writes a file under a fixed directory, or changes the HMI's own view.
    Export,
    Write,
}

impl ToolClass {
    pub fn read_only(self) -> bool {
        matches!(self, ToolClass::Read)
    }

    pub fn destructive(self) -> bool {
        matches!(self, ToolClass::Write)
    }

    pub fn gated(self) -> bool {
        matches!(self, ToolClass::Write)
    }
}

pub struct ToolDef {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub schema: Value,
    pub class: ToolClass,
}

fn no_args() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

fn limit_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 }
        },
        "additionalProperties": false
    })
}

pub fn catalog() -> Vec<ToolDef> {
    vec![
        ToolDef {
            name: "linc_ping",
            title: "Ping the LiNC diagnostic socket",
            description: "Liveness and clock sample. Use before a long series of calls.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_build_info",
            title: "Build and protocol information",
            description: "Server metadata, protocol version, capabilities and limits.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_environment",
            title: "Runtime environment",
            description: "Allowlisted process environment, profile, runtime directory and display.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_health_check",
            title: "Health report",
            description: "Findings from freshness, drops, panel link, machine safety and the gate report.",
            schema: json!({
                "type": "object",
                "properties": {
                    "stale_ms": { "type": "integer", "minimum": 100, "default": 2000 }
                },
                "additionalProperties": false
            }),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_state_summary",
            title: "Operator state summary",
            description: "Mode, interpreter, power, panel, homing, program, tool, WCS, alarms and blockers.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_snapshot",
            title: "Machine and UI snapshot",
            description: "Full snapshot. The live trail is excluded unless include_trail is set.",
            schema: json!({
                "type": "object",
                "properties": {
                    "include_trail": { "type": "boolean", "default": false },
                    "trail_points": { "type": "integer", "minimum": 1, "maximum": 5000, "default": 200 }
                },
                "additionalProperties": false
            }),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_why_blocked",
            title: "Why is an action blocked?",
            description: "The shared gate report: every blocker with code, severity, source and remediation.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_ui_state",
            title: "UI state",
            description: "Page, mode, program view, toolpath view, zoom, selected line and toasts.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_program",
            title: "Program state and text",
            description: "Loaded program metadata and a line window of its content.",
            schema: json!({
                "type": "object",
                "properties": {
                    "line_start": { "type": "integer", "minimum": 1, "default": 1 },
                    "line_count": { "type": "integer", "minimum": 1, "maximum": 2000, "default": 200 }
                },
                "additionalProperties": false
            }),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_offsets",
            title: "Work offsets",
            description: "Active system, all rows, rotations and the G92 offsets.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_tools",
            title: "Tool table and tool change",
            description: "Tool table rows, columns, load error, current/prepared tool and ATC state.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_config",
            title: "Configuration",
            description: "Profile summary, INI paths and validation result.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_logs",
            title: "Recent logs",
            description: "The app's log ring, newest last.",
            schema: json!({
                "type": "object",
                "properties": {
                    "since_ms": { "type": "integer" },
                    "level": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 }
                },
                "additionalProperties": false
            }),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_events",
            title: "State transitions",
            description: "Recorded transitions (power, e-stop, panel, mode, interpreter, program, alarms).",
            schema: json!({
                "type": "object",
                "properties": {
                    "since_sequence": { "type": "integer" },
                    "kind": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 }
                },
                "additionalProperties": false
            }),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_snapshot_at",
            title: "Snapshot at a sequence",
            description: "A retained snapshot (at most one minute of history, 1 Hz).",
            schema: json!({
                "type": "object",
                "properties": { "sequence": { "type": "integer", "minimum": 0 } },
                "required": ["sequence"],
                "additionalProperties": false
            }),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_diff_snapshots",
            title: "Diff two retained snapshots",
            description: "Field-level changes between two sequences; volatile fields are ignored.",
            schema: json!({
                "type": "object",
                "properties": {
                    "from_sequence": { "type": "integer", "minimum": 0 },
                    "to_sequence": { "type": "integer", "minimum": 0 }
                },
                "required": ["from_sequence"],
                "additionalProperties": false
            }),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_timeline",
            title: "Merged log and event timeline",
            description: "Logs and state transitions in time order.",
            schema: json!({
                "type": "object",
                "properties": {
                    "around_ms": { "type": "integer" },
                    "window_ms": { "type": "integer", "minimum": 1, "default": 5000 },
                    "since_sequence": { "type": "integer" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 }
                },
                "additionalProperties": false
            }),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_export_bundle",
            title: "Export a diagnostic bundle",
            description: "Writes snapshot, summary, gates, offsets, tools, config, logs and events to a fixed export directory.",
            schema: no_args(),
            class: ToolClass::Export,
        },
        ToolDef {
            name: "linc_get_panel_state",
            title: "Panel state",
            description: "The panel service's own snapshot: link, mode, overrides, buttons and protect.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_panel_frames",
            title: "Raw panel frames",
            description: "Recent raw frames tapped from the panel link, newest first.",
            schema: json!({
                "type": "object",
                "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 100 } },
                "additionalProperties": false
            }),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_ui_action",
            title: "Drive the HMI",
            description: "Queue an HMI intent: go to a page, cycle the view, show/hide bounds, fit, clear the trail, select a line.",
            schema: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["page", "cycle_view", "show_bounds", "fit", "clear_trail", "select_line"]
                    },
                    "page": { "type": "string", "description": "For action=page: MACHINE, PROGRAM, FILES, TOOLS, OFFSETS, DIAGNOSTICS, HELP, SETTINGS, MANUAL, HOMING, MDI, PROBE" },
                    "visible": { "type": "boolean", "description": "For action=show_bounds" },
                    "line": { "type": "integer", "minimum": 1, "description": "For action=select_line" }
                },
                "required": ["action"],
                "additionalProperties": false
            }),
            class: ToolClass::Export,
        },
        ToolDef {
            name: "linc_screenshot",
            title: "Screenshot of the HMI",
            description: "Captures the window as PNG (image content plus the file path).",
            schema: no_args(),
            class: ToolClass::Export,
        },
        ToolDef {
            name: "linc_get_metrics",
            title: "Diagnostic metrics",
            description: "Sequence, ring sizes, drop counters and capture time.",
            schema: no_args(),
            class: ToolClass::Read,
        },
    ]
}

/// Copy the envelope metadata and replace the payload.
fn rewrap(envelope: &Value, data: Value) -> Value {
    let mut out = envelope.clone();
    if let Some(object) = out.as_object_mut() {
        object.insert("data".into(), data);
    }
    out
}

pub fn call(
    client: &mut Client,
    panel: Option<&mut Client>,
    name: &str,
    args: &Value,
) -> Result<Value, DiagError> {
    match name {
        "linc_ping" => client.call("diag.ping", Value::Null),
        "linc_get_build_info" => {
            let envelope = client.call("diag.hello", Value::Null)?;
            let data = json!({
                "server": { "name": crate::SERVER_NAME, "title": crate::SERVER_TITLE },
                "protocol_version": crate::PROTOCOL_VERSION,
                "process": envelope.get("process").cloned().unwrap_or(Value::Null),
                "hello": envelope.get("data").cloned().unwrap_or(Value::Null),
            });
            Ok(rewrap(&envelope, data))
        }
        "linc_get_environment" => {
            let envelope = client.call("diag.hello", Value::Null)?;
            let allow = [
                "LINC_CONFIG",
                "LINC_DIAG_DIR",
                "LINC_INSTANCE",
                "LINC_SIM_IPC",
                "RUST_LOG",
                "XDG_RUNTIME_DIR",
                "XDG_SESSION_TYPE",
                "DISPLAY",
                "WAYLAND_DISPLAY",
            ];
            let mut env = serde_json::Map::new();
            for key in allow {
                if let Ok(value) = std::env::var(key) {
                    env.insert(key.into(), Value::String(value));
                }
            }
            env.insert("cwd".into(), json!(std::env::current_dir().ok()));
            env.insert("exe".into(), json!(std::env::current_exe().ok()));
            let data = json!({
                "environment": env,
                "diagnostic_dir": linc_diag::discovery::runtime_dir(),
                "hello": envelope.get("data").cloned().unwrap_or(Value::Null),
            });
            Ok(rewrap(&envelope, data))
        }
        "linc_health_check" => {
            let stale_ms = args
                .get("stale_ms")
                .and_then(Value::as_i64)
                .unwrap_or(2_000);
            let snapshot = client.call("diag.snapshot", json!({ "include_trail": false }))?;
            let blocked = client.call("diag.why_blocked", Value::Null)?;
            let age_ms = snapshot.get("age_ms").and_then(Value::as_i64).unwrap_or(0);
            let mut findings: Vec<Value> = Vec::new();
            let mut worst = "ok";
            if age_ms > stale_ms {
                findings.push(json!({
                    "code": "STALE_SNAPSHOT",
                    "severity": "error",
                    "message": format!("state is {age_ms} ms old (limit {stale_ms} ms)"),
                }));
                worst = "error";
            }
            let dropped = snapshot
                .get("dropped")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let any_dropped = ["events", "logs", "snapshots", "panel_frames"]
                .iter()
                .any(|key| dropped.get(*key).and_then(Value::as_u64).unwrap_or(0) > 0);
            if any_dropped {
                findings.push(json!({
                    "code": "DROPPED_RECORDS",
                    "severity": "warning",
                    "message": format!("dropped counters are non-zero: {dropped}"),
                }));
                if worst == "ok" {
                    worst = "warning";
                }
            }
            if let Some(blockers) = blocked
                .get("data")
                .and_then(|data| data.get("blockers"))
                .and_then(Value::as_array)
            {
                for blocker in blockers {
                    let severity = blocker
                        .get("severity")
                        .and_then(Value::as_str)
                        .unwrap_or("info");
                    if severity == "error" {
                        worst = "error";
                    } else if severity == "warning" && worst == "ok" {
                        worst = "warning";
                    }
                    findings.push(blocker.clone());
                }
            }
            let data = json!({
                "status": worst,
                "findings": findings,
                "stale_ms": stale_ms,
            });
            Ok(rewrap(&snapshot, data))
        }
        "linc_get_state_summary" => client.call("diag.summary", Value::Null),
        "linc_get_snapshot" => {
            let params = json!({
                "include_trail": args.get("include_trail").and_then(Value::as_bool).unwrap_or(false),
                "trail_points": args.get("trail_points").and_then(Value::as_u64).unwrap_or(200),
            });
            client.call("diag.snapshot", params)
        }
        "linc_why_blocked" => client.call("diag.why_blocked", Value::Null),
        "linc_get_ui_state" => client.call("diag.ui", Value::Null),
        "linc_get_program" => {
            let params = json!({
                "line_start": args.get("line_start").and_then(Value::as_u64).unwrap_or(1),
                "line_count": args.get("line_count").and_then(Value::as_u64).unwrap_or(200),
            });
            client.call("diag.program", params)
        }
        "linc_get_offsets" => client.call("diag.offsets", Value::Null),
        "linc_get_tools" => client.call("diag.tools", Value::Null),
        "linc_get_config" => client.call("diag.config", Value::Null),
        "linc_get_logs" => {
            let params = json!({
                "since_ms": args.get("since_ms").cloned().unwrap_or(Value::Null),
                "level": args.get("level").cloned().unwrap_or(Value::Null),
                "limit": args.get("limit").and_then(Value::as_u64).unwrap_or(100),
            });
            client.call("diag.logs", params)
        }
        "linc_get_events" => {
            let params = json!({
                "since_sequence": args.get("since_sequence").cloned().unwrap_or(Value::Null),
                "kind": args.get("kind").cloned().unwrap_or(Value::Null),
                "limit": args.get("limit").and_then(Value::as_u64).unwrap_or(100),
            });
            client.call("diag.events", params)
        }
        "linc_get_snapshot_at" => {
            let sequence = args.get("sequence").and_then(Value::as_u64).unwrap_or(0);
            client.call("diag.snapshot_at", json!({ "sequence": sequence }))
        }
        "linc_diff_snapshots" => {
            let from = args.get("from_sequence").and_then(Value::as_u64).unwrap_or(0);
            let params = json!({
                "from_sequence": from,
                "to_sequence": args.get("to_sequence").cloned().unwrap_or(Value::Null),
            });
            client.call("diag.snapshot_diff", params)
        }
        "linc_get_timeline" => {
            let params = json!({
                "around_ms": args.get("around_ms").cloned().unwrap_or(Value::Null),
                "window_ms": args.get("window_ms").and_then(Value::as_u64).unwrap_or(5_000),
                "since_sequence": args.get("since_sequence").cloned().unwrap_or(Value::Null),
                "limit": args.get("limit").and_then(Value::as_u64).unwrap_or(100),
            });
            client.call("diag.timeline", params)
        }
        "linc_export_bundle" => client.call("diag.export_bundle", Value::Null),
        "linc_ui_action" => {
            let action = args.get("action").and_then(Value::as_str).unwrap_or("");
            let params = match action {
                "page" => json!({ "action": "page", "page": args.get("page").cloned().unwrap_or(Value::Null) }),
                "cycle_view" => json!({ "action": "cycle_view" }),
                "show_bounds" => json!({
                    "action": "show_bounds",
                    "visible": args.get("visible").and_then(Value::as_bool).unwrap_or(false)
                }),
                "fit" => json!({ "action": "fit" }),
                "clear_trail" => json!({ "action": "clear_trail" }),
                "select_line" => json!({
                    "action": "select_line",
                    "line": args.get("line").and_then(Value::as_u64).unwrap_or(1)
                }),
                other => return Err(DiagError::invalid_argument(format!("unknown ui action {other:?}"))),
            };
            client.call("diag.ui_action", params)
        }
        "linc_screenshot" => client.call("diag.screenshot", Value::Null),
        "linc_get_panel_state" | "linc_get_panel_frames" => {
            let panel = panel.ok_or_else(|| {
                DiagError::new(
                    "PANEL_NOT_RUNNING",
                    "no panel diagnostic socket; start linc-panel (or pass --panel-socket)",
                    true,
                )
            })?;
            if name == "linc_get_panel_state" {
                panel.call("diag.panel", Value::Null)
            } else {
                panel.call(
                    "diag.panel_frames",
                    json!({ "limit": args.get("limit").and_then(Value::as_u64).unwrap_or(100) }),
                )
            }
        }
        "linc_get_metrics" => client.call("diag.metrics", Value::Null),
        other => Err(DiagError::not_found(format!("unknown tool {other:?}"))),
    }
}

/// Tools that can move the machine, which the policy gate refuses by name.
pub fn is_gated(name: &str) -> bool {
    catalog()
        .iter()
        .find(|tool| tool.name == name)
        .map(|tool| tool.class.gated())
        .unwrap_or(false)
}

/// The MCP `tools/list` payload.
pub fn list() -> Value {
    json!({
        "tools": catalog()
            .into_iter()
            .map(|tool| json!({
                "name": tool.name,
                "title": tool.title,
                "description": tool.description,
                "inputSchema": tool.schema,
                "annotations": crate::policy::annotations(tool.class),
            }))
            .collect::<Vec<_>>()
    })
}

#[allow(dead_code)]
fn _unused_limit_schema() -> Value {
    limit_schema()
}
