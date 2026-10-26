//! Deterministic scan VM. `scan` allocates nothing, takes no locks and does no I/O.
//!
//! Image rules: `next` starts as a copy of `prev`; inputs are written into both
//! images so logic sees the live input while coil writes only reach `next` and
//! therefore stay one scan behind. Forces are applied to both images before the
//! ops pass (a forced input or variable is what logic reads) and again to `next`
//! after it, so a forced coil wins over Store, Set and Reset.

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::compile::{Binding, Compiled, Op, Program, SymbolKind};

/// Operand-stack depth. Validation caps logic nesting at 64, so this is slack.
const STACK_DEPTH: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanStats {
    pub scan_us: u32,
    pub scan_count: u64,
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
    output_image: Vec<bool>,
    forces: Vec<Option<bool>>,
    scan_count: u64,
    last_scan_us: u32,
}

impl Vm {
    pub fn new(compiled: &Compiled) -> Self {
        let slots = compiled.symbols.len();
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
        self.output_image.fill(false);
        self.forces.fill(None);
    }

    pub fn scan(&mut self, inputs: &[bool]) -> ScanStats {
        let started = Instant::now();
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

        apply_forces(&mut self.prev, &self.forces);

        let mut stack = [false; STACK_DEPTH];
        let mut depth = 0usize;
        for op_index in 0..self.program.ops.len() {
            match self.program.ops[op_index] {
                Op::PushConst(value) => push(&mut stack, &mut depth, value),
                Op::Push(index) => {
                    let value = self.prev.get(index as usize).copied().unwrap_or(false);
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

    /// Output values in compiled output order, as produced by the last scan.
    pub fn outputs(&self) -> &[bool] {
        &self.output_image
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
