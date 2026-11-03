//! Read-only HAL browser data.
//!
//! `linc-hal` deliberately links only the handful of HAL entry points the HMI
//! needs, so it cannot walk HAL's pin, signal and component lists. `halcmd` can,
//! and its `-s` (script friendly) output is stable text. This module runs
//! `halcmd -s show pin` and `halcmd -s show sig` on a background thread at a low
//! rate, parses the rows and publishes an immutable snapshot.
//!
//! Nothing here writes to HAL: `halcmd` is invoked with `show` subcommands only,
//! and a failure is reported as an unavailable snapshot rather than replaced
//! with pinned-over values from the machine snapshot.

use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// One HAL pin as `halcmd show pin` prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HalPin {
    /// Owning component, the first column of the `show pin` row.
    pub component: String,
    pub name: String,
    pub ty: String,
    pub dir: String,
    pub value: String,
    /// Signal the pin is linked to, if any.
    pub signal: Option<String>,
    /// The marker between the pin and its signal: `<==` read, `==>` write,
    /// `<=>` bidirectional.
    pub link: Option<String>,
}

/// One HAL signal as `halcmd show sig` prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HalSignal {
    pub name: String,
    pub ty: String,
    pub value: String,
    /// Pins attached to the signal, in `halcmd` order.
    pub pins: Vec<String>,
}

/// One successful or failed poll.
#[derive(Debug)]
pub struct HalSnapshot {
    pub available: bool,
    /// Why the poll failed. `None` while `available` is true.
    pub error: Option<String>,
    pub pins: Vec<HalPin>,
    pub signals: Vec<HalSignal>,
    /// When the rows were read, for the age shown beside the browser.
    pub captured: Option<Instant>,
}

impl Default for HalSnapshot {
    fn default() -> Self {
        Self {
            available: false,
            error: Some("halcmd has not been polled yet".into()),
            pins: Vec::new(),
            signals: Vec::new(),
            captured: None,
        }
    }
}

impl HalSnapshot {
    /// Milliseconds since the rows were read, for the staleness readout.
    pub fn age_ms(&self) -> Option<u64> {
        self.captured
            .map(|captured| captured.elapsed().as_millis() as u64)
    }

    /// Component names owning at least one pin, in sorted order. HAL keeps no
    /// component list in `show pin`, and a component with no pins has nothing
    /// the browser can show, so the owners are the component list.
    pub fn components(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.pins.iter().map(|pin| pin.component.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Pins owned by `component`, in `halcmd` order.
    pub fn pins_of<'a>(&'a self, component: &'a str) -> impl Iterator<Item = &'a HalPin> {
        self.pins
            .iter()
            .filter(move |pin| pin.component == component)
    }

    /// The snapshot as one poll would produce it, from raw `halcmd` output.
    fn from_output(pins: &str, signals: &str) -> Self {
        Self {
            available: true,
            error: None,
            pins: parse_pins(pins),
            signals: parse_signals(signals),
            captured: Some(Instant::now()),
        }
    }
}

/// Parse `halcmd -s show pin` output.
///
/// A row is `owner type dir value name` and, when the pin is linked, a marker
/// and the signal name: `and2 bit IN FALSE and2.cycle_gate.in0 <== is-auto`.
pub fn parse_pins(output: &str) -> Vec<HalPin> {
    let mut pins = Vec::new();
    for line in output.lines() {
        let mut columns = line.split_whitespace();
        let (Some(component), Some(ty), Some(dir), Some(value), Some(name)) = (
            columns.next(),
            columns.next(),
            columns.next(),
            columns.next(),
            columns.next(),
        ) else {
            continue;
        };
        let (link, signal) = match (columns.next(), columns.next()) {
            (Some(marker), Some(signal)) if is_link_marker(marker) => {
                (Some(marker.to_string()), Some(signal.to_string()))
            }
            _ => (None, None),
        };
        pins.push(HalPin {
            component: component.to_string(),
            name: name.to_string(),
            ty: ty.to_string(),
            dir: dir.to_string(),
            value: value.to_string(),
            signal,
            link,
        });
    }
    pins
}

/// Parse `halcmd -s show sig` output.
///
/// A row is `type value name` followed by any number of `marker pin` pairs:
/// `bit TRUE axis-x-homed ==> cnc_panel.led_x <== halui.joint.0.is-homed`.
pub fn parse_signals(output: &str) -> Vec<HalSignal> {
    let mut signals = Vec::new();
    for line in output.lines() {
        let mut columns = line.split_whitespace();
        let (Some(ty), Some(value), Some(name)) = (columns.next(), columns.next(), columns.next())
        else {
            continue;
        };
        let mut pins = Vec::new();
        let mut rest = columns.collect::<Vec<_>>();
        while rest.len() >= 2 {
            let pin = rest[1];
            if is_link_marker(rest[0]) {
                pins.push(pin.to_string());
                rest.drain(..2);
            } else {
                rest.remove(0);
            }
        }
        signals.push(HalSignal {
            name: name.to_string(),
            ty: ty.to_string(),
            value: value.to_string(),
            pins,
        });
    }
    signals
}

fn is_link_marker(text: &str) -> bool {
    matches!(text, "<==" | "==>" | "<=>")
}

/// Poll `halcmd` once. Split out from the thread so a test can drive it.
pub fn poll_once() -> HalSnapshot {
    match (show(&["pin"]), show(&["sig"])) {
        (Ok(pins), Ok(signals)) => HalSnapshot::from_output(&pins, &signals),
        (Err(error), _) | (_, Err(error)) => HalSnapshot {
            available: false,
            error: Some(error),
            pins: Vec::new(),
            signals: Vec::new(),
            captured: Some(Instant::now()),
        },
    }
}

/// One read-only `halcmd -s show <what>` call.
fn show(what: &[&str]) -> Result<String, String> {
    let output = Command::new("halcmd")
        .arg("-s")
        .arg("show")
        .args(what)
        .output()
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::NotFound => {
                "halcmd is not on PATH; this build cannot enumerate HAL".to_string()
            }
            _ => format!("halcmd failed to start: {error}"),
        })?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let message = stderr
            .lines()
            .find(|line| !line.trim().is_empty())
            .unwrap_or("halcmd reported no reason")
            .trim();
        return Err(format!(
            "halcmd -s show {} failed: {message}",
            what.join(" ")
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A background `halcmd` poller.
///
/// The snapshot is swapped as a whole, so a reader never sees a half-updated
/// tree and a clone costs one `Arc` increment, not 1500 pin copies.
pub struct HalBrowser {
    snapshot: Arc<RwLock<Arc<HalSnapshot>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl HalBrowser {
    /// Start polling every `interval`. The thread exits when the browser is
    /// dropped; a hung `halcmd` only stops the snapshot from advancing, which
    /// the age readout shows.
    pub fn spawn(interval: Duration) -> Self {
        let snapshot = Arc::new(RwLock::new(Arc::new(HalSnapshot::default())));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let snapshot = Arc::clone(&snapshot);
            let stop = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("linc-hal-browser".into())
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let fresh = Arc::new(poll_once());
                        if let Ok(mut slot) = snapshot.write() {
                            *slot = fresh;
                        }
                        let deadline = Instant::now() + interval;
                        while Instant::now() < deadline {
                            if stop.load(Ordering::Relaxed) {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(50));
                        }
                    }
                })
                .ok()
        };
        Self {
            snapshot,
            stop,
            thread,
        }
    }

