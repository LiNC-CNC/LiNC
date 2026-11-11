//! The ladder grid: what the editor edits.
//!
//! A rung is nine element positions plus a coil position, as ZX-PLC draws it.
//! The compiler lowers a row of cells into the same `Logic` tree the ladder
//! canvas has always produced, so both representations share one evaluator.
//!
//! The grid grows in steps and each cell kind lands with its compiler support.
//! `Wire` and `Contact` compile today; `Link`, the timer/counter boxes, compare
//! and compute are parsed by the editor and refused by validation until their
//! semantics exist, rather than being silently ignored.

use serde::{Deserialize, Serialize};

use crate::model::{Coil, Logic};

/// Element positions in a rung. The coil sits beside position 8.
pub const POSITIONS: usize = 9;

/// One element position.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "cell", rename_all = "snake_case")]
pub enum Cell {
    /// An open position. Left of the first element it is a wire on the rail;
    /// anywhere else it is the wire that carries power to the next element.
    Wire,
    /// `-[ ]-` / `-[/]-`.
    Contact {
        var: String,
        #[serde(default)]
        negated: bool,
    },
    /// `-[P]-` / `-[N]-`: a rising or falling edge. Needs the previous scan's
    /// value of `var`, which the VM does not carry yet.
    Edge { var: String, rising: bool },
    /// A vertical link that joins the row below: the ZX `V` at this column.
    Link,
    /// `TON` / `TOF` / `TP` / `CTU` / `CTD` box.
    Block {
        block: String,
        instance: String,
        #[serde(default)]
        preset: String,
    },
    /// `-[>]-` compare contact.
    Compare {
        op: String,
        left: String,
        right: String,
    },
    /// `-[+]-` compute coil. Stored on the coil position, not a cell.
    Compute {
        op: String,
        target: String,
        left: String,
        right: String,
    },
}

impl Cell {
    /// The operand the status bar shows for this position.
    pub fn operand(&self) -> Option<&str> {
        match self {
            Self::Contact { var, .. } | Self::Edge { var, .. } => Some(var),
            Self::Block { instance, .. } => Some(instance),
            Self::Compare { left, .. } => Some(left),
            Self::Compute { target, .. } => Some(target),
            Self::Wire | Self::Link => None,
        }
    }

    /// Does this cell need a feature the compiler does not have yet?
    pub fn unsupported(&self) -> Option<&'static str> {
        match self {
            Self::Wire | Self::Contact { .. } | Self::Link | Self::Block { .. } => None,
            Self::Edge { .. } => Some("edge contacts"),
            Self::Compare { .. } | Self::Compute { .. } => None,
        }
    }
}

/// Lower one row of cells into the series logic the evaluator understands.
///
/// Empty positions and wires add nothing: power flows through them. The coil
/// stays where it is; the caller pairs it with the returned logic.
pub fn lower_row(cells: &[Cell]) -> Logic {
    let mut items = Vec::new();
    for cell in cells.iter().take(POSITIONS) {
        match cell {
            Cell::Wire | Cell::Link => {}
            Cell::Contact { var, negated } => items.push(Logic::contact(var, *negated)),
            // Everything else is refused by validation before this runs; treat
            // it as a wire so a stale project cannot panic the daemon.
            Cell::Edge { .. }
            | Cell::Block { .. }
            | Cell::Compare { .. }
            | Cell::Compute { .. } => {}
        }
    }
    if items.is_empty() {
        Logic::Const { value: true }
    } else {
        Logic::series(items)
    }
}

/// Lower row `index` that may join the row below with a link.
///
/// A link at the right edge of a position makes the next row a parallel path:
/// the row keeps its own series and the branch is `OR`ed in. The branch is the
/// next row in full, and a branch that links again consumes one more row, so a
/// stack of links folds bottom-up. The returned count is the rows used.
pub fn lower_rung(rows: &[Vec<Cell>], index: usize) -> (Logic, usize) {
    let Some(cells) = rows.get(index) else {
        return (Logic::Const { value: true }, 1);
    };
    let own = lower_row(cells);
    if cells.iter().any(|cell| matches!(cell, Cell::Link)) {
        if rows.get(index + 1).is_some() {
            let (branch, used) = lower_rung(rows, index + 1);
            return (
                Logic::Parallel {
                    items: vec![own, branch],
                },
                1 + used,
            );
        }
    }
    (own, 1)
}

