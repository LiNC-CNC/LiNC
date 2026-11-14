//! Deterministic scan VM. `scan` allocates nothing, takes no locks and does no I/O.
//!
//! Image rules: `next` starts as a copy of `prev`; inputs are written into both
//! images so logic sees the live input while coil writes only reach `next` and
//! therefore stay one scan behind. Forces are applied to both images before the
//! ops pass (a forced input or variable is what logic reads) and again to `next`
//! after it, so a forced coil wins over Store, Set and Reset.
//!
//! Word I/O follows the bit rule: `scan_with_io` writes the word and float
//! inputs into their bank regions before the ops pass, so a rung reads the field
//! value on the same scan, and reads the output regions after it.

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::compile::{
    Bank, Binding, Compiled, FbKind, MathOp, Op, PresetSource, Program, SymbolKind, WordOperand,
};
use crate::profile::counter::{CounterDone, CounterRule};

/// Operand-stack depth. Validation caps logic nesting at 64, so this is slack.
const STACK_DEPTH: usize = 128;

/// A guard against a jump loop that never terminates. A scan that reaches it
/// stops executing instructions instead of hanging the scheduler.
const MAX_OPS_PER_SCAN: usize = 100_000;

/// One 20 ms tick, the default scan period.
pub const DEFAULT_DT_MS: u16 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanStats {
    pub scan_us: u32,
    pub scan_count: u64,
}

/// One word of the bank in a snapshot: a marker word, a word point, or a block
/// `TON0.ET`. The value is 32 bits; the wire type widened at protocol 8.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordValue {
    pub name: String,
    pub value: i32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VarValue {
    pub name: String,
    pub value: bool,
    pub kind: SymbolKind,
    pub forced: bool,
}

/// One scan's field I/O, borrowed so the scan path still allocates nothing.
///
/// `bits` are the bit inputs in compiled order; `words` and `floats` are the
/// word/float input regions in bank order. The daemon converts HAL `f64` pins to
/// `i32` at the boundary, which is ClassicLadder's `%IF` behaviour.
pub struct IoFrame<'a> {
    pub bits: &'a [bool],
    pub words: &'a [i32],
    pub floats: &'a [i32],
}

#[derive(Debug, Clone)]
pub struct Vm {
    program: Program,
    names: Vec<(String, SymbolKind)>,
    /// Declared word symbols by bank slot, so the snapshot can name `IW`/`QW`
    /// slots the way the project does instead of by a bare slot number.
    word_names: Vec<(String, u16)>,
    inputs: Vec<Binding>,
    outputs: Vec<Binding>,
    prev: Vec<bool>,
    next: Vec<bool>,
    words: Vec<i32>,
    dt_ms: u16,
    blocks: Vec<FbState>,
    /// Previous value of each edge contact, one slot per contact occurrence.
    edge_prev: Vec<bool>,
    /// Slots declared `retain`, with their names, in program order.
    retained: Vec<(u16, String)>,
    /// Hash of that layout, so a store from another shape is refused.
    layout: u64,
    output_image: Vec<bool>,
    forces: Vec<Option<bool>>,
    scan_count: u64,
    last_scan_us: u32,
}

impl Vm {
    pub fn new(compiled: &Compiled) -> Self {
        // Word symbols index the bank, not the bit image: only bit symbols make
        // the image, and `var_count` counts exactly those.
        let slots = compiled.program.var_count as usize;
        let retained = compiled
            .symbols
            .iter()
            .filter(|symbol| symbol.retain && symbol.bank == Bank::Bit)
            .map(|symbol| (symbol.index, symbol.name.clone()))
            .collect::<Vec<_>>();
        let layout = crate::retain::layout_hash(
            &retained
                .iter()
                .map(|(index, name)| (*index, name.as_str()))
                .collect::<Vec<_>>(),
            compiled.program.words,
        );
        Self {
            program: compiled.program.clone(),
            names: compiled
                .symbols
                .iter()
                .filter(|symbol| symbol.bank == Bank::Bit)
                .map(|symbol| (symbol.name.clone(), symbol.kind))
                .collect(),
            word_names: compiled
                .symbols
                .iter()
                .filter(|symbol| symbol.bank == Bank::Word)
                .map(|symbol| (symbol.name.clone(), symbol.index))
                .collect(),
            inputs: compiled.inputs.clone(),
            outputs: compiled.outputs.clone(),
            prev: vec![false; slots],
            next: vec![false; slots],
            words: vec![0; compiled.program.word_count as usize],
            dt_ms: DEFAULT_DT_MS,
            blocks: vec![FbState::default(); compiled.program.fb_count as usize],
            edge_prev: vec![false; compiled.program.edge_count as usize],
            retained,
            layout,
            output_image: vec![false; compiled.outputs.len()],
            forces: vec![None; slots],
            scan_count: 0,
            last_scan_us: 0,
        }
    }

