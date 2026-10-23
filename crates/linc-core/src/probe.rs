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

/// The tool number the Probe Basic parameter contract expects in `#3014`.
///
/// A different value is not fatal, but it silently changes which tool the
/// probe cycles assume is mounted, so it is worth surfacing at startup.
pub const EXPECTED_PROBE_TOOL: u16 = 99;

/// Probe Basic parameter contract `#3014`–`#3036`.
#[derive(Clone, Debug, PartialEq)]
pub struct ProbeParameters {
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
    pub wcs_rotation: f64,
    pub calibration_offset: f64,
    pub calibration_diameter: f64,
    pub x_cal_width: f64,
    pub y_cal_width: f64,
    pub square_cal_axis: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
            probe_mode: ProbeMode::ApplyWcs,
            wcs_rotation: 0.0,
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
            diameter_hint: config.probe_diameter,
            calibration_diameter: config.calibration_diameter,
            ..Default::default()
        }
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
            self.wcs_rotation,
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
        for (name, value) in [
            ("slow feed", self.slow_feed),
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

    /// Build the two MDI blocks used by `RUN PROBE`: update the Probe Basic
    /// parameter block, then invoke the selected routine. `IssueMdi` splits
    /// newline-separated blocks and issues them sequentially.
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

/// Probe result contract `#1001`–`#1009`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ParameterResults {
    pub values: [f64; 9],
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
        let mut out = [0.0; 9];
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

    /// The work-offset values this result actually determines, as
    /// `(axis, value)`. Only the axes the routine measured are returned; an
    /// axis the routine did not touch is never written.
    pub fn wcs_offsets(&self, operation: &ProbeOperation) -> Vec<(String, f64)> {
        let call = operation.call;
        match operation.result_family() {
            // The corner/boss/pocket routines locate a feature centre.
            ResultFamily::BossPocket => vec![
                ("X".to_string(), self.values[1]),
                ("Y".to_string(), self.values[3]),
            ],
            // A single-edge routine locates one face.
            ResultFamily::Edge => {
                let mut out = Vec::new();
                if call.contains("x_plus") {
                    out.push(("X".to_string(), self.values[5]));
                } else if call.contains("x_minus") {
                    out.push(("X".to_string(), self.values[4]));
                } else if call.contains("y_plus") {
                    out.push(("Y".to_string(), self.values[7]));
                } else if call.contains("y_minus") {
                    out.push(("Y".to_string(), self.values[6]));
                }
                out
            }
            ResultFamily::SurfaceZ => vec![("Z".to_string(), self.values[8])],
        }
    }

    /// The probe error, when the routine measured a feature whose size is
    /// known from the configured diameter. Never fabricated: `None` when the
    /// routine does not determine an error.
    pub fn error(&self, operation: &ProbeOperation, expected_diameter: Option<f64>) -> Option<f64> {
        if operation.result_family() != ResultFamily::BossPocket {
            return None;
        }
        let expected = expected_diameter?;
        if expected <= 0.0 {
            return None;
        }
        let x = self.values[0];
        let y = self.values[2];
        if x <= 0.0 && y <= 0.0 {
            return None;
        }
        let mut worst = 0.0_f64;
        if x > 0.0 {
            worst = worst.max((x - expected).abs());
        }
        if y > 0.0 {
            worst = worst.max((y - expected).abs());
        }
        Some(worst)
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
    fn wcs_offsets_follow_the_routine_family() {
        let values = ParameterResults::from_values(&[
            50.0, 12.5, 30.0, -4.25, -100.0, 100.0, -200.0, 200.0, -3.5,
        ]);
        let corner = operations()
            .iter()
            .find(|op| op.call.contains("front_left_top"))
            .expect("corner routine");
        assert_eq!(corner.result_family(), ResultFamily::BossPocket);
        assert_eq!(
            values.wcs_offsets(corner),
            vec![("X".to_string(), 12.5), ("Y".to_string(), -4.25)]
        );

        let z = operations()
            .iter()
            .find(|op| op.result_family() == ResultFamily::SurfaceZ)
            .expect("z routine");
        assert_eq!(values.wcs_offsets(z), vec![("Z".to_string(), -3.5)]);
    }

    #[test]
    fn z_surface_result_is_never_read_as_x() {
        // A Z-surface routine writes only #1009. Reading #1001 as X (the old
        // behaviour) displayed a stale width as the probed X coordinate.
        let results =
            ParameterResults::from_values(&[0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, -7.25]);
        let measured = results.measurements(ResultFamily::SurfaceZ);
        assert_eq!(measured.len(), 1);
        assert_eq!(measured[0].parameter, 1009);
        assert_eq!(measured[0].slot, ResultSlot::ZSurface);
        assert_eq!(measured[0].value, -7.25);
    }

    #[test]
    fn every_operation_has_a_known_result_family() {
        for operation in operations() {
            let family = operation.result_family();
            let used = family.slots().iter().filter(|slot| slot.used()).count();
            assert!(used > 0, "{} has no result slots", operation.call);
        }
    }

    #[test]
    fn catalog_covers_all_categories_and_unambiguous_calls() {
        assert!(operations().len() >= 36);
        assert_eq!(ProbeCategory::ALL.len(), 7);
        let mut calls = std::collections::HashSet::new();
        for operation in operations() {
            assert!(calls.insert(operation.call), "duplicate {}", operation.call);
        }
        for category in ProbeCategory::ALL {
            if category == ProbeCategory::ProbeHelp {
                continue;
            }
            assert!(
                !operations_in(category).is_empty(),
                "empty category {:?}",
                category
            );
        }
    }

    #[test]
    fn mdi_builder_emits_parameter_update_then_call() {
        let params = ProbeParameters::default();
        let operation = operation("front_left_top_corner").unwrap();
        let blocks = params.blocks(operation);
        assert_eq!(blocks.len(), 2);
        assert!(blocks[0].starts_with("o<touch_probe_param_update> call [99]"));
        assert_eq!(blocks[1], "o<probe_front_left_top_corner> call");
    }

    #[test]
    fn validation_requires_probe_tool_and_positive_feeds() {
        let mut params = ProbeParameters::default();
        assert!(params.validate(true, 99).is_ok());
        assert!(params.validate(true, 12).is_err());
        params.slow_feed = 900.0;
        assert!(params.validate(true, 99).is_err());
        params.slow_feed = 10.0;
        params.fast_feed = 5.0;
        assert!(params.validate(true, 99).is_err());
    }

    #[test]
    fn values_match_parameter_contract_order() {
        let params = ProbeParameters::default();
        let values = params.values();
        assert_eq!(values.len(), 23);
        assert_eq!(values[0], 99.0);
        assert_eq!(values[16], 0.0);
    }

    fn vendored_subroutines() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../subroutines/probe_basic")
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
        for support in [
            "touch_probe_param_update",
            "reset_all_data",
            "x_data_reset",
            "y_data_reset",
            "on_abort",
        ] {
            let path = dir.join(format!("{support}.ngc"));
            assert!(path.is_file(), "missing support routine {}", path.display());
        }
    }

    #[test]
    fn every_vendored_call_is_also_vendored() {
        let dir = vendored_subroutines();
        for entry in std::fs::read_dir(&dir).expect("read subroutine bundle") {
            let path = entry.expect("directory entry").path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("ngc") {
                continue;
            }
            let text = std::fs::read_to_string(&path).expect("read subroutine");
            for line in text.lines() {
                let lower = line.to_ascii_lowercase();
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
        }
    }
}
