//! M2's acceptance is also a property of the code: one operand grammar, in one
//! module, with every other accessor delegating to it.
//!
//! The handoff's §II.5 M2 says "exactly one function in the tree turns text into
//! a slot; `grep -c MW` in `linc-plc-core/src` drops to the profile module". A
//! behavioural test cannot see a second parser that happens to agree today, so
//! these scan the sources the way that sentence does.

use linc_plc_core::profile::resolve::{parse_preset, Operand, MARKER_PREFIX, TIME_PREFIX};
use linc_plc_core::profile::SymbolLookup;

fn sources() -> Vec<(&'static str, &'static str)> {
    vec![
        ("clp.rs", include_str!("../src/clp.rs")),
        ("compile.rs", include_str!("../src/compile.rs")),
        ("expr.rs", include_str!("../src/expr.rs")),
        ("il.rs", include_str!("../src/il.rs")),
        ("lib.rs", include_str!("../src/lib.rs")),
        ("model.rs", include_str!("../src/model.rs")),
        ("plcopen.rs", include_str!("../src/plcopen.rs")),
        ("protocol.rs", include_str!("../src/protocol.rs")),
        ("retain.rs", include_str!("../src/retain.rs")),
        ("validate.rs", include_str!("../src/validate.rs")),
        ("vm.rs", include_str!("../src/vm.rs")),
    ]
}

/// The marker spelling lives in the profile and nowhere else: no module outside
/// `src/profile/` names a marker word, in code or in a comment.
#[test]
fn no_module_outside_the_profile_spells_a_marker_word() {
    for (name, text) in sources() {
        assert!(
            !text.contains(MARKER_PREFIX),
            "{name} spells `{MARKER_PREFIX}`; ask the profile for the spelling \
             (`marker_word`, `marker_range`, `resolve`) instead"
        );
    }
}

/// The document says which semantics it means. An `iec` project serialises
/// without the field, so its bytes — and the program hash a retained store is
/// keyed by — are exactly what they were before the field existed. A
/// `classicladder` one carries its name, and the name in the JSON is the name
/// diagnostics print.
#[test]
fn a_projects_dialect_is_written_only_when_it_is_not_iec() {
    use linc_plc_core::{Dialect, Project};

    let iec = Project::new("plain");
    assert_eq!(
        iec.profile,
        Dialect::Iec,
        "the default is the shipped semantics"
    );
    let json = serde_json::to_string(&iec).expect("serialize");
    assert!(
        !json.contains("profile"),
        "an iec project must not grow a field, or every hash changes: {json}"
    );
    let back: Project = serde_json::from_str(&json).expect("decode");
    assert_eq!(back.profile, Dialect::Iec);

    let mut classic = Project::new("cl");
    classic.profile = Dialect::ClassicLadder;
    let json = serde_json::to_string(&classic).expect("serialize");
    assert!(
        json.contains("\"classicladder\""),
        "the document name is `classicladder`: {json}"
    );
    let back: Project = serde_json::from_str(&json).expect("decode");
    assert_eq!(back.profile, Dialect::ClassicLadder);
    assert_eq!(
        Dialect::ClassicLadder.label(),
        "classicladder",
        "the document name and the printed name cannot drift"
    );

    // A name the tree does not know is a decode error, not a silent default.
    let mut value: serde_json::Value = serde_json::from_str(&json).expect("json");
    value["profile"] = serde_json::json!("plc5");
    assert!(
        serde_json::from_value::<Project>(value).is_err(),
        "an unknown profile must not be guessed"
    );

    // The dialect is a fact of the document, not a diagnostic: both validate
    // cleanly, and the profile is what gives them different meaning.
    for project in [&iec, &classic] {
        assert!(
            !linc_plc_core::validate(project)
                .iter()
                .any(|item| item.code == "profile"),
            "`{}` says nothing about profiles",
            project.profile.label()
        );
    }
}

/// M4's acceptance: the validator names no instruction and no operand format.
/// The rules are `profile/instr.rs`'s; `validate.rs` walks the document and
/// checks its structure, so a new mnemonic or a changed operand spelling cannot
/// need an edit there.
#[test]
fn validate_spells_no_mnemonic_and_no_operand_format() {
    let validate = include_str!("../src/validate.rs");
    for row in linc_plc_core::profile::spec::SPECS {
        let literal = format!("\"{}\"", row.mnemonic);
        assert!(
            !validate.contains(&literal),
            "validate.rs spells `{}`; its rule belongs in `profile/instr.rs`",
            row.mnemonic
        );
    }
    for token in [
        linc_plc_core::profile::resolve::MARKER_PREFIX,
        linc_plc_core::profile::resolve::TIME_PREFIX,
        ".PT",
        ".PV",
        ".ET",
        ".CV",
    ] {
        let literal = format!("\"{token}\"");
        assert!(
            !validate.contains(&literal),
            "validate.rs spells the operand format `{token}`; `resolve` owns it"
        );
    }
    // The instruction rules moved, not vanished: each family still has its home.
    let instr = include_str!("../src/profile/instr.rs");
    for marker in [
        "il_type",
        "cell_block",
        "coil_expr",
        "fbd_input",
        "compute_target",
    ] {
        assert!(
            instr.contains(marker),
            "`{marker}` should be an instruction rule, in `profile/instr.rs`"
        );
    }
}

