//! Probe Basic-compatible operation catalogue, parameter contract and G-code
//! call construction.
//!
//! The HMI never auto-starts a routine. The UI calls [`build_probe_blocks`]
//! only from the explicit `RUN PROBE` action after validation.

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeCategory {
    OutsideCorners,
    InsideCorners,
    BossAndPocket,
    RidgeAndValley,
    EdgeAngle,
    RotaryAxis,
    SurfaceZ,
    Calibrate,
    ProbeHelp,
}

impl ProbeCategory {
    pub const ALL: [ProbeCategory; 7] = [
        Self::OutsideCorners,
        Self::InsideCorners,
        Self::BossAndPocket,
        Self::RidgeAndValley,
        Self::EdgeAngle,
        Self::Calibrate,
        Self::ProbeHelp,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::OutsideCorners => "OUTSIDE CORNERS",
            Self::InsideCorners => "INSIDE CORNERS",
            Self::BossAndPocket => "BOSS AND POCKET",
            Self::RidgeAndValley => "RIDGE AND VALLEY",
            Self::EdgeAngle => "EDGE ANGLE",
            Self::RotaryAxis => "ROTARY AXIS",
            Self::SurfaceZ => "SURFACE / Z",
            Self::Calibrate => "CALIBRATE",
            Self::ProbeHelp => "PROBE HELP",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeDiagram {
    Corner,
    InsideCorner,
    RoundBoss,
    RoundPocket,
    RectBoss,
    RectPocket,
    Ridge,
    Valley,
    EdgeAngle,
    Surface,
    Calibration,
    Help,
}

/// One work-coordinate value determined by a probe routine. Most values come
/// from the Probe Basic result slots; angle routines additionally use the
/// controller-side context captured by `linc_probe_context`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApplySource {
    Slot(u16),
    SlotMinusDelta(u16),
    SlotPlusDelta(u16),
    StartX,
    StartY,
    CornerX,
    CornerY,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApplyTerm {
    pub axis: &'static str,
    pub source: ApplySource,
}

impl ApplySource {
    pub fn value(
        self,
        results: &ParameterResults,
        auxiliary: &std::collections::BTreeMap<String, f64>,
    ) -> Option<f64> {
        let slot = |parameter: u16| {
            (1001..=1012)
                .contains(&parameter)
                .then(|| results.values[(parameter - 1001) as usize])
        };
        match self {
            Self::Slot(parameter) => slot(parameter),
            Self::SlotMinusDelta(parameter) => Some(slot(parameter)? - slot(1011)?),
            Self::SlotPlusDelta(parameter) => Some(slot(parameter)? + slot(1011)?),
            Self::StartX => auxiliary.get("start_x").copied(),
            Self::StartY => auxiliary.get("start_y").copied(),
            Self::CornerX => auxiliary.get("corner_x").copied(),
            Self::CornerY => auxiliary.get("corner_y").copied(),
        }
    }

    /// Work-coordinate expression evaluated by LinuxCNC during explicit Apply.
    pub fn work_expression(self) -> String {
        match self {
            Self::Slot(parameter) => format!("#{parameter}"),
            Self::SlotMinusDelta(parameter) => format!("[#{parameter} - #1011]"),
            Self::SlotPlusDelta(parameter) => format!("[#{parameter} + #1011]"),
            Self::StartX => "#<_linc_start_x>".into(),
            Self::StartY => "#<_linc_start_y>".into(),
            Self::CornerX => "#<_linc_corner_x>".into(),
            Self::CornerY => "#<_linc_corner_y>".into(),
        }
    }

    /// Convert the work-coordinate value to the raw offset expected by G10 L2,
    /// using the origin captured before this exact probe run.
    pub fn raw_expression(self, axis: &str) -> String {
        format!(
            "[{} + #<_linc_probe_{}_offset>]",
            self.work_expression(),
            axis.to_ascii_lowercase()
        )
    }
}

#[derive(Clone, Debug)]
pub struct ProbeOperation {
    pub id: &'static str,
    pub name: &'static str,
    pub call: &'static str,
    pub category: ProbeCategory,
    pub diagram: ProbeDiagram,
    pub description: &'static str,
    pub applies_wcs: bool,
    pub calibration: bool,
}

impl ProbeOperation {
    /// Which `#1001`–`#1009` layout the matching Probe Basic subroutine writes.
    /// Derived from the subroutine name so the table stays the single source
    /// of routine metadata.
    pub fn result_family(&self) -> ResultFamily {
        let call = self.call;
        if call.contains("z_minus") || call.contains("spindle_nose") {
            ResultFamily::SurfaceZ
        } else if call.contains("_x_plus")
            || call.contains("_x_minus")
            || call.contains("_y_plus")
            || call.contains("_y_minus")
        {
            ResultFamily::Edge
        } else {
            ResultFamily::BossPocket
        }
    }

    /// Every result slot written by this routine, including values written by
    /// nested Probe Basic helpers. Live snapshots replace this static set with
    /// the exact update messages observed during the run.
    pub fn result_parameters(&self) -> Vec<u16> {
        let range = |first, last| (first..=last).collect::<Vec<u16>>();
        match self.id {
            "front_left_top_corner" => vec![1006, 1008, 1009],
            "front_right_top_corner" => vec![1005, 1008, 1009],
            "back_left_top_corner" => vec![1006, 1007, 1009],
            "back_right_top_corner" => vec![1005, 1007, 1009],
            "front_left_inside_corner" => vec![1005, 1007, 1009],
            "front_right_inside_corner" => vec![1006, 1007, 1009],
            "back_left_inside_corner" => vec![1005, 1008, 1009],
            "back_right_inside_corner" => vec![1006, 1008, 1009],
            "front_top_side" => vec![1008, 1009],
            "right_top_side" => vec![1005, 1009],
            "back_top_side" => vec![1007, 1009],
            "left_top_side" => vec![1006, 1009],
            "edge_x_minus" => vec![1005],
            "edge_x_plus" => vec![1006],
            "edge_y_minus" => vec![1007],
            "edge_y_plus" => vec![1008],
            "surface_z" | "spindle_nose" => vec![1009],
            "round_boss" | "round_pocket" | "cal_round_boss" | "cal_round_pocket" => {
                range(1001, 1010)
            }
            "round_pocket_center" => {
                let mut slots = range(1001, 1008);
                slots.push(1010);
                slots
            }
            "rect_boss" | "rect_pocket" | "cal_square_boss" | "cal_square_pocket" => {
                range(1001, 1009)
            }
            "rect_pocket_center" => range(1001, 1008),
            "ridge_x" | "valley_x" | "valley_x_center" => vec![1001, 1002, 1005, 1006, 1009],
            "ridge_y" | "valley_y" | "valley_y_center" => vec![1003, 1004, 1007, 1008, 1009],
            "corner_x_plus_edge_angle" => vec![1006, 1007, 1009, 1011, 1012],
            "corner_x_minus_edge_angle" => vec![1005, 1008, 1009, 1011, 1012],
            "corner_y_plus_edge_angle" => vec![1006, 1008, 1009, 1011, 1012],
            "corner_y_minus_edge_angle" => vec![1005, 1007, 1009, 1011, 1012],
            "top_front_edge_angle" => vec![1008, 1009, 1011, 1012],
            "top_back_edge_angle" => vec![1007, 1009, 1011, 1012],
            "top_left_edge_angle" => vec![1006, 1009, 1011, 1012],
            "top_right_edge_angle" => vec![1005, 1009, 1011, 1012],
            "cal_reset" => Vec::new(),
            _ => Vec::new(),
        }
        .into_iter()
        .filter(|parameter| {
            !(self.id.ends_with("_center") && *parameter == 1009)
                && !(self.id.starts_with("valley_")
                    && self.id.ends_with("_center")
                    && *parameter == 1009)
        })
        .collect()
    }

    /// Axes and exact result sources used by an explicit work-offset Apply.
    /// An empty list means that generic Apply is forbidden for the operation.
    pub fn apply_terms(&self) -> Vec<ApplyTerm> {
        use ApplySource::{CornerX, CornerY, Slot, SlotMinusDelta, SlotPlusDelta, StartX, StartY};
        let term = |axis, source| ApplyTerm { axis, source };
        let xyz = |x, y, z: Option<u16>| {
            let mut terms = vec![term("X", x), term("Y", y)];
            if let Some(z) = z {
                terms.push(term("Z", Slot(z)));
            }
            terms
        };
        match self.id {
            "front_left_top_corner" => xyz(Slot(1006), Slot(1008), Some(1009)),
            "front_right_top_corner" => xyz(Slot(1005), Slot(1008), Some(1009)),
            "back_left_top_corner" => xyz(Slot(1006), Slot(1007), Some(1009)),
            "back_right_top_corner" => xyz(Slot(1005), Slot(1007), Some(1009)),
            "front_left_inside_corner" => xyz(Slot(1005), Slot(1007), Some(1009)),
            "front_right_inside_corner" => xyz(Slot(1006), Slot(1007), Some(1009)),
            "back_left_inside_corner" => xyz(Slot(1005), Slot(1008), Some(1009)),
            "back_right_inside_corner" => xyz(Slot(1006), Slot(1008), Some(1009)),
            "front_top_side" => vec![term("Y", Slot(1008)), term("Z", Slot(1009))],
            "right_top_side" => vec![term("X", Slot(1005)), term("Z", Slot(1009))],
            "back_top_side" => vec![term("Y", Slot(1007)), term("Z", Slot(1009))],
            "left_top_side" => vec![term("X", Slot(1006)), term("Z", Slot(1009))],
            "edge_x_minus" => vec![term("X", Slot(1005))],
            "edge_x_plus" => vec![term("X", Slot(1006))],
            "edge_y_minus" => vec![term("Y", Slot(1007))],
            "edge_y_plus" => vec![term("Y", Slot(1008))],
            "surface_z" => vec![term("Z", Slot(1009))],
            _ if self.category == ProbeCategory::BossAndPocket => {
                let z = self.result_parameters().contains(&1009).then_some(1009);
                xyz(Slot(1002), Slot(1004), z)
            }
            "ridge_x" | "valley_x" => vec![term("X", Slot(1002)), term("Z", Slot(1009))],
            "ridge_y" | "valley_y" => vec![term("Y", Slot(1004)), term("Z", Slot(1009))],
            "valley_x_center" => vec![term("X", Slot(1002))],
            "valley_y_center" => vec![term("Y", Slot(1004))],
            "corner_x_plus_edge_angle"
            | "corner_x_minus_edge_angle"
            | "corner_y_plus_edge_angle"
            | "corner_y_minus_edge_angle" => xyz(CornerX, CornerY, Some(1009)),
            "top_front_edge_angle" => xyz(StartX, SlotMinusDelta(1008), Some(1009)),
            "top_back_edge_angle" => xyz(StartX, SlotPlusDelta(1007), Some(1009)),
            "top_left_edge_angle" => xyz(SlotMinusDelta(1006), StartY, Some(1009)),
            "top_right_edge_angle" => xyz(SlotPlusDelta(1005), StartY, Some(1009)),
            // Calibration is already stored in #3032. Spindle nose and reset
            // have no generic Apply action.
            _ => Vec::new(),
        }
    }

    pub fn apply_block(
        &self,
        index: u8,
        results: &ParameterResults,
        rotation: Option<f64>,
    ) -> Result<String, String> {
        if !(1..=9).contains(&index) {
            return Err("invalid work-offset index".into());
        }
        if results.values.len() != 12 || results.values.iter().any(|value| !value.is_finite()) {
            return Err("probe result requires twelve finite values".into());
        }
        if rotation.is_some_and(|value| !value.is_finite()) {
            return Err("probe rotation must be finite".into());
        }
        let terms = self.apply_terms();
        if terms.is_empty() {
            return Err(format!("{} has no work-offset Apply action", self.name));
        }
        let mut block = format!("G10 L2 P{index}");
        for term in terms {
            block.push_str(&format!(
                " {}{}",
                term.axis,
                term.source.raw_expression(term.axis)
            ));
        }
        if let Some(rotation) = rotation {
            block.push_str(&format!(" R{}", format_number(rotation)));
        }
        Ok(block)
    }
}

/// Effective axes a routine's DIRECT run (`probe_mode EQ 0`) writes into the
/// active work offset row.
///
/// Two kinds of write make up the set, and both matter:
///
/// 1. The routine's own inline `G10 L2` expressions, and
/// 2. the inline DIRECT writes of helpers it calls with the mode argument
///    `#3030`.
///
/// This is deliberately not `apply_terms()`: an explicit Apply writes every
/// term, while a DIRECT run writes only what the call graph actually executes.
///
/// ## Inline writes
///
/// - the 8 `*_corner.ngc` `o130 if [#<probe_mode> EQ 0]` branches:
///   probe_front_left_top_corner.ngc:104-105, probe_front_right_top_corner.ngc:105-106,
///   probe_back_left_top_corner.ngc:103-104, probe_back_right_top_corner.ngc:102-103,
///   probe_front_left_inside_corner.ngc:108-109, probe_front_right_inside_corner.ngc:108-109,
///   probe_back_left_inside_corner.ngc:107-108, probe_back_right_inside_corner.ngc:107-108
///   write `X[...]` and `Y[...]`;
/// - probe_round_boss.ngc:283, probe_round_pocket.ngc:173-174,
///   probe_round_pocket_center_start.ngc:156-157, probe_rect_boss.ngc:175,
///   probe_rect_pocket.ngc:129, probe_rect_pocket_center_start.ngc:111 write X and Y;
/// - the 8 Edge Angle routines carry both rotation branches
///   (probe_top_*_edge_angle.ngc:113/:119, probe_corner_*_edge_angle.ngc:155/:161)
///   and write X and Y, adding `R[#<edge_angle>]` in the rotation branch;
/// - probe_front_top_side.ngc:80 and probe_back_top_side.ngc:80 write Y;
///   probe_right_top_side.ngc:79, probe_left_top_side.ngc:80,
///   probe_x_minus_wco.ngc:95, probe_x_plus_wco.ngc:94, probe_ridge_x.ngc:119,
///   probe_valley_x.ngc:99, probe_valley_x_center_start.ngc:81 write X;
///   probe_y_minus_wco.ngc:94, probe_y_plus_wco.ngc:94, probe_ridge_y.ngc:118,
///   probe_valley_y.ngc:97, probe_valley_y_center_start.ngc:81 write Y;
///   probe_z_minus_wco.ngc:79 writes Z;
/// - the calibration routines record the probed centre in X and Y
///   (probe_cal_round_boss.ngc:319, probe_cal_round_pocket.ngc:212,
///   probe_cal_square_boss.ngc:213, probe_cal_square_pocket.ngc:168).
///
/// ## Helper-mediated writes
///
/// `probe_z_minus_sub.ngc:73-77` is the only helper that writes the row: under
/// `o130 if [#<probe_mode> EQ 0]` it executes
/// `G10 L2 P#5220 Z[#<z_minus_probed> + #<workspace_z>]`. It receives the mode
/// as its seventh argument (`#<probe_mode> = #7`), so every caller that passes
/// `#3030` inherits a DIRECT Z write. There are 32 such call sites: the 8
/// corners, the 4 top sides, round/rect boss and pocket (not the centre-start
/// variants), ridge/valley X and Y (not their centre-start variants), the 8
/// Edge Angle routines, and the 4 calibration routines. The X/Y probe helpers
/// (probe_x_plus.ngc, probe_x_minus.ngc, probe_y_plus.ngc, probe_y_minus.ngc)
/// contain no `G10` at all, so Z is the only helper-mediated axis.
///
/// `direct_written_axes_match_the_subroutine_call_graph` re-derives these sets
/// from the NGC files, following `o<...> call` lines that pass `#3030` into the
/// helper, so an NGC edit fails that test instead of silently invalidating this
/// table.
pub fn direct_written_axes(operation: &ProbeOperation) -> &'static [&'static str] {
    match operation.id {
        // Inline X + Y, plus the helper's DIRECT Z.
        "front_left_top_corner"
        | "front_right_top_corner"
        | "back_left_top_corner"
        | "back_right_top_corner"
        | "front_left_inside_corner"
        | "front_right_inside_corner"
        | "back_left_inside_corner"
        | "back_right_inside_corner"
        | "round_boss"
        | "round_pocket"
        | "rect_boss"
        | "rect_pocket"
        | "corner_x_plus_edge_angle"
        | "corner_x_minus_edge_angle"
        | "corner_y_plus_edge_angle"
        | "corner_y_minus_edge_angle"
        | "top_front_edge_angle"
        | "top_back_edge_angle"
        | "top_left_edge_angle"
        | "top_right_edge_angle"
        | "cal_round_boss"
        | "cal_round_pocket"
        | "cal_square_boss"
        | "cal_square_pocket" => &["X", "Y", "Z"],
        // Inline Y + the helper's DIRECT Z (probe_front_top_side.ngc:80,
        // probe_back_top_side.ngc:80, probe_ridge_y.ngc:118,
        // probe_valley_y.ngc:97).
        "front_top_side" | "back_top_side" | "ridge_y" | "valley_y" => &["Y", "Z"],
        // Inline X + the helper's DIRECT Z (probe_right_top_side.ngc:79,
        // probe_left_top_side.ngc:80, probe_ridge_x.ngc:119,
        // probe_valley_x.ngc:99).
        "right_top_side" | "left_top_side" | "ridge_x" | "valley_x" => &["X", "Z"],
        // Centre-start variants call no seven-argument Z helper.
        "round_pocket_center" | "rect_pocket_center" => &["X", "Y"],
        // Single-edge routines and the valley centre-start variants.
        "edge_x_minus" | "edge_x_plus" | "valley_x_center" => &["X"],
        "edge_y_minus" | "edge_y_plus" | "valley_y_center" => &["Y"],
        // probe_z_minus_wco.ngc:79.
        "surface_z" => &["Z"],
        // probe_spindle_nose.ngc and probe_cal_reset.ngc contain no `G10` and
        // call no Z helper, so a DIRECT run there has nothing to write.
        _ => &[],
    }
}

macro_rules! op {
    ($id:literal, $name:literal, $call:literal, $cat:ident, $diagram:ident, $desc:literal, $applies:literal, $cal:literal) => {
        ProbeOperation {
            id: $id,
            name: $name,
            call: $call,
            category: ProbeCategory::$cat,
            diagram: ProbeDiagram::$diagram,
            description: $desc,
            applies_wcs: $applies,
            calibration: $cal,
        }
    };
}

pub fn operations() -> &'static [ProbeOperation] {
    &[
        // Outside corners (8)
        op!(
            "front_left_top_corner",
            "FRONT LEFT TOP",
            "probe_front_left_top_corner",
            OutsideCorners,
            Corner,
            "Probe the front-left top corner for X, Y and Z zero.",
            true,
            false
        ),
        op!(
            "front_right_top_corner",
            "FRONT RIGHT TOP",
            "probe_front_right_top_corner",
            OutsideCorners,
            Corner,
            "Probe the front-right top corner.",
            true,
            false
        ),
        op!(
            "back_left_top_corner",
            "BACK LEFT TOP",
            "probe_back_left_top_corner",
            OutsideCorners,
            Corner,
            "Probe the back-left top corner.",
            true,
            false
        ),
        op!(
            "back_right_top_corner",
            "BACK RIGHT TOP",
            "probe_back_right_top_corner",
            OutsideCorners,
            Corner,
            "Probe the back-right top corner.",
            true,
            false
        ),
        op!(
            "front_top_side",
            "FRONT TOP SIDE",
            "probe_front_top_side",
            OutsideCorners,
            EdgeAngle,
            "Probe the front top edge.",
            true,
            false
        ),
        op!(
            "right_top_side",
            "RIGHT TOP SIDE",
            "probe_right_top_side",
            OutsideCorners,
            EdgeAngle,
            "Probe the right top edge.",
            true,
            false
        ),
        op!(
            "back_top_side",
            "BACK TOP SIDE",
            "probe_back_top_side",
            OutsideCorners,
            EdgeAngle,
            "Probe the back top edge.",
            true,
            false
        ),
        op!(
            "left_top_side",
            "LEFT TOP SIDE",
            "probe_left_top_side",
            OutsideCorners,
            EdgeAngle,
            "Probe the left top edge.",
            true,
            false
        ),
        // Inside corners (4)
        op!(
            "front_left_inside_corner",
            "FRONT LEFT INSIDE",
            "probe_front_left_inside_corner",
            InsideCorners,
            InsideCorner,
            "Probe a front-left inside corner.",
            true,
            false
        ),
        op!(
            "front_right_inside_corner",
            "FRONT RIGHT INSIDE",
            "probe_front_right_inside_corner",
            InsideCorners,
            InsideCorner,
            "Probe a front-right inside corner.",
            true,
            false
        ),
        op!(
            "back_left_inside_corner",
            "BACK LEFT INSIDE",
            "probe_back_left_inside_corner",
            InsideCorners,
            InsideCorner,
            "Probe a back-left inside corner.",
            true,
            false
        ),
        op!(
            "back_right_inside_corner",
            "BACK RIGHT INSIDE",
            "probe_back_right_inside_corner",
            InsideCorners,
            InsideCorner,
            "Probe a back-right inside corner.",
            true,
            false
        ),
        // Boss and pocket (6)
        op!(
            "round_boss",
            "ROUND BOSS",
            "probe_round_boss",
            BossAndPocket,
            RoundBoss,
            "Measure a round boss centre.",
            true,
            false
        ),
        op!(
            "round_pocket",
            "ROUND POCKET",
            "probe_round_pocket",
            BossAndPocket,
            RoundPocket,
            "Measure a round pocket centre.",
            true,
            false
        ),
        op!(
            "round_pocket_center",
            "ROUND POCKET CENTRE START",
            "probe_round_pocket_center_start",
            BossAndPocket,
            RoundPocket,
            "Measure a round pocket starting at the centre.",
            true,
            false
        ),
        op!(
            "rect_boss",
            "RECT BOSS",
            "probe_rect_boss",
            BossAndPocket,
            RectBoss,
            "Measure a rectangular boss.",
            true,
            false
        ),
        op!(
            "rect_pocket",
            "RECT POCKET",
            "probe_rect_pocket",
            BossAndPocket,
            RectPocket,
            "Measure a rectangular pocket.",
            true,
            false
        ),
        op!(
            "rect_pocket_center",
            "RECT POCKET CENTRE START",
            "probe_rect_pocket_center_start",
            BossAndPocket,
            RectPocket,
            "Measure a rectangular pocket starting at the centre.",
            true,
            false
        ),
        // Ridge and valley (6)
        op!(
            "ridge_x",
            "RIDGE X",
            "probe_ridge_x",
            RidgeAndValley,
            Ridge,
            "Find the centre of an X ridge.",
            true,
            false
        ),
        op!(
            "ridge_y",
            "RIDGE Y",
            "probe_ridge_y",
            RidgeAndValley,
            Ridge,
            "Find the centre of a Y ridge.",
            true,
            false
        ),
        op!(
            "valley_x",
            "VALLEY X",
            "probe_valley_x",
            RidgeAndValley,
            Valley,
            "Find the centre of an X valley.",
            true,
            false
        ),
        op!(
            "valley_y",
            "VALLEY Y",
            "probe_valley_y",
            RidgeAndValley,
            Valley,
            "Find the centre of a Y valley.",
            true,
            false
        ),
        op!(
            "valley_x_center",
            "VALLEY X CENTRE START",
            "probe_valley_x_center_start",
            RidgeAndValley,
            Valley,
            "Find an X valley from the centre.",
            true,
            false
        ),
        op!(
            "valley_y_center",
            "VALLEY Y CENTRE START",
            "probe_valley_y_center_start",
            RidgeAndValley,
            Valley,
            "Find a Y valley from the centre.",
            true,
            false
        ),
        // Edge angle (8)
        op!(
            "corner_x_plus_edge_angle",
            "CORNER X+ EDGE ANGLE",
            "probe_corner_x_plus_edge_angle",
            EdgeAngle,
            EdgeAngle,
            "Measure an X+ edge angle.",
            true,
            false
        ),
        op!(
            "corner_x_minus_edge_angle",
            "CORNER X- EDGE ANGLE",
            "probe_corner_x_minus_edge_angle",
            EdgeAngle,
            EdgeAngle,
            "Measure an X- edge angle.",
            true,
            false
        ),
        op!(
            "corner_y_plus_edge_angle",
            "CORNER Y+ EDGE ANGLE",
            "probe_corner_y_plus_edge_angle",
            EdgeAngle,
            EdgeAngle,
            "Measure a Y+ edge angle.",
            true,
            false
        ),
        op!(
            "corner_y_minus_edge_angle",
            "CORNER Y- EDGE ANGLE",
            "probe_corner_y_minus_edge_angle",
            EdgeAngle,
            EdgeAngle,
            "Measure a Y- edge angle.",
            true,
            false
        ),
        op!(
            "top_front_edge_angle",
            "TOP FRONT EDGE ANGLE",
            "probe_top_front_edge_angle",
            EdgeAngle,
            EdgeAngle,
            "Measure a top-front edge angle.",
            true,
            false
        ),
        op!(
            "top_back_edge_angle",
            "TOP BACK EDGE ANGLE",
            "probe_top_back_edge_angle",
            EdgeAngle,
            EdgeAngle,
            "Measure a top-back edge angle.",
            true,
            false
        ),
        op!(
            "top_left_edge_angle",
            "TOP LEFT EDGE ANGLE",
            "probe_top_left_edge_angle",
            EdgeAngle,
            EdgeAngle,
            "Measure a top-left edge angle.",
            true,
            false
        ),
        op!(
            "top_right_edge_angle",
            "TOP RIGHT EDGE ANGLE",
            "probe_top_right_edge_angle",
            EdgeAngle,
            EdgeAngle,
            "Measure a top-right edge angle.",
            true,
            false
        ),
        // Edge operations. SPEC §6.8: the plain X±/Y± edge probes stay
        // available in the category that matches what they measure.
        op!(
            "edge_x_minus",
            "EDGE X-",
            "probe_x_minus_wco",
            OutsideCorners,
            EdgeAngle,
            "Probe the X- face and set the selected WCS X.",
            true,
            false
        ),
        op!(
            "edge_x_plus",
            "EDGE X+",
            "probe_x_plus_wco",
            OutsideCorners,
            EdgeAngle,
            "Probe the X+ face and set the selected WCS X.",
            true,
            false
        ),
        op!(
            "edge_y_minus",
            "EDGE Y-",
            "probe_y_minus_wco",
            OutsideCorners,
            EdgeAngle,
            "Probe the Y- face and set the selected WCS Y.",
            true,
            false
        ),
        op!(
            "edge_y_plus",
            "EDGE Y+",
            "probe_y_plus_wco",
            OutsideCorners,
            EdgeAngle,
            "Probe the Y+ face and set the selected WCS Y.",
            true,
            false
        ),
        // Surface / Z
        op!(
            "surface_z",
            "SURFACE Z",
            "probe_z_minus_wco",
            SurfaceZ,
            Surface,
            "Measure a Z surface and set the selected WCS Z.",
            true,
            false
        ),
        op!(
            "spindle_nose",
            "SPINDLE NOSE",
            "probe_spindle_nose",
            SurfaceZ,
            Surface,
            "Measure the spindle nose reference.",
            false,
            true
        ),
        // Calibration
        op!(
            "cal_round_boss",
            "CALIBRATE ROUND BOSS",
            "probe_cal_round_boss",
            Calibrate,
            Calibration,
            "Calibrate the probe on a round boss.",
            false,
            true
        ),
        op!(
            "cal_round_pocket",
            "CALIBRATE ROUND POCKET",
            "probe_cal_round_pocket",
            Calibrate,
            Calibration,
            "Calibrate the probe in a round pocket.",
            false,
            true
        ),
        op!(
            "cal_square_boss",
            "CALIBRATE SQUARE BOSS",
            "probe_cal_square_boss",
            Calibrate,
            Calibration,
            "Calibrate the probe on a square boss.",
            false,
            true
        ),
        op!(
            "cal_square_pocket",
            "CALIBRATE SQUARE POCKET",
            "probe_cal_square_pocket",
            Calibrate,
            Calibration,
            "Calibrate the probe in a square pocket.",
            false,
            true
        ),
        op!(
            "cal_reset",
            "RESET CALIBRATION",
            "probe_cal_reset",
            Calibrate,
            Calibration,
            "Clear the stored probe calibration offset.",
            false,
            true
        ),
    ]
}

