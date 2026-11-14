//! Validation and bytecode lowering.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::il::{lower_line, parse_line, IlLine, IlOperand};
use crate::model::{CoilKind, IoDirection, IoKind, Logic, PouLanguage, Project};
use crate::profile::counter::CounterRule;
use crate::profile::spec::{CompareName, Family, FbName, IlForm, MathName};
use crate::validate::{validate, Diagnostic, Severity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Input,
    Output,
    Var,
}

/// Which image a symbol's `index` addresses.
///
/// A `Bit` symbol indexes the scan image (`Vm.prev` / `Vm.next`); a `Word`
/// symbol indexes `Vm.words`. Word I/O cannot share the bit namespace, so the
/// bank is explicit rather than inferred from the symbol kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Bank {
    Bit,
    Word,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub index: u16,
    pub kind: SymbolKind,
    pub retain: bool,
    pub bank: Bank,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub name: String,
    pub index: u16,
}

/// The compiled symbol table split by bank. Bit slots and word-bank slots are
/// separate namespaces, so every lookup says which one it wants; a word point
/// can never be read as a bit slot by accident.
struct Lookup {
    /// The dialect this program is compiled for. Compile-time only: the scan
    /// executes the ops it produces, never a profile.
    profile: &'static dyn crate::profile::Profile,
    /// Each counter instance's preset text, from the block that declares it. A
    /// member coil in another rung loads it, and on the first scan the block has
    /// not run yet, so the text has to be found before lowering starts.
    presets: HashMap<u16, (String, bool)>,
    bit: HashMap<String, u16>,
    /// IL operands are typed by hand; accept any spelling of a declared name.
    bit_ci: HashMap<String, u16>,
    word: HashMap<String, u16>,
    /// Bank slot -> the width and signedness the declaration gave it. A slot with
    /// no entry is a raw 32-bit word: a marker word, a HAL point, or a dialect's
    /// own address.
    word_types: HashMap<u16, crate::profile::layout::WordType>,
    /// The typed declarations, so a bit-of-word operand (`Bits[0].0`) resolves its
    /// element and its width against the same declaration the bank laid out.
    word_vars: Vec<crate::profile::layout::WordVar>,
    /// The declared arrays, by lowercased name: an element read (`Durations[Phase]`)
    /// needs the first slot and the element count.
    arrays: HashMap<String, ArraySlot>,
    /// A function block instance's variables: (instance, variable) to the bit slot it
    /// owns. Two instances of one block have two sets, which is the whole point of an
    /// instance.
    fb_slots: HashMap<(String, String), u16>,
    /// Each instance's block-instance base, so the two copies of a body drive two
    /// timers instead of one: site index times the number of instances the body
    /// declares.
    fb_block_base: HashMap<String, u16>,
    /// The function block an instance is of, by instance name: a call on `pump1`
    /// runs `MotorStarter`'s body.
    fb_type: HashMap<String, String>,
}

/// A declared array as an operand sees it: the first element's slot and how many
/// elements there are.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArraySlot {
    pub slot: u16,
    pub elements: u16,
}

impl Lookup {
    fn new(profile: &'static dyn crate::profile::Profile) -> Self {
        Self {
            profile,
            presets: HashMap::new(),
            bit: HashMap::new(),
            bit_ci: HashMap::new(),
            word: HashMap::new(),
            word_types: HashMap::new(),
            word_vars: Vec::new(),
            arrays: HashMap::new(),
            fb_slots: HashMap::new(),
            fb_block_base: HashMap::new(),
            fb_type: HashMap::new(),
        }
    }

    fn insert(&mut self, name: &str, bank: Bank, index: u16) {
        match bank {
            Bank::Bit => {
                self.bit.insert(name.to_string(), index);
                self.bit_ci.insert(name.to_ascii_lowercase(), index);
            }
            Bank::Word => {
                self.word.insert(name.to_string(), index);
            }
        }
    }

    /// The bit-slot index of a declared bit symbol.
    fn bit(&self, name: &str) -> Option<u16> {
        self.bit
            .get(name)
            .or_else(|| self.bit_ci.get(&name.to_ascii_lowercase()))
            .copied()
    }

    /// The word-bank slot of a declared word symbol.
    fn word(&self, name: &str) -> Option<u16> {
        self.word
            .get(name)
            .or_else(|| self.word.get(&name.to_ascii_lowercase()))
            .copied()
    }

    /// The declared word names and slots, for the expression resolver.
    fn words(&self) -> &HashMap<String, u16> {
        &self.word
    }

    /// The type a slot was declared with, if it has one.
    fn slot_type(&self, slot: u16) -> Option<crate::profile::layout::WordType> {
        self.word_types.get(&slot).copied()
    }

    /// One instance's variable: its bit slot, or `None` when the name is not one of
    /// the instance's.
    fn instance_slot(&self, instance: &str, var: &str) -> Option<u16> {
        self.fb_slots
            .get(&(instance.to_ascii_lowercase(), var.to_ascii_lowercase()))
            .copied()
    }

    /// The global block index for the instance this lowering pass is in. Outside a
    /// function block body the declared index is the global one.
    fn block_index(&self, site: Option<&str>, index: u16) -> u16 {
        match site.and_then(|site| self.fb_block_base.get(site)) {
            Some(base) => base.saturating_add(index),
            None => index,
        }
    }

    /// A declared array, by any spelling of its name.
    fn array(&self, name: &str) -> Option<ArraySlot> {
        self.arrays
            .get(name)
            .or_else(|| self.arrays.get(&name.to_ascii_lowercase()))
            .copied()
    }

    /// A bit of a declared word: the slot its element lives in, and the bit. `None`
    /// when the text is not that form or the declaration refuses it — validation
    /// has already reported the refusal, so the compiler only needs the address.
    fn word_bit(&self, text: &str) -> Option<(u16, u8)> {
        crate::profile::layout::word_bit(text, &self.word_vars).ok()
    }
}

/// One lowering pass: the project's lookup, plus the function-block instance whose
/// body is being lowered.
///
/// An instance's variables are its own (`#Start` is instance `pump1`'s `start`), so
/// the body is lowered once per call site with that site in hand. `Deref` keeps every
/// existing operand resolution reading the project's tables.
struct Lowering<'a> {
    lookup: &'a Lookup,
    site: Option<&'a str>,
}

impl std::ops::Deref for Lowering<'_> {
    type Target = Lookup;

    fn deref(&self) -> &Lookup {
        self.lookup
    }
}

/// Standard function blocks, four instances each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FbKind {
    Ton,
    Tof,
    Tp,
    Ctu,
    Ctd,
    /// `R_TRIG` / `RT`: a rising edge on CLK.
    Rt,
    /// `F_TRIG` / `RF`: a falling edge on CLK.
    Rf,
    /// Set-dominant latch (`SR`): S1 or (Q and not R).
    Sr,
    /// Reset-dominant latch (`RS`): not R1 and (S or Q).
    Rs,
}

impl FbKind {
    /// Resolve a block name through the instruction table. The table is the only
    /// place a mnemonic is spelled.
    pub fn parse(name: &str) -> Option<Self> {
        match crate::profile::spec::spec(name)?.family {
            Family::Fb(name) => Some(Self::from_name(name)),
            _ => None,
        }
    }

    fn from_name(name: FbName) -> Self {
        match name {
            FbName::Ton => Self::Ton,
            FbName::Tof => Self::Tof,
            FbName::Tp => Self::Tp,
            FbName::Ctu => Self::Ctu,
            FbName::Ctd => Self::Ctd,
            FbName::Rt => Self::Rt,
            FbName::Rf => Self::Rf,
            FbName::Sr => Self::Sr,
            FbName::Rs => Self::Rs,
        }
    }

    pub fn code(self) -> u16 {
        self.name().code()
    }

    pub(crate) fn name(self) -> FbName {
        match self {
            Self::Ton => FbName::Ton,
            Self::Tof => FbName::Tof,
            Self::Tp => FbName::Tp,
            Self::Ctu => FbName::Ctu,
            Self::Ctd => FbName::Ctd,
            Self::Rt => FbName::Rt,
            Self::Rf => FbName::Rf,
            Self::Sr => FbName::Sr,
            Self::Rs => FbName::Rs,
        }
    }

    pub fn is_timer(self) -> bool {
        matches!(self, Self::Ton | Self::Tof | Self::Tp)
    }

    /// Counters step a value; the rest are bit or time blocks.
    pub fn is_counter(self) -> bool {
        matches!(self, Self::Ctu | Self::Ctd)
    }

    /// Latches take two inputs; the rest take one.
    pub fn is_latch(self) -> bool {
        matches!(self, Self::Sr | Self::Rs)
    }
}

/// Arithmetic of a compute coil.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathOp {
    Add,
    Sub,
    Mul,
    Div,
    Move,
}

impl MathOp {
    /// Resolve a block arithmetic name through the instruction table. The
    /// operator spellings a compute cell accepts (`+`, `:=`, …) are rows there
    /// too, so this has no name list of its own.
    pub fn parse(name: &str) -> Option<Self> {
        match crate::profile::spec::spec(name)?.family {
            Family::Math(name) => Some(Self::from_name(name)),
            _ => None,
        }
    }