    /// Write one frame's field values into the images. A bit input goes into
    /// both images, so logic reads the live value for the whole scan; word and
    /// float inputs go into their bank regions before the ops pass. The `take`
    /// bounds stop a longer frame from spilling into the output regions.
    fn write_inputs(&mut self, io: &IoFrame<'_>) {
        for slot in 0..self.inputs.len() {
            if let Some(value) = io.bits.get(slot) {
                let index = self.inputs[slot].index as usize;
                if let Some(cell) = self.next.get_mut(index) {
                    *cell = *value;
                }
                if let Some(cell) = self.prev.get_mut(index) {
                    *cell = *value;
                }
            }
        }
        let layout = self.program.words;
        for (offset, value) in io.words.iter().take(layout.iw as usize).enumerate() {
            if let Some(cell) = self.words.get_mut(layout.iw_base() as usize + offset) {
                *cell = *value;
            }
        }
        for (offset, value) in io.floats.iter().take(layout.fi as usize).enumerate() {
            if let Some(cell) = self.words.get_mut(layout.fi_base() as usize + offset) {
                *cell = *value;
            }
        }
    }

    /// Clears both scan images and every force. The scan counter is kept.
    pub fn reset(&mut self) {
        self.prev.fill(false);
        self.next.fill(false);
        self.words.fill(0);
        self.blocks.fill(FbState::default());
        self.edge_prev.fill(false);
        self.output_image.fill(false);
        self.forces.fill(None);
    }

    /// Scan with the time since the previous scan. The default is one 20 ms
    /// tick, which is the unit the scan measures time in and what deterministic
    /// tests use.
    pub fn scan(&mut self, inputs: &[bool]) -> ScanStats {
        self.scan_with_dt(inputs, DEFAULT_DT_MS)
    }

    /// The bit-only scan the tests and the older callers use: no word or float
    /// field values are written, so the bank keeps whatever a test put in it.
    pub fn scan_with_dt(&mut self, inputs: &[bool], dt_ms: u16) -> ScanStats {
        self.scan_with_io(
            IoFrame {
                bits: inputs,
                words: &[],
                floats: &[],
            },
            dt_ms,
        )
    }

