//! The program interlock, checked against the HAL files themselves.
//!
//! `linc.program-ok` only closes a gate if something routes the pin into the
//! cycle-start chain, and that chain crosses two files: the machine HAL creates
//! the `and2` instance, the post-GUI file nets the pin into it. A rename on either
//! side would leave a gate reading FALSE forever, which looks like a dead panel
//! rather than a broken net, so the wiring is asserted here.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

fn hal_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../hal")
}

fn read(name: &str) -> String {
    let path = hal_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

/// `net <signal> <pin> [=> <pin>...]`. Both `=>` and `<=` are dropped: a pin and
/// the signal it is on are all this test needs.
#[derive(Default)]
struct Nets {
    /// signal -> every pin on it.
    on_signal: BTreeMap<String, BTreeSet<String>>,
}

impl Nets {
    fn parse(text: &str) -> Self {
        let mut nets = Self::default();
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some(rest) = line.strip_prefix("net ") else {
                continue;
            };
            let mut words = rest.split_whitespace();
            let Some(signal) = words.next() else {
                continue;
            };
            for pin in words.filter(|word| *word != "=>" && *word != "<=") {
                nets.on_signal
                    .entry(signal.to_string())
                    .or_default()
                    .insert(pin.to_string());
            }
        }
        nets
    }

    fn merge(files: &[&str]) -> Self {
        let mut nets = Self::default();
        for name in files {
            for (signal, pins) in Self::parse(&read(name)).on_signal {
                nets.on_signal.entry(signal).or_default().extend(pins);
            }
        }
        nets
    }

    /// Pins sharing a signal with `pin`, plus the output of the instance the pin
    /// is an input of: `and2.x.in1` and `and2.x.out` are the same component, and
    /// only the net list tells us what the output carries.
    fn neighbours(&self, pin: &str) -> Vec<String> {
        let mut found: Vec<String> = self
            .on_signal
            .values()
            .filter(|pins| pins.contains(pin))
            .flatten()
            .filter(|other| other.as_str() != pin)
            .cloned()
            .collect();
        if let Some((instance, suffix)) = pin.rsplit_once('.') {
            if suffix.starts_with('i') {
                found.push(format!("{instance}.out"));
            }
        }
        found
    }

    /// Whether `from` and `to` sit on nets that meet, through components that sit
    /// between them. Bounded: a HAL net list is small and a cycle in it must not
    /// hang a test.
    fn connected(&self, from: &str, to: &str) -> bool {
        const MAX_HOPS: usize = 8;
        let mut queue: VecDeque<(String, usize)> = VecDeque::new();
        let mut seen: BTreeSet<String> = BTreeSet::new();
        queue.push_back((from.to_string(), 0));
        while let Some((pin, hops)) = queue.pop_front() {
            if pin == to {
                return true;
            }
            if hops >= MAX_HOPS || !seen.insert(pin.clone()) {
                continue;
            }
            for next in self.neighbours(&pin) {
                // Step through a component: the input pin read by an `and2` is
                // wired to that instance's output in the net list, so both ends
                // are already neighbours of the shared signal.
                queue.push_back((next, hops + 1));
            }
        }
        false
    }

    /// The `and2` instance whose input carries the interlock and whose output
    /// still reaches `destination`.
    fn gate_for(&self, destination: &str) -> Option<String> {
        let interlock = self
            .on_signal
            .iter()
            .find(|(_, pins)| pins.contains("linc.program-ok"))
            .map(|(signal, _)| signal)?;
        for pin in self.on_signal.get(interlock)? {
            let Some((instance, _)) = pin.rsplit_once('.') else {
                continue;
            };
            if !instance.starts_with("and2.") {
                continue;
            }
            let out = format!("{instance}.out");
            if self.connected(&out, destination) {
                return Some(instance.to_string());
            }
        }
        None
    }
}

#[test]
fn the_mill_cycle_start_and_resume_pass_through_the_program_interlock() {
    let nets = Nets::merge(&["linc.hal", "linc-postgui.hal"]);
    for destination in ["halui.program.run", "halui.program.resume"] {
        assert!(
            nets.connected("linc.program-ok", destination),
            "linc.program-ok does not reach {destination}"
        );
        // The interlock has to close a gate, not sit on the pin that drives
        // halui: an `and2` instance carries it.
        assert!(
            nets.gate_for(destination).is_some(),
            "{destination} is not gated by an and2 carrying linc.program-ok"
        );
    }
}

