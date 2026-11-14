//! The instruction set as data.
//!
//! One row per instruction name, and one reader for all of them: `compile.rs`
//! resolves block kinds, block arithmetic and compares through [`spec`]; the IL
//! lowerer resolves a mnemonic through the same row; `validate.rs` classifies a
//! mnemonic from the row's [`IlForm`] instead of keeping its own name lists; and
//! the editor's mnemonic help reads [`il_mnemonics`].
//!
//! Nothing in here depends on `compile`, `vm` or `validate`: a dialect is data,
//! and the modules that use it map the data onto their own types. That direction
//! is what lets M2–M5 move the rest of the dialect behind the seam without a
//! cycle.
//!
//! A name may carry more than one role. `ADD` is a block arithmetic operation
//! *and* an IL accumulator instruction, so its row is [`Family::Math`] with
//! [`IlForm::Word`]; `GT` is a compare block and an IL compare. One row, read by
//! both sides, is the point.

/// A standard function block, by behaviour rather than by spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FbName {
    Ton,
    Tof,
    Tp,
    Ctu,
    Ctd,
    Sr,
    Rs,
    /// Rising edge: `RT` and `R_TRIG`.
    Rt,
    /// Falling edge: `RF` and `F_TRIG`.
    Rf,
}

impl FbName {
    /// The mnemonic this kind is drawn and imported as, read from the table so
    /// the spelling has one source.
    pub fn text(self) -> &'static str {
        SPECS
            .iter()
            .find(|row| row.family == Family::Fb(self))
            .map(|row| row.mnemonic)
            .expect("every block kind has a table row")
    }

    /// The ABI code, `0..=8`, in the order the instance slots are numbered.
    /// `compile::FbKind::code` delegates here, so the numbering has one source.
    pub fn code(self) -> u16 {
        match self {
            Self::Ton => 0,
            Self::Tof => 1,
            Self::Tp => 2,
            Self::Ctu => 3,
            Self::Ctd => 4,
            Self::Rt => 5,
            Self::Rf => 6,
            Self::Sr => 7,
            Self::Rs => 8,
        }
    }
}

/// Block arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathName {
    Add,
    Sub,
    Mul,
    Div,
    Move,
}

/// The IL accumulator's word operations. `MOD` has no block form, which is why
/// this is separate from [`MathName`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordName {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

/// Block comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompareName {
    Gt,
    Ge,
    Eq,
    Ne,
    Le,
    Lt,
}

/// What an instruction is, independent of how it is spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// IL-only: accumulator operations, stores, jumps, calls and returns.
    Il,
    /// Block arithmetic, also reachable from IL.
    Math(MathName),
    /// A block comparison, also reachable from IL.
    Compare(CompareName),
    /// A standard function block.
    Fb(FbName),
}

/// How the element is drawn. Presentation lives with the instruction so one
/// table can drive the renderer, as the handoff's §II.4 asks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// `-[ ]-`
    Contact,
    /// `-[/]-`
    ContactNc,
    /// `-( )-` and its letters, against the right rail
    Coil,
    /// A compact box: compare, arithmetic, edge
    Small,
    /// A function-block box
    Block,
}

/// What an instruction reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperandKind {
    /// Either bank: `ST` writes a bit or a marker word, `CAL` names a POU.
    Any,
    Bit,
    Word,
    /// A time literal such as `T#4s`.
    Time,
    /// A count.
    Count,
}

/// The IL language's action for a mnemonic. The IL body is a single
/// accumulator, so the action is the whole of what a mnemonic means there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IlForm {
    /// `LD`
    Push,
    /// `LDN`
    PushNot,
    And,
    AndNot,
    Or,
    OrNot,
    Xor,
    XorNot,
    /// `NOT`
    Not,
    /// `ST`, `=` and the word store
    Store,
    /// `STN`
    StoreNot,
    Set,
    Reset,
    /// `ADD` … `MOD`: read the accumulator, combine, leave a word.
    Word(WordName),
    /// `GT` … `LT`: read the accumulator, compare, leave a bit.
    Compare(CompareName),
    /// `JMP` / `JMPC` / `JMPCN`. `Some(when)` is the condition that takes the
    /// jump.
    Jump(Option<bool>),
    /// `CAL` / `CALC` / `CALCN`. `Some(when)` is the condition that *skips* the
    /// inlined block, matching `Op::JumpIf`.
    Call(Option<bool>),
    /// `RET` / `RETC` / `RETCN`. `Some(when)` is the condition that returns.
    Return(Option<bool>),
    /// Not an IL mnemonic: a block name.
    None,
}

