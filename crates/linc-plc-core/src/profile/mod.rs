//! The profile seam.
//!
//! Part II of the handoff splits the PLC into a canonical model, a profile
//! (everything that differs between PLC families), a notation (presentation
//! only) and a device (how the images reach the field). M1 starts the
//! strangler: the instruction set moves here as data, so a second dialect is a
//! second table rather than a second set of matches.
//!
//! The `Profile` trait arrives with M2; today this module is the table.

pub mod spec;
