//! Deterministic scan VM. `scan` allocates nothing, takes no locks and does no I/O.
//!
//! Image rules: `next` starts as a copy of `prev`; inputs are written into both
//! images so logic sees the live input while coil writes only reach `next` and
//! therefore stay one scan behind. Forces are applied to both images before the
//! ops pass (a forced input or variable is what logic reads) and again to `next`
//! after it, so a forced coil wins over Store, Set and Reset.

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::compile::{Binding, Compiled, FbKind, MathOp, Op, Program, SymbolKind, WordOperand};

/// Operand-stack depth. Validation caps logic nesting at 64, so this is slack.
const STACK_DEPTH: usize = 128;

/// One 20 ms tick, ZX-PLC's scan period.
pub const DEFAULT_DT_MS: u16 = 20;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanStats {
    pub scan_us: u32,
    pub scan_count: u64,
}

/// One word of the bank in a snapshot: `MW3`, or a block member `TON0.ET`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WordValue {
    pub name: String,
    pub value: i16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VarValue {
    pub name: String,
    pub value: bool,
    pub kind: SymbolKind,
    pub forced: bool,
}

#[derive(Debug, Clone)]
pub struct Vm {
    program: Program,
    names: Vec<(String, SymbolKind)>,
    inputs: Vec<Binding>,
    outputs: Vec<Binding>,
    prev: Vec<bool>,
    next: Vec<bool>,
    words: Vec<i16>,
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
        let slots = compiled.symbols.len();
        let retained = compiled
            .symbols
            .iter()
            .filter(|symbol| symbol.retain)
            .map(|symbol| (symbol.index, symbol.name.clone()))
            .collect::<Vec<_>>();
        let layout = crate::retain::layout_hash(
            &retained
                .iter()
                .map(|(index, name)| (*index, name.as_str()))
                .collect::<Vec<_>>(),
            compiled.program.word_count,
        );
        Self {
            program: compiled.program.clone(),
            names: compiled
                .symbols
                .iter()
                .map(|symbol| (symbol.name.clone(), symbol.kind))
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
    /// tick, which is what ZX-PLC measures in and what deterministic tests use.
    pub fn scan(&mut self, inputs: &[bool]) -> ScanStats {
        self.scan_with_dt(inputs, DEFAULT_DT_MS)
    }

    pub fn scan_with_dt(&mut self, inputs: &[bool], dt_ms: u16) -> ScanStats {
        let started = Instant::now();
        self.dt_ms = dt_ms.max(1);
        self.next.copy_from_slice(&self.prev);

        for slot in 0..self.inputs.len() {
            if let Some(value) = inputs.get(slot) {
                let index = self.inputs[slot].index as usize;
                if let Some(cell) = self.next.get_mut(index) {
                    *cell = *value;
                }
                if let Some(cell) = self.prev.get_mut(index) {
                    *cell = *value;
                }
            }
        }

        // Both images, as documented: a function-block line reads `next`, so a
        // force that only reached `prev` would be invisible to it.
        apply_forces(&mut self.prev, &self.forces);
        apply_forces(&mut self.next, &self.forces);

        let mut stack = [false; STACK_DEPTH];
        let mut depth = 0usize;
        let mut word_stack = [0i16; STACK_DEPTH];
        let mut word_depth = 0usize;
        for op_index in 0..self.program.ops.len() {
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
                    if let Some(cell) = self.next.get_mut(index as usize) {
                        *cell = value;
                    }
                }
                Op::Set(index) => {
                    if pop(&stack, &mut depth) {
                        if let Some(cell) = self.next.get_mut(index as usize) {
                            *cell = true;
                        }
                    }
                }
                Op::Reset(index) => {
                    if pop(&stack, &mut depth) {
                        if let Some(cell) = self.next.get_mut(index as usize) {
                            *cell = false;
                        }
                    }
                }
                Op::LoadWord(index) => {
                    let value = self.words.get(index as usize).copied().unwrap_or(0);
                    push_word(&mut word_stack, &mut word_depth, value);
                }
                Op::LoadConst(value) => push_word(&mut word_stack, &mut word_depth, value),
                Op::StoreWord(index) => {
                    let value = pop_word(&word_stack, &mut word_depth);
                    if let Some(cell) = self.words.get_mut(index as usize) {
                        *cell = value;
                    }
                }
                Op::Add => bin_word(&mut word_stack, &mut word_depth, i16::wrapping_add),
                Op::Sub => bin_word(&mut word_stack, &mut word_depth, i16::wrapping_sub),
                Op::Mul => bin_word(&mut word_stack, &mut word_depth, i16::wrapping_mul),
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
                Op::FbLoad(instance) => {
                    let power = pop(&stack, &mut depth);
                    if power {
                        if let Some(state) = self.blocks.get_mut(instance as usize) {
                            state.value = state.preset;
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
                        self.set_word(target, result);
                    }
                }
                Op::FbQ(instance) => {
                    let value = self
                        .blocks
                        .get(instance as usize)
                        .map(|state| state.q)
                        .unwrap_or(false);
                    push(&mut stack, &mut depth, value);
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

    /// The retained slots and the whole word bank, ready to store.
    pub fn retain_pack(&self) -> crate::retain::Retained {
        crate::retain::Retained {
            bits: self
                .retained
                .iter()
                .map(|(slot, _)| (*slot, self.prev.get(*slot as usize).copied().unwrap_or(false)))
                .collect(),
            words: self
                .words
                .iter()
                .enumerate()
                .map(|(index, value)| (index as u16, *value))
                .collect(),
        }
    }

    /// Restore values. This writes the two scan images and the word bank; it
    /// never touches forces, so a restore cannot silently force the plant.
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
            if let Some(cell) = self.words.get_mut(*slot as usize) {
                *cell = *value;
            }
        }
    }

    /// Hash of the retained layout, for the store's gate.
    pub fn retain_layout_hash(&self) -> u64 {
        self.layout
    }

    /// Output values in compiled output order, as produced by the last scan.
    pub fn outputs(&self) -> &[bool] {
        &self.output_image
    }

    fn word_value(&self, value: WordOperand) -> i16 {
        match value {
            WordOperand::Word(index) => self.word(index),
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
    pub fn block_value(&self, instance: u16) -> i16 {
        self.blocks
            .get(instance as usize)
            .map(|state| state.value)
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
    pub fn word(&self, index: u16) -> i16 {
        self.words.get(index as usize).copied().unwrap_or(0)
    }

    /// Write a word slot, for the protocol's force/set and for tests.
    pub fn set_word(&mut self, index: u16, value: i16) {
        if let Some(cell) = self.words.get_mut(index as usize) {
            *cell = value;
        }
    }

    /// The whole word bank, for the snapshot.
    pub fn words(&self) -> &[i16] {
        &self.words
    }

    /// Words and block members with their names, in a stable order.
    pub fn word_snapshot(&self) -> Vec<WordValue> {
        let mut values = self
            .words
            .iter()
            .enumerate()
            .map(|(index, value)| WordValue {
                name: format!("MW{index}"),
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
    /// `ET` of a timer, `CV` of a counter.
    value: i16,
    /// `PT` of a timer, `PV` of a counter, as last executed.
    preset: i16,
}

/// Advance one block and return its `Q`. `dt` is the scan delta in
/// milliseconds; presets are milliseconds for timers and counts for counters.
fn run_block(
    blocks: &mut [FbState],
    instance: usize,
    kind: FbKind,
    input: bool,
    preset: i16,
    dt: u16,
) -> bool {
    let Some(state) = blocks.get_mut(instance) else {
        return false;
    };
    let preset = preset.max(0);
    state.preset = preset;
    let dt = i16::try_from(dt).unwrap_or(i16::MAX);
    match kind {
        FbKind::Ton => {
            if input {
                state.value = state.value.saturating_add(dt).min(preset);
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
                state.value = state.value.saturating_add(dt).min(preset);
                state.running = true;
                if state.value >= preset {
                    state.q = false;
                    state.running = false;
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
        FbKind::Ctu => {
            if input && !state.input {
                state.value = state.value.saturating_add(1);
            }
            state.q = state.value >= preset;
        }
        FbKind::Ctd => {
            if input && !state.input {
                state.value = state.value.saturating_sub(1);
            }
            state.q = state.value <= 0;
        }
        FbKind::Rt => state.q = input && !state.input,
        FbKind::Rf => state.q = !input && state.input,
        // Latches run through `run_latch`.
        FbKind::Sr | FbKind::Rs => {}
    }
    state.input = input;
    state.q
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

fn push_word(stack: &mut [i16; STACK_DEPTH], depth: &mut usize, value: i16) {
    if *depth < STACK_DEPTH {
        stack[*depth] = value;
        *depth += 1;
    }
}

fn pop_word(stack: &[i16; STACK_DEPTH], depth: &mut usize) -> i16 {
    if *depth == 0 {
        return 0;
    }
    *depth -= 1;
    stack[*depth]
}

fn bin_word(stack: &mut [i16; STACK_DEPTH], depth: &mut usize, op: fn(i16, i16) -> i16) {
    let right = pop_word(stack, depth);
    let left = pop_word(stack, depth);
    push_word(stack, depth, op(left, right));
}

fn cmp_word(
    bits: &mut [bool; STACK_DEPTH],
    bit_depth: &mut usize,
    words: &mut [i16; STACK_DEPTH],
    word_depth: &mut usize,
    op: fn(i16, i16) -> bool,
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