    fn from_name(name: MathName) -> Self {
        match name {
            MathName::Add => Self::Add,
            MathName::Sub => Self::Sub,
            MathName::Mul => Self::Mul,
            MathName::Div => Self::Div,
            MathName::Move => Self::Move,
        }
    }
}

/// Edge contacts a project may declare.
pub const MAX_EDGE_CONTACTS: u16 = 32;

/// Instances per function-block kind. Dialect data now; the name stays here
/// because every caller already imports it.
pub use crate::profile::FB_PER_TYPE;

/// Presets parse in the profile. The name stays here because the tests that pin
/// which presets are refused, and the importer, already call it.
pub use crate::profile::resolve::parse_preset;

/// Where a function block's preset comes from.
///
/// `Word` holds a bank slot, so the preset is read on every scan and a recipe
/// word drives the timer or counter. `Const` is the literal a drawn block box or
/// `PT:=T#4s` bakes into the program. The block's state still holds one preset;
/// only where it came from changes, so `FbPreset` and `WordOperand::BlockPreset`
/// read the field they always did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PresetSource {
    Const(i32),
    Word(u16),
    /// One element of a declared array, read each scan: `Durations[Phase]` sets a
    /// timer's `PT`, so the element moves with `Phase`.
    ///
    /// `index` is the slot the element number is read from, and `len` is the
    /// declaration's element count. The scan clamps the index into `0..len`: a scan
    /// that reads the wrong element is a program fault, and a deterministic value is
    /// worth more than an stopped PLC (see `Op::Pow` for the same reasoning). A
    /// literal index is folded at compile time, and an out-of-range literal is
    /// refused by validation, so only a variable index reaches the clamp.
    Indexed {
        base: u16,
        index: u16,
        len: u16,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    PushConst(bool),
    Push(u16),
    /// Read the value being built this scan rather than the previous image.
    /// Function-block lines chain inside one scan, so their inputs must see a
    /// value an earlier line stored.
    PushNext(u16),
    Not,
    And,
    Or,
    Xor,
    Store(u16),
    Set(u16),
    Reset(u16),
    /// Push a marker word on the word stack.
    LoadWord(u16),
    /// Push a literal on the word stack.
    LoadConst(i32),
    /// Pop a word and write it to a marker word.
    StoreWord(u16),
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    /// Pop two words, push the smaller of the two.
    Min,
    /// Pop two words, push the larger of the two.
    Max,
    /// Pop a word, push its magnitude. `i32::MIN` stays `i32::MIN`, like the
    /// rest of the word arithmetic, which wraps.
    Abs,
    /// Pop the exponent, then the base, and push the base raised to it. A
    /// negative exponent yields 0; the result wraps at 32 bits.
    Pow,
    /// Pop two words and push the boolean result on the bit stack.
    CmpGt,
    CmpGe,
    CmpEq,
    CmpNe,
    CmpLe,
    CmpLt,
    /// Run a standard function block: pop its input, push `Q`.
    Fb {
        kind: FbKind,
        instance: u16,
        preset: PresetSource,
    },
    /// Run a counter: pop its input, re-assert the preset, step the value the way
    /// the profile baked (`rule`), push `Q`. A counter whose steps come from
    /// member coils re-asserts the preset and computes `Q` here, and only
    /// `Op::Count` moves the value.
    Counter {
        instance: u16,
        preset: PresetSource,
        rule: CounterRule,
    },
    /// Step a member-coil counter: pop the power bit and, on its rising edge, add
    /// or subtract one inside `top`'s range. `None` saturates.
    Count {
        instance: u16,
        up: bool,
        top: Option<i32>,
    },
    /// Push the block's `ET` (timers) or `CV` (counters) on the word stack.
    FbWord(u16),
    /// Push a bank slot's value as the declared type reads it: a 16-bit variable
    /// sign-extends, a 32-bit one is already the value.
    LoadTyped {
        slot: u16,
        ty: crate::profile::layout::WordType,
    },
    /// Pop a value and write it as the declared type writes it: a 16-bit variable
    /// keeps its low half, sign-extended, so two reads of it cannot disagree.
    StoreTyped {
        slot: u16,
        ty: crate::profile::layout::WordType,
    },
    /// Push one bit of a bank word on the bit stack: `Bits[0].0`, or a marker
    /// word's bit.
    WordBit {
        slot: u16,
        bit: u8,
    },
    /// Pop a bit and write it into the word, leaving the other bits as they were.
    StoreWordBit {
        slot: u16,
        bit: u8,
    },
    /// Pop power and set the bit while it is true (`OTL` on a word bit).
    SetWordBit {
        slot: u16,
        bit: u8,
    },
    /// Pop power and clear the bit while it is true (`OTU` on a word bit).
    ResetWordBit {
        slot: u16,
        bit: u8,
    },
    /// Push the block's `Q` on the bit stack.
    FbQ(u16),
    /// Push the block's timing bit: enabled this scan and not yet done.
    FbTiming(u16),
    /// A two-input latch: pop the reset bit, then the set bit.
    Latch {
        kind: FbKind,
        instance: u16,
    },
    /// Pop the power bit; when it is true, clear `CV` (`CTU0.R`).
    FbReset(u16),
    /// Pop the power bit; when it is true, load the counter's preset into `CV`
    /// (`CTD0.LD`, ClassicLadder's `CTU0.L`). The preset is baked, so a load on
    /// the first scan sees the value the block would assert.
    FbLoad {
        instance: u16,
        preset: PresetSource,
    },
    /// Push the block's preset (`PT` / `PV`) on the word stack.
    FbPreset(u16),
    /// Edge contact: push true on the scan the operand changed, then remember
    /// the value in slot `slot` for the next scan.
    Edge {
        slot: u16,
        var: u16,
        rising: bool,
    },
    /// A compute coil: pop the power bit and, when it is true, write the result
    /// to `target`, which is a bank slot or a block member. It ends the rung;
    /// there is no coil output.
    Compute {
        op: MathOp,
        target: WordOperand,
        /// The width the target is written at, when the target is a declared
        /// variable. A marker word or a HAL point has none and is written raw.
        target_ty: Option<crate::profile::layout::WordType>,
        left: WordOperand,
        right: WordOperand,
    },
    /// Pop one bit and discard it: a rung with no coil leaves its power bit
    /// behind, and the stack has to stay balanced across rungs.
    Drop,
    /// Pop a word and write it to `target`. This is the free-text assignment's
    /// store: the value is already on the word stack.
    StoreTarget(WordOperand),
    /// Continue at `target` in the instruction list.
    Jump(u16),
    /// Pop the result bit and jump to `target` when it equals `when`:
    /// `JMPC` is `when: true`, `JMPCN` is `when: false`.
    JumpIf {
        target: u16,
        when: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Program {
    pub ops: Vec<Op>,
    /// Bit slots in the scan image: inputs, then outputs, then variables.
    pub var_count: u16,
    /// Word slots in the integer bank: `words.total()`.
    pub word_count: u16,
    /// The bank's regions. The ABI fixes the slot order, not just the count.
    pub words: WordLayout,
    /// Function-block instances: `FB_PER_TYPE` per kind, index `kind*4 + n`.
    pub fb_count: u16,
    /// Previous-value slots for edge contacts.
    pub edge_count: u16,
    /// The values a declaration says its words start with: slot and value. A scan
    /// image starts zeroed, and these are applied once, at construction and at
    /// reset.
    pub init: Vec<(u16, i32)>,
    pub input_count: u16,
    pub output_count: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    pub program: Program,
    pub symbols: Vec<Symbol>,
    /// Bit bindings, in scan-image order.
    pub inputs: Vec<Binding>,
    pub outputs: Vec<Binding>,
    /// Word- and float-point bindings, in word-bank order. `index` is the slot
    /// the value lives in, so the daemon can read and write one contiguous
    /// slice per region.
    pub word_inputs: Vec<Binding>,
    pub word_outputs: Vec<Binding>,
    pub float_inputs: Vec<Binding>,
    pub float_outputs: Vec<Binding>,
    pub hash: String,
    pub warnings: Vec<Diagnostic>,
}

pub fn compile(project: &Project) -> Result<Compiled, Vec<Diagnostic>> {
    let diagnostics = validate(project);
    if diagnostics
        .iter()
        .any(|item| item.severity == Severity::Error)
    {
        return Err(diagnostics);
    }

    let capacity = project.io.len() + project.vars.len();
    let mut symbols = Vec::with_capacity(capacity);
    let mut lookup = Lookup::new(crate::profile::for_dialect(project.profile));
    collect_presets(project, &mut lookup);
    let mut inputs = Vec::new();
    let mut outputs = Vec::new();
    let mut word_inputs = Vec::new();
    let mut word_outputs = Vec::new();
    let mut float_inputs = Vec::new();
    let mut float_outputs = Vec::new();

    // Bit points first, so every bit symbol's index is its position in the
    // image and `var_count` stays the size of `prev`/`next`.
    for point in project
        .io
        .iter()
        .filter(|point| point.direction == IoDirection::In && point.kind == IoKind::Bit)
    {
        let index = intern(
            &mut symbols,
            &mut lookup,
            &point.name,
            SymbolKind::Input,
            false,
            Bank::Bit,
            None,
        );
        inputs.push(Binding {
            name: point.name.clone(),
            index,
        });
    }
    for point in project
        .io
        .iter()
        .filter(|point| point.direction == IoDirection::Out && point.kind == IoKind::Bit)
    {
        let index = intern(
            &mut symbols,
            &mut lookup,
            &point.name,
            SymbolKind::Output,
            false,
            Bank::Bit,
            None,
        );
        outputs.push(Binding {
            name: point.name.clone(),
            index,
        });
    }
    for var in &project.vars {
        // Only a bit variable owns a bit: a word variable lives in the bank, and an
        // instance is the set of its block's variables, not a bit of its own.
        if !var.ty.is_bool() {
            continue;
        }
        intern(
            &mut symbols,
            &mut lookup,
            &var.name,
            SymbolKind::Var,
            var.retain,
            Bank::Bit,
            None,
        );
    }

    // Word points second: their index is their slot in the bank. `word_points`
    // owns the slot arithmetic, so validation and the expression resolver name
    // the same slot the compiler binds.
    let bank = word_points(project);
    let words = bank.layout;
    for (name, slot, kind, direction) in &bank.io {
        let symbol_kind = match direction {
            IoDirection::In => SymbolKind::Input,
            IoDirection::Out => SymbolKind::Output,
        };
        intern(
            &mut symbols,
            &mut lookup,
            name,
            symbol_kind,
            false,
            Bank::Word,
            Some(*slot),
        );
        let binding = Binding {
            name: name.clone(),
            index: *slot,
        };
        match (direction, kind) {
            (IoDirection::In, IoKind::Word) => word_inputs.push(binding),
            (IoDirection::Out, IoKind::Word) => word_outputs.push(binding),
            (IoDirection::In, IoKind::Float) => float_inputs.push(binding),
            (IoDirection::Out, IoKind::Float) => float_outputs.push(binding),
            _ => {}
        }
    }
    // Typed declarations: their name addresses the first slot, and every element of
    // an array carries the same type, so an indexed element reads the same width as
    // the name does.
    // Function-block instances: a project variable whose type names a block owns one
    // bit per BOOL variable of that block, and the settings it needs to be lowered.
    let declared_pous = project.all_pous();
    let fb_blocks: Vec<&crate::model::Pou> = declared_pous
        .iter()
        .filter(|pou| pou.kind == crate::model::PouKind::FunctionBlock)
        .collect();
    let mut site_index = 0u16;
    for var in &project.vars {
        let crate::model::VarType::Derived(block) = &var.ty else {
            continue;
        };
        let Some(block_pou) = fb_blocks
            .iter()
            .find(|pou| pou.name.eq_ignore_ascii_case(block))
        else {
            continue;
        };
        let site = var.name.to_ascii_lowercase();
        let mut span = 0u16;
        for field in &block_pou.vars {
            let index = intern(
                &mut symbols,
                &mut lookup,
                &format!("{site}.{}", field.name),
                SymbolKind::Var,
                false,
                Bank::Bit,
                None,
            );
            lookup
                .fb_slots
                .insert((site.clone(), field.name.to_ascii_lowercase()), index);
        }
        // How many block instances the body declares, of any kind: the next site's
        // copies start after them.
        for section in &block_pou.sections {
            for rung in &section.rungs {
                span = span.max(block_span_in(&rung.logic, 0));
            }
        }
        lookup
            .fb_block_base
            .insert(site.clone(), site_index.saturating_mul(span.max(1)));
        lookup
            .fb_type
            .insert(site.clone(), block_pou.name.to_ascii_uppercase());
        site_index = site_index.saturating_add(1);
    }

    lookup.word_vars = bank.vars.clone();
    for var in &bank.vars {
        // The name addresses element 0, and the declaration says how many follow.
        lookup.arrays.insert(
            var.name.to_ascii_lowercase(),
            ArraySlot {
                slot: var.slot,
                elements: var.ty.elements(),
            },
        );
    }
    for var in &bank.vars {
        intern(
            &mut symbols,
            &mut lookup,
            &var.name,
            SymbolKind::Var,
            false,
            Bank::Word,
            Some(var.slot),
        );
        if let Some(ty) = crate::profile::layout::word_type(&var.ty) {
            for offset in 0..var.ty.slots() {
                lookup.word_types.insert(var.slot + offset, ty);
            }
        }
    }

    let mut ops = Vec::new();
    let mut edges = 0u16;
    let pous = project.all_pous();
    // A POU that another POU calls is a routine: it runs where it is called,
    // not on its own. With no calls every POU is a program, which is how the
    // compiler behaved before `CAL` existed.
    let mut called: std::collections::HashSet<String> = std::collections::HashSet::new();
    for pou in &pous {
        for line in &pou.il {
            if let IlLine::Instruction { mnemonic, operand } = parse_line(line) {
                if matches!(
                    crate::profile::spec::il_form(&mnemonic),
                    Some(IlForm::Call(None))
                ) {
                    called.insert(operand.trim().to_ascii_uppercase());
                }
            }
        }
        // A call coil names its POU the same way a `CAL` line does, so the
        // callee is a routine here too and does not also run on its own.
        for section in &pou.sections {
            for rung in &section.rungs {
                if rung.coil().kind == CoilKind::Call {
                    called.insert(rung.coil().var.trim().to_ascii_uppercase());
                }
            }
        }
    }
    let by_name: HashMap<String, &crate::model::Pou> = pous
        .iter()
        .map(|pou| (pou.name.to_ascii_uppercase(), pou))
        .collect();
    // MAIN is the entry point: the scan runs MAIN and
    // MAIN calls the rest. A project without MAIN (older files, tests) keeps
    // the previous rule so nothing stops running silently.
    let main = pous
        .iter()
        .find(|pou| pou.name.eq_ignore_ascii_case("MAIN"));
    match main {
        Some(main) => lower_pou(
            main,
            &by_name,
            &Lowering {
                lookup: &lookup,
                site: None,
            },
            &mut ops,
            &mut edges,
            0,
        ),
        None => {
            for pou in &pous {
                if called.contains(&pou.name.to_ascii_uppercase()) {
                    continue;
                }
                lower_pou(
                    pou,
                    &by_name,
                    &Lowering {
                        lookup: &lookup,
                        site: None,
                    },
                    &mut ops,
                    &mut edges,
                    0,
                );
            }
        }
    }

    let bit_count = symbols
        .iter()
        .filter(|symbol| symbol.bank == Bank::Bit)
        .count() as u16;
    let program = Program {
        word_count: words.total(),
        words,
        fb_count: FB_PER_TYPE * 9,
        edge_count: edges,
        init: initial_words(&bank.vars),
        ops,
        var_count: bit_count,
        input_count: inputs.len() as u16,
        output_count: outputs.len() as u16,
    };
    let warnings = diagnostics
        .into_iter()
        .filter(|item| item.severity == Severity::Warning)
        .collect();

    Ok(Compiled {
        program,
        symbols,
        inputs,
        outputs,
        word_inputs,
        word_outputs,
        float_inputs,
        float_outputs,
        hash: project_hash(project),
        warnings,
    })
}

/// The highest block-instance number a tree declares, plus one: how far a site's
/// copies have to be shifted to not collide with the next one's.
fn block_span_in(logic: &Logic, seen: u16) -> u16 {
    match logic {
        Logic::Block { instance, .. } => {
            let declared = crate::profile::resolve::parse_block(instance, instance, "")
                .map(|(_, index, _)| index)
                .unwrap_or(0);
            seen.max(declared.saturating_add(1))
        }
        Logic::Series { items } | Logic::Parallel { items } => items
            .iter()
            .fold(seen, |seen, item| block_span_in(item, seen)),
        _ => seen,
    }
}

/// The initial word values a project declares, by bank slot: element `n` of a
/// declaration lands in `slot + n`.
fn initial_words(vars: &[crate::profile::layout::WordVar]) -> Vec<(u16, i32)> {
    let mut init = Vec::new();
    for var in vars {
        for (offset, value) in var.init.iter().enumerate() {
            let slot = var.slot.saturating_add(offset as u16);
            if offset < usize::from(var.ty.slots()) {
                init.push((slot, *value));
            }
        }
    }
    init
}

/// The project's word bank and every declared word or float point, in ABI order.
/// The profile owns the geometry and the region order; this name stays here
/// because the compiler and the validator already import it.
pub use crate::profile::layout::{word_points, WordLayout};

/// Stable FNV-1a 64-bit digest of the serialised project, as 16 lowercase hex digits.
pub fn project_hash(project: &Project) -> String {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    // The watch list is a view, not the program: pinning an operand must not
    // make a downloaded program look stale.
    let mut canonical = project.clone();
    canonical.watch.clear();
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(PRIME);
    }
    format!("{hash:016x}")
}

fn intern(
    symbols: &mut Vec<Symbol>,
    lookup: &mut Lookup,
    name: &str,
    kind: SymbolKind,
    retain: bool,
    bank: Bank,
    index: Option<u16>,
) -> u16 {
    // A bit symbol's index is its position in the image; a word symbol's is the
    // bank slot the layout assigned it.
    let index = index.unwrap_or(symbols.len() as u16);
    symbols.push(Symbol {
        name: name.to_string(),
        index,
        kind,
        retain,
        bank,
    });
    lookup.insert(name, bank, index);
    index
}

/// Point a jump at its target; `at` was reserved when the line was lowered.
fn patch_jump(ops: &mut [Op], at: usize, target: u16) {
    match &mut ops[at] {
        Op::Jump(slot) => *slot = target,
        Op::JumpIf { target: slot, .. } => *slot = target,
        _ => {}
    }
}

/// Lower one POU. `CAL` reaches another POU by lowering its body here, which is
/// what a call means in a scan-based PLC: no locals, no stack frame, the
/// callee's coils and timers are the same ones it owns when it runs alone.
fn lower_pou(
    pou: &crate::model::Pou,
    pous: &HashMap<String, &crate::model::Pou>,
    lookup: &Lowering<'_>,
    ops: &mut Vec<Op>,
    edges: &mut u16,
    depth: u8,
) {
    if depth > 8 {
        return;
    }
    match pou.language {
        PouLanguage::Ld => {
            for section in &pou.sections {
                // Labels are local to a section, as jump targets are. The patch
                // waits until the whole section is lowered, so a forward jump
                // and a backward one take the same path.
                let mut labels: HashMap<String, usize> = HashMap::new();
                let mut jumps: Vec<(usize, String)> = Vec::new();
                let mut index = 0;
                while index < section.rungs.len() {
                    if section.rungs[index].cells.is_empty() {
                        let rung = &section.rungs[index];
                        record_label(&mut labels, &rung.label, ops.len());
                        lower_logic(&rung.logic, lookup, edges, ops);
                        // A rung with no coil is a block whose result nothing
                        // stores: the logic is the whole rung.
                        if rung.has_coil() {
                            lower_coil(ops, lookup, pous, rung.coil(), &mut jumps, edges, depth);
                        } else {
                            ops.push(Op::Drop);
                        }
                        index += 1;
                        continue;
                    }
                    // A run of grid rows. A link in one row makes the next
                    // row its parallel branch, so the run is lowered as a
                    // stack rather than one rung at a time.
                    let mut run: Vec<Vec<crate::grid::Cell>> = Vec::new();
                    while let Some(row) = section.rungs.get(index + run.len()) {
                        if row.cells.is_empty() {
                            break;
                        }
                        // A row linked under another is part of that row's
                        // group, so a label there names the group's start.
                        record_label(&mut labels, &row.label, ops.len());
                        run.push(row.cells.clone());
                    }
                    let mut offset = 0;
                    while offset < run.len() {
                        let (used, compute) = lower_grid_rung(&run, offset, lookup, ops, edges);
                        if !compute {
                            let row = &section.rungs[index + offset];
                            if row.has_coil() {
                                lower_coil(ops, lookup, pous, row.coil(), &mut jumps, edges, depth);
                            } else {
                                // Nothing stores the row's power bit.
                                ops.push(Op::Drop);
                            }
                        }
                        offset += used;
                    }
                    index += run.len();
                }
                for (at, label) in jumps {
                    if let Some(target) = labels.get(&label) {
                        patch_jump(ops, at, *target as u16);
                    }
                }
            }
        }
        PouLanguage::Il => {
            // Labels are local to the POU. Calls are inlined, so a RET inside a
            // callee jumps to the end of that callee's inlined block, and a
            // CALC wraps the whole block in one conditional skip.
            let mut labels: HashMap<String, usize> = HashMap::new();
            let mut label_jumps: Vec<(usize, String)> = Vec::new();
            let mut returns: Vec<usize> = Vec::new();
            for line in &pou.il {
                let IlLine::Instruction { mnemonic, operand } = parse_line(line) else {
                    if let IlLine::Label(name) = parse_line(line) {
                        labels.insert(name.to_ascii_uppercase(), ops.len());
                    }
                    continue;
                };
                // The instruction table says what a mnemonic means; this
                // loop only arranges the control flow around that meaning.
                let spec = crate::profile::spec::spec(&mnemonic);
                match spec.map(|spec| spec.il) {
                    Some(IlForm::Return(when)) => {
                        returns.push(ops.len());
                        match when {
                            Some(when) => ops.push(Op::JumpIf { target: 0, when }),
                            None => ops.push(Op::Jump(0)),
                        }
                    }
                    Some(IlForm::Jump(when)) => {
                        label_jumps.push((ops.len(), operand.trim().to_ascii_uppercase()));
                        match when {
                            Some(when) => ops.push(Op::JumpIf { target: 0, when }),
                            None => ops.push(Op::Jump(0)),
                        }
                    }
                    Some(IlForm::Call(Some(when))) => {
                        // Execute the callee when the accumulator says so; `when`
                        // is the condition that skips its inlined block.
                        if let Some(callee) = pous.get(&operand.trim().to_ascii_uppercase()) {
                            let skip = ops.len();
                            ops.push(Op::JumpIf { target: 0, when });
                            lower_pou(callee, pous, lookup, ops, edges, depth + 1);
                            let after = ops.len() as u16;
                            if let Op::JumpIf { target, .. } = &mut ops[skip] {
                                *target = after;
                            }
                        }
                    }
                    Some(IlForm::Call(None)) => {
                        // `CAL TON0(IN:=x, PT:=T#1s)` runs a block; a bare name
                        // is a POU call.
                        if let Some((name, args)) = crate::il::parse_call(&operand) {
                            if let Some((kind, instance, default)) = parse_block(&name, &name, "") {
                                let input = args
                                    .iter()
                                    .find(|(key, _)| matches!(key.as_str(), "IN" | "CU" | "CD"))
                                    .map(|(_, value)| value.as_str())
                                    .unwrap_or("FALSE");
                                // A word source is live; a literal bakes in.
                                // Validation refused anything else, so a failed
                                // resolve keeps the block's default preset.
                                let preset = args
                                    .iter()
                                    .find(|(key, _)| matches!(key.as_str(), "PT" | "PV"))
                                    .and_then(|(_, value)| {
                                        resolve_preset(value, kind.is_timer(), lookup)
                                    })
                                    .unwrap_or(PresetSource::Const(default));
                                push_bit_operand(input, lookup, ops);
                                push_block(ops, lookup, kind, instance, preset);
                            }
                        } else if let Some(callee) = pous.get(&operand.trim().to_ascii_uppercase())
                        {
                            lower_pou(callee, pous, lookup, ops, edges, depth + 1);
                        }
                    }
                    _ => {
                        if let Some(spec) = spec {
                            lower_line(spec, il_operand(&operand, lookup), ops);
                        }
                    }
                }
            }
            let end = ops.len() as u16;
            for (at, label) in label_jumps {
                if let Some(target) = labels.get(&label) {
                    patch_jump(ops, at, *target as u16);
                }
            }
            for at in returns {
                patch_jump(ops, at, end);
            }
        }
        PouLanguage::Fbd => match pou.fbd_grid.as_ref().filter(|grid| !grid.blocks.is_empty()) {
            Some(grid) => lower_fbd_grid(grid, lookup, ops),
            None => {
                for index in 0..pou.fbd.len() {
                    lower_fbd_line(&pou.fbd, index, lookup, ops);
                }
            }
        },
    }
}

/// Push a bit operand: `TRUE`, a declared symbol, or false when unknown.
fn push_bit_operand(operand: &str, lookup: &Lookup, ops: &mut Vec<Op>) {
    if operand.eq_ignore_ascii_case("TRUE") {
        ops.push(Op::PushConst(true));
    } else if let Some(index) = lookup.bit(operand.trim()) {
        ops.push(Op::Push(index));
    } else {
        ops.push(Op::PushConst(false));
    }
}

/// Lower one function-block assignment: build the block from its inputs, then
/// store its result in the output operand.
fn lower_fbd_line(
    lines: &[crate::model::FbdLine],
    index: usize,
    lookup: &Lowering<'_>,
    ops: &mut Vec<Op>,
) {
    let Some(line) = lines.get(index) else {
        return;
    };
    lower_fbd_block(lines, index, lookup, ops, 0);
    let (negated, name) = match line.output.trim().strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, line.output.trim()),
    };
    if negated {
        ops.push(Op::Not);
    }
    if let Some(target) = lookup.bit(name) {
        ops.push(Op::Store(target));
    }
}

