//! Drift guards for the rig without the 3D simulator.
//!
//! configs/sim-no3d.ini is configs/sim.ini with the linc-sim3d HAL file swapped
//! for hal/linc-sim-no3d.hal, which keeps the panel PROBE key wired to
//! motion.probe-input, and the launcher loads that INI unless `--sim3d` asks
//! for the 3D rig. These tests read the repository files, so editing sim.ini
//! (or renaming the flag or its INI) without the counterpart fails here instead
//! of silently starting the wrong rig.

use std::fs;
use std::path::{Path, PathBuf};

fn repo_file(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn read_repo_file(relative: &str) -> String {
    let path = repo_file(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
}

const SIM3D_HALFILE: &str = "HALFILE = ../hal/linc-sim3d.hal";
const NO3D_HALFILE: &str = "HALFILE = ../hal/linc-sim-no3d.hal";
/// The cargo call the normal rig needs, simulator included.
const NORMAL_BUILD: &str = "cargo build --release -p linc-app -p linc-panel -p linc-sim3d";

/// The lines of an INI, without trailing whitespace or empty lines: the guard
/// is about content, not layout.
fn normalise(text: &str) -> Vec<String> {
    text.lines()
        .map(|line| line.trim_end().to_string())
        .filter(|line| !line.is_empty())
        .collect()
}

/// An INI split into its leading comment block and the rest, which starts at
/// the first section header.
fn header_and_body(text: &str) -> (Vec<String>, Vec<String>) {
    let mut lines = normalise(text);
    let start = lines
        .iter()
        .position(|line| line.trim_start().starts_with('['))
        .unwrap_or(lines.len());
    let header = lines.drain(..start).collect();
    (header, lines)
}

/// The body of sim.ini with the simulator's HALFILE block replaced by the
/// single no-simulator HALFILE line, in the same position.
fn body_with_the_reader_swapped(body: &[String]) -> Vec<String> {
    let index = body
        .iter()
        .position(|line| line.trim() == SIM3D_HALFILE)
        .expect("configs/sim.ini must load hal/linc-sim3d.hal");
    assert!(
        index >= 2,
        "the simulator HALFILE needs its two comment lines above it"
    );
    for above in [index - 1, index - 2] {
        assert!(
            body[above].trim_start().starts_with('#'),
            "line {} of the simulator block must be a comment, found {:?}",
            above + 1,
            body[above]
        );
    }
    body[..index - 2]
        .iter()
        .cloned()
        .chain(std::iter::once(NO3D_HALFILE.to_string()))
        .chain(body[index + 1..].iter().cloned())
        .collect()
}

/// The text between `start` and the next `end`.
fn between<'a>(text: &'a str, start: &str, end: &str) -> &'a str {
    let tail = text
        .split_once(start)
        .unwrap_or_else(|| panic!("scripts/start-sim.sh must contain {start:?}"))
        .1;
    tail.split_once(end)
        .map(|(head, _)| head)
        .unwrap_or_else(|| panic!("scripts/start-sim.sh must have {end:?} after {start:?}"))
}

#[test]
fn sim_no3d_ini_swaps_only_the_probe_reader() {
    let sim = read_repo_file("configs/sim.ini");
    let no3d = read_repo_file("configs/sim-no3d.ini");
    let (sim_header, sim_body) = header_and_body(&sim);
    let (no3d_header, no3d_body) = header_and_body(&no3d);

    // Everything from [EMC] down is sim.ini with the simulator's HALFILE block
    // replaced by the no-simulator reader, in the same place.
    assert_eq!(
        no3d_body,
        body_with_the_reader_swapped(&sim_body),
        "configs/sim-no3d.ini must stay configs/sim.ini with the linc-sim3d \
         HALFILE block replaced by the single linc-sim-no3d HALFILE line: edit \
         both files together"
    );

    // The header keeps the shared text and adds a note that says what differs.
    assert!(
        no3d_header.starts_with(&sim_header),
        "configs/sim-no3d.ini must keep configs/sim.ini's header and add its note"
    );
    let note = no3d_header[sim_header.len()..].join(" ");
    assert!(
        note.contains("without the 3D simulator"),
        "the header note must say what this file is: {note:?}"
    );
    assert!(
        note.contains("linc-sim-no3d.hal"),
        "the header note must name the reader this file loads: {note:?}"
    );
}

#[test]
fn the_simulated_rig_still_loads_the_simulator_once() {
    let sim = read_repo_file("configs/sim.ini");
    let loads = sim
        .lines()
        .filter(|line| line.trim() == SIM3D_HALFILE)
        .count();
    assert_eq!(
        loads, 1,
        "configs/sim.ini loads hal/linc-sim3d.hal exactly once"
    );
}

