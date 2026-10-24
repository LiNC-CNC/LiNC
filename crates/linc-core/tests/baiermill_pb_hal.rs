//! Deployment guard for LiNC's BaierMill HAL entry (review finding PR-01).
//!
//! `configs/baiermill-pb.ini` used to load the machine's own
//! `hallib/szgh_panel.hal`, which runs `loadusr ./szgh-panel-hal`: a binary that
//! is not in the machine tree, so a HAL start could not succeed. LiNC now loads
//! its own `hal/baiermill-pb-szgh-panel.hal`, which starts the machine's
//! existing python driver. These tests read the shipped files, so pointing the
//! INI back at a HAL entry whose `loadusr` program does not exist fails here
//! instead of on the machine.

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

fn repo(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative)
}

fn configs_dir() -> PathBuf {
    repo("configs")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("reading {}: {error}", path.display()))
}

/// Values of `key` inside the INI's `[section]`, in file order.
fn ini_values(text: &str, section: &str, key: &str) -> Vec<String> {
    let mut current = String::new();
    let mut values = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if let Some(name) = trimmed
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            current = name.trim().to_ascii_uppercase();
            continue;
        }
        if current != section.to_ascii_uppercase() {
            continue;
        }
        let Some((name, value)) = trimmed.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case(key) {
            values.push(value.trim().to_string());
        }
    }
    values
}

fn resolve(base: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn is_executable(path: &Path) -> bool {
    fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

fn on_path(program: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join(program))
            .find(|candidate| is_executable(candidate))
    })
}

/// The program a `loadusr` line starts, skipping halcmd's own flags.
fn loadusr_program(line: &str) -> Option<String> {
    let mut tokens = line.split_whitespace();
    if tokens.next()? != "loadusr" {
        return None;
    }
    let mut skip_argument = false;
    for token in tokens {
        if skip_argument {
            skip_argument = false;
            continue;
        }
        if token == "-n" {
            skip_argument = true;
            continue;
        }
        if token.starts_with('-') {
            continue;
        }
        return Some(token.to_string());
    }
    None
}

fn hal_entries(key: &str) -> Vec<String> {
    let ini = read(&configs_dir().join("baiermill-pb.ini"));
    ini_values(&ini, "HAL", key)
}

#[test]
fn every_hal_file_the_profile_loads_exists() {
    let base = configs_dir();
    let entries: Vec<String> = hal_entries("HALFILE")
        .into_iter()
        .chain(hal_entries("POSTGUI_HALFILE"))
        .collect();
    assert!(
        entries.len() >= 4,
        "configs/baiermill-pb.ini must keep the machine's HAL composition, found {entries:?}"
    );
    for entry in entries {
        let path = resolve(&base, &entry);
        assert!(
            path.is_file(),
            "configs/baiermill-pb.ini loads {entry}, which does not exist at {}",
            path.display()
        );
    }
}

#[test]
fn the_panel_helper_loadusr_resolves_to_a_linc_owned_executable() {
    let base = configs_dir();
    let hal_files = hal_entries("HALFILE");
    let panel_entries: Vec<&String> = hal_files
        .iter()
        .filter(|entry| entry.contains("szgh"))
        .collect();
    assert_eq!(
        panel_entries.len(),
        1,
        "the INI must load exactly one SZGH panel HAL file, found {hal_files:?}"
    );
    let entry = panel_entries[0];
    assert_eq!(
        entry, "../hal/baiermill-pb-szgh-panel.hal",
        "the panel HAL must be LiNC's copy; the machine's own file starts a missing binary"
    );

    let hal_path = resolve(&base, entry);
    let hal = read(&hal_path);
    let loadusr: Vec<String> = hal
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with('#'))
        .filter_map(loadusr_program)
        .collect();
    assert_eq!(
        loadusr.len(),
        1,
        "{} must start exactly one helper, found {loadusr:?}",
        hal_path.display()
    );

    // The machine's compiled helper is absent; the python driver is not.
    let program = &loadusr[0];
    assert!(
        program.ends_with("szgh_panel_hal.py"),
        "the helper must be the machine's python driver, found {program}"
    );
    let helper = resolve(&base, program);
    assert!(
        is_executable(&helper),
        "{} must exist and be executable (loadusr executes it directly)",
        helper.display()
    );

    // Its shebang must resolve, or loadusr cannot run it.
    let shebang = read(&helper)
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("#!"))
        .map(str::trim)
        .map(str::to_string)
        .expect("the helper must carry a shebang");
    let mut parts = shebang.split_whitespace();
    let interpreter = parts.next().unwrap_or_default();
    let resolved = if interpreter.ends_with("/env") {
        on_path(parts.next().unwrap_or_default())
    } else {
        Some(PathBuf::from(interpreter))
    };
    assert!(
        resolved.as_deref().is_some_and(is_executable),
        "the helper's shebang {shebang:?} does not resolve to an executable"
    );
}