/// Lower a drawn grid. Blocks are evaluated in reading order (row, then
/// column), which is the order the editor enforces: a block may only take its
/// input from something to its left or above.
fn lower_fbd_grid(grid: &crate::model::FbdGrid, lookup: &Lowering<'_>, ops: &mut Vec<Op>) {
    let mut order = grid
        .blocks
        .iter()
        .enumerate()
        .collect::<Vec<(usize, &crate::model::FbdBlock)>>();
    order.sort_by_key(|(_, block)| (block.row, block.col));
    for (index, block) in order {
        lower_fbd_grid_block(grid, index, lookup, ops);
        // The output cell to the right of the anchor stores the result.
        if let crate::model::FbdCell::Out { operand } = grid.cell(block.row, block.col + 1) {
            let (negated, name) = split_negation(&operand);
            if negated {
                ops.push(Op::Not);
            }
            let name = name.trim();
            // A word target is written by the compute block itself.
            if crate::il::word_operand(name).is_none() {
                if let Some(slot) = lookup.bit(name) {
                    ops.push(Op::Store(slot));
                }
            }
        }
    }
}

/// `!start` -> (true, "start").
fn split_negation(operand: &str) -> (bool, &str) {
    match operand.trim().strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, operand.trim()),
    }
}

/// Push one grid block's result on the stack.
fn lower_fbd_grid_block(
    grid: &crate::model::FbdGrid,
    index: usize,
    lookup: &Lowering<'_>,
    ops: &mut Vec<Op>,
) {
    let Some(block) = grid.blocks.get(index) else {
        ops.push(Op::PushConst(false));
        return;
    };
    let name = block.kind.trim().to_ascii_uppercase();
    let mut inputs = Vec::new();
    for offset in 0..block.ins.max(1) {
        let row = block.row + offset;
        let mut operand = grid
            .input_at(row, block.col.saturating_sub(1))
            .unwrap_or_default();
        if operand.trim().is_empty() {
            // Chaining: the value on the wire is the block to the left whose
            // output leaves on this row.
            operand = chained_source(grid, index, row);
        }
        inputs.push(operand);
    }
    // The gate names are instruction-table rows too (`AND`, `OR`, `XOR`, `NOT`),
    // so the FBD path has no name list of its own.
    let form = crate::profile::spec::il_form(&name);
    match form {
        Some(IlForm::And) | Some(IlForm::Or) | Some(IlForm::Xor) => {
            if inputs.is_empty() {
                ops.push(Op::PushConst(matches!(form, Some(IlForm::And))));
            } else {
                for (position, input) in inputs.iter().enumerate() {
                    lower_fbd_grid_bit(input, grid, index, lookup, ops, 0);
                    if position > 0 {
                        ops.push(match form {
                            Some(IlForm::Or) => Op::Or,
                            Some(IlForm::And) => Op::And,
                            _ => Op::Xor,
                        });
                    }
                }
            }
        }
        Some(IlForm::Not) => {
            lower_fbd_grid_bit(&inputs[0], grid, index, lookup, ops, 0);
            ops.push(Op::Not);
        }
        _ => {
            if let Some((kind, instance, preset)) = block_of(&name, &name, &block.preset, lookup) {
                if kind.is_latch() {
                    lower_fbd_grid_bit(
                        inputs.first().map(String::as_str).unwrap_or("FALSE"),
                        grid,
                        index,
                        lookup,
                        ops,
                        0,
                    );
                    lower_fbd_grid_bit(
                        inputs.get(1).map(String::as_str).unwrap_or("FALSE"),
                        grid,
                        index,
                        lookup,
                        ops,
                        0,
                    );
                    ops.push(Op::Latch { kind, instance });
                    return;
                }
                lower_fbd_grid_bit(
                    inputs.first().map(String::as_str).unwrap_or("FALSE"),
                    grid,
                    index,
                    lookup,
                    ops,
                    0,
                );
                push_block(ops, lookup, kind, instance, preset);
            } else if let Some(compare) = compare_op(&name) {
                let (Some(left), Some(right)) = (
                    resolve_word(inputs.first().map(String::as_str).unwrap_or("0"), lookup),
                    resolve_word(inputs.get(1).map(String::as_str).unwrap_or("0"), lookup),
                ) else {
                    ops.push(Op::PushConst(false));
                    return;
                };
                emit_word(ops, left, lookup);
                emit_word(ops, right, lookup);
                ops.push(compare);
            } else if let Some(math) = MathOp::parse(&name) {
                // A compute block: `row` holds A, the next holds B, and the
                // result goes to the word named on its output cell.
                let target = match grid.cell(block.row, block.col + 1) {
                    crate::model::FbdCell::Out { operand } => operand,
                    _ => String::new(),
                };
                let (Some(left), Some(right), Some(target)) = (
                    resolve_word(inputs.first().map(String::as_str).unwrap_or("0"), lookup),
                    resolve_word(inputs.get(1).map(String::as_str).unwrap_or("0"), lookup),
                    resolve_word(&target, lookup).filter(|target| target.is_writable()),
                ) else {
                    ops.push(Op::PushConst(false));
                    return;
                };
                ops.push(Op::PushConst(true));
                ops.push(Op::Compute {
                    op: math,
                    target_ty: target_type(&target, lookup),
                    target,
                    left,
                    right,
                });
            } else {
                ops.push(Op::PushConst(false));
            }
        }
    }
}

