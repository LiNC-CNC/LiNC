//! `diag.*` methods answered from the shared [`linc_core::diag::Bus`].
//!
//! One implementation, so the MCP server, the tests and any future client all
//! see the same payloads.

use std::sync::{Arc, Mutex};

use linc_core::diag::{Bus, DiagError, ProcessInfo};
use serde_json::{json, Value};

use crate::SourceMeta;

/// What the server needs from a state owner.
pub trait Source: Send + Sync + 'static {
    fn process(&self) -> ProcessInfo;
    fn meta(&self) -> SourceMeta;
    fn call(&self, method: &str, params: &Value) -> Result<Value, DiagError>;
}

/// Answers protocol methods from a [`Bus`] the UI publishes into.
pub struct BusSource {
    bus: Arc<Mutex<Bus>>,
    process: ProcessInfo,
    config: Value,
    stale_after_ms: i64,
}

impl BusSource {
    pub fn new(bus: Arc<Mutex<Bus>>, process: ProcessInfo) -> Self {
        Self {
            bus,
            process,
            config: Value::Null,
            stale_after_ms: linc_core::diag::DEFAULT_STALE_MS,
        }
    }

    /// Extra `diag.config` payload (profile summary, INI paths, validation).
    pub fn with_config(mut self, config: Value) -> Self {
        self.config = config;
        self
    }

    pub fn with_stale_after(mut self, stale_after_ms: i64) -> Self {
        self.stale_after_ms = stale_after_ms;
        self
    }

    pub fn bus(&self) -> &Arc<Mutex<Bus>> {
        &self.bus
    }
}

/// Standard base64, so an MCP client can render the PNG as image content.
fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
    }
    out
}

/// Where `diag.export_bundle` writes. Fixed, so a request cannot choose a path.
pub fn export_dir() -> std::path::PathBuf {
    if let Some(dir) = std::env::var_os("LINC_EXPORT_DIR") {
        if !dir.is_empty() {
            return std::path::PathBuf::from(dir);
        }
    }
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".local/state")))
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"));
    state.join("linc").join("exports")
}

fn param_u64(params: &Value, key: &str) -> Option<u64> {
    params.get(key).and_then(Value::as_u64)
}

fn param_i64(params: &Value, key: &str) -> Option<i64> {
    params.get(key).and_then(Value::as_i64)
}

fn param_bool(params: &Value, key: &str) -> Option<bool> {
    params.get(key).and_then(Value::as_bool)
}

fn param_str<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
}

impl BusSource {
    /// Set the request, wait for the UI thread to answer, and keep the PNG for
    /// the caller. Writes `screenshot-latest.png` under the export directory so
    /// an agent can open it without base64.
    fn screenshot(&self) -> Result<Value, DiagError> {
        {
            let mut bus = self.bus.lock().unwrap_or_else(|poison| poison.into_inner());
            bus.request_screenshot();
        }
        for _ in 0..60 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            let mut bus = self.bus.lock().unwrap_or_else(|poison| poison.into_inner());
            if let Some(screenshot) = bus.take_screenshot() {
                let path = export_dir().join("screenshot-latest.png");
                let mut written = None;
                if std::fs::create_dir_all(export_dir()).is_ok()
                    && std::fs::write(&path, &screenshot.png).is_ok()
                {
                    written = Some(path);
                }
                return Ok(json!({
                    "width": screenshot.width,
                    "height": screenshot.height,
                    "bytes": screenshot.png.len(),
                    "at_ms": screenshot.at_ms,
                    "path": written,
                    "png_base64": base64(&screenshot.png),
                }));
            }
        }
        Err(DiagError::new(
            "TIMEOUT",
            "the UI thread did not capture a frame within 3 s",
            true,
        ))
    }

}

impl Source for BusSource {
    fn process(&self) -> ProcessInfo {
        self.process.clone()
    }

    fn meta(&self) -> SourceMeta {
        let bus = self.bus.lock().unwrap_or_else(|poison| poison.into_inner());
        SourceMeta {
            sequence: bus.sequence(),
            captured_at_ms: bus.captured_at_ms(),
            stale_after_ms: self.stale_after_ms,
            dropped: bus.dropped(),
        }
    }

