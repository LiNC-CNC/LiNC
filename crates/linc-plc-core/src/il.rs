//! Instruction-list parsing and lowering.
//!
//! One source line holds one instruction. Comments start with `//`, a label is
//! `NAME:`, and a blank line is allowed anywhere. The dialect is the one a PLC
//! operator knows from IEC 61131-3: `LD`, `LDN`, `AND`, `ANDN`, `OR`, `ORN`,
//! `XOR`, `XORN`, `NOT`, `ST`, `STN`, `S`, `R` and `=`.
//!
//! Function blocks and POU calls (`CAL`), jumps and labels are parsed and then
//! refused with a named diagnostic rather than silently ignored.

use crate::compile::{compare_op_for, Op};
use crate::profile::spec::{IlForm, Spec, WordName};

/// One parsed source line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IlLine {
    Blank,
    Comment,
    /// `LOOP:` — a jump target. Accepted by the parser, refused by the compiler.
    Label(String),
    Instruction {
        mnemonic: String,
        operand: String,
    },
    /// A line the parser could not read at all.
    Bad(String),
}

/// Parse a function-block call: `TON0(IN:=start, PT:=T#4s)`.
pub fn parse_call(operand: &str) -> Option<(String, Vec<(String, String)>)> {
    let text = operand.trim();
    let open = text.find('(')?;
    let close = text.rfind(')')?;
    if close < open {
        return None;
    }
    let name = text[..open].trim().to_string();
    if name.is_empty() {
        return None;
    }
    let mut args = Vec::new();
    for part in text[open + 1..close].split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (key, value) = part.split_once(":=")?;
        args.push((key.trim().to_ascii_uppercase(), value.trim().to_string()));
    }
    Some((name, args))
}

/// A word operand of the `MWn` bank, as the compiler resolves it.
pub fn word_operand(operand: &str) -> Option<u16> {
    let text = operand.trim();
    let bare = text.strip_prefix('%').unwrap_or(text);
    let digits = bare
        .strip_prefix("MW")
        .or_else(|| bare.strip_prefix("mw"))?;
    let index = digits.parse::<u16>().ok()?;
    (index < crate::compile::WORD_COUNT).then_some(index)
}

pub fn parse_line(line: &str) -> IlLine {
    let text = match line.find("//") {
        Some(index) => {
            if line[..index].trim().is_empty() {
                return IlLine::Comment;
            }
            &line[..index]
        }
        None => line,
    };
    let text = text.trim();
    if text.is_empty() {
        return IlLine::Blank;
    }
    if text.ends_with(':') && !text.contains(char::is_whitespace) {
        return IlLine::Label(text.trim_end_matches(':').to_string());
    }
    let (mnemonic, operand) = match text.split_once(char::is_whitespace) {
        Some((head, tail)) => (head, tail.trim().to_string()),
        None => (text, String::new()),
    };
    // `AND(` is IEC's guarded form; `)` closes it. Neither is in the VM.
    if mnemonic.ends_with('(') || mnemonic == ")" {
        return IlLine::Bad(format!("'{mnemonic}' is not supported by this compiler"));
    }
    let mnemonic = mnemonic.to_ascii_uppercase();
    // The instruction table is the only place a mnemonic is spelled; this parser
    // rejects a name by its absence from the profile, not by its own list.
    let Some(spec) = crate::profile::spec::spec(&mnemonic) else {
        return IlLine::Bad(format!("unknown instruction '{mnemonic}'"));
    };
    if spec.takes_operand() && operand.is_empty() {
        return IlLine::Bad(format!("'{mnemonic}' needs an operand"));
    }
    if !spec.takes_operand() && !operand.is_empty() {
        return IlLine::Bad(format!("'{mnemonic}' takes no operand"));
    }
    IlLine::Instruction { mnemonic, operand }
}

/// Append the ops for one instruction. `index` is the resolved symbol slot.
/// What an IL operand resolved to: a bit slot, a word slot, or a literal.
///
/// `Word` is a slot in the word bank, so `MWn` and a declared word point share
/// one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IlOperand {
    Bit(u16),
    Word(u16),
    Literal(i32),
}

