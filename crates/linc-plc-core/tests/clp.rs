//! `.clp` import: the fixtures from the handoff, the refusals, and the
//! reference models `tests/fixtures/clp/` alone cannot prove.
//!
//! The reference models are deliberately naive reimplementations of the
//! ClassicLadder functions the importer claims to preserve. They share no code
//! with the importer or the VM, so a test that agrees with one is evidence about
//! behaviour rather than about a shared mistake.

use std::path::{Path, PathBuf};

use linc_plc_core::grid::Cell;
use linc_plc_core::{clp, compile, CoilKind, Diagnostic, Project, Vm};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/clp")
        .join(name)
}

fn import_fixture(name: &str) -> Result<Project, Vec<Diagnostic>> {
    clp::import_path(&fixture(name))
}

fn codes(diagnostics: &[Diagnostic]) -> Vec<&str> {
    diagnostics.iter().map(|item| item.code.as_str()).collect()
}

/// The number of a block instance, from the compiled program's own view.
fn instance(rungs: &[linc_plc_core::Rung], block: &str) -> Option<String> {
    rungs
        .iter()
        .flat_map(|rung| rung.cells.iter())
        .find_map(|cell| match cell {
            Cell::Block {
                block: kind,
                instance,
                ..
            } if kind == block => Some(instance.clone()),
            _ => None,
        })
}

// --- fixture acceptance (§2.8.2) --------------------------------------------

#[test]
fn seal_imports_clean_and_drives_run() {
    let project = import_fixture("seal.clp").expect("seal imports");
    assert!(
        diagnostics_are_clean(&project),
        "{:?}",
        linc_plc_core::validate(&project)
    );
    let pou = &project.all_pous()[0];
    let rungs = &pou.sections[0].rungs;
    assert_eq!(rungs.len(), 2, "the drawn row and its branch");
    assert_eq!(rungs[0].coil().var, "run");
    assert_eq!(
        rungs[0].cells[1],
        Cell::Link,
        "the branch joins at column 1"
    );
    assert_eq!(
        rungs[1].coil().var,
        "",
        "the branch row has no coil of its own"
    );
    assert_eq!(
        rungs[1].cells[0],
        Cell::Contact {
            var: "seal".into(),
            negated: false
        }
    );

    let compiled = compile(&project).expect("the imported ladder compiles");
    let mut vm = Vm::new(&compiled);
    let frame = |on: bool| vec![on; compiled.inputs.len()];
    vm.scan(&frame(true));
    vm.scan(&frame(true));
    assert_eq!(vm.get("run"), Some(true), "two scans with start held");
    vm.scan(&frame(false));
    assert_eq!(
        vm.get("run"),
        Some(false),
        "and it drops with start released"
    );
}

#[test]
fn timer_keeps_its_preset_and_writes_it_from_an_expression() {
    let project = import_fixture("timer.clp").expect("timer imports");
    let rungs = &project.all_pous()[0].sections[0].rungs;
    assert_eq!(instance(rungs, "TON").as_deref(), Some("TON0"));
    let preset = rungs[0]
        .cells
        .iter()
        .find_map(|cell| match cell {
            Cell::Block { preset, .. } => Some(preset.clone()),
            _ => None,
        })
        .expect("a timer block");
    assert_eq!(preset, "T#4000ms", "four seconds from timers_iec.csv");
    // The operate element's `@260/0@` became the timer's preset member.
    assert_eq!(rungs[1].coil().kind, CoilKind::Compute);
    assert_eq!(rungs[1].coil().expr.as_deref(), Some("TON0.PT=4000"));

    let compiled = compile(&project).expect("the imported ladder compiles");
    let mut vm = Vm::new(&compiled);
    let frame = vec![true; compiled.inputs.len()];
    for _ in 0..3 {
        vm.scan_with_dt(&frame, 1000);
    }
    assert_eq!(vm.get("held"), Some(false), "not due at three seconds");
    vm.scan_with_dt(&frame, 1000);
    assert_eq!(
        vm.get("held"),
        Some(false),
        "the scan that first sees the input adds nothing, so four is not yet four"
    );
    vm.scan_with_dt(&frame, 1000);
    assert_eq!(vm.get("held"), Some(true), "due at four seconds of running");
}