#[test]
fn postgui_hal_is_linc_owned_and_free_of_probe_basic_gui_pins() {
    let base = configs_dir();
    let entries = hal_entries("POSTGUI_HALFILE");
    assert_eq!(
        entries.len(),
        1,
        "the INI must load exactly one post-GUI file, found {entries:?}"
    );
    assert_eq!(
        entries[0], "../hal/baiermill-pb-postgui.hal",
        "the post-GUI HAL must be LiNC's copy of the machine's file"
    );
    let path = resolve(&base, &entries[0]);
    let hal = read(&path);

    // Probe Basic's GUI pins do not exist under LiNC; a single one left behind
    // fails a real HAL start.
    assert!(
        !hal.contains("qtpyvcp"),
        "{} must not reference Probe Basic GUI pins",
        path.display()
    );

    // POSTGUI_HALFILE is loaded before the HMI creates linc.*, and this profile
    // starts no linc-panel, so it must not wire either pin family.
    let code: Vec<&str> = hal
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect();
    for line in &code {
        assert!(
            !line.contains("linc.") && !line.contains("cnc_panel."),
            "{} loads before linc.*/cnc_panel.* exist: {line}",
            path.display()
        );
    }

    // The machine nets must survive: one named line per function of the machine.
    for required in [
        // spindle inhibit and at speed, including the override term
        "net sig_rio_ls-b_bit => spindle.0.inhibit",
        "net spindle-at-speed near.spindle-speed.out => spindle.0.at-speed",
        "net spindle-speed-cmd-with-override mult2.spindle-speed.out => near.spindle-speed.in1",
        // ATC digital in/out and the tool-change piston
        "net sig_rio_ls-a_bit => motion.digital-in-05",
        "net sig_rio_ls-b_bit => motion.digital-in-02",
        "net auto-tool-change-out <= motion.digital-out-02 => or2.tool-change-piston.in1",
        "net tool-change-piston-out <= or2.tool-change-piston.out => rio.tool-change.bit",
        // coolant, worklamp, lube
        "net coolant-toggle toggle.coolant.out => halui.flood.on",
        "net coolant-off and2.coolant-off.out => halui.flood.off",
        "net worklamp-on and2.worklamp.out => rio.worklamp.bit",
        "net lube-toggle toggle.lube.out => rio.oil-lube.bit",
        // home buttons
        "net home-all-gated and2.home-all.out => halui.home-all",
        "net home-x-gated and2.home-x.out => halui.joint.0.home",
        "net home-y-gated and2.home-y.out => halui.joint.1.home",
        "net home-z-gated and2.home-z.out => halui.joint.2.home",
        // mode switches (the only surviving index: it gates jog-enable)
        "net mode-mpg szgh_panel.mode_mpg",
        // program control from the panel and the pendant
        "net pendant-run-cmd or2.pendant-run.out => halui.program.run",
        "net pendant-pause-cmd or2.pendant-pause.out => halui.program.pause",
        "net pendant-stop-cmd or2.pendant-stop.out => halui.program.stop",
        // probe input and tool-99 spindle gating
        "net sig_rio_probe_bit => motion.probe-input",
        "net gated-spindle-enable => rio.spindle.enable",
        "net gated-spindle-forward => rio.spindle-forward.bit",
        "net gated-spindle-rev => rio.spindle-reverse.bit",
    ] {
        assert!(
            code.iter().any(|line| *line == required),
            "{} is missing the machine net {required:?}",
            path.display()
        );
    }

    // HAL pins take exactly one signal: no pin may appear on two net lines, and
    // a net needs at least one pin.
    let mut seen: Vec<&str> = Vec::new();
    for line in &code {
        let Some(rest) = line.strip_prefix("net ") else {
            continue;
        };
        let mut tokens = rest.split_whitespace();
        let signal = tokens.next().expect("net needs a signal name");
        let pins: Vec<&str> = tokens
            .filter(|token| !matches!(*token, "=>" | "<=" | ">" | "<"))
            .collect();
        assert!(!pins.is_empty(), "net {signal} has no pin: {line}");
        for pin in pins {
            assert!(
                !seen.contains(&pin),
                "pin {pin} is attached to more than one signal ({line})"
            );
            seen.push(pin);
        }
    }
}

#[test]
fn the_machine_postgui_hal_file_is_not_loaded() {
    let entries = hal_entries("POSTGUI_HALFILE");
    assert!(
        !entries
            .iter()
            .any(|entry| entry.ends_with("probe_basic_postgui.hal")),
        "the INI must load LiNC's post-GUI file, not the machine's: {entries:?}"
    );
}

#[test]
fn the_machine_panel_hal_file_is_not_loaded() {
    let entries: Vec<String> = hal_entries("HALFILE")
        .into_iter()
        .chain(hal_entries("POSTGUI_HALFILE"))
        .collect();
    assert!(
        !entries
            .iter()
            .any(|entry| entry.ends_with("hallib/szgh_panel.hal")),
        "configs/baiermill-pb.ini must not load the machine's szgh_panel.hal,          whose loadusr program is missing: {entries:?}"
    );
}
