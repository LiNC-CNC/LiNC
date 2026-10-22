//! Where the diagnostic sockets live, and the `instance.json` handshake.

use std::io::Write;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use linc_core::diag::{BuildInfo, ProcessInfo};
use serde::{Deserialize, Serialize};

pub const INSTANCE_FILE: &str = "instance.json";

/// `$LINC_DIAG_DIR`, else `$XDG_RUNTIME_DIR/linc/<instance>`, else
/// `/tmp/linc-<uid>/<instance>`.
///
/// `LINC_INSTANCE` names the instance; it defaults to `default`, so a test can
/// run a private server without touching a live one.
pub fn runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("LINC_DIAG_DIR") {
        let dir = PathBuf::from(dir);
        if !dir.as_os_str().is_empty() {
            return dir;
        }
    }
    let instance = std::env::var("LINC_INSTANCE").unwrap_or_else(|_| "default".into());
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(base) if !base.is_empty() => PathBuf::from(base).join("linc").join(instance),
        _ => PathBuf::from(format!("/tmp/linc-{}", libc_uid())).join(instance),
    }
}

/// The process's uid without pulling in a libc crate: `/proc/self/status`.
fn libc_uid() -> u32 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("Uid:"))
                .and_then(|line| line.split_whitespace().nth(1)?.parse().ok())
        })
        .unwrap_or(0)
}

pub fn socket_path(dir: &Path, role: &str) -> PathBuf {
    dir.join(format!("{role}.sock"))
}

pub fn instance_path(dir: &Path) -> PathBuf {
    dir.join(INSTANCE_FILE)
}

/// Create the runtime directory with mode `0700`.
pub fn prepare_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(dir)?.permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(dir, permissions)?;
    }
    Ok(())
}

/// Mode `0600` on the socket: only the owner may connect.
pub fn restrict_socket(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(path)?.permissions();
        permissions.set_mode(0o600);
        std::fs::set_permissions(path, permissions)?;
    }
    Ok(())
}

/// A socket file is stale when nothing is listening on it.
pub fn socket_is_stale(path: &Path) -> bool {
    if !path.exists() {
        return false;
    }
    match UnixStream::connect(path) {
        Ok(_) => false,
        Err(error) => {
            // ECONNREFUSED means the file is there but nothing listens; any
            // other error is treated as not-stale, so we never delete a socket
            // we merely failed to reach.
            error.kind() == std::io::ErrorKind::ConnectionRefused
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Instance {
    pub schema_version: u32,
    pub pid: u32,
    pub role: String,
    pub source: String,
    pub socket: PathBuf,
    pub started_at_ms: i64,
    pub profile: String,
    pub build: BuildInfo,
}

pub fn write_instance(
    dir: &Path,
    role: &str,
    socket: &Path,
    process: ProcessInfo,
) -> std::io::Result<()> {
    let instance = Instance {
        schema_version: linc_core::diag::SCHEMA_VERSION,
        pid: process.pid,
        role: role.into(),
        source: format!("linc-{role}"),
        socket: socket.to_path_buf(),
        started_at_ms: linc_core::diag::now_ms(),
        profile: process.build.profile.clone(),
        build: process.build,
    };
    let path = instance_path(dir);
    let temp = path.with_extension("json.tmp");
    let mut file = std::fs::File::create(&temp)?;
    serde_json::to_writer_pretty(&mut file, &instance)?;
    file.write_all(b"\n")?;
    file.flush()?;
    std::fs::rename(&temp, &path)?;
    Ok(())
}

/// Read `instance.json` for a role, refusing a record whose process is gone.
pub fn read_instance(dir: &Path, role: &str) -> std::io::Result<Instance> {
    let path = instance_path(dir);
    let text = std::fs::read_to_string(&path)?;
    let instance: Instance = serde_json::from_str(&text)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if instance.role != role {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("instance.json is for role {:?}, not {:?}", instance.role, role),
        ));
    }
    if !pid_alive(instance.pid) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{} is not running", instance.pid),
        ));
    }
    Ok(instance)
}

pub fn pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

/// Discover a role's socket: explicit override, then `instance.json`, then the
/// conventional path, and only if it is actually connectable.
pub fn discover(role: &str, override_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(path) = override_path {
        return Some(path.to_path_buf());
    }
    let dir = runtime_dir();
    if let Ok(instance) = read_instance(&dir, role) {
        if instance.socket.exists() {
            return Some(instance.socket);
        }
    }
    let conventional = socket_path(&dir, role);
    conventional
        .exists()
        .then_some(conventional)
}