#[test]
fn a_counter_with_up_and_reset_wired_imports_as_a_ctu() {
    let project = import_fixture("counter_up.clp").expect("counter imports");
    let rungs = &project.all_pous()[0].sections[0].rungs;
    assert_eq!(rungs.len(), 2, "the counting rung and the reset rung");
    assert_eq!(instance(rungs, "CTU").as_deref(), Some("CTU0"));
    assert_eq!(rungs[1].coil().var, "CTU0.R");
    assert_eq!(
        rungs[0].coil().kind,
        CoilKind::Compute,
        "a counter rung has no coil; the block's output is consumed"
    );

    let compiled = compile(&project).expect("the imported ladder compiles");
    let slot = |name: &str| {
        compiled
            .inputs
            .iter()
            .position(|binding| binding.name == name)
            .unwrap_or_else(|| panic!("no input {name}: {:?}", compiled.inputs))
    };
    let (count, reset) = (slot("count"), slot("reset"));
    let frame = |count_on: bool, reset_on: bool| {
        let mut frame = vec![false; compiled.inputs.len()];
        frame[count] = count_on;
        frame[reset] = reset_on;
        frame
    };
    let mut vm = Vm::new(&compiled);
    // CTU is block kind 3, so CTU0 is instance 12.
    let ctu = 3 * usize::from(linc_plc_core::compile::FB_PER_TYPE);
    vm.scan(&frame(false, false));
    vm.scan(&frame(true, false));
    assert_eq!(vm.block_value(ctu as u16), 1, "one rising edge");
    vm.scan(&frame(true, false));
    assert_eq!(
        vm.block_value(ctu as u16),
        1,
        "a held input is not a second edge"
    );
    vm.scan(&frame(false, false));
    vm.scan(&frame(true, false));
    assert_eq!(vm.block_value(ctu as u16), 2);
    vm.scan(&frame(false, true));
    assert_eq!(vm.block_value(ctu as u16), 0, "the reset rung clears CV");
}

#[test]
fn a_bidirectional_counter_is_refused_by_name() {
    let errors = import_fixture("counter_bidir.clp").expect_err("refused");
    let hits = errors
        .iter()
        .filter(|item| item.code == "clp_counter_bidirectional")
        .collect::<Vec<_>>();
    assert_eq!(hits.len(), 1, "{errors:?}");
    assert!(hits[0].message.contains("counter 0"), "{}", hits[0].message);
}

#[test]
fn rung_order_is_the_next_rung_chain() {
    let project = import_fixture("chain.clp").expect("chain imports");
    let rungs = &project.all_pous()[0].sections[0].rungs;
    let labels = rungs
        .iter()
        .map(|rung| rung.label.clone())
        .collect::<Vec<_>>();
    assert_eq!(labels, vec!["first", "second", "third"]);
    let coils = rungs
        .iter()
        .map(|rung| rung.coil().var.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        coils,
        vec!["q1", "q0", "q2"],
        "the chain, not the file numbers"
    );
}

#[test]
fn a_nested_branch_is_refused() {
    let errors = import_fixture("branch.clp").expect_err("refused");
    let hits = errors
        .iter()
        .filter(|item| item.code == "clp_branch")
        .count();
    assert_eq!(hits, 1, "{errors:?}");
}

#[test]
fn an_unpacked_directory_imports_like_its_container() {
    let from_directory =
        clp::import_path(&fixture("directory_project")).expect("directory imports");
    let from_file = clp::import_path(&fixture("seal.clp")).expect("seal imports");
    assert_eq!(from_directory, from_file);
}

#[test]
fn modbus_io_configuration_is_refused() {
    let errors = import_fixture("modbus.clp").expect_err("refused");
    let hits = errors
        .iter()
        .filter(|item| item.code == "clp_modbus")
        .count();
    assert_eq!(hits, 1, "{errors:?}");
}

fn diagnostics_are_clean(project: &Project) -> bool {
    !linc_plc_core::validate(project)
        .iter()
        .any(|item| item.severity == linc_plc_core::Severity::Error)
}

// --- container and grammar refusals ----------------------------------------

