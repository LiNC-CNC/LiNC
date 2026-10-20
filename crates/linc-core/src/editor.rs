//! Editor buffer and line operations for the QMK keyboard.
//!
//! The buffer is a `ropey::Rope`; the UI renders a temporary string through
//! egui's `TextEdit` and feeds changes back here. Undo/redo snapshots are
//! bounded so a long editing session cannot grow without limit.

use std::path::PathBuf;

use ropey::Rope;

const UNDO_LIMIT: usize = 100;

#[derive(Clone, Debug)]
pub struct Editor {
    rope: Rope,
    pub path: Option<PathBuf>,
    pub dirty: bool,
    pub revision: u64,
    undo: Vec<(Rope, bool)>,
    redo: Vec<(Rope, bool)>,
    last_saved_text: String,
    pub search: String,
    pub search_direction_forward: bool,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new()
    }
}

impl Editor {
    pub fn new() -> Self {
        Self {
            rope: Rope::new(),
            path: None,
            dirty: false,
            revision: 0,
            undo: Vec::new(),
            redo: Vec::new(),
            last_saved_text: String::new(),
            search: String::new(),
            search_direction_forward: true,
        }
    }

    pub fn from_text(path: Option<PathBuf>, text: &str) -> Self {
        let mut editor = Self::new();
        editor.rope = Rope::from_str(text);
        editor.path = path;
        editor.last_saved_text = text.to_string();
        editor.revision = 1;
        editor
    }

    pub fn text(&self) -> String {
        self.rope.to_string()
    }

    pub fn line_count(&self) -> usize {
        self.rope.len_lines().max(1)
    }

    pub fn line(&self, line_1based: usize) -> String {
        if line_1based == 0 || line_1based > self.rope.len_lines() {
            return String::new();
        }
        self.rope
            .line(line_1based - 1)
            .to_string()
            .trim_end_matches(['\n', '\r'])
            .to_string()
    }

    pub fn line_char_start(&self, line_1based: usize) -> Option<usize> {
        if line_1based == 0 || line_1based > self.rope.len_lines() {
            None
        } else {
            Some(self.rope.line_to_char(line_1based - 1))
        }
    }

    /// Replace the whole buffer after a UI edit. Text equal to the current
    /// content is ignored so key events that do not change the buffer do not
    /// pollute undo history.
    pub fn replace_text(&mut self, text: String) -> bool {
        if text == self.rope.to_string() {
            return false;
        }
        self.push_undo();
        self.rope = Rope::from_str(&text);
        self.revision += 1;
        self.dirty = text != self.last_saved_text;
        true
    }

    /// Used after a successful save; the next edits are compared to this.
    pub fn mark_saved(&mut self) {
        self.last_saved_text = self.rope.to_string();
        self.dirty = false;
    }

    pub fn set_loaded(&mut self, path: PathBuf, text: String) {
        self.rope = Rope::from_str(&text);
        self.path = Some(path);
        self.last_saved_text = text;
        self.dirty = false;
        self.revision += 1;
        self.undo.clear();
        self.redo.clear();
    }

    pub fn set_saved_text(&mut self, text: String) {
        self.last_saved_text = text;
        self.dirty = self.rope.to_string() != self.last_saved_text;
    }

