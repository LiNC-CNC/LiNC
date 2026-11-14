//! The word bank's shape: its regions, their order, and the slots they own.
//!
//! Before M3 the bank's geometry lived in `compile`, the region order lived in
//! `compile::word_points`, and the marker-word count was a `compile` constant
//! that `resolve`, `validate`, the importer and the editor each imported. This
//! module owns all three, and [`crate::profile::Profile::layout`] names the seam:
//! a second dialect declares its own region order and its own marker words.

use crate::model::{IoDirection, IoKind, Project, VarType, WordBit};

/// Marker words the bank begins with: the internal, retained words a program
/// addresses by number: `MW0`-`MW15`, sixteen retained words.
///
/// The spelling of those words is [`crate::profile::resolve::marker_word`]'s, not
/// this constant's: a count is a width, not a name.
pub const WORD_COUNT: u16 = 16;

/// Most word-bank slots a project may ask for: the marker words plus every word
/// and float point. The bank is a fixed table in the scan image, so a project
/// past this is refused rather than truncated.
pub const MAX_WORDS: u16 = 256;

/// Most edge contacts a project may declare.
pub const MAX_EDGE_CONTACTS: u16 = 32;

/// One region of the bank: a name, the slot it starts at, and how many slots it
/// owns.
///
/// The name is what `layout_hash` sees and what a diagnostic says; the numbers are
/// what the compiler binds. A region of length zero still exists, at the slot it
/// would start at, so appending to a region never moves another region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub name: &'static str,
    pub base: u16,
    pub len: u16,
}

impl Region {
    pub fn end(&self) -> u16 {
        self.base + self.len
    }
}

/// The width of one word-bank slot's value.
///
/// A slot is 32 bits of the scan image either way; the width says how many of them
/// the declared type owns, which is what a load sign-extends and a store
/// canonicalises.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordWidth {
    W16,
    W32,
}

/// What a declared word variable carries: a width and a signedness.
///
/// `REAL` is `W32` unsigned, and that is deliberate rather than a placeholder: a
/// float value kept as its 32 bits *is* the value, so a load and a store move the
/// same bits and no conversion happens. The arithmetic that would have to know it
/// is a float is a later step's op, not this one's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordType {
    pub width: WordWidth,
    pub signed: bool,
}

impl WordType {
    pub const INT: Self = Self {
        width: WordWidth::W16,
        signed: true,
    };
    pub const DINT: Self = Self {
        width: WordWidth::W32,
        signed: true,
    };
    pub const TIME: Self = Self {
        width: WordWidth::W32,
        signed: true,
    };
    pub const REAL: Self = Self {
        width: WordWidth::W32,
        signed: false,
    };

    /// The declared type as an operator reads it.
    pub fn label(self) -> &'static str {
        match (self.width, self.signed) {
            (WordWidth::W16, true) => "INT",
            (WordWidth::W32, true) => "DINT",
            // A `W32` unsigned slot holds whatever 32 bits were put there: a REAL's
            // bits, or another dialect's word. Naming it REAL here would be a guess.
            (WordWidth::W32, false) => "REAL",
            (WordWidth::W16, false) => "UINT",
        }
    }

    /// The value a load produces from the slot's raw contents: a 16-bit variable
    /// sign-extends, a 32-bit one is already the value.
    pub fn load(self, raw: i32) -> i32 {
        match self.width {
            WordWidth::W16 => i32::from(raw as i16),
            WordWidth::W32 => raw,
        }
    }

    /// The value a store leaves in the slot: a 16-bit variable keeps its low half,
    /// sign-extended, so two reads of it cannot disagree about its value.
    pub fn store(self, value: i32) -> i32 {
        match self.width {
            WordWidth::W16 => i32::from(value as i16),
            WordWidth::W32 => value,
        }
    }
}

/// The word type a declared type carries. `None` for a bit variable (it has no
/// bank slot) and for a structured type (validation refuses it by name).
pub fn word_type(ty: &VarType) -> Option<WordType> {
    match ty {
        VarType::Int => Some(WordType::INT),
        VarType::Dint => Some(WordType::DINT),
        VarType::Real => Some(WordType::REAL),
        VarType::Time => Some(WordType::TIME),
        // An array's elements are the base type's; the slot arithmetic is the
        // caller's, because an array's length decides how many of them there are.
        VarType::Array { base, .. } => word_type(base),
        VarType::Bool | VarType::Derived(_) => None,
    }
}

