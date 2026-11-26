---
title: A G92 shift belongs on the work-origin marker, not on the tool head
date: 2026-11-26
category: ui-bugs
module: toolpath view, travel check and preview frame (linc-ui, linc-view, sim)
problem_type: ui_bug
component: frontend
symptoms:
  - "The tool head is drawn a whole EXT shift away from the part it is cutting"
  - "The work-origin cross stays at the unshifted place while a G92 is in effect"
  - "The out-of-travel red and the travel check stay in the frame before the shift"
root_cause: logic_error
resolution_type: code_fix
severity: high
related_components: ["linc-view", "linc-ui shell", "offsets page", "sim backend"]
tags: [g92, work-offsets, preview, toolpath, travel-limits, frame]
---

# A G92 shift belongs on the work-origin marker, not on the tool head

## Problem

The toolpath draws three things in one frame: the parsed program path, the live
tool tip with its trail, and the work-origin cross. The frame is machine space
with the work offset applied — `work + work offset + G92 + tool length` is what
the interpreter reports as the machine position, and the preview adds the work
offset and the `G92` register to every program word.

The trail is corrected into that frame by `preview offset + work - machine`. That
difference is the tool length `G43` adds, which the preview cannot know — until
the `G92` register was added to the reported work position, the same expression
picked the register up as well. The drawn head then sat a whole shift away from
the part, and the cross, computed from the offset row alone, stayed where the
frame used to be.

## Symptoms

- With `EXT X 12.5` set, the tool marker and the live trail were drawn 12.5 mm
  off the program path, in the direction the part had moved.
- The work-origin cross did not move at all, so the picture read as "the head
  moved" rather than "the frame moved".
- The travel check and the red out-of-travel paint used a preview parsed against
  the register the controller held when the preview was last built. A shift the
  controller set on its own — a probe macro's `G92`, an MDI `G92`, a value
  restored at startup — never reached the parse, because nothing in the offset
  rows or their revision changed.

## What Didn't Work

- **Correcting the head by the register.** The register is already inside
  `work`, so subtracting it again was the defect, not the fix. The invariant
  that settles it is that the head must land on the program path when the
  interpreter is at the start of the block it is cutting.
- **Reading the register per frame instead of per parse.** The preview is parsed
  once and rebuilt on a trigger. Comparing the register only while drawing fixes
  the paint and leaves the travel check and the interlock answer on the old
  frame.
- **Re-seeding on every register change.** A running program that executes `G92`
  moves the live register itself, and the parse already models that block.
  Re-seeding from the live value applies the program's own shift twice to the
  part of the file that ran before it.

## Solution

Three statements, one per surface:

```rust
// The frame, from the live snapshot: the row plus the register.
pub fn work_origin(snapshot: &MachineSnapshot) -> Option<[f64; 3]>

// The head: the register comes back out of the difference, so only the tool
// length is left.
offset + work - machine + register

// The parse: the register is a rebuild trigger, from an idle interpreter only.
!self.program_active() && register != doc.preview_register
```

The corner dimension labels take the register off as part of `work_offset`, the
offsets page's selected-row cross adds it, the MDI block overlay is parsed with
it, and the simulator adds and subtracts it on the same sides as the controller.

## Why This Works

A `G92` is a statement about the frame, not about the tip: the machine does not
move, the coordinate the tip reads changes. So the origin marker moves and the
head stays where the controller reports it. Once the preview is parsed in the
shifted frame, everything derived from it — the drawn path, the red
out-of-travel split, the `PROGRAM OUTSIDE MACHINE LIMITS` banner and the cycle
start interlock — follows without its own copy of the arithmetic.

The idle gate on the rebuild is what keeps the program's own `G92` from being
counted twice: while a program runs, its register changes are the program's, and
the parse already holds them.

## Prevention

- Tests that read the frame rather than the number: the head lands on the path
  under a shift, the head takes only the tool length out when both a shift and a
  tool length are in play, the origin marker carries the register, and a program
  that only leaves the envelope *because* of the shift is refused where the
  unshifted one is not.
- A register change is exercised through the backend, not by editing the shell's
  snapshot: the write path and the poll path disagreeing is exactly how a
  controller-side shift went unnoticed.
- `CODING_STANDARDS.md` (G-code) states both conditions, so a reviewer sees a
  shift subtracted from a live position, or a frame without a rebuild trigger,
  and asks why.
