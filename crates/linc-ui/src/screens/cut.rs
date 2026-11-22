//! The CUT card: the rate the machine is being asked to cut at.
//!
//! A machine whose drives publish no load feedback has no meter to draw, and
//! an open-loop joint reports a following error of zero whatever the cut is
//! doing, so neither SERVO LOAD nor FOLLOWING ERROR says anything true about
//! it. What is left is the process itself: the rate in force, the rate the
//! trajectory is actually running, and the chip load that follows from those
//! and the spindle speed. All three are published by a position-only
//! controller, so the card is valid with no encoder anywhere.
//!
//! It is deliberately not a gauge: nothing here is measured. `F 850 → 852`
//! says what the machine was asked for and what it is running, not how hard it
//! is working, and the card never claims a percentage of anything it cannot
//! read.

use egui::Ui;
use linc_core::model::{MachineSnapshot, Positions};

use crate::screens::common::{self, ValueRow};
use crate::shell::LincApp;

/// Axis letters whose velocity is a linear rate. A joint's velocity arrives in
/// machine units per second; `A`, `B` and `C` report degrees per second, so
/// adding one to a feed rate would be adding degrees to millimetres.
const LINEAR_AXES: [char; 6] = ['X', 'Y', 'Z', 'U', 'V', 'W'];

/// What the card shows, computed once per frame from the snapshot.
#[derive(Debug, Clone, PartialEq)]
pub struct CutReadout {
    /// The rate in force: the programmed rate scaled by its override, in machine
    /// units per minute, or per revolution when the mode says so. `None` in G93,
    /// where the F word is inverse time and has no rate to show beside the
    /// commanded speed.
    pub lead: Option<f64>,
    /// The trajectory's own velocity, machine units per minute. A position-only
    /// machine still publishes this: it is the planner's output, not feedback.
    pub commanded: f64,
    pub units: String,
    /// Decimals the rate in force needs: G95 and G93 are fractional where a
    /// units-per-minute rate is whole. The commanded rate is a speed in every
    /// mode, so it is always written whole.
    pub lead_digits: usize,
    /// Feed per spindle revolution, when the active mode defines one.
    pub per_rev: Option<f64>,
    pub per_rev_digits: usize,
    pub rapid_override: f64,
    /// The move in flight is a rapid: the commanded rate is not the cut's.
    pub rapid: bool,
}

/// Read the card's figures from a snapshot.
pub fn readout(snapshot: &MachineSnapshot) -> CutReadout {
    let feed = &snapshot.feed;
    let units = if feed.units.is_empty() {
        "mm".to_string()
    } else {
        feed.units.clone()
    };
    let effective = if feed.feed_override_enabled {
        feed.feed * feed.feed_override
    } else {
        feed.feed
    };
    // G95 is feed per revolution and G93 is inverse time, so both are
    // fractional where a mm/min rate is whole. Same rule as FEED / SPINDLE,
    // which would otherwise print "0 mm/min" for a lathe's 0.008 in/rev.
    let lead_digits = match feed.feed_mode.as_str() {
        "G95" | "G93" => 3,
        _ => 0,
    };
    // The true spindle speed when the machine publishes one, the programmed
    // speed otherwise: a chip load divided by a zero would be no number at all.
    let rpm = snapshot
        .spindles
        .first()
        .map(|spindle| {
            if spindle.actual_speed > 0.0 {
                spindle.actual_speed
            } else {
                spindle.commanded_speed
            }
        })
        .unwrap_or(0.0);
    let per_rev = match feed.feed_mode.as_str() {
        // Already a distance per revolution; the override scales it like any
        // other feed.
        "G95" => Some(effective),
        // Inverse time has no per-revolution meaning to report.
        "G93" => None,
        _ if rpm >= 1.0 => Some(effective / rpm),
        _ => None,
    };
    CutReadout {
        lead: (feed.feed_mode != "G93").then_some(effective),
        commanded: commanded_rate(&snapshot.positions),
        per_rev_digits: if units.starts_with("in") { 4 } else { 3 },
        units,
        lead_digits,
        per_rev,
        rapid_override: feed.rapid_override,
        rapid: snapshot.motion_kind.is_rapid(),
    }
}

