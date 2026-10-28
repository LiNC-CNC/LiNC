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
    /// Was the program paused when the trip was raised?
    ///
    /// The controller aborts the program as part of the trip, so the paused
    /// flag is gone by the time the next poll describes the machine: the answer
    /// has to be recorded here, at the moment the message is ingested. It
    /// decides whether the trip switches the machine off (see
    /// [`ProbeTripFault::switches_machine_off`]).
    ///
    /// Defaults to `false` so a snapshot serialized before this field existed
    /// still decodes, and so a caller that constructs a fault without the
    /// information keeps the older, safer response.
    #[serde(default)]
    pub paused_when_raised: bool,
}

impl ProbeTripFault {
    pub fn new(kind: ProbeTripKind, message: impl Into<String>, raised_at_ms: u64) -> Self {
        Self::new_paused(kind, message, raised_at_ms, false)
    }

    /// A fault that also records the controller's paused flag at raise time.
    pub fn new_paused(
        kind: ProbeTripKind,
        message: impl Into<String>,
        raised_at_ms: u64,
        paused_when_raised: bool,
    ) -> Self {
        Self {
            kind,
            message: message.into(),
            raised_at_ms,
            paused_when_raised,
        }
    }

    pub fn label(&self) -> &'static str {
        self.kind.label()
    }

    /// Does this trip switch the machine off?
    ///
    /// A trip raised while the program was paused leaves the machine alone: the
    /// queue was held mid-block and nothing was aborted that had to be stopped.
    /// That is the case the operator provokes on purpose, with a finger on the
    /// probe, to prove the input is wired.
    pub fn switches_machine_off(&self) -> bool {
        !self.paused_when_raised
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
    /// `paused` is the controller's paused flag as of the last poll *before*
    /// the message: the trip itself aborts the program, so it cannot be read
    /// afterwards.
    ///
    /// Returns the kind that was latched. A non-probe-trip message returns
    /// `None` and leaves any existing fault untouched.
    pub fn raise(&mut self, message: &str, now_ms: u64, paused: bool) -> Option<ProbeTripKind> {
        let kind = ProbeTripKind::from_message(message)?;
        self.fault = Some(ProbeTripFault::new_paused(
            kind,
            message.trim(),
            now_ms,
            paused,
        ));
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
            state.raise(ProbeTripKind::CoordinateJog.message(), 1_000, false),
            Some(ProbeTripKind::CoordinateJog)
        );
        let fault = state.fault().cloned().expect("fault latched");
        assert_eq!(fault.message, "Probe tripped during a coordinate jog.");
        assert_eq!(fault.raised_at_ms, 1_000);

        // No cleaning, acknowledgement or clock advance takes it away.
        for _ in 0..10 {
            state.raise("a later unrelated controller error", u64::MAX, false);
        }
        assert!(state.is_active());
        assert_eq!(state.fault(), Some(&fault));

        assert_eq!(state.clear(), Some(fault));
        assert!(!state.is_active());
        assert_eq!(state.clear(), None);

        // The same trip can be raised again after a clear.
        assert_eq!(
            state.raise(ProbeTripKind::HomingMotion.message(), 2_000, false),
            Some(ProbeTripKind::HomingMotion)
        );
        assert!(state.is_active());
    }

    #[test]
    fn a_later_probe_trip_replaces_the_previous_one() {
        let mut state = ProbeTripState::new();
        state.raise(ProbeTripKind::JointJog.message(), 1, false);
        state.raise(ProbeTripKind::NonProbeMove.message(), 2, false);
        let fault = state.fault().expect("fault latched");
        assert_eq!(fault.kind, ProbeTripKind::NonProbeMove);
        assert_eq!(fault.message, ProbeTripKind::NonProbeMove.message());
    }

    /// The paused flag is the state the trip was raised in, recorded on the
    /// fault itself: the controller aborts the program as part of the trip, so
    /// the next poll can no longer answer the question.
    #[test]
    fn a_fault_records_the_paused_state_it_was_raised_with() {
        let mut state = ProbeTripState::new();
        state.raise(ProbeTripKind::NonProbeMove.message(), 5, true);
        let paused = state.fault().cloned().expect("fault latched");
        assert!(paused.paused_when_raised, "{paused:?}");
        assert!(
            !paused.switches_machine_off(),
            "a trip raised with the program paused leaves the machine on"
        );

        // Clearing and raising again records the new answer, not the old one.
        state.clear();
        state.raise(ProbeTripKind::NonProbeMove.message(), 6, false);
        let running = state.fault().cloned().expect("fault latched");
        assert!(!running.paused_when_raised, "{running:?}");
        assert!(
            running.switches_machine_off(),
            "a trip raised while the program ran switches the machine off"
        );
    }

    /// The field is new: a snapshot written before it existed still decodes, and
    /// keeps the older response (switch the machine off).
    #[test]
    fn an_old_serialized_fault_decodes_with_the_field_defaulted() {
        let old = r#"{
            "fault": {
                "kind": "JointJog",
                "message": "Probe tripped during a joint jog.",
                "raised_at_ms": 42
            }
        }"#;
        let state: ProbeTripState = serde_json::from_str(old).expect("old payload decodes");
        let fault = state.fault().expect("fault latched");
        assert_eq!(fault.raised_at_ms, 42);
        assert!(!fault.paused_when_raised);
        assert!(fault.switches_machine_off());

        // And the new field round-trips.
        let mut raised = ProbeTripState::new();
        raised.raise(ProbeTripKind::JointJog.message(), 42, true);
        let json = serde_json::to_string(&raised).expect("serializes");
        let decoded: ProbeTripState = serde_json::from_str(&json).expect("decodes");
        assert_eq!(decoded, raised);
        assert!(decoded
            .fault()
            .expect("fault latched")
            .paused_when_raised);
    }
}
