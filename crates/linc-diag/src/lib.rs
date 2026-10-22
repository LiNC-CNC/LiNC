//! The diagnostic protocol between `linc` (and `linc-panel`) and `linc-mcp`.
//!
//! This crate is deliberately small: a Unix socket, one JSON object per line,
//! an envelope, and the [`BusSource`] adapter that answers `diag.*` from the
//! shared [`linc_core::diag::Bus`]. It does not depend on the UI, HAL, LinuxCNC
//! or the panel, so `linc-mcp` can be built and tested without any of them.
//!
//! Security model: the socket lives in a directory with mode `0700` and has
//! mode `0600`, so only the same user can connect. Requests are capped at
//! [`MAX_FRAME`] bytes and responses are capped by the handlers.

pub mod client;
pub mod discovery;
pub mod panel;
pub mod source;

use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;

use linc_core::diag::{DiagError, Dropped, Envelope};
use serde_json::{json, Value};

pub use client::Client;
pub use panel::{PanelBus, PanelSource, SimControl};
pub use source::{BusSource, Source};

/// One request line larger than this is rejected instead of buffered.
pub const MAX_FRAME: usize = 8 * 1024 * 1024;

/// A running diagnostic server. Dropping it stops the accept loop and removes
/// the socket and `instance.json`.
pub struct ServerHandle {
    pub socket: PathBuf,
    pub dir: PathBuf,
    role: String,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        // The accept loop wakes itself: it never waits on a connection, so a
        // caller that removed the socket file first (a test cleaning its temp
        // directory, an operator deleting a stale socket) cannot make this join
        // wait for a connect that can no longer succeed.
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        let _ = std::fs::remove_file(&self.socket);
        let _ = std::fs::remove_file(discovery::instance_path(&self.dir, &self.role));
    }
}

/// Serve `source` on `<dir>/<role>.sock` until the handle is dropped.
pub fn serve(dir: PathBuf, role: &str, source: Arc<dyn Source>) -> std::io::Result<ServerHandle> {
    discovery::prepare_dir(&dir)?;
    let socket = discovery::socket_path(&dir, role);
    if discovery::socket_is_stale(&socket) {
        let _ = std::fs::remove_file(&socket);
    }
    let listener = UnixListener::bind(&socket)?;
    discovery::restrict_socket(&socket)?;
    discovery::write_instance(&dir, role, &socket, source.process())?;

    let source_name = if role == "hmi" {
        "linc".to_string()
    } else {
        format!("linc-{role}")
    };
    // Poll instead of blocking in `accept`. A blocking accept can only be woken
    // by a connection, so a shutdown after the socket file disappeared - which
    // is exactly what tidying a temp directory does - used to join forever.
    listener.set_nonblocking(true)?;
    let poll = std::time::Duration::from_millis(50);
    let stop = Arc::new(AtomicBool::new(false));
    let stop_thread = stop.clone();
    let join = std::thread::Builder::new()
        .name(format!("diag-{role}"))
        .spawn(move || {
            while !stop_thread.load(Ordering::Relaxed) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // The listener is non-blocking; the connection handler
                        // uses a read timeout and must not inherit that.
                        let _ = stream.set_nonblocking(false);
                        let source = source.clone();
                        let source_name = source_name.clone();
                        std::thread::Builder::new()
                            .name("diag-conn".into())
                            .spawn(move || {
                                let _ = handle_connection(stream, source.as_ref(), &source_name);
                            })
                            .ok();
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(poll);
                    }
                    Err(error) => {
                        log::debug!("diagnostic accept failed: {error}");
                        std::thread::sleep(poll);
                    }
                }
            }
        })?;

    Ok(ServerHandle {
        socket,
        dir,
        role: role.to_string(),
        stop,
        join: Some(join),
    })
}