/// The block whose output leaves on `row` to the left of `index`, if any.
fn chained_source(grid: &crate::model::FbdGrid, index: usize, row: usize) -> String {
    let mut best: Option<(usize, String)> = None;
    for (position, block) in grid.blocks.iter().enumerate() {
        if position == index || block.row != row {
            continue;
        }
        if let crate::model::FbdCell::Out { operand } = grid.cell(block.row, block.col + 1) {
            if best
                .as_ref()
                .map(|(col, _)| block.col < *col)
                .unwrap_or(true)
            {
                best = Some((block.col, operand));
            }
        }
    }
    best.map(|(_, operand)| operand).unwrap_or_default()
}

/// A bit input of a grid block: `@n` names another block, `!` negates, and an
/// empty string is a chain that produced nothing.
fn lower_fbd_grid_bit(
    input: &str,
    grid: &crate::model::FbdGrid,
    index: usize,
    lookup: &Lowering<'_>,
    ops: &mut Vec<Op>,
    depth: u8,
) {
    let (negated, rest) = split_negation(input);
    let text = rest.trim();
    if depth < 8 {
        if let Some(reference) = text.strip_prefix('@') {
            if let Ok(number) = reference.parse::<usize>() {
                let target = number.saturating_sub(1);
                if target < index {
                    lower_fbd_grid_block(grid, target, lookup, ops);
                    if negated {
                        ops.push(Op::Not);
                    }
                    return;
                }
            }
        }
    }
    if text.is_empty() {
        ops.push(Op::PushConst(false));
    } else if text.eq_ignore_ascii_case("TRUE") {
        ops.push(Op::PushConst(true));
    } else if let Some(slot) = lookup.bit(text) {
        ops.push(Op::PushNext(slot));
    } else {
        ops.push(Op::PushConst(false));
    }
    if negated {
        ops.push(Op::Not);
    }
}

