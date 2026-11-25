//! The cycle-start wiring, checked against the HAL files themselves.
//!
//! CYCLE START belongs to the HMI: it reads `cnc_panel.cycle_start`, runs its own
//! refusal checks and sends the run over NML. The HAL used to carry the same key
//! into `halui.program.run` as well, which issued two commands per press and ran
//! a press the HMI had refused. That path is gone, and this file is what keeps it
//! gone: nothing may connect the panel key to halui's program pins again.
//!
//! `linc.program-ok` is still published as state. It no longer gates a HAL path,
//! so this file does not assert that it reaches halui; it asserts the opposite of
//! what it used to, and keeps the two checks that are still true.

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
}

/// The key that used to be a HAL edge must not reach halui's program pins again.
///
/// The assertion is reachability, not a net name: the old lathe net was called
/// `lathe-cycle-start`, and in the mill the key reached `halui` two `and2` gates
/// downstream, so a name-based check would pass on the very wiring it is meant to
/// forbid.
#[test]
fn no_hal_file_wires_the_panel_key_into_halui() {
    const FILES: [&str; 6] = [
        "linc.hal",
        "linc-postgui.hal",
        "linc-sim-postgui.hal",
        "linc-lathe.hal",
        "linc-lathe-postgui.hal",
        "baiermill-pb-postgui.hal",
    ];
    const DESTINATIONS: [&str; 2] = ["halui.program.run", "halui.program.resume"];

    for name in FILES {
        let nets = Nets::parse(&read(name));
        for destination in DESTINATIONS {
            assert!(
                !nets.connected("cnc_panel.cycle_start", destination),
                "{name} carries cnc_panel.cycle_start to {destination}: the HMI owns cycle start, \
                 and a HAL path runs a press the HMI refused"
            );
        }
    }

    // A chain may also cross files: the machine HAL creates the instance, a
    // post-GUI file nets it. Check the merged graph as well.
    let merged = Nets::merge(&FILES);
    for destination in DESTINATIONS {
        assert!(
            !merged.connected("cnc_panel.cycle_start", destination),
            "cnc_panel.cycle_start reaches {destination} across these files: the HMI owns cycle start"
        );
    }
}

/// An `and2` that is created and netted but never added to a thread computes
/// nothing: its output stays at its initial FALSE, so the run it was meant to
/// permit never happens. That was a real defect in the old cycle-start gate, and
/// the same shape remains a load error in the other direction — an `addf` naming
/// an instance no file creates fails at that line.
#[test]
fn every_and2_these_files_create_is_added_to_a_thread() {
    let names = ["linc.hal", "linc-lathe.hal", "baiermill-pb-postgui.hal"];
    let mut created_anywhere: BTreeSet<String> = BTreeSet::new();
    let mut added_anywhere: Vec<(String, String)> = Vec::new();
    for name in names {
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
            // The whole list, not its first whitespace-separated word: taking
            // one word kept 1 of the 24 instances a continued list declares,
            // which is how a dead `addf` stayed invisible here.
            created.extend(
                names
                    .split(',')
                    .map(str::trim)
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
        created_anywhere.extend(created.iter().cloned());
        added_anywhere.extend(added.iter().cloned().map(|pin| (name.to_string(), pin)));
        for instance in &created {
            assert!(
                added.iter().any(|pin| pin == instance),
                "{name} creates {instance} and never adds it to a thread: it computes nothing, so its output stays FALSE"
            );
        }
    }
    // The other direction: an `addf` naming an instance no loaded file creates
    // fails at load, and the created-to-added assertion cannot see it because
    // the name never appears in a `loadrt` list at all.
    for (file, instance) in &added_anywhere {
        assert!(
            created_anywhere.contains(instance),
            "{file} adds {instance} to a thread, but no HAL file creates it: the load fails at that line"
        );
    }
}

/// `baiermill-pb-postgui.hal` is the INI's POSTGUI_HALFILE, loaded before the HMI
/// creates `linc.*`, so it cannot carry a `linc.*` net itself. This asserts the
/// reason the file is exempt, so nobody "fixes" it by adding a net that cannot
/// load.
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
