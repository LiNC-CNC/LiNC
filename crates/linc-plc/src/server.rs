//! The Unix-socket front end: framing, request dispatch and project persistence.
//!
//! One listener thread accepts connections and serves them one at a time; the
//! UI opens one connection and keeps it. Validation and compilation happen
//! here because they need no HAL; every state change is forwarded to the scan
//! loop with a reply channel.

use std::fs;
use std::io::{self, Write};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use linc_plc_core::protocol::{self, Request, Response, PROTOCOL_VERSION};
use linc_plc_core::{Project, ProtocolError, SCHEMA_VERSION};

use crate::{evaluate, summarize, Command, Error, Shared};

/// How long a connection waits for the scan loop to answer a command.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// How often the listener checks the shutdown flag while no client connects.
const ACCEPT_POLL: Duration = Duration::from_millis(5);
/// A connection that sends nothing for this long is dropped.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Create the socket: parent directory, stale-socket cleanup, bind, chmod 0600.
pub(crate) fn bind(path: &Path) -> Result<UnixListener, Error> {
    if let Some(dir) = path.parent() {
        if !dir.as_os_str().is_empty() {
            fs::create_dir_all(dir)?;
        }
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => {
            log::info!("linc-plc: removing the stale socket {}", path.display());
            fs::remove_file(path)?;
        }
        Ok(_) => return Err(Error::SocketPath(path.display().to_string())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    log::info!("linc-plc: serving {}", path.display());
    Ok(listener)
}

/// Accept connections until the shutdown flag is set.
pub(crate) fn serve(listener: UnixListener, shared: Arc<Shared>) {
    while !shared.shutdown.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _address)) => {
                if let Err(error) = handle(stream, &shared) {
                    log::warn!("linc-plc: connection ended with an error: {error}");
                }
            }
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                thread::sleep(ACCEPT_POLL);
            }
            Err(error) => {
                log::error!("linc-plc: accept failed: {error}");
                thread::sleep(ACCEPT_POLL);
            }
        }
    }
}

