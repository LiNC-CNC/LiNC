//! Panel-side state for `linc-panel`'s diagnostic socket.
//!
//! The panel answers `diag.panel`, `diag.panel_frames` and `diag.metrics`. It
//! does not answer machine methods: those belong to `linc`.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use linc_core::diag::{DiagError, Dropped, ProcessInfo};
use linc_core::model::PanelSnapshot;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::source::Source;
use crate::SourceMeta;

/// One raw frame, hex encoded and capped so a burst cannot grow without bound.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FrameRecord {
    pub sequence: u64,
    pub at_ms: i64,
    /// `rx` for panel→HMI, `tx` for HMI→panel.
    pub direction: String,
    pub bytes: usize,
    pub hex: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decoded: Option<String>,
}

pub struct PanelBus {
    snapshot: PanelSnapshot,
    sequence: u64,
    captured_at_ms: i64,
    frames: VecDeque<FrameRecord>,
    frame_limit: usize,
    dropped: Dropped,
}

impl Default for PanelBus {
    fn default() -> Self {
        Self::new()
    }
}

impl PanelBus {
    pub fn new() -> Self {
        Self {
            snapshot: PanelSnapshot::default(),
            sequence: 0,
            captured_at_ms: linc_core::diag::now_ms(),
            frames: VecDeque::new(),
            frame_limit: 500,
            dropped: Dropped::default(),
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

    /// Publish a panel snapshot. The sequence advances only when the payload
    /// changes, so a steady panel does not look busy.
    pub fn publish(&mut self, snapshot: PanelSnapshot) {
        if snapshot == self.snapshot {
            return;
        }
        self.snapshot = snapshot;
        self.sequence += 1;
        self.captured_at_ms = linc_core::diag::now_ms();
    }

    /// Tap one raw frame. Kept for the RS-485 driver; the simulator sends none.
    pub fn push_frame(&mut self, direction: &str, bytes: &[u8], decoded: Option<String>) {
        if self.frames.len() >= self.frame_limit {
            self.frames.pop_front();
            self.dropped.panel_frames += 1;
        }
        let hex: String = bytes.iter().take(64).map(|byte| format!("{byte:02x}")).collect();
        self.frames.push_back(FrameRecord {
            sequence: self.sequence,
            at_ms: linc_core::diag::now_ms(),
            direction: direction.into(),
            bytes: bytes.len(),
            hex,
            decoded,
        });
    }

    pub fn snapshot_value(&self) -> Value {
        serde_json::to_value(&self.snapshot).unwrap_or(Value::Null)
    }

    pub fn frames(&self, limit: usize) -> Value {
        let limit = limit.clamp(1, 500);
        let frames: Vec<Value> = self
            .frames
            .iter()
            .rev()
            .take(limit)
            .map(|frame| serde_json::to_value(frame).unwrap_or(Value::Null))
            .collect();
        json!({ "frames": frames, "next_cursor": Value::Null })
    }
}

pub struct PanelSource {
    bus: Arc<Mutex<PanelBus>>,
    process: ProcessInfo,
}

impl PanelSource {
    pub fn new(bus: Arc<Mutex<PanelBus>>, process: ProcessInfo) -> Self {
        Self { bus, process }
    }
}

fn param_u64(params: &Value, key: &str) -> Option<u64> {
    params.get(key).and_then(Value::as_u64)
}

impl Source for PanelSource {
    fn process(&self) -> ProcessInfo {
        self.process.clone()
    }

    fn meta(&self) -> SourceMeta {
        let bus = self.bus.lock().unwrap_or_else(|poison| poison.into_inner());
        SourceMeta {
            sequence: bus.sequence(),
            captured_at_ms: bus.captured_at_ms(),
            stale_after_ms: linc_core::diag::DEFAULT_STALE_MS,
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
                "capabilities": ["panel", "panel_frames", "metrics"],
                "limits": { "max_frame": crate::MAX_FRAME, "panel_frames_max": 500 },
                "stale_after_ms": linc_core::diag::DEFAULT_STALE_MS,
            })),
            "diag.ping" => Ok(json!({
                "now_ms": linc_core::diag::now_ms(),
                "sequence": bus.sequence(),
            })),
            "diag.panel" => Ok(json!({
                "snapshot": bus.snapshot_value(),
                "frames_available": bus.frames(1).get("frames").and_then(Value::as_array).map(Vec::len).unwrap_or(0),
            })),
            "diag.panel_frames" => Ok(bus.frames(param_u64(params, "limit").unwrap_or(100) as usize)),
            "diag.metrics" => Ok(json!({
                "sequence": bus.sequence(),
                "captured_at_ms": bus.captured_at_ms(),
                "dropped": bus.dropped(),
            })),
            other => Err(DiagError::not_found(format!("unknown method {other:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_panel_bus_advances_only_on_a_change() {
        let mut bus = PanelBus::new();
        let start = bus.sequence();
        bus.publish(PanelSnapshot::default());
        assert_eq!(bus.sequence(), start, "an identical snapshot is not new state");

        let mut next = PanelSnapshot::default();
        next.connected = true;
        bus.publish(next.clone());
        assert!(bus.sequence() > start);
        let after = bus.sequence();
        bus.publish(next);
        assert_eq!(bus.sequence(), after);
    }

    #[test]
    fn frames_are_capped_and_hex_encoded() {
        let mut bus = PanelBus::new();
        for index in 0..(bus.frame_limit + 5) {
            bus.push_frame("rx", &[index as u8, 0xaa], None);
        }
        assert_eq!(bus.frames.len(), bus.frame_limit);
        assert_eq!(bus.dropped.panel_frames, 5);
        let latest = bus.frames(1);
        let frame = &latest["frames"][0];
        assert_eq!(frame["direction"], "rx");
        assert_eq!(frame["bytes"], 2);
        assert_eq!(frame["hex"], format!("{:02x}aa", bus.frame_limit as u8 + 4));
    }
}