    /// One scan with the field I/O the daemon read this cycle. Still allocates
    /// nothing, takes no locks and performs no I/O.
    pub fn scan_with_io(&mut self, io: IoFrame<'_>, dt_ms: u16) -> ScanStats {
        let started = Instant::now();
        self.dt_ms = dt_ms.max(1);
        self.next.copy_from_slice(&self.prev);
        self.write_inputs(&io);

        // Both images, as documented: a function-block line reads `next`, so a
        // force that only reached `prev` would be invisible to it.
        apply_forces(&mut self.prev, &self.forces);
        apply_forces(&mut self.next, &self.forces);

        let mut stack = [false; STACK_DEPTH];
        let mut depth = 0usize;
        let mut word_stack = [0i32; STACK_DEPTH];
        let mut word_depth = 0usize;
        let mut pc = 0usize;
        let mut executed = 0usize;
        while pc < self.program.ops.len() {
            executed += 1;
            if executed > MAX_OPS_PER_SCAN {
                // A jump loop that never ends would freeze the scan. The limit
                // is far above a real program's op count; the rest of the scan
                // is skipped and the ladder keeps its last state.
                break;
            }
            let op_index = pc;
            pc += 1;
            match self.program.ops[op_index] {
                Op::PushConst(value) => push(&mut stack, &mut depth, value),
                Op::Push(index) => {
                    let value = self.prev.get(index as usize).copied().unwrap_or(false);
                    push(&mut stack, &mut depth, value);
                }
                Op::PushNext(index) => {
                    let value = self.next.get(index as usize).copied().unwrap_or(false);
                    push(&mut stack, &mut depth, value);
                }
                Op::Drop => {
                    let _ = pop(&stack, &mut depth);
                }
                Op::Not => {
                    let value = pop(&stack, &mut depth);
                    push(&mut stack, &mut depth, !value);
                }
                Op::And => {
                    let right = pop(&stack, &mut depth);
                    let left = pop(&stack, &mut depth);
                    push(&mut stack, &mut depth, left && right);
                }
                Op::Or => {
                    let right = pop(&stack, &mut depth);
                    let left = pop(&stack, &mut depth);
                    push(&mut stack, &mut depth, left || right);
                }
                Op::Xor => {
                    let right = pop(&stack, &mut depth);
                    let left = pop(&stack, &mut depth);
                    push(&mut stack, &mut depth, left ^ right);
                }
                Op::Store(index) => {
                    let value = pop(&stack, &mut depth);
                    if let (Some(cell), Some(seen)) = (
                        self.next.get_mut(index as usize),
                        self.prev.get_mut(index as usize),
                    ) {
                        // A coil write is visible to the rungs after it in the same
                        // scan, as the manual's ladder shows. External inputs stay
                        // latched for the whole scan, which is what makes the image
                        // a scan-boundary snapshot of the field I/O only.
                        *seen = value;
                        *cell = value;
                    }
                }
                Op::Set(index) => {
                    if pop(&stack, &mut depth) {
                        if let (Some(cell), Some(seen)) = (
                            self.next.get_mut(index as usize),
                            self.prev.get_mut(index as usize),
                        ) {
                            // Latched writes are live for later rungs too.
                            *seen = true;
                            *cell = true;
                        }
                    }
                }
                Op::Reset(index) => {
                    if pop(&stack, &mut depth) {
                        if let (Some(cell), Some(seen)) = (
                            self.next.get_mut(index as usize),
                            self.prev.get_mut(index as usize),
                        ) {
                            *seen = false;
                            *cell = false;
                        }
                    }
                }
                Op::LoadWord(index) => {
                    let value = self.words.get(index as usize).copied().unwrap_or(0);
                    push_word(&mut word_stack, &mut word_depth, value);
                }
                Op::LoadConst(value) => push_word(&mut word_stack, &mut word_depth, value),
                // A declared variable is read at the width it was declared with.
                Op::LoadTyped { slot, ty } => {
                    let raw = self.words.get(slot as usize).copied().unwrap_or(0);
                    push_word(&mut word_stack, &mut word_depth, ty.load(raw));
                }
                // One bit of a bank word. A read is the bit; a write leaves the
                // word's other bits as they were, which is what makes a coil on
                // `Bits[0].0` a write to one bit and not to the word.
                Op::WordBit { slot, bit } => {
                    let word = self.words.get(slot as usize).copied().unwrap_or(0);
                    push(&mut stack, &mut depth, (word >> bit) & 1 == 1);
                }
                Op::StoreWordBit { slot, bit } => {
                    let value = pop(&stack, &mut depth);
                    self.set_word_bit(slot, bit, value);
                }
                Op::SetWordBit { slot, bit } => {
                    if pop(&stack, &mut depth) {
                        self.set_word_bit(slot, bit, true);
                    }
                }
                Op::ResetWordBit { slot, bit } => {
                    if pop(&stack, &mut depth) {
                        self.set_word_bit(slot, bit, false);
                    }
                }
                Op::StoreTyped { slot, ty } => {
                    let value = pop_word(&word_stack, &mut word_depth);
                    if let Some(cell) = self.words.get_mut(slot as usize) {
                        *cell = ty.store(value);
                    }
                }
                Op::StoreWord(index) => {
                    let value = pop_word(&word_stack, &mut word_depth);
                    if let Some(cell) = self.words.get_mut(index as usize) {
                        *cell = value;
                    }
                }
                Op::Add => bin_word(&mut word_stack, &mut word_depth, i32::wrapping_add),
                Op::Sub => bin_word(&mut word_stack, &mut word_depth, i32::wrapping_sub),
                Op::Mul => bin_word(&mut word_stack, &mut word_depth, i32::wrapping_mul),
                Op::Div => bin_word(&mut word_stack, &mut word_depth, |left, right| {
                    if right == 0 {
                        0
                    } else {
                        left.wrapping_div(right)
                    }
                }),
                Op::Mod => bin_word(&mut word_stack, &mut word_depth, |left, right| {
                    if right == 0 {
                        0
                    } else {
                        left.wrapping_rem(right)
                    }
                }),
                Op::Min => bin_word(&mut word_stack, &mut word_depth, |left, right| {
                    left.min(right)
                }),
                Op::Max => bin_word(&mut word_stack, &mut word_depth, |left, right| {
                    left.max(right)
                }),
                Op::Abs => {
                    let value = pop_word(&word_stack, &mut word_depth);
                    push_word(&mut word_stack, &mut word_depth, value.wrapping_abs());
                }
                Op::Pow => bin_word(&mut word_stack, &mut word_depth, |base, exponent| {
                    // A negative exponent has no integer result; 0 keeps the
                    // scan deterministic instead of raising a fault.
                    if exponent < 0 {
                        0
                    } else {
                        base.wrapping_pow(exponent as u32)
                    }
                }),
                Op::CmpGt => cmp_word(
                    &mut stack,
                    &mut depth,
                    &mut word_stack,
                    &mut word_depth,
                    |a, b| a > b,
                ),
                Op::CmpGe => cmp_word(
                    &mut stack,
                    &mut depth,
                    &mut word_stack,
                    &mut word_depth,
                    |a, b| a >= b,
                ),
                Op::CmpEq => cmp_word(
                    &mut stack,
                    &mut depth,
                    &mut word_stack,
                    &mut word_depth,
                    |a, b| a == b,
                ),
                Op::CmpNe => cmp_word(
                    &mut stack,
                    &mut depth,
                    &mut word_stack,
                    &mut word_depth,
                    |a, b| a != b,
                ),
                Op::CmpLe => cmp_word(
                    &mut stack,
                    &mut depth,
                    &mut word_stack,
                    &mut word_depth,
                    |a, b| a <= b,
                ),
                Op::CmpLt => cmp_word(
                    &mut stack,
                    &mut depth,
                    &mut word_stack,
                    &mut word_depth,
                    |a, b| a < b,
                ),
                Op::Fb {
                    kind,
                    instance,
                    preset,
                } => {
                    let input = pop(&stack, &mut depth);
                    // A word source is read every scan, so a recipe word drives
                    // the block; a literal is the value it was compiled with.
                    let preset = match preset {
                        PresetSource::Const(value) => value,
                        PresetSource::Word(slot) => self.word(slot),
                    };
                    let q = run_block(
                        &mut self.blocks,
                        instance as usize,
                        kind,
                        input,
                        preset,
                        self.dt_ms,
                    );
                    push(&mut stack, &mut depth, q);
                }
                Op::Counter {
                    instance,
                    preset,
                    rule,
                } => {
                    let input = pop(&stack, &mut depth);
                    let preset = match preset {
                        PresetSource::Const(value) => value,
                        PresetSource::Word(slot) => self.word(slot),
                    };
                    let q = run_counter(&mut self.blocks, instance as usize, input, preset, rule);
                    push(&mut stack, &mut depth, q);
                }
                Op::Count { instance, up, top } => {
                    let power = pop(&stack, &mut depth);
                    count_member(&mut self.blocks, instance as usize, up, power, top);
                }
                Op::Latch { kind, instance } => {
                    let reset = pop(&stack, &mut depth);
                    let set = pop(&stack, &mut depth);
                    let q = run_latch(&mut self.blocks, instance as usize, kind, set, reset);
                    push(&mut stack, &mut depth, q);
                }
                Op::FbReset(instance) => {
                    let power = pop(&stack, &mut depth);
                    if power {
                        if let Some(state) = self.blocks.get_mut(instance as usize) {
                            state.value = 0;
                        }
                    }
                }
                Op::FbLoad { instance, preset } => {
                    let power = pop(&stack, &mut depth);
                    if power {
                        let preset = match preset {
                            PresetSource::Const(value) => value,
                            PresetSource::Word(slot) => self.word(slot),
                        }
                        .max(0);
                        if let Some(state) = self.blocks.get_mut(instance as usize) {
                            state.value = preset;
                            state.preset = preset;
                        }
                    }
                }
                Op::FbPreset(instance) => {
                    let value = self
                        .blocks
                        .get(instance as usize)
                        .map(|state| state.preset)
                        .unwrap_or(0);
                    push_word(&mut word_stack, &mut word_depth, value);
                }
                Op::FbWord(instance) => {
                    let value = self
                        .blocks
                        .get(instance as usize)
                        .map(|state| state.value)
                        .unwrap_or(0);
                    push_word(&mut word_stack, &mut word_depth, value);
                }
                Op::Edge { slot, var, rising } => {
                    let now = self.prev.get(var as usize).copied().unwrap_or(false);
                    let before = self.edge_prev.get(slot as usize).copied().unwrap_or(false);
                    let changed = if rising {
                        now && !before
                    } else {
                        before && !now
                    };
                    if let Some(cell) = self.edge_prev.get_mut(slot as usize) {
                        *cell = now;
                    }
                    push(&mut stack, &mut depth, changed);
                }
                Op::Compute {
                    op,
                    target,
                    target_ty,
                    left,
                    right,
                } => {
                    let power = pop(&stack, &mut depth);
                    if power {
                        let left = self.word_value(left);
                        let right = self.word_value(right);
                        let result = match op {
                            MathOp::Add => left.wrapping_add(right),
                            MathOp::Sub => left.wrapping_sub(right),
                            MathOp::Mul => left.wrapping_mul(right),
                            MathOp::Div => {
                                if right == 0 {
                                    0
                                } else {
                                    left.wrapping_div(right)
                                }
                            }
                            MathOp::Move => left,
                        };
                        // A declared variable holds the value its width can hold, so
                        // a compute writing one is stored the same way a rung's
                        // compute coil stores it.
                        let result = match target_ty {
                            Some(ty) => ty.store(result),
                            None => result,
                        };
                        self.write_word(target, result);
                    }
                }
                Op::StoreTarget(target) => {
                    let value = pop_word(&word_stack, &mut word_depth);
                    self.write_word(target, value);
                }
                Op::FbQ(instance) => {
                    let value = self
                        .blocks
                        .get(instance as usize)
                        .map(|state| state.q)
                        .unwrap_or(false);
                    push(&mut stack, &mut depth, value);
                }
                // The timing bit is "enabled and not done", recomputed from the
                // state this scan's block run left behind. It is not `ET < PT`: a
                // timer that has been reset is idle with `ET = 0`, not timing.
                Op::FbTiming(instance) => {
                    let value = self
                        .blocks
                        .get(instance as usize)
                        .map(|state| state.input && !state.q)
                        .unwrap_or(false);
                    push(&mut stack, &mut depth, value);
                }
                Op::Jump(target) => pc = target as usize,
                Op::JumpIf { target, when } => {
                    // Conditional jumps consume the result bit: the next line
                    // starts a new expression.
                    let value = pop(&stack, &mut depth);
                    if value == when {
                        pc = target as usize;
                    }
                }
            }
        }

        apply_forces(&mut self.next, &self.forces);

        std::mem::swap(&mut self.prev, &mut self.next);
        for slot in 0..self.outputs.len() {
            let value = self
                .prev
                .get(self.outputs[slot].index as usize)
                .copied()
                .unwrap_or(false);
            if let Some(cell) = self.output_image.get_mut(slot) {
                *cell = value;
            }
        }

        self.scan_count = self.scan_count.saturating_add(1);
        self.last_scan_us = started.elapsed().as_micros().min(u128::from(u32::MAX)) as u32;
        ScanStats {
            scan_us: self.last_scan_us,
            scan_count: self.scan_count,
        }
    }