#[test]
fn the_simulation_wires_the_same_gate() {
    let nets = Nets::merge(&["linc.hal", "linc-sim-postgui.hal"]);
    for destination in ["halui.program.run", "halui.program.resume"] {
        assert!(
            nets.connected("linc.program-ok", destination),
            "the simulation does not route linc.program-ok to {destination}"
        );
    }
}

#[test]
fn the_lathe_run_is_gated_and_the_instance_is_declared_before_the_net() {
    let machine = read("linc-lathe.hal");
    let nets = Nets::merge(&["linc-lathe.hal", "linc-lathe-postgui.hal"]);
    let gate = nets
        .gate_for("halui.program.run")
        .expect("the lathe run is gated");
    let loadrt = machine
        .lines()
        .position(|line| line.starts_with("loadrt and2 names=") && line.contains(&gate))
        .unwrap_or_else(|| panic!("{gate} is used but never created"));
    let net = machine
        .lines()
        .position(|line| line.starts_with("net ") && line.contains(&format!("{gate}.in0")))
        .expect("the cycle net uses the gate");
    assert!(
        loadrt < net,
        "{gate} is netted at line {} before HAL creates it at line {}",
        net + 1,
        loadrt + 1
    );
}

/// An `and2` that is created and netted but never added to a thread computes
/// nothing: its output stays at its initial FALSE, so the run it was meant to
/// permit never happens and cycle start looks dead. That was a real defect in
/// this gate, so every instance these files create has to be in a thread.
#[test]
fn every_and2_these_files_create_is_added_to_a_thread() {
    for name in ["linc.hal", "linc-lathe.hal", "baiermill-pb-postgui.hal"] {
        let text = read(name);
        // Continuations first: `loadrt and2 names=a,\` spans lines.
        let code: String = text
            .lines()
            .map(|line| line.split('#').next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
            .replace("\\\n", " ");
        let mut created: Vec<String> = Vec::new();
        for line in code.lines() {
            let Some(rest) = line.split("loadrt").nth(1) else {
                continue;
            };
            let rest = rest.trim_start().strip_prefix("and2").unwrap_or("");
            let Some(names) = rest.trim_start().strip_prefix("names=") else {
                continue;
            };
            created.extend(
                names
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .split(',')
                    .filter(|name| name.starts_with("and2."))
                    .map(str::to_string),
            );
        }
        let added: Vec<String> = code
            .lines()
            .filter_map(|line| line.trim().strip_prefix("addf "))
            .filter_map(|rest| rest.split_whitespace().next())
            .filter(|pin| pin.starts_with("and2."))
            .map(str::to_string)
            .collect();
        for instance in &created {
            assert!(
                added.iter().any(|pin| pin == instance),
                "{name} creates {instance} and never adds it to a thread: it computes nothing, so its output stays FALSE"
            );
        }
    }
}

#[test]
fn every_machine_hal_that_starts_a_program_is_gated() {
    // The list is explicit: a profile with its own cycle-start chain has to be
    // added here, which is the reminder to wire the gate into it.
    for files in [
        vec!["linc.hal", "linc-postgui.hal"],
        vec!["linc.hal", "linc-sim-postgui.hal"],
        vec!["linc-lathe.hal", "linc-lathe-postgui.hal"],
    ] {
        let nets = Nets::merge(&files);
        assert!(
            nets.connected("linc.program-ok", "halui.program.run"),
            "{files:?} starts a program without the interlock"
        );
    }
}

/// `baiermill-pb-postgui.hal` is the INI's POSTGUI_HALFILE, loaded before the HMI
/// creates `linc.*`, so it cannot carry the interlock net itself. Its machine
/// hallib has to net `linc.program-ok` into `and2.cycle-permit` from a file
/// loaded after the HMI starts; until it does, that machine's panel key is gated
/// by `halui`'s own checks and by LiNC's load gate only. This asserts the reason
/// the file is exempt, so nobody "fixes" it by adding a net that cannot load.
#[test]
fn the_baiermill_postgui_file_cannot_carry_the_net() {
    let text = read("baiermill-pb-postgui.hal");
    assert!(
        !text
            .lines()
            .map(|line| line.split('#').next().unwrap_or("").trim())
            .any(|line| line.contains("linc.")),
        "baiermill-pb-postgui.hal is loaded before linc.* exists: a linc.* net there fails HAL start"
    );
    let nets = Nets::parse(&text);
    assert!(
        !nets.connected("linc.program-ok", "halui.program.run"),
        "the file must not pretend to gate on a pin that does not exist yet"
    );
}