pub fn operation(id: &str) -> Option<&'static ProbeOperation> {
    operations().iter().find(|operation| operation.id == id)
}

pub fn operations_in(category: ProbeCategory) -> Vec<&'static ProbeOperation> {
    operations()
        .iter()
        .filter(|operation| operation.category == category)
        .collect()
}

/// Does this line call `o<call>` with the exact `o<name> call` token?
///
/// Names share prefixes (`probe_round_pocket` prefixes
/// `probe_round_pocket_center_start`), so a contained name is not a match.
fn calls_operation(line: &str, call: &str) -> bool {
    let lowered = line.trim().to_ascii_lowercase();
    let name = call.to_ascii_lowercase();
    let Some(rest) = lowered.strip_prefix('o') else {
        return false;
    };
    let Some(rest) = rest.trim_start().strip_prefix('<') else {
        return false;
    };
    let Some(rest) = rest.strip_prefix(&name) else {
        return false;
    };
    let Some(rest) = rest.strip_prefix('>') else {
        return false;
    };
    let Some(rest) = rest.trim_start().strip_prefix("call") else {
        return false;
    };
    !rest.starts_with(|ch: char| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Resolve the routine a probe MDI block calls, by its exact
/// `o<name> call` token.
pub fn operation_for_mdi(block: &str) -> Option<&'static ProbeOperation> {
    block.lines().find_map(|line| {
        operations()
            .iter()
            .find(|operation| calls_operation(line, operation.call))
    })
}

/// An MDI probe action that writes controller state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeWriteAction {
    /// A DIRECT-WCO run (`probe_mode` 0) writes the active work offset row.
    DirectWco,
    /// An Edge Angle run clears rotation with `R0` in every mode, and adds the
    /// requested rotation in DIRECT mode.
    EdgeAngleRotation,
    /// A calibration run: the catalogue's `calibration` routines rewrite
    /// `#3032` and the spindle reference parameters (`#3007`, `#3010`), in
    /// POSITION mode too.
    CalibrationRun,
    /// `o<probe_cal_reset> call` writes `#3032`.
    CalibrationReset,
    /// The explicit parameter update (`U`) rewrites `#3014`-`#3036`.
    ParameterUpdate,
    /// `reset_all_data`, `x_data_reset` or `y_data_reset` clear the result
    /// slots.
    ResultReset,
    /// A routine call with no typed parameter block: the mode it inherits from
    /// the interpreter is unknown, so it fails closed.
    UnknownRun,
}

