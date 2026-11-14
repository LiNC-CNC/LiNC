//! M1's acceptance is a property of the code, not a feature: one instruction
//! table, and no second spelling list in the modules that read it.
//!
//! The handoff's §II.5 says "compile.rs contains no match on a mnemonic string;
//! the ladder, the validator and the compiler all read the same table". A source
//! scan is the only honest way to test "no match": a behavioural test cannot see
//! a name list that happens to agree with the table today and drifts tomorrow.

use linc_plc_core::compile::{compare_op, FbKind, MathOp};
use linc_plc_core::il::{parse_line, IlLine};
use linc_plc_core::profile::spec::{self, Family, IlForm, SPECS};

/// No mnemonic is spelled outside the table — not in the compiler, not in the
/// validator. One-character names (`=`, `+`, `S`, `R`) are scanned too: they are
/// unambiguous literals today, and a stray one would be exactly the drift this
/// guards.
#[test]
fn compile_and_validate_spell_no_mnemonic() {
    let compile = include_str!("../src/compile.rs");
    let validate = include_str!("../src/validate.rs");
    for row in SPECS {
        let literal = format!("\"{}\"", row.mnemonic);
        assert!(
            !compile.contains(&literal),
            "compile.rs spells `{}`; the instruction table is the only place a mnemonic lives",
            row.mnemonic
        );
        assert!(
            !validate.contains(&literal),
            "validate.rs spells `{}`; classify it from the table row instead",
            row.mnemonic
        );
    }
}

/// The parser admits exactly the table's IL mnemonics, and nothing else.
#[test]
fn the_parser_accepts_exactly_the_tables_il_mnemonics() {
    for mnemonic in spec::il_mnemonics() {
        let row = spec::spec(mnemonic).expect("published mnemonic");
        let line = if row.takes_operand() {
            format!("{mnemonic} X0")
        } else {
            mnemonic.to_string()
        };
        assert!(
            matches!(parse_line(&line), IlLine::Instruction { .. }),
            "`{line}` must parse"
        );
    }
    for name in ["FROB", "TONN", "MOV"] {
        assert!(
            matches!(parse_line(&format!("{name} X0")), IlLine::Bad(_)),
            "`{name}` is not an instruction"
        );
    }
}

/// A block name, an arithmetic name and a compare name all resolve through their
/// row, and an IL-only or unknown name resolves to none of them.
#[test]
fn the_compiler_resolves_a_block_math_or_compare_through_its_row() {
    for row in SPECS {
        match row.family {
            Family::Fb(_) => assert!(
                FbKind::parse(row.mnemonic).is_some(),
                "`{}` is a block row",
                row.mnemonic
            ),
            Family::Math(_) => assert!(
                MathOp::parse(row.mnemonic).is_some(),
                "`{}` is a math row",
                row.mnemonic
            ),
            Family::Compare(_) => assert!(
                compare_op(row.mnemonic).is_some(),
                "`{}` is a compare row",
                row.mnemonic
            ),
            Family::Il => {}
        }
    }

    // A block name is not a math op or a compare; an IL name is neither.
    assert!(MathOp::parse("TON").is_none());
    assert!(compare_op("ADD").is_none());
    assert!(MathOp::parse("MOD").is_none(), "MOD has no block form");
    assert!(FbKind::parse("LD").is_none());
    assert!(FbKind::parse("FROB").is_none());
    assert!(MathOp::parse("FROB").is_none());
    assert!(compare_op("FROB").is_none());
}

/// The table is the whole set: every row is distinguishable, and the IL set is
/// exactly the rows the compiler can lower.
#[test]
fn the_table_is_the_whole_instruction_set() {
    assert!(SPECS.len() >= 40, "the shipped set is ~50 instructions");
    for row in SPECS {
        match row.il {
            IlForm::None => {
                // A block-only name: it must resolve as a block, math or compare.
                let block = matches!(row.family, Family::Fb(_));
                let math = matches!(row.family, Family::Math(_));
                let compare = matches!(row.family, Family::Compare(_));
                assert!(
                    block || math || compare,
                    "`{}` has no IL form and no block family",
                    row.mnemonic
                );
            }
            _ => {
                // A row with an IL form is either IL-only, or it shares its name
                // with a block whose family matches the action (`ADD` is Math +
                // Word, `GT` is Compare + Compare).
                let consistent = match (row.family, row.il) {
                    (Family::Il, _) => true,
                    (Family::Math(_), IlForm::Word(_)) => true,
                    (Family::Compare(_), IlForm::Compare(_)) => true,
                    _ => false,
                };
                assert!(
                    consistent,
                    "`{}`: {:?} with {:?}",
                    row.mnemonic, row.family, row.il
                );
            }
        }
    }
}