    /// The retained slots and the marker words, ready to store. The I/O regions are
    /// not packed: HAL owns them, and a restored `QW` would be a value nobody
    /// commanded.
    pub fn retain_pack(&self) -> crate::retain::Retained {
        crate::retain::Retained {
            bits: self
                .retained
                .iter()
                .map(|(slot, _)| {
                    (
                        *slot,
                        self.prev.get(*slot as usize).copied().unwrap_or(false),
                    )
                })
                .collect(),
            words: self
                .words
                .iter()
                .enumerate()
                .take(self.program.words.mw as usize)
                .map(|(index, value)| (index as u16, *value))
                .collect(),
        }
    }

    /// Restore values. This writes the two scan images and the marker words; it
    /// never touches forces, so a restore cannot silently force the plant, and
    /// it refuses a slot outside the marker words, so a crafted store cannot
    /// an output.
    pub fn retain_unpack(&mut self, retained: &crate::retain::Retained) {
        for (slot, value) in &retained.bits {
            if let Some(cell) = self.prev.get_mut(*slot as usize) {
                *cell = *value;
            }
            if let Some(cell) = self.next.get_mut(*slot as usize) {
                *cell = *value;
            }
        }
        for (slot, value) in &retained.words {
            if (*slot as usize) >= self.program.words.mw as usize {
                continue;
            }
            if let Some(cell) = self.words.get_mut(*slot as usize) {
                *cell = *value;
            }
        }
    }