/// The resultant of the linear joints' velocities, in machine units per minute.
///
/// The controller reports velocity per joint and per second, so the square sum
/// is scaled by 60. Rotary joints are excluded: their unit is a degree.
fn commanded_rate(positions: &Positions) -> f64 {
    let sum: f64 = positions
        .axes
        .iter()
        .filter(|(axis, _)| {
            axis.chars()
                .next()
                .map(|letter| LINEAR_AXES.contains(&letter.to_ascii_uppercase()))
                .unwrap_or(false)
        })
        .map(|(_, position)| position.velocity * position.velocity)
        .sum();
    sum.sqrt() * 60.0
}

/// The card, in the slot the SERVO LOAD card takes on a machine that has load
/// feedback. Same frame, same width, same height, so the row does not move.
pub fn cut_card(app: &LincApp, ui: &mut Ui, height: f32) {
    let cut = readout(&app.snapshot);
    let rows = card_rows(&cut);
    let body = common::value_card(ui, height, "CUT", None);
    common::paint_value_rows(ui, body, &rows);
}

fn card_rows(cut: &CutReadout) -> Vec<ValueRow> {
    let per_rev = cut.per_rev.map(|value| ValueRow {
        label: "CHIP".into(),
        lead: None,
        value: format!("{:.*}", cut.per_rev_digits, value),
        units: format!("{}/rev", cut.units),
        muted: false,
    });
    vec![
        ValueRow {
            label: "FEED".into(),
            lead: cut
                .lead
                .map(|rate| format!("{:.*} →", cut.lead_digits, rate)),
            // The trajectory's rate is a speed in machine units per minute in
            // every feed mode; only the rate in force changes units.
            value: format!("{:.0}", cut.commanded),
            units: format!("{}/min", cut.units),
            muted: cut.rapid,
        },
        // A mode with no per-revolution figure keeps its row: an absent number
        // is what the mode means, and a missing row would read as a bug.
        per_rev.unwrap_or(ValueRow {
            label: "CHIP".into(),
            lead: None,
            value: "—".into(),
            units: String::new(),
            muted: true,
        }),
        ValueRow {
            label: "RAPID".into(),
            lead: None,
            value: format!("{:.0}", cut.rapid_override * 100.0),
            units: "%".into(),
            muted: false,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use linc_core::model::{FeedState, MachineVariant, MotionKind, PositionSet, SpindleState};

    fn snapshot() -> MachineSnapshot {
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.feed = FeedState {
            feed: 850.0,
            feed_override: 1.0,
            rapid_override: 1.0,
            units: "mm".into(),
            feed_mode: "G94".into(),
            ..Default::default()
        };
        snapshot.spindles.push(SpindleState {
            index: 0,
            name: "MAIN SPINDLE".into(),
            commanded_speed: 4200.0,
            actual_speed: 4200.0,
            load: None,
            override_factor: 1.0,
            direction: 1,
            on: true,
            live_tool: false,
            live_tool_axial: false,
            live_tool_max_rpm: 0.0,
        });
        snapshot
    }

    fn moving(snapshot: &mut MachineSnapshot, axis: &str, velocity: f64) {
        snapshot.positions.axes.insert(
            axis,
            PositionSet {
                velocity,
                ..Default::default()
            },
        );
    }

    #[test]
    fn chip_load_divides_the_rate_in_force_by_the_spindle() {
        let mut snapshot = snapshot();
        snapshot.feed.feed_override = 0.5;
        let cut = readout(&snapshot);
        assert_eq!(cut.lead, Some(425.0), "the override is part of the rate");
        assert_eq!(cut.lead_digits, 0, "a mm/min rate is whole");
        let Some(per_rev) = cut.per_rev else {
            panic!("a running spindle gives the rate a chip load");
        };
        assert!(
            (per_rev - 425.0 / 4200.0).abs() < 1e-9,
            "{per_rev} should be the rate in force over the true speed"
        );
        assert_eq!(cut.per_rev_digits, 3);
    }

    #[test]
    fn a_feed_per_revolution_program_is_already_a_chip_load() {
        let mut snapshot = snapshot();
        snapshot.feed.units = "in".into();
        snapshot.feed.feed = 0.008;
        snapshot.feed.feed_mode = "G95".into();
        let cut = readout(&snapshot);
        assert_eq!(cut.per_rev, Some(0.008));
        assert_eq!(cut.lead_digits, 3, "0.008 rounded to a whole number is 0");
        assert_eq!(cut.per_rev_digits, 4, "an inch per revolution is finer");
    }

    #[test]
    fn inverse_time_and_a_stopped_spindle_have_no_chip_load() {
        let mut snapshot = snapshot();
        snapshot.feed.feed_mode = "G93".into();
        assert_eq!(readout(&snapshot).per_rev, None);
        assert_eq!(
            readout(&snapshot).lead,
            None,
            "G93's F word is inverse time, not a rate to set beside the speed"
        );
        snapshot.feed.feed_mode = "G94".into();
        snapshot.spindles[0].actual_speed = 0.0;
        snapshot.spindles[0].commanded_speed = 0.0;
        assert_eq!(readout(&snapshot).per_rev, None);
        // A programmed speed stands in for a missing true speed.
        snapshot.spindles[0].commanded_speed = 1000.0;
        assert_eq!(readout(&snapshot).per_rev, Some(0.85));
    }

    #[test]
    fn the_commanded_rate_is_the_resultant_of_the_linear_joints() {
        let mut snapshot = snapshot();
        moving(&mut snapshot, "X", 3.0);
        moving(&mut snapshot, "Y", 4.0);
        // Joint velocity is per second: 5 mm/s is 300 mm/min.
        assert!((readout(&snapshot).commanded - 300.0).abs() < 1e-9);
    }

    #[test]
    fn a_rotary_joint_is_not_added_to_a_linear_rate() {
        let mut snapshot = snapshot();
        moving(&mut snapshot, "X", 2.0);
        moving(&mut snapshot, "A", 90.0);
        assert!(
            (readout(&snapshot).commanded - 120.0).abs() < 1e-9,
            "degrees per second are not millimetres per minute"
        );
    }

    #[test]
    fn a_rapid_is_marked_as_one_and_keeps_the_rapid_override() {
        let mut snapshot = snapshot();
        snapshot.feed.rapid_override = 0.25;
        snapshot.motion_kind = MotionKind::Traverse;
        let cut = readout(&snapshot);
        assert!(cut.rapid);
        assert_eq!(cut.rapid_override, 0.25);
        let rows = card_rows(&cut);
        assert!(rows[0].muted, "a rapid's velocity is not a cut rate");
        assert_eq!(rows[2].value, "25");
        snapshot.motion_kind = MotionKind::Feed;
        assert!(!readout(&snapshot).rapid);
        assert!(!card_rows(&readout(&snapshot))[0].muted);
    }

    #[test]
    fn the_rows_name_the_rate_in_force_ahead_of_the_rate_being_run() {
        let mut snapshot = snapshot();
        snapshot.feed.feed_override = 0.5;
        moving(&mut snapshot, "X", 425.0 / 60.0);
        let rows = card_rows(&readout(&snapshot));
        assert_eq!(rows[0].label, "FEED");
        assert_eq!(rows[0].lead.as_deref(), Some("425 →"));
        assert_eq!(rows[0].value, "425");
        assert_eq!(rows[0].units, "mm/min");
        assert_eq!(rows[1].units, "mm/rev");
        assert_eq!(rows[2].units, "%");
        // A mode without a per-revolution figure keeps the row and drops the
        // number, so the card's shape never changes under the eye.
        snapshot.feed.feed_mode = "G93".into();
        let rows = card_rows(&readout(&snapshot));
        assert_eq!(rows[1].value, "—");
        assert!(rows[1].units.is_empty());
        assert!(
            rows[0].lead.is_none(),
            "no inverse-time figure beside a mm/min speed"
        );
        assert_eq!(rows[0].value, "425", "the speed it runs is still a speed");
    }

    #[test]
    fn an_empty_snapshot_reads_as_zero_without_a_chip_load() {
        let cut = readout(&MachineSnapshot::empty(MachineVariant::Mill));
        assert_eq!(cut.commanded, 0.0);
        assert_eq!(cut.per_rev, None);
        assert_eq!(cut.units, "mm", "an unset profile still names a unit");
    }
}
