//! The TLO card: the offsets the loaded tool carries.
//!
//! `G43` applies the tool table's own offsets for the tool in the spindle, so
//! the numbers here are what the controller is adding to the machine position,
//! not a figure the HMI computes. The radius is the cutter compensation the same
//! row carries: half the `D` diameter on a mill, the `R` nose radius on a lathe.
//!
//! Two cards draw this readout. MAIN's bottom strip shows it beside FEED and
//! CUT, and the alternate dashboard's TOOL LENGTH OFFSET card shows the same
//! numbers in the larger type the coordinate matrix uses, so the two cannot
//! disagree about what the loaded tool is.

use egui::Ui;
use linc_core::model::{MachineSnapshot, MachineVariant, ToolColumn, ToolRow};

use crate::screens::common::{self, ValueRow};
use crate::shell::LincApp;
use crate::theme;

/// Decimals the tool table writes its offsets with.
const OFFSET_DECIMALS: usize = 4;

/// The offsets the loaded tool carries.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolLengths {
    /// Tool number, `0` when the spindle is empty.
    pub tool: u16,
    /// One entry per offset axis the tool table declares, in the order
    /// `TOOL_TABLE_COLUMNS` lists them. `None` means the row carries no word for
    /// that axis, which LinuxCNC reads as zero.
    pub axial: Vec<(String, Option<f64>)>,
    /// The cutter-compensation radius, when the row describes one.
    pub radius: Option<f64>,
}

/// The offset one axis of a tool row carries.
///
/// `Z` is the length: LinuxCNC's `Z` word *is* the tool length, and the loader
/// fills both fields, so a row from either path reads the same number.
fn axis_offset(row: &ToolRow, axis: &str) -> Option<f64> {
    if axis == "Z" {
        row.geometry
            .length
            .or_else(|| row.geometry.values.get("Z").copied())
    } else {
        row.geometry.values.get(axis).copied()
    }
}

/// The radius cutter compensation uses, from the same row.
///
/// A mill compensates by the cutter's radius, which the table states as a
/// diameter. A lathe states the nose radius itself, and a turret tool that has
/// only a diameter falls back to its half rather than claiming no radius.
fn tool_radius(row: &ToolRow, variant: MachineVariant) -> Option<f64> {
    match variant {
        MachineVariant::Lathe => row
            .geometry
            .nose_radius
            .or_else(|| row.geometry.diameter.map(|diameter| diameter / 2.0)),
        _ => row.geometry.diameter.map(|diameter| diameter / 2.0),
    }
}

/// Read the loaded tool's offsets from a snapshot.
///
/// `columns` is what the tool table declares (`[EMCIO] TOOL_TABLE_COLUMNS`), so
/// the card lists exactly the offsets the file carries and the controller
/// applies — a machine whose table names only Z has no X or Y row to show.
pub fn read(
    snapshot: &MachineSnapshot,
    columns: &[ToolColumn],
    variant: MachineVariant,
) -> ToolLengths {
    let tool = snapshot.tool_change.current_tool;
    let row = snapshot.tool_table.rows.iter().find(|row| row.tool == tool);
    ToolLengths {
        tool,
        axial: columns
            .iter()
            .map(|column| {
                let axis = column.label();
                let value = row.and_then(|row| axis_offset(row, axis));
                (axis.to_string(), value)
            })
            .collect(),
        radius: row.and_then(|row| tool_radius(row, variant)),
    }
}

impl ToolLengths {
    /// How many lines [`Self::rows`] draws: one per configured axis and the
    /// radius.
    pub fn line_count(&self) -> usize {
        self.axial.len() + 1
    }

    /// The machine units the offsets are in, as the CUT card spells them.
    fn units(&self, snapshot: &MachineSnapshot) -> String {
        if snapshot.feed.units.is_empty() {
            "mm".into()
        } else {
            snapshot.feed.units.clone()
        }
    }

    /// The card's rows: every configured axis, then the radius.
    ///
    /// An axis the table does not carry keeps its row and shows a dash: a
    /// missing row would read as an axis this machine does not have, and the
    /// offset really is zero.
    pub fn rows(&self, snapshot: &MachineSnapshot) -> Vec<ValueRow> {
        let units = self.units(snapshot);
        let mut rows: Vec<ValueRow> = self
            .axial
            .iter()
            .map(|(axis, value)| {
                let mut row = match value {
                    Some(value) => ValueRow::new(
                        axis.clone(),
                        theme::format_position(*value, OFFSET_DECIMALS),
                        units.clone(),
                    ),
                    None => ValueRow::new(axis.clone(), "—", String::new()),
                };
                row.muted = value.is_none();
                row
            })
            .collect();
        let mut radius = match self.radius {
            Some(value) => {
                ValueRow::new("RAD", theme::format_position(value, OFFSET_DECIMALS), units)
            }
            None => ValueRow::new("RAD", "—", String::new()),
        };
        radius.muted = self.radius.is_none();
        rows.push(radius);
        rows
    }

