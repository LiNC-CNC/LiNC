//! The sampling loop that turns machine position into probe and collision signals.
//!
//! Owner: wave-2 task C.
//!
//! This module owns the machine position a tick is evaluated at ([Positions])
//! and where that position comes from ([PositionSource]). Two sources ship here:
//! [FilePositionSource], a scripted sample list for tests and replays, and
//! [MockPositionSource], a thread-safe cell a test writes to while the engine
//! reads it.
//!
//! [step_once] is one tick: sample the machine, apply the changes the UI
//! queued, ask the model for probe and collision, publish the snapshot and the
//! signal. [run_engine] is the thread body that calls it. [run_hal] is the
//! headless mode `hal/linc-sim3d.hal` starts; it lives in [crate::hal] and is
//! re-exported here, so `linc_sim3d::engine::run_hal` and
//! `linc_sim3d::hal::run_hal` are one function.

use std::fs::File;
use std::io::{self, BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use crate::model::{SharedState, Snapshot};
use crate::sink::Sim3dSignal;

pub use crate::hal::run_hal;

/// Sampling period used when `hal.sample_ms` is missing or unusable.
pub const DEFAULT_SAMPLE_MS: f64 = 2.0;

/// One sample of the machine state, in machine (joint) millimetres.
///
/// The machine-coordinate convention is
/// `tip = [x, y, z + tool_offset_z]`; the tool table's own Z is resolved by
/// [crate::model::SimModel::active_tool], so [Positions::tool_offset_z] is a
/// live offset added on top of it, not a replacement for it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Positions {
    pub x: f64,
    pub y: f64,
    pub z: f64,
    /// Live tool offset from HAL (`linc-sim3d.tool-offset`), normally zero.
    pub tool_offset_z: f64,
    /// Active tool number from iocontrol (`linc-sim3d.tool-number`).
    pub tool_number: u16,
}

impl Positions {
    /// A position on the programmed point, with no tool offset and tool 0.
    pub fn new(x: f64, y: f64, z: f64) -> Self {
        Self {
            x,
            y,
            z,
            ..Self::default()
        }
    }
}

/// Where the engine gets the next machine position.
///
/// [PositionSource::sample] returns [None] when the source has no further
/// samples; the engine then stops. Implementations must not block for long:
/// they run on the sampling thread.
pub trait PositionSource: Send {
    fn sample(&mut self) -> Option<Positions>;
}

/// A scripted position source: one sample per line.
///
/// Each line is `x y z [tool [tlo]]` in millimetres, separated by whitespace.
/// Blank lines and everything after `#` are ignored, so a fixture can carry
/// comments. A malformed line is logged and skipped: one typo must not look
/// like the end of the file. [PositionSource::sample] returns [None] at end of
/// file and on every call after it.
pub struct FilePositionSource {
    lines: std::io::Lines<Box<dyn BufRead + Send>>,
    finished: bool,
}

impl FilePositionSource {
    /// Open a sample file. Fails only when the file cannot be opened.
    pub fn from_path(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = File::open(path)?;
        Ok(Self::from_reader(BufReader::new(file)))
    }

    /// Read samples from this process's standard input.
    pub fn stdin() -> Self {
        Self::from_reader(BufReader::new(io::stdin()))
    }

    /// Read samples from any buffered reader.
    pub fn from_reader(reader: impl BufRead + Send + 'static) -> Self {
        let reader: Box<dyn BufRead + Send> = Box::new(reader);
        Self {
            lines: reader.lines(),
            finished: false,
        }
    }
}

impl PositionSource for FilePositionSource {
    fn sample(&mut self) -> Option<Positions> {
        if self.finished {
            return None;
        }
        loop {
            match self.lines.next() {
                Some(Ok(line)) => {
                    let line = strip_comment(&line);
                    if line.trim().is_empty() {
                        continue;
                    }
                    match parse_positions(line) {
                        Ok(positions) => return Some(positions),
                        Err(error) => {
                            log::warn!("linc-sim3d: skipping position line {line:?}: {error}");
                            continue;
                        }
                    }
                }
                Some(Err(error)) => {
                    log::warn!("linc-sim3d: position source read error: {error}");
                    self.finished = true;
                    return None;
                }
                None => {
                    self.finished = true;
                    return None;
                }
            }
        }
    }
}

/// A [PositionSource] a test (or the UI) can move between samples.
///
/// Every handle shares the same cell, so a test can keep one handle, hand a
/// clone to the engine thread and write the next position from the test
/// thread; the last position written wins. The source never runs out of
/// samples.
#[derive(Debug, Clone)]
pub struct MockPositionSource {
    current: Arc<Mutex<Positions>>,
}