/// Emit one block and leave its result on the stack.
fn lower_fbd_block(
    lines: &[crate::model::FbdLine],
    index: usize,
    lookup: &Lowering<'_>,
    ops: &mut Vec<Op>,
    depth: u8,
) {
    let Some(line) = lines.get(index) else {
        ops.push(Op::PushConst(false));
        return;
    };
    if depth > 8 {
        ops.push(Op::PushConst(false));
        return;
    }
    let block = line.block.trim().to_ascii_uppercase();
    let form = crate::profile::spec::il_form(&block);
    match form {
        Some(IlForm::And) | Some(IlForm::Or) | Some(IlForm::Xor) => {
            if line.inputs.is_empty() {
                ops.push(Op::PushConst(matches!(form, Some(IlForm::And))));
            } else {
                for (position, input) in line.inputs.iter().enumerate() {
                    lower_fbd_bit(input, lines, index, lookup, ops, depth);
                    if position > 0 {
                        ops.push(match form {
                            Some(IlForm::Or) => Op::Or,
                            Some(IlForm::And) => Op::And,
                            _ => Op::Xor,
                        });
                    }
                }
            }
        }
        Some(IlForm::Not) => {
            lower_fbd_bit(
                line.inputs.first().map(String::as_str).unwrap_or("FALSE"),
                lines,
                index,
                lookup,
                ops,
                depth,
            );
            ops.push(Op::Not);
        }
        _ => {
            if let Some((kind, instance, preset)) = block_of(&block, &block, &line.preset, lookup) {
                if kind.is_latch() {
                    lower_fbd_bit(
                        line.inputs.first().map(String::as_str).unwrap_or("FALSE"),
                        lines,
                        index,
                        lookup,
                        ops,
                        depth,
                    );
                    lower_fbd_bit(
                        line.inputs.get(1).map(String::as_str).unwrap_or("FALSE"),
                        lines,
                        index,
                        lookup,
                        ops,
                        depth,
                    );
                    ops.push(Op::Latch { kind, instance });
                    return;
                }
                lower_fbd_bit(
                    line.inputs.first().map(String::as_str).unwrap_or("FALSE"),
                    lines,
                    index,
                    lookup,
                    ops,
                    depth,
                );
                push_block(ops, lookup, kind, instance, preset);
            } else if let Some(compare) = compare_op(&block) {
                let (Some(left), Some(right)) = (
                    resolve_word(
                        line.inputs.first().map(String::as_str).unwrap_or("0"),
                        lookup,
                    ),
                    resolve_word(
                        line.inputs.get(1).map(String::as_str).unwrap_or("0"),
                        lookup,
                    ),
                ) else {
                    ops.push(Op::PushConst(false));
                    return;
                };
                emit_word(ops, left, lookup);
                emit_word(ops, right, lookup);
                ops.push(compare);
            } else {
                ops.push(Op::PushConst(false));
            }
        }
    }
}