/// Which typed region a declaration belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TypedRegion {
    Int,
    Dint,
    Real,
    Time,
}

impl TypedRegion {
    pub const COUNT: usize = 4;

    fn index(self) -> usize {
        match self {
            Self::Int => 0,
            Self::Dint => 1,
            Self::Real => 2,
            Self::Time => 3,
        }
    }
}

/// Which typed region a declared type belongs to.
///
/// `None` for a bit variable and for a structured type: neither owns a word slot.
/// An array belongs to its base type's region.
pub fn typed_region(ty: &VarType) -> Option<TypedRegion> {
    match ty {
        VarType::Int => Some(TypedRegion::Int),
        VarType::Dint => Some(TypedRegion::Dint),
        VarType::Real => Some(TypedRegion::Real),
        VarType::Time => Some(TypedRegion::Time),
        VarType::Array { base, .. } => typed_region(base),
        VarType::Bool | VarType::Derived(_) => None,
    }
}

/// How many slots each typed region owns, in slots rather than declarations: a
/// `DINT[6]` asks for six.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Typed {
    pub ints: u16,
    pub dints: u16,
    pub reals: u16,
    pub times: u16,
}

impl Typed {
    fn count_mut(&mut self, region: TypedRegion) -> &mut u16 {
        match region {
            TypedRegion::Int => &mut self.ints,
            TypedRegion::Dint => &mut self.dints,
            TypedRegion::Real => &mut self.reals,
            TypedRegion::Time => &mut self.times,
        }
    }
}

/// The word bank's regions, in the fixed order the ABI assigns:
///
/// ```text
/// [  0 .. mw )        marker words     internal, retained
/// [ mw .. mw+iw )     word inputs      s32 HAL pins, written each scan
/// [ .. +qw )          word outputs     s32 HAL pins, read each scan
/// [ .. +fi )          float inputs     float HAL pins, converted to i32
/// [ .. +fo )          float outputs    converted from i32 to float
/// [ .. +ints )        int              INT declarations, one slot each
/// [ .. +dints )       dint             DINT declarations
/// [ .. +reals )       real             REAL declarations, bits in an s32 slot
/// [ .. +times )       time             TIME declarations, milliseconds
/// ```
///
/// The order is part of the ABI: inserting a word point renumbers every slot
/// after it, exactly as the bit image does, and the retain layout hash notices.
/// [`WordLayout::new`] derives every base from the counts, so the order cannot be
/// stated twice.
///
/// The typed regions are **appended** to the I/O regions, which is the ABI-safe
/// direction: a project that declares types gains slots at the end of the bank and
/// every existing slot keeps its number. A new region inserted between two others
/// would renumber the later ones, so `retain::layout_hash` hashes the region table
/// rather than only its total — a bank that grew in the middle is a different
/// layout even when it is the same size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WordLayout {
    /// [`WORD_COUNT`]: the retained marker words.
    pub mw: u16,
    pub iw: u16,
    pub qw: u16,
    pub fi: u16,
    pub fo: u16,
    pub ints: u16,
    pub dints: u16,
    pub reals: u16,
    pub times: u16,
}

impl WordLayout {
    pub fn new(iw: u16, qw: u16, fi: u16, fo: u16, typed: Typed) -> Self {
        Self {
            mw: WORD_COUNT,
            iw,
            qw,
            fi,
            fo,
            ints: typed.ints,
            dints: typed.dints,
            reals: typed.reals,
            times: typed.times,
        }
    }

    pub fn total(&self) -> u16 {
        self.mw
            .saturating_add(self.iw)
            .saturating_add(self.qw)
            .saturating_add(self.fi)
            .saturating_add(self.fo)
            .saturating_add(self.ints)
            .saturating_add(self.dints)
            .saturating_add(self.reals)
            .saturating_add(self.times)
    }

    pub fn iw_base(&self) -> u16 {
        self.mw
    }

    pub fn qw_base(&self) -> u16 {
        self.mw.saturating_add(self.iw)
    }

    pub fn fi_base(&self) -> u16 {
        self.qw_base().saturating_add(self.qw)
    }

    pub fn fo_base(&self) -> u16 {
        self.fi_base().saturating_add(self.fi)
    }

