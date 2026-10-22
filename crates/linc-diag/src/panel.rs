//! Panel-side state for `linc-panel`'s diagnostic socket.
//!
//! The panel answers `diag.panel`, `diag.panel_frames`, `diag.panel_events` and
//! `diag.metrics`. It does not answer machine methods: those belong to `linc`.
//!
//! Simulator control (`diag.sim_*`) is a write path, so it is only present when
//! the panel service was started with `--allow-sim-control` *and* is running the
//! simulator. The real serial panel has no [`SimControl`] at all, which is what
//! keeps "the panel and its hardware E-stop remain authoritative" true.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use linc_core::diag::{
    now_ms, DiagError, Dropped, PanelEventRecord, PanelFault, ProcessInfo, SimPanelPatch,
};
use linc_core::model::{PanelModeValidity, PanelSnapshot};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::source::Source;
use crate::SourceMeta;

/// Frames retained per panel. At 20 Hz the request, reply and LED frame make
/// about 60 records a second, so this is roughly half a minute of traffic.
pub const FRAME_CAPACITY: usize = 2048;
/// Panel transitions retained per panel.
pub const EVENT_CAPACITY: usize = 500;
/// The largest page a caller may ask for.
pub const FRAME_PAGE_MAX: usize = 1000;

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
    /// `None` when the frame carries no checksum of its own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub crc_ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// A simulator's control surface, as the panel service exposes it.
///
/// Implemented by `linc-panel::sim::SimControl` over the same panel state the
/// simulator window uses, so a change made here is an operator's change: the
/// driver publishes it on its next cycle and `cnc_panel.*` follows.
pub trait SimControl: Send + Sync {
    /// The current simulator state as JSON.
    fn state(&self) -> Value;
    /// Apply a partial change and return the resulting state.
    fn apply(&self, patch: &SimPanelPatch) -> Result<Value, DiagError>;
    /// Press and/or release buttons by name; `duration_ms` releases a pressed
    /// button on its own.
    fn buttons(
        &self,
        press: &[String],
        release: &[String],
        duration_ms: Option<u64>,
    ) -> Result<Value, DiagError>;
    /// Inject a sticky fault. [`PanelFault::Stale`] is handled by the bus.
    fn inject_fault(&self, fault: PanelFault) -> Result<Value, DiagError>;
    /// Clear every injected simulator fault.
    fn clear_faults(&self) -> Result<Value, DiagError>;
    /// Return the simulator to the state a cabinet is found in: E-stop latched,
    /// power off, no faults, overrides at 100 %, no buttons held.
    fn reset(&self) -> Result<Value, DiagError>;
}

pub struct PanelBus {
    snapshot: PanelSnapshot,
    sequence: u64,
    captured_at_ms: i64,
    frames: VecDeque<FrameRecord>,
    frame_limit: usize,
    events: VecDeque<PanelEventRecord>,
    event_limit: usize,
    dropped: Dropped,
    /// Injected `stale` fault: publish stops advancing, so the envelope age
    /// grows and `stale` turns true. The panel itself keeps running.
    stale: bool,
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
            captured_at_ms: now_ms(),
            frames: VecDeque::new(),
            frame_limit: FRAME_CAPACITY,
            events: VecDeque::new(),
            event_limit: EVENT_CAPACITY,
            dropped: Dropped::default(),
            stale: false,
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

    pub fn stale(&self) -> bool {
        self.stale
    }

    /// Freeze or resume publication. While frozen the sequence and capture time
    /// stay put, which is what a stale snapshot looks like to a client.
    pub fn set_stale(&mut self, stale: bool) {
        self.stale = stale;
    }

    /// Publish a panel snapshot. The sequence advances only when the payload
    /// changes, so a steady panel does not look busy.
    pub fn publish(&mut self, snapshot: PanelSnapshot) {
        if self.stale || snapshot == self.snapshot {
            return;
        }
        self.sequence += 1;
        self.captured_at_ms = now_ms();
        let events = transitions(&self.snapshot, &snapshot, self.sequence, self.captured_at_ms);
        self.snapshot = snapshot;
        for event in events {
            self.push_event(event);
        }
    }

    fn push_event(&mut self, event: PanelEventRecord) {
        if self.events.len() >= self.event_limit {
            self.events.pop_front();
            self.dropped.events += 1;
        }
        self.events.push_back(event);
    }

    /// Tap one raw frame. The RS-485 driver calls this for every request,
    /// reply and write; the simulator sends no bytes and records none.
    pub fn push_frame(
        &mut self,
        direction: &str,
        bytes: &[u8],
        decoded: Option<String>,
        crc_ok: Option<bool>,
        error: Option<String>,
    ) {
        if self.frames.len() >= self.frame_limit {
            self.frames.pop_front();
            self.dropped.panel_frames += 1;
        }
        let hex: String = bytes.iter().take(64).map(|byte| format!("{byte:02x}")).collect();
        self.frames.push_back(FrameRecord {
            sequence: self.sequence,
            at_ms: now_ms(),
            direction: direction.into(),
            bytes: bytes.len(),
            hex,
            decoded,
            crc_ok,
            error,
        });
    }