/// Classify an MDI block by the action it performs, never by the probe mode
/// alone. `None` means the block writes no controller state that this
/// contract knows about.
///
/// Rules, first match wins:
/// 1. `CalibrationReset`: a `probe_cal_reset` call.
/// 2. `ResultReset`: a `reset_all_data`, `x_data_reset` or `y_data_reset`
///    call.
/// 3. A block that calls a catalogue routine is a Run:
///    a. `CalibrationRun` when the catalogue marks the routine `calibration`
///       - `spindle_nose` is one of them (`probe_spindle_nose.ngc:65-68`
///       writes `#3010` and `#3007`), so it is a write even though its
///       category is `SurfaceZ` and its block is POSITION;
///    b. `DirectWco` when the typed parameter block selects `probe_mode` 0;
///    c. `EdgeAngleRotation` for `ProbeCategory::EdgeAngle` in the other
///       mode, because the routine still clears rotation with `R0`;
///    d. `UnknownRun` when the block carries no typed parameter block, so the
///       inherited mode cannot be trusted;
///    e. otherwise no write: an ordinary measure-only POSITION run, whose
///       `touch_probe_param_update` preamble only assigns the routine's own
///       `#3014`-`#3036` inputs.
/// 4. `ParameterUpdate`: a `touch_probe_param_update` call with no routine
///    call - the explicit `U` action.
/// 5. Otherwise `None`.
///
/// Both the native backend and the simulator gate on this, and the UI is meant
/// to use the same answer, so the three cannot drift.
pub fn probe_write_action(block: &str) -> Option<ProbeWriteAction> {
    let calls = |name: &str| block.lines().any(|line| calls_operation(line, name));
    if calls("probe_cal_reset") {
        return Some(ProbeWriteAction::CalibrationReset);
    }
    if ["reset_all_data", "x_data_reset", "y_data_reset"]
        .iter()
        .any(|name| calls(name))
    {
        return Some(ProbeWriteAction::ResultReset);
    }
    if let Some(operation) = operation_for_mdi(block) {
        if operation.calibration {
            return Some(ProbeWriteAction::CalibrationRun);
        }
        let Some(parameters) = ProbeParameters::from_mdi(block) else {
            return Some(ProbeWriteAction::UnknownRun);
        };
        if parameters.probe_mode == ProbeMode::ApplyWcs {
            return Some(ProbeWriteAction::DirectWco);
        }
        if operation.category == ProbeCategory::EdgeAngle {
            return Some(ProbeWriteAction::EdgeAngleRotation);
        }
        return None;
    }
    calls("touch_probe_param_update").then_some(ProbeWriteAction::ParameterUpdate)
}

