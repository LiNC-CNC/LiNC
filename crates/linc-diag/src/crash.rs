//! Panic records on disk.
//!
//! A crash is the one diagnostic that cannot be read from the socket: the
//! process that would answer is gone. Each process therefore installs a panic
//! hook that writes one JSON record before the panic propagates, and `linc-mcp`
//! reads the directory directly.
//!
//! Location: `$XDG_STATE_HOME/linc/crash-<role>-<pid>.json`, falling back to
//! `$HOME/.local/state/linc`. A record is small (message, location and a capped
//! backtrace) so a repeated crash does not fill the disk.

use std::path::PathBuf;

use linc_core::diag::{now_ms, CrashRecord, ProcessInfo};

/// Where crash records live. `LINC_CRASH_DIR` overrides it for tests.
pub fn crash_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LINC_CRASH_DIR") {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return dir;
        }
    }
    if let Some(state) = std::env::var_os("XDG_STATE_HOME") {
        let state = PathBuf::from(state);
        if !state.as_os_str().is_empty() {
            return state.join("linc");
        }
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    home.join(".local/state/linc")
}

pub fn record_path(role: &str, pid: u32) -> PathBuf {
    record_path_in(&crash_dir(), role, pid)
}

fn record_path_in(dir: &std::path::Path, role: &str, pid: u32) -> PathBuf {
    dir.join(format!("crash-{role}-{pid}.json"))
}

/// Write one record, returning the path it landed at.
pub fn write_record(record: &CrashRecord) -> std::io::Result<PathBuf> {
    write_record_in(&crash_dir(), record)
}

fn write_record_in(dir: &std::path::Path, record: &CrashRecord) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    let path = record_path_in(dir, &record.role, record.pid);
    let text = serde_json::to_string_pretty(record).unwrap_or_default();
    std::fs::write(&path, format!("{text}\n"))?;
    Ok(path)
}

/// Read every record, newest first.
pub fn read_all(limit: usize) -> Vec<CrashRecord> {
    read_all_in(&crash_dir(), limit)
}

fn read_all_in(dir: &std::path::Path, limit: usize) -> Vec<CrashRecord> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut records: Vec<CrashRecord> = entries
        .filter_map(Result::ok)
        .filter(|entry| {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            name.starts_with("crash-") && name.ends_with(".json")
        })
        .filter_map(|entry| {
            let text = std::fs::read_to_string(entry.path()).ok()?;
            serde_json::from_str(&text).ok()
        })
        .collect();
    records.sort_by(|left, right| right.at_ms.cmp(&left.at_ms));
    records.truncate(limit.clamp(1, 200));
    records
}

/// Cap the backtrace so a recursive panic cannot write megabytes.
const BACKTRACE_LIMIT: usize = 8 * 1024;

fn message_of(info: &std::panic::PanicHookInfo<'_>) -> String {
    if let Some(text) = info.payload().downcast_ref::<&str>() {
        return (*text).to_string();
    }
    if let Some(text) = info.payload().downcast_ref::<String>() {
        return text.clone();
    }
    "panic with a non-string payload".into()
}

/// Install the hook for this process. Call once, early in `main`.
pub fn install_hook(role: &'static str, process: ProcessInfo) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|location| format!("{}:{}:{}", location.file(), location.line(), location.column()));
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        let backtrace = if backtrace.len() > BACKTRACE_LIMIT {
            backtrace[..BACKTRACE_LIMIT].to_string()
        } else {
            backtrace
        };
        let record = CrashRecord {
            at_ms: now_ms(),
            pid: std::process::id(),
            role: role.into(),
            build: process.build.clone(),
            message: message_of(info),
            location,
            // `force_capture` gives a backtrace even when RUST_BACKTRACE is off;
            // "disabled backtrace" is not useful in a crash record.
            backtrace: Some(backtrace),
        };
        if let Err(error) = write_record(&record) {
            eprintln!("linc: cannot write crash record: {error}");
        }
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use linc_core::diag::BuildInfo;

    fn record(role: &str, at_ms: i64) -> CrashRecord {
        CrashRecord {
            at_ms,
            pid: 42,
            role: role.into(),
            build: BuildInfo {
                version: "test".into(),
                ..BuildInfo::default()
            },
            message: "boom".into(),
            location: Some("src/main.rs:1:1".into()),
            backtrace: None,
        }
    }

    #[test]
    fn records_round_trip_newest_first() {
        let dir = std::env::temp_dir().join(format!("linc-crash-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        write_record_in(&dir, &record("hmi", 1_000)).expect("write oldest");
        write_record_in(&dir, &record("panel", 2_000)).expect("write newest");
        // An unrelated file in the directory is ignored.
        std::fs::write(dir.join("notes.txt"), b"not a crash").unwrap();

        let records = read_all_in(&dir, 10);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].role, "panel");
        assert_eq!(records[1].role, "hmi");
        assert_eq!(records[0].message, "boom");
        assert_eq!(records[0].location.as_deref(), Some("src/main.rs:1:1"));

        assert_eq!(read_all_in(&dir, 1).len(), 1, "the limit applies after sorting");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
