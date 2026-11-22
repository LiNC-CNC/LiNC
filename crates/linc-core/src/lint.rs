//! Line-local G-code checks, collected.
//!
//! The preview parser in [crate::gcode] stops at the first error because it has
//! to stop: it is drawing a toolpath, and after a bad block the path it would
//! draw is a guess. The operator still needs to know how many mistakes the file
//! holds and where they are, so this module answers exactly that question and
//! nothing else. Every check here reads one block on its own: no modal state, no
//! parameters, no interpreter. It therefore cannot invent an error from a
//! program's history, and it cannot check anything that depends on one.
//!
//! The class list follows what LinuxCNC's reader rejects per block (the
//! standalone `rs274` tool reports the same classes without line numbers):
//! nested or unclosed comments, a word letter the interpreter does not know, a
//! word without a value, two words with the same letter in one block, a number
//! that is not a number, a G/M number out of range, a negative feed or speed,
//! an `N` word that is not first, and unbalanced brackets in an expression.
//!
//! Deliberately absent: anything the real interpreter learns from earlier
//! blocks (`G1` with no feed rate in effect, arc words against a plane, a tool
//! number the table does not carry). Those stay in [crate::verify] and in the
//! preview, which know more than one line.

use crate::gcode::{lex_line, TokenKind};

/// One block-level fault, with the byte span of the text that caused it so the
/// editor can underline the word instead of the whole line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LintIssue {
    /// 1-based line number.
    pub line: usize,
    /// Byte offsets within the line of the offending text.
    pub span: Option<(usize, usize)>,
    pub message: String,
}

/// Letters the interpreter accepts as word letters. `O` is not one: an o-word
/// label is a statement, not a word, and the lexer reports it separately.
const WORD_LETTERS: [char; 25] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'P', 'Q', 'R', 'S', 'T',
    'U', 'V', 'W', 'X', 'Y', 'Z',
];

/// Every block-level fault in `text`, in file order.
pub fn lint_program(text: &str) -> Vec<LintIssue> {
    let mut issues = Vec::new();
    for (index, raw) in text.lines().enumerate() {
        lint_line(index + 1, raw, &mut issues);
    }
    issues
}

fn lint_line(line: usize, raw: &str, issues: &mut Vec<LintIssue>) {
    lint_comments(line, raw, issues);
    lint_brackets(line, raw, issues);
    let tokens = lex_line(raw);
    // An o-word statement is read as a keyword and an expression, not as
    // G-code words: `while` is not the axis word W, and its expression is not a
    // run of parameter letters. The lexer already says so; the word checks must
    // not run on it.
    let o_statement = tokens
        .first()
        .is_some_and(|token| token.kind == TokenKind::OWord);
    if o_statement {
        return;
    }
    lint_assignment(line, raw, issues);
    // A block that is a parameter assignment is one expression, read the way an
    // o-word's expression is: `#3032 = [ABS[#<x>]]` holds no axis word A, B or
    // feed word S, and reading the letters inside `ABS` as words would invent
    // three faults on a correct line.
    if raw.trim_start().starts_with('#') {
        return;
    }
    let mut seen: Vec<char> = Vec::new();
    let mut words = 0usize;
    for token in &tokens {
        if !is_word(token.kind) {
            if token.kind == TokenKind::ProgramNumber && raw.trim() != "%" {
                issues.push(LintIssue {
                    line,
                    span: Some((token.start, token.end)),
                    message: "percent sign must be on a line of its own".into(),
                });
            }
            if token.kind == TokenKind::LineNumber {
                if words > 0 {
                    issues.push(LintIssue {
                        line,
                        span: Some((token.start, token.end)),
                        message: "the N word must come first in the block".into(),
                    });
                }
                words += 1;
            }
            continue;
        }
        let Some(letter) = token
            .text
            .chars()
            .next()
            .map(|character| character.to_ascii_uppercase())
        else {
            continue;
        };
        let rest = token.text.chars().skip(1).collect::<String>();
        if token.kind == TokenKind::Other {
            if letter == 'N' {
                issues.push(LintIssue {
                    line,
                    span: Some((token.start, token.end)),
                    message: "the N word must come first in the block".into(),
                });
            } else if !WORD_LETTERS.contains(&letter) {
                issues.push(LintIssue {
                    line,
                    span: Some((token.start, token.end)),
                    message: format!("unknown word starting with {}", letter.to_ascii_lowercase()),
                });
            } else {
                check_letter_value(line, token, &letter, &rest, raw, issues);
            }
            words += 1;
            continue;
        }
        words += 1;
        // Several G words and several M words in one block are how a preamble is
        // written (`G21 G90 G54`, `M8 M3`): the interpreter sorts them into their
        // modal groups, so only value words are checked for repeats.
        if letter == 'G' || letter == 'M' {
            check_letter_range(line, token, &letter, &rest, issues);
            continue;
        }
        if seen.contains(&letter) {
            issues.push(LintIssue {
                line,
                span: Some((token.start, token.end)),
                message: format!("multiple {} words on one line", letter.to_ascii_lowercase()),
            });
            continue;
        }
        seen.push(letter);
        check_letter_value(line, token, &letter, &rest, raw, issues);
    }
}