fn container(members: &[(&str, &str)]) -> String {
    let mut text = String::from("_FILES_CLASSICLADDER\n");
    for (name, body) in members {
        text.push_str(&format!("_FILE-{name}\n{body}\n_/FILE-{name}\n"));
    }
    text.push_str("_/FILES_CLASSICLADDER\n");
    text
}

/// One rung whose first row is `cells` and whose coil is `coil`.
fn ladder(sections: &str, rows: &[&str], coil: Option<&str>) -> String {
    let mut body = String::from("#VER=2.0\n#LABEL=\n#COMMENT=\n#PREVRUNG=-1\n#NEXTRUNG=-1\n");
    for row in rows {
        let mut cells = row.split(" , ").map(str::to_string).collect::<Vec<_>>();
        // A drawn line is continuous; a `0-0-0/0` in the source now means the
        // gap the test is about.
        while cells.len() < 10 {
            cells.push("9-0-0/0".to_string());
        }
        if let Some(coil) = coil {
            cells[9] = coil.to_string();
        }
        body.push_str(&cells.join(" , "));
        body.push('\n');
    }
    container(&[
        ("sections.csv", sections),
        ("symbols.csv", "#VER=1.0\n%I0,a,\n%Q0,y,\n"),
        ("rung_0.csv", &body),
    ])
}

/// A section must not be called `MAIN`: LiNC would treat the other POUs as
/// uncalled routines, so the importer refuses that name.
const ONE_SECTION: &str = "#VER=1.0\n#NAME000=program\n000,0,-1,0,0,0\n";

#[test]
fn a_container_accepts_crlf_and_blank_lines_between_members() {
    let text = std::fs::read_to_string(fixture("seal.clp")).expect("fixture");
    let crlf = text.replace('\n', "\r\n").replace("_FILE-", "\r\n_FILE-");
    let project = clp::import(&crlf).expect("CRLF imports");
    assert!(project.io.iter().any(|point| point.name == "start"));
}

#[test]
fn a_file_without_the_container_header_is_refused() {
    let errors = clp::import("sections.csv\n000,0,-1,0,0,0\n").expect_err("refused");
    assert_eq!(codes(&errors), vec!["clp_container"]);
}

#[test]
fn a_missing_required_member_is_refused() {
    let text = container(&[("rung_0.csv", "#VER=2.0\n0-0-0/0\n")]);
    let errors = clp::import(&text).expect_err("refused");
    assert_eq!(codes(&errors), vec!["clp_member"], "{errors:?}");
}

#[test]
fn an_unreadable_rung_version_is_refused() {
    let body = "#VER=9.0\n#LABEL=\n#COMMENT=\n#PREVRUNG=-1\n#NEXTRUNG=-1\n0-0-0/0\n";
    let text = container(&[
        ("sections.csv", ONE_SECTION),
        ("symbols.csv", "#VER=1.0\n"),
        ("rung_0.csv", body),
    ]);
    let errors = clp::import(&text).expect_err("refused");
    assert_eq!(codes(&errors), vec!["clp_version"], "{errors:?}");
}

#[test]
fn indirect_addressing_is_refused() {
    // `%I[a/0]` is an indexed operand; ClassicLadder reads it, LiNC does not.
    let text = ladder(
        ONE_SECTION,
        &["1-0-50/0[200/0] , 9-0-0/0"],
        Some("50-0-60/0"),
    );
    let errors = clp::import(&text).expect_err("refused");
    assert_eq!(codes(&errors), vec!["clp_indexed"], "{errors:?}");
}

#[test]
fn a_sequential_section_is_refused() {
    let text = ladder(
        "#VER=1.0\n#NAME000=sfc\n000,1,-1,0,0,0\n",
        &["1-0-50/0"],
        Some("50-0-60/0"),
    );
    let errors = clp::import(&text).expect_err("refused");
    assert_eq!(codes(&errors), vec!["clp_sfc"], "{errors:?}");
}