fn handle_connection(mut stream: UnixStream, source: &dyn Source, source_name: &str) -> std::io::Result<()> {
    let mut reader = std::io::BufReader::new(stream.try_clone()?);
    let mut subscribed = false;
    let mut last_sequence = 0u64;
    let poll = std::time::Duration::from_millis(250);
    stream.set_read_timeout(Some(poll))?;
    loop {
        let mut line = String::new();
        match read_line_limited(&mut reader, &mut line, MAX_FRAME) {
            Ok(0) => break,
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                // No request within the poll window: push a notification when
                // the sequence moved, which is what `diag.subscribe` promises.
                if subscribed {
                    let meta = source.meta();
                    if meta.sequence != last_sequence {
                        last_sequence = meta.sequence;
                        let notify = json!({
                            "jsonrpc": "2.0",
                            "method": "diag.notify",
                            "params": {
                                "topic": "snapshot",
                                "sequence": meta.sequence,
                                "captured_at_ms": meta.captured_at_ms,
                                "data": json!({
                                    "summary": {
                                        "sequence": meta.sequence,
                                        "captured_at_ms": meta.captured_at_ms,
                                    }
                                }),
                            },
                        });
                        if write_response(&mut stream, &notify).is_err() {
                            break;
                        }
                    }
                }
                continue;
            }
            Err(error) => {
                let _ = write_response(&mut stream, &error_response(Value::Null, &DiagError::new(
                    "INVALID_ARGUMENT",
                    format!("unreadable request: {error}"),
                    false,
                )));
                break;
            }
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let request: Value = match serde_json::from_str(trimmed) {
            Ok(value) => value,
            Err(error) => {
                let _ = write_response(
                    &mut stream,
                    &error_response(
                        Value::Null,
                        &DiagError::invalid_argument(format!("bad JSON: {error}")),
                    ),
                );
                continue;
            }
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = request.get("method").and_then(Value::as_str) else {
            let _ = write_response(
                &mut stream,
                &error_response(id, &DiagError::invalid_argument("missing method")),
            );
            continue;
        };
        let params = request.get("params").cloned().unwrap_or(Value::Null);
        if method == "diag.subscribe" {
            subscribed = true;
            last_sequence = source.meta().sequence;
            let response = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": envelope_for(
                    source_name,
                    source,
                    json!({ "subscribed": true, "since_sequence": last_sequence }),
                ),
            });
            let _ = write_response(&mut stream, &response);
            continue;
        }
        if method == "diag.unsubscribe" {
            subscribed = false;
            let response = json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": envelope_for(source_name, source, json!({ "subscribed": false })),
            });
            let _ = write_response(&mut stream, &response);
            continue;
        }
        match source.call(method, &params) {
            Ok(data) => {
                let response = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": envelope_for(source_name, source, data),
                });
                let _ = write_response(&mut stream, &response);
            }
            Err(error) => {
                let _ = write_response(&mut stream, &error_response(id, &error));
            }
        }
    }
    Ok(())
}

fn envelope_for(source_name: &str, source: &dyn Source, data: Value) -> Envelope {
    let meta = source.meta();
    Envelope::new(
        source_name,
        source.process(),
        meta.sequence,
        meta.captured_at_ms,
        meta.stale_after_ms,
        meta.dropped,
        data,
    )
}

fn error_response(id: Value, error: &DiagError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": error.code, "message": error.message, "data": error },
    })
}

fn write_response(stream: &mut UnixStream, value: &Value) -> std::io::Result<()> {
    let mut bytes = serde_json::to_vec(value).unwrap_or_default();
    bytes.push(b'\n');
    stream.write_all(&bytes)?;
    stream.flush()
}

