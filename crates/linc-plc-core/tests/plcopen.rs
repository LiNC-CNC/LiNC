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
    assert_eq!(rung.coil().var, "lamp", "names are lowercased on import");
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
    let Logic::Block { block, preset, .. } = &items[1] else {
        panic!("expected the timer second, got {:?}", items[1]);
    };
    assert_eq!(block, "TON");
    assert_eq!(preset, "T#2s");

    // And it runs: A or B feeds the timer.
    let compiled = compile(&project).expect("compile");
    let mut vm = Vm::new(&compiled);
    // The first scan starts the timer; the second brings 2.1 s, past the preset.
    vm.scan_with_dt(&[false, true], 2100);
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
    assert_eq!(rungs[0].coil().kind, CoilKind::Set);

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

/// Every type this build reads, in one interface: the declaration survives the
/// import, arrays keep their length, a derived type keeps its name, and a type with
/// no width is still refused by name.
/// A struct type and an array of it: the fields import with their types, the array's
/// starting values are read element-major, and a field of one element stays an operand
/// rather than being flattened into a name.
#[test]
fn a_struct_type_and_an_array_of_it_import() {
    let xml = r#"<?xml version="1.0"?>
<project>
  <contentHeader name="structs"/>
  <types>
    <dataTypes>
      <dataType name="Recipe"><struct>
        <variable name="FillTime"><type><DINT/></type></variable>
        <variable name="MixTime"><type><DINT/></type></variable>
      </struct></dataType>
    </dataTypes>
    <pous><pou name="MAIN" pouType="program">
    <interface><localVars>
      <variable name="Recipes"><type><array><baseElementType><derived name="Recipe"/></baseElementType><dimension lower="0" upper="1"/></array></type><initialValue><arrayValue><value>1000</value><value>2000</value><value>3000</value><value>4000</value></arrayValue></initialValue></variable>
      <variable name="Sel"><type><DINT/></type></variable>
    </localVars></interface>
    <body><LD>
      <leftPowerRail localId="0"/>
      <rightPowerRail localId="99"/>
      <block localId="1" typeName="TON"><connectionPointIn><connection refLocalId="0"/></connectionPointIn><inputVariables><variable formalParameter="PT"><expression>Recipes[Sel].FillTime</expression></variable></inputVariables></block>
    </LD></body>
  </pou></pous>
  </types>
</project>"#;
    let project = imported(xml);
    assert_eq!(project.structs.len(), 1);
    assert_eq!(project.structs[0].name, "Recipe");
    let fields: Vec<String> = project.structs[0]
        .fields
        .iter()
        .map(|field| field.ty.label())
        .collect();
    assert_eq!(fields, vec!["DINT".to_string(), "DINT".to_string()]);
    assert_eq!(
        project
            .vars
            .iter()
            .find(|var| var.name == "recipes")
            .map(|var| var.ty.label()),
        Some("Recipe[2]".to_string())
    );
    assert_eq!(
        project
            .vars
            .iter()
            .find(|var| var.name == "recipes")
            .map(|var| var.init.clone()),
        Some(vec![1000, 2000, 3000, 4000]),
        "an array of structs is element-major"
    );
    // The preset keeps its shape, so the scan can find the field by arithmetic.
    let logic = &project.all_pous()[0].sections[0].rungs[0].logic;
    let preset = match logic {
        Logic::Series { items } => items.iter().find_map(|item| match item {
            Logic::Block { preset, .. } => Some(preset.clone()),
            _ => None,
        }),
        _ => None,
    };
    assert_eq!(preset.as_deref(), Some("Recipes[Sel].FillTime"));

    let compiled = compile(&project).expect("the struct array compiles");
    assert_eq!(
        compiled.program.words.dints, 5,
        "two fields times two elements, plus `Sel`"
    );
    assert_eq!(compiled.program.init.len(), 4);
}