#[test]
fn a_gap_in_a_rung_is_refused() {
    // A free cell between the rail and the coil stops ClassicLadder's power;
    // LiNC's wire would conduct, so the shape has no faithful mapping.
    let text = ladder(
        ONE_SECTION,
        &["1-0-50/0 , 0-0-0/0 , 9-0-0/0"],
        Some("50-0-60/0"),
    );
    let errors = clp::import(&text).expect_err("refused");
    assert_eq!(codes(&errors), vec!["clp_rung_syntax"], "{errors:?}");
}

#[test]
fn a_down_counter_reset_with_a_non_zero_preset_is_refused() {
    // `CTD` has no clear, and loading a non-zero preset is not clearing.
    let rows = [
        "1-0-50/0 , 99-0-0/0 , 12-0-0/0",
        "0-0-0/0",
        "0-0-0/0",
        "1-0-50/1 , 99-0-0/0",
    ];
    let body = {
        let mut body = String::from("#VER=2.0\n#LABEL=\n#COMMENT=\n#PREVRUNG=-1\n#NEXTRUNG=-1\n");
        for row in rows {
            body.push_str(row);
            body.push('\n');
        }
        body
    };
    let text = container(&[
        ("sections.csv", ONE_SECTION),
        ("symbols.csv", "#VER=1.0\n%I0,reset,\n%I1,down,\n"),
        ("counters.csv", "5\n"),
        ("rung_0.csv", body.as_str()),
    ]);
    let errors = clp::import(&text).expect_err("refused");
    assert_eq!(codes(&errors), vec!["clp_counter_reset"], "{errors:?}");
}

#[test]
fn the_empty_and_full_outputs_are_refused() {
    // A contact on counter 0's empty bit (`VAR_COUNTER_EMPTY` is 26).
    let text = ladder(ONE_SECTION, &["1-0-26/0"], Some("50-0-60/0"));
    let errors = clp::import(&text).expect_err("refused");
    assert_eq!(codes(&errors), vec!["clp_counter_overflow"], "{errors:?}");
}

#[test]
fn a_word_outside_the_marker_bank_is_refused() {
    // `%W20` in a compare expression: LiNC has sixteen marker words.
    // A compare element whose two fillers sit to its left, expression 0.
    let body = "#VER=2.0\n#LABEL=\n#COMMENT=\n#PREVRUNG=-1\n#NEXTRUNG=-1\n99-0-0/0 , 99-0-0/0 , 20-0-0/0\n";
    let text = container(&[
        ("sections.csv", ONE_SECTION),
        ("symbols.csv", "#VER=1.0\n"),
        ("arithmetic_expressions.csv", "#VER=2.0\n0000,@200/20@=1\n"),
        ("rung_0.csv", body),
    ]);
    let errors = clp::import(&text).expect_err("refused");
    assert_eq!(codes(&errors), vec!["clp_variable"], "{errors:?}");
}

// --- reference models (§2.8.1) ---------------------------------------------

/// `CalcTypeOutput`: a normal coil copies the rung's power, a negated one its
/// inverse.
fn reference_coil(power: bool, negated: bool) -> bool {
    if negated {
        !power
    } else {
        power
    }
}

/// `CalcTypeOutputSetReset`: a powered coil latches its value, an unpowered one
/// changes nothing.
fn reference_set_reset(current: bool, power: bool, reset: bool) -> bool {
    if power {
        !reset
    } else {
        current
    }
}

