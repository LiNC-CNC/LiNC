//! Ladder PLC service control.
//!
//! The PLC is a separate userspace service. The HMI only decides whether to
//! start it and where it listens: nothing here waits for the child or kills it
//! on exit, so a display restart never takes the PLC down with it and a PLC
//! crash never blocks the HMI. In the simulation `stop-sim.sh` stops it from
//! the pid file written below.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use linc_core::config::Profile;

const PLC_BINARY: &str = "linc-plc";

/// One-run override from `LINC_PLC`: `1|true|on` enables, `0|false|off`
/// disables. Anything else (including unset) leaves the profile in charge.
pub fn plc_override() -> Option<bool> {
    parse_override(&std::env::var("LINC_PLC").unwrap_or_default())
}

fn parse_override(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" => Some(true),
        "0" | "false" | "off" => Some(false),
        _ => None,
    }
}

/// Effective PLC state: the `LINC_PLC` override wins over `[plc] enabled`.
pub fn enabled(profile: &Profile) -> bool {
    plc_override().unwrap_or(profile.config.plc.enabled)
}

/// Ladder project path. An empty `plc.project` yields an empty path: the PLC
/// may be enabled, but there is nothing to run and no pin map to bind.
pub fn project_path(profile: &Profile) -> PathBuf {
    project_path_from(&profile_dir(profile), &profile.config.plc.project).unwrap_or_default()
}

/// Daemon socket. An empty `plc.socket` uses the client's default so the page
/// and the daemon agree without either one guessing.
pub fn socket_path(profile: &Profile) -> PathBuf {
    socket_path_from(&profile_dir(profile), &profile.config.plc.socket)
}

/// Start the daemon when the profile or the override asks for it.
///
/// `no_hal` keeps the PLC off HAL: without the LinuxCNC backend there is no
/// HAL session to register a component with.
pub fn start(profile: &Profile, no_hal: bool) -> Option<PathBuf> {
    if !enabled(profile) {
        return None;
    }
    let project = project_path(profile);
    if project.as_os_str().is_empty() {
        log::warn!("PLC enabled but plc.project is empty; linc-plc not started");
        return None;
    }
    if !project.is_file() {
        log::warn!(
            "PLC project {} does not exist; linc-plc not started",
            project.display()
        );
        return None;
    }
    let binary = binary_path();
    if !binary.is_file() {
        log::warn!(
            "linc-plc binary not found at {}; PLC not started",
            binary.display()
        );
        return None;
    }

    let socket = socket_path(profile);
    // The daemon outlives the HMI on purpose. If it is already listening, a
    // second spawn would fail to bind (and on a HAL build fail to register the
    // component), then overwrite the pid file with a dead process. Reconnect to
    // the running service instead; a changed project needs an explicit restart.
    if linc_plc_client::Client::connect(&socket).is_ok() {
        log::info!(
            "linc-plc already listening on {}; not starting another",
            socket.display()
        );
        return Some(socket);
    }
    let config = &profile.config.plc;
    let mut command = Command::new(&binary);
    command
        .arg("--project")
        .arg(&project)
        .arg("--socket")
        .arg(&socket)
        .arg("--scan-us")
        .arg(config.scan_us.to_string());
    if config.allow_force {
        command.arg("--allow-force");
    }
    if no_hal {
        command.arg("--no-hal");
    }
    // The daemon logs to the session's own stdout/stderr, which is where the
    // operator already reads LinuxCNC and LiNC output.
    command
        .stdin(Stdio::null())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    match command.spawn() {
        Ok(child) => {
            log::info!(
                "linc-plc started (pid {}, project {}, socket {}{})",
                child.id(),
                project.display(),
                socket.display(),
                if no_hal { ", no HAL" } else { "" }
            );
            if let Err(error) = write_pid(child.id()) {
                log::warn!("cannot write the PLC pid file: {error}");
            }
            Some(socket)
        }
        Err(error) => {
            log::warn!("cannot start {}: {error}", binary.display());
            None
        }
    }
}

fn project_path_from(dir: &Path, value: &str) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(resolve(dir, value))
}

fn socket_path_from(dir: &Path, value: &str) -> PathBuf {
    let value = value.trim();
    if value.is_empty() {
        return linc_plc_client::default_socket_path();
    }
    resolve(dir, value)
}

/// Profile-relative paths resolve against the profile's own directory, the same
/// rule the loader uses for the INI and the preferences file.
fn resolve(dir: &Path, value: &str) -> PathBuf {
    let path = PathBuf::from(value);
    if path.is_absolute() {
        path
    } else {
        dir.join(path)
    }
}

fn profile_dir(profile: &Profile) -> PathBuf {
    profile
        .path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn binary_path() -> PathBuf {
    if let Some(explicit) = env_path("LINC_PLC_BIN") {
        return explicit;
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.join(PLC_BINARY)))
        .unwrap_or_else(|| PathBuf::from(PLC_BINARY))
}

/// `LINC_PLC_PID_FILE`, else `$XDG_RUNTIME_DIR/linc/plc.pid`, else
/// `/tmp/linc-plc.pid`. The sim launcher exports the first, so `stop-sim.sh`
/// and `linc-display.sh` find the same file.
fn pid_file() -> PathBuf {
    if let Some(explicit) = env_path("LINC_PLC_PID_FILE") {
        return explicit;
    }
    match env_path("XDG_RUNTIME_DIR") {
        Some(dir) => dir.join("linc").join("plc.pid"),
        None => PathBuf::from("/tmp/linc-plc.pid"),
    }
}

fn write_pid(pid: u32) -> std::io::Result<()> {
    let path = pid_file();
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            std::fs::create_dir_all(dir)?;
        }
    }
    std::fs::write(&path, format!("{pid}\n"))
}

fn env_path(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_override_accepts_the_documented_spellings_only() {
        for on in ["1", "true", "on", "TRUE", " On ", "True"] {
            assert_eq!(parse_override(on), Some(true), "{on}");
        }
        for off in ["0", "false", "off", "FALSE", " Off ", "False"] {
            assert_eq!(parse_override(off), Some(false), "{off}");
        }
        for other in ["", " ", "yes", "no", "2", "enabled"] {
            assert_eq!(parse_override(other), None, "{other:?}");
        }
    }

    #[test]
    fn a_relative_project_and_socket_resolve_against_the_profile_directory() {
        let dir = Path::new("/profiles/sim");
        assert_eq!(
            project_path_from(dir, "plc-sim.json"),
            Some(dir.join("plc-sim.json"))
        );
        assert_eq!(
            project_path_from(dir, "  plc-sim.json \n"),
            Some(dir.join("plc-sim.json"))
        );
        assert_eq!(project_path_from(dir, ""), None);
        assert_eq!(project_path_from(dir, "   "), None);
        // An absolute path is taken as written.
        assert_eq!(
            project_path_from(dir, "/opt/ladder/main.json"),
            Some(PathBuf::from("/opt/ladder/main.json"))
        );

        assert_eq!(
            socket_path_from(dir, "run/plc.sock"),
            dir.join("run/plc.sock")
        );
        assert_eq!(
            socket_path_from(dir, "/run/linc/plc.sock"),
            PathBuf::from("/run/linc/plc.sock")
        );
        // An empty socket is the client's default, not a profile-relative path.
        assert_eq!(
            socket_path_from(dir, "  "),
            linc_plc_client::default_socket_path()
        );
    }
}