    fn push_undo(&mut self) {
        self.undo.push((self.rope.clone(), self.dirty));
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    pub fn undo(&mut self) -> bool {
        let Some((previous, dirty)) = self.undo.pop() else {
            return false;
        };
        self.redo.push((self.rope.clone(), self.dirty));
        self.rope = previous;
        self.dirty = dirty;
        self.revision += 1;
        true
    }

    pub fn redo(&mut self) -> bool {
        let Some((next, dirty)) = self.redo.pop() else {
            return false;
        };
        self.undo.push((self.rope.clone(), self.dirty));
        self.rope = next;
        self.dirty = dirty;
        self.revision += 1;
        true
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn duplicate_line(&mut self, line_1based: usize) -> Option<usize> {
        if line_1based == 0 || line_1based > self.rope.len_lines() {
            return None;
        }
        self.push_undo();
        let (start, end) = self.line_char_range(line_1based);
        let line_text = self.rope.slice(start..end).to_string();
        self.rope.insert(end, &line_text);
        self.revision += 1;
        self.dirty = true;
        Some(line_1based + 1)
    }

    pub fn delete_line(&mut self, line_1based: usize) -> Option<usize> {
        if line_1based == 0 || line_1based > self.rope.len_lines() {
            return None;
        }
        self.push_undo();
        let (start, end) = self.line_char_range(line_1based);
        if end > start {
            self.rope.remove(start..end);
        }
        if self.rope.len_chars() == 0 {
            self.rope.insert_char(0, '\n');
        }
        self.revision += 1;
        self.dirty = true;
        Some(line_1based.min(self.rope.len_lines()))
    }

    pub fn move_line_up(&mut self, line_1based: usize) -> Option<usize> {
        if line_1based <= 1 || line_1based > self.rope.len_lines() {
            return None;
        }
        self.push_undo();
        let (prev_start, prev_end) = self.line_char_range(line_1based - 1);
        let (start, end) = self.line_char_range(line_1based);
        let prev = self.rope.slice(prev_start..prev_end).to_string();
        let current = self.rope.slice(start..end).to_string();
        self.rope.remove(prev_start..end);
        self.rope.insert(prev_start, &current);
        self.rope
            .insert(prev_start + current.chars().count(), &prev);
        self.revision += 1;
        self.dirty = true;
        Some(line_1based - 1)
    }

    pub fn move_line_down(&mut self, line_1based: usize) -> Option<usize> {
        if line_1based == 0 || line_1based >= self.rope.len_lines() {
            return None;
        }
        self.push_undo();
        let (start, end) = self.line_char_range(line_1based);
        let (next_start, next_end) = self.line_char_range(line_1based + 1);
        let current = self.rope.slice(start..end).to_string();
        let next = self.rope.slice(next_start..next_end).to_string();
        self.rope.remove(start..next_end);
        self.rope.insert(start, &next);
        self.rope.insert(start + next.chars().count(), &current);
        self.revision += 1;
        self.dirty = true;
        Some(line_1based + 1)
    }

    fn line_char_range(&self, line_1based: usize) -> (usize, usize) {
        let start = self.rope.line_to_char(line_1based - 1);
        let end = if line_1based < self.rope.len_lines() {
            self.rope.line_to_char(line_1based)
        } else {
            self.rope.len_chars()
        };
        (start, end)
    }

    /// Search forward from a 1-based line. Returns the matching line and the
    /// character range within the line.
    pub fn find(&self, needle: &str, from_line: usize) -> Option<(usize, usize, usize)> {
        if needle.is_empty() {
            return None;
        }
        let needle_lower = needle.to_ascii_lowercase();
        let start_line = from_line.max(1);
        let lines = self.rope.len_lines();
        for offset in 0..lines {
            let line_no = ((start_line - 1 + offset) % lines) + 1;
            let line_text = self.line(line_no);
            if let Some(byte_start) = line_text.to_ascii_lowercase().find(&needle_lower) {
                let char_start = line_text[..byte_start].chars().count();
                return Some((line_no, char_start, char_start + needle.chars().count()));
            }
        }
        None
    }

    pub fn find_all(&self, needle: &str) -> Vec<usize> {
        if needle.is_empty() {
            return Vec::new();
        }
        let needle_lower = needle.to_ascii_lowercase();
        (1..=self.line_count())
            .filter(|line_no| {
                self.line(*line_no)
                    .to_ascii_lowercase()
                    .contains(&needle_lower)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_marks_dirty_and_supports_undo_redo() {
        let mut editor = Editor::from_text(Some(PathBuf::from("demo.ngc")), "G0 X0\nG1 X1\n");
        assert!(!editor.dirty);
        assert!(editor.replace_text("G0 X0\nG1 X2\n".into()));
        assert!(editor.dirty);
        assert!(editor.undo());
        assert_eq!(editor.text(), "G0 X0\nG1 X1\n");
        assert!(editor.redo());
        assert_eq!(editor.text(), "G0 X0\nG1 X2\n");
        editor.mark_saved();
        assert!(!editor.dirty);
    }

    #[test]
    fn line_operations_preserve_content() {
        let mut editor = Editor::from_text(None, "A\nB\nC\n");
        assert_eq!(editor.duplicate_line(2), Some(3));
        assert_eq!(editor.text(), "A\nB\nB\nC\n");
        assert_eq!(editor.move_line_up(3), Some(2));
        assert_eq!(editor.text(), "A\nB\nB\nC\n");
        assert_eq!(editor.delete_line(1), Some(1));
        assert_eq!(editor.text(), "B\nB\nC\n");
    }

    #[test]
    fn find_wraps_and_reports_line() {
        let editor = Editor::from_text(None, "G0 X0\nG1 X1\nG0 X2\n");
        let (line, start, end) = editor.find("g0", 2).unwrap();
        assert_eq!(line, 3);
        assert_eq!((start, end), (0, 2));
        assert_eq!(editor.find_all("G0"), vec![1, 3]);
    }

    #[test]
    fn dirty_state_survives_protect_style_readonly() {
        let mut editor = Editor::from_text(None, "G0 X0\n");
        editor.replace_text("G0 X1\n".into());
        let text = editor.text();
        assert!(editor.dirty);
        // Simulate a protect lock: the UI stops editing, the buffer is kept.
        assert_eq!(editor.text(), text);
        assert!(editor.dirty);
    }
}