#[test]
fn declared_types_are_read_and_carried() {
    use linc_plc_core::model::VarType;

    let xml = r#"<?xml version="1.0"?>
<project>
  <contentHeader name="typed"/>
  <types><pous><pou name="P" pouType="program">
    <interface><localVars>
      <variable name="Count"><type><INT/></type></variable>
      <variable name="Total"><type><DINT/></type></variable>
      <variable name="Level"><type><REAL/></type></variable>
      <variable name="Dwell"><type><TIME/></type></variable>
      <variable name="Steps"><type><array><baseElementType><DINT/></baseElementType><dimension lower="0" upper="5"/></array></type></variable>
      <variable name="Station"><type><derived name="FillStation"/></type></variable>
      <variable name="Flag"><type><BOOL/></type></variable>
    </localVars>
    <inputVars>
      <variable name="Start"><type><BOOL/></type></variable>
      <variable name="Speed"><type><INT/></type></variable>
      <variable name="Pressure"><type><REAL/></type></variable>
    </inputVars></interface>
    <body><LD>
      <leftPowerRail localId="1"/>
      <rightPowerRail localId="2"/>
      <contact localId="3"><connectionPointIn><connection refLocalId="1"/></connectionPointIn><variable>Start</variable></contact>
      <coil localId="4"><connectionPointIn><connection refLocalId="3"/></connectionPointIn><variable>Flag</variable></coil>
    </LD></body>
  </pou></pous></types>
</project>"#;
    let project = imported(xml);
    let types: Vec<(String, String)> = project
        .vars
        .iter()
        .map(|var| (var.name.clone(), var.ty.label()))
        .collect();
    assert_eq!(
        types,
        vec![
            ("count".to_string(), "INT".to_string()),
            ("total".to_string(), "DINT".to_string()),
            ("level".to_string(), "REAL".to_string()),
            ("dwell".to_string(), "TIME".to_string()),
            ("steps".to_string(), "DINT[6]".to_string()),
            ("station".to_string(), "FillStation".to_string()),
            ("flag".to_string(), "BOOL".to_string()),
        ],
        "each declaration keeps the type it was imported with"
    );
    assert_eq!(
        project.vars[4].ty,
        VarType::Array {
            base: Box::new(VarType::Dint),
            len: 6
        }
    );
    // A point's type picks its HAL pin, so an INT input is a word and a REAL input
    // is a float; the importer must not make them all bits.
    let kinds: Vec<(&str, linc_plc_core::IoKind)> = project
        .io
        .iter()
        .map(|point| (point.name.as_str(), point.kind))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("start", linc_plc_core::IoKind::Bit),
            ("speed", linc_plc_core::IoKind::Word),
            ("pressure", linc_plc_core::IoKind::Float),
        ]
    );

    // A structured declaration is named and refused: the type is readable, the
    // storage is not there yet.
    let refused = compile(&project).expect_err("a structured declaration has no storage");
    assert!(
        refused
            .iter()
            .any(|item| item.code == "var_type" && item.message.contains("FillStation")),
        "{refused:#?}"
    );

    // The typed declarations are storage the scan can reach: a compile has to lay
    // out slots for them and keep the bit variable a bit.
    let mut storable = project.clone();
    storable.vars.retain(|var| !var.ty.is_structured());
    let compiled = compile(&storable).expect("typed declarations compile");
    assert_eq!(
        compiled.program.words.dints, 7,
        "the array owns six slots and `Total` one"
    );
    assert_eq!(compiled.program.words.ints, 1);
    assert_eq!(compiled.program.words.reals, 1);
    assert_eq!(compiled.program.words.times, 1);
}