fn read_line_limited<R: Read>(
    reader: &mut std::io::BufReader<R>,
    out: &mut String,
    limit: usize,
) -> std::io::Result<usize> {
    let mut total = 0;
    let mut byte = [0u8; 1];
    out.clear();
    loop {
        match reader.read(&mut byte) {
            Ok(0) => return Ok(total),
            Ok(_) => {
                total += 1;
                if total > limit {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "request exceeds the frame limit",
                    ));
                }
                if byte[0] == b'\n' {
                    return Ok(total);
                }
                out.push(byte[0] as char);
            }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Metadata a source reports with every response.
#[derive(Clone, Debug)]
pub struct SourceMeta {
    pub sequence: u64,
    pub captured_at_ms: i64,
    pub stale_after_ms: i64,
    pub dropped: Dropped,
}

#[cfg(test)]
mod tests {
    use super::*;
    use linc_core::diag::{BuildInfo, Bus, ProcessInfo};
    use linc_core::model::{MachineSnapshot, MachineVariant};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "linc-diag-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn start(dir: &PathBuf) -> (ServerHandle, Arc<Mutex<Bus>>) {
        let bus = Arc::new(Mutex::new(Bus::new(MachineSnapshot::empty(
            MachineVariant::Mill,
        ))));
        let process = ProcessInfo {
            pid: std::process::id(),
            role: "hmi".into(),
            build: BuildInfo {
                version: "test".into(),
                ..BuildInfo::default()
            },
        };
        let source = Arc::new(BusSource::new(bus.clone(), process));
        let handle = serve(dir.clone(), "hmi", source).expect("serve");
        (handle, bus)
    }

    #[test]
    fn the_server_answers_the_protocol_and_keeps_its_socket_private() {
        let dir = temp_dir("protocol");
        let (handle, bus) = start(&dir);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&handle.socket)
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "socket mode");
            let dir_mode = std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
            assert_eq!(dir_mode, 0o700, "directory mode");
        }

        let instance = discovery::read_instance(&dir, "hmi").expect("instance.json");
        assert_eq!(instance.pid, std::process::id());
        assert_eq!(instance.role, "hmi");

        let mut client = Client::connect(&handle.socket).expect("connect");
        let hello = client.call("diag.hello", Value::Null).expect("hello");
        assert_eq!(hello["schema_version"], 1);
        assert!(hello["process"]["build"]["version"].is_string());
        assert!(hello["age_ms"].as_i64().unwrap() >= 0);

        let snapshot = client
            .call("diag.snapshot", json!({ "include_trail": false }))
            .expect("snapshot");
        assert!(snapshot["data"]["machine"].is_object());
        assert!(snapshot["data"]["ui"].is_object());
        assert!(!snapshot["data"]["trail_included"].as_bool().unwrap());

        let blocked = client
            .call("diag.why_blocked", Value::Null)
            .expect("why_blocked");
        let blockers = blocked["data"]["blockers"].as_array().unwrap();
        assert!(blockers.iter().any(|b| b["code"] == "NO_PROGRAM"), "{blocked}");

        // Unknown methods are protocol errors; the server stays up.
        let error = client
            .call("diag.nope", Value::Null)
            .expect_err("unknown method");
        assert_eq!(error.code, "NOT_FOUND");
        let ping = client.call("diag.ping", Value::Null).expect("still alive");
        assert!(ping["data"]["now_ms"].is_i64());

        // A published snapshot shows up in the next response.
        let mut next = MachineSnapshot::empty(MachineVariant::Mill);
        next.machine_name = "TestMill".into();
        bus.lock().unwrap().publish_machine(next);
        let snapshot = client
            .call("diag.snapshot", json!({}))
            .expect("snapshot again");
        assert_eq!(snapshot["data"]["machine"]["machine_name"], "TestMill");

        let socket = handle.socket.clone();
        drop(handle);
        assert!(!socket.exists(), "the socket is removed on drop");
        assert!(!discovery::instance_path(&dir, "hmi").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_dead_socket_file_is_replaced_but_a_live_one_is_left_alone() {
        let dir = temp_dir("stale");
        discovery::prepare_dir(&dir).unwrap();
        let socket = discovery::socket_path(&dir, "hmi");
        // A plain file where the socket should be is not connectable.
        std::fs::write(&socket, b"").unwrap();
        assert!(discovery::socket_is_stale(&socket));
        let (handle, _bus) = start(&dir);
        assert!(!discovery::socket_is_stale(&handle.socket));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