    pub fn snapshot_value(&self) -> Value {
        serde_json::to_value(&self.snapshot).unwrap_or(Value::Null)
    }

    pub fn frames(&self, limit: usize) -> Value {
        let limit = limit.clamp(1, FRAME_PAGE_MAX);
        let frames: Vec<Value> = self
            .frames
            .iter()
            .rev()
            .take(limit)
            .map(|frame| serde_json::to_value(frame).unwrap_or(Value::Null))
            .collect();
        json!({ "frames": frames, "next_cursor": Value::Null })
    }

    pub fn events(
        &self,
        since_sequence: Option<u64>,
        kind: Option<&str>,
        limit: usize,
    ) -> Value {
        let limit = limit.clamp(1, EVENT_CAPACITY);
        let events: Vec<Value> = self
            .events
            .iter()
            .filter(|event| {
                since_sequence
                    .map(|since| event.sequence > since)
                    .unwrap_or(true)
            })
            .filter(|event| kind.map(|wanted| event.kind == wanted).unwrap_or(true))
            .take(limit)
            .map(|event| serde_json::to_value(event).unwrap_or(Value::Null))
            .collect();
        json!({ "events": events, "next_cursor": Value::Null })
    }
}

fn mode_text(mode: &PanelModeValidity) -> String {
    match mode {
        PanelModeValidity::Valid(mode) => mode.label().into(),
        PanelModeValidity::Invalid(_) => "INVALID".into(),
        PanelModeValidity::Unknown => "UNKNOWN".into(),
    }
}

fn protect_text(snapshot: &PanelSnapshot) -> String {
    if !snapshot.program_protect_known {
        "UNKNOWN".into()
    } else if snapshot.program_protect {
        "ON".into()
    } else {
        "OFF".into()
    }
}

fn push_change(
    events: &mut Vec<PanelEventRecord>,
    sequence: u64,
    at_ms: i64,
    kind: &str,
    from: String,
    to: String,
) {
    if from != to {
        events.push(PanelEventRecord {
            sequence,
            at_ms,
            kind: kind.into(),
            from,
            to,
        });
    }
}

/// Every recorded field that differs between two snapshots, as one event each.
fn transitions(
    previous: &PanelSnapshot,
    next: &PanelSnapshot,
    sequence: u64,
    at_ms: i64,
) -> Vec<PanelEventRecord> {
    let mut events = Vec::new();
    push_change(
        &mut events,
        sequence,
        at_ms,
        "connected",
        previous.connected.to_string(),
        next.connected.to_string(),
    );
    push_change(
        &mut events,
        sequence,
        at_ms,
        "fault",
        previous.communication_error.to_string(),
        next.communication_error.to_string(),
    );
    push_change(
        &mut events,
        sequence,
        at_ms,
        "mode",
        mode_text(&previous.mode),
        mode_text(&next.mode),
    );
    push_change(
        &mut events,
        sequence,
        at_ms,
        "estop",
        previous.estop.to_string(),
        next.estop.to_string(),
    );
    push_change(
        &mut events,
        sequence,
        at_ms,
        "power",
        format!("{:?}", previous.power_on),
        format!("{:?}", next.power_on),
    );
    push_change(
        &mut events,
        sequence,
        at_ms,
        "protect",
        protect_text(previous),
        protect_text(next),
    );
    push_change(
        &mut events,
        sequence,
        at_ms,
        "feed_override",
        format!("{:.2}", previous.feed_override),
        format!("{:.2}", next.feed_override),
    );
    push_change(
        &mut events,
        sequence,
        at_ms,
        "rapid_override",
        format!("{:.2}", previous.rapid_override),
        format!("{:.2}", next.rapid_override),
    );
    push_change(
        &mut events,
        sequence,
        at_ms,
        "spindle_override",
        format!("{:.2}", previous.spindle_override),
        format!("{:.2}", next.spindle_override),
    );
    push_change(
        &mut events,
        sequence,
        at_ms,
        "buttons",
        format!("0x{:016x}", previous.buttons),
        format!("0x{:016x}", next.buttons),
    );
    events
}

pub struct PanelSource {
    bus: Arc<Mutex<PanelBus>>,
    process: ProcessInfo,
    sim: Option<Arc<dyn SimControl>>,
}

impl PanelSource {
    /// `sim` is `Some` only for a simulator started with `--allow-sim-control`;
    /// a real panel always passes `None` and never answers `diag.sim_*`.
    pub fn new(bus: Arc<Mutex<PanelBus>>, process: ProcessInfo, sim: Option<Arc<dyn SimControl>>) -> Self {
        Self { bus, process, sim }
    }