    /// The same rows as text pairs, for the large matrix card: axis label and
    /// formatted value, with no units column.
    pub fn lines(&self, snapshot: &MachineSnapshot) -> Vec<(String, String, bool)> {
        let units = self.units(snapshot);
        let quantity = |value: Option<f64>| match value {
            Some(value) => (
                theme::format_position(value, OFFSET_DECIMALS),
                units.clone(),
                false,
            ),
            None => ("—".into(), String::new(), true),
        };
        self.axial
            .iter()
            .map(|(axis, value)| {
                let (text, units, muted) = quantity(*value);
                (
                    axis.clone(),
                    format!("{text} {units}").trim_end().to_string(),
                    muted,
                )
            })
            .chain(std::iter::once({
                let (text, units, muted) = quantity(self.radius);
                (
                    "RAD".to_string(),
                    format!("{text} {units}").trim_end().to_string(),
                    muted,
                )
            }))
            .collect()
    }
}

/// Height the TLO card needs for `lines` rows: the title, the frame's two
/// margins, and the rows at the shared value-row height.
pub fn card_height(lines: usize) -> f32 {
    44.0 + lines as f32 * common::VALUE_ROW_HEIGHT
}

/// The card, in MAIN's bottom strip beside FEED and CUT.
pub fn tlo_card(app: &LincApp, ui: &mut Ui, height: f32) {
    let lengths = read(
        &app.snapshot,
        &app.profile.machine_ini.tool_offset_columns(),
        app.profile.variant,
    );
    let rows = lengths.rows(&app.snapshot);
    let badge = format!("T{}", lengths.tool);
    let body = common::value_card(ui, height, "TLO", Some((&badge, theme::MUTED)));
    common::paint_value_rows(ui, body, &rows);
}

#[cfg(test)]
mod tests {
    use super::*;
    use linc_core::model::{ToolGeometry, ToolRow};

    fn tool_row(tool: u16, diameter: f64, length: f64) -> ToolRow {
        let mut row = ToolRow::new(tool);
        row.geometry = ToolGeometry {
            diameter: Some(diameter),
            length: Some(length),
            ..Default::default()
        };
        row.geometry.values.insert("Z".to_string(), length);
        row.geometry.values.insert("X".to_string(), 12.5);
        row
    }

    fn snapshot(rows: Vec<ToolRow>, current: u16) -> MachineSnapshot {
        let mut snapshot = MachineSnapshot::empty(MachineVariant::Mill);
        snapshot.tool_table.rows = rows;
        snapshot.tool_change.current_tool = current;
        snapshot
    }

    fn columns() -> Vec<ToolColumn> {
        vec![ToolColumn::X, ToolColumn::Y, ToolColumn::Z]
    }

    #[test]
    fn the_readout_is_the_loaded_tools_own_offsets() {
        let lengths = read(
            &snapshot(vec![tool_row(1, 6.0, -56.068)], 1),
            &columns(),
            MachineVariant::Mill,
        );
        assert_eq!(lengths.tool, 1);
        assert_eq!(
            lengths.axial,
            vec![
                ("X".to_string(), Some(12.5)),
                // The row carries no Y word, which is a zero offset, not a
                // missing axis.
                ("Y".to_string(), None),
                ("Z".to_string(), Some(-56.068)),
            ]
        );
        // A mill compensates by the cutter's radius, which the table states as
        // a diameter.
        assert_eq!(lengths.radius, Some(3.0));
    }

    #[test]
    fn an_empty_spindle_has_no_offsets_to_read() {
        let lengths = read(&snapshot(vec![], 0), &columns(), MachineVariant::Mill);
        assert_eq!(lengths.tool, 0);
        assert!(lengths.axial.iter().all(|(_, value)| value.is_none()));
        assert_eq!(lengths.radius, None);
        // The rows are still all there, each saying there is no number.
        let rows = lengths.rows(&snapshot(vec![], 0));
        assert_eq!(rows.len(), 4, "three axes and the radius");
        assert!(rows.iter().all(|row| row.value == "—" && row.muted));
    }

    #[test]
    fn a_lathe_compensates_by_its_nose_radius() {
        let mut row = tool_row(2, 0.4, -10.0);
        row.geometry.nose_radius = Some(0.2);
        let lengths = read(
            &snapshot(vec![row], 2),
            &[ToolColumn::X, ToolColumn::Z],
            MachineVariant::Lathe,
        );
        assert_eq!(lengths.radius, Some(0.2), "R wins over D/2 on a lathe");
    }

    #[test]
    fn the_rows_are_sign_printed_and_carry_their_units() {
        let lengths = read(
            &snapshot(vec![tool_row(1, 6.0, -56.068)], 1),
            &columns(),
            MachineVariant::Mill,
        );
        let rows = lengths.rows(&snapshot(vec![tool_row(1, 6.0, -56.068)], 1));
        let by_axis = |axis: &str| {
            rows.iter()
                .find(|row| row.label == axis)
                .unwrap_or_else(|| panic!("{axis} has a row"))
        };
        assert_eq!(by_axis("X").value, "+12.5000");
        assert_eq!(by_axis("Z").value, "-56.0680");
        assert_eq!(by_axis("RAD").value, "+3.0000");
        assert_eq!(by_axis("X").units, "mm");
        // The axis the table omits keeps its row, dimmed, with nothing to say.
        assert_eq!(by_axis("Y").value, "—");
        assert!(by_axis("Y").muted);
    }
}
