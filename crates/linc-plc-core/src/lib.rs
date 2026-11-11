//! Ladder project model, validator, compiler and scan VM.
//!
//! This crate is plain Rust: no HAL, no egui, no LinuxCNC. The daemon owns
//! I/O; the UI owns editing. Both depend on the types here.
//!
//! Scan semantics are deterministic: the VM image is a plain `Vec<bool>`, and
//! `Vm::scan` allocates nothing, takes no locks and performs no I/O. `retain` is
//! carried on `Symbol` and reported over the protocol, but persistence of
//! retained variables across daemon restarts is not implemented in this round.

#![forbid(unsafe_code)]

pub mod compile;
pub mod grid;
pub mod plcopen;
pub mod retain;
pub mod il;
pub mod model;
pub mod protocol;
pub mod validate;
pub mod vm;

pub use compile::{compile, project_hash, Binding, Compiled, Op, Program, Symbol, SymbolKind};
pub use grid::{Cell, POSITIONS};
pub use model::{
    Coil, CoilKind, IoDirection, IoKind, IoPoint, Logic, Pou, PouLanguage, Project, Rung, Section,
    VarDecl, VarKind,
};
pub use protocol::{ProtocolError, Request, Response, RunState, StateSnapshot};
pub use validate::{validate, Diagnostic, Severity};
pub use vm::{ScanStats, VarValue, Vm, WordValue};

/// Schema version written by this crate and accepted by the validator.
pub const SCHEMA_VERSION: u32 = 1;

/// Wire protocol version spoken by the daemon and the client.
/// Bumped when a snapshot gains a field the other side must understand. 2 adds
/// the word bank, 3 the per-scan trend, 4 the single-operand unforce, 5 the trend window.
pub const PROTOCOL_VERSION: u32 = 5;

/// Largest accepted frame payload, in bytes.
pub const MAX_FRAME: usize = 4 * 1024 * 1024;