    pub fn int_base(&self) -> u16 {
        self.fo_base().saturating_add(self.fo)
    }

    pub fn dint_base(&self) -> u16 {
        self.int_base().saturating_add(self.ints)
    }

    pub fn real_base(&self) -> u16 {
        self.dint_base().saturating_add(self.dints)
    }

    pub fn time_base(&self) -> u16 {
        self.real_base().saturating_add(self.reals)
    }

    pub fn typed_base(&self, region: TypedRegion) -> u16 {
        match region {
            TypedRegion::Int => self.int_base(),
            TypedRegion::Dint => self.dint_base(),
            TypedRegion::Real => self.real_base(),
            TypedRegion::Time => self.time_base(),
        }
    }

    /// The regions, in ABI order, each with the slot it starts at and its length.
    pub fn regions(&self) -> [Region; 9] {
        [
            Region {
                name: "marker words",
                base: 0,
                len: self.mw,
            },
            Region {
                name: "word inputs",
                base: self.iw_base(),
                len: self.iw,
            },
            Region {
                name: "word outputs",
                base: self.qw_base(),
                len: self.qw,
            },
            Region {
                name: "float inputs",
                base: self.fi_base(),
                len: self.fi,
            },
            Region {
                name: "float outputs",
                base: self.fo_base(),
                len: self.fo,
            },
            Region {
                name: "int",
                base: self.int_base(),
                len: self.ints,
            },
            Region {
                name: "dint",
                base: self.dint_base(),
                len: self.dints,
            },
            Region {
                name: "real",
                base: self.real_base(),
                len: self.reals,
            },
            Region {
                name: "time",
                base: self.time_base(),
                len: self.times,
            },
        ]
    }
}

/// A declared word variable with the slot it starts at and its type: an array owns
/// `ty.slots()` consecutive slots from `slot`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordVar {
    pub name: String,
    pub slot: u16,
    pub ty: VarType,
    /// The initial values the declaration carried, in element order.
    pub init: Vec<i32>,
}

impl WordVar {
    /// How many bank slots one element owns. An array's elements are consecutive, so
    /// element `n` starts at `slot + n`.
    pub fn stride(&self) -> u16 {
        self.ty.slots() / self.ty.elements()
    }
}

/// Why a bit-of-word operand did not resolve.
///
/// The validator turns this into a diagnostic, naming the declaration and the width;
/// the compiler treats every variant as "not this operand form", because validation
/// has already refused the ones that are wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WordBitError {
    /// The text is not a bit-of-word operand at all.
    NotABit,
    /// The word is not declared: no typed variable and no marker word.
    Unknown,
    /// The array element is outside the declaration.
    Element { elements: u16, declared: String },
    /// The bit is outside the word's width.
    Bit { width: u8, declared: String },
    /// The word's type has no addressable bits.
    NoBits { declared: String },
}

impl WordBitError {
    /// The sentence a diagnostic carries. It names the declaration and, where there
    /// is one, the bound that was exceeded.
    pub fn message(&self, text: &str) -> String {
        match self {
            Self::NotABit => format!("`{text}` is not a bit of a declared word"),
            Self::Unknown => format!("`{text}` names no declared word"),
            Self::Element { elements, declared } => format!(
                "`{text}` indexes {declared}, which has {elements} element{}",
                if *elements == 1 { "" } else { "s" }
            ),
            Self::Bit { width, declared } => format!(
                "`{text}` is bit {} of {declared}, which has {width} bits",
                text.rsplit_once('.').map(|(_, bit)| bit).unwrap_or("?"),
            ),
            Self::NoBits { declared } => {
                format!("`{text}` is a bit of {declared}, and that type has no bits to address")
            }
        }
    }
}

