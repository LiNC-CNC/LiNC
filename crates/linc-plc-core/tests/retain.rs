//! Retain stores: the seven pack/store cases and the gates around them.

use linc_plc_core::retain::{self, Refusal, Retained};
use linc_plc_core::{compile, Coil, CoilKind, IoDirection, IoPoint, Logic, Project, Rung, Section, VarDecl};

fn project(retain: bool) -> Project {
    let mut project = Project::new("retain-test");
    project.io = vec![IoPoint::new("y", IoDirection::Out)];
    let mut var = VarDecl::new("count");
    var.retain = retain;
    project.vars = vec![var];
    project.sections = vec![Section {
        name: "main".into(),
        rungs: vec![Rung::new(
            Logic::Const { value: true },
            Coil::new("y", CoilKind::Normal),
        )],
    }];
    project
}

fn sample() -> Retained {
    Retained {
        bits: vec![(3, true), (7, false)],
        words: vec![(0, 42), (15, -7)],
    }
}

#[test]
fn a_store_round_trips() {
    let bytes = retain::pack("0123456789abcdef", 99, &sample());
    assert_eq!(bytes[0..4], retain::MAGIC);
    let back = retain::unpack(&bytes, "0123456789abcdef", 99).expect("round trip");
    assert_eq!(back, sample());
}

#[test]
fn a_foreign_magic_is_refused() {
    let mut bytes = retain::pack("hash", 1, &sample());
    bytes[0] = b'X';
    assert_eq!(
        retain::unpack(&bytes, "hash", 1).unwrap_err(),
        Refusal::BadMagic
    );
}

#[test]
fn another_format_is_refused_with_its_version() {
    let mut bytes = retain::pack("hash", 1, &sample());
    bytes[4] = 9;
    assert_eq!(
        retain::unpack(&bytes, "hash", 1).unwrap_err(),
        Refusal::UnsupportedFormat(9)
    );
}

#[test]
fn a_torn_store_fails_the_crc() {
    let mut bytes = retain::pack("hash", 1, &sample());
    let last = bytes.len() - 5;
    bytes[last] ^= 0x40;
    assert_eq!(
        retain::unpack(&bytes, "hash", 1).unwrap_err(),
        Refusal::CrcMismatch
    );
}

#[test]
fn a_short_store_is_truncated() {
    let bytes = retain::pack("hash", 1, &sample());
    assert_eq!(
        retain::unpack(&bytes[..6], "hash", 1).unwrap_err(),
        Refusal::Truncated
    );
}

#[test]
fn a_changed_layout_is_refused() {
    let bytes = retain::pack("hash", 1, &sample());
    assert_eq!(
        retain::unpack(&bytes, "hash", 2).unwrap_err(),
        Refusal::LayoutMismatch
    );
}

#[test]
fn a_store_from_another_program_is_refused() {
    let bytes = retain::pack("aaaa", 1, &sample());
    assert_eq!(
        retain::unpack(&bytes, "bbbb", 1).unwrap_err(),
        Refusal::WrongProgram
    );
}

#[test]
fn an_oversized_store_is_refused_before_parsing() {
    let bytes = vec![0u8; retain::MAX_BLOB + 1];
    assert_eq!(
        retain::unpack(&bytes, "hash", 1).unwrap_err(),
        Refusal::TooLarge(retain::MAX_BLOB + 1)
    );
}

#[test]
fn the_vm_packs_restored_values_and_never_a_force() {
    let compiled = compile(&project(true)).expect("compile");
    let mut vm = linc_plc_core::Vm::new(&compiled);
    vm.set_word(3, 11);
    vm.set_force("count", true).expect("force");
    // The force reaches the scan image on a scan, and the store keeps values.
    vm.scan(&[]);
    let packed = vm.retain_pack();
    assert!(
        packed.bits.iter().any(|(_, value)| *value),
        "the forced retained bit is packed as a value"
    );
    assert!(packed.words.iter().any(|(slot, value)| *slot == 3 && *value == 11));

    let mut other = linc_plc_core::Vm::new(&compiled);
    other.retain_unpack(&packed);
    assert_eq!(other.word(3), 11);
    assert!(
        other.forced().is_empty(),
        "a restore must not carry a force across"
    );
    assert_eq!(
        other.retain_layout_hash(),
        vm.retain_layout_hash(),
        "the same program has the same layout"
    );
}

#[test]
fn a_variable_that_is_not_retained_stays_out_of_the_store() {
    let compiled = compile(&project(false)).expect("compile");
    let vm = linc_plc_core::Vm::new(&compiled);
    let packed = vm.retain_pack();
    assert!(packed.bits.is_empty(), "nothing was declared retained");
    assert!(!packed.words.is_empty(), "the word bank is always kept");
}
