//! Instruction-list parsing and lowering.
//!
//! One source line holds one instruction. Comments start with `//`, a label is
//! `NAME:`, and a blank line is allowed anywhere. The dialect is the one a PLC
//! operator knows from IEC 61131-3: `LD`, `LDN`, `AND`, `ANDN`, `OR`, `ORN`,
//! `XOR`, `XORN`, `NOT`, `ST`, `STN`, `S`, `R` and `=`.
//!
//! Function blocks and POU calls (`CAL`), jumps and labels are parsed and then
//! refused with a named diagnostic rather than silently ignored.

use crate::compile::Op;

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

/// Mnemonics this compiler can lower into the scan VM.
pub const SUPPORTED: [&str; 25] = [
    "LD", "LDN", "AND", "ANDN", "OR", "ORN", "XOR", "XORN", "NOT", "ST", "STN", "S", "R", "=",
    "ADD", "SUB", "MUL", "DIV", "MOD", "GT", "GE", "EQ", "NE", "LE", "CAL",
];

/// Mnemonics that need a feature this compiler does not have yet.
pub const UNSUPPORTED: [&str; 8] = [
    "CALC", "CALCN", "JMP", "JMPC", "JMPCN", "RET", "RETC", "RETCN",
];

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
    if !SUPPORTED.contains(&mnemonic.as_str()) && !UNSUPPORTED.contains(&mnemonic.as_str()) {
        return IlLine::Bad(format!("unknown instruction '{mnemonic}'"));
    }
    if mnemonic != "NOT" && operand.is_empty() {
        return IlLine::Bad(format!("'{mnemonic}' needs an operand"));
    }
    if mnemonic == "NOT" && !operand.is_empty() {
        return IlLine::Bad("'NOT' takes no operand".to_string());
    }
    IlLine::Instruction { mnemonic, operand }
}

/// Append the ops for one instruction. `index` is the resolved symbol slot.
/// What an IL operand resolved to: a bit slot, a word slot, or a literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IlOperand {
    Bit(u16),
    Word(u16),
    Literal(i16),
}

/// Lower one instruction. `false` means this compiler has no such operation for
/// that operand kind, and validation reports it at the source line.
pub fn lower_line(mnemonic: &str, operand: IlOperand, ops: &mut Vec<Op>) -> bool {
    use IlOperand::{Bit, Literal, Word};

    // Word instructions: the operand is a word or a literal.
    match (mnemonic, operand) {
        ("LD", Word(word)) => {
            ops.push(Op::LoadWord(word));
            return true;
        }
        ("LD", Literal(value)) => {
            ops.push(Op::LoadConst(value));
            return true;
        }
        ("ST", Word(word)) => {
            ops.push(Op::StoreWord(word));
            return true;
        }
        ("ADD", Word(_)) | ("ADD", Literal(_)) => {
            push_operand(operand, ops);
            ops.push(Op::Add);
            return true;
        }
        ("SUB", Word(_)) | ("SUB", Literal(_)) => {
            push_operand(operand, ops);
            ops.push(Op::Sub);
            return true;
        }
        ("MUL", Word(_)) | ("MUL", Literal(_)) => {
            push_operand(operand, ops);
            ops.push(Op::Mul);
            return true;
        }
        ("DIV", Word(_)) | ("DIV", Literal(_)) => {
            push_operand(operand, ops);
            ops.push(Op::Div);
            return true;
        }
        ("MOD", Word(_)) | ("MOD", Literal(_)) => {
            push_operand(operand, ops);
            ops.push(Op::Mod);
            return true;
        }
        ("GT", Word(_)) | ("GT", Literal(_)) => {
            push_operand(operand, ops);
            ops.push(Op::CmpGt);
            return true;
        }
        ("GE", Word(_)) | ("GE", Literal(_)) => {
            push_operand(operand, ops);
            ops.push(Op::CmpGe);
            return true;
        }
        ("EQ", Word(_)) | ("EQ", Literal(_)) => {
            push_operand(operand, ops);
            ops.push(Op::CmpEq);
            return true;
        }
        ("NE", Word(_)) | ("NE", Literal(_)) => {
            push_operand(operand, ops);
            ops.push(Op::CmpNe);
            return true;
        }
        ("LE", Word(_)) | ("LE", Literal(_)) => {
            push_operand(operand, ops);
            ops.push(Op::CmpLe);
            return true;
        }
        ("LT", Word(_)) | ("LT", Literal(_)) => {
            push_operand(operand, ops);
            ops.push(Op::CmpLt);
            return true;
        }
        _ => {}
    }

    let Bit(index) = operand else {
        return false;
    };
    match mnemonic {
        "LD" => ops.push(Op::Push(index)),
        "LDN" => {
            ops.push(Op::Push(index));
            ops.push(Op::Not);
        }
        "AND" => {
            ops.push(Op::Push(index));
            ops.push(Op::And);
        }
        "ANDN" => {
            ops.push(Op::Push(index));
            ops.push(Op::Not);
            ops.push(Op::And);
        }
        "OR" => {
            ops.push(Op::Push(index));
            ops.push(Op::Or);
        }
        "ORN" => {
            ops.push(Op::Push(index));
            ops.push(Op::Not);
            ops.push(Op::Or);
        }
        "XOR" => {
            ops.push(Op::Push(index));
            ops.push(Op::Xor);
        }
        "XORN" => {
            ops.push(Op::Push(index));
            ops.push(Op::Not);
            ops.push(Op::Xor);
        }
        "NOT" => ops.push(Op::Not),
        "ST" | "=" => ops.push(Op::Store(index)),
        "STN" => {
            ops.push(Op::Not);
            ops.push(Op::Store(index));
        }
        "S" => ops.push(Op::Set(index)),
        "R" => ops.push(Op::Reset(index)),
        _ => return false,
    }
    true
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
    fn a_label_parses_and_is_refused_later() {
        assert_eq!(parse_line("LOOP:"), IlLine::Label("LOOP".into()));
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
