//! The MCP tool catalog and dispatch.
//!
//! Every tool is a thin, typed step over a `diag.*` method. Composition stays
//! here so the socket protocol remains a flat state reader: an agent asking for
//! "health" gets findings assembled from the snapshot and the gate report.

use std::time::Duration;

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
                "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 1000, "default": 100 } },
                "additionalProperties": false
            }),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_panel_events",
            title: "Panel state transitions",
            description: "Recorded panel transitions: link, fault, mode, E-stop, power, protect, overrides, buttons.",
            schema: json!({
                "type": "object",
                "properties": {
                    "since_sequence": { "type": "integer" },
                    "kind": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 500, "default": 100 }
                },
                "additionalProperties": false
            }),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_sim_get_state",
            title: "Simulator panel state",
            description: "The simulated panel's controls, pressed buttons, lamps and injected faults.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_sim_set_panel",
            title: "Change the simulated panel",
            description: "Set panel controls as an operator would: link, mode, overrides, protect, E-stop, power, buttons.",
            schema: json!({
                "type": "object",
                "properties": {
                    "connected": { "type": "boolean" },
                    "mode": { "type": "string", "description": "EDIT, AUTO, MDI, JOG, INC, MPG, HOME or PROG.HOME" },
                    "feed_override": { "type": "number", "minimum": 0, "maximum": 2 },
                    "rapid_override": { "type": "number", "minimum": 0, "maximum": 2 },
                    "spindle_override": { "type": "number", "minimum": 0, "maximum": 2 },
                    "program_protect": { "type": "boolean" },
                    "estop": { "type": "boolean", "description": "true presses the mushroom, false releases it" },
                    "power": { "type": "boolean" },
                    "press": { "type": "array", "items": { "type": "string" } },
                    "release": { "type": "array", "items": { "type": "string" } }
                },
                "additionalProperties": false
            }),
            class: ToolClass::Write,
        },
        ToolDef {
            name: "linc_sim_press_button",
            title: "Press a simulated panel button",
            description: "Press one button by its panel name (cycle_start, rapid, jog_xp, ...), optionally for a bounded time.",
            schema: json!({
                "type": "object",
                "properties": {
                    "button": { "type": "string" },
                    "duration_ms": { "type": "integer", "minimum": 1, "maximum": 60000, "description": "Release the button after this long" }
                },
                "required": ["button"],
                "additionalProperties": false
            }),
            class: ToolClass::Write,
        },
        ToolDef {
            name: "linc_sim_release_button",
            title: "Release a simulated panel button",
            description: "Release one button by its panel name.",
            schema: json!({
                "type": "object",
                "properties": { "button": { "type": "string" } },
                "required": ["button"],
                "additionalProperties": false
            }),
            class: ToolClass::Write,
        },
        ToolDef {
            name: "linc_sim_inject_fault",
            title: "Inject a simulated panel fault",
            description: "Sticky until cleared: link, CRC, timeout, mode or protect faults, or a frozen snapshot.",
            schema: json!({
                "type": "object",
                "properties": {
                    "kind": {
                        "type": "string",
                        "enum": ["disconnect", "timeout", "crc_error", "invalid_mode", "protect_unknown", "stale"]
                    }
                },
                "required": ["kind"],
                "additionalProperties": false
            }),
            class: ToolClass::Write,
        },
        ToolDef {
            name: "linc_sim_clear_faults",
            title: "Clear simulated panel faults",
            description: "Remove every injected simulator fault, including a frozen snapshot.",
            schema: no_args(),
            class: ToolClass::Write,
        },
        ToolDef {
            name: "linc_sim_reset",
            title: "Reset the simulated panel",
            description: "Return the simulator to E-stop latched, power off, overrides at 100 %, no buttons held.",
            schema: no_args(),
            class: ToolClass::Write,
        },
        ToolDef {
            name: "linc_compare_panel_views",
            title: "Compare the HMI's panel copy with the panel service",
            description: "Field-level differences between the panel's own snapshot and the copy the HMI is acting on.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_test_wait_for",
            title: "Wait for a state condition",
            description: "Poll one allowlisted path until it matches, or return what it last saw at the timeout.",
            schema: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "e.g. panel.connected, machine.estop, program.current_line, sim.estop" },
                    "op": { "type": "string", "enum": ["eq", "ne", "lt", "lte", "gt", "gte", "contains"], "default": "eq" },
                    "value": {},
                    "timeout_ms": { "type": "integer", "minimum": 0, "maximum": 60000, "default": 5000 },
                    "interval_ms": { "type": "integer", "minimum": 10, "maximum": 1000, "default": 50 }
                },
                "required": ["path", "value"],
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
                        "enum": ["page", "cycle_view", "show_bounds", "cycle_bounds", "fit", "clear_trail", "select_line"]
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
            name: "linc_get_motion_history",
            title: "Motion history",
            description: "10 Hz samples of positions, following error, servo load, spindle, feed and program lines.",
            schema: json!({
                "type": "object",
                "properties": {
                    "seconds": { "type": "number", "minimum": 0, "maximum": 120, "description": "Only samples this recent" },
                    "max_samples": { "type": "integer", "minimum": 1, "maximum": 1200, "default": 600 }
                },
                "additionalProperties": false
            }),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_screen_model",
            title: "Screen model",
            description: "Page, viewport, shell regions and toasts; enough to reproduce a layout bug.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_nml",
            title: "NML bridge",
            description: "Bridge process identity, request/reply counters, latency, stderr tail and connection state.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_hal",
            title: "linc.* HAL component",
            description: "The HMI's own HAL outputs and heartbeat, plus whether real HAL introspection is available.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_hal_pins",
            title: "HAL pins (if available)",
            description: "Returns available=false with a reason when real HAL introspection is not linked.",
            schema: json!({
                "type": "object",
                "properties": {
                    "filter": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 5000, "default": 1000 }
                },
                "additionalProperties": false
            }),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_hal_signals",
            title: "HAL signals (if available)",
            description: "Returns available=false with a reason when real HAL introspection is not linked.",
            schema: json!({
                "type": "object",
                "properties": {
                    "filter": { "type": "string" },
                    "limit": { "type": "integer", "minimum": 1, "maximum": 5000, "default": 1000 }
                },
                "additionalProperties": false
            }),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_io",
            title: "Machine I/O",
            description: "Coolant, lube, door, limits, following error and servo load.",
            schema: no_args(),
            class: ToolClass::Read,
        },
        ToolDef {
            name: "linc_get_crash_records",
            title: "Crash records",
            description: "Panic records written by linc, linc-panel and linc-mcp, newest first.",
            schema: json!({
                "type": "object",
                "properties": { "limit": { "type": "integer", "minimum": 1, "maximum": 200, "default": 50 } },
                "additionalProperties": false
            }),
            class: ToolClass::Read,
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
                "cycle_bounds" => json!({ "action": "cycle_bounds" }),
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
        "linc_get_panel_state" | "linc_get_panel_frames" | "linc_get_panel_events" => {
            let panel = panel_client(panel)?;
            match name {
                "linc_get_panel_state" => panel.call("diag.panel", Value::Null),
                "linc_get_panel_frames" => panel.call(
                    "diag.panel_frames",
                    json!({ "limit": args.get("limit").and_then(Value::as_u64).unwrap_or(100) }),
                ),
                _ => panel.call(
                    "diag.panel_events",
                    json!({
                        "since_sequence": args.get("since_sequence").cloned().unwrap_or(Value::Null),
                        "kind": args.get("kind").cloned().unwrap_or(Value::Null),
                        "limit": args.get("limit").and_then(Value::as_u64).unwrap_or(100),
                    }),
                ),
            }
        }
        "linc_sim_get_state" => panel_client(panel)?.call("diag.sim_state", Value::Null),
        "linc_sim_set_panel" => {
            if args.as_object().map(|object| object.is_empty()).unwrap_or(true) {
                return Err(DiagError::invalid_argument(
                    "linc_sim_set_panel needs at least one field",
                ));
            }
            panel_client(panel)?.call("diag.sim_set", args.clone())
        }
        "linc_sim_press_button" => {
            let button = required_button(args)?;
            let mut params = json!({ "press": [button] });
            if let Some(duration) = args.get("duration_ms").and_then(Value::as_u64) {
                params["duration_ms"] = json!(duration);
            }
            panel_client(panel)?.call("diag.sim_buttons", params)
        }
        "linc_sim_release_button" => {
            let button = required_button(args)?;
            panel_client(panel)?.call("diag.sim_buttons", json!({ "release": [button] }))
        }
        "linc_sim_inject_fault" => {
            let kind = args.get("kind").and_then(Value::as_str).ok_or_else(|| {
                DiagError::invalid_argument("linc_sim_inject_fault needs a kind")
            })?;
            panel_client(panel)?.call("diag.sim_fault", json!({ "kind": kind }))
        }
        "linc_sim_clear_faults" => panel_client(panel)?.call("diag.sim_clear_faults", Value::Null),
        "linc_sim_reset" => panel_client(panel)?.call("diag.sim_reset", Value::Null),
        "linc_compare_panel_views" => {
            let panel_envelope = panel_client(panel)?.call("diag.panel", Value::Null)?;
            let hmi_envelope = client.call("diag.snapshot", json!({ "include_trail": false }))?;
            let data = crate::compare::compare_views(&hmi_envelope, &panel_envelope);
            Ok(rewrap(&hmi_envelope, data))
        }
        "linc_test_wait_for" => {
            let condition = crate::wait::Condition::parse(args)?;
            let timeout = Duration::from_millis(
                args.get("timeout_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(5_000)
                    .min(60_000),
            );
            let interval = Duration::from_millis(
                args.get("interval_ms")
                    .and_then(Value::as_u64)
                    .unwrap_or(50)
                    .clamp(10, 1_000),
            );
            let mut panel = panel;
            let outcome = crate::wait::wait_for(&condition, timeout, interval, || {
                read_condition_value(client, panel.as_deref_mut(), condition.path())
            })?;
            let envelope = client.call("diag.snapshot", json!({ "include_trail": false }))?;
            let data = json!({
                "matched": outcome.matched,
                "elapsed_ms": outcome.elapsed_ms,
                "samples": outcome.samples,
                "path": condition.path(),
                "last_value": outcome.value,
            });
            Ok(rewrap(&envelope, data))
        }
        "linc_get_motion_history" => client.call(
            "diag.motion_history",
            json!({
                "seconds": args.get("seconds").cloned().unwrap_or(Value::Null),
                "max_samples": args.get("max_samples").and_then(Value::as_u64).unwrap_or(600),
            }),
        ),
        "linc_get_screen_model" => client.call("diag.screen_model", Value::Null),
        "linc_get_nml" => client.call("diag.nml", Value::Null),
        "linc_get_hal" => client.call("diag.hal", Value::Null),
        "linc_get_hal_pins" => client.call(
            "diag.hal_pins",
            json!({
                "filter": args.get("filter").cloned().unwrap_or(Value::Null),
                "limit": args.get("limit").and_then(Value::as_u64).unwrap_or(1000),
            }),
        ),
        "linc_get_hal_signals" => client.call(
            "diag.hal_signals",
            json!({
                "filter": args.get("filter").cloned().unwrap_or(Value::Null),
                "limit": args.get("limit").and_then(Value::as_u64).unwrap_or(1000),
            }),
        ),
        "linc_get_io" => client.call("diag.io", Value::Null),
        "linc_get_crash_records" => client.call(
            "diag.crash_records",
            json!({ "limit": args.get("limit").and_then(Value::as_u64).unwrap_or(50) }),
        ),
        "linc_get_metrics" => client.call("diag.metrics", Value::Null),
        other => Err(DiagError::not_found(format!("unknown tool {other:?}"))),
    }
}