/// One instruction.
#[derive(Debug, Clone, Copy)]
pub struct Spec {
    /// The canonical spelling. Lookup is case-insensitive; this is what the
    /// editor shows.
    pub mnemonic: &'static str,
    pub family: Family,
    /// Operands in order, for the block and IL forms.
    pub operands: &'static [OperandKind],
    /// How many trailing operands may be omitted.
    pub optional: u8,
    /// Drawn against the right rail: a coil.
    pub output: bool,
    pub shape: Shape,
    /// The glyph inside a coil or small box, for the renderer.
    pub letter: &'static str,
    /// What the mnemonic does in IL, or [`IlForm::None`] for a block-only name.
    pub il: IlForm,
}

impl Spec {
    /// Does this instruction take an operand at all? `NOT` and the returns do
    /// not; everything else does.
    pub fn takes_operand(&self) -> bool {
        !self.operands.is_empty()
    }
}

use Family::{Compare, Fb, Il, Math};
use IlForm::{
    And, AndNot, Call, Compare as IlCompare, Jump, Not, Or, OrNot, Push, PushNot, Reset, Return,
    Set, Store, StoreNot, Word as IlWord, Xor, XorNot,
};
use OperandKind::{Any, Bit, Count, Time, Word};
use Shape::{Block, Coil, Contact, ContactNc, Small};