impl MockPositionSource {
    /// A source sitting at the machine origin.
    pub fn new() -> Self {
        Self::from_positions(Positions::default())
    }

    /// A source sitting at `positions`.
    pub fn from_positions(positions: Positions) -> Self {
        Self {
            current: Arc::new(Mutex::new(positions)),
        }
    }

    /// Move the source. The next sample returns this position.
    pub fn set(&self, positions: Positions) {
        *lock(&self.current) = positions;
    }

    /// The position the next sample will return.
    pub fn get(&self) -> Positions {
        *lock(&self.current)
    }
}

impl Default for MockPositionSource {
    fn default() -> Self {
        Self::new()
    }
}

impl PositionSource for MockPositionSource {
    fn sample(&mut self) -> Option<Positions> {
        Some(self.get())
    }
}

/// One engine tick: publish the machine state at `positions` and ask the model
/// what the probe and the cutter touch.
///
/// The tip is the active tool's tip in machine coordinates,
/// `[x, y, z + tool.z_offset]`. `z_offset` comes from
/// `SimModel::active_tool`, so an offset the machine reports wins, then the
/// tool row's Z, then the config fallback for that tool.
///
/// Probe contact is evaluated only while the spindle holds the probe: a cutter
/// must never drive the touch-probe input. Cutter collision is evaluated for
/// every other tool, and the model never reports the probe as a crash.
///
/// Nothing here panics. A refused action is recorded as a model fault (so it
/// stays visible instead of vanishing with the tick), a tool number the table
/// does not have is a fault too, and every fault the model carries becomes a
/// [Snapshot::faults] entry; a fault the previous snapshot did not carry is
/// also pushed into the signal once. `SharedState` hands out poisoned locks
/// instead of panicking, so a panicking reader cannot wedge the engine.
pub fn step_once(
    shared: &SharedState,
    positions: &Positions,
    signal: &dyn Sim3dSignal,
) -> Snapshot {
    let previous = shared.snapshot();
    let mut faults: Vec<String> = Vec::new();

    // The model changes the UI queued since the last tick, under one write
    // lock so the batch is applied atomically. A refused action is recorded on
    // the model, which is where the UI reads model faults from: keeping it only
    // for the tick that applied it would hide the reason a change vanished.
    let actions = shared.take_actions();
    if !actions.is_empty() {
        let mut model = shared.write_model();
        for action in actions {
            let label = format!("{action:?}");
            if let Err(error) = model.apply(action) {
                let fault = format!("{label} failed: {error}");
                let known = model.faults().iter().any(|seen| seen == &fault);
                if !known {
                    model.record_fault(fault);
                }
            }
        }
    }

    let model = shared.read_model();

    // Standing model faults (a refused action, a skipped workpiece, an
    // unusable tool table) are republished every tick, so a late reader still
    // sees them.
    for fault in model.faults() {
        if !faults.iter().any(|seen| seen == fault) {
            faults.push(fault.clone());
        }
    }

    let tool = model.active_tool(positions.tool_number, positions.tool_offset_z);
    if positions.tool_number != 0 {
        if let Some(table) = model.tool_table() {
            if table.row(positions.tool_number).is_none() {
                let fault = format!(
                    "tool T{} is not in the tool table; the configured fallback offset is used",
                    positions.tool_number
                );
                if !faults.iter().any(|seen| seen == &fault) {
                    faults.push(fault);
                }
            }
        }
    }

    let tip = [positions.x, positions.y, positions.z + tool.z_offset];

    let probe = tool.is_probe().then(|| model.probe_contact(tip));
    let probe_touching = probe.is_some_and(|contact| contact.touching);
    let probe_point = probe
        .filter(|contact| contact.touching)
        .map(|contact| contact.point);
    let probe_workpiece = probe
        .filter(|contact| contact.touching)
        .and_then(|contact| contact.workpiece);

    let cutter = model.cutter_contact(tip, &tool);

    let snapshot = Snapshot {
        tip,
        probe_touching,
        probe_point,
        probe_workpiece,
        collision: cutter.hit,
        collision_point: cutter.hit.then_some(cutter.point),
        collision_target: if cutter.hit { cutter.target } else { None },
        tool_number: positions.tool_number,
        tick: previous.tick + 1,
        faults,
    };
    drop(model);

    signal.tip(tip);
    signal.probe(snapshot.probe_touching);
    signal.collision(snapshot.collision);
    for fault in &snapshot.faults {
        if !previous.faults.iter().any(|seen| seen == fault) {
            signal.fault(fault);
        }
    }

    shared.set_snapshot(snapshot.clone());
    snapshot
}