    /// The latest snapshot. Cheap: this clones one `Arc`.
    pub fn snapshot(&self) -> Arc<HalSnapshot> {
        self.snapshot
            .read()
            .map(|slot| Arc::clone(&slot))
            .unwrap_or_default()
    }
}

impl Default for HalBrowser {
    fn default() -> Self {
        Self::spawn(Duration::from_secs(1))
    }
}

impl Drop for HalBrowser {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Not joined: `halcmd` may be blocked in a syscall, and the process is
        // on its way out anyway. The stop flag ends the loop at the next tick.
        self.thread.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PINS: &str = "\
and2 bit IN FALSE and2.cycle_gate.in0 <== is-auto
and2 bit OUT FALSE and2.cycle_gate.out ==> cycle-gate
and2 s32 OUT 304 and2.cycle_gate.time
linc bit OUT TRUE linc.hmi-alive
";

    const SIGNALS: &str = "\
bit TRUE axis-x-homed ==> cnc_panel.led_x <== halui.joint.0.is-homed
bit FALSE bdt-button <== cnc_panel.bdt ==> toggle.bdt.in
float 12.5 spindle-pos <== spindle.0.speed-out
";

    #[test]
    fn pins_keep_owner_type_direction_value_and_signal() {
        let pins = parse_pins(PINS);
        assert_eq!(pins.len(), 4);
        assert_eq!(pins[0].component, "and2");
        assert_eq!(pins[0].ty, "bit");
        assert_eq!(pins[0].dir, "IN");
        assert_eq!(pins[0].value, "FALSE");
        assert_eq!(pins[0].signal.as_deref(), Some("is-auto"));
        assert_eq!(pins[0].link.as_deref(), Some("<=="));
        // A pin with no signal still carries its value.
        assert_eq!(pins[2].name, "and2.cycle_gate.time");
        assert_eq!(pins[2].value, "304");
        assert_eq!(pins[2].signal, None);
    }

    #[test]
    fn components_are_the_sorted_pin_owners() {
        let snapshot = HalSnapshot::from_output(PINS, SIGNALS);
        assert_eq!(snapshot.components(), vec!["and2", "linc"]);
        assert_eq!(snapshot.pins_of("and2").count(), 3);
    }

    #[test]
    fn signals_collect_every_attached_pin() {
        let signals = parse_signals(SIGNALS);
        assert_eq!(signals.len(), 3);
        assert_eq!(signals[0].name, "axis-x-homed");
        assert_eq!(signals[0].value, "TRUE");
        assert_eq!(
            signals[0].pins,
            vec!["cnc_panel.led_x", "halui.joint.0.is-homed"]
        );
        assert_eq!(signals[2].ty, "float");
        assert_eq!(signals[2].pins, vec!["spindle.0.speed-out"]);
    }

    #[test]
    fn an_empty_poll_still_reports_availability() {
        let snapshot = HalSnapshot::from_output("", "");
        assert!(snapshot.available);
        assert!(snapshot.pins.is_empty());
        assert!(snapshot.components().is_empty());
        assert!(snapshot.captured.is_some());
    }

    #[test]
    fn malformed_rows_are_skipped_not_guessed() {
        assert!(parse_pins("and2 bit IN\n\n").is_empty());
        assert!(parse_signals("bit TRUE\n").is_empty());
    }
}