/// Serve one connection until it closes, times out, or asks for Shutdown.
fn handle(mut stream: UnixStream, shared: &Shared) -> Result<(), Error> {
    stream.set_read_timeout(Some(IDLE_TIMEOUT))?;
    stream.set_write_timeout(Some(REPLY_TIMEOUT))?;
    loop {
        let frame = match protocol::read_frame(&mut stream) {
            Ok(frame) => frame,
            Err(ProtocolError::Eof) => return Ok(()),
            Err(ProtocolError::Io(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::ConnectionReset
                ) =>
            {
                return Ok(());
            }
            Err(error) => {
                log::warn!("linc-plc: cannot read a request: {error}");
                return Ok(());
            }
        };
        let request: Request = match serde_json::from_slice(&frame) {
            Ok(request) => request,
            Err(error) => {
                log::warn!("linc-plc: malformed request: {error}");
                send(
                    &mut stream,
                    &Response::Error {
                        code: "protocol".to_string(),
                        message: format!("malformed request: {error}"),
                    },
                )?;
                continue;
            }
        };
        match handle_request(request, shared)? {
            Outcome::Reply(response) => send(&mut stream, &response)?,
            Outcome::Bye => {
                send(&mut stream, &Response::Bye)?;
                return Ok(());
            }
        }
    }
}

enum Outcome {
    Reply(Response),
    Bye,
}

fn handle_request(request: Request, shared: &Shared) -> Result<Outcome, Error> {
    let outcome = match request {
        Request::Hello { protocol } => {
            if protocol != PROTOCOL_VERSION {
                Outcome::Reply(Response::Error {
                    code: "protocol".to_string(),
                    message: format!(
                        "protocol {protocol} does not match daemon protocol {PROTOCOL_VERSION}",
                    ),
                })
            } else {
                Outcome::Reply(Response::Hello {
                    protocol: PROTOCOL_VERSION,
                    schema: SCHEMA_VERSION,
                    daemon: env!("CARGO_PKG_VERSION").to_string(),
                    hal: !shared.no_hal,
                })
            }
        }

        Request::Validate { project } => {
            let (ok, diagnostics, _) = evaluate(&project);
            Outcome::Reply(Response::Validated { ok, diagnostics })
        }

        Request::Load { project } => Outcome::Reply(load(project, shared)?),

        Request::Start => Outcome::Reply(dispatch(shared, |reply| Command::Start { reply })),

        Request::Stop => Outcome::Reply(dispatch(shared, |reply| Command::Stop { reply })),

        Request::Force {
            name,
            value,
            ttl_ms,
        } => {
            if !shared.allow_force {
                Outcome::Reply(Response::Error {
                    code: "force_disabled".to_string(),
                    message: "force is disabled; restart linc-plc with --allow-force".to_string(),
                })
            } else {
                Outcome::Reply(dispatch(shared, |reply| Command::Force {
                    name,
                    value,
                    ttl_ms,
                    reply,
                }))
            }
        }

        Request::ClearForces => {
            Outcome::Reply(dispatch(shared, |reply| Command::ClearForces { reply }))
        }

        Request::Snapshot => Outcome::Reply(match shared.snapshot.lock() {
            Ok(snapshot) => Response::State(snapshot.clone()),
            Err(_) => Response::Error {
                code: "internal".to_string(),
                message: "the snapshot lock is poisoned".to_string(),
            },
        }),

        Request::Shutdown => {
            shared.shutdown.store(true, Ordering::SeqCst);
            Outcome::Bye
        }
    };
    Ok(outcome)
}

/// Compile, check the frozen I/O map, persist, then hand the program to the
/// scan loop for a scan-boundary swap.
fn load(project: Project, shared: &Shared) -> Result<Response, Error> {
    let (ok, diagnostics, compiled) = evaluate(&project);
    let Some(compiled) = compiled else {
        return Ok(Response::Error {
            code: "invalid".to_string(),
            message: summarize(&diagnostics),
        });
    };
    if !ok {
        return Ok(Response::Error {
            code: "invalid".to_string(),
            message: summarize(&diagnostics),
        });
    }
    if !shared.io_map.accepts(&compiled) {
        let wanted = describe_map(&project);
        return Ok(Response::Error {
            code: "io_map_locked".to_string(),
            message: format!(
                "the project's I/O bindings ({wanted}) differ from the pin map this daemon \
                 created at startup; restart linc-plc to bind them"
            ),
        });
    }
    if let Err(error) = persist(&shared.project, &project) {
        return Ok(Response::Error {
            code: "persist".to_string(),
            message: error.to_string(),
        });
    }

    let warnings = compiled.warnings.clone();
    let response = dispatch(shared, |reply| Command::Load {
        compiled: Box::new(compiled),
        warnings,
        reply,
    });
    Ok(response)
}

/// Send one command to the scan loop and wait a bounded time for its answer.
fn dispatch(shared: &Shared, build: impl FnOnce(Sender<Response>) -> Command) -> Response {
    let (reply, answer) = mpsc::channel();
    if shared.commands.send(build(reply)).is_err() {
        return Response::Error {
            code: "stopped".to_string(),
            message: "the scan loop is not running".to_string(),
        };
    }
    match answer.recv_timeout(REPLY_TIMEOUT) {
        Ok(response) => response,
        Err(RecvTimeoutError::Timeout) => Response::Error {
            code: "timeout".to_string(),
            message: "the scan loop did not answer in time".to_string(),
        },
        Err(RecvTimeoutError::Disconnected) => Response::Error {
            code: "stopped".to_string(),
            message: "the scan loop stopped".to_string(),
        },
    }
}

/// Write the project atomically: temp file in the same directory, fsync,
/// rename, then fsync the directory so the rename itself is durable.
fn persist(path: &Path, project: &Project) -> Result<(), Error> {
    let json = serde_json::to_vec_pretty(project)
        .map_err(|error| Error::Project(format!("cannot serialize the project: {error}")))?;
    let directory = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    fs::create_dir_all(directory)?;
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ladder.json".to_string());
    let temporary = directory.join(format!(".{name}.tmp-{}", std::process::id()));

    let written = (|| -> io::Result<()> {
        let mut file = fs::File::create(&temporary)?;
        file.write_all(&json)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        Ok(())
    })();
    if let Err(error) = written {
        let _ = fs::remove_file(&temporary);
        return Err(error.into());
    }
    match fs::File::open(directory) {
        Ok(handle) => {
            if let Err(error) = handle.sync_all() {
                log::warn!("linc-plc: cannot fsync {}: {error}", directory.display());
            }
        }
        Err(error) => log::warn!(
            "linc-plc: cannot open {} for fsync: {error}",
            directory.display()
        ),
    }
    log::info!("linc-plc: saved the project to {}", path.display());
    Ok(())
}

fn describe_map(project: &Project) -> String {
    let inputs: Vec<&str> = project
        .io
        .iter()
        .filter(|point| matches!(point.direction, linc_plc_core::IoDirection::In))
        .map(|point| point.name.as_str())
        .collect();
    let outputs: Vec<&str> = project
        .io
        .iter()
        .filter(|point| matches!(point.direction, linc_plc_core::IoDirection::Out))
        .map(|point| point.name.as_str())
        .collect();
    format!("in: [{}], out: [{}]", inputs.join(", "), outputs.join(", "))
}

fn send(stream: &mut UnixStream, response: &Response) -> Result<(), Error> {
    let bytes = serde_json::to_vec(response)
        .map_err(|error| Error::Project(format!("cannot serialize a response: {error}")))?;
    protocol::write_frame(stream, &bytes)?;
    stream.flush()?;
    Ok(())
}