/// Lower one instruction from its table row. `false` means this compiler has no
/// operation for that row and operand kind, and validation reports it at the
/// source line.
///
/// The row decides what the instruction is, so a mnemonic is never matched here:
/// the same `IlForm` is what `validate.rs` classifies for its type flow.
pub fn lower_line(spec: &Spec, operand: IlOperand, ops: &mut Vec<Op>) -> bool {
    use IlOperand::{Bit, Literal, Word};

    // The word forms first: the operand decides whether a word slot or a
    // literal is pushed.
    match (spec.il, operand) {
        (IlForm::Push, Word(word)) => {
            ops.push(Op::LoadWord(word));
            return true;
        }
        (IlForm::Push, Literal(value)) => {
            ops.push(Op::LoadConst(value));
            return true;
        }
        (IlForm::Store, Word(word)) => {
            ops.push(Op::StoreWord(word));
            return true;
        }
        (IlForm::Word(name), Word(_) | Literal(_)) => {
            push_operand(operand, ops);
            ops.push(word_op(name));
            return true;
        }
        (IlForm::Compare(name), Word(_) | Literal(_)) => {
            push_operand(operand, ops);
            ops.push(compare_op_for(name));
            return true;
        }
        _ => {}
    }

    let Bit(index) = operand else {
        return false;
    };
    match spec.il {
        IlForm::Push => ops.push(Op::Push(index)),
        IlForm::PushNot => {
            ops.push(Op::Push(index));
            ops.push(Op::Not);
        }
        IlForm::And => {
            ops.push(Op::Push(index));
            ops.push(Op::And);
        }
        IlForm::AndNot => {
            ops.push(Op::Push(index));
            ops.push(Op::Not);
            ops.push(Op::And);
        }
        IlForm::Or => {
            ops.push(Op::Push(index));
            ops.push(Op::Or);
        }
        IlForm::OrNot => {
            ops.push(Op::Push(index));
            ops.push(Op::Not);
            ops.push(Op::Or);
        }
        IlForm::Xor => {
            ops.push(Op::Push(index));
            ops.push(Op::Xor);
        }
        IlForm::XorNot => {
            ops.push(Op::Push(index));
            ops.push(Op::Not);
            ops.push(Op::Xor);
        }
        IlForm::Not => ops.push(Op::Not),
        IlForm::Store => ops.push(Op::Store(index)),
        IlForm::StoreNot => {
            ops.push(Op::Not);
            ops.push(Op::Store(index));
        }
        IlForm::Set => ops.push(Op::Set(index)),
        IlForm::Reset => ops.push(Op::Reset(index)),
        _ => return false,
    }
    true
}

fn word_op(name: WordName) -> Op {
    match name {
        WordName::Add => Op::Add,
        WordName::Sub => Op::Sub,
        WordName::Mul => Op::Mul,
        WordName::Div => Op::Div,
        WordName::Mod => Op::Mod,
    }
}

fn push_operand(operand: IlOperand, ops: &mut Vec<Op>) {
    match operand {
        IlOperand::Word(word) => ops.push(Op::LoadWord(word)),
        IlOperand::Literal(value) => ops.push(Op::LoadConst(value)),
        IlOperand::Bit(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_comment_and_a_blank_line_are_not_instructions() {
        assert_eq!(parse_line("// pump"), IlLine::Comment);
        assert_eq!(parse_line("   "), IlLine::Blank);
    }

    #[test]
    fn an_instruction_keeps_its_mnemonic_upper_case() {
        assert_eq!(
            parse_line("ld start // seal"),
            IlLine::Instruction {
                mnemonic: "LD".into(),
                operand: "start".into()
            }
        );
    }

    #[test]
    fn a_label_parses_as_its_own_line() {
        assert_eq!(parse_line("LOOP:"), IlLine::Label("LOOP".into()));
    }

    #[test]
    fn the_jump_and_call_instructions_parse() {
        for line in [
            "JMP LOOP",
            "JMPC LOOP",
            "JMPCN LOOP",
            "CALC POU2",
            "CALCN POU2",
        ] {
            assert!(
                matches!(parse_line(line), IlLine::Instruction { .. }),
                "{line}"
            );
        }
        assert!(matches!(parse_line("RETC"), IlLine::Instruction { .. }));
        assert!(matches!(parse_line("RETCN"), IlLine::Instruction { .. }));
        assert!(matches!(parse_line("JMP"), IlLine::Bad(_)));
        assert!(matches!(parse_line("RETC now"), IlLine::Bad(_)));
    }

    #[test]
    fn an_unknown_mnemonic_is_reported_with_its_spelling() {
        assert_eq!(
            parse_line("FROB start"),
            IlLine::Bad("unknown instruction 'FROB'".into())
        );
    }

    #[test]
    fn an_operand_is_required_except_for_not() {
        assert!(matches!(parse_line("AND"), IlLine::Bad(_)));
        assert!(matches!(parse_line("NOT start"), IlLine::Bad(_)));
        assert!(matches!(parse_line("NOT"), IlLine::Instruction { .. }));
    }
}