/// The tool number the Probe Basic parameter contract expects in `#3014`.
///
/// A different value is not fatal, but it silently changes which tool the
/// probe cycles assume is mounted, so it is worth surfacing at startup.
pub const EXPECTED_PROBE_TOOL: u16 = 99;

/// Probe Basic parameter contract `#3014`–`#3036`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ProbeParameters {
    /// Machine/profile-owned; deliberately not persisted in UI preferences.
    #[serde(skip, default = "default_probe_tool_number")]
    pub probe_tool_number: u16,
    pub slow_feed: f64,
    pub fast_feed: f64,
    pub traverse_feed: f64,
    pub max_xy_distance: f64,
    pub xy_clearance: f64,
    pub max_z_distance: f64,
    pub z_clearance: f64,
    pub extra_depth: f64,
    pub step_off_width: f64,
    pub edge_width: f64,
    pub diameter_hint: f64,
    pub x_hint_boss_pocket: f64,
    pub y_hint_boss_pocket: f64,
    pub x_hint_ridge_valley: f64,
    pub y_hint_ridge_valley: f64,
    pub probe_mode: ProbeMode,
    pub wcs_rotation: bool,
    /// Controller-owned calibration result; synchronized after execution.
    #[serde(skip)]
    pub calibration_offset: f64,
    pub calibration_diameter: f64,
    pub x_cal_width: f64,
    pub y_cal_width: f64,
    pub square_cal_axis: u8,
}

fn default_probe_tool_number() -> u16 {
    EXPECTED_PROBE_TOOL
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProbeMode {
    ApplyWcs,
    MeasureOnly,
}

impl ProbeMode {
    pub fn value(self) -> i32 {
        match self {
            Self::ApplyWcs => 0,
            Self::MeasureOnly => 1,
        }
    }
}

impl Default for ProbeParameters {
    fn default() -> Self {
        Self {
            probe_tool_number: 99,
            slow_feed: 50.0,
            fast_feed: 500.0,
            traverse_feed: 1000.0,
            max_xy_distance: 10.0,
            xy_clearance: 5.0,
            max_z_distance: 10.0,
            z_clearance: 5.0,
            extra_depth: 0.0,
            step_off_width: 1.0,
            edge_width: 1.0,
            diameter_hint: 50.0,
            x_hint_boss_pocket: 0.0,
            y_hint_boss_pocket: 0.0,
            x_hint_ridge_valley: 0.0,
            y_hint_ridge_valley: 0.0,
            probe_mode: ProbeMode::MeasureOnly,
            wcs_rotation: false,
            calibration_offset: 0.0,
            calibration_diameter: 4.0,
            x_cal_width: 0.0,
            y_cal_width: 0.0,
            square_cal_axis: 0,
        }
    }
}

impl ProbeParameters {
    pub fn from_config(config: &crate::config::ProbeConfig) -> Self {
        Self {
            probe_tool_number: config.tool_number,
            slow_feed: config.slow_feed,
            fast_feed: config.fast_feed,
            traverse_feed: config.fast_feed * 2.0,
            max_xy_distance: config.max_travel,
            xy_clearance: config.clearance,
            max_z_distance: config.probe_depth,
            z_clearance: config.clearance,
            calibration_diameter: config.calibration_diameter,
            ..Default::default()
        }
    }

    /// Recover the typed parameter block emitted by [`Self::parameter_update_block`].
    /// Backends use this to enforce mode/protect rules and to simulate the same
    /// call without parsing G-code expressions or accepting arbitrary commands.
    pub fn from_mdi(block: &str) -> Option<Self> {
        let line = block.lines().find(|line| {
            line.trim_start()
                .to_ascii_lowercase()
                .starts_with("o<touch_probe_param_update>")
        })?;
        let lower = line.to_ascii_lowercase();
        let call = lower.find("call")? + 4;
        let mut rest = line[call..].trim();
        let mut values = Vec::with_capacity(23);
        while !rest.is_empty() {
            rest = rest.strip_prefix('[')?;
            let end = rest.find(']')?;
            values.push(rest[..end].trim().parse::<f64>().ok()?);
            rest = rest[end + 1..].trim_start();
        }
        if values.len() != 23 || values.iter().any(|value| !value.is_finite()) {
            return None;
        }
        let tool = values[0];
        let mode = values[16];
        let rotation = values[17];
        let axis = values[22];
        if tool < 1.0 || tool > u16::MAX as f64 || tool.fract() != 0.0 {
            return None;
        }
        if mode != 0.0 && mode != 1.0 {
            return None;
        }
        if rotation != 0.0 && rotation != 1.0 {
            return None;
        }
        if !(0.0..=2.0).contains(&axis) || axis.fract() != 0.0 {
            return None;
        }
        Some(Self {
            probe_tool_number: tool as u16,
            slow_feed: values[1],
            fast_feed: values[2],
            traverse_feed: values[3],
            max_xy_distance: values[4],
            xy_clearance: values[5],
            max_z_distance: values[6],
            z_clearance: values[7],
            extra_depth: values[8],
            step_off_width: values[9],
            edge_width: values[10],
            diameter_hint: values[11],
            x_hint_boss_pocket: values[12],
            y_hint_boss_pocket: values[13],
            x_hint_ridge_valley: values[14],
            y_hint_ridge_valley: values[15],
            probe_mode: if mode == 0.0 {
                ProbeMode::ApplyWcs
            } else {
                ProbeMode::MeasureOnly
            },
            wcs_rotation: rotation == 1.0,
            calibration_offset: values[18],
            calibration_diameter: values[19],
            x_cal_width: values[20],
            y_cal_width: values[21],
            square_cal_axis: axis as u8,
        })
    }

    /// Values in `#3014`…`#3036` order.
    pub fn values(&self) -> Vec<f64> {
        vec![
            self.probe_tool_number as f64,
            self.slow_feed,
            self.fast_feed,
            self.traverse_feed,
            self.max_xy_distance,
            self.xy_clearance,
            self.max_z_distance,
            self.z_clearance,
            self.extra_depth,
            self.step_off_width,
            self.edge_width,
            self.diameter_hint,
            self.x_hint_boss_pocket,
            self.y_hint_boss_pocket,
            self.x_hint_ridge_valley,
            self.y_hint_ridge_valley,
            self.probe_mode.value() as f64,
            if self.wcs_rotation { 1.0 } else { 0.0 },
            self.calibration_offset,
            self.calibration_diameter,
            self.x_cal_width,
            self.y_cal_width,
            self.square_cal_axis as f64,
        ]
    }

    pub fn validate(&self, enabled: bool, current_tool: u16) -> Result<(), String> {
        if !enabled {
            return Err("touch probing is disabled for this profile".into());
        }
        if self.probe_tool_number == 0 {
            return Err("probe tool number must be non-zero".into());
        }
        if !self.slow_feed.is_finite() || self.slow_feed < 0.0 {
            return Err("slow feed must be zero (single touch) or positive".into());
        }
        for (name, value) in [
            ("fast feed", self.fast_feed),
            ("traverse feed", self.traverse_feed),
            ("max XY distance", self.max_xy_distance),
            ("XY clearance", self.xy_clearance),
            ("max Z distance", self.max_z_distance),
            ("Z clearance", self.z_clearance),
            ("step off width", self.step_off_width),
            ("edge width", self.edge_width),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(format!("{name} must be a positive number"));
            }
        }
        for (name, value) in [
            ("extra probe depth", self.extra_depth),
            ("calibration offset", self.calibration_offset),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(format!("{name} must be zero or positive"));
            }
        }
        if self.square_cal_axis > 2 {
            return Err("calibration axis must be AVG XY, X or Y".into());
        }
        if self.slow_feed > self.fast_feed {
            return Err("slow feed must not exceed fast feed".into());
        }
        if self.fast_feed > self.traverse_feed {
            return Err("fast feed must not exceed traverse feed".into());
        }
        if current_tool != self.probe_tool_number {
            return Err(format!(
                "probe tool T{} is not in the spindle (current T{})",
                self.probe_tool_number, current_tool
            ));
        }
        Ok(())
    }

    /// Validate the common contract and the size inputs used by this routine.
    pub fn validate_for_operation(
        &self,
        enabled: bool,
        current_tool: u16,
        operation: &ProbeOperation,
    ) -> Result<(), String> {
        if operation.id == "cal_reset" {
            return if enabled {
                Ok(())
            } else {
                Err("touch probing is disabled for this profile".into())
            };
        }
        self.validate(enabled, current_tool)?;
        let positive = |name: &str, value: f64| -> Result<(), String> {
            if value.is_finite() && value > 0.0 {
                Ok(())
            } else {
                Err(format!("{name} must be a positive number"))
            }
        };
        match operation.category {
            ProbeCategory::BossAndPocket if operation.call.contains("round") => {
                positive("boss/pocket diameter hint", self.diameter_hint)?;
            }
            ProbeCategory::BossAndPocket => {
                positive("boss/pocket X width hint", self.x_hint_boss_pocket)?;
                positive("boss/pocket Y width hint", self.y_hint_boss_pocket)?;
            }
            ProbeCategory::RidgeAndValley
                if operation.call.ends_with("_y") || operation.call.contains("_y_") =>
            {
                positive("ridge/valley Y width hint", self.y_hint_ridge_valley)?;
            }
            ProbeCategory::RidgeAndValley => {
                positive("ridge/valley X width hint", self.x_hint_ridge_valley)?;
            }
            ProbeCategory::Calibrate if operation.call.contains("round") => {
                positive("calibration diameter", self.calibration_diameter)?;
            }
            ProbeCategory::Calibrate => {
                positive("calibration X width", self.x_cal_width)?;
                positive("calibration Y width", self.y_cal_width)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Build the MDI blocks used by `RUN PROBE`: update the Probe Basic
    /// parameters, capture the source WCS/start context for an idempotent
    /// explicit Apply, then invoke the selected routine.
    pub fn parameter_update_block(&self) -> String {
        let values = self
            .values()
            .iter()
            .map(|value| format_number(*value))
            .collect::<Vec<_>>();
        format!(
            "o<touch_probe_param_update> call {}",
            values
                .iter()
                .map(|value| format!("[{value}]"))
                .collect::<String>()
        )
    }

    pub fn blocks(&self, operation: &ProbeOperation) -> Vec<String> {
        vec![
            self.parameter_update_block(),
            "o<linc_probe_context> call".into(),
            format!("o<{}> call", operation.call),
        ]
    }

    pub fn mdi_text(&self, operation: &ProbeOperation) -> String {
        self.blocks(operation).join("\n")
    }
}

fn format_number(value: f64) -> String {
    if (value - value.round()).abs() < 1e-9 {
        format!("{}", value as i64)
    } else {
        let mut text = format!("{value:.6}");
        while text.ends_with('0') {
            text.pop();
        }
        if text.ends_with('.') {
            text.pop();
        }
        text
    }
}

/// Probe result contract `#1001`–`#1012`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParameterResults {
    pub values: [f64; 12],
}

/// Which `#1001`–`#1009` layout the Probe Basic subroutine writes. The slots
/// are NOT an X/Y/Z position: `#1001` is an X *width* and `#1002` an X
/// *centre* for the boss/pocket family. Reading them as axis positions shows
/// widths and centres where the operator expects coordinates.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResultFamily {
    /// Corner / boss / pocket / ridge routines: two widths and two centres.
    #[default]
    BossPocket,
    /// Single-edge routines: the four probed face positions.
    Edge,
    /// Z-surface routines: only `#1009`.
    SurfaceZ,
}

