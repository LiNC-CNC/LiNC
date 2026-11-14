//! The word bank's shape: its regions, their order, and the slots they own.
//!
//! Before M3 the bank's geometry lived in `compile`, the region order lived in
//! `compile::word_points`, and the marker-word count was a `compile` constant
//! that `resolve`, `validate`, the importer and the editor each imported. This
//! module owns all three, and [`crate::profile::Profile::layout`] names the seam:
//! a second dialect declares its own region order and its own marker words.

use crate::model::{IoDirection, IoKind, Project};
use crate::profile::Profile;

/// Marker words the bank begins with: the internal, retained words a program
/// addresses by number. ZX-PLC has `MW0`-`MW15`, and keeping the same sixteen
/// means an existing ZX program's words mean the same thing here.
///
/// The spelling of those words is [`crate::profile::resolve::marker_word`]'s, not
/// this constant's: a count is a width, not a name.
pub const WORD_COUNT: u16 = 16;

/// The word bank's regions, in the fixed order the ABI assigns:
///
/// ```text
/// [  0 .. mw )        marker words     internal, retained
/// [ mw .. mw+iw )     word inputs      s32 HAL pins, written each scan
/// [ .. +qw )          word outputs     s32 HAL pins, read each scan
/// [ .. +fi )          float inputs     float HAL pins, converted to i32
/// [ .. +fo )          float outputs    converted from i32 to float
/// ```
///
/// The order is part of the ABI: inserting a word point renumbers every slot
/// after it, exactly as the bit image does, and the retain layout hash notices.
/// [`WordLayout::new`] derives every base from the four point counts, so the
/// order cannot be stated twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordLayout {
    /// [`WORD_COUNT`]: the retained marker words.
    pub mw: u16,
    pub iw: u16,
    pub qw: u16,
    pub fi: u16,
    pub fo: u16,
}

impl WordLayout {
    pub fn new(iw: u16, qw: u16, fi: u16, fo: u16) -> Self {
        Self {
            mw: WORD_COUNT,
            iw,
            qw,
            fi,
            fo,
        }
    }

    pub fn total(&self) -> u16 {
        self.mw + self.iw + self.qw + self.fi + self.fo
    }

    pub fn iw_base(&self) -> u16 {
        self.mw
    }

    pub fn qw_base(&self) -> u16 {
        self.mw + self.iw
    }

    pub fn fi_base(&self) -> u16 {
        self.mw + self.iw + self.qw
    }

    pub fn fo_base(&self) -> u16 {
        self.mw + self.iw + self.qw + self.fi
    }
}

/// The project's word bank: its region layout and every declared word or float
/// point with the slot it owns, in ABI order.
///
/// Compile builds the symbol table and the HAL bindings from this list, and
/// validation resolves expression operands through it, so a slot and a name can
/// never differ between the two.
pub fn word_points(project: &Project) -> (WordLayout, Vec<(String, u16, IoKind, IoDirection)>) {
    let words = crate::profile::profile().layout(
        count_points(project, IoDirection::In, IoKind::Word),
        count_points(project, IoDirection::Out, IoKind::Word),
        count_points(project, IoDirection::In, IoKind::Float),
        count_points(project, IoDirection::Out, IoKind::Float),
    );
    let mut points = Vec::with_capacity((words.iw + words.qw + words.fi + words.fo) as usize);
    let mut push = |direction: IoDirection, kind: IoKind, base: u16| {
        for (offset, point) in project
            .io
            .iter()
            .filter(|point| point.direction == direction && point.kind == kind)
            .enumerate()
        {
            points.push((point.name.clone(), base + offset as u16, kind, direction));
        }
    };
    // The order is the ABI; see `WordLayout`.
    push(IoDirection::In, IoKind::Word, words.iw_base());
    push(IoDirection::Out, IoKind::Word, words.qw_base());
    push(IoDirection::In, IoKind::Float, words.fi_base());
    push(IoDirection::Out, IoKind::Float, words.fo_base());
    (words, points)
}

/// Number of I/O points of one direction and declared kind.
fn count_points(project: &Project, direction: IoDirection, kind: IoKind) -> u16 {
    project
        .io
        .iter()
        .filter(|point| point.direction == direction && point.kind == kind)
        .count() as u16
}

#[cfg(test)]
mod tests {
    use super::{word_points, WordLayout, WORD_COUNT};
    use crate::model::{IoDirection, IoKind, IoPoint, Project};

    #[test]
    fn the_regions_are_contiguous_and_follow_the_documented_order() {
        let layout = WordLayout::new(2, 3, 4, 5);
        assert_eq!(layout.mw, WORD_COUNT);
        assert_eq!(layout.iw_base(), WORD_COUNT);
        assert_eq!(layout.qw_base(), WORD_COUNT + 2);
        assert_eq!(layout.fi_base(), WORD_COUNT + 5);
        assert_eq!(layout.fo_base(), WORD_COUNT + 9);
        assert_eq!(layout.total(), WORD_COUNT + 14);

        let empty = WordLayout::new(0, 0, 0, 0);
        assert_eq!(
            (
                empty.iw_base(),
                empty.qw_base(),
                empty.fi_base(),
                empty.fo_base()
            ),
            (WORD_COUNT, WORD_COUNT, WORD_COUNT, WORD_COUNT),
            "with no points every region starts at the marker words"
        );
    }

    fn point(name: &str, direction: IoDirection, kind: IoKind) -> IoPoint {
        IoPoint {
            name: name.to_string(),
            direction,
            kind,
            comment: String::new(),
        }
    }

    #[test]
    fn a_word_point_lands_in_its_regions_slot() {
        let mut project = Project::new("bank");
        project.io = vec![
            point("start", IoDirection::In, IoKind::Word),
            point("level", IoDirection::In, IoKind::Word),
            point("speed", IoDirection::Out, IoKind::Float),
        ];
        let (layout, points) = word_points(&project);
        assert_eq!(layout, WordLayout::new(2, 0, 0, 1));
        let slots: Vec<(&str, u16)> = points
            .iter()
            .map(|(name, slot, _, _)| (name.as_str(), *slot))
            .collect();
        assert_eq!(
            slots,
            vec![
                ("start", WORD_COUNT),
                ("level", WORD_COUNT + 1),
                ("speed", WORD_COUNT + 2)
            ],
            "declaration order decides the slot, region order decides the base"
        );
    }
}
