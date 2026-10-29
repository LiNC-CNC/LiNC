//! The word-spacing rule shared by the text inputs that hold G-code.
//!
//! The MDI input and the program editor both insert a space before a word
//! letter (an axis, a feed or speed word, or a G/M command) when it is typed
//! into a run of non-whitespace, so `G1X10` reads `G1 X10` as the operator
//! types. Keeping the rule here makes both callers agree on exactly when the
//! space is inserted, and keeps the rewriting pure and testable without an
//! `egui::Context`.

/// Whether `c` opens a word that takes a space in front of it.
///
/// A value word (an axis, or a feed or speed word) or a command word (G or M).
/// Either case; letters arrive lowercase from the panel keyboard.
pub fn is_value_word(c: char) -> bool {
    matches!(
        c.to_ascii_uppercase(),
        'X' | 'Y' | 'Z' | 'A' | 'B' | 'C' | 'U' | 'V' | 'W' | 'F' | 'S' | 'G' | 'M'
    )
}

/// Whether `c` typed at character index `at` opens a new word and takes a
/// space in front of it.
///
/// Never at the start of the current line, never after a space, a tab or a
/// newline, and never inside a `(...)` comment or a `[...]` expression, where
/// the characters are prose or an expression rather than G-code words.
pub fn needs_space(text: &[char], at: usize, c: char) -> bool {
    if !is_value_word(c) || at == 0 || at > text.len() {
        return false;
    }
    match text.get(at - 1) {
        // The start of a line is as good as the start of the buffer: the first
        // word of a block takes no leading space.
        Some(' ') | Some('\t') | Some('\n') | None => false,
        Some(_) => !inside_comment_or_expression(&text[..at]),
    }
}

/// Whether the characters up to the caret leave a `(...)` comment or a
/// `[...]` expression open on the current line.
///
/// A newline acts like the start of the buffer: the editor is multiline, so a
/// block's first word takes no leading space and an open comment or expression
/// from an earlier block does not leak into the one being typed. The MDI input
/// is a single line, where this changes nothing.
pub fn inside_comment_or_expression(text: &[char]) -> bool {
    let (mut comment, mut expression) = (0i32, 0i32);
    for c in text {
        match c {
            '\n' => {
                comment = 0;
                expression = 0;
            }
            '(' => comment += 1,
            ')' => comment = (comment - 1).max(0),
            '[' => expression += 1,
            ']' => expression = (expression - 1).max(0),
            _ => {}
        }
    }
    comment > 0 || expression > 0
}

/// Insert the spaces the typed characters need, before the edit that inserts
/// them.
///
/// `text` is the buffer's characters, `caret` and `anchor` are the edit's
/// selection (both equal for a plain caret), and `typed` are the characters
/// the frame's `Event::Text` events carry. Returns the rewritten buffer and
/// the caret the edit should use, or `None` when no space is needed and the
/// caller should leave the buffer and cursor alone for egui's own insertion.
///
/// The selection is removed here — it is what the keystroke replaces — and the
/// space goes into the buffer before the character that will replace it, so
/// the caret handed back is already in the new buffer's coordinates: egui
/// inserts the typed characters at that caret, and its own cursor arithmetic
/// ends after the character that was typed. Normalising after the edit would
/// instead have to diff the old and new text to find where the insertion
/// happened, and then move a cursor the widget had already stored.
pub fn space_typed(
    text: &[char],
    caret: usize,
    anchor: usize,
    typed: &[char],
) -> Option<(Vec<char>, usize)> {
    let (caret, anchor) = (caret.min(text.len()), anchor.min(text.len()));
    let (start, end) = (caret.min(anchor), caret.max(anchor));
    // The selection is replaced, so it goes first: the space has to land
    // before the character that replaces it, not after the text being
    // replaced.
    let mut rewritten: Vec<char> = text[..start]
        .iter()
        .copied()
        .chain(text[end..].iter().copied())
        .collect();
    // `model` carries what the edit is about to insert, so the next character
    // in the same frame sees the right neighbour; `rewritten` only ever grows
    // by the spaces, so the caret handed back is already in the buffer's
    // coordinates.
    let mut model: Vec<char> = rewritten.clone();
    let mut at = start;
    let mut model_at = start;
    for &character in typed {
        if needs_space(&model, model_at, character) {
            rewritten.insert(at, ' ');
            model.insert(model_at, ' ');
            at += 1;
            model_at += 1;
        }
        model.insert(model_at, character);
        model_at += 1;
    }
    if rewritten.as_slice() == text {
        return None;
    }
    Some((rewritten, at))
}

#[cfg(test)]
mod tests {
    use super::{inside_comment_or_expression, is_value_word, needs_space, space_typed};

    fn chars(text: &str) -> Vec<char> {
        text.chars().collect()
    }