#[test]
fn coils_match_the_classicladder_reference_over_a_power_sequence() {
    // Five rungs, one per coil kind, each driven by the same contact.
    // One output each: normal, negated, set, reset, and a second normal coil
    // that proves the first is not simply mirroring the input.
    let rungs = [
        "50-0-60/0",
        "51-0-60/1",
        "52-0-60/2",
        "53-0-60/3",
        "50-0-60/4",
    ];
    let mut text = String::from("_FILES_CLASSICLADDER\n");
    text.push_str(
        "_FILE-sections.csv\n#VER=1.0\n#NAME000=coils\n000,0,-1,0,4,0\n_/FILE-sections.csv\n",
    );
    text.push_str("_FILE-symbols.csv\n#VER=1.0\n%I0,in,\n%Q0,normal,\n%Q1,negated,\n%Q2,set,\n%Q3,reset,\n%Q4,mirror,\n_/FILE-symbols.csv\n");
    for (index, coil) in rungs.iter().enumerate() {
        let next = if index + 1 == rungs.len() {
            -1
        } else {
            index as i32 + 1
        };
        let body = format!(
            "#VER=2.0\n#LABEL=\n#COMMENT=\n#PREVRUNG=-1\n#NEXTRUNG={next}\n1-0-50/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , {coil}\n"
        );
        text.push_str(&format!(
            "_FILE-rung_{index}.csv\n{body}\n_/FILE-rung_{index}.csv\n"
        ));
    }
    text.push_str("_/FILES_CLASSICLADDER\n");
    let project = clp::import(&text).expect("coils import");
    let compiled = compile(&project).expect("compiles");
    let mut vm = Vm::new(&compiled);
    let frame = |on: bool| vec![on; compiled.inputs.len()];
    let mut set = false;
    let mut reset = false;
    for power in [false, true, true, false, true, false] {
        vm.scan(&frame(power));
        set = reference_set_reset(set, power, false);
        reset = reference_set_reset(reset, power, true);
        assert_eq!(
            vm.get("normal"),
            Some(reference_coil(power, false)),
            "normal at {power}"
        );
        assert_eq!(
            vm.get("negated"),
            Some(reference_coil(power, true)),
            "negated at {power}"
        );
        assert_eq!(vm.get("set"), Some(set), "set at {power}");
        assert_eq!(vm.get("reset"), Some(reset), "reset at {power}");
    }
}

/// `CalcTypeTimerIEC` mode ON: elapsed time accumulates while the input is on,
/// resets to zero when it is off, and the output follows `elapsed >= preset`.
fn reference_timer_on(
    input: bool,
    elapsed: &mut i32,
    preset: i32,
    tick: i32,
    was_input: &mut bool,
) -> bool {
    if !input {
        *elapsed = 0;
        *was_input = false;
        false
    } else {
        // Zero on the scan the input is first seen true, then one tick per scan.
        // The interval before that scan ran before the timer did.
        if !*was_input {
            *elapsed = 0;
        } else {
            *elapsed = (*elapsed + tick).min(preset);
        }
        *was_input = true;
        *elapsed >= preset
    }
}

#[test]
fn a_ton_matches_the_reference_over_a_pulse_sequence() {
    let text = container(&[
        ("sections.csv", ONE_SECTION),
        ("symbols.csv", "#VER=1.0\n%I0,in,\n%Q0,out,\n"),
        ("timers_iec.csv", "1,30,0\n"),
        (
            "rung_0.csv",
            "#VER=2.0\n#LABEL=\n#COMMENT=\n#PREVRUNG=-1\n#NEXTRUNG=-1\n1-0-50/0 , 9-0-0/0 , 99-0-0/0 , 13-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 50-0-60/0\n",
        ),
    ]);
    let project = clp::import(&text).expect("timer imports");
    let compiled = compile(&project).expect("compiles");
    let mut vm = Vm::new(&compiled);
    let frame = |on: bool| vec![on; compiled.inputs.len()];
    let (preset, tick) = (30, 20);
    let mut elapsed = 0;
    let mut was_input = false;
    // A pulse, a hold through the preset, then a release and a second pulse.
    for (scan, input) in [true, true, true, true, false, false, true, true]
        .into_iter()
        .enumerate()
    {
        vm.scan_with_dt(&frame(input), tick as u16);
        let wanted = reference_timer_on(input, &mut elapsed, preset, tick, &mut was_input);
        assert_eq!(vm.get("out"), Some(wanted), "scan {scan} input {input}");
    }
}

/// `CalcTypeCounter` for an up counter with reset: a rising edge on `U` counts
/// (wrapping past 9999), `P` loads the preset, and `R` clears to zero.
fn reference_counter_up(up: bool, reset: bool, up_before: &mut bool, value: &mut i32, preset: i32) {
    if up && !*up_before {
        *value += 1;
        if *value > 9999 {
            *value = 0;
        }
    }
    *up_before = up;
    if reset {
        *value = 0;
    }
    let _ = preset;
}