    /// The word output region, in compiled order, as produced by the last scan.
    pub fn word_outputs(&self) -> &[i32] {
        let layout = self.program.words;
        let base = layout.qw_base() as usize;
        let end = base
            .saturating_add(layout.qw as usize)
            .min(self.words.len());
        self.words.get(base..end).unwrap_or(&[])
    }

    /// The float output region, in compiled order, as the integer view of the
    /// float pins. The daemon converts each value to `f64` at the HAL boundary.
    pub fn float_outputs(&self) -> &[i32] {
        let layout = self.program.words;
        let base = layout.fo_base() as usize;
        let end = base
            .saturating_add(layout.fo as usize)
            .min(self.words.len());
        self.words.get(base..end).unwrap_or(&[])
    }

    /// Hash of the retained layout, for the store's gate.
    pub fn retain_layout_hash(&self) -> u64 {
        self.layout
    }

    /// Output values in compiled output order, as produced by the last scan.
    pub fn outputs(&self) -> &[bool] {
        &self.output_image
    }

    /// Record the values the field is still driving from the previous program.
    ///
    /// Only a reload uses this: the pins keep their last values across the swap,
    /// so the snapshot between the swap and the first scan reports what the
    /// machine is actually driving instead of the new program's empty image. The
    /// first scan overwrites it. The binding lists match (the pin map is frozen),
    /// so the copy is by position.
    pub fn hold_outputs(&mut self, previous: &[bool]) {
        for (slot, value) in previous.iter().take(self.output_image.len()).enumerate() {
            self.output_image[slot] = *value;
        }
    }

