//! Pre-run verification with the controller's own interpreter.
//!
//! The preview parser draws a toolpath and marks what it can see; the lint in
//! [crate::lint] reads one block at a time. Neither is the interpreter that will
//! run the file. `rs274` is: it is the same reader and the same preparation code
//! the machine's task uses, driven through a stand-in canon layer, so it refuses
//! a file for the same reasons and with the same words the machine would - and
//! refuses it without moving an axis.
//!
//! What it cannot do is tell us where. Except for o-word label faults, the
//! interpreter's message carries no line number: it echoes the offending block
//! instead. The block is therefore matched back against the file, first
//! occurrence first. When the same block appears more than once the operator
//! still sees the block verbatim, so the mark is at worst on the wrong one of two
//! identical lines. When the block lives in a subroutine file the interpreter
//! names no file either, so nothing is marked and the message stands alone.
//!
//! Three properties make this safe to run while the machine is idle:
//! the child writes its canon trace to a discarded stdout, it is killed on the
//! timeout because a program can loop forever without moving, and it never
//! touches the machine: it takes the INI read-only, does not write the parameter
//! file, and opens no HAL shared memory.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// How the machine's interpreter answered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckOutcome {
    /// The interpreter read the whole file without complaint.
    Ok,
    /// It stopped, and said why.
    Refused,
    /// It was still running at the deadline: an unbounded loop, or a program far
    /// longer than the budget. Not a verdict on the file.
    Timeout,
    /// `rs274` is not installed here, so there is nothing to ask.
    Unavailable,
    /// The interpreter died without a verdict. Measured, not guessed: two
    /// instances started at the same moment produce a `SIGBUS` in one of them, so
    /// runs are serialised below and a crash is never read as "the file is fine".
    Crashed,
}

impl CheckOutcome {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ok => "MACHINE CHECK OK",
            Self::Refused => "MACHINE CHECK FAILED",
            Self::Timeout => "MACHINE CHECK TIMED OUT",
            Self::Unavailable => "MACHINE CHECK UNAVAILABLE",
            Self::Crashed => "MACHINE CHECK FAILED TO RUN",
        }
    }
}

/// One interpreter run, reduced to what the UI needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MachineCheck {
    pub outcome: CheckOutcome,
    /// The interpreter's message, verbatim.
    pub message: Option<String>,
    /// The block it echoed after the message, verbatim.
    pub block: Option<String>,
    /// 1-based line the echoed block was found on, when it was found at all.
    pub line: Option<usize>,
}

impl MachineCheck {
    fn unavailable() -> Self {
        Self {
            outcome: CheckOutcome::Unavailable,
            message: None,
            block: None,
            line: None,
        }
    }

    /// The line to mark, when the message names one outright or the echoed block
    /// was located in the file.
    pub fn marked_line(&self) -> Option<usize> {
        self.line
    }

    /// A single line for the status row.
    pub fn summary(&self) -> String {
        match self.outcome {
            CheckOutcome::Ok => self.outcome.label().to_string(),
            CheckOutcome::Unavailable => {
                "MACHINE CHECK SKIPPED: rs274 is not installed".to_string()
            }
            CheckOutcome::Timeout => {
                "MACHINE CHECK TIMED OUT: the trace stopped before the file ended".to_string()
            }
            CheckOutcome::Crashed => {
                "MACHINE CHECK FAILED TO RUN: the interpreter exited without a verdict".to_string()
            }
            CheckOutcome::Refused => {
                let message = self.message.as_deref().unwrap_or("refused");
                match self.marked_line() {
                    Some(line) => format!("MACHINE LN {line}: {message}"),
                    None => format!("MACHINE: {message}"),
                }
            }
        }
    }
}

/// Whether `rs274` is on this machine's PATH.
pub fn rs274_available() -> bool {
    rs274_path().is_some()
}

/// The `rs274` executable, resolved on PATH.
pub fn rs274_path() -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join("rs274"))
        .find(|candidate| candidate.is_file())
}