/// The meaning of one `#1001`–`#1009` slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultSlot {
    Unused,
    XWidth,
    XCenter,
    YWidth,
    YCenter,
    XMinus,
    XPlus,
    YMinus,
    YPlus,
    ZSurface,
}

impl ResultSlot {
    pub fn label(self) -> &'static str {
        match self {
            ResultSlot::Unused => "—",
            ResultSlot::XWidth => "X WIDTH",
            ResultSlot::XCenter => "X CENTRE",
            ResultSlot::YWidth => "Y WIDTH",
            ResultSlot::YCenter => "Y CENTRE",
            ResultSlot::XMinus => "X- FACE",
            ResultSlot::XPlus => "X+ FACE",
            ResultSlot::YMinus => "Y- FACE",
            ResultSlot::YPlus => "Y+ FACE",
            ResultSlot::ZSurface => "Z SURFACE",
        }
    }

    pub fn used(self) -> bool {
        self != ResultSlot::Unused
    }
}

impl ResultFamily {
    /// Slot meanings for `#1001` … `#1009`, in parameter order.
    pub fn slots(self) -> [ResultSlot; 9] {
        match self {
            ResultFamily::BossPocket => [
                ResultSlot::XWidth,
                ResultSlot::XCenter,
                ResultSlot::YWidth,
                ResultSlot::YCenter,
                ResultSlot::Unused,
                ResultSlot::Unused,
                ResultSlot::Unused,
                ResultSlot::Unused,
                ResultSlot::Unused,
            ],
            ResultFamily::Edge => [
                ResultSlot::Unused,
                ResultSlot::Unused,
                ResultSlot::Unused,
                ResultSlot::Unused,
                ResultSlot::XMinus,
                ResultSlot::XPlus,
                ResultSlot::YMinus,
                ResultSlot::YPlus,
                ResultSlot::Unused,
            ],
            ResultFamily::SurfaceZ => [
                ResultSlot::Unused,
                ResultSlot::Unused,
                ResultSlot::Unused,
                ResultSlot::Unused,
                ResultSlot::Unused,
                ResultSlot::Unused,
                ResultSlot::Unused,
                ResultSlot::Unused,
                ResultSlot::ZSurface,
            ],
        }
    }
}

/// One named measurement read back from the probe subroutines.
#[derive(Clone, Debug, PartialEq)]
pub struct ProbeMeasurement {
    pub parameter: u16,
    pub slot: ResultSlot,
    pub value: f64,
}

impl ParameterResults {
    pub fn from_values(values: &[f64]) -> Self {
        let mut out = [0.0; 12];
        for (slot, value) in out.iter_mut().zip(values.iter().copied()) {
            *slot = value;
        }
        Self { values: out }
    }

    /// Named measurements for the routine family, skipping unused slots.
    pub fn measurements(&self, family: ResultFamily) -> Vec<ProbeMeasurement> {
        family
            .slots()
            .iter()
            .enumerate()
            .filter(|(_, slot)| slot.used())
            .map(|(index, slot)| ProbeMeasurement {
                parameter: 1001 + index as u16,
                slot: *slot,
                value: self.values[index],
            })
            .collect()
    }

    /// The measured work-coordinate values this routine actually determines.
    /// Auxiliary values are the captured start/corner coordinates required by
    /// angle routines; ordinary routines need none.
    pub fn work_positions(
        &self,
        operation: &ProbeOperation,
        auxiliary: &std::collections::BTreeMap<String, f64>,
    ) -> Vec<(String, f64)> {
        operation
            .apply_terms()
            .into_iter()
            .filter_map(|term| {
                term.source
                    .value(self, auxiliary)
                    .map(|value| (term.axis.to_string(), value))
            })
            .collect()
    }

    /// Backwards-compatible ordinary-slot view. Angle-only auxiliary values
    /// are omitted unless the caller uses [`Self::work_positions`].
    pub fn wcs_offsets(&self, operation: &ProbeOperation) -> Vec<(String, f64)> {
        self.work_positions(operation, &std::collections::BTreeMap::new())
    }

    /// Convert measured work positions to raw G10 L2 offsets using the origin
    /// captured before the run. Untouched axes are absent and must be merged,
    /// never overwritten with zero.
    pub fn raw_offsets(
        &self,
        operation: &ProbeOperation,
        source_offsets: &crate::model::AxisMap<f64>,
        auxiliary: &std::collections::BTreeMap<String, f64>,
    ) -> Result<crate::model::AxisMap<f64>, String> {
        let mut raw = crate::model::AxisMap::new();
        for term in operation.apply_terms() {
            let measured = term.source.value(self, auxiliary).ok_or_else(|| {
                format!(
                    "missing captured {} value for {}",
                    term.axis, operation.name
                )
            })?;
            let source = source_offsets
                .get(term.axis)
                .copied()
                .ok_or_else(|| format!("missing source {} work offset", term.axis))?;
            let value = source + measured;
            if !value.is_finite() {
                return Err(format!("non-finite {} Apply value", term.axis));
            }
            raw.insert(term.axis.to_string(), value);
        }
        Ok(raw)
    }