    fn word_value(&self, value: WordOperand) -> i32 {
        match value {
            WordOperand::Word(index) | WordOperand::Slot(index) => self.word(index),
            WordOperand::Const(value) => value,
            WordOperand::Block(instance) => self.block_value(instance),
            WordOperand::BlockPreset(instance) => self
                .blocks
                .get(instance as usize)
                .map(|state| state.preset)
                .unwrap_or(0),
        }
    }

    /// `ET`/`CV` of a block instance.
    pub fn block_value(&self, instance: u16) -> i32 {
        self.blocks
            .get(instance as usize)
            .map(|state| state.value)
            .unwrap_or(0)
    }

    /// `PT`/`PV` of a block instance as last executed. The preset is 32 bits, so
    /// a five-minute `T#5m` reports 300,000 rather than a clamped 32,767.
    pub fn block_preset(&self, instance: u16) -> i32 {
        self.blocks
            .get(instance as usize)
            .map(|state| state.preset)
            .unwrap_or(0)
    }

    /// `Q` of a block instance.
    pub fn block_q(&self, instance: u16) -> bool {
        self.blocks
            .get(instance as usize)
            .map(|state| state.q)
            .unwrap_or(false)
    }

    /// Read a word slot of the bank.
    pub fn word(&self, index: u16) -> i32 {
        self.words.get(index as usize).copied().unwrap_or(0)
    }

    /// The bank slot of a declared word point, by name. Word points do not
    /// follow the marker-word spelling, so a watch entry or a trend needs this
    /// rather than the operand parser.
    pub fn word_slot(&self, name: &str) -> Option<u16> {
        self.word_names
            .iter()
            .find(|(symbol, _)| symbol == name)
            .map(|(_, slot)| *slot)
    }

    /// Write a word slot, for the protocol's force/set and for tests.
    pub fn set_word(&mut self, index: u16, value: i32) {
        if let Some(cell) = self.words.get_mut(index as usize) {
            *cell = value;
        }
    }

    /// Write a compute coil's result: a bank slot, a block's `PT`/`PV`, or a
    /// block's `ET`/`CV`.
    ///
    /// The preset field is the one `Op::Fb` re-asserts from its own source on
    /// the next execution, so a written preset holds until that block runs
    /// again. A preset *sourced from a word* is the way to make the ladder's
    /// value authoritative.
    /// Write one bit of a bank word, keeping the others. The `s32` bit pattern is
    /// the word's value, so a 16-bit variable's upper half is left as the store path
    /// left it.
    fn set_word_bit(&mut self, slot: u16, bit: u8, value: bool) {
        if let Some(cell) = self.words.get_mut(slot as usize) {
            let mask = 1i32 << bit;
            if value {
                *cell |= mask;
            } else {
                *cell &= !mask;
            }
        }
    }

    fn write_word(&mut self, target: WordOperand, value: i32) {
        match target {
            WordOperand::Word(index) | WordOperand::Slot(index) => self.set_word(index, value),
            WordOperand::Block(instance) => {
                if let Some(state) = self.blocks.get_mut(instance as usize) {
                    state.value = value;
                }
            }
            WordOperand::BlockPreset(instance) => {
                if let Some(state) = self.blocks.get_mut(instance as usize) {
                    state.preset = value;
                }
            }
            // Validation refuses a literal target; keep the scan alive.
            WordOperand::Const(_) => {}
        }
    }

    /// The whole word bank, for the snapshot and for tests.
    pub fn words(&self) -> &[i32] {
        &self.words
    }

    /// Words and block members with their names, in a stable order. The marker
    /// words are the marker words; every other slot carries the name its project
    /// gave it, so a watch entry for a word point resolves like any other operand.
    pub fn word_snapshot(&self) -> Vec<WordValue> {
        let mw = self.program.words.mw as usize;
        let mut values = self
            .words
            .iter()
            .enumerate()
            .map(|(index, value)| WordValue {
                name: if index < mw {
                    crate::profile::resolve::marker_word(index as u16)
                } else {
                    self.word_names
                        .iter()
                        .find(|(_, slot)| *slot as usize == index)
                        .map(|(name, _)| name.clone())
                        .unwrap_or_else(|| format!("W{index}"))
                },
                value: *value,
            })
            .collect::<Vec<_>>();
        for (index, state) in self.blocks.iter().enumerate() {
            let kind = match index / usize::from(crate::compile::FB_PER_TYPE) {
                0 => "TON",
                1 => "TOF",
                2 => "TP",
                3 => "CTU",
                _ => "CTD",
            };
            let number = index % usize::from(crate::compile::FB_PER_TYPE);
            let member = if matches!(kind, "CTU" | "CTD") {
                "CV"
            } else {
                "ET"
            };
            values.push(WordValue {
                name: format!("{kind}{number}.{member}"),
                value: state.value,
            });
        }
        values
    }