/// The sampling thread: one [step_once] per `interval` until `stop` is set or
/// the source runs out.
///
/// The loop sleeps for the rest of the interval instead of spinning, so it
/// costs one wake-up per sample. When it ends - stop flag, or end of source -
/// it releases probe and collision, so no consumer is left holding contact
/// nothing maintains any more.
pub fn run_engine(
    shared: Arc<SharedState>,
    mut source: Box<dyn PositionSource>,
    signal: Box<dyn Sim3dSignal>,
    interval: Duration,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        let started = Instant::now();
        let Some(positions) = source.sample() else {
            log::debug!("linc-sim3d: the position source ended");
            break;
        };
        step_once(&shared, &positions, signal.as_ref());
        let remaining = interval.saturating_sub(started.elapsed());
        if !remaining.is_zero() {
            thread::sleep(remaining);
        }
    }

    // Nothing moves any more: never leave contact or collision latched.
    signal.probe(false);
    signal.collision(false);
    log::debug!(
        "linc-sim3d: engine stopped after {} ticks",
        shared.snapshot().tick
    );
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn strip_comment(line: &str) -> &str {
    match line.find('#') {
        Some(index) => &line[..index],
        None => line,
    }
}

fn parse_positions(line: &str) -> Result<Positions, String> {
    let fields: Vec<&str> = line.split_whitespace().collect();
    if fields.len() < 3 || fields.len() > 5 {
        return Err(format!(
            "expected 'x y z [tool [tlo]]', found {} fields",
            fields.len()
        ));
    }
    let number = |index: usize, what: &str| -> Result<f64, String> {
        fields[index]
            .parse::<f64>()
            .map_err(|error| format!("{what} is not a number: {error}"))
    };
    let x = number(0, "x")?;
    let y = number(1, "y")?;
    let z = number(2, "z")?;
    let tool_number = match fields.get(3) {
        Some(field) => field
            .parse::<u16>()
            .map_err(|error| format!("tool number is not a u16: {error}"))?,
        None => 0,
    };
    let tool_offset_z = match fields.get(4) {
        Some(field) => field
            .parse::<f64>()
            .map_err(|error| format!("tool offset is not a number: {error}"))?,
        None => 0.0,
    };
    Ok(Positions {
        x,
        y,
        z,
        tool_offset_z,
        tool_number,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(text: &'static str) -> FilePositionSource {
        FilePositionSource::from_reader(text.as_bytes())
    }

    #[test]
    fn file_source_yields_samples_in_order_then_none() {
        let mut source = source("1.0 2.0 3.0 7 -1.5\n4.0 5.0 6.0\n");
        assert_eq!(
            source.sample(),
            Some(Positions {
                x: 1.0,
                y: 2.0,
                z: 3.0,
                tool_offset_z: -1.5,
                tool_number: 7,
            })
        );
        assert_eq!(source.sample(), Some(Positions::new(4.0, 5.0, 6.0)));
        assert_eq!(source.sample(), None);
        assert_eq!(source.sample(), None, "EOF stays EOF");
    }

    #[test]
    fn file_source_skips_comments_and_blank_lines() {
        let mut source = source("# a fixture\n\n  10 20 30  # trailing\n");
        assert_eq!(source.sample(), Some(Positions::new(10.0, 20.0, 30.0)));
        assert_eq!(source.sample(), None);
    }

    #[test]
    fn file_source_skips_a_malformed_line_without_ending() {
        let mut source = source("1 2\n7 8 9\n");
        assert_eq!(source.sample(), Some(Positions::new(7.0, 8.0, 9.0)));
        assert_eq!(source.sample(), None);
    }

    #[test]
    fn mock_source_survives_a_thread_and_last_write_wins() {
        let handle = MockPositionSource::new();
        let mut engine_side = handle.clone();
        let writer = std::thread::spawn(move || {
            for step in 0..100 {
                handle.set(Positions::new(f64::from(step), 0.0, 0.0));
            }
        });
        writer.join().unwrap();
        let last = engine_side.sample().expect("a mock source never ends");
        assert_eq!(last.x, 99.0);
        assert_eq!(engine_side.sample(), Some(last));
        assert_send(&engine_side);
    }

    fn assert_send<T: Send>(_: &T) {}
}