/// Resolve a bit-of-word operand: the bank slot the element lives in, and the bit.
///
/// A declared variable's element and width come from its declaration, so an index
/// past the addressable bits of the type is refused rather than read from the next
/// slot. A marker word is the bank's own word, sixteen bits wide.
pub fn word_bit(text: &str, vars: &[WordVar]) -> Result<(u16, u8), WordBitError> {
    let Some(operand) = WordBit::parse(text) else {
        return Err(WordBitError::NotABit);
    };
    if let Some(slot) = crate::profile::resolve::marker_word_slot(&operand.word) {
        if operand.element.is_some() {
            return Err(WordBitError::Element {
                elements: 1,
                declared: format!("marker word {}", operand.word),
            });
        }
        if operand.bit >= 16 {
            return Err(WordBitError::Bit {
                width: 16,
                declared: format!("marker word {}", operand.word),
            });
        }
        return Ok((slot, operand.bit));
    }
    let lower = operand.word.to_ascii_lowercase();
    let Some(var) = vars
        .iter()
        .find(|var| var.name.to_ascii_lowercase() == lower)
    else {
        return Err(WordBitError::Unknown);
    };
    let Some(width) = var.ty.bit_width() else {
        return Err(WordBitError::NoBits {
            declared: format!("`{}` ({})", var.name, var.ty.label()),
        });
    };
    let elements = var.ty.elements();
    let element = operand.element.unwrap_or(0);
    if element >= u32::from(elements) {
        return Err(WordBitError::Element {
            elements,
            declared: format!("`{}` ({})", var.name, var.ty.label()),
        });
    }
    if operand.bit >= width {
        return Err(WordBitError::Bit {
            width,
            declared: format!("`{}` ({})", var.name, var.ty.label()),
        });
    }
    Ok((var.slot + element as u16, operand.bit))
}

/// The project's word bank: its layout, its declared word and float points, and its
/// typed variables, in ABI order.
///
/// Compile builds the symbol table and the HAL bindings from this, and validation
/// resolves expression operands through it, so a slot and a name can never differ
/// between the two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WordBank {
    pub layout: WordLayout,
    /// Word and float points: name, slot, kind, direction.
    pub io: Vec<(String, u16, IoKind, IoDirection)>,
    /// Typed variables: name, first slot, declared type.
    pub vars: Vec<WordVar>,
}

pub fn word_points(project: &Project) -> WordBank {
    let typed = typed_counts(project);
    let layout = crate::profile::for_dialect(project.profile).layout(
        count_points(project, IoDirection::In, IoKind::Word),
        count_points(project, IoDirection::Out, IoKind::Word),
        count_points(project, IoDirection::In, IoKind::Float),
        count_points(project, IoDirection::Out, IoKind::Float),
        typed,
    );
    let mut io = Vec::with_capacity((layout.iw + layout.qw + layout.fi + layout.fo) as usize);
    let mut push = |direction: IoDirection, kind: IoKind, base: u16| {
        for (offset, point) in project
            .io
            .iter()
            .filter(|point| point.direction == direction && point.kind == kind)
            .enumerate()
        {
            io.push((point.name.clone(), base + offset as u16, kind, direction));
        }
    };
    // The order is the ABI; see `WordLayout`.
    push(IoDirection::In, IoKind::Word, layout.iw_base());
    push(IoDirection::Out, IoKind::Word, layout.qw_base());
    push(IoDirection::In, IoKind::Float, layout.fi_base());
    push(IoDirection::Out, IoKind::Float, layout.fo_base());

    // Typed declarations take the next slot of their own region, in declaration
    // order: an array's elements are consecutive, and one declaration's length
    // moves only the declarations after it in the same region.
    let mut cursor = [
        layout.int_base(),
        layout.dint_base(),
        layout.real_base(),
        layout.time_base(),
    ];
    let mut vars = Vec::new();
    for var in &project.vars {
        let Some(region) = typed_region(&var.ty) else {
            continue;
        };
        let at = region.index();
        let slot = cursor[at];
        cursor[at] = cursor[at].saturating_add(var.ty.slots());
        vars.push(WordVar {
            name: var.name.clone(),
            slot,
            ty: var.ty.clone(),
            init: var.init.clone(),
        });
    }
    WordBank { layout, io, vars }
}