    pub fn get(&self, name: &str) -> Option<bool> {
        self.slot(name)
            .and_then(|slot| self.prev.get(slot).copied())
    }

    pub fn snapshot(&self) -> Vec<VarValue> {
        self.names
            .iter()
            .enumerate()
            .map(|(slot, (name, kind))| VarValue {
                name: name.clone(),
                value: self.prev.get(slot).copied().unwrap_or(false),
                kind: *kind,
                forced: matches!(self.forces.get(slot), Some(Some(_))),
            })
            .chain(self.block_outputs())
            .collect()
    }

    /// Each block's `Q`, published as `TON0.Q`, so the editor can colour a block by
    /// its real output instead of guessing from the rung.
    fn block_outputs(&self) -> Vec<VarValue> {
        self.blocks
            .iter()
            .enumerate()
            .map(|(index, state)| {
                let kind = match index / usize::from(crate::compile::FB_PER_TYPE) {
                    0 => "TON",
                    1 => "TOF",
                    2 => "TP",
                    3 => "CTU",
                    _ => "CTD",
                };
                let number = index % usize::from(crate::compile::FB_PER_TYPE);
                VarValue {
                    name: format!("{kind}{number}.Q"),
                    value: state.q,
                    kind: SymbolKind::Output,
                    forced: false,
                }
            })
            .collect()
    }

    /// Forces hold across scans until cleared and are applied to both images, so a
    /// forced input or variable is what logic reads and a forced coil wins over
    /// Store, Set and Reset. `Force::ttl_ms` is accepted by the protocol this round
    /// but is not expired here.
    pub fn set_force(&mut self, name: &str, value: bool) -> Result<(), String> {
        match self.slot(name) {
            Some(slot) => {
                self.forces[slot] = Some(value);
                Ok(())
            }
            None => Err(format!("unknown symbol `{name}`")),
        }
    }

    /// Release one operand, leaving the others forced.
    pub fn clear_force(&mut self, name: &str) {
        if let Some(slot) = self.slot(name) {
            self.forces[slot] = None;
        }
    }

    pub fn clear_forces(&mut self) {
        self.forces.fill(None);
    }

    pub fn forced(&self) -> Vec<String> {
        self.names
            .iter()
            .enumerate()
            .filter(|(slot, _)| matches!(self.forces.get(*slot), Some(Some(_))))
            .map(|(_, (name, _))| name.clone())
            .collect()
    }

    fn slot(&self, name: &str) -> Option<usize> {
        self.names.iter().position(|(symbol, _)| symbol == name)
    }
}

fn apply_forces(image: &mut [bool], forces: &[Option<bool>]) {
    for (slot, force) in forces.iter().enumerate() {
        if let Some(value) = force {
            if let Some(cell) = image.get_mut(slot) {
                *cell = *value;
            }
        }
    }
}

/// A standard function block, one record per instance.
#[derive(Debug, Clone, Copy, Default)]
struct FbState {
    input: bool,
    q: bool,
    running: bool,
    /// Rising-edge memory for the member-coil counter's `CU` and `CD` coils.
    /// Separate from `input`, which is the block's own count input.
    up_input: bool,
    down_input: bool,
    /// `ET` of a timer, `CV` of a counter.
    value: i32,
    /// `PT` of a timer, `PV` of a counter, as last executed.
    preset: i32,
}

/// Advance one block and return its `Q`. `dt` is the scan delta in
/// milliseconds; presets are milliseconds for timers and counts for counters.
fn run_block(
    blocks: &mut [FbState],
    instance: usize,
    kind: FbKind,
    input: bool,
    preset: i32,
    dt: u16,
) -> bool {
    let Some(state) = blocks.get_mut(instance) else {
        return false;
    };
    let preset = preset.max(0);
    state.preset = preset;
    let dt = i32::from(dt);
    match kind {
        FbKind::Ton => {
            if input {
                // Elapsed starts at zero on the scan the input is first seen
                // true: the interval before that scan happened before the timer
                // was running, so it is not the timer's to count. A timer with
                // `T#2s` on a 10 ms scan therefore expires on the 201st scan,
                // not the 200th — the same instant the reference engine and a
                // real PLC report.
                if !state.input {
                    state.value = 0;
                } else {
                    state.value = state.value.saturating_add(dt).min(preset);
                }
                state.q = state.value >= preset;
            } else {
                state.value = 0;
                state.q = false;
            }
        }
        FbKind::Tof => {
            if input {
                state.value = 0;
                state.q = true;
                state.running = false;
            } else if state.q {
                // The same rule on the falling edge: the delay is measured from
                // the scan that saw the input go false.
                if state.input {
                    state.value = 0;
                    state.running = true;
                } else {
                    state.value = state.value.saturating_add(dt).min(preset);
                    if state.value >= preset {
                        state.q = false;
                        state.running = false;
                    }
                }
            }
        }
        FbKind::Tp => {
            if input && !state.input {
                state.value = 0;
                state.q = true;
                state.running = true;
            }
            if state.running {
                state.value = state.value.saturating_add(dt).min(preset);
                if state.value >= preset {
                    state.q = false;
                    state.running = false;
                }
            }
        }
        // Counters run through `Op::Counter` and `Op::Count` with the mechanics
        // the compiler baked, so reaching here would be a compiler bug: leave
        // the state alone and report its `Q`.
        FbKind::Ctu | FbKind::Ctd => {}
        FbKind::Rt => state.q = input && !state.input,
        FbKind::Rf => state.q = !input && state.input,
        // Latches run through `run_latch`.
        FbKind::Sr | FbKind::Rs => {}
    }
    state.input = input;
    state.q
}