    fn control(&self) -> Result<&Arc<dyn SimControl>, DiagError> {
        self.sim.as_ref().ok_or_else(|| {
            DiagError::new(
                "SIM_CONTROL_DISABLED",
                "this panel has no simulator control: start linc-panel with --sim --allow-sim-control",
                false,
            )
        })
    }

    fn snapshot_value(&self) -> Value {
        let bus = self.bus.lock().unwrap_or_else(|poison| poison.into_inner());
        bus.snapshot_value()
    }

    /// Apply a simulator write. The response is taken from the bus *after* the
    /// write, but the panel loop publishes on its own cycle, so the returned
    /// snapshot can be one cycle behind the change; `state` is current.
    fn sim_call(&self, method: &str, params: &Value) -> Result<Value, DiagError> {
        let control = self.control()?.clone();
        match method {
            "diag.sim_state" => {
                let state = control.state();
                Ok(json!({ "armed": true, "state": state, "panel": self.snapshot_value() }))
            }
            "diag.sim_set" => {
                let patch: SimPanelPatch = serde_json::from_value(params.clone()).map_err(|error| {
                    DiagError::invalid_argument(format!("bad sim panel patch: {error}"))
                })?;
                let state = control.apply(&patch)?;
                Ok(json!({ "applied": true, "state": state, "panel": self.snapshot_value() }))
            }
            "diag.sim_buttons" => {
                let press = string_list(params, "press")?;
                let release = string_list(params, "release")?;
                if press.is_empty() && release.is_empty() {
                    return Err(DiagError::invalid_argument(
                        "diag.sim_buttons needs press or release",
                    ));
                }
                let duration = params.get("duration_ms").and_then(Value::as_u64);
                let state = control.buttons(&press, &release, duration)?;
                Ok(json!({ "applied": true, "state": state, "panel": self.snapshot_value() }))
            }
            "diag.sim_fault" => {
                let kind = params
                    .get("kind")
                    .and_then(Value::as_str)
                    .ok_or_else(|| DiagError::invalid_argument("diag.sim_fault needs kind"))?;
                let fault = PanelFault::parse(kind).ok_or_else(|| {
                    DiagError::invalid_argument(format!(
                        "unknown fault {kind:?}; expected one of {}",
                        PanelFault::ALL
                            .iter()
                            .map(|fault| fault.label())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                })?;
                let state = if fault == PanelFault::Stale {
                    self.bus
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .set_stale(true);
                    control.state()
                } else {
                    control.inject_fault(fault)?
                };
                Ok(json!({ "injected": fault.label(), "state": state, "panel": self.snapshot_value() }))
            }
            "diag.sim_clear_faults" => {
                self.bus
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .set_stale(false);
                let state = control.clear_faults()?;
                Ok(json!({ "cleared": true, "state": state, "panel": self.snapshot_value() }))
            }
            "diag.sim_reset" => {
                self.bus
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .set_stale(false);
                let state = control.reset()?;
                Ok(json!({ "reset": true, "state": state, "panel": self.snapshot_value() }))
            }
            other => Err(DiagError::not_found(format!("unknown method {other:?}"))),
        }
    }
}

fn string_list(params: &Value, key: &str) -> Result<Vec<String>, DiagError> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| {
                item.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| DiagError::invalid_argument(format!("{key} must be strings")))
            })
            .collect(),
        Some(_) => Err(DiagError::invalid_argument(format!("{key} must be an array"))),
    }
}

fn param_u64(params: &Value, key: &str) -> Option<u64> {
    params.get(key).and_then(Value::as_u64)
}

