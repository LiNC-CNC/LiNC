//! Blocking Unix-socket client for the linc-plc daemon.
//!
//! The client speaks one request and one response per call over a length-
//! prefixed JSON frame; both read and write timeouts are 2 s so a wedged daemon
//! surfaces as an error instead of a hung caller.

#![forbid(unsafe_code)]

use std::env;
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use linc_plc_core::protocol::{read_message, write_message, ProtocolError};
use linc_plc_core::{Request, Response, PROTOCOL_VERSION};

/// Socket timeout applied to both directions.
const IO_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("protocol error: {0}")]
    Protocol(#[from] ProtocolError),
}

#[derive(Debug)]
pub struct Client {
    stream: UnixStream,
}

impl Client {
    pub fn connect(path: &Path) -> Result<Client, ClientError> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        stream.set_write_timeout(Some(IO_TIMEOUT))?;
        Ok(Client { stream })
    }

    pub fn connect_default() -> Result<Client, ClientError> {
        Client::connect(&default_socket_path())
    }

    pub fn call(&mut self, request: &Request) -> Result<Response, ClientError> {
        write_message(&mut self.stream, request)?;
        Ok(read_message(&mut self.stream)?)
    }

    pub fn hello(&mut self) -> Result<Response, ClientError> {
        self.call(&Request::Hello {
            protocol: PROTOCOL_VERSION,
        })
    }
}

/// `$LINC_PLC_SOCKET` when set, else `$XDG_RUNTIME_DIR/linc/plc-<uid>.sock`,
/// else `/tmp/linc-plc-<uid>.sock`.
pub fn default_socket_path() -> PathBuf {
    if let Some(explicit) = env_value("LINC_PLC_SOCKET") {
        return PathBuf::from(explicit);
    }
    let uid = current_uid();
    if let Some(runtime) = env_value("XDG_RUNTIME_DIR") {
        return PathBuf::from(runtime)
            .join("linc")
            .join(format!("plc-{uid}.sock"));
    }
    PathBuf::from(format!("/tmp/linc-plc-{uid}.sock"))
}

fn env_value(key: &str) -> Option<String> {
    match env::var(key) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => None,
    }
}

// `libc::getuid` is a foreign call and therefore unsafe, which the crate-wide
// `forbid(unsafe_code)` rejects. The real uid is read from procfs instead; the
// value is the same one `getuid` returns.
fn current_uid() -> libc::uid_t {
    if let Some(uid) = uid_from_proc_status() {
        return uid;
    }
    if let Ok(metadata) = std::fs::metadata("/proc/self") {
        return metadata.uid();
    }
    0
}

fn uid_from_proc_status() -> Option<libc::uid_t> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            return rest.split_whitespace().next()?.parse().ok();
        }
    }
    None
}
