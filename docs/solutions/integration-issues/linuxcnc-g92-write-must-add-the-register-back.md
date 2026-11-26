---
title: A G92 write must add the register back before subtracting, or every edit compounds
date: 2026-11-26
category: integration-issues
module: EXT offset write path (LinuxCNC backend and simulator)
problem_type: integration_issue
component: api_layer
symptoms:
  - "Typing 12.5 into the EXT card twice leaves a shift of 25.0"
  - "Typing 0 clears nothing, because the register keeps a shift equal to the position"
  - "The DRO agrees with the controller while the part sits 12.5 mm away from where the operator put it"
  - "The simulator and the LinuxCNC backend disagree on the same typed value"
root_cause: wrong_arithmetic
resolution_type: code_fix
severity: high
related_components: ["sim backend", "preview (G92 and G52)", "offsets page"]
tags: [linuxcnc, g92, g52, work-offsets, rs274, interpreter, silent-failure]
---

# A G92 write must add the register back before subtracting, or every edit compounds

## Problem

Setting the global EXT shift (`G92`) from a typed offset needs a coordinate the
interpreter accepts, not the offset itself: `G92` makes the current point read a
value and derives the register from that. The derivation used the displayed work
position and subtracted the wanted offset, which is wrong by the register
already in effect. Every edit therefore added to the last one.

## Symptoms

- Typing 12.5 twice produced 25.0. The DRO moved by 12.5 each time, so nothing
  looked broken until the part was cut in the wrong place.
- Typing 0 did not clear an axis; it left a shift equal to the position at the
  moment of the edit.
- The simulator landed on the typed value while the LinuxCNC backend landed on
  the sum, because the simulator assigned the register directly.

## What Didn't Work

- **Reasoning from the display.** `work = machine − WCS − G92 − tool` makes it
  look as if the displayed work position already carries the register, so
  `word = work − wanted` reads as correct. It is correct only while the register
  is zero, which is the case every existing test happened to exercise.
- **Two independent reviews, both loud and both wrong.** Coherence and
  feasibility each flagged the derivation, and one of them supplied a formula
  that would have broken the working case. A claimed interpreter fact is worth
  nothing until the interpreter is asked.

## Solution

Ask `rs274`, which runs the real interpreter with no machine attached:

```console
$ printf 'G10 L2 P1 X0 Y0 Z0\nG92 X5\nG92 X10\nM2\n' | rs274 -g /dev/stdin
SET_G92_OFFSET(-5.0000, ...)    # G92 X5
SET_G92_OFFSET(-10.0000, ...)   # G92 X10, no motion in between
```

If the register in effect had been read, the second block would have left −5:
the point read 5 at that moment. It left −10, the position the machine holds.
The interpreter derives a new register from the position **without** the
existing one.

So the word carries the register back:

```rust
fn g92_word(work: f64, register: f64, wanted: f64) -> f64 {
    work + register - wanted
}
```

and the write reads both from one status, then re-reads the register after the
interpreter has run. The simulator is the same statement in another form: it
stores `wanted` and moves the readout by `old − new`.

## Why This Works

The register is an offset the display has already applied, and `G92` is a
statement about the machine point, not about the display. Adding the register
back reconstructs the frame the interpreter derives in, so a second edit lands
on the typed number instead of on top of the first. It also decides the other
cases for free: 0 clears the axis (`word = work + register`), and an edit that
repeats the value already shown is a no-op.

The same mistake existed in the preview, which derives the frame it draws from
a `G92` block. It is invisible at register zero and puts the drawn path 12.5 mm
from the cut once a shift is in effect.

## Prevention

- A test that edits an axis already holding a shift, in both the simulator and
  the backend helper, and asserts the register equals the typed value. A test
  that only edits from zero cannot see this defect.
- Any arithmetic that touches an offset, a register, or a frame is settled with
  `rs274 -g` before it is written, not by reading the documentation's prose or
  by review.
- `CODING_STANDARDS.md` (Controller boundary) states the condition, so a
  reviewer sees `work − wanted` in a diff and asks where the register went.