/// Slots each typed region needs, in slots: a `DINT[6]` adds six to `dints`.
fn typed_counts(project: &Project) -> Typed {
    let mut typed = Typed::default();
    for var in &project.vars {
        if let Some(region) = typed_region(&var.ty) {
            let slots = var.ty.slots();
            *typed.count_mut(region) = typed.count_mut(region).saturating_add(slots);
        }
    }
    typed
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
    use super::{
        typed_counts, word_points, word_type, Region, Typed, WordLayout, WordType, WORD_COUNT,
    };
    use crate::model::{IoDirection, IoKind, IoPoint, Project, VarDecl, VarType};

    #[test]
    fn the_regions_are_contiguous_and_follow_the_documented_order() {
        let typed = Typed {
            ints: 1,
            dints: 2,
            reals: 3,
            times: 4,
        };
        let layout = WordLayout::new(2, 3, 4, 5, typed);
        assert_eq!(layout.mw, WORD_COUNT);
        assert_eq!(layout.iw_base(), WORD_COUNT);
        assert_eq!(layout.qw_base(), WORD_COUNT + 2);
        assert_eq!(layout.fi_base(), WORD_COUNT + 5);
        assert_eq!(layout.fo_base(), WORD_COUNT + 9);
        assert_eq!(layout.int_base(), WORD_COUNT + 14);
        assert_eq!(layout.dint_base(), WORD_COUNT + 15);
        assert_eq!(layout.real_base(), WORD_COUNT + 17);
        assert_eq!(layout.time_base(), WORD_COUNT + 20);
        assert_eq!(layout.total(), WORD_COUNT + 24);

        let empty = WordLayout::new(0, 0, 0, 0, Typed::default());
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
        assert_eq!(empty.total(), WORD_COUNT);
    }

    #[test]
    fn the_region_table_covers_every_slot_once() {
        let typed = Typed {
            ints: 1,
            dints: 2,
            reals: 3,
            times: 4,
        };
        let layout = WordLayout::new(2, 3, 4, 5, typed);
        let regions: [Region; 9] = layout.regions();
        let mut at = 0u16;
        for region in regions {
            assert_eq!(
                region.base, at,
                "{} begins where the last ended",
                region.name
            );
            at = region.end();
        }
        assert_eq!(at, layout.total(), "the regions tile the bank exactly");
    }

    #[test]
    fn a_word_type_is_its_width_and_signedness() {
        assert_eq!(word_type(&VarType::Int), Some(WordType::INT));
        assert_eq!(word_type(&VarType::Dint), Some(WordType::DINT));
        assert_eq!(word_type(&VarType::Real), Some(WordType::REAL));
        assert_eq!(word_type(&VarType::Time), Some(WordType::TIME));
        assert_eq!(
            word_type(&VarType::Array {
                base: Box::new(VarType::Dint),
                len: 6
            }),
            Some(WordType::DINT),
            "an array's elements carry the base type"
        );
        assert_eq!(word_type(&VarType::Bool), None, "a bit has no bank slot");
        assert_eq!(word_type(&VarType::Derived("Recipe".into())), None);
    }

    #[test]
    fn an_int_load_sign_extends_and_a_store_canonicalises() {
        assert_eq!(WordType::INT.load(0x0001_FFFF), -1);
        assert_eq!(WordType::INT.store(70_000), 70_000 - 65_536);
        assert_eq!(
            WordType::DINT.load(0x0001_FFFF),
            0x0001_FFFF,
            "32 bits are the value"
        );
        assert_eq!(WordType::DINT.store(0x0001_FFFF), 0x0001_FFFF);
        // REAL moves the same bits: no conversion, so a bit pattern round-trips.
        let bits = 1.5f32.to_bits() as i32;
        assert_eq!(WordType::REAL.load(WordType::REAL.store(bits)), bits);
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
        let bank = word_points(&project);
        assert_eq!(bank.layout, WordLayout::new(2, 0, 0, 1, Typed::default()));
        let slots: Vec<(&str, u16)> = bank
            .io
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

    #[test]
    fn typed_declarations_own_slots_and_do_not_move_the_io_regions() {
        let mut project = Project::new("typed");
        project.io = vec![point("start", IoDirection::In, IoKind::Word)];
        project.vars = vec![
            VarDecl::typed("count", VarType::Int),
            VarDecl::typed("total", VarType::Dint),
            VarDecl::typed("level", VarType::Real),
            VarDecl::typed("dwell", VarType::Time),
            // An array's elements are consecutive, and its own length is what moves
            // the next declaration in the same region.
            VarDecl::typed(
                "steps",
                VarType::Array {
                    base: Box::new(VarType::Dint),
                    len: 3,
                },
            ),
            VarDecl::typed("more", VarType::Dint),
            // A bit variable and a structured one own no slot.
            VarDecl::new("flag"),
            VarDecl::typed("station", VarType::Derived("FillStation".into())),
        ];
        let bank = word_points(&project);
        let layout = bank.layout;
        assert_eq!(
            layout,
            WordLayout::new(
                1,
                0,
                0,
                0,
                Typed {
                    ints: 1,
                    // `total`, then `steps`' three elements, then `more`.
                    dints: 5,
                    reals: 1,
                    times: 1,
                }
            )
        );
        assert_eq!(
            bank.io[0].1, WORD_COUNT,
            "a typed declaration does not move a word input"
        );
        let slots: Vec<(&str, u16, String)> = bank
            .vars
            .iter()
            .map(|var| (var.name.as_str(), var.slot, var.ty.label()))
            .collect();
        assert_eq!(
            slots,
            vec![
                ("count", layout.int_base(), "INT".to_string()),
                ("total", layout.dint_base(), "DINT".to_string()),
                ("level", layout.real_base(), "REAL".to_string()),
                ("dwell", layout.time_base(), "TIME".to_string()),
                ("steps", layout.dint_base() + 1, "DINT[3]".to_string()),
                ("more", layout.dint_base() + 4, "DINT".to_string()),
            ],
            "each region fills in declaration order, and an array owns its length"
        );
    }

    #[test]
    fn a_bit_of_a_word_resolves_against_its_declaration() {
        use super::{word_bit, WordBitError};

        let mut project = Project::new("bits");
        project.vars = vec![
            VarDecl::typed(
                "bits",
                VarType::Array {
                    base: Box::new(VarType::Int),
                    len: 2,
                },
            ),
            VarDecl::typed("total", VarType::Dint),
            VarDecl::typed("level", VarType::Real),
            VarDecl::new("flag"),
        ];
        let bank = word_points(&project);
        let vars = &bank.vars;
        let int = bank.layout.int_base();

        // The element moves the slot and the bit stays inside the word.
        assert_eq!(word_bit("bits[0].0", vars), Ok((int, 0)));
        assert_eq!(word_bit("bits[1].15", vars), Ok((int + 1, 15)));
        assert_eq!(
            word_bit("total.31", vars),
            Ok((bank.layout.dint_base(), 31))
        );
        assert_eq!(
            word_bit("bits.7", vars),
            Ok((int, 7)),
            "no element means the first one"
        );

        // A declaration's own width is the bound, and the sentence says which.
        assert_eq!(
            word_bit("bits[0].16", vars),
            Err(WordBitError::Bit {
                width: 16,
                declared: "`bits` (INT[2])".to_string()
            })
        );
        assert_eq!(
            word_bit("total.32", vars),
            Err(WordBitError::Bit {
                width: 32,
                declared: "`total` (DINT)".to_string()
            })
        );
        assert_eq!(
            word_bit("bits[2].0", vars),
            Err(WordBitError::Element {
                elements: 2,
                declared: "`bits` (INT[2])".to_string()
            })
        );
        // A REAL's 32 bits are a value, and a BOOL has no word at all.
        assert_eq!(
            word_bit("level.0", vars),
            Err(WordBitError::NoBits {
                declared: "`level` (REAL)".to_string()
            })
        );
        assert_eq!(
            word_bit("flag.0", vars),
            Err(WordBitError::Unknown),
            "a bit variable owns no slot, so nothing declares that word"
        );
        assert_eq!(word_bit("nope.0", vars), Err(WordBitError::Unknown));
        assert_eq!(word_bit("bits", vars), Err(WordBitError::NotABit));

        // A marker word is the bank's own word, sixteen bits wide.
        let marker = crate::profile::resolve::marker_word(3);
        assert_eq!(word_bit(&format!("{marker}.15"), vars), Ok((3, 15)));
        assert_eq!(
            word_bit(&format!("{marker}.16"), vars),
            Err(WordBitError::Bit {
                width: 16,
                declared: format!("marker word {marker}")
            })
        );
    }

    #[test]
    fn the_typed_counts_count_slots_not_declarations() {
        let mut project = Project::new("counts");
        project.vars = vec![
            VarDecl::typed(
                "steps",
                VarType::Array {
                    base: Box::new(VarType::Dint),
                    len: 6,
                },
            ),
            VarDecl::typed("total", VarType::Dint),
        ];
        let typed = typed_counts(&project);
        assert_eq!(typed.dints, 7);
        assert_eq!(typed.ints + typed.reals + typed.times, 0);
    }
}