/// Run a counter. `rule` is data: where the steps come from and what `done`
/// compares. The scan never asks which dialect produced it — Appendix D.
fn run_counter(
    blocks: &mut [FbState],
    instance: usize,
    input: bool,
    preset: i32,
    rule: CounterRule,
) -> bool {
    let Some(state) = blocks.get_mut(instance) else {
        return false;
    };
    let preset = preset.max(0);
    state.preset = preset;
    if rule.steps_on_coils() {
        // Member coils step the value; this execution only re-asserts the
        // preset and decides `done`.
    } else if input && !state.input {
        state.value = state.value.saturating_add(1);
    }
    state.q = match rule.done {
        CounterDone::AtLeastPreset => state.value >= preset,
        CounterDone::AtMostZero => state.value <= 0,
        CounterDone::EqualsPreset => state.value == preset,
    };
    state.input = input;
    state.q
}

/// Step a member-coil counter: on the power bit's rising edge, add or subtract
/// one. `top` is the inclusive upper end of the range; a step past either end
/// wraps to the other, and `None` saturates instead.
fn count_member(blocks: &mut [FbState], instance: usize, up: bool, power: bool, top: Option<i32>) {
    let Some(state) = blocks.get_mut(instance) else {
        return;
    };
    let previous = if up { state.up_input } else { state.down_input };
    if power && !previous {
        let mut value = if up {
            state.value.saturating_add(1)
        } else {
            state.value.saturating_sub(1)
        };
        if let Some(top) = top {
            if value > top {
                value = 0;
            } else if value < 0 {
                value = top;
            }
        }
        state.value = value;
    }
    if up {
        state.up_input = power;
    } else {
        state.down_input = power;
    }
}

/// Set-dominant `SR` and reset-dominant `RS`.
fn run_latch(
    blocks: &mut [FbState],
    instance: usize,
    kind: FbKind,
    set: bool,
    reset: bool,
) -> bool {
    let Some(state) = blocks.get_mut(instance) else {
        return false;
    };
    state.q = match kind {
        FbKind::Rs => !reset && (set || state.q),
        _ => set || (state.q && !reset),
    };
    state.q
}

fn push_word(stack: &mut [i32; STACK_DEPTH], depth: &mut usize, value: i32) {
    if *depth < STACK_DEPTH {
        stack[*depth] = value;
        *depth += 1;
    }
}

fn pop_word(stack: &[i32; STACK_DEPTH], depth: &mut usize) -> i32 {
    if *depth == 0 {
        return 0;
    }
    *depth -= 1;
    stack[*depth]
}

fn bin_word(stack: &mut [i32; STACK_DEPTH], depth: &mut usize, op: fn(i32, i32) -> i32) {
    let right = pop_word(stack, depth);
    let left = pop_word(stack, depth);
    push_word(stack, depth, op(left, right));
}

fn cmp_word(
    bits: &mut [bool; STACK_DEPTH],
    bit_depth: &mut usize,
    words: &mut [i32; STACK_DEPTH],
    word_depth: &mut usize,
    op: fn(i32, i32) -> bool,
) {
    let right = pop_word(words, word_depth);
    let left = pop_word(words, word_depth);
    push(bits, bit_depth, op(left, right));
}

fn push(stack: &mut [bool; STACK_DEPTH], depth: &mut usize, value: bool) {
    if *depth < STACK_DEPTH {
        stack[*depth] = value;
        *depth += 1;
    }
}

fn pop(stack: &[bool; STACK_DEPTH], depth: &mut usize) -> bool {
    if *depth == 0 {
        return false;
    }
    *depth -= 1;
    stack[*depth]
}
