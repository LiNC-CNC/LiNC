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
        let bus = self.bus.lock().unwrap_or_else(|poison| poison.into_inner());
        match method {
            "diag.hello" => Ok(json!({
                "protocol": 1,
                "schema_version": linc_core::diag::SCHEMA_VERSION,
                "source": self.process.role,
                "capabilities": [
                    "snapshot", "summary", "ui", "why_blocked", "offsets", "tools", "program",
                    "logs", "events", "config", "metrics"
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