    /// Probe Basic's radial calibration error stored in #3032. This is not a
    /// generic feature-size error and has no meaning for non-calibration runs.
    pub fn calibration_error(
        &self,
        operation: &ProbeOperation,
        parameters: &ProbeParameters,
    ) -> Option<f64> {
        if operation.category != ProbeCategory::Calibrate || operation.id == "cal_reset" {
            return None;
        }
        let round = operation.call.contains("round");
        let expected_x = if round {
            parameters.calibration_diameter
        } else {
            parameters.x_cal_width
        };
        let expected_y = if round {
            parameters.calibration_diameter
        } else {
            parameters.y_cal_width
        };
        if !expected_x.is_finite()
            || !expected_y.is_finite()
            || expected_x <= 0.0
            || expected_y <= 0.0
        {
            return None;
        }
        let ex = (expected_x - self.values[0]).abs() / 2.0;
        let ey = (expected_y - self.values[2]).abs() / 2.0;
        Some(match parameters.square_cal_axis {
            0 => (ex + ey) / 2.0,
            1 => ex,
            _ => ey,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_slots_are_not_axis_positions() {
        // #1001 is an X width and #1002 an X centre, not X and Y positions.
        let results =
            ParameterResults::from_values(&[50.0, 12.5, 30.0, -4.25, 0.0, 0.0, 0.0, 0.0, 0.0]);
        let measured = results.measurements(ResultFamily::BossPocket);
        assert_eq!(measured.len(), 4);
        assert_eq!(measured[0].parameter, 1001);
        assert_eq!(measured[0].slot, ResultSlot::XWidth);
        assert_eq!(measured[1].parameter, 1002);
        assert_eq!(measured[1].slot, ResultSlot::XCenter);
        assert_eq!(measured[3].slot, ResultSlot::YCenter);
        assert_eq!(measured[3].value, -4.25);
    }

    #[test]
    fn wcs_positions_and_raw_offsets_follow_the_exact_operation() {
        let values = ParameterResults::from_values(&[
            50.0, 12.5, 30.0, -4.25, -100.0, 100.0, -200.0, 200.0, -3.5, 40.0, 1.25, 2.5,
        ]);
        let corner = operation("front_left_top_corner").unwrap();
        assert_eq!(
            values.wcs_offsets(corner),
            vec![("X".into(), 100.0), ("Y".into(), 200.0), ("Z".into(), -3.5)]
        );
        let source = crate::model::AxisMap::from_entries([
            ("X".into(), 1000.0),
            ("Y".into(), 2000.0),
            ("Z".into(), 3000.0),
        ]);
        let raw = values
            .raw_offsets(corner, &source, &std::collections::BTreeMap::new())
            .unwrap();
        assert_eq!(raw.get("X"), Some(&1100.0));
        assert_eq!(raw.get("Y"), Some(&2200.0));
        assert_eq!(raw.get("Z"), Some(&2996.5));
        assert_eq!(raw.len(), 3);

        let z = operation("surface_z").unwrap();
        assert_eq!(values.wcs_offsets(z), vec![("Z".into(), -3.5)]);
        assert!(values
            .wcs_offsets(operation("spindle_nose").unwrap())
            .is_empty());
        assert!(values
            .wcs_offsets(operation("cal_round_boss").unwrap())
            .is_empty());
    }

    #[test]
    fn angle_apply_uses_first_edge_and_captured_corner_not_final_face() {
        let values = ParameterResults::from_values(&[
            0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 25.0, -3.0, 0.0, 5.0, 2.0,
        ]);
        let mut auxiliary = std::collections::BTreeMap::new();
        auxiliary.insert("start_x".into(), 7.0);
        assert_eq!(
            values.work_positions(operation("top_front_edge_angle").unwrap(), &auxiliary),
            vec![("X".into(), 7.0), ("Y".into(), 20.0), ("Z".into(), -3.0)]
        );
        auxiliary.insert("corner_x".into(), 11.0);
        auxiliary.insert("corner_y".into(), 13.0);
        assert_eq!(
            values.work_positions(operation("corner_x_plus_edge_angle").unwrap(), &auxiliary),
            vec![("X".into(), 11.0), ("Y".into(), 13.0), ("Z".into(), -3.0)]
        );
        let block = operation("top_front_edge_angle")
            .unwrap()
            .apply_block(2, &values, Some(2.0))
            .unwrap();
        assert!(block.contains("X[#<_linc_start_x> + #<_linc_probe_x_offset>]"));
        assert!(block.contains("Y[[#1008 - #1011] + #<_linc_probe_y_offset>]"));
        assert!(block.ends_with("R2"));
        assert!(!block.contains("[[["));
    }

    fn vendored_subroutines() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../subroutines/probe_basic")
    }

    fn apply_result(
        operation: &ProbeOperation,
        updated: Vec<u16>,
        wcs: &str,
        applied: bool,
    ) -> crate::model::ProbeResult {
        let mut values = [0.0; 12];
        for parameter in &updated {
            values[(parameter - 1001) as usize] = 1.0;
        }
        let results = ParameterResults::from_values(&values);
        let source = crate::model::AxisMap::from_entries([
            ("X".into(), 10.0),
            ("Y".into(), 20.0),
            ("Z".into(), 30.0),
        ]);
        crate::model::ProbeResult::from_operation(
            operation,
            &results,
            source,
            wcs.into(),
            "POSITION".into(),
            updated,
            std::collections::BTreeMap::new(),
            None,
            applied,
        )
    }

    #[test]
    fn apply_requires_a_complete_current_result_in_the_source_wcs() {
        let edge = operation("edge_x_plus").unwrap();
        // A complete result captured in the active WCS is applicable.
        assert_eq!(
            apply_result(edge, vec![1006], "G54", false).apply_error(Some(1)),
            Ok(())
        );
        // A result from another WCS is refused.
        assert!(apply_result(edge, vec![1006], "G55", false)
            .apply_error(Some(1))
            .is_err());
        // A result missing the slot its Apply term needs is refused.
        assert!(apply_result(edge, vec![], "G54", false)
            .apply_error(Some(1))
            .is_err());
        // An already-applied result is refused.
        assert!(apply_result(edge, vec![1006], "G54", true)
            .apply_error(Some(1))
            .is_err());
        // Without an active work offset there is nothing to apply to.
        assert!(apply_result(edge, vec![1006], "G54", false)
            .apply_error(None)
            .is_err());
        // Calibration and spindle nose have no generic Apply at all.
        for id in ["cal_round_boss", "spindle_nose"] {
            assert!(
                apply_result(operation(id).unwrap(), vec![1001, 1009], "G54", false)
                    .apply_error(Some(1))
                    .is_err(),
                "{id}"
            );
        }
    }

    #[test]
    fn values_match_parameter_contract_order() {
        let mut parameters = ProbeParameters::default();
        parameters.probe_tool_number = 7;
        parameters.slow_feed = 11.0;
        parameters.fast_feed = 22.0;
        parameters.traverse_feed = 33.0;
        parameters.max_xy_distance = 1.5;
        parameters.xy_clearance = 2.5;
        parameters.max_z_distance = 3.5;
        parameters.z_clearance = 4.5;
        parameters.extra_depth = 0.25;
        parameters.step_off_width = 5.5;
        parameters.edge_width = 6.5;
        parameters.diameter_hint = 7.5;
        parameters.x_hint_boss_pocket = 8.5;
        parameters.y_hint_boss_pocket = 9.5;
        parameters.x_hint_ridge_valley = 10.5;
        parameters.y_hint_ridge_valley = 11.5;
        parameters.probe_mode = ProbeMode::ApplyWcs;
        parameters.wcs_rotation = true;
        parameters.calibration_offset = 0.012;
        parameters.calibration_diameter = 4.0;
        parameters.x_cal_width = 12.5;
        parameters.y_cal_width = 13.5;
        parameters.square_cal_axis = 2;

        let values = parameters.values();
        assert_eq!(values.len(), 23, "#3014..#3036");
        assert_eq!(
            values,
            vec![
                7.0, 11.0, 22.0, 33.0, 1.5, 2.5, 3.5, 4.5, 0.25, 5.5, 6.5, 7.5, 8.5, 9.5, 10.5,
                11.5, 0.0, 1.0, 0.012, 4.0, 12.5, 13.5, 2.0
            ]
        );
        // The MDI builder round-trips exactly the values it was given.
        assert_eq!(
            ProbeParameters::from_mdi(&parameters.parameter_update_block()),
            Some(parameters)
        );
    }

    #[test]
    fn mdi_builder_emits_parameter_update_then_call() {
        let parameters = ProbeParameters::default();
        for operation in operations() {
            let blocks = parameters.blocks(operation);
            assert_eq!(blocks.len(), 3, "{}", operation.id);
            assert!(
                blocks[0].starts_with("o<touch_probe_param_update> call "),
                "{}",
                operation.id
            );
            assert_eq!(blocks[1], "o<linc_probe_context> call", "{}", operation.id);
            assert_eq!(blocks[2], format!("o<{}> call", operation.call));
            assert_eq!(parameters.mdi_text(operation), blocks.join("\n"));
            assert_eq!(
                ProbeParameters::from_mdi(&parameters.mdi_text(operation)),
                Some(parameters.clone()),
                "{}",
                operation.id
            );
        }
    }

    #[test]
    fn probe_mdi_blocks_pass_the_issue_preview() {
        let axes: Vec<String> = ["X", "Y", "Z"]
            .iter()
            .map(|axis| axis.to_string())
            .collect();
        let mut parameters = ProbeParameters::default();
        for mode in [ProbeMode::MeasureOnly, ProbeMode::ApplyWcs] {
            parameters.probe_mode = mode;
            for rotation in [false, true] {
                parameters.wcs_rotation = rotation;
                for operation in operations() {
                    for line in parameters.mdi_text(operation).lines() {
                        let preview = crate::gcode::parse_preview(line, &axes, None, [0.0; 3]);
                        assert!(
                            preview.error.is_none(),
                            "{} {mode:?} rotation={rotation}: {line}",
                            operation.id
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn catalog_covers_all_categories_and_unambiguous_calls() {
        let mut ids = std::collections::BTreeSet::new();
        let mut calls = std::collections::BTreeSet::new();
        for operation in operations() {
            assert!(ids.insert(operation.id), "duplicate id {}", operation.id);
            assert!(
                calls.insert(operation.call),
                "duplicate call {}",
                operation.call
            );
            assert!(!operation.id.is_empty() && !operation.name.is_empty());
        }
        // A number key selects by catalogue position, so the order is fixed
        // and every id resolves back to the same routine.
        for (index, entry) in operations().iter().enumerate() {
            assert_eq!(operations()[index].id, entry.id);
            assert_eq!(
                operation(entry.id).map(|found| found.call),
                Some(entry.call)
            );
        }
        // The tabs need routines, and no call may be a prefix of another, so
        // an "o<...> call" token can never resolve to a shorter name.
        let used: Vec<ProbeCategory> = operations().iter().map(|op| op.category).collect();
        for category in [
            ProbeCategory::OutsideCorners,
            ProbeCategory::InsideCorners,
            ProbeCategory::BossAndPocket,
            ProbeCategory::RidgeAndValley,
            ProbeCategory::EdgeAngle,
            ProbeCategory::Calibrate,
        ] {
            assert!(
                used.contains(&category),
                "{} has no routines",
                category.label()
            );
            assert!(!operations_in(category).is_empty());
        }
        // Longer names that share a prefix must still resolve to themselves:
        // the "o<name> call" token is '>'-terminated, so a shorter sibling can
        // never steal a call (PR-08).
        let resolve = |block: &str| -> Option<&'static str> {
            operations()
                .iter()
                .find(|candidate| {
                    let call = format!("o<{}>", candidate.call);
                    block
                        .trim()
                        .strip_prefix(&call)
                        .and_then(|rest| rest.split_whitespace().next())
                        == Some("call")
                })
                .map(|candidate| candidate.id)
        };
        for entry in operations() {
            let block = format!("o<{}> call [#3014]", entry.call);
            assert_eq!(resolve(&block), Some(entry.id), "{}", entry.call);
            for shorter in operations() {
                if shorter.call.len() < entry.call.len() && entry.call.starts_with(shorter.call) {
                    assert_ne!(
                        resolve(&block),
                        Some(shorter.id),
                        "{} resolved to the shorter {}",
                        entry.call,
                        shorter.call
                    );
                }
            }
        }
    }

    #[test]
    fn every_operation_has_a_known_result_family() {
        for id in ["surface_z", "spindle_nose"] {
            assert_eq!(
                operation(id).unwrap().result_family(),
                ResultFamily::SurfaceZ,
                "{id}"
            );
        }
        for id in ["edge_x_minus", "edge_x_plus", "edge_y_minus", "edge_y_plus"] {
            assert_eq!(
                operation(id).unwrap().result_family(),
                ResultFamily::Edge,
                "{id}"
            );
        }
        for operation in operations_in(ProbeCategory::BossAndPocket) {
            assert_eq!(
                operation.result_family(),
                ResultFamily::BossPocket,
                "{}",
                operation.id
            );
        }
        // The labelled measurements follow the family layout, not the id.
        let values =
            ParameterResults::from_values(&[50.0, 1.0, 30.0, 2.0, -1.0, 1.0, -2.0, 2.0, -3.0]);
        let surface = values.measurements(ResultFamily::SurfaceZ);
        assert_eq!(surface.len(), 1);
        assert_eq!(surface[0].parameter, 1009);
        assert_eq!(surface[0].slot, ResultSlot::ZSurface);
        assert_eq!(surface[0].value, -3.0);
        let edge = values.measurements(ResultFamily::Edge);
        assert_eq!(
            edge.iter().map(|m| m.parameter).collect::<Vec<_>>(),
            vec![1005, 1006, 1007, 1008]
        );
        assert_eq!(edge[0].slot, ResultSlot::XMinus);
        assert_eq!(edge[3].slot, ResultSlot::YPlus);
        assert_eq!(values.measurements(ResultFamily::BossPocket).len(), 4);
    }

    #[test]
    fn every_operation_matches_the_probe_basic_result_and_apply_contract() {
        for operation in operations() {
            let declared = operation.result_parameters();
            let mut unique = declared.clone();
            unique.dedup();
            assert_eq!(unique, declared, "{} repeats a slot", operation.id);
            for parameter in &declared {
                assert!(
                    (1001..=1012).contains(parameter),
                    "{} declares #{parameter}",
                    operation.id
                );
            }
            let mut axes: Vec<&'static str> = Vec::new();
            for term in operation.apply_terms() {
                assert!(
                    ["X", "Y", "Z"].contains(&term.axis),
                    "{} applies {}",
                    operation.id,
                    term.axis
                );
                assert!(
                    !axes.contains(&term.axis),
                    "{} applies {} twice",
                    operation.id,
                    term.axis
                );
                axes.push(term.axis);
                match term.source {
                    ApplySource::Slot(parameter)
                    | ApplySource::SlotMinusDelta(parameter)
                    | ApplySource::SlotPlusDelta(parameter) => assert!(
                        declared.contains(&parameter),
                        "{} applies #{parameter} but never reports it",
                        operation.id
                    ),
                    _ => {}
                }
                if matches!(
                    term.source,
                    ApplySource::SlotMinusDelta(_) | ApplySource::SlotPlusDelta(_)
                ) {
                    assert!(
                        declared.contains(&1011),
                        "{} applies the edge delta but never reports #1011",
                        operation.id
                    );
                }
            }
            // Calibration is stored by the routine and the DRO-only routines
            // have no generic Apply; everything else must offer one.
            if operation.calibration || !operation.applies_wcs {
                assert!(
                    operation.apply_terms().is_empty(),
                    "{} must not offer Apply",
                    operation.id
                );
            } else {
                assert!(
                    !operation.apply_terms().is_empty(),
                    "{} has no Apply terms",
                    operation.id
                );
            }
        }
    }

    #[test]
    fn wcs_offsets_follow_the_routine_family() {
        let values = ParameterResults::from_values(&[
            50.0, 12.5, 30.0, -4.25, -100.0, 100.0, -200.0, 200.0, -3.5, 40.0, 1.25, 2.5,
        ]);
        // Edge routines read their own face slot only.
        assert_eq!(
            values.wcs_offsets(operation("edge_x_minus").unwrap()),
            vec![("X".into(), -100.0)]
        );
        assert_eq!(
            values.wcs_offsets(operation("edge_y_plus").unwrap()),
            vec![("Y".into(), 200.0)]
        );
        // Boss/pocket routines read the centre slots and the surface.
        assert_eq!(
            values.wcs_offsets(operation("round_boss").unwrap()),
            vec![("X".into(), 12.5), ("Y".into(), -4.25), ("Z".into(), -3.5)]
        );
        // Ridge/valley carry Z; their centre-start variants do not.
        assert_eq!(
            values.wcs_offsets(operation("ridge_x").unwrap()),
            vec![("X".into(), 12.5), ("Z".into(), -3.5)]
        );
        assert_eq!(
            values.wcs_offsets(operation("valley_y").unwrap()),
            vec![("Y".into(), -4.25), ("Z".into(), -3.5)]
        );
        assert_eq!(
            values.wcs_offsets(operation("valley_x_center").unwrap()),
            vec![("X".into(), 12.5)]
        );
        // Surface Z is Z only.
        assert_eq!(
            values.wcs_offsets(operation("surface_z").unwrap()),
            vec![("Z".into(), -3.5)]
        );
    }

    #[test]
    fn z_surface_result_is_never_read_as_x() {
        let values = ParameterResults::from_values(&[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -3.5]);
        let surface = operation("surface_z").unwrap();
        assert_eq!(values.wcs_offsets(surface), vec![("Z".into(), -3.5)]);
        let block = surface.apply_block(1, &values, None).unwrap();
        assert_eq!(block, "G10 L2 P1 Z[#1009 + #<_linc_probe_z_offset>]");
        assert!(!block.contains(" X") && !block.contains(" Y"));
        // An edge X result reads the probed face, never the Z surface slot.
        let edge_values =
            ParameterResults::from_values(&[0.0, 0.0, 0.0, 0.0, 0.0, 100.0, 0.0, 0.0, -3.5]);
        assert_eq!(
            edge_values.wcs_offsets(operation("edge_x_plus").unwrap()),
            vec![("X".into(), 100.0)]
        );
    }

    #[test]
    fn calibration_error_is_limited_to_measurement_calibration() {
        let mut parameters = ProbeParameters::default();
        parameters.calibration_diameter = 50.0;
        parameters.x_cal_width = 40.0;
        parameters.y_cal_width = 30.0;
        parameters.square_cal_axis = 0;
        // Measured 50.04 and 49.98 against a 50.0 diameter: 0.02 and 0.01.
        let values = ParameterResults::from_values(&[
            50.04, 1.0, 49.98, 2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 50.01,
        ]);
        let round = operation("cal_round_boss").unwrap();
        assert!((values.calibration_error(round, &parameters).unwrap() - 0.015).abs() < 1e-9);
        // The axis selector picks one axis instead of the average.
        parameters.square_cal_axis = 1;
        assert!((values.calibration_error(round, &parameters).unwrap() - 0.02).abs() < 1e-9);
        parameters.square_cal_axis = 2;
        assert!((values.calibration_error(round, &parameters).unwrap() - 0.01).abs() < 1e-9);
        // Nothing outside measurement calibration carries a calibration error,
        // and a reset never does.
        for id in ["cal_reset", "edge_x_plus", "round_boss", "surface_z"] {
            assert_eq!(
                values.calibration_error(operation(id).unwrap(), &parameters),
                None,
                "{id}"
            );
        }
        // Without a positive expected size there is nothing to compare against.
        parameters.square_cal_axis = 0;
        parameters.calibration_diameter = 0.0;
        assert_eq!(values.calibration_error(round, &parameters), None);
    }

    #[test]
    fn validation_requires_probe_tool_and_positive_feeds() {
        let parameters = ProbeParameters::default();
        let tool = parameters.probe_tool_number;
        assert_eq!(parameters.validate(true, tool), Ok(()));
        // The probe must be enabled and in the spindle.
        assert!(parameters.validate(false, tool).is_err());
        assert!(parameters.validate(true, 1).is_err());
        for broken in [
            |parameters: &mut ProbeParameters| parameters.slow_feed = -1.0,
            |parameters: &mut ProbeParameters| parameters.fast_feed = 0.0,
            |parameters: &mut ProbeParameters| parameters.traverse_feed = 1.0,
            |parameters: &mut ProbeParameters| parameters.xy_clearance = 0.0,
            |parameters: &mut ProbeParameters| parameters.z_clearance = 0.0,
            |parameters: &mut ProbeParameters| parameters.edge_width = -2.0,
            |parameters: &mut ProbeParameters| parameters.extra_depth = -0.5,
            |parameters: &mut ProbeParameters| parameters.square_cal_axis = 3,
        ] {
            let mut parameters = parameters.clone();
            broken(&mut parameters);
            assert!(
                parameters
                    .validate(true, parameters.probe_tool_number)
                    .is_err(),
                "{parameters:?}"
            );
        }
    }

    #[test]
    fn validation_requires_probe_tool_feeds_and_operation_hints() {
        let mut parameters = ProbeParameters::default();
        let tool = parameters.probe_tool_number;
        // Round boss/pocket needs a positive diameter hint.
        parameters.diameter_hint = 0.0;
        assert!(parameters
            .validate_for_operation(true, tool, operation("round_boss").unwrap())
            .is_err());
        parameters.diameter_hint = 30.0;
        assert_eq!(
            parameters.validate_for_operation(true, tool, operation("round_boss").unwrap()),
            Ok(())
        );
        // Rectangular boss/pocket needs both width hints.
        parameters.x_hint_boss_pocket = 0.0;
        assert!(parameters
            .validate_for_operation(true, tool, operation("rect_boss").unwrap())
            .is_err());
        parameters.x_hint_boss_pocket = 10.0;
        parameters.y_hint_boss_pocket = 20.0;
        assert_eq!(
            parameters.validate_for_operation(true, tool, operation("rect_boss").unwrap()),
            Ok(())
        );
        // Ridge/valley needs the hint for its own axis.
        parameters.x_hint_ridge_valley = 0.0;
        assert!(parameters
            .validate_for_operation(true, tool, operation("ridge_x").unwrap())
            .is_err());
        parameters.x_hint_ridge_valley = 5.0;
        assert_eq!(
            parameters.validate_for_operation(true, tool, operation("ridge_x").unwrap()),
            Ok(())
        );
        // Calibration needs the workpiece size, except for the reset.
        parameters.calibration_diameter = 0.0;
        assert!(parameters
            .validate_for_operation(true, tool, operation("cal_round_boss").unwrap())
            .is_err());
        assert_eq!(
            parameters.validate_for_operation(true, tool, operation("cal_reset").unwrap()),
            Ok(())
        );
        assert!(parameters
            .validate_for_operation(false, tool, operation("cal_reset").unwrap())
            .is_err());
        // A routine without size hints only needs the common contract.
        assert_eq!(
            parameters.validate_for_operation(true, tool, operation("edge_x_plus").unwrap()),
            Ok(())
        );
        assert!(parameters
            .validate_for_operation(true, 1, operation("edge_x_plus").unwrap())
            .is_err());
    }

    #[test]
    fn every_operation_call_is_vendored() {
        let dir = vendored_subroutines();
        assert!(
            dir.is_dir(),
            "missing Probe Basic subroutine bundle at {}",
            dir.display()
        );
        for operation in operations() {
            let path = dir.join(format!("{}.ngc", operation.call));
            assert!(
                path.is_file(),
                "{} calls o<{}>, but {} is missing",
                operation.id,
                operation.call,
                path.display()
            );
        }
    }

    #[test]
    fn every_vendored_call_is_also_vendored() {
        let dir = vendored_subroutines();
        let mut checked = 0;
        for entry in std::fs::read_dir(&dir).expect("subroutine bundle") {
            let path = entry.expect("directory entry").path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("ngc") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read subroutine");
            for line in text.lines() {
                let lower = line.trim().to_ascii_lowercase();
                let Some(rest) = lower.split("o<").nth(1) else {
                    continue;
                };
                if !rest.contains("> call") {
                    continue;
                }
                let Some(name) = rest.split('>').next() else {
                    continue;
                };
                let name = name.trim();
                if name.is_empty() || name.chars().all(|ch| ch.is_ascii_digit()) {
                    continue;
                }
                let called = dir.join(format!("{name}.ngc"));
                assert!(
                    called.is_file(),
                    "{} calls o<{}>, but {} is missing",
                    path.display(),
                    name,
                    called.display()
                );
            }
            checked += 1;
        }
        assert!(checked >= 40, "only {checked} subroutines were scanned");
    }

    /// Re-derive a routine's effective DIRECT axes from its own `G10 L2`
    /// branch *and* the helpers it calls with the mode argument `#3030`, so a
    /// top-level-only reading can never declare an incomplete table correct.
    fn direct_axes_in_file(path: &std::path::Path, depth: u8) -> Vec<&'static str> {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Vec::new();
        };
        let mut axes: Vec<&'static str> = Vec::new();
        let mut direct = false;
        for line in text.lines() {
            let line = line.trim();
            let lower = line.to_ascii_lowercase();
            if line.starts_with('o') && lower.contains(" if [") {
                direct = lower.contains("probe_mode> eq 0");
            } else if line.starts_with('o') && lower.contains(" endif") {
                direct = false;
            } else if direct && lower.starts_with("g10 l2") {
                for (word, axis) in [("x[", "X"), ("y[", "Y"), ("z[", "Z")] {
                    if lower.contains(word) && !axes.contains(&axis) {
                        axes.push(axis);
                    }
                }
            }
            // Follow a helper call that receives the mode, because the helper
            // decides for itself whether to write the row.
            if depth > 0 && lower.contains("> call") && lower.contains("#3030") {
                let Some(name) = lower
                    .split("o<")
                    .nth(1)
                    .and_then(|rest| rest.split('>').next())
                else {
                    continue;
                };
                let name = name.trim();
                if name.is_empty() || name.chars().all(|ch| ch.is_ascii_digit()) {
                    continue;
                }
                let helper = path.with_file_name(format!("{name}.ngc"));
                for axis in direct_axes_in_file(&helper, depth - 1) {
                    if !axes.contains(&axis) {
                        axes.push(axis);
                    }
                }
            }
        }
        axes
    }

    fn block_for(id: &str, mode: ProbeMode) -> String {
        let mut parameters = ProbeParameters::default();
        parameters.probe_mode = mode;
        parameters.mdi_text(operation(id).expect("operation"))
    }

    #[test]
    fn probe_write_action_classifies_every_action() {
        // DIRECT-WCO wins over the Edge Angle label: it is the wider write.
        assert_eq!(
            probe_write_action(&block_for("edge_x_plus", ProbeMode::ApplyWcs)),
            Some(ProbeWriteAction::DirectWco)
        );
        assert_eq!(
            probe_write_action(&block_for("top_left_edge_angle", ProbeMode::ApplyWcs)),
            Some(ProbeWriteAction::DirectWco)
        );
        // A POSITION Edge Angle run still clears rotation with R0.
        for id in [
            "top_front_edge_angle",
            "corner_y_minus_edge_angle",
            "edge_x_plus",
        ] {
            let expected = if id == "edge_x_plus" {
                None
            } else {
                Some(ProbeWriteAction::EdgeAngleRotation)
            };
            assert_eq!(
                probe_write_action(&block_for(id, ProbeMode::MeasureOnly)),
                expected,
                "{id}"
            );
        }
        // Calibration is classified by the catalogue flag, not the category:
        // spindle_nose is SurfaceZ yet writes #3010/#3007
        // (probe_spindle_nose.ngc:65-68).
        assert_eq!(
            operation("spindle_nose").map(|operation| (operation.category, operation.calibration)),
            Some((ProbeCategory::SurfaceZ, true))
        );
        for id in ["spindle_nose", "cal_round_boss", "cal_square_pocket"] {
            assert_eq!(
                probe_write_action(&block_for(id, ProbeMode::MeasureOnly)),
                Some(ProbeWriteAction::CalibrationRun),
                "{id}"
            );
        }
        // Controller reset and the explicit parameter/data actions.
        assert_eq!(
            probe_write_action("o<probe_cal_reset> call"),
            Some(ProbeWriteAction::CalibrationReset)
        );
        assert_eq!(
            probe_write_action(&ProbeParameters::default().parameter_update_block()),
            Some(ProbeWriteAction::ParameterUpdate)
        );
        for block in [
            "o<reset_all_data> call",
            "o<x_data_reset> call",
            "o<y_data_reset> call",
        ] {
            assert_eq!(
                probe_write_action(block),
                Some(ProbeWriteAction::ResultReset),
                "{block}"
            );
        }
        // No typed parameter block: the inherited mode is unknown.
        assert_eq!(
            probe_write_action("o<probe_x_plus_wco> call"),
            Some(ProbeWriteAction::UnknownRun)
        );
        // Ordinary POSITION runs and unrelated blocks write nothing.
        for id in ["edge_x_plus", "round_boss", "valley_x", "surface_z"] {
            assert_eq!(
                probe_write_action(&block_for(id, ProbeMode::MeasureOnly)),
                None,
                "{id}"
            );
        }
        assert_eq!(probe_write_action("o<linc_probe_context> call"), None);
        assert_eq!(probe_write_action("G0 X0"), None);
        assert_eq!(probe_write_action("M2"), None);
        // The exact-token resolver keeps prefix names apart (PR-08).
        assert_eq!(
            operation_for_mdi("o<probe_round_pocket_center_start> call").map(|op| op.id),
            Some("round_pocket_center")
        );
        assert_eq!(
            operation_for_mdi("o<probe_round_pocket> call").map(|op| op.id),
            Some("round_pocket")
        );
    }

    #[test]
    fn direct_written_axes_match_the_subroutine_call_graph() {
        let dir = vendored_subroutines();
        let mut checked = 0;
        for operation in operations() {
            let path = dir.join(format!("{}.ngc", operation.call));
            let mut derived = direct_axes_in_file(&path, 2);
            derived.sort_unstable();
            let mut declared = direct_written_axes(operation).to_vec();
            declared.sort_unstable();
            assert_eq!(
                derived, declared,
                "{} ({}): the DIRECT call graph and direct_written_axes disagree",
                operation.id, operation.call
            );
            checked += 1;
        }
        assert!(checked >= 43, "only {checked} routines were checked");
    }
}