/// Every instruction the LiNC profile admits, in editor order (IL first, then
/// the block arithmetic, compares and function blocks).
pub const SPECS: &[Spec] = &[
    // --- the IL accumulator -------------------------------------------------
    Spec {
        mnemonic: "LD",
        family: Il,
        operands: &[Bit],
        optional: 0,
        output: false,
        shape: Contact,
        letter: "",
        il: Push,
    },
    Spec {
        mnemonic: "LDN",
        family: Il,
        operands: &[Bit],
        optional: 0,
        output: false,
        shape: ContactNc,
        letter: "",
        il: PushNot,
    },
    Spec {
        mnemonic: "AND",
        family: Il,
        operands: &[Bit],
        optional: 0,
        output: false,
        shape: Contact,
        letter: "",
        il: And,
    },
    Spec {
        mnemonic: "ANDN",
        family: Il,
        operands: &[Bit],
        optional: 0,
        output: false,
        shape: ContactNc,
        letter: "",
        il: AndNot,
    },
    Spec {
        mnemonic: "OR",
        family: Il,
        operands: &[Bit],
        optional: 0,
        output: false,
        shape: Contact,
        letter: "",
        il: Or,
    },
    Spec {
        mnemonic: "ORN",
        family: Il,
        operands: &[Bit],
        optional: 0,
        output: false,
        shape: ContactNc,
        letter: "",
        il: OrNot,
    },
    Spec {
        mnemonic: "XOR",
        family: Il,
        operands: &[Bit],
        optional: 0,
        output: false,
        shape: Contact,
        letter: "",
        il: Xor,
    },
    Spec {
        mnemonic: "XORN",
        family: Il,
        operands: &[Bit],
        optional: 0,
        output: false,
        shape: ContactNc,
        letter: "",
        il: XorNot,
    },
    Spec {
        mnemonic: "NOT",
        family: Il,
        operands: &[],
        optional: 0,
        output: false,
        shape: Small,
        letter: "NOT",
        il: Not,
    },
    Spec {
        mnemonic: "ST",
        family: Il,
        operands: &[Any],
        optional: 0,
        output: true,
        shape: Coil,
        letter: "",
        il: Store,
    },
    Spec {
        mnemonic: "STN",
        family: Il,
        operands: &[Any],
        optional: 0,
        output: true,
        shape: Coil,
        letter: "/",
        il: StoreNot,
    },
    Spec {
        mnemonic: "=",
        family: Il,
        operands: &[Any],
        optional: 0,
        output: true,
        shape: Coil,
        letter: "",
        il: Store,
    },
    Spec {
        mnemonic: "S",
        family: Il,
        operands: &[Bit],
        optional: 0,
        output: true,
        shape: Coil,
        letter: "S",
        il: Set,
    },
    Spec {
        mnemonic: "R",
        family: Il,
        operands: &[Bit],
        optional: 0,
        output: true,
        shape: Coil,
        letter: "R",
        il: Reset,
    },
    // --- arithmetic: a block and an IL accumulator operation ----------------
    Spec {
        mnemonic: "ADD",
        family: Math(MathName::Add),
        operands: &[Word, Word],
        optional: 1,
        output: false,
        shape: Small,
        letter: "+",
        il: IlWord(WordName::Add),
    },
    Spec {
        mnemonic: "SUB",
        family: Math(MathName::Sub),
        operands: &[Word, Word],
        optional: 1,
        output: false,
        shape: Small,
        letter: "-",
        il: IlWord(WordName::Sub),
    },
    Spec {
        mnemonic: "MUL",
        family: Math(MathName::Mul),
        operands: &[Word, Word],
        optional: 1,
        output: false,
        shape: Small,
        letter: "*",
        il: IlWord(WordName::Mul),
    },
    Spec {
        mnemonic: "DIV",
        family: Math(MathName::Div),
        operands: &[Word, Word],
        optional: 1,
        output: false,
        shape: Small,
        letter: "/",
        il: IlWord(WordName::Div),
    },
    Spec {
        mnemonic: "MOD",
        family: Il,
        operands: &[Word],
        optional: 0,
        output: false,
        shape: Small,
        letter: "%",
        il: IlWord(WordName::Mod),
    },
    Spec {
        mnemonic: "MOVE",
        family: Math(MathName::Move),
        operands: &[Word],
        optional: 0,
        output: false,
        shape: Small,
        letter: "",
        il: IlForm::None,
    },
    // The operator spellings a grid compute cell has always accepted.
    Spec {
        mnemonic: "+",
        family: Math(MathName::Add),
        operands: &[Word, Word],
        optional: 1,
        output: false,
        shape: Small,
        letter: "+",
        il: IlForm::None,
    },
    Spec {
        mnemonic: "-",
        family: Math(MathName::Sub),
        operands: &[Word, Word],
        optional: 1,
        output: false,
        shape: Small,
        letter: "-",
        il: IlForm::None,
    },
    Spec {
        mnemonic: "*",
        family: Math(MathName::Mul),
        operands: &[Word, Word],
        optional: 1,
        output: false,
        shape: Small,
        letter: "*",
        il: IlForm::None,
    },
    Spec {
        mnemonic: "/",
        family: Math(MathName::Div),
        operands: &[Word, Word],
        optional: 1,
        output: false,
        shape: Small,
        letter: "/",
        il: IlForm::None,
    },
    Spec {
        mnemonic: ":=",
        family: Math(MathName::Move),
        operands: &[Word],
        optional: 0,
        output: false,
        shape: Small,
        letter: "",
        il: IlForm::None,
    },
    // --- compares: a block and an IL compare --------------------------------
    Spec {
        mnemonic: "GT",
        family: Compare(CompareName::Gt),
        operands: &[Word, Word],
        optional: 0,
        output: false,
        shape: Small,
        letter: ">",
        il: IlCompare(CompareName::Gt),
    },
    Spec {
        mnemonic: "GE",
        family: Compare(CompareName::Ge),
        operands: &[Word, Word],
        optional: 0,
        output: false,
        shape: Small,
        letter: ">=",
        il: IlCompare(CompareName::Ge),
    },
    Spec {
        mnemonic: "EQ",
        family: Compare(CompareName::Eq),
        operands: &[Word, Word],
        optional: 0,
        output: false,
        shape: Small,
        letter: "=",
        il: IlCompare(CompareName::Eq),
    },
    Spec {
        mnemonic: "NE",
        family: Compare(CompareName::Ne),
        operands: &[Word, Word],
        optional: 0,
        output: false,
        shape: Small,
        letter: "<>",
        il: IlCompare(CompareName::Ne),
    },
    Spec {
        mnemonic: "LE",
        family: Compare(CompareName::Le),
        operands: &[Word, Word],
        optional: 0,
        output: false,
        shape: Small,
        letter: "<=",
        il: IlCompare(CompareName::Le),
    },
    Spec {
        mnemonic: "LT",
        family: Compare(CompareName::Lt),
        operands: &[Word, Word],
        optional: 0,
        output: false,
        shape: Small,
        letter: "<",
        il: IlCompare(CompareName::Lt),
    },
    // --- control ------------------------------------------------------------
    Spec {
        mnemonic: "JMP",
        family: Il,
        operands: &[Word],
        optional: 0,
        output: false,
        shape: Small,
        letter: "",
        il: Jump(None),
    },
    Spec {
        mnemonic: "JMPC",
        family: Il,
        operands: &[Word],
        optional: 0,
        output: false,
        shape: Small,
        letter: "",
        il: Jump(Some(true)),
    },
    Spec {
        mnemonic: "JMPCN",
        family: Il,
        operands: &[Word],
        optional: 0,
        output: false,
        shape: Small,
        letter: "",
        il: Jump(Some(false)),
    },
    Spec {
        mnemonic: "CAL",
        family: Il,
        operands: &[Any],
        optional: 0,
        output: false,
        shape: Block,
        letter: "",
        il: Call(None),
    },
    Spec {
        mnemonic: "CALC",
        family: Il,
        operands: &[Any],
        optional: 0,
        output: false,
        shape: Block,
        letter: "",
        il: Call(Some(false)),
    },
    Spec {
        mnemonic: "CALCN",
        family: Il,
        operands: &[Any],
        optional: 0,
        output: false,
        shape: Block,
        letter: "",
        il: Call(Some(true)),
    },
    Spec {
        mnemonic: "RET",
        family: Il,
        operands: &[],
        optional: 0,
        output: false,
        shape: Small,
        letter: "",
        il: Return(None),
    },
    Spec {
        mnemonic: "RETC",
        family: Il,
        operands: &[],
        optional: 0,
        output: false,
        shape: Small,
        letter: "",
        il: Return(Some(true)),
    },
    Spec {
        mnemonic: "RETCN",
        family: Il,
        operands: &[],
        optional: 0,
        output: false,
        shape: Small,
        letter: "",
        il: Return(Some(false)),
    },
    // --- function blocks ----------------------------------------------------
    Spec {
        mnemonic: "TON",
        family: Fb(FbName::Ton),
        operands: &[Bit, Time],
        optional: 1,
        output: false,
        shape: Block,
        letter: "T",
        il: IlForm::None,
    },
    Spec {
        mnemonic: "TOF",
        family: Fb(FbName::Tof),
        operands: &[Bit, Time],
        optional: 1,
        output: false,
        shape: Block,
        letter: "T",
        il: IlForm::None,
    },
    Spec {
        mnemonic: "TP",
        family: Fb(FbName::Tp),
        operands: &[Bit, Time],
        optional: 1,
        output: false,
        shape: Block,
        letter: "T",
        il: IlForm::None,
    },
    Spec {
        mnemonic: "CTU",
        family: Fb(FbName::Ctu),
        operands: &[Bit, Bit, Count],
        optional: 2,
        output: false,
        shape: Block,
        letter: "C",
        il: IlForm::None,
    },
    Spec {
        mnemonic: "CTD",
        family: Fb(FbName::Ctd),
        operands: &[Bit, Bit, Count],
        optional: 2,
        output: false,
        shape: Block,
        letter: "C",
        il: IlForm::None,
    },
    Spec {
        mnemonic: "SR",
        family: Fb(FbName::Sr),
        operands: &[Bit, Bit],
        optional: 0,
        output: false,
        shape: Block,
        letter: "S",
        il: IlForm::None,
    },
    Spec {
        mnemonic: "RS",
        family: Fb(FbName::Rs),
        operands: &[Bit, Bit],
        optional: 0,
        output: false,
        shape: Block,
        letter: "R",
        il: IlForm::None,
    },
    Spec {
        mnemonic: "RT",
        family: Fb(FbName::Rt),
        operands: &[Bit],
        optional: 0,
        output: false,
        shape: Small,
        letter: "P",
        il: IlForm::None,
    },
    Spec {
        mnemonic: "R_TRIG",
        family: Fb(FbName::Rt),
        operands: &[Bit],
        optional: 0,
        output: false,
        shape: Small,
        letter: "P",
        il: IlForm::None,
    },
    Spec {
        mnemonic: "RF",
        family: Fb(FbName::Rf),
        operands: &[Bit],
        optional: 0,
        output: false,
        shape: Small,
        letter: "N",
        il: IlForm::None,
    },
    Spec {
        mnemonic: "F_TRIG",
        family: Fb(FbName::Rf),
        operands: &[Bit],
        optional: 0,
        output: false,
        shape: Small,
        letter: "N",
        il: IlForm::None,
    },
];