/// A bit of a word keeps its shape through the import: the word is spelled the way
/// its declaration is, the bit stays a number, and a marker word keeps the profile's
/// spelling. A bit *address* is still sanitised the way it always was.
#[test]
fn a_bit_of_a_word_keeps_its_operand_shape() {
    let xml = r#"<?xml version="1.0"?>
<project>
  <contentHeader name="bits"/>
  <types><pous><pou name="P" pouType="program">
    <interface><localVars>
      <variable name="Bits"><type><array><baseElementType><INT/></baseElementType><dimension lower="0" upper="0"/></array></type></variable>
    </localVars>
    <inputVars>
      <variable name="Start"><type><BOOL/></type></variable>
      <variable name="%I0.0"><type><BOOL/></type></variable>
    </inputVars>
    <outputVars>
      <variable name="Y"><type><BOOL/></type></variable>
      <variable name="Z"><type><BOOL/></type></variable>
    </outputVars></interface>
    <body><LD>
      <leftPowerRail localId="1"/>
      <rightPowerRail localId="2"/>
      <contact localId="3"><connectionPointIn><connection refLocalId="1"/></connectionPointIn><variable>Start</variable></contact>
      <coil localId="4" storage="set"><connectionPointIn><connection refLocalId="3"/></connectionPointIn><variable>Bits[0].3</variable></coil>
      <contact localId="5"><connectionPointIn><connection refLocalId="1"/></connectionPointIn><variable>Bits[0].3</variable></contact>
      <coil localId="6"><connectionPointIn><connection refLocalId="5"/></connectionPointIn><variable>Y</variable></coil>
      <contact localId="7"><connectionPointIn><connection refLocalId="1"/></connectionPointIn><variable>%I0.0</variable></contact>
      <coil localId="8"><connectionPointIn><connection refLocalId="7"/></connectionPointIn><variable>Z</variable></coil>
    </LD></body>
  </pou></pous></types>
</project>"#;
    let project = imported(xml);
    let rungs = &project.all_pous()[0].sections[0].rungs;
    assert_eq!(
        rungs[0].coil().var,
        "bits[0].3",
        "the word is spelled the way its declaration is, and the bit stays a number"
    );
    use linc_plc_core::model::Logic as L;
    assert_eq!(
        rungs[1].logic,
        L::Contact {
            var: "bits[0].3".into(),
            negated: false
        }
    );
    assert_eq!(
        rungs[2].logic,
        L::Contact {
            var: "n__i0_0".into(),
            negated: false
        },
        "a bit address is still a symbol name, not a bit of a word"
    );
    assert_eq!(
        project.vars.len(),
        1,
        "a bit of a word declares its word, not the operand"
    );

    // It resolves and runs: element 0 of `bits`, bit 3.
    let compiled = compile(&project).expect("a bit of a word compiles");
    let mut vm = Vm::new(&compiled);
    // `Start` and the `%I0.0` input are both true: the one sets the bit, the other
    // drives the coil fed by the bit address.
    vm.scan(&[true, true]);
    assert_eq!(
        vm.word(compiled.program.words.int_base()),
        1 << 3,
        "the coil set bit 3 of the word"
    );
    assert_eq!(vm.outputs().last(), Some(&true), "and the contact read it");
}