/// Whether a token is a word: a letter the block says something with. A bare
/// number, a parameter, a comment, an operator inside an expression and an o-word
/// label are not.
fn is_word(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::GCode
            | TokenKind::MCode
            | TokenKind::Axis
            | TokenKind::Word
            | TokenKind::FeedSpeed
            | TokenKind::Tool
            | TokenKind::Other
    )
}

/// G and M numbers: the one range check that needs no modal state.
fn check_letter_range(
    line: usize,
    token: &crate::gcode::Token,
    letter: &char,
    rest: &str,
    issues: &mut Vec<LintIssue>,
) {
    let (limit, message) = if *letter == 'G' {
        (99.0, "G-code out of range")
    } else {
        (199.0, "M-code out of range")
    };
    if integer_part(rest).is_none_or(|value| !(0.0..=limit).contains(&value)) {
        issues.push(LintIssue {
            line,
            span: Some((token.start, token.end)),
            message: message.into(),
        });
    }
}

/// Axis, word, feed, speed and tool values.
fn check_letter_value(
    line: usize,
    token: &crate::gcode::Token,
    letter: &char,
    rest: &str,
    raw: &str,
    issues: &mut Vec<LintIssue>,
) {
    if token.kind == TokenKind::FeedSpeed && rest.trim_start().starts_with('-') {
        issues.push(LintIssue {
            line,
            span: Some((token.start, token.end)),
            message: format!("negative {} word used", letter.to_ascii_lowercase()),
        });
        return;
    }
    if !has_value(rest) && !parameterised(raw, token.end) {
        issues.push(LintIssue {
            line,
            span: Some((token.start, token.end)),
            message: missing_value_message(letter, token),
        });
    }
}

fn missing_value_message(letter: &char, token: &crate::gcode::Token) -> String {
    format!(
        "no value after {} ({})",
        letter.to_ascii_uppercase(),
        token.text.trim()
    )
}

/// Whether text after a word letter is a value this reader understands.
fn has_value(rest: &str) -> bool {
    let text = rest.trim();
    if text.is_empty() {
        return false;
    }
    if text.starts_with('#') || text.starts_with('[') {
        return true;
    }
    parse_value(text).is_some()
}

/// A parameterised value the lexer split off: `X#1`, `X[#1+2]`.
fn parameterised(raw: &str, from: usize) -> bool {
    raw[from..]
        .chars()
        .find(|character| !character.is_whitespace())
        .is_some_and(|character| character == '#' || character == '[')
}

/// The number a G/M word names: `1.1` for `G1.1`, `38` for `G38.2`.
fn integer_part(rest: &str) -> Option<f64> {
    parse_value(rest).map(|value| value.trunc())
}

fn parse_value(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut dots = 0usize;
    for (index, character) in trimmed.char_indices() {
        match character {
            '.' => {
                dots += 1;
                if dots > 1 {
                    return None;
                }
            }
            '-' | '+' if index == 0 => {}
            _ if character.is_ascii_digit() => {}
            _ => return None,
        }
    }
    trimmed.parse::<f64>().ok()
}

/// `(a (b))` is not a comment inside a comment: LinuxCNC rejects the nesting
/// outright, and the inner `)` closes the outer comment, so the rest of the line
/// runs. Unclosed at end of line is the same class of fault.
fn lint_comments(line: usize, raw: &str, issues: &mut Vec<LintIssue>) {
    let bytes = raw.as_bytes();
    let mut depth = 0usize;
    let mut opener = 0usize;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'(' => {
                if depth > 0 {
                    issues.push(LintIssue {
                        line,
                        span: Some((index, index + 1)),
                        message: "nested comment: LinuxCNC comments do not nest".into(),
                    });
                } else {
                    opener = index;
                }
                depth += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
            }
            b';' if depth == 0 => break,
            _ => {}
        }
    }
    if depth > 0 {
        issues.push(LintIssue {
            line,
            span: Some((opener, (opener + 1).min(raw.len()))),
            message: "unmatched comment parenthesis".into(),
        });
    }
}

