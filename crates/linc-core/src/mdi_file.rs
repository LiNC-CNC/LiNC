//! Writing the MDI block buffer to a program file.
//!
//! The MDI pane keeps the blocks it has issued in a session buffer. Saving that
//! buffer is the only place the HMI creates a program file, so the name rules
//! and the compose step live here instead of in the UI: the pane passes a bare
//! name, this module decides the path inside `PROGRAM_PREFIX`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use thiserror::Error;

#[derive(Debug, Error)]
pub enum SaveError {
    /// An existing program would be replaced; the save dialog asks first.
    #[error("{0} already exists")]
    Exists(PathBuf),
    #[error("{0}")]
    InvalidName(&'static str),
    #[error("cannot write the program file: {0}")]
    Io(#[from] std::io::Error),
}

/// The program text for `buffer`: the blocks in order, optionally bracketed by
/// the profile's preamble and postamble. Always ends with one newline, so a
/// appended run on the machine does not see the last block joined to the next.
pub fn compose(buffer: &[String], preamble: &[String], postamble: &[String], wrap: bool) -> String {
    let mut text = String::new();
    if wrap {
        push_lines(&mut text, preamble);
    }
    push_lines(&mut text, buffer);
    if wrap {
        push_lines(&mut text, postamble);
    }
    text
}

fn push_lines(text: &mut String, lines: &[String]) {
    for line in lines {
        text.push_str(line);
        text.push('\n');
    }
}

/// The file name for a save: trimmed, `.ngc` appended when it is missing, and
/// refused when it is not a bare name. A separator or `..` would move the write
/// out of `PROGRAM_PREFIX`, so both are rejected instead of stripped.
pub fn normalise_name(raw: &str) -> Result<String, SaveError> {
    let name = raw.trim();
    if name.is_empty() {
        return Err(SaveError::InvalidName("a program needs a name"));
    }
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(SaveError::InvalidName(
            "a program name cannot contain '/', '\\' or '..'",
        ));
    }
    if name.to_lowercase().ends_with(".ngc") {
        return Ok(name.to_string());
    }
    Ok(format!("{name}.ngc"))
}

/// Write `body` to `name` under `dir` and return the full path. The path is
/// built from the normalised name only, so no raw input can escape `dir`, and
/// an existing file is refused unless `overwrite` is set.
pub fn write_program(
    dir: &Path,
    name: &str,
    body: &str,
    overwrite: bool,
) -> Result<PathBuf, SaveError> {
    let path = dir.join(normalise_name(name)?);
    let mut options = OpenOptions::new();
    options.write(true);
    if overwrite {
        options.create(true).truncate(true);
    } else {
        // create_new refuses an existing file without a check-then-write window.
        options.create_new(true);
    }
    let mut file = match options.open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(SaveError::Exists(path));
        }
        Err(error) => return Err(SaveError::Io(error)),
    };
    file.write_all(body.as_bytes())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_string()).collect()
    }

    fn preamble() -> Vec<String> {
        lines(&["G17", "G21", "G90"])
    }

    fn postamble() -> Vec<String> {
        lines(&["M5", "M30"])
    }

    #[test]
    fn compose_with_wrap_brackets_the_buffer() {
        let buffer = lines(&["G0 X0 Y0", "G1 Z-1 F100"]);
        let body = compose(&buffer, &preamble(), &postamble(), true);
        assert_eq!(body, "G17\nG21\nG90\nG0 X0 Y0\nG1 Z-1 F100\nM5\nM30\n");
    }

    #[test]
    fn compose_without_wrap_writes_the_buffer_only() {
        let buffer = lines(&["G0 X0"]);
        assert_eq!(
            compose(&buffer, &preamble(), &postamble(), false),
            "G0 X0\n"
        );
        assert_eq!(compose(&[], &preamble(), &postamble(), false), "");
    }

    #[test]
    fn normalise_name_appends_the_extensions() {
        assert_eq!(normalise_name("part").unwrap(), "part.ngc");
        assert_eq!(normalise_name("  part.ngc ").unwrap(), "part.ngc");
        assert_eq!(normalise_name("PART.NGC").unwrap(), "PART.NGC");
        assert_eq!(normalise_name("part.ngc.tar").unwrap(), "part.ngc.tar.ngc");
    }

    #[test]
    fn normalise_name_rejects_empty_and_escaping_names() {
        assert!(matches!(
            normalise_name("   "),
            Err(SaveError::InvalidName(_))
        ));
        assert!(matches!(
            normalise_name("../x"),
            Err(SaveError::InvalidName(_))
        ));
        assert!(matches!(
            normalise_name("a/b"),
            Err(SaveError::InvalidName(_))
        ));
        assert!(matches!(
            normalise_name("a\\b"),
            Err(SaveError::InvalidName(_))
        ));
    }

    #[test]
    fn write_program_refuses_an_existing_file_then_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let first = write_program(dir.path(), "part", "G0 X0\n", false).unwrap();
        assert_eq!(first, dir.path().join("part.ngc"));
        assert!(matches!(
            write_program(dir.path(), "part", "G0 X1\n", false),
            Err(SaveError::Exists(path)) if path == first
        ));
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "G0 X0\n");
        let second = write_program(dir.path(), "part", "G0 X1\n", true).unwrap();
        assert_eq!(second, first);
        assert_eq!(std::fs::read_to_string(&first).unwrap(), "G0 X1\n");
    }

    #[test]
    fn write_program_stays_inside_the_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            write_program(dir.path(), "../escape", "G0\n", false),
            Err(SaveError::InvalidName(_))
        ));
        assert!(!dir.path().parent().unwrap().join("escape.ngc").exists());
    }

    #[test]
    fn a_saved_wrapped_program_starts_with_the_preamble_and_ends_with_the_postamble() {
        let dir = tempfile::tempdir().unwrap();
        let body = compose(
            &lines(&["G0 X0 Y0", "M3 S1200"]),
            &preamble(),
            &postamble(),
            true,
        );
        let path = write_program(dir.path(), "wrapper", &body, false).unwrap();
        let saved = std::fs::read_to_string(path).unwrap();
        let mut saved_lines = saved.lines();
        assert_eq!(saved_lines.next(), Some("G17"));
        assert_eq!(saved_lines.last(), Some("M30"));
        assert!(saved.ends_with("M30\n"));
        assert_eq!(saved.matches("\n").count(), saved.lines().count());
    }
}