    /// Feed characters one at a time, the way the panel keyboard delivers them,
    /// and apply the returned caret the way egui does: the typed character is
    /// inserted at the shifted caret, so the buffer ends with the character
    /// just typed.
    fn type_line(start: &str, text: &str) -> (String, usize) {
        let mut buffer = chars(start);
        let mut caret = buffer.len();
        for c in text.chars() {
            match space_typed(&buffer, caret, caret, &[c]) {
                Some((rewritten, at)) => {
                    buffer = rewritten;
                    buffer.insert(at, c);
                    caret = at + 1;
                }
                None => {
                    buffer.insert(caret, c);
                    caret += 1;
                }
            }
        }
        (buffer.into_iter().collect(), caret)
    }

    #[test]
    fn typed_command_and_value_words_gain_a_space() {
        // `G1X10` splits before the value word; `X10G0` and `M3M5` split
        // before the command word that follows a run of digits.
        assert_eq!(type_line("", "G1X10"), ("G1 X10".to_string(), 6));
        assert_eq!(type_line("", "X10G0"), ("X10 G0".to_string(), 6));
        assert_eq!(type_line("", "M3M5"), ("M3 M5".to_string(), 5));
    }

    #[test]
    fn a_word_at_the_start_of_a_line_is_left_alone() {
        // Nothing before the first word, in the MDI input or on a fresh editor
        // line.
        assert_eq!(type_line("", "X10"), ("X10".to_string(), 3));
        assert_eq!(type_line("G1\n", "X10"), ("G1\nX10".to_string(), 6));
    }

    #[test]
    fn a_word_inside_a_comment_or_an_expression_is_left_alone() {
        // A G typed inside an open comment is prose, not a command.
        assert_eq!(type_line("", "(G"), ("(G".to_string(), 2));
        assert_eq!(type_line("", "[#1X"), ("[#1X".to_string(), 4));
        // Closed again, the rule applies once more.
        assert_eq!(type_line("", "(x)G"), ("(x) G".to_string(), 5));
    }

    #[test]
    fn the_caret_lands_after_the_character_that_was_typed() {
        // Typing the block one character per frame keeps the caret at the end,
        // after the last character typed, not before the space that was added.
        let mut buffer = chars("");
        let mut caret = 0;
        for c in "G1X10".chars() {
            match space_typed(&buffer, caret, caret, &[c]) {
                Some((rewritten, at)) => {
                    buffer = rewritten;
                    buffer.insert(at, c);
                    caret = at + 1;
                }
                None => {
                    buffer.insert(caret, c);
                    caret += 1;
                }
            }
        }
        assert_eq!(buffer, chars("G1 X10"));
        assert_eq!(caret, 6);
    }

    #[test]
    fn a_word_takes_a_space_only_where_a_word_starts() {
        // The brief's cases: `G1X10` and `X10Y5` split, an existing space left
        // alone.
        assert!(needs_space(&chars("G1"), 2, 'X'));
        assert!(needs_space(&chars("X10"), 3, 'Y'));
        assert!(!needs_space(&chars("M3 "), 3, 'S'));
        // Every letter the rule covers, either case: the value words and the
        // G/M command words.
        for letter in "XYZABCUVWFSGM".chars() {
            assert!(needs_space(&chars("G1"), 2, letter), "{letter}");
            assert!(
                needs_space(&chars("G1"), 2, letter.to_ascii_lowercase()),
                "{letter}"
            );
        }
        // Nothing at the start of a line, and nothing after a space, a tab or
        // a newline.
        assert!(!needs_space(&chars(""), 0, 'X'));
        assert!(!needs_space(&chars("G1 "), 3, 'X'));
        assert!(!needs_space(&chars("G1\t"), 3, 'X'));
        assert!(!needs_space(&chars("G1\n"), 3, 'X'));
        assert!(!needs_space(&chars("G1\n"), 3, 'G'));
        assert!(!needs_space(&chars("G1\n\n"), 4, 'M'));
        // Prose and expressions are left exactly as typed.
        assert!(!needs_space(&chars("G1 (move"), 8, 'X'));
        assert!(!needs_space(&chars("G1 [#1+2"), 8, 'X'));
        assert!(!inside_comment_or_expression(&chars("G1 (a) ")));
        assert!(needs_space(&chars("G1 (a)"), 6, 'X'));
        // A comment closed on an earlier line does not leak into this one, and
        // the count restarts after the newline.
        assert!(!inside_comment_or_expression(&chars("G1 (a)\nG1")));
        assert!(needs_space(&chars("G1 (a)\nG1"), 8, 'X'));
        // Letters that do not open a word, and a caret past the end.
        assert!(!needs_space(&chars("G1"), 2, '1'));
        assert!(!needs_space(&chars("G1"), 9, 'X'));
        assert!(is_value_word('G'));
        assert!(is_value_word('m'));
        assert!(!is_value_word('1'));
    }
}