/// The row for an instruction name, case-insensitively. `None` means this
/// profile does not know the name.
pub fn spec(mnemonic: &str) -> Option<&'static Spec> {
    let mnemonic = mnemonic.trim();
    SPECS
        .iter()
        .find(|spec| spec.mnemonic.eq_ignore_ascii_case(mnemonic))
}

/// The mnemonics the IL language accepts, for the editor's help and for the
/// parser's rejection message. Block-only names are not listed.
pub fn il_mnemonics() -> impl Iterator<Item = &'static str> {
    SPECS
        .iter()
        .filter(|spec| spec.il != IlForm::None)
        .map(|spec| spec.mnemonic)
}

/// The IL action for a mnemonic, or `None` when this profile does not admit it.
pub fn il_form(mnemonic: &str) -> Option<IlForm> {
    spec(mnemonic).map(|spec| spec.il)
}

#[cfg(test)]
mod tests {
    use super::{il_mnemonics, spec, Family, IlForm, SPECS};

    #[test]
    fn every_mnemonic_is_unique_case_insensitively() {
        for (index, spec) in SPECS.iter().enumerate() {
            for other in &SPECS[index + 1..] {
                assert!(
                    !spec.mnemonic.eq_ignore_ascii_case(other.mnemonic),
                    "`{}` appears twice",
                    spec.mnemonic
                );
            }
        }
    }