/// Serialises interpreter runs. Two `rs274` processes started at the same moment
/// raced on a shared resource and one died with `SIGBUS`; a check that crashes
/// reads as "no faults", which is the one answer this module must never invent.
static INTERPRETER_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Read `text` through the controller's interpreter.
///
/// `ini` is the machine INI, so subroutine paths, units and the tool table are
/// the machine's own; `tool_table` overrides where the tool table is read from
/// when the caller has a fresher copy than the INI's.
pub fn check_program(
    ini: Option<&Path>,
    tool_table: Option<&Path>,
    text: &str,
    timeout: Duration,
) -> MachineCheck {
    let Some(rs274) = rs274_path() else {
        return MachineCheck::unavailable();
    };
    let _serialised = INTERPRETER_LOCK
        .lock()
        .unwrap_or_else(|held| held.into_inner());
    // The interpreter takes a path, not stdin: `rs274 -` is read as a file name.
    let mut file = match tempfile::Builder::new()
        .prefix("linc-verify-")
        .suffix(".ngc")
        .tempfile()
    {
        Ok(file) => file,
        Err(error) => {
            return MachineCheck {
                outcome: CheckOutcome::Unavailable,
                message: Some(format!("no temporary file for the check: {error}")),
                block: None,
                line: None,
            }
        }
    };
    if let Err(error) = std::io::Write::write_all(&mut file, text.as_bytes()) {
        return MachineCheck {
            outcome: CheckOutcome::Unavailable,
            message: Some(format!("could not write the check file: {error}")),
            block: None,
            line: None,
        };
    }
    let mut command = Command::new(rs274);
    if let Some(table) = tool_table.filter(|path| path.is_file()) {
        command.arg("-t").arg(table);
    }
    if let Some(ini) = ini.filter(|path| path.is_file()) {
        command.arg("-i").arg(ini);
    }
    // Batch mode: read the file to the end and report the first refusal without
    // the interactive prompt. Options come before the file name.
    command.arg("-g").arg(file.path());
    command
        .stdin(Stdio::null())
        // The canon trace is megabytes on a long program and says nothing about
        // syntax; nothing is read from it.
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            return MachineCheck {
                outcome: CheckOutcome::Unavailable,
                message: Some(format!("could not start rs274: {error}")),
                block: None,
                line: None,
            }
        }
    };
    // Drained on its own thread: a child that fills the pipe while we wait would
    // deadlock against `try_wait`.
    let mut program_stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut buffer = String::new();
        if let Some(program_stderr) = program_stderr.as_mut() {
            let _ = program_stderr.read_to_string(&mut buffer);
        }
        buffer
    });
    let deadline = Instant::now() + timeout;
    let mut timed_out = false;
    let mut status = None;
    loop {
        match child.try_wait() {
            Ok(Some(exit)) => {
                status = Some(exit);
                break;
            }
            Ok(None) => {}
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            // A program may loop without moving; the interpreter has no deadline
            // of its own, so this one is the only thing that ends it.
            let _ = child.kill();
            let _ = child.wait();
            timed_out = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let report = reader.join().unwrap_or_default();
    if timed_out {
        return MachineCheck {
            outcome: CheckOutcome::Timeout,
            message: None,
            block: None,
            line: None,
        };
    }
    let (message, block) = parse_rs274_stderr(&report);
    match message {
        // No message and a clean exit is the only "yes" there is. A crash is
        // explicitly not one: an interpreter that died said nothing about the
        // file, and reading that as approval would open the gate on nothing.
        None if status.is_some_and(|status| status.success()) => MachineCheck {
            outcome: CheckOutcome::Ok,
            message: None,
            block: None,
            line: None,
        },
        None => MachineCheck {
            outcome: CheckOutcome::Crashed,
            message: status.map(|status| format!("rs274 exited with {status}")),
            block: None,
            line: None,
        },
        Some(message) => {
            // The echoed block is the only pointer to the place: match it against
            // the text that was handed to the interpreter.
            let line = block
                .as_deref()
                .and_then(|echoed| locate_block(text.lines(), echoed));
            MachineCheck {
                outcome: CheckOutcome::Refused,
                message: Some(message),
                block,
                line,
            }
        }
    }
}

/// The interpreter prints a status word, the message, then the block it was
/// reading. Everything else on stderr is noise.
pub fn parse_rs274_stderr(stderr: &str) -> (Option<String>, Option<String>) {
    let mut lines = stderr
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.trim().is_empty())
        .filter(|line| line.trim() != "executing");
    let message = lines.next().map(|line| line.trim().to_string());
    let block = lines.next().map(|line| line.trim().to_string());
    (message, block)
}