/// A bit input: `@n` is the value on line `n`'s output pin, lowered here so a
/// block can feed another without naming an intermediate symbol.
fn lower_fbd_bit(
    input: &str,
    lines: &[crate::model::FbdLine],
    index: usize,
    lookup: &Lowering<'_>,
    ops: &mut Vec<Op>,
    depth: u8,
) {
    let text = input.trim();
    // A leading `!` is the negated input/output the manual draws as a circle.
    if let Some(rest) = text.strip_prefix('!') {
        lower_fbd_bit(rest, lines, index, lookup, ops, depth);
        ops.push(Op::Not);
        return;
    }
    if let Some(reference) = text.strip_prefix('@') {
        if let Ok(number) = reference.parse::<usize>() {
            let target = number.saturating_sub(1);
            if target < index {
                lower_fbd_block(lines, target, lookup, ops, depth + 1);
                return;
            }
        }
        ops.push(Op::PushConst(false));
        return;
    }
    if text.eq_ignore_ascii_case("TRUE") {
        ops.push(Op::PushConst(true));
    } else if let Some(slot) = lookup.bit(text) {
        ops.push(Op::PushNext(slot));
    } else {
        ops.push(Op::PushConst(false));
    }
}

/// Lower one grid row to ops, leaving exactly one power bit on the stack.
///
/// Power starts true at the rail. A contact pops nothing: it pushes its operand
/// and ANDs it with the power so far. A box consumes the power as its input and
/// pushes `Q`, so the rung continues from the block's output. A row with no
/// element is a true rung.
fn lower_cells(
    cells: &[crate::grid::Cell],
    lookup: &Lowering<'_>,
    ops: &mut Vec<Op>,
    edges: &mut u16,
) -> bool {
    let mut has_power = false;
    for cell in cells {
        match cell {
            crate::grid::Cell::Wire | crate::grid::Cell::Link => continue,
            crate::grid::Cell::Contact { var, negated } => {
                push_bit_read(ops, lookup, var);
                if *negated {
                    ops.push(Op::Not);
                }
                if has_power {
                    ops.push(Op::And);
                }
            }
            crate::grid::Cell::Block {
                block,
                instance,
                preset,
            } => {
                if !has_power {
                    ops.push(Op::PushConst(true));
                }
                if let Some((kind, instance, preset)) = block_of(block, instance, preset, lookup) {
                    push_block(ops, lookup, kind, instance, preset);
                } else {
                    // Validation refused this cell; keep the rung alive.
                    ops.push(Op::PushConst(false));
                }
            }
            crate::grid::Cell::Compare { op, left, right } => {
                let (Some(left), Some(right)) =
                    (resolve_word(left, lookup), resolve_word(right, lookup))
                else {
                    continue;
                };
                emit_word(ops, left, lookup);
                emit_word(ops, right, lookup);
                match compare_op(op) {
                    Some(compare) => ops.push(compare),
                    None => continue,
                }
                if has_power {
                    ops.push(Op::And);
                }
                has_power = true;
                continue;
            }
            crate::grid::Cell::Compute {
                op,
                target,
                left,
                right,
            } => {
                let (Some(left), Some(right)) =
                    (resolve_word(left, lookup), resolve_word(right, lookup))
                else {
                    continue;
                };
                let (Some(math), Some(target)) = (
                    MathOp::parse(op),
                    resolve_word(target, lookup).filter(|target| target.is_writable()),
                ) else {
                    continue;
                };
                if !has_power {
                    ops.push(Op::PushConst(true));
                }
                ops.push(Op::Compute {
                    op: math,
                    target_ty: target_type(&target, lookup),
                    target,
                    left,
                    right,
                });
                // A compute coil ends the rung: there is no coil output.
                return true;
            }
            crate::grid::Cell::Expr { text } => {
                // Validation refused a bad expression, so a false bit keeps the
                // rung from passing on text the compiler could not read.
                match crate::expr::lower_bit(text, lookup.words()) {
                    Ok(expression) => ops.extend(expression),
                    Err(_) => ops.push(Op::PushConst(false)),
                }
                if has_power {
                    ops.push(Op::And);
                }
                has_power = true;
                continue;
            }
            // Validation refuses these until their semantics exist.
            crate::grid::Cell::Edge { var, rising } => {
                let index = lookup.bit(var.as_str()).unwrap_or(0);
                ops.push(Op::Edge {
                    slot: *edges,
                    var: index,
                    rising: *rising,
                });
                *edges = edges.saturating_add(1);
                if has_power {
                    ops.push(Op::And);
                }
            }
        }
        has_power = true;
    }
    if !has_power {
        ops.push(Op::PushConst(true));
    }
    false
}

/// Lower a row and, when it links, the branch below it. Returns rows used.
fn lower_grid_rung(
    run: &[Vec<crate::grid::Cell>],
    index: usize,
    lookup: &Lowering<'_>,
    ops: &mut Vec<Op>,
    edges: &mut u16,
) -> (usize, bool) {
    let Some(cells) = run.get(index) else {
        ops.push(Op::PushConst(true));
        return (1, false);
    };
    // A `Link` at position k joins the row below at that column, so the branch is
    // in parallel with the cells left of the link only; cells from the link on
    // stay in series with both. Lowering the whole row in parallel let a branch
    // contact bypass a series contact to the right of the link.
    let link = cells
        .iter()
        .position(|cell| matches!(cell, crate::grid::Cell::Link))
        .filter(|_| index + 1 < run.len());
    if let Some(link) = link {
        let before = &cells[..link];
        let after = &cells[link + 1..];
        let mut compute = false;
        if !before
            .iter()
            .all(|cell| matches!(cell, crate::grid::Cell::Wire))
        {
            compute |= lower_cells(before, lookup, ops, edges);
        } else {
            ops.push(Op::PushConst(true));
        }
        let mut used = 1;
        used += lower_grid_rung(run, index + 1, lookup, ops, edges).0;
        ops.push(Op::Or);
        if !after
            .iter()
            .all(|cell| matches!(cell, crate::grid::Cell::Wire))
        {
            compute |= lower_cells(after, lookup, ops, edges);
            ops.push(Op::And);
        }
        return (used, compute);
    }
    let compute = lower_cells(cells, lookup, ops, edges);
    (1, compute)
}

/// A word-valued operand of a compare contact. The type is the profile's, so the
/// compiler, the expression parser and the importer share one operand model; the
/// name stays here because every caller imports it from `compile`.
pub use crate::profile::resolve::Operand as WordOperand;

impl Lookup {
    /// The preset a counter's block declares, as the op must bake it.
    fn preset_of(&self, instance: u16) -> PresetSource {
        match self.presets.get(&instance) {
            Some((text, timer)) => {
                resolve_preset(text, *timer, self).unwrap_or(PresetSource::Const(0))
            }
            None => PresetSource::Const(0),
        }
    }
}

/// Every counter instance's preset text, from the block that declares it.
///
/// A `.L`/`.LD` coil lives in its own rung and must load the value the block
/// would assert, including on the first scan, when the block has not run yet.
fn collect_presets(project: &Project, lookup: &mut Lookup) {
    fn walk(logic: &Logic, lookup: &mut Lookup) {
        match logic {
            Logic::Block {
                block,
                instance,
                preset,
            } => record(block, instance, preset, lookup),
            Logic::Series { items } | Logic::Parallel { items } => {
                for item in items {
                    walk(item, lookup);
                }
            }
            _ => {}
        }
    }
    fn record(block: &str, instance: &str, preset: &str, lookup: &mut Lookup) {
        if let Some((kind, index, _)) = parse_block(block, instance, "") {
            if kind.is_counter() {
                lookup
                    .presets
                    .entry(index)
                    .or_insert_with(|| (preset.to_string(), kind.is_timer()));
            }
        }
    }
    for pou in project.all_pous() {
        // An IL call declares its block's preset too, and a `.LD` in another POU
        // must load the same value.
        for line in &pou.il {
            let crate::il::IlLine::Instruction { mnemonic, operand } = crate::il::parse_line(line)
            else {
                continue;
            };
            if !crate::profile::instr::is_call_mnemonic(&mnemonic) {
                continue;
            }
            let Some((name, args)) = crate::il::parse_call(&operand) else {
                continue;
            };
            if let Some((_, value)) = args.iter().find(|(key, _)| key == "PT" || key == "PV") {
                record(&name, &name, value, lookup);
            }
        }
        for section in &pou.sections {
            for rung in &section.rungs {
                for cell in &rung.cells {
                    if let crate::grid::Cell::Block {
                        block,
                        instance,
                        preset,
                    } = cell
                    {
                        record(block, instance, preset, lookup);
                    }
                }
                walk(&rung.logic, lookup);
            }
        }
    }
}