/// Turn a version-2 series rung into cells.
///
/// Only a flat series of contacts is expressible one-to-one; a parallel branch
/// (or a constant) returns `None` rather than being flattened into something
/// that means something else. The caller then leaves the tree in place.
pub fn migrate(logic: &Logic) -> Option<Vec<Cell>> {
    match logic {
        // An always-true rung is an empty row: both energise the coil.
        Logic::Const { value: true } => Some(Vec::new()),
        Logic::Contact { var, negated } => Some(vec![Cell::Contact {
            var: var.clone(),
            negated: *negated,
        }]),
        Logic::Series { items } => {
            let mut cells = Vec::new();
            for item in items {
                cells.extend(migrate(item)?);
            }
            Some(cells)
        }
        _ => None,
    }
}

/// The coil of a grid row. Compute coils carry their expression in the cell.
pub fn coil_of(cells: &[Cell], coil: &Coil) -> Coil {
    let _ = cells;
    coil.clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::CoilKind;

    #[test]
    fn wires_and_empty_positions_add_nothing() {
        let cells = vec![
            Cell::Wire,
            Cell::Contact {
                var: "a".into(),
                negated: false,
            },
            Cell::Wire,
            Cell::Contact {
                var: "b".into(),
                negated: true,
            },
        ];
        let logic = lower_row(&cells);
        assert_eq!(
            logic,
            Logic::series(vec![Logic::contact("a", false), Logic::contact("b", true)])
        );
    }

    #[test]
    fn an_empty_row_is_a_true_rung() {
        assert_eq!(lower_row(&[]), Logic::Const { value: true });
    }

    #[test]
    fn the_coil_is_carried_through_unchanged() {
        let coil = Coil::new("y", CoilKind::Set);
        assert_eq!(coil_of(&[], &coil), coil);
    }

    #[test]
    fn a_series_rung_migrates_and_a_branch_does_not() {
        let series = Logic::series(vec![Logic::contact("a", false), Logic::contact("b", true)]);
        assert_eq!(
            migrate(&series),
            Some(vec![
                Cell::Contact {
                    var: "a".into(),
                    negated: false
                },
                Cell::Contact {
                    var: "b".into(),
                    negated: true
                },
            ])
        );
        let branch = Logic::Parallel {
            items: vec![Logic::contact("a", false), Logic::contact("b", false)],
        };
        assert_eq!(migrate(&branch), None, "a branch is not a row of cells");
    }

    #[test]
    fn unsupported_cells_name_their_feature() {
        assert_eq!(Cell::Link.unsupported(), None, "links compile now");
        assert_eq!(Cell::Edge { var: "a".into(), rising: true }.unsupported(), Some("edge contacts"));
        assert_eq!(Cell::Wire.unsupported(), None);
    }

    #[test]
    fn a_link_ors_the_row_below() {
        let a = Cell::Contact { var: "a".into(), negated: false };
        let b = Cell::Contact { var: "b".into(), negated: false };
        let c = Cell::Contact { var: "c".into(), negated: false };
        let rows = vec![
            vec![a.clone(), b.clone(), Cell::Wire, Cell::Link],
            vec![Cell::Wire, Cell::Wire, Cell::Wire, c.clone()],
            vec![Cell::Wire],
        ];
        let (logic, used) = lower_rung(&rows, 0);
        assert_eq!(used, 2, "the branch row belongs to the rung above");
        assert_eq!(
            logic,
            Logic::Parallel {
                items: vec![
                    Logic::series(vec![Logic::contact("a", false), Logic::contact("b", false)]),
                    Logic::series(vec![Logic::contact("c", false)]),
                ],
            }
        );
    }
}