/// The line a message names outright. The o-word and label faults are the only
/// ones the interpreter prefixes this way: `2: undefined O-word label: 'o100
/// endif'`.
pub fn line_from_message(message: &str) -> Option<usize> {
    let (prefix, _) = message.split_once(':')?;
    prefix.trim().parse::<usize>().ok().filter(|line| *line > 0)
}

/// The first line whose trimmed text is the echoed block.
pub fn locate_block<'a>(lines: impl Iterator<Item = &'a str>, block: &str) -> Option<usize> {
    let needle = block.trim();
    if needle.is_empty() {
        return None;
    }
    lines
        .enumerate()
        .find(|(_, line)| line.trim() == needle)
        .map(|(index, _)| index + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_word_is_not_the_message() {
        let stderr = "executing\nMultiple x words on one line\nG1 X10 X20 F100\n\n";
        let (message, block) = parse_rs274_stderr(stderr);
        assert_eq!(message.as_deref(), Some("Multiple x words on one line"));
        assert_eq!(block.as_deref(), Some("G1 X10 X20 F100"));
    }

    #[test]
    fn a_clean_run_has_no_message() {
        let (message, block) = parse_rs274_stderr("executing\n");
        assert!(message.is_none());
        assert!(block.is_none());
    }

    #[test]
    fn an_o_word_fault_keeps_its_own_line_number_in_the_message() {
        let stderr = "executing\n2: undefined O-word label: 'o100 endif'\n";
        let (message, _) = parse_rs274_stderr(stderr);
        assert_eq!(
            message.as_deref(),
            Some("2: undefined O-word label: 'o100 endif'")
        );
    }

    #[test]
    fn a_message_that_names_its_line_is_read() {
        assert_eq!(
            line_from_message("2: undefined O-word label: 'o100 endif'"),
            Some(2)
        );
        assert_eq!(line_from_message("Cannot do g1 with zero feed rate"), None);
        assert_eq!(line_from_message("EOF in file:x seeking o-word"), None);
    }

    #[test]
    fn the_block_is_found_where_it_sits_in_the_file() {
        let text = "G0 X0 Y0\nG1 X10 F100\nG1 X10 X20 F100\nM2\n";
        let line = locate_block(text.lines(), "G1 X10 X20 F100").unwrap();
        assert_eq!(line, 3);
    }

    #[test]
    fn indentation_does_not_hide_the_block() {
        let text = "G0 X0\n    G1 X10 X20 F100\nM2\n";
        assert_eq!(locate_block(text.lines(), "  G1 X10 X20 F100  "), Some(2));
    }

    #[test]
    fn an_empty_block_matches_nothing() {
        let text = "\n\nG0 X0\n";
        assert_eq!(locate_block(text.lines(), "   "), None);
    }

    #[test]
    fn a_program_the_interpreter_accepts_comes_back_ok() {
        if !rs274_available() {
            return;
        }
        let check = check_program(
            None,
            None,
            "G0 X0 Y0\nG1 X10 F100\nM2\n",
            Duration::from_secs(10),
        );
        assert_eq!(check.outcome, CheckOutcome::Ok, "{:?}", check);
    }

    #[test]
    fn a_mistake_the_lint_cannot_see_is_caught_and_located() {
        if !rs274_available() {
            return;
        }
        // Zero feed rate is modal knowledge: no single block can decide it.
        let check = check_program(
            None,
            None,
            "G0 X0 Y0\nG1 X10\nM2\n",
            Duration::from_secs(10),
        );
        assert_eq!(check.outcome, CheckOutcome::Refused, "{:?}", check);
        assert!(check
            .message
            .as_deref()
            .is_some_and(|message| message.contains("zero feed rate")));
        assert_eq!(check.line, Some(2));
    }

    #[test]
    fn a_program_that_loops_forever_is_killed_at_the_deadline() {
        if !rs274_available() {
            return;
        }
        let check = check_program(
            None,
            None,
            "G0 X0\n#1=0\no100 while [1]\n#1=0\no100 endwhile\nM2\n",
            Duration::from_millis(400),
        );
        assert_eq!(check.outcome, CheckOutcome::Timeout, "{:?}", check);
    }
}
