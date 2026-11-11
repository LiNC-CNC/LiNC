//! PLCopen TC6 import, against four fixtures written by hand from the standard.

use linc_plc_core::model::{CoilKind, Logic, PouLanguage};
use linc_plc_core::{compile, plcopen, validate, Project, Severity, Vm};

const MOTOR: &str = include_str!("fixtures/motor.ld.xml");
const PARALLEL: &str = include_str!("fixtures/parallel.ld.xml");
const REFUSE: &str = include_str!("fixtures/refuse.ld.xml");
const SET_COIL: &str = include_str!("fixtures/set-coil.ld.xml");

fn imported(xml: &str) -> Project {
    match plcopen::import(xml) {
        Ok(project) => project,
        Err(diagnostics) => {
            panic!("import failed: {diagnostics:#?}");
        }
    }
}

#[test]
fn a_contact_and_a_coil_import_and_run() {
    let project = imported(MOTOR);
    assert_eq!(project.schema, 1, "the importer writes the model's schema");
    assert_eq!(project.name, "motor");

    let errors = validate(&project)
        .into_iter()
        .filter(|item| item.severity == Severity::Error)
        .collect::<Vec<_>>();
    assert!(errors.is_empty(), "{errors:#?}");

    let pous = project.all_pous();
    assert_eq!(pous.len(), 1);
    assert_eq!(pous[0].name, "MAIN", "the first program is the entry");
    assert_eq!(pous[0].language, PouLanguage::Ld);
    let rung = &pous[0].sections[0].rungs[0];
    assert_eq!(rung.coil.var, "lamp", "names are lowercased on import");
    assert_eq!(
        rung.logic,
        Logic::Contact {
            var: "motor".into(),
            negated: false
        }
    );
    assert!(rung.cells.is_empty(), "imports produce trees, not cells");

    let compiled = compile(&project).expect("the import must compile");
    assert_eq!(compiled.program.ops.len(), 2, "one contact and one store");

    let mut vm = Vm::new(&compiled);
    vm.scan(&[true]);
    assert_eq!(vm.outputs(), &[true], "the coil follows the contact");
    vm.scan(&[false]);
    assert_eq!(vm.outputs(), &[false]);
}

#[test]
fn a_parallel_pair_feeding_a_timer_keeps_its_shape() {
    let project = imported(PARALLEL);
    let errors = validate(&project)
        .into_iter()
        .filter(|item| item.severity == Severity::Error)
        .collect::<Vec<_>>();
    assert!(errors.is_empty(), "{errors:#?}");

    let rung = &project.all_pous()[0].sections[0].rungs[0];
    let Logic::Series { items } = &rung.logic else {
        panic!("expected a series, got {:?}", rung.logic);
    };
    assert_eq!(items.len(), 2, "the parallel pair and the block");
    let Logic::Parallel { items: branches } = &items[0] else {
        panic!("expected the parallel pair first, got {:?}", items[0]);
    };
    assert_eq!(branches.len(), 2);
    assert!(branches
        .iter()
        .all(|branch| matches!(branch, Logic::Contact { .. })));
    let Logic::Block {
        block, preset, ..
    } = &items[1]
    else {
        panic!("expected the timer second, got {:?}", items[1]);
    };
    assert_eq!(block, "TON");
    assert_eq!(preset, "T#2s");

    // And it runs: A or B feeds the timer.
    let compiled = compile(&project).expect("compile");
    let mut vm = Vm::new(&compiled);
    vm.scan_with_dt(&[false, true], 2100);
    assert_eq!(vm.outputs(), &[true], "B alone reached the timer");
}

#[test]
fn an_sfc_body_refuses_the_whole_file() {
    let diagnostics = plcopen::import(REFUSE).expect_err("an SFC body must be refused");
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    assert_eq!(diagnostics[0].code, "plcopen_unsupported_body");
}

#[test]
fn a_set_coil_maps_and_a_second_coil_trips_duplicate_coil() {
    let project = imported(SET_COIL);
    let rungs = &project.all_pous()[0].sections[0].rungs;
    assert_eq!(rungs.len(), 2);
    assert_eq!(rungs[0].coil.kind, CoilKind::Set);

    let codes = validate(&project)
        .into_iter()
        .map(|item| item.code)
        .collect::<Vec<_>>();
    assert!(
        codes.iter().any(|code| code == "duplicate_coil"),
        "{codes:?}"
    );
}

#[test]
fn a_connection_cycle_is_reported() {
    let xml = r#"<?xml version="1.0"?>
<project>
  <contentHeader name="loop"/>
  <types><pous><pou name="P" pouType="program">
    <interface><localVars><variable name="A"><type><BOOL/></type></variable></localVars></interface>
    <body><LD>
      <leftPowerRail localId="1"/>
      <rightPowerRail localId="2"/>
      <contact localId="3"><connectionPointIn><connection refLocalId="4"/></connectionPointIn><variable>A</variable></contact>
      <contact localId="4"><connectionPointIn><connection refLocalId="3"/></connectionPointIn><variable>A</variable></contact>
      <coil localId="5"><connectionPointIn><connection refLocalId="3"/></connectionPointIn><variable>A</variable></coil>
    </LD></body>
  </pou></pous></types>
</project>"#;
    let diagnostics = plcopen::import(xml).expect_err("a loop must be refused");
    assert!(
        diagnostics.iter().any(|item| item.code == "plcopen_cycle"),
        "{diagnostics:#?}"
    );
}

#[test]
fn a_coil_fed_by_another_coil_is_refused() {
    let xml = r#"<?xml version="1.0"?>
<project>
  <contentHeader name="twocoils"/>
  <types><pous><pou name="P" pouType="program">
    <interface><localVars>
      <variable name="A"><type><BOOL/></type></variable>
      <variable name="B"><type><BOOL/></type></variable>
    </localVars></interface>
    <body><LD>
      <leftPowerRail localId="1"/>
      <rightPowerRail localId="2"/>
      <coil localId="3"><connectionPointIn><connection refLocalId="1"/></connectionPointIn><variable>A</variable></coil>
      <coil localId="4"><connectionPointIn><connection refLocalId="3"/></connectionPointIn><variable>B</variable></coil>
    </LD></body>
  </pou></pous></types>
</project>"#;
    let diagnostics = plcopen::import(xml).expect_err("a coil cannot feed a coil");
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "plcopen_multi_coil"),
        "{diagnostics:#?}"
    );
}

#[test]
fn an_unknown_element_is_refused_by_name() {
    let xml = r#"<?xml version="1.0"?>
<project>
  <contentHeader name="jump"/>
  <types><pous><pou name="P" pouType="program">
    <interface><localVars><variable name="A"><type><BOOL/></type></variable></localVars></interface>
    <body><LD>
      <leftPowerRail localId="1"/>
      <rightPowerRail localId="2"/>
      <jump localId="3" label="L"/>
    </LD></body>
  </pou></pous></types>
</project>"#;
    let diagnostics = plcopen::import(xml).expect_err("a jump has no mapping");
    assert!(
        diagnostics
            .iter()
            .any(|item| item.code == "plcopen_unsupported_element"),
        "{diagnostics:#?}"
    );
}