/// Push the op that runs one block. A counter carries the profile's baked rule;
/// everything else is the shared block op.
fn push_block(
    ops: &mut Vec<Op>,
    lookup: &Lowering<'_>,
    kind: FbKind,
    instance: u16,
    preset: PresetSource,
) {
    if kind.is_counter() {
        ops.push(Op::Counter {
            instance,
            preset,
            rule: lookup.profile.counter_rule(kind.name()),
        });
    } else {
        ops.push(Op::Fb {
            kind,
            instance,
            preset,
        });
    }
}

/// Push one declared bit: a bit-image slot, a block member, or a bit of a word.
///
/// The three forms are one operand space to the program — `%I0.0`, `TON0.Q`,
/// `Bits[0].0` — so a contact, an edge or a coil asks this rather than each spelling
/// its own question.
fn push_bit_read(ops: &mut Vec<Op>, lookup: &Lowering<'_>, var: &str) {
    match lookup.profile.bit_member(var) {
        // A body's block member reads the site's copy: `#Delay.Q` is this instance's
        // timer, not the first one the program happens to declare.
        Some((instance, member)) => push_member(
            ops,
            lookup,
            lookup.block_index(lookup.site, instance),
            member,
        ),
        None => match lookup.word_bit(var) {
            Some((slot, bit)) => ops.push(Op::WordBit { slot, bit }),
            None => ops.push(Op::Push(resolve_bit(lookup, var).unwrap_or(0))),
        },
    }
}

/// The bit slot one operand names. A function block's body reads its own instance's
/// variables (`#Start` is this instance's `start`), a program reads an instance's
/// field (`Station.Filling`), and everything else is a project bit.
fn resolve_bit(lookup: &Lowering<'_>, var: &str) -> Option<u16> {
    if let Some(site) = lookup.site {
        if let Some(slot) = lookup.instance_slot(site, var) {
            return Some(slot);
        }
    }
    if let Some((instance, field)) = var.split_once('.') {
        if let Some(slot) = lookup.instance_slot(instance, field) {
            return Some(slot);
        }
    }
    lookup.bit(var)
}

/// Push one block member's value: `Q` from the block's state, and empty/full as
/// the compares the compiler bakes from the profile's numbers.
fn push_member(
    ops: &mut Vec<Op>,
    lookup: &Lowering<'_>,
    instance: u16,
    member: crate::profile::BitMember,
) {
    use crate::profile::BitMember;
    match member {
        BitMember::Done => ops.push(Op::FbQ(instance)),
        BitMember::Timing => ops.push(Op::FbTiming(instance)),
        BitMember::Empty => {
            ops.push(Op::FbWord(instance));
            ops.push(Op::LoadConst(0));
            ops.push(Op::CmpEq);
        }
        BitMember::Full => {
            ops.push(Op::FbWord(instance));
            ops.push(Op::LoadConst(lookup.profile.counter_top().unwrap_or(0)));
            ops.push(Op::CmpEq);
        }
        // A write member read as a value: validation refuses it, and a false bit
        // is the stand-in that keeps the compiler total.
        BitMember::Up | BitMember::Down | BitMember::Reset | BitMember::Load => {
            ops.push(Op::PushConst(false))
        }
    }
}

/// The width a compute's target is written at, when the target is a declared
/// variable rather than a marker word or a HAL point.
fn target_type(target: &WordOperand, lookup: &Lookup) -> Option<crate::profile::layout::WordType> {
    match target {
        WordOperand::Slot(index) => lookup.slot_type(*index),
        _ => None,
    }
}

/// Resolve a word operand through the profile: the dialect's own spellings, then
/// the declared word symbols.
fn resolve_word(text: &str, lookup: &Lookup) -> Option<WordOperand> {
    crate::profile::resolve::resolve(text, lookup.words()).ok()
}

/// Resolve a preset in a context that has the symbol table: a declared word
/// point becomes a live source, everything else goes through `preset_source`.
fn resolve_preset(text: &str, timer: bool, lookup: &Lookup) -> Option<PresetSource> {
    if let Some(source) = indexed_word(text, lookup) {
        return Some(source);
    }
    lookup
        .word(text)
        .map(PresetSource::Word)
        .or_else(|| preset_source(text, timer))
}

/// Split `name[index]` when the text is one array element read, and resolve both
/// halves against the declarations.
///
/// A literal index folds into the element's slot, so the scan reads a fixed slot and
/// validation has already refused one past the array. A declared index stays a slot:
/// the scan reads it, clamps, and adds it to the base.
fn indexed_word(text: &str, lookup: &Lookup) -> Option<PresetSource> {
    let operand = crate::model::ArrayElement::parse(text)?;
    let var = lookup.array(&operand.array.to_ascii_lowercase())?;
    let element = match lookup.word(&operand.index) {
        Some(slot) => PresetSource::Indexed {
            base: var.slot,
            index: slot,
            len: var.elements,
        },
        None => {
            let number = operand.literal()?;
            if number < 0 || number >= i64::from(var.elements) {
                return None;
            }
            PresetSource::Word(var.slot + number as u16)
        }
    };
    Some(element)
}

/// A preset that needs no symbol table: a marker word, a time (`T#4s`), or a
/// count.
/// Validation and the compiler share this, so a preset one accepts is one the
/// other can resolve.
pub fn preset_source(text: &str, timer: bool) -> Option<PresetSource> {
    match crate::il::word_operand(text) {
        Some(slot) => Some(PresetSource::Word(slot)),
        None => parse_preset(text, timer).map(PresetSource::Const),
    }
}

fn emit_word(ops: &mut Vec<Op>, value: WordOperand, lookup: &Lookup) {
    match value {
        // A declared variable is read at its own width; a marker word or a HAL
        // point is the raw 32 bits, because nothing declared a width for it.
        WordOperand::Slot(index) => match lookup.slot_type(index) {
            Some(ty) => ops.push(Op::LoadTyped { slot: index, ty }),
            None => ops.push(Op::LoadWord(index)),
        },
        WordOperand::Word(index) => ops.push(Op::LoadWord(index)),
        WordOperand::Const(value) => ops.push(Op::LoadConst(value)),
        WordOperand::Block(index) => ops.push(Op::FbWord(index)),
        WordOperand::BlockPreset(index) => ops.push(Op::FbPreset(index)),
    }
}

/// A compare block name, through the instruction table.
pub fn compare_op(name: &str) -> Option<Op> {
    match crate::profile::spec::spec(name)?.family {
        Family::Compare(name) => Some(compare_op_for(name)),
        _ => None,
    }
}

/// The VM operation for a compare, for callers that already resolved the name
/// through the table.
pub fn compare_op_for(name: CompareName) -> Op {
    match name {
        CompareName::Gt => Op::CmpGt,
        CompareName::Ge => Op::CmpGe,
        CompareName::Eq => Op::CmpEq,
        CompareName::Ne => Op::CmpNe,
        CompareName::Le => Op::CmpLe,
        CompareName::Lt => Op::CmpLt,
    }
}

/// `TON` + `TON2` + `T#4s` -> kind, instance index, preset in ms or counts.
///
/// The profile parses the name and counts the instances; this maps its kind onto
/// the compiler's enum.
/// A block with its preset resolved through the symbol table: a literal, a time, a
/// marker word, a declared word, or one element of a declared array.
///
/// The kind and the instance come from the block's own spelling; the preset is a
/// separate operand and is resolved like any other, because `Durations[Phase]`
/// moves with the index and cannot be a compile-time number.
fn block_of(
    block: &str,
    instance: &str,
    preset: &str,
    lookup: &Lowering<'_>,
) -> Option<(FbKind, u16, PresetSource)> {
    let (kind, index, _) = parse_block(block, instance, "")?;
    let source = resolve_preset(preset, kind.is_timer(), lookup).unwrap_or(PresetSource::Const(0));
    let index = lookup.block_index(lookup.site, index);
    Some((kind, index, source))
}

pub fn parse_block(block: &str, instance: &str, preset: &str) -> Option<(FbKind, u16, i32)> {
    let (name, index, preset) = crate::profile::resolve::parse_block(block, instance, preset)?;
    Some((FbKind::from_name(name), index, preset))
}

/// Resolve an IL operand: a marker word, a decimal literal, or a declared
/// either bank. A word symbol resolves to its bank slot, never to a bit slot.
fn il_operand(operand: &str, lookup: &Lookup) -> IlOperand {
    let text = operand.trim();
    if let Some(index) = crate::il::word_operand(text) {
        return IlOperand::Word(index);
    }
    if let Ok(value) = text.parse::<i32>() {
        return IlOperand::Literal(value);
    }
    if let Some(slot) = lookup.word(text) {
        return IlOperand::Word(slot);
    }
    IlOperand::Bit(lookup.bit(text).unwrap_or(0))
}

/// Register a rung label against the op index its row starts at. A blank label
/// is not a label; validation refuses a jump that names one.
fn record_label(labels: &mut HashMap<String, usize>, label: &str, at: usize) {
    let label = label.trim();
    if !label.is_empty() {
        labels.insert(label.to_ascii_uppercase(), at);
    }
}