/// Several POUs in one file: every routine imports, a call names its POU, a compute
/// coil carries an assignment, and a preset may be one element of a declared array.
#[test]
fn a_routine_per_pou_imports_with_calls_and_compute_coils() {
    let xml = r#"<?xml version="1.0"?>
<project>
  <contentHeader name="routines"/>
  <types><pous>
  <pou name="MAIN" pouType="program">
    <interface><localVars>
      <variable name="Durations"><type><array><baseElementType><DINT/></baseElementType><dimension lower="0" upper="2"/></array></type><initialValue><arrayValue><value>100</value><value>200</value><value>300</value></arrayValue></initialValue></variable>
      <variable name="Phase"><type><DINT/></type></variable>
    </localVars>
    <outputVars><variable name="Y"><type><BOOL/></type></variable></outputVars></interface>
    <body><LD>
      <leftPowerRail localId="0"/>
      <rightPowerRail localId="99"/>
      <coil localId="1" storage="call"><connectionPointIn><connection refLocalId="0"/></connectionPointIn><variable>SEQUENCE</variable></coil>
      <coil localId="2" storage="compute"><connectionPointIn><connection refLocalId="0"/></connectionPointIn><expression>Phase = Phase + 1</expression></coil>
      <contact localId="3"><connectionPointIn><connection refLocalId="0"/></connectionPointIn><variable>Phase</variable></contact>
    </LD></body>
  </pou>
  <pou name="SEQUENCE" pouType="program">
    <interface/>
    <body><LD>
      <leftPowerRail localId="0"/>
      <rightPowerRail localId="99"/>
      <block localId="4" typeName="TON"><connectionPointIn><connection refLocalId="0"/></connectionPointIn><inputVariables><variable formalParameter="PT"><expression>Durations[Phase]</expression></variable></inputVariables></block>
      <coil localId="5" storage="compute"><connectionPointIn><connection refLocalId="4"/></connectionPointIn><expression>Phase = 0</expression></coil>
    </LD></body>
  </pou>
  </pous></types>
</project>"#;
    let project = imported(xml);
    let pous = project.all_pous();
    assert_eq!(pous.len(), 2, "both routines import");
    assert_eq!(pous[0].name, "MAIN");
    assert_eq!(pous[1].name, "SEQUENCE");
    assert_eq!(
        project.vars[0].init,
        vec![100, 200, 300],
        "the declaration's starting values survived the import"
    );
    let rungs = &pous[0].sections[0].rungs;
    assert_eq!(rungs[0].coil().kind, linc_plc_core::model::CoilKind::Call);
    assert_eq!(
        rungs[1].coil().kind,
        linc_plc_core::model::CoilKind::Compute
    );
    assert_eq!(
        rungs[1].coil().expr.as_deref(),
        Some("Phase = Phase + 1"),
        "a compute coil keeps its assignment"
    );
    assert_eq!(
        pous[1].sections[0].name, "sequence",
        "each body's section is named after its POU, so two bodies can coexist"
    );

    // It compiles and runs: the call inlines the routine, the array element sets the
    // timer's preset, and the assignment moves `Phase`.
    let compiled = compile(&project).expect("the routines compile");
    assert_eq!(
        compiled.program.init,
        vec![(16, 100), (17, 200), (18, 300)],
        "the initial values are part of the program"
    );
    let mut vm = Vm::new(&compiled);
    vm.scan(&[true, false]);
    let phase = compiled.program.words.dint_base() + 3;
    assert_eq!(vm.word(phase), 1, "the compute coil ran");
    assert_eq!(vm.block_value(0), 0, "the timer saw the rail's true bit");
}

#[test]
fn a_type_with_no_width_is_refused_by_name() {
    let xml = r#"<?xml version="1.0"?>
<project>
  <contentHeader name="odd"/>
  <types><pous><pou name="P" pouType="program">
    <interface><localVars>
      <variable name="Weird"><type><LREAL/></type></variable>
      <variable name="Broken"><type/></variable>
      <variable name="NoType"/>
    </localVars></interface>
    <body><LD><leftPowerRail localId="1"/><rightPowerRail localId="2"/></LD></body>
  </pou></pous></types>
</project>"#;
    let diagnostics = plcopen::import(xml).expect_err("an unknown type is refused");
    let messages: Vec<&str> = diagnostics
        .iter()
        .filter(|item| item.code == "plcopen_unsupported_element")
        .map(|item| item.message.as_str())
        .collect();
    assert!(
        messages.contains(&"variable 'Weird' has a type with no LiNC form"),
        "{diagnostics:#?}"
    );
    assert!(
        messages.contains(&"variable 'Broken' has a type with no LiNC form"),
        "{diagnostics:#?}"
    );
    assert!(
        messages.contains(&"variable 'NoType' has a type with no LiNC form"),
        "{diagnostics:#?}"
    );
}

