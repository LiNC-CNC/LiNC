//! Latching operator faults derived from controller messages.
//!
//! LinuxCNC reports a probe trip as a one-shot operator error and aborts the
//! move itself. The error channel does not carry a later "cleared" message, so
//! the HMI has to latch the fault when the text arrives and keep it until the
//! operator performs the controller reset that clears the motion error
//! (`EMC_TASK_ABORT`, the same action as the panel's K4/Esc program reset).
//!
//! Nothing in this module takes a clock or a timeout: a fault cannot expire.

use serde::{Deserialize, Serialize};

/// The four probe-trip messages LinuxCNC 2.9 emits from
/// `src/emc/motion/control.c` (`process_probe_inputs`).
///
/// The text is part of the operator contract: LinuxCNC sends exactly these
/// strings on the NML error channel, and the Messages page shows them as sent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProbeTripKind {
    /// `Probe tripped during non-probe move.`
    NonProbeMove,
    /// `Probe tripped during homing motion.`
    HomingMotion,
    /// `Probe tripped during a joint jog.`
    JointJog,
    /// `Probe tripped during a coordinate jog.`
    CoordinateJog,
}

impl ProbeTripKind {
    pub const ALL: [Self; 4] = [
        Self::NonProbeMove,
        Self::HomingMotion,
        Self::JointJog,
        Self::CoordinateJog,
    ];

    /// The exact LinuxCNC wording for this trip.
    pub fn message(self) -> &'static str {
        match self {
            Self::NonProbeMove => "Probe tripped during non-probe move.",
            Self::HomingMotion => "Probe tripped during homing motion.",
            Self::JointJog => "Probe tripped during a joint jog.",
            Self::CoordinateJog => "Probe tripped during a coordinate jog.",
        }
    }

    /// Short label for the banner; the controller text is shown separately.
    pub fn label(self) -> &'static str {
        match self {
            Self::NonProbeMove => "NON-PROBE MOVE",
            Self::HomingMotion => "HOMING",
            Self::JointJog => "JOINT JOG",
            Self::CoordinateJog => "COORDINATE JOG",
        }
    }

    /// Classify one NML error message. Only the four exact wordings match, so
    /// an unrelated controller error is never mistaken for a probe trip.
    pub fn from_message(text: &str) -> Option<Self> {
        let text = text.trim();
        Self::ALL
            .into_iter()
            .find(|kind| kind.message().eq_ignore_ascii_case(text))
    }
}

/// A latched probe trip. `message` is the controller's text, kept verbatim for
/// the Messages page and the banner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeTripFault {
    pub kind: ProbeTripKind,
    pub message: String,
    pub raised_at_ms: u64,
}

impl ProbeTripFault {
    pub fn new(kind: ProbeTripKind, message: impl Into<String>, raised_at_ms: u64) -> Self {
        Self {
            kind,
            message: message.into(),
            raised_at_ms,
        }
    }

    pub fn label(&self) -> &'static str {
        self.kind.label()
    }
}

/// The latched fault, if any.
///
/// State is deliberately a single `Option`: there is no age, deadline or
/// timeout. [`ProbeTripState::clear`] is the only way the fault goes away.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeTripState {
    fault: Option<ProbeTripFault>,
}

impl ProbeTripState {
    pub fn new() -> Self {
        Self::default()
    }

    /// Latch a probe trip if `message` is one of the four LinuxCNC wordings.
    ///
    /// Returns the kind that was latched. A non-probe-trip message returns
    /// `None` and leaves any existing fault untouched.
    pub fn raise(&mut self, message: &str, now_ms: u64) -> Option<ProbeTripKind> {
        let kind = ProbeTripKind::from_message(message)?;
        self.fault = Some(ProbeTripFault::new(kind, message.trim(), now_ms));
        Some(kind)
    }

    /// Clear the fault. This is only called by the controller reset path, never
    /// by a timer or a poll.
    pub fn clear(&mut self) -> Option<ProbeTripFault> {
        self.fault.take()
    }

    pub fn fault(&self) -> Option<&ProbeTripFault> {
        self.fault.as_ref()
    }

    pub fn is_active(&self) -> bool {
        self.fault.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_linuxcnc_probe_trip_wording_is_classified() {
        for kind in ProbeTripKind::ALL {
            assert_eq!(
                ProbeTripKind::from_message(kind.message()),
                Some(kind),
                "{:?}",
                kind
            );
        }
    }

    #[test]
    fn classification_ignores_surrounding_whitespace_but_not_other_errors() {
        assert_eq!(
            ProbeTripKind::from_message("  Probe tripped during a joint jog.\n"),
            Some(ProbeTripKind::JointJog)
        );
        for text in [
            "G38.2 move finished without making contact.",
            "Probe tripped during probing.",
            "probe tripped during a jog",
            "",
        ] {
            assert_eq!(ProbeTripKind::from_message(text), None, "{text:?}");
        }
    }

    #[test]
    fn a_fault_never_expires_and_clears_only_explicitly() {
        let mut state = ProbeTripState::new();
        assert_eq!(
            state.raise(ProbeTripKind::CoordinateJog.message(), 1_000),
            Some(ProbeTripKind::CoordinateJog)
        );
        let fault = state.fault().cloned().expect("fault latched");
        assert_eq!(fault.message, "Probe tripped during a coordinate jog.");
        assert_eq!(fault.raised_at_ms, 1_000);

        // No cleaning, acknowledgement or clock advance takes it away.
        for _ in 0..10 {
            state.raise("a later unrelated controller error", u64::MAX);
        }
        assert!(state.is_active());
        assert_eq!(state.fault(), Some(&fault));

        assert_eq!(state.clear(), Some(fault));
        assert!(!state.is_active());
        assert_eq!(state.clear(), None);

        // The same trip can be raised again after a clear.
        assert_eq!(
            state.raise(ProbeTripKind::HomingMotion.message(), 2_000),
            Some(ProbeTripKind::HomingMotion)
        );
        assert!(state.is_active());
    }

    #[test]
    fn a_later_probe_trip_replaces_the_previous_one() {
        let mut state = ProbeTripState::new();
        state.raise(ProbeTripKind::JointJog.message(), 1);
        state.raise(ProbeTripKind::NonProbeMove.message(), 2);
        let fault = state.fault().expect("fault latched");
        assert_eq!(fault.kind, ProbeTripKind::NonProbeMove);
        assert_eq!(fault.message, ProbeTripKind::NonProbeMove.message());
    }
}