#[test]
fn the_no_simulator_rig_loads_its_own_probe_reader_once() {
    let no3d = read_repo_file("configs/sim-no3d.ini");
    let (_, body) = header_and_body(&no3d);

    let loads = body
        .iter()
        .filter(|line| line.trim() == NO3D_HALFILE)
        .count();
    assert_eq!(
        loads, 1,
        "configs/sim-no3d.ini loads hal/linc-sim-no3d.hal exactly once"
    );

    let linc_sim = body
        .iter()
        .position(|line| line.trim() == "HALFILE = ../hal/linc-sim.hal")
        .expect("configs/sim-no3d.ini must keep hal/linc-sim.hal");
    let reader = body
        .iter()
        .position(|line| line.trim() == NO3D_HALFILE)
        .expect("the no-simulator reader");
    let postgui = body
        .iter()
        .position(|line| line.trim_start().starts_with("POSTGUI_HALFILE"))
        .expect("configs/sim-no3d.ini must keep its POSTGUI_HALFILE");
    assert!(
        linc_sim < reader && reader < postgui,
        "the reader loads where the simulator did, after linc-sim.hal and before \
         the post-GUI file: {body:#?}"
    );

    // No loaded file may be the simulator, in any HAL section.
    for line in &body {
        assert!(
            line.trim_start().starts_with('#') || !line.contains("linc-sim3d"),
            "the no-simulator rig must not load the simulator: {line:?}"
        );
    }

    // And the rest of the HAL wiring is untouched.
    for line in [
        "[HAL]",
        "HALFILE = ../hal/rio_sim.hal",
        "HALFILE = ../hal/linc.hal",
        "HALFILE = ../hal/linc-sim.hal",
        "POSTGUI_HALFILE = ../hal/linc-sim-postgui.hal",
        "HALUI = halui",
    ] {
        assert!(
            body.iter().any(|candidate| candidate.trim() == line),
            "configs/sim-no3d.ini must keep {line}"
        );
    }
}

#[test]
fn the_launcher_runs_without_the_simulator_by_default() {
    let script = read_repo_file("scripts/start-sim.sh");

    assert!(
        script.lines().any(|line| line.trim() == "SIM3D=0"),
        "the simulator is off until a flag asks for it"
    );
    assert!(
        script
            .lines()
            .any(|line| line.trim() == "--sim3d) SIM3D=1 ;;"),
        "--sim3d must turn the simulator on"
    );
    assert!(
        script
            .lines()
            .any(|line| line.trim() == "--no-sim3d) SIM3D=0 ;;"),
        "--no-sim3d stays accepted and still asks for no simulator"
    );
    assert!(
        script.contains("INI=\"$ROOT/configs/sim-no3d.ini\""),
        "the default rig must select configs/sim-no3d.ini"
    );
    assert!(
        script.contains("INI=\"$ROOT/configs/sim.ini\""),
        "--sim3d must select configs/sim.ini"
    );
    assert!(
        script.contains("[[ -z \"${LINC_SIM_INI:-}\" ]]"),
        "an explicit LINC_SIM_INI must still win over the flag's default"
    );
    assert!(
        script
            .lines()
            .any(|line| line.trim_start().starts_with('#') && line.contains("--sim3d")),
        "the flag belongs in the documented options at the top"
    );
}

#[test]
fn only_the_simulator_branch_builds_the_simulator() {
    let script = read_repo_file("scripts/start-sim.sh");

    // The normal path builds all three binaries in one cargo call.
    assert_eq!(
        script.matches(NORMAL_BUILD).count(),
        1,
        "the normal path keeps one cargo call that includes -p linc-sim3d"
    );
    let required = between(&script, "if (( NEED_SIM3D )); then", "\n    else");
    assert!(
        required.contains(NORMAL_BUILD),
        "the simulator build belongs in the NEED_SIM3D branch: {required:?}"
    );
    let without = between(&script, "\n    else", "\n    fi");
    assert!(
        !without.contains("linc-sim3d"),
        "the rig without the simulator must not build or name it: {without:?}"
    );

    // The requirement follows the flag, and the flag follows --sim3d.
    let guard = script
        .lines()
        .find(|line| line.contains("NEED_SIM3D") && line.contains("SIM3D_BIN"))
        .expect("the build guard must mention the simulator binary");
    assert!(
        guard.contains("$NEED_SIM3D -eq 1"),
        "the guard may only require the simulator binary when it is needed: {guard}"
    );
    assert!(
        script
            .lines()
            .any(|line| line.trim() == "NEED_SIM3D=$SIM3D"),
        "the build requirement must follow --sim3d"
    );
}

/// `net <signal> <pin>...`: a pin name always carries its component, so a word
/// without a dot is a signal name in a pin's place. That is what broke the rig
/// without the simulator: `net probe-input sig_rio_probe_bit => motion.probe-input`
/// made halcmd read `sig_rio_probe_bit` as a pin and LinuxCNC aborted the session.
#[test]
fn the_simulator_hal_files_net_pins_not_signal_names() {
    for file in ["hal/linc-sim3d.hal", "hal/linc-sim-no3d.hal"] {
        let text = read_repo_file(file);
        for line in text.lines() {
            let line = line.trim();
            let Some(net) = line.strip_prefix("net ") else {
                continue;
            };
            for word in net.split_whitespace().skip(1) {
                if word == "=>" || word == "<=" {
                    continue;
                }
                assert!(
                    word.contains('.'),
                    "{file}: `{word}` is not a pin name: {line}"
                );
            }
        }
    }
}

#[test]
fn the_no_simulator_rig_joins_the_simulated_probe_signal() {
    let hal = read_repo_file("hal/linc-sim-no3d.hal");
    let nets: Vec<&str> = hal
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("net "))
        .collect();
    assert_eq!(nets.len(), 1, "one net carries the probe path: {nets:?}");
    assert_eq!(
        nets[0], "net sig_rio_probe_bit => motion.probe-input",
        "the signal name comes from rio_sim.hal, so motion.probe-input joins it"
    );
    let rio = read_repo_file("hal/rio_sim.hal");
    assert!(
        rio.lines()
            .any(|line| line.trim_start().starts_with("net sig_rio_probe_bit")),
        "hal/rio_sim.hal must define the signal the rig joins"
    );
}