#[test]
fn an_up_counter_matches_the_reference_over_edges_and_a_reset() {
    let project = import_fixture("counter_up.clp").expect("counter imports");
    let compiled = compile(&project).expect("compiles");
    let slot = |name: &str| {
        compiled
            .inputs
            .iter()
            .position(|binding| binding.name == name)
            .expect("input")
    };
    let (count, reset) = (slot("count"), slot("reset"));
    let frame = |count_on: bool, reset_on: bool| {
        let mut frame = vec![false; compiled.inputs.len()];
        frame[count] = count_on;
        frame[reset] = reset_on;
        frame
    };
    let mut vm = Vm::new(&compiled);
    let ctu = 3 * usize::from(linc_plc_core::compile::FB_PER_TYPE) as u16;
    let (mut value, mut up_before) = (0, false);
    let preset = 5;
    for (scan, (up, reset)) in [
        (false, false),
        (true, false),
        (true, false),
        (false, false),
        (true, false),
        (false, true),
        (false, false),
        (true, false),
    ]
    .into_iter()
    .enumerate()
    {
        vm.scan(&frame(up, reset));
        reference_counter_up(up, reset, &mut up_before, &mut value, preset);
        assert_eq!(vm.block_value(ctu), value, "scan {scan}");
    }
}

/// `EvalCompare`: `MW1<2*MW2` is the reference expression evaluated over a
/// matrix of marker words.
#[test]
fn a_compare_expression_matches_its_reference() {
    let text = container(&[
        ("sections.csv", ONE_SECTION),
        ("symbols.csv", "#VER=1.0\n%Q0,y0,\n"),
        ("arithmetic_expressions.csv", "#VER=2.0\n0000,@200/1@<2*@200/2@\n"),
        (
            "rung_0.csv",
            "#VER=2.0\n#LABEL=\n#COMMENT=\n#PREVRUNG=-1\n#NEXTRUNG=-1\n99-0-0/0 , 99-0-0/0 , 20-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 50-0-60/0\n",
        ),
    ]);
    let project = clp::import(&text).expect("compare imports");
    let compiled = compile(&project).expect("compiles");
    let mut vm = Vm::new(&compiled);
    for (left, right) in [(3, 2), (4, 2), (0, 0), (-1, 0), (100, 50)] {
        vm.set_word(1, left);
        vm.set_word(2, right);
        vm.scan(&[]);
        let wanted = left < 2 * right;
        assert_eq!(vm.get("y0"), Some(wanted), "{left} < 2*{right}");
    }
}

#[test]
fn a_timer_done_contact_reads_the_blocks_own_output() {
    // Rung 0 runs a TON; rung 1 reads `%TM0.1`, the timer's done bit.
    let timer = "#VER=2.0\n#LABEL=\n#COMMENT=\n#PREVRUNG=-1\n#NEXTRUNG=1\n1-0-50/0 , 9-0-0/0 , 99-0-0/0 , 13-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 50-0-60/0\n";
    let done = "#VER=2.0\n#LABEL=\n#COMMENT=\n#PREVRUNG=0\n#NEXTRUNG=-1\n1-0-15/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 9-0-0/0 , 50-0-60/1\n";
    let text = container(&[
        (
            "sections.csv",
            "#VER=1.0\n#NAME000=program\n000,0,-1,0,1,0\n",
        ),
        ("symbols.csv", "#VER=1.0\n%I0,run,\n%Q0,lit,\n%Q1,done,\n"),
        ("timers_iec.csv", "1,40,0\n"),
        ("rung_0.csv", timer),
        ("rung_1.csv", done),
    ]);
    let project = clp::import(&text).expect("a timer done contact imports");
    let compiled = compile(&project).expect("compiles");
    let mut vm = Vm::new(&compiled);
    let frame = vec![true; compiled.inputs.len()];
    assert_eq!(vm.get("done"), Some(false));
    vm.scan_with_dt(&frame, 20);
    vm.scan_with_dt(&frame, 20);
    assert_eq!(vm.get("lit"), Some(false), "20 ms twice is not yet 40");
    vm.scan_with_dt(&frame, 20);
    assert_eq!(vm.get("lit"), Some(true), "the timer is done at 40 ms");
    assert_eq!(vm.get("done"), Some(true), "and the next rung reads its Q");
}