    fn call(&self, method: &str, params: &Value) -> Result<Value, DiagError> {
        if method == "diag.screenshot" {
            return self.screenshot();
        }
        let bus = self.bus.lock().unwrap_or_else(|poison| poison.into_inner());
        match method {
            "diag.hello" => Ok(json!({
                "protocol": 1,
                "schema_version": linc_core::diag::SCHEMA_VERSION,
                "source": self.process.role,
                "capabilities": [
                    "snapshot", "summary", "ui", "why_blocked", "offsets", "tools", "program",
                    "logs", "events", "config", "metrics", "snapshot_at", "snapshot_diff", "timeline", "export_bundle"
                ],
                "limits": {
                    "max_frame": crate::MAX_FRAME,
                    "request_timeout_ms": 2000,
                    "trail_points_max": 5000,
                    "logs_max": 1000,
                },
                "stale_after_ms": self.stale_after_ms,
            })),
            "diag.ping" => Ok(json!({
                "now_ms": linc_core::diag::now_ms(),
                "sequence": bus.sequence(),
            })),
            "diag.snapshot" => Ok(bus.state(
                param_bool(params, "include_trail").unwrap_or(false),
                param_u64(params, "trail_points").unwrap_or(200) as usize,
            )),
            "diag.ui" => Ok(serde_json::to_value(bus.ui()).unwrap_or(Value::Null)),
            "diag.summary" => Ok(bus.summary()),
            "diag.why_blocked" => Ok(bus.why_blocked()),
            "diag.offsets" => Ok(bus.offsets()),
            "diag.tools" => Ok(bus.tools()),
            "diag.program" => Ok(bus.program(
                param_u64(params, "line_start").unwrap_or(1) as usize,
                param_u64(params, "line_count").unwrap_or(200) as usize,
            )),
            "diag.logs" => Ok(bus.logs(
                param_i64(params, "since_ms"),
                param_str(params, "level"),
                param_u64(params, "limit").unwrap_or(100) as usize,
            )),
            "diag.events" => Ok(bus.events(
                param_u64(params, "since_sequence"),
                param_str(params, "kind"),
                param_u64(params, "limit").unwrap_or(100) as usize,
            )),
            "diag.config" => Ok(if self.config.is_null() {
                json!({ "available": false })
            } else {
                self.config.clone()
            }),
            "diag.snapshot_at" => {
                let sequence = param_u64(params, "sequence").unwrap_or(0);
                match bus.snapshot_at(sequence) {
                    Some(entry) => Ok(json!({
                        "sequence": entry.sequence,
                        "captured_at_ms": entry.captured_at_ms,
                        "machine": serde_json::to_value(&entry.snapshot).unwrap_or(Value::Null),
                    })),
                    None => Err(DiagError::not_found(format!(
                        "no retained snapshot at or before {sequence}"
                    ))),
                }
            }
            "diag.snapshot_diff" => {
                let from = param_u64(params, "from_sequence").unwrap_or(0);
                let to = param_u64(params, "to_sequence").unwrap_or_else(|| bus.sequence());
                bus.snapshot_diff(from, to)
                    .map_err(|error| DiagError::not_found(error))
            }
            "diag.timeline" => Ok(bus.timeline(
                param_i64(params, "around_ms"),
                param_i64(params, "window_ms").unwrap_or(5_000),
                param_u64(params, "since_sequence"),
                param_u64(params, "limit").unwrap_or(100) as usize,
            )),
            "diag.export_bundle" => {
                let dir = export_dir();
                std::fs::create_dir_all(&dir)
                    .map_err(|error| DiagError::internal(format!("cannot create {}: {error}", dir.display())))?;
                let stamp = linc_core::diag::now_ms();
                let bundle = dir.join(format!("linc-bundle-{stamp}"));
                std::fs::create_dir_all(&bundle).map_err(|error| {
                    DiagError::internal(format!("cannot create {}: {error}", bundle.display()))
                })?;
                let write = |name: &str, value: &Value| -> Result<(), DiagError> {
                    let path = bundle.join(name);
                    std::fs::write(&path, serde_json::to_vec_pretty(value).unwrap_or_default())
                        .map_err(|error| DiagError::internal(format!("write {}: {error}", path.display())))
                };
                write("hello.json", &json!({ "process": self.process }))?;
                write("summary.json", &bus.summary())?;
                write("snapshot.json", &bus.state(true, 5_000))?;
                write("gates.json", &bus.why_blocked())?;
                write("offsets.json", &bus.offsets())?;
                write("tools.json", &bus.tools())?;
                write("config.json", &self.config)?;
                write("logs.json", &bus.logs(None, None, 1_000))?;
                write("events.json", &bus.events(None, None, 1_000))?;
                Ok(json!({ "path": bundle, "files": 9 }))
            }
            "diag.metrics" => Ok(json!({
                "sequence": bus.sequence(),
                "logs": bus.logs(None, None, 1_000_000).get("logs").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
                "events": bus.events(None, None, 1_000_000).get("events").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
                "dropped": bus.dropped(),
                "captured_at_ms": bus.captured_at_ms(),
            })),
            "diag.cancel" => Ok(json!({ "cancelled": params.get("id").cloned().unwrap_or(Value::Null) })),
            other => Err(DiagError::not_found(format!("unknown method {other:?}"))),
        }
    }
}