/// The bank's width is the layout's. M3's acceptance: no module outside
/// `src/profile/` names the marker-word count, so a second dialect can change it
/// without editing the compiler, the importer, the validator or the editor.
#[test]
fn no_module_outside_the_profile_names_the_word_count() {
    for (name, text) in sources() {
        assert!(
            !text.contains("WORD_COUNT"),
            "{name} names the marker-word count; ask the profile (`marker_word_count`, \
             `marker_words`, `WordLayout`) instead"
        );
    }
}

/// A time preset's spelling and a block member's suffix are the resolver's, so no
/// other module builds either. Diagnostic sentences and doc examples may still
/// name them — they are the user's words, not a second parser — so the scan looks
/// at code only.
#[test]
fn no_module_outside_the_profile_parses_a_time_or_a_block_member() {
    for (name, text) in sources() {
        let code = code_only(text);
        assert!(
            !code.contains(TIME_PREFIX) && !code.contains("t#"),
            "{name} spells a time preset in code; use `time_preset` / `parse_preset`"
        );
        for member in [".PT", ".PV", ".ET", ".CV"] {
            assert!(
                !code.contains(member),
                "{name} spells the block member `{member}` in code; use `block_member`"
            );
        }
    }
}

/// Strip comments and string literals, so a scan sees the code and not the
/// sentences it emits. Adequate for these sources: no raw strings, and `//` only
/// starts a comment outside a literal.
fn code_only(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    let mut in_line = false;
    let mut in_string = false;
    let mut escaped = false;
    while let Some(ch) = chars.next() {
        if in_line {
            if ch == '\n' {
                in_line = false;
                out.push(ch);
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '/' && chars.peek() == Some(&'/') {
            chars.next();
            in_line = true;
            continue;
        }
        if ch == '"' {
            in_string = true;
            continue;
        }
        out.push(ch);
    }
    out
}

/// The operand accessors that other modules kept are delegations, not parsers:
/// each one hands its text to the resolver and keeps no spelling of its own.
#[test]
fn every_operand_accessor_delegates_to_the_resolver() {
    let il = include_str!("../src/il.rs");
    let compile = include_str!("../src/compile.rs");
    // `il::word_operand` is the narrow marker-word form: one call, no grammar.
    let body = il
        .split("pub fn word_operand")
        .nth(1)
        .expect("il::word_operand exists");
    let body = &body[..body.find("\n}\n").expect("word_operand has a body")];
    assert!(
        body.contains("profile::resolve::marker_word_slot"),
        "il::word_operand must delegate to the profile"
    );
    assert!(
        !body.contains("parse") && !body.contains("strip_prefix"),
        "il::word_operand must not parse; the profile does"
    );
    // `compile::resolve_word` is the same deal, with the symbol table.
    let body = compile
        .split("fn resolve_word")
        .nth(1)
        .expect("compile::resolve_word exists");
    let body = &body[..body.find("\n}\n").expect("resolve_word has a body")];
    assert!(
        body.contains("profile::resolve::resolve"),
        "compile::resolve_word must delegate to the profile"
    );
    assert!(!body.contains("strip_prefix") && !body.contains("strip_suffix"));
}

/// One function in the profile turns operand text into a slot. Everything else
/// there either answers a narrower question (`marker_word_slot` is the marker
/// word's lexer, which `resolve` composes) or produces text rather than parsing.
#[test]
fn resolve_is_the_only_operand_parser() {
    let profile = include_str!("../src/profile/resolve.rs");
    let parsers = profile.match_indices("fn resolve(").count()
        + profile.match_indices("\nfn resolve(").count();
    assert_eq!(parsers, 1, "one `fn resolve`, the operand grammar");
    // The grammar's own sub-parsers live in the same module and are not exposed
    // as a second entry point: `parse_preset` and `parse_block` are published
    // because the compiler's tests and the importer call them, but neither can
    // address a slot.
    let empty: SymbolLookup = SymbolLookup::new();
    assert!(matches!(
        linc_plc_core::profile::resolve::resolve("T#2s", &empty),
        Ok(Operand::Const(2_000))
    ));
    assert!(linc_plc_core::profile::resolve::resolve("MW3", &empty).is_ok());
}

/// A preset the profile writes parses back to the same value, so the importer's
/// text and the compiler's parser cannot drift.
#[test]
fn a_written_preset_parses_back() {
    for milliseconds in [0, 1, 250, 4_000, 300_000] {
        let text = linc_plc_core::profile::resolve::time_preset(milliseconds);
        assert_eq!(
            parse_preset(&text, true),
            Some(milliseconds),
            "`{text}` must parse back"
        );
    }
}