/// One block rung, built by hand: the body the importer's backwards walk cannot
/// reach yet, but the model and the compiler can now express and run it.
#[test]
fn a_block_with_no_coil_imports_into_a_runnable_rung() {
    use linc_plc_core::grid::Cell;
    use linc_plc_core::model::{Coil, Rung, Section};

    let mut project = Project::new("block-only");
    project.io = vec![
        linc_plc_core::IoPoint::new("go", linc_plc_core::IoDirection::In),
        linc_plc_core::IoPoint::new("done", linc_plc_core::IoDirection::Out),
    ];
    let mut count = Rung::without_coil(Logic::Const { value: true });
    count.cells = vec![
        Cell::Contact {
            var: "go".to_string(),
            negated: false,
        },
        Cell::Block {
            block: "CTU".to_string(),
            instance: "CTU0".to_string(),
            preset: "2".to_string(),
        },
    ];
    // A second rung reads the block's done, so the run is observable.
    let mut lamp = Rung::new(
        Logic::Contact {
            var: "CTU0.Q".to_string(),
            negated: false,
        },
        Coil::new("done", CoilKind::Normal),
    );
    lamp.cells = vec![Cell::Contact {
        var: "CTU0.Q".to_string(),
        negated: false,
    }];
    let mut pou = linc_plc_core::Pou::new("MAIN", PouLanguage::Ld);
    pou.sections = vec![Section {
        name: "main".into(),
        rungs: vec![count, lamp],
    }];
    project.pous = vec![pou];

    let errors = validate(&project)
        .into_iter()
        .filter(|item| item.severity == Severity::Error)
        .collect::<Vec<_>>();
    assert!(
        errors.is_empty(),
        "a rung with no coil is a real rung: {errors:#?}"
    );

    let compiled = compile(&project).expect("compile");
    let mut vm = Vm::new(&compiled);
    for _ in 0..2 {
        vm.scan(&[false]);
        vm.scan(&[true]);
    }
    assert_eq!(
        vm.outputs().first().copied(),
        Some(true),
        "the block ran and its done reached the second rung"
    );
}

/// Old files carry `"coil": {..}` and must still decode; a rung with no coil is
/// written by omitting the key.
#[test]
fn the_coil_field_is_optional_and_backwards_compatible() {
    let with_coil = r#"{
        "schema": 1, "name": "old", "io": [], "vars": [], "watch": [],
        "sections": [{
            "name": "main", "rungs": [{
                "logic": {"op": "const", "value": true},
                "coil": {"var": "run", "kind": "normal"}
            }]
        }],
        "pous": []
    }"#;
    let project: Project = serde_json::from_str(with_coil).expect("an old file decodes");
    let rung = &project.sections[0].rungs[0];
    assert_eq!(rung.coil().var, "run");
    assert!(rung.has_coil());

    let without_coil = r#"{
        "schema": 1, "name": "new", "io": [], "vars": [], "watch": [],
        "sections": [{
            "name": "main", "rungs": [{
                "logic": {"op": "const", "value": true}
            }]
        }],
        "pous": []
    }"#;
    let project: Project = serde_json::from_str(without_coil).expect("a coil-less rung decodes");
    assert!(!project.sections[0].rungs[0].has_coil());

    let saved = serde_json::to_string(&project).expect("serialize");
    assert!(
        !saved.contains("\"coil\""),
        "a rung with no coil writes no key: {saved}"
    );
}

/// Appendix E: every `<block>` used to bind instance 0, so two timers shared one
/// `FbState`. Each block of a kind gets its own instance now.
#[test]
fn two_timers_in_one_program_get_their_own_instances() {
    let xml = include_str!("fixtures/two-timers.ld.xml");
    let project = imported(xml);
    // A block rung is the block series its feeding bit: the block consumes the
    // power bit and pushes its `Q`, so a bare block would be handed an empty stack.
    let block_of = |logic: &Logic| match logic {
        Logic::Block { instance, .. } => Some(instance.clone()),
        Logic::Series { items } => items.iter().find_map(|item| match item {
            Logic::Block { instance, .. } => Some(instance.clone()),
            _ => None,
        }),
        _ => None,
    };
    let mut instances = Vec::new();
    for rung in &project.all_pous()[0].sections[0].rungs {
        if let Some(instance) = block_of(&rung.logic) {
            instances.push(instance);
        }
    }
    instances.sort();
    assert_eq!(
        instances,
        vec!["TON0".to_string(), "TON1".to_string()],
        "two timers, two instances"
    );
}