/// Lower a rung's coil.
///
/// A `Jump` reserves a conditional jump the section patches once its labels are
/// known. A `Call` inlines the callee, guarded by the rung's power: a call is an
/// inline, so there are no locals and no stack frame, the same trade-off `CAL`
/// makes. Both consume the power bit, so the operand stack stays as balanced as
/// a store leaves it.
fn lower_coil(
    ops: &mut Vec<Op>,
    lookup: &Lowering<'_>,
    pous: &HashMap<String, &crate::model::Pou>,
    coil: &crate::model::Coil,
    jumps: &mut Vec<(usize, String)>,
    edges: &mut u16,
    depth: u8,
) {
    match coil.kind {
        CoilKind::Jump => {
            // `when: true` is the jump taken while the rung passes.
            jumps.push((ops.len(), coil.var.trim().to_ascii_uppercase()));
            ops.push(Op::JumpIf {
                target: 0,
                when: true,
            });
        }
        CoilKind::Call => {
            // Skip the inlined body when the rung does not pass. `when: false`
            // is a jump taken while the power bit is false.
            let skip = ops.len();
            ops.push(Op::JumpIf {
                target: 0,
                when: false,
            });
            let site = coil.var.trim().to_ascii_lowercase();
            // An input pin is written into the instance's variable before the body
            // runs, and an output is read out of it afterwards. The pins are the
            // block's, so an instance's two calls do not share them.
            for argument in coil.args.iter().filter(|argument| !argument.out) {
                let (Some(pin), Some(source)) = (
                    lookup.instance_slot(&site, &argument.pin),
                    resolve_bit(lookup, &argument.var),
                ) else {
                    continue;
                };
                ops.push(Op::Push(source));
                ops.push(Op::Store(pin));
            }
            let callee = pous
                .get(&coil.var.trim().to_ascii_uppercase())
                .copied()
                .or_else(|| {
                    lookup
                        .fb_type
                        .get(&site)
                        .and_then(|block| pous.get(block))
                        .copied()
                });
            if let Some(callee) = callee {
                let inside = Lowering {
                    lookup: lookup.lookup,
                    site: Some(&site),
                };
                lower_pou(callee, pous, &inside, ops, edges, depth + 1);
            }
            for argument in coil.args.iter().filter(|argument| argument.out) {
                let (Some(pin), Some(target)) = (
                    lookup.instance_slot(&site, &argument.pin),
                    resolve_bit(lookup, &argument.var),
                ) else {
                    continue;
                };
                ops.push(Op::Push(pin));
                ops.push(Op::Store(target));
            }
            let after = ops.len() as u16;
            if let Op::JumpIf { target, .. } = &mut ops[skip] {
                *target = after;
            }
        }
        CoilKind::Compute => {
            // The assignment runs only while the rung passes, like any coil.
            // Validation refused an empty or unreadable text, so a failed
            // resolve writes nothing instead of guessing a target.
            let skip = ops.len();
            ops.push(Op::JumpIf {
                target: 0,
                when: false,
            });
            if let Some(text) = coil.expr.as_deref() {
                if let Ok((target, value)) = crate::expr::lower_assignment(text, lookup.words()) {
                    ops.extend(value);
                    // A declared variable is written at its own width, so the value
                    // two reads see is the value the type can hold. A marker word or
                    // a HAL point is written raw.
                    match target {
                        WordOperand::Slot(index) => match lookup.slot_type(index) {
                            Some(ty) => ops.push(Op::StoreTyped { slot: index, ty }),
                            None => ops.push(Op::StoreTarget(target)),
                        },
                        _ => ops.push(Op::StoreTarget(target)),
                    }
                }
            }
            let after = ops.len() as u16;
            if let Op::JumpIf { target, .. } = &mut ops[skip] {
                *target = after;
            }
        }
        _ => store_coil(ops, lookup, coil),
    }
}

fn store_coil(ops: &mut Vec<Op>, lookup: &Lowering<'_>, coil: &crate::model::Coil) {
    // `lower_coil` routes these before they get here.
    if matches!(
        coil.kind,
        CoilKind::Jump | CoilKind::Call | CoilKind::Compute
    ) {
        return;
    }
    // A coil on a block member — `CTU0.R`, `CTD0.LD`, and ClassicLadder's
    // `.CU`/`.CD`/`.L` — is a write to that block, not to a bit.
    if let Some((instance, member)) = lookup.profile.bit_member(&coil.var) {
        if member.is_writable() {
            use crate::profile::BitMember;
            let instance = lookup.block_index(lookup.site, instance);
            ops.push(match member {
                BitMember::Reset => Op::FbReset(instance),
                BitMember::Load => Op::FbLoad {
                    instance,
                    preset: lookup.preset_of(instance),
                },
                BitMember::Up => Op::Count {
                    instance,
                    up: true,
                    top: lookup.profile.counter_top(),
                },
                BitMember::Down => Op::Count {
                    instance,
                    up: false,
                    top: lookup.profile.counter_top(),
                },
                _ => return,
            });
            return;
        }
    }
    let kind = coil.kind;
    if kind == CoilKind::Negated {
        ops.push(Op::Not);
    }
    // A coil on a bit of a declared word writes that bit and leaves the word's other
    // bits alone; the three coil kinds are the three bit writes.
    if let Some((slot, bit)) = lookup.word_bit(coil.var.as_str()) {
        ops.push(match kind {
            CoilKind::Normal | CoilKind::Negated => Op::StoreWordBit { slot, bit },
            CoilKind::Set => Op::SetWordBit { slot, bit },
            CoilKind::Reset => Op::ResetWordBit { slot, bit },
            // The guard at the top of this function returned for these.
            CoilKind::Jump | CoilKind::Call | CoilKind::Compute => return,
        });
        return;
    }
    let index = resolve_bit(lookup, coil.var.as_str()).unwrap_or(0);
    ops.push(match kind {
        CoilKind::Normal | CoilKind::Negated => Op::Store(index),
        CoilKind::Set => Op::Set(index),
        CoilKind::Reset => Op::Reset(index),
        // The guard at the top of this function returned for these.
        CoilKind::Jump | CoilKind::Call | CoilKind::Compute => return,
    });
}

fn lower_logic(logic: &Logic, lookup: &Lowering<'_>, edges: &mut u16, ops: &mut Vec<Op>) {
    match logic {
        Logic::Const { value } => ops.push(Op::PushConst(*value)),
        Logic::Contact { var, negated } => {
            push_bit_read(ops, lookup, var);
            if *negated {
                ops.push(Op::Not);
            }
        }
        Logic::Expr { text } => match crate::expr::lower_bit(text, lookup.words()) {
            Ok(expression) => ops.extend(expression),
            // Validation refused it; a false bit is the safe stand-in.
            Err(_) => ops.push(Op::PushConst(false)),
        },
        Logic::Edge { var, rising } => {
            let slot = *edges;
            *edges = edges.saturating_add(1);
            ops.push(Op::Edge {
                slot,
                var: lookup.bit(var.as_str()).unwrap_or(0),
                rising: *rising,
            });
        }
        Logic::Compare {
            operator,
            left,
            right,
        } => {
            let (Some(left), Some(right)) =
                (resolve_word(left, lookup), resolve_word(right, lookup))
            else {
                ops.push(Op::PushConst(false));
                return;
            };
            emit_word(ops, left, lookup);
            emit_word(ops, right, lookup);
            match compare_op(operator) {
                Some(compare) => ops.push(compare),
                None => ops.push(Op::PushConst(false)),
            }
        }
        Logic::Block {
            block,
            instance,
            preset,
        } => match block_of(block, instance, preset, lookup) {
            Some((kind, instance, preset)) => push_block(ops, lookup, kind, instance, preset),
            None => ops.push(Op::PushConst(false)),
        },
        Logic::Series { items } => {
            // Power flows left to right. A block consumes the power so far as
            // its input and pushes Q, so the rest of the rung continues from
            // the block's output.
            let mut powered = false;
            for item in items {
                match item {
                    Logic::Block {
                        block,
                        instance,
                        preset,
                    } => {
                        if !powered {
                            ops.push(Op::PushConst(true));
                        }
                        match block_of(block, instance, preset, lookup) {
                            Some((kind, instance, preset)) => {
                                push_block(ops, lookup, kind, instance, preset)
                            }
                            None => ops.push(Op::PushConst(false)),
                        }
                    }
                    other => {
                        lower_logic(other, lookup, edges, ops);
                        if powered {
                            ops.push(Op::And);
                        }
                    }
                }
                powered = true;
            }
            if !powered {
                ops.push(Op::PushConst(true));
            }
        }
        Logic::Parallel { items } => {
            if items.is_empty() {
                ops.push(Op::PushConst(false));
            } else {
                for (index, item) in items.iter().enumerate() {
                    lower_logic(item, lookup, edges, ops);
                    if index > 0 {
                        ops.push(Op::Or);
                    }
                }
            }
        }
    }
}
