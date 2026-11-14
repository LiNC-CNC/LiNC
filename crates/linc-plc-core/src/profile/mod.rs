//! The profile seam.
//!
//! Part II of the handoff splits the PLC into a canonical model, a profile
//! (everything that differs between PLC families), a notation (presentation
//! only) and a device (how the images reach the field).
//!
//! M1 put the instruction set here as data ([`spec`]). M2 put operand
//! resolution here ([`resolve`]) and the [`Profile`] interface that names both.
//! M3–M5 add the memory layout, the instruction-validity split and the device.
//!
//! Nothing in this module depends on `compile`, `vm` or `validate`: a dialect is
//! data and a few pure functions, and the modules that use it map that data onto
//! their own types. That direction is what lets the rest of the strangler move
//! without a cycle.

pub mod resolve;
pub mod spec;

use std::collections::HashMap;

use resolve::{Operand, OperandError};
use spec::Spec;

/// Instances per function-block kind. Part of the dialect: the instance index is
/// `kind * FB_PER_TYPE + number`, and the editor offers exactly this many.
pub const FB_PER_TYPE: u16 = 4;

/// Declared word names, lowercased, mapped to their bank slot. A word or float
/// point resolves through it by the name the project gave it.
pub type SymbolLookup = HashMap<String, u16>;

/// Everything that differs between PLC families.
///
/// Small on purpose; a cell is added when a second dialect needs it. `layout`,
/// `intern`, `languages` and `check_instruction` arrive with M3–M5, in the
/// handoff's order.
pub trait Profile {
    fn name(&self) -> &'static str;

    /// The instruction set, as data.
    fn specs(&self) -> &'static [Spec];

    /// One instruction row, case-insensitively.
    fn spec(&self, mnemonic: &str) -> Option<&'static Spec>;

    /// Resolve one operand's text to something the compiler can address.
    fn resolve(&self, text: &str, symbols: &SymbolLookup) -> Result<Operand, OperandError>;
}

/// The dialect LiNC ships: the ZX-derived instruction set, its sixteen marker
/// words and its block members.
#[derive(Debug, Clone, Copy, Default)]
pub struct Iec;

impl Profile for Iec {
    fn name(&self) -> &'static str {
        "iec"
    }

    fn specs(&self) -> &'static [Spec] {
        spec::SPECS
    }

    fn spec(&self, mnemonic: &str) -> Option<&'static Spec> {
        spec::spec(mnemonic)
    }

    fn resolve(&self, text: &str, symbols: &SymbolLookup) -> Result<Operand, OperandError> {
        resolve::resolve(text, symbols)
    }
}

/// The profile the compiler, the validator and the editor read.
pub fn profile() -> &'static Iec {
    &IEC
}

static IEC: Iec = Iec;
