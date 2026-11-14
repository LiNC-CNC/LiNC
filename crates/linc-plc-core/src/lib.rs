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

pub mod clp;
pub mod compile;
pub mod expr;
pub mod grid;
pub mod il;
pub mod model;
pub mod plcopen;
pub mod profile;
pub mod protocol;
pub mod retain;
pub mod timing;
pub mod validate;
pub mod vm;

pub use compile::{
    compile, project_hash, Bank, Binding, Compiled, Op, PresetSource, Program, Symbol, SymbolKind,
    WordLayout,
};
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
/// the word bank, 3 the per-scan trend, 4 the single-operand unforce, 5 the trend window, 6 scan timing.
/// 8 widens the word bank and the `WordValue` wire type to 32 bits; a 7 client
/// would decode `MW0 = 100000` as a truncated value, so the mismatch is refused.
pub const PROTOCOL_VERSION: u32 = 8;

/// Largest accepted frame payload, in bytes.
pub const MAX_FRAME: usize = 4 * 1024 * 1024;