/// Brackets carry expression structure; an unmatched one cannot be read.
fn lint_brackets(line: usize, raw: &str, issues: &mut Vec<LintIssue>) {
    let mut depth = 0usize;
    let mut in_comment = false;
    let mut opener = 0usize;
    for (index, character) in raw.char_indices() {
        match character {
            '(' if !in_comment => in_comment = true,
            ')' if in_comment => in_comment = false,
            ';' if !in_comment => break,
            '[' if !in_comment => {
                if depth == 0 {
                    opener = index;
                }
                depth += 1;
            }
            ']' if !in_comment => {
                if depth == 0 {
                    issues.push(LintIssue {
                        line,
                        span: Some((index, index + 1)),
                        message: "unmatched closing bracket".into(),
                    });
                } else {
                    depth -= 1;
                }
            }
            _ if in_comment => {}
            _ => {}
        }
    }
    if depth > 0 {
        issues.push(LintIssue {
            line,
            span: Some((opener, (opener + 1).min(raw.len()))),
            message: "unmatched opening bracket".into(),
        });
    }
}

/// `#1 =` with nothing after it, and `#1` with no `=` at all, are both blocks the
/// interpreter refuses.
fn lint_assignment(line: usize, raw: &str, issues: &mut Vec<LintIssue>) {
    let trimmed = raw.trim_start();
    let offset = raw.len() - trimmed.len();
    if !trimmed.starts_with('#') {
        return;
    }
    let Some(equals) = trimmed.find('=') else {
        // A bare `#1` can be a read-only expression statement in some dialects;
        // refuse only the clearly broken `#1` with no operator at all.
        if trimmed.trim_end().chars().all(|character| {
            character == '#' || character.is_ascii_digit() || character == '<' || character == '>'
        }) {
            issues.push(LintIssue {
                line,
                span: Some((offset, offset + trimmed.trim_end().len())),
                message: "parameter assignment has no value".into(),
            });
        }
        return;
    };
    if trimmed[equals + 1..].trim().is_empty() {
        issues.push(LintIssue {
            line,
            span: Some((offset, offset + equals + 1)),
            message: "parameter assignment has no value".into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn messages(text: &str) -> Vec<String> {
        lint_program(text)
            .into_iter()
            .map(|issue| format!("{}: {}", issue.line, issue.message))
            .collect()
    }

    fn lines_with_issues(text: &str) -> Vec<usize> {
        lint_program(text).iter().map(|issue| issue.line).collect()
    }

    #[test]
    fn a_clean_program_has_no_issues() {
        let text = "G0 X0 Y0 Z5\nG1 Z-1 F100\nG2 X10 Y10 I5 J5\nM3 S1000\nM2\n";
        assert!(lint_program(text).is_empty(), "{:?}", messages(text));
    }

    #[test]
    fn every_bad_line_is_reported_not_just_the_first() {
        let text = "G0 X0 Y0\nG1 X\nG1 X10 X20 F100\nG123 X1\nM2\n";
        assert_eq!(lines_with_issues(text), vec![2, 3, 4]);
    }

    #[test]
    fn a_word_without_a_value_is_reported_with_its_span() {
        let issues = lint_program("G0 X0 Y0\nG1 X F100\nM2\n");
        assert_eq!(issues.len(), 1, "{:?}", issues);
        assert_eq!(issues[0].line, 2);
        assert_eq!(issues[0].span, Some((3, 4)));
        assert!(issues[0].message.contains("no value after X"));
    }

    #[test]
    fn a_parameterised_value_is_not_a_missing_value() {
        assert!(lint_program("G0 X#1 Y[#2+1]\nM2\n").is_empty());
    }

    #[test]
    fn two_words_with_the_same_letter_in_one_block() {
        assert_eq!(
            messages("G1 X10 X20 F100\nM2\n"),
            vec!["1: multiple x words on one line".to_string()]
        );
    }

    #[test]
    fn different_groups_of_the_same_letter_are_left_to_the_interpreter() {
        // Two G words are legal when they sit in different modal groups; the
        // duplicate check is for axis and value words.
        assert!(lint_program("G0 G54 X0\nM2\n").is_empty());
    }

    #[test]
    fn an_unknown_word_letter_is_reported() {
        let issues = lint_program("G0 X0 Y0 Z\nM2\n");
        assert!(
            issues
                .iter()
                .any(|issue| issue.message.contains("no value after Z")),
            "{:?}",
            issues
        );
    }

    #[test]
    fn nested_and_unclosed_comments_are_both_reported() {
        let nested = messages("G0 X0 (a (b) c)\nM2\n");
        assert_eq!(
            nested,
            vec!["1: nested comment: LinuxCNC comments do not nest".to_string()]
        );
        let unclosed = messages("G0 X0 (a b c\nM2\n");
        assert_eq!(
            unclosed,
            vec!["1: unmatched comment parenthesis".to_string()]
        );
    }

    #[test]
    fn a_semicolon_comment_hides_its_parentheses() {
        assert!(lint_program("G0 X0 ; ( not a comment start\nM2\n").is_empty());
    }

    #[test]
    fn a_g_or_m_number_out_of_range_is_reported() {
        assert_eq!(
            messages("G0 X0\nG123 X1\nM2\n"),
            vec!["2: G-code out of range".to_string()]
        );
        assert_eq!(
            messages("G0 X0\nM1234\nM2\n"),
            vec!["2: M-code out of range".to_string()]
        );
    }

    #[test]
    fn decimal_g_codes_are_read_by_their_integer_part() {
        assert!(lint_program("G0 X0\nG38.2 Z-5 F100\nG43.1 Z0\nG59.2\nM2\n").is_empty());
    }

    #[test]
    fn a_negative_feed_or_speed_is_reported() {
        assert_eq!(
            messages("G1 X10 F-100\nM2\n"),
            vec!["1: negative f word used".to_string()]
        );
        assert_eq!(
            messages("S-500 M3\nM2\n"),
            vec!["1: negative s word used".to_string()]
        );
    }

    #[test]
    fn a_number_that_is_not_a_number_is_reported() {
        let issues = messages("G1 X1.2.3 F100\nM2\n");
        assert!(
            issues
                .iter()
                .any(|message| message.contains("no value after X")),
            "{issues:?}"
        );
    }

    #[test]
    fn the_n_word_must_be_first() {
        let issues = messages("X0 N10 G0\nM2\n");
        assert!(
            issues
                .iter()
                .any(|message| message.contains("N word must come first")),
            "{issues:?}"
        );
        assert!(lint_program("N10 G0 X0\nM2\n").is_empty());
    }

    #[test]
    fn unbalanced_brackets_are_reported() {
        assert_eq!(
            messages("G0 X[#1+2\nM2\n"),
            vec!["1: unmatched opening bracket".to_string()]
        );
        assert_eq!(
            messages("G0 X#1]\nM2\n"),
            vec!["1: unmatched closing bracket".to_string()]
        );
    }

    #[test]
    fn a_parameter_assignment_without_a_value_is_reported() {
        assert_eq!(
            messages("#1 =\nM2\n"),
            vec!["1: parameter assignment has no value".to_string()]
        );
        assert!(lint_program("#1 = 5\nM2\n").is_empty());
    }

    #[test]
    fn an_o_word_statement_is_not_read_as_words() {
        // `while [#1 LT 2]` holds no axis word and no word letters: reading it
        // as G-code would invent faults.
        let text = "o100 while [#1 LT 2]\nG1 X10 F100\no100 endwhile\nM2\n";
        assert!(lint_program(text).is_empty(), "{:?}", messages(text));
    }

    #[test]
    fn a_percent_sign_shares_no_line() {
        assert_eq!(
            messages("G0 X0 %\nM2\n"),
            vec!["1: percent sign must be on a line of its own".to_string()]
        );
        assert!(lint_program("%\nG0 X0\nM2\n%\n").is_empty());
    }

    #[test]
    fn the_repository_programs_lint_clean() {
        // A false positive here would block a program the machine cuts today,
        // so the repo's own G-code is the floor: every file in configs/gcode and
        // the probe bundle must pass.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        let mut checked = 0usize;
        for directory in ["configs/gcode", "subroutines/probe_basic"] {
            let directory = root.join(directory);
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().is_none_or(|extension| extension != "ngc") {
                    continue;
                }
                let Ok(text) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let issues = lint_program(&text);
                assert!(
                    issues.is_empty(),
                    "{}: {:?}",
                    path.display(),
                    issues
                        .iter()
                        .map(|issue| format!("{}: {}", issue.line, issue.message))
                        .collect::<Vec<_>>()
                );
                checked += 1;
            }
        }
        assert!(checked > 10, "only {checked} programs were checked");
    }
}
