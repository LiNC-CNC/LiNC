//! Counter policy: what a counter's members are and how its value steps.
//!
//! Two dialects disagree about counters, and the disagreement is *data*: the
//! compiler bakes a [`CounterRule`] and a member table into the program, and the
//! scan executes the mechanics it is handed. `vm.rs` never asks which dialect it
//! is running and never calls back into a profile — Appendix D's rule.
//!
//! `iec`: one count input on the block, saturating, `Q = CV >= PV` (`CTU`) or
//! `CV <= 0` (`CTD`), members `.Q`, `.R`, `.LD`.
//!
//! `classicladder`: the value is stepped by member coils (`.CU`/`.CD`) and held
//! at rest by `.R`/`.L`, it wraps at `0..=9999`, `done` is equality, and it has
//! the empty (`.E`) and full (`.F`) outputs. `Q` is `CV == PV`.

use crate::profile::resolve;
use crate::profile::spec::FbName;

/// A bit-valued block member: what a contact reads or a coil writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitMember {
    /// `Q`: the block's done output.
    Done,
    /// `E`: ClassicLadder's empty output, `CV == 0`.
    Empty,
    /// `F`: ClassicLadder's full output, `CV == top`.
    Full,
    /// `CU`: count up, on the power bit's rising edge.
    Up,
    /// `CD`: count down, on the power bit's rising edge.
    Down,
    /// `R`: reset to zero, at level.
    Reset,
    /// `L`/`LD`: load the preset, at level.
    Load,
}

impl BitMember {
    /// A member a coil may write. The rest are read-only outputs.
    pub fn is_writable(self) -> bool {
        matches!(self, Self::Up | Self::Down | Self::Reset | Self::Load)
    }
}

/// Where a counter's steps come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterSteps {
    /// The block's own input edge (`iec`): one input, stepped by the block.
    Block,
    /// Member coils (`classicladder`): the block re-asserts its preset and
    /// computes `done`, the coils step the value.
    Coils,
}

/// What a counter's `done` compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CounterDone {
    /// `CV >= PV` (`CTU`).
    AtLeastPreset,
    /// `CV <= 0` (`CTD`).
    AtMostZero,
    /// `CV == PV` (ClassicLadder).
    EqualsPreset,
}

/// A counter's mechanics, baked by the compiler from the profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CounterRule {
    pub steps: CounterSteps,
    pub done: CounterDone,
}

impl CounterRule {
    /// Would a step in this direction change the value? Only `Coils` counters
    /// step, so a block-stepped counter answers false.
    pub fn steps_on_coils(self) -> bool {
        self.steps == CounterSteps::Coils
    }
}

/// `iec`'s member table: the done output, the reset and the preset load.
pub fn iec_member(text: &str) -> Option<(u16, BitMember)> {
    let (instance, member) = split(text)?;
    let member = match member.to_ascii_uppercase().as_str() {
        "Q" => BitMember::Done,
        "R" => BitMember::Reset,
        "LD" => BitMember::Load,
        _ => return None,
    };
    Some((instance, member))
}

/// ClassicLadder's member table: done, empty, full, and the four inputs.
pub fn classicladder_member(text: &str) -> Option<(u16, BitMember)> {
    let (instance, member) = split(text)?;
    let member = match member.to_ascii_uppercase().as_str() {
        "Q" => BitMember::Done,
        "E" => BitMember::Empty,
        "F" => BitMember::Full,
        "CU" => BitMember::Up,
        "CD" => BitMember::Down,
        "R" => BitMember::Reset,
        "L" => BitMember::Load,
        _ => return None,
    };
    Some((instance, member))
}

/// Split `<instance>.<member>` and resolve the instance to its ABI slot.
fn split(text: &str) -> Option<(u16, &str)> {
    let text = text.trim();
    let (rest, member) = text.rsplit_once('.')?;
    if member.is_empty() {
        return None;
    }
    let (_, instance, _) = resolve::parse_block(rest, rest, "")?;
    Some((instance, member))
}

/// `iec`'s counter rule for a kind. Timers and latches never ask.
pub fn iec_rule(kind: FbName) -> CounterRule {
    CounterRule {
        steps: CounterSteps::Block,
        done: match kind {
            FbName::Ctd => CounterDone::AtMostZero,
            _ => CounterDone::AtLeastPreset,
        },
    }
}

/// ClassicLadder's counter rule: coils step, done is equality.
pub fn classicladder_rule(_kind: FbName) -> CounterRule {
    CounterRule {
        steps: CounterSteps::Coils,
        done: CounterDone::EqualsPreset,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        classicladder_member, classicladder_rule, iec_member, iec_rule, BitMember, CounterDone,
        CounterSteps,
    };
    use crate::profile::spec::FbName;

    #[test]
    fn the_two_dialects_spell_their_members_differently() {
        // `CTU` is kind 3 in the ABI order, so its four instances are slots
        // 12..16; deriving the number keeps the test readable.
        let ctu = usize::from(FbName::Ctu.code()) * usize::from(crate::profile::FB_PER_TYPE);
        let ctu = ctu as u16;
        assert_eq!(iec_member("CTU0.Q"), Some((ctu, BitMember::Done)));
        assert_eq!(iec_member("CTU0.R"), Some((ctu, BitMember::Reset)));
        assert_eq!(iec_member("CTU3.LD"), Some((ctu + 3, BitMember::Load)));
        assert_eq!(iec_member("CTU0.L"), None, "iec loads with `LD`");
        assert_eq!(iec_member("CTU0.CU"), None, "iec has no count coil");
        assert_eq!(iec_member("CTU0.E"), None, "iec has no empty output");

        assert_eq!(classicladder_member("CTU0.Q"), Some((ctu, BitMember::Done)));
        assert_eq!(
            classicladder_member("CTU1.CU"),
            Some((ctu + 1, BitMember::Up))
        );
        assert_eq!(
            classicladder_member("CTU0.CD"),
            Some((ctu, BitMember::Down))
        );
        assert_eq!(classicladder_member("CTU0.L"), Some((ctu, BitMember::Load)));
        assert_eq!(classicladder_member("CTU0.LD"), None, "CL loads with `L`");
        assert_eq!(classicladder_member("CTU0.Z"), None);
        assert_eq!(classicladder_member("NOPE.Q"), None, "not an instance");
    }

    #[test]
    fn a_dialect_bakes_its_own_rule() {
        assert_eq!(
            iec_rule(FbName::Ctu),
            super::CounterRule {
                steps: CounterSteps::Block,
                done: CounterDone::AtLeastPreset,
            }
        );
        assert_eq!(iec_rule(FbName::Ctd).done, CounterDone::AtMostZero);
        assert!(!iec_rule(FbName::Ctu).steps_on_coils());

        assert_eq!(
            classicladder_rule(FbName::Ctu),
            super::CounterRule {
                steps: CounterSteps::Coils,
                done: CounterDone::EqualsPreset,
            }
        );
        assert!(classicladder_rule(FbName::Ctu).steps_on_coils());
    }
}
