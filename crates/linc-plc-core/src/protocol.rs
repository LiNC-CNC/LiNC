//! Wire protocol: JSON messages in 4-byte big-endian length-prefixed frames.

use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::model::Project;
use crate::validate::Diagnostic;
use crate::vm::VarValue;

pub use crate::{MAX_FRAME, PROTOCOL_VERSION};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", content = "data", rename_all = "snake_case")]
pub enum Request {
    Hello {
        protocol: u32,
    },
    Validate {
        project: Project,
    },
    Load {
        project: Project,
    },
    Start,
    Stop,
    Snapshot,
    Force {
        name: String,
        value: bool,
        ttl_ms: u32,
    },
    ClearForces,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "response", content = "data", rename_all = "snake_case")]
pub enum Response {
    Hello {
        protocol: u32,
        schema: u32,
        daemon: String,
        hal: bool,
    },
    Validated {
        ok: bool,
        diagnostics: Vec<Diagnostic>,
    },
    Loaded {
        hash: String,
        diagnostics: Vec<Diagnostic>,
    },
    State(StateSnapshot),
    Error {
        code: String,
        message: String,
    },
    Bye,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateSnapshot {
    pub state: RunState,
    pub project: String,
    pub hash: String,
    pub scan_count: u64,
    pub scan_time_us: u32,
    pub overruns: u64,
    pub alive: bool,
    pub fault: Option<String>,
    pub vars: Vec<VarValue>,
    pub forced: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Idle,
    Loaded,
    Running,
    Fault,
}

#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("io error: {0}")]
    Io(#[from] io::Error),
    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("frame too large")]
    FrameTooLarge,
    #[error("connection closed")]
    Eof,
}

impl ProtocolError {
    /// Stable short code, for logs and for mapping onto `Response::Error`.
    pub fn kind(&self) -> &'static str {
        match self {
            ProtocolError::Io(_) => "io",
            ProtocolError::Json(_) => "json",
            ProtocolError::FrameTooLarge => "frame_too_large",
            ProtocolError::Eof => "eof",
        }
    }
}

pub fn write_frame<W: Write>(writer: &mut W, payload: &[u8]) -> Result<(), ProtocolError> {
    if payload.len() > MAX_FRAME {
        return Err(ProtocolError::FrameTooLarge);
    }
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame<R: Read>(reader: &mut R) -> Result<Vec<u8>, ProtocolError> {
    let mut header = [0u8; 4];
    let mut filled = 0usize;
    while filled < header.len() {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Err(ProtocolError::Eof),
            Ok(0) => {
                return Err(ProtocolError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "truncated frame header",
                )))
            }
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(ProtocolError::Io(error)),
        }
    }

    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_FRAME {
        return Err(ProtocolError::FrameTooLarge);
    }
    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

pub fn write_message<W: Write, T: Serialize + ?Sized>(
    writer: &mut W,
    message: &T,
) -> Result<(), ProtocolError> {
    let payload = serde_json::to_vec(message)?;
    write_frame(writer, &payload)
}

pub fn read_message<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, ProtocolError> {
    let payload = read_frame(reader)?;
    Ok(serde_json::from_slice(&payload)?)
}
