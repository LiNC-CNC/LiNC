//! Blocking client for the diagnostic socket.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use linc_core::diag::DiagError;
use serde_json::Value;

use crate::MAX_FRAME;

pub struct Client {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
    next_id: u64,
    timeout_ms: u64,
}

impl Client {
    pub fn connect(socket: &Path) -> std::io::Result<Self> {
        let stream = UnixStream::connect(socket)?;
        let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(5_000)));
        let _ = stream.set_write_timeout(Some(std::time::Duration::from_millis(5_000)));
        let reader = BufReader::new(stream.try_clone()?);
        Ok(Self {
            stream,
            reader,
            next_id: 1,
            timeout_ms: 2_000,
        })
    }

    /// Discover and connect in one step.
    pub fn discover(role: &str, override_path: Option<&Path>) -> Result<Self, DiagError> {
        let socket = crate::discovery::discover(role, override_path).ok_or_else(|| {
            DiagError::new(
                "LINC_NOT_RUNNING",
                "no diagnostic socket found; start linc first",
                true,
            )
        })?;
        Self::connect(&socket).map_err(|error| {
            DiagError::new(
                "LINC_NOT_RUNNING",
                format!("cannot connect to {}: {error}", socket.display()),
                true,
            )
        })
    }

    pub fn with_timeout(mut self, timeout_ms: u64) -> Self {
        self.timeout_ms = timeout_ms;
        let _ = self
            .stream
            .set_read_timeout(Some(std::time::Duration::from_millis(timeout_ms.max(50))));
        self
    }

    /// Call a method and return the envelope.
    pub fn call(&mut self, method: &str, params: Value) -> Result<Value, DiagError> {
        let id = self.next_id;
        self.next_id += 1;
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut bytes = serde_json::to_vec(&request)
            .map_err(|error| DiagError::internal(format!("encode request: {error}")))?;
        bytes.push(b'\n');
        self.stream
            .write_all(&bytes)
            .and_then(|_| self.stream.flush())
            .map_err(|error| {
                DiagError::new(
                    "LINC_NOT_RUNNING",
                    format!("write to diagnostic socket failed: {error}"),
                    true,
                )
            })?;
        let mut line = String::new();
        let read = self.reader.read_line(&mut line).map_err(|error| {
            let code = if error.kind() == std::io::ErrorKind::TimedOut {
                "TIMEOUT"
            } else {
                "LINC_NOT_RUNNING"
            };
            DiagError::new(code, format!("read from diagnostic socket failed: {error}"), true)
        })?;
        if read == 0 {
            return Err(DiagError::new(
                "LINC_NOT_RUNNING",
                "the diagnostic socket closed the connection",
                true,
            ));
        }
        if line.len() > MAX_FRAME {
            return Err(DiagError::internal("response exceeds the frame limit"));
        }
        let response: Value = serde_json::from_str(line.trim())
            .map_err(|error| DiagError::internal(format!("bad response: {error}")))?;
        if let Some(error) = response.get("error") {
            let detail = error.get("data").cloned().unwrap_or_else(|| error.clone());
            return Err(serde_json::from_value::<DiagError>(detail).unwrap_or_else(|_| {
                DiagError::internal(error.get("message").and_then(Value::as_str).unwrap_or("error"))
            }));
        }
        response
            .get("result")
            .cloned()
            .ok_or_else(|| DiagError::internal("response has neither result nor error"))
    }

    pub fn timeout_ms(&self) -> u64 {
        self.timeout_ms
    }
}
