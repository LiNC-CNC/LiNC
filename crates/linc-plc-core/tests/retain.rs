//! Retain stores: the seven pack/store cases and the gates around them.

use linc_plc_core::retain::{self, Refusal, Retained};
use linc_plc_core::{
    compile, Coil, CoilKind, IoDirection, IoKind, IoPoint, Logic, Project, Rung, Section, VarDecl,
    Vm,
};

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
    assert!(packed
        .words
        .iter()
        .any(|(slot, value)| *slot == 3 && *value == 11));

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

/// The layout hash is what makes a store from another bank shape unusable. M3
/// moved the bank geometry into the profile; this pins that a moved slot is still
/// noticed, because `total` and `mw` are the only things the hash sees about the
/// bank and nothing else would catch a store that lined up by accident.
#[test]
fn the_layout_hash_notices_a_changed_bank_shape() {
    use linc_plc_core::profile::WordLayout;

    let names = [(0u16, "run")];
    let base = retain::layout_hash(&names, WordLayout::new(1, 0, 0, 0));
    assert_eq!(
        base,
        retain::layout_hash(&names, WordLayout::new(1, 0, 0, 0)),
        "the same shape hashes the same"
    );
    for moved in [
        WordLayout::new(2, 0, 0, 0),
        WordLayout::new(1, 1, 0, 0),
        WordLayout::new(1, 0, 1, 0),
        WordLayout::new(1, 0, 0, 1),
    ] {
        assert_ne!(
            base,
            retain::layout_hash(&names, moved),
            "a point in another region moves the bank: {moved:?}"
        );
    }
    assert_ne!(
        base,
        retain::layout_hash(&[], WordLayout::new(1, 0, 0, 0)),
        "a retained slot that disappears is a layout the store no longer fits"
    );
    assert_ne!(
        retain::layout_hash(&names, WordLayout::new(1, 0, 0, 0)),
        retain::layout_hash(&[(1u16, "run")], WordLayout::new(1, 0, 0, 0)),
        "the same name at another index is another layout"
    );
}

/// The bank is the profile's: the shipped dialect fills the shape the ABI
/// documents, and a second dialect is free to answer differently.
#[test]
fn the_profile_builds_the_bank() {
    use linc_plc_core::profile::{profile, Profile, WordLayout};

    assert_eq!(
        profile().layout(1, 2, 3, 4),
        WordLayout::new(1, 2, 3, 4),
        "the shipped dialect keeps the documented region order"
    );
    assert_eq!(profile().layout(1, 2, 3, 4).total(), 16 + 10);
}

#[test]
fn a_variable_that_is_not_retained_stays_out_of_the_store() {
    let compiled = compile(&project(false)).expect("compile");
    let vm = linc_plc_core::Vm::new(&compiled);
    let packed = vm.retain_pack();
    assert!(packed.bits.is_empty(), "nothing was declared retained");
    assert_eq!(packed.words.len(), 16, "the marker words are always kept");
}

/// The format gate. A 32-bit build writes FORMAT 2; an old reader's rule is
/// `version != 1`, so it answers `UnsupportedFormat(2)` and never lines
/// four-byte values up with two-byte slots. This reader has the mirror rule,
/// asserted here, and the version is written before the CRC so a stale blob is
/// refused for its format rather than its checksum.
#[test]
fn a_format_1_store_is_refused_by_the_32_bit_reader() {
    let mut bytes = retain::pack("hash", 1, &sample());
    assert_eq!(
        u16::from_le_bytes([bytes[4], bytes[5]]),
        2,
        "this build writes format 2, which a format-1 reader refuses as UnsupportedFormat(2)"
    );
    bytes[4] = 1;
    bytes[5] = 0;
    assert_eq!(
        retain::unpack(&bytes, "hash", 1).unwrap_err(),
        Refusal::UnsupportedFormat(1)
    );
}

fn word_io_project() -> Project {
    let mut project = Project::new("retain-io");
    let point = |name: &str, direction: IoDirection, kind: IoKind| IoPoint {
        name: name.to_string(),
        direction,
        kind,
        comment: String::new(),
    };
    project.io = vec![
        point("iw0", IoDirection::In, IoKind::Word),
        point("qw0", IoDirection::Out, IoKind::Word),
        point("if0", IoDirection::In, IoKind::Float),
        point("qf0", IoDirection::Out, IoKind::Float),
    ];
    let mut pou = linc_plc_core::Pou::new("MAIN", linc_plc_core::PouLanguage::Il);
    pou.il = vec!["LD 1".to_string()];
    project.pous = vec![pou];
    project
}

/// Retention keeps `MW0..MW15` only. Packing the I/O regions would restore a
/// stale field value the next scan overwrites and, worse, a `QW` value nobody
/// commanded.
#[test]
fn a_project_with_word_io_retains_only_the_marker_words() {
    let compiled = compile(&word_io_project()).expect("compile");
    assert_eq!(compiled.program.words.total(), 20, "16 + 4 I/O slots");
    let mut vm = Vm::new(&compiled);
    for slot in 16..20 {
        vm.set_word(slot, 999);
    }
    vm.set_word(3, 7);

    let packed = vm.retain_pack();
    assert_eq!(packed.words.len(), 16, "exactly the marker words");
    assert!(packed.words.iter().all(|(slot, _)| *slot < 16));
    assert!(packed
        .words
        .iter()
        .any(|(slot, value)| *slot == 3 && *value == 7));

    // A store that named an output slot cannot command it: the restore keeps
    // to the marker-word region.
    let crafted = Retained {
        bits: Vec::new(),
        words: vec![(17, 123)],
    };
    let mut other = Vm::new(&compiled);
    other.retain_unpack(&crafted);
    assert_eq!(other.word(17), 0, "a QW slot is not restorable");
}