fn param_str<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params.get(key).and_then(Value::as_str)
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
        if method.starts_with("diag.sim_") {
            return self.sim_call(method, params);
        }
        let bus = self.bus.lock().unwrap_or_else(|poison| poison.into_inner());
        match method {
            "diag.hello" => Ok(json!({
                "protocol": 1,
                "schema_version": linc_core::diag::SCHEMA_VERSION,
                "source": self.process.role,
                "capabilities": [
                    "panel", "panel_frames", "panel_events", "metrics",
                    if self.sim.is_some() { "sim_control" } else { "panel_read_only" },
                ],
                "limits": {
                    "max_frame": crate::MAX_FRAME,
                    "panel_frames_max": FRAME_PAGE_MAX,
                    "panel_events_max": EVENT_CAPACITY,
                },
                "stale_after_ms": linc_core::diag::DEFAULT_STALE_MS,
            })),
            "diag.ping" => Ok(json!({
                "now_ms": now_ms(),
                "sequence": bus.sequence(),
            })),
            "diag.panel" => Ok(json!({
                "snapshot": bus.snapshot_value(),
                "stale": bus.stale(),
                "sim_control": self.sim.is_some(),
            })),
            "diag.panel_frames" => Ok(bus.frames(param_u64(params, "limit").unwrap_or(100) as usize)),
            "diag.panel_events" => Ok(bus.events(
                param_u64(params, "since_sequence"),
                param_str(params, "kind"),
                param_u64(params, "limit").unwrap_or(100) as usize,
            )),
            "diag.metrics" => Ok(json!({
                "sequence": bus.sequence(),
                "captured_at_ms": bus.captured_at_ms(),
                "stale": bus.stale(),
                "frames": bus.frames.len(),
                "events": bus.events.len(),
                "dropped": bus.dropped(),
            })),
            other => Err(DiagError::not_found(format!("unknown method {other:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connected_snapshot() -> PanelSnapshot {
        let mut snapshot = PanelSnapshot::default();
        snapshot.connected = true;
        snapshot
    }

    #[test]
    fn the_panel_bus_advances_only_on_a_change() {
        let mut bus = PanelBus::new();
        let start = bus.sequence();
        bus.publish(PanelSnapshot::default());
        assert_eq!(bus.sequence(), start, "an identical snapshot is not new state");

        let next = connected_snapshot();
        bus.publish(next.clone());
        assert!(bus.sequence() > start);
        let after = bus.sequence();
        bus.publish(next);
        assert_eq!(bus.sequence(), after);
    }

    #[test]
    fn transitions_are_recorded_once_per_changed_field() {
        let mut bus = PanelBus::new();
        bus.publish(connected_snapshot());
        let before = bus.sequence();
        let mut next = connected_snapshot();
        next.mode = PanelModeValidity::Valid(linc_core::model::PanelMode::Jog);
        next.estop = true;
        next.feed_override = 0.5;
        bus.publish(next);

        let events = bus.events(Some(before), None, 10);
        let events = events["events"].as_array().unwrap();
        let kinds: Vec<&str> = events.iter().filter_map(|event| event["kind"].as_str()).collect();
        assert_eq!(kinds, vec!["mode", "estop", "feed_override"], "{events:?}");
        assert_eq!(events[0]["to"], "JOG");
        assert_eq!(events[2]["to"], "0.50");
    }

    #[test]
    fn a_stale_fault_freezes_the_sequence_and_the_capture_time() {
        let mut bus = PanelBus::new();
        bus.publish(connected_snapshot());
        let sequence = bus.sequence();
        let captured = bus.captured_at_ms();
        bus.set_stale(true);
        let mut next = connected_snapshot();
        next.estop = false;
        next.mode = PanelModeValidity::Valid(linc_core::model::PanelMode::Auto);
        bus.publish(next);
        assert_eq!(bus.sequence(), sequence, "a stale panel does not advance");
        assert_eq!(bus.captured_at_ms(), captured);
        bus.set_stale(false);
        let mut recovered = connected_snapshot();
        recovered.mode = PanelModeValidity::Valid(linc_core::model::PanelMode::Auto);
        bus.publish(recovered);
        assert!(bus.sequence() > sequence, "clearing the fault resumes publication");
    }

    #[test]
    fn frames_are_capped_hex_encoded_and_keep_their_crc_verdict() {
        let mut bus = PanelBus::new();
        for index in 0..(bus.frame_limit + 5) {
            bus.push_frame("rx", &[index as u8, 0xaa], None, Some(true), None);
        }
        assert_eq!(bus.frames.len(), bus.frame_limit);
        assert_eq!(bus.dropped.panel_frames, 5);
        let latest = bus.frames(1);
        let frame = &latest["frames"][0];
        assert_eq!(frame["direction"], "rx");
        assert_eq!(frame["bytes"], 2);
        assert_eq!(frame["crc_ok"], true);
        assert_eq!(frame["hex"], format!("{:02x}aa", bus.frame_limit as u8 + 4));

        bus.push_frame(
            "rx",
            &[0xde, 0xad],
            Some("crc mismatch".into()),
            Some(false),
            Some("CRC mismatch".into()),
        );
        let latest = bus.frames(1);
        let frame = &latest["frames"][0];
        assert_eq!(frame["crc_ok"], false);
        assert_eq!(frame["error"], "CRC mismatch");
    }

    #[test]
    fn a_panel_without_sim_control_refuses_sim_writes() {
        let bus = Arc::new(Mutex::new(PanelBus::new()));
        let source = PanelSource::new(
            bus,
            ProcessInfo {
                pid: std::process::id(),
                role: "panel".into(),
                build: Default::default(),
            },
            None,
        );
        let error = source
            .call("diag.sim_set", &json!({ "estop": false }))
            .expect_err("no simulator");
        assert_eq!(error.code, "SIM_CONTROL_DISABLED");
    }
}