fn panel_client(panel: Option<&mut Client>) -> Result<&mut Client, DiagError> {
    panel.ok_or_else(|| {
        DiagError::new(
            "PANEL_NOT_RUNNING",
            "no panel diagnostic socket; start linc-panel (or pass --panel-socket)",
            true,
        )
    })
}

fn required_button(args: &Value) -> Result<String, DiagError> {
    args.get("button")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| DiagError::invalid_argument("this tool needs a button name"))
}

/// Read one allowlisted wait path. `panel.*` and `sim.*` come from the panel
/// socket; everything else is the HMI's snapshot.
fn read_condition_value(
    client: &mut Client,
    panel: Option<&mut Client>,
    path: &str,
) -> Result<Value, DiagError> {
    if let Some(key) = path.strip_prefix("panel.") {
        let envelope = panel_client(panel)?.call("diag.panel", Value::Null)?;
        return Ok(panel_field(&envelope["data"]["snapshot"], key));
    }
    if let Some(key) = path.strip_prefix("sim.") {
        let envelope = panel_client(panel)?.call("diag.sim_state", Value::Null)?;
        return Ok(envelope["data"]["state"].get(key).cloned().unwrap_or(Value::Null));
    }
    let envelope = client.call("diag.snapshot", json!({ "include_trail": false }))?;
    let machine = &envelope["data"]["machine"];
    Ok(match path {
        "machine.estop" => machine["machine"]["estop"].clone(),
        "machine.on" => machine["machine"]["on"].clone(),
        "machine.fault" => machine["machine"]["fault"].clone(),
        "machine.homing" => json!(any_true(&machine["machine"]["homing"])),
        "machine.nml_command_connected" => machine["machine"]["nml_command_connected"].clone(),
        "machine.nml_status_connected" => machine["machine"]["nml_status_connected"].clone(),
        "task.power" => machine["task"]["power"].clone(),
        "task.mode" => machine["task"]["mode"].clone(),
        "task.interp" => machine["task"]["interp"].clone(),
        "task.paused" => machine["task"]["paused"].clone(),
        "task.running" => machine["task"]["running"].clone(),
        "task.motion_active" => machine["task"]["motion_active"].clone(),
        "program.loaded" => json!(!machine["program"]["path"].is_null()),
        "program.running" => machine["program"]["running"].clone(),
        "program.paused" => machine["program"]["paused"].clone(),
        "program.dirty" => machine["program"]["dirty"].clone(),
        "program.current_line" => machine["program"]["current_line"].clone(),
        "program.motion_line" => machine["program"]["motion_line"].clone(),
        "program.progress" => machine["program"]["progress"].clone(),
        "alarms.length" => json!(machine["alarms"].as_array().map(Vec::len).unwrap_or(0)),
        "ui.page" => envelope["data"]["ui"]["page"].clone(),
        other => {
            return Err(DiagError::invalid_argument(format!(
                "unknown condition path {other:?}"
            )))
        }
    })
}

fn panel_field(snapshot: &Value, key: &str) -> Value {
    match key {
        "mode" => json!(crate::compare::mode_text(&snapshot["mode"])),
        "protect" => json!(
            !snapshot["program_protect_known"].as_bool().unwrap_or(false)
                || snapshot["program_protect"].as_bool().unwrap_or(false)
        ),
        "power" => snapshot["power_on"].clone(),
        other => snapshot.get(other).cloned().unwrap_or(Value::Null),
    }
}

/// True when an axis map has any true value, whatever shape it serialized to.
fn any_true(value: &Value) -> bool {
    match value {
        Value::Bool(value) => *value,
        Value::Array(items) => items.iter().any(any_true),
        Value::Object(map) => map.values().any(any_true),
        _ => false,
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