    #[test]
    fn lookup_is_case_insensitive_and_trimmed() {
        assert_eq!(spec("ton").map(|spec| spec.mnemonic), Some("TON"));
        assert_eq!(spec("  GT ").map(|spec| spec.mnemonic), Some("GT"));
        assert!(spec("FROB").is_none());
    }

    #[test]
    fn the_il_language_is_the_table_rows_with_an_il_form() {
        let listed = il_mnemonics().count();
        assert!(listed > 0);
        for mnemonic in il_mnemonics() {
            assert_ne!(
                spec(mnemonic).map(|spec| spec.il),
                Some(IlForm::None),
                "{mnemonic}"
            );
        }
        // A block-only name is not an IL instruction.
        assert_eq!(spec("TON").map(|spec| spec.il), Some(IlForm::None));
        assert!(!il_mnemonics().any(|mnemonic| mnemonic == "TON"));
    }

    #[test]
    fn a_name_can_serve_a_block_and_the_accumulator() {
        let add = spec("ADD").expect("ADD");
        assert_eq!(add.family, Family::Math(super::MathName::Add));
        assert_eq!(add.il, IlForm::Word(super::WordName::Add));
        let gt = spec("GT").expect("GT");
        assert_eq!(gt.family, Family::Compare(super::CompareName::Gt));
        assert_eq!(gt.il, IlForm::Compare(super::CompareName::Gt));
    }

    #[test]
    fn only_coils_are_drawn_against_the_rail() {
        for spec in SPECS {
            assert_eq!(
                spec.output,
                matches!(spec.shape, super::Shape::Coil),
                "{}",
                spec.mnemonic
            );
        }
    }

    #[test]
    fn an_instruction_with_no_operands_is_exactly_the_no_operand_set() {
        let mut empty = SPECS
            .iter()
            .filter(|spec| !spec.takes_operand())
            .map(|spec| spec.mnemonic)
            .collect::<Vec<_>>();
        empty.sort_unstable();
        assert_eq!(empty, vec!["NOT", "RET", "RETC", "RETCN"]);
    }
}
