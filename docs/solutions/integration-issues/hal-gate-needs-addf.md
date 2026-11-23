---
title: A HAL interlock fails silently unless it is also added to a thread
date: 2026-11-23
category: integration-issues
module: Program interlock (cycle start)
problem_type: integration_issue
component: api_layer
symptoms:
  - "Cycle start does nothing with a clean program loaded, idle in AUTO, no alarm and no blocker"
  - "halui.program.run stays FALSE however often the panel key is pressed"
  - "The panel service records the press, so the key itself is not in doubt"
  - "and2.<gate>.time reads 0 while every neighbouring and2 counter advances"
root_cause: missing_workflow_step
resolution_type: code_fix
severity: high
related_components: ["HAL wiring", "LinuxCNC backend", "worker loop", "panel service"]
tags: [hal, addf, threads, interlock, halui, cycle-start, silent-failure]
---

# A HAL interlock fails silently unless it is also added to a thread

## Problem

A program interlock was added to gate cycle start: the HMI publishes
`linc.program-ok`, and `and2.cycle_permit` ANDs it with the panel's run request
before `halui.program.run`. With a clean program loaded and the interlock high,
cycle start did nothing. Nothing was logged, no alarm was raised, and
`linc_why_blocked` reported no blockers — every visible signal said the machine
was ready.

It took two independent defects, each sufficient on its own, both silent:

1. `MachineCommand::SetHalOutput { output: ProgramOk }` was grouped with the
   outputs the backend worker publishes and dropped in that arm. The worker did
   not know about it, so the pin was never written and kept its HAL default.
2. The gate instance was created with `loadrt and2 names=…`, netted, and never
   added to a thread with `addf`, so its output never evaluated.

## Symptoms

- `halcmd show pin linc.program-ok` — TRUE after the first fix, and still no run.
- `halcmd show pin and2.cycle_permit`:

  ```text
  IN   TRUE   and2.cycle_permit.in1 <== linc-program-ok
  OUT  FALSE  and2.cycle_permit.out ==> cycle-permit
  s32  0      and2.cycle_permit.time
  ```

  The `.time` counter is the tell: `and2.cycle_gate.time` read 114 and
  `and2.cycle_run.time` 38, so those functions were being called. A counter that
  never advances names a function nothing calls.
- `linc_get_panel_events` showed `buttons 0x0 -> 0x4000 -> 0x0` (bit 14,
  CYCLE_START) on every press, so the key reached the panel service and the
  press path was never the problem.
- `linc_get_events` showed no `interp` or `program` transition: the interpreter
  stayed Idle through every press, so halui never received a run request.

## What Didn't Work

- Reading the pin's *value*. TRUE/FALSE says what a function last computed, not
  whether anything calls it. Only `.time` answers that.
- Fixing the write path and stopping there. `linc.program-ok` was then TRUE and
  the gate's `in1` was TRUE, but a gate that is never evaluated cannot pass
  anything on — the second defect hid behind the first.
- Trusting the sim. `linc_core::sim` implements `SetHalOutput{ProgramOk}` and
  stores the level, so every UI test and `--check-config` were satisfied while
  the LinuxCNC backend dropped the write.
- Blaming single-block. The panel turns a CYCLE START press into a *step* when
  the SBK LED is on (`sim.rs` `step_active`), which is a real silent no-op — but
  `cnc_panel.led_sbk` was FALSE, so it was not this one. Check it before
  discounting the key.

## Solution

Two fixes, each with a test behind it:

- `crates/linc-core/src/backend.rs` publishes `ProgramOk` from
  `snapshot.hal.program_ok` on change, first pass included, exactly as it
  publishes `net-safe`. `crates/linc-linuxcnc/src/lib.rs` mirrors the HMI's
  level into the snapshot instead of dropping it, and `carry_hal_selections`
  keeps it across a rebuilt snapshot.
- `addf and2.cycle_permit servo-thread` and `addf and2.resume_permit
  servo-thread` in `hal/linc.hal`, plus the same line for the gate in
  `hal/linc-lathe.hal` and `hal/baiermill-pb-postgui.hal`.

`crates/linc-core/tests/program_gate_hal.rs` now carries both checks:
`every_and2_these_files_create_is_added_to_a_thread` parses each `loadrt and2
names=` list and requires every instance in an `addf` (mutation-checked: delete
the line and the test fails), and the existing gate tests require the interlock
to reach `halui.program.run`.

## Why This Works

A HAL function is only called when it is in a thread's function list. `loadrt`
creates the instance, `net` connects its pins, `addf` is what schedules it — and
HAL accepts all three separately, so a missing `addf` produces no error at load
time. Its output simply stays at the value it was initialised to, for ever,
which for an AND gate is FALSE.

## Prevention

- Any new HAL function gets three lines, never two: `loadrt … names=<instance>`,
  the `net` that uses it, and `addf <instance> <thread>`.
- Prove it with the counter: `halcmd show pin and2.<name>` and watch `.time`
  advance, or `halcmd show thread | grep <instance>`. A gate whose `.time` is 0
  is not a gate.
- Every `LincOutput` variant a UI path writes must be published by something —
  by the writer call itself, or by the backend loop reading
  `snapshot.hal.<field>`. A variant parked in a "the worker owns these" arm that
  no worker handles is dropped without a trace.
- Do not let the simulation backend stand as the only evidence that a HAL output
  works: it stores levels the real backend has to publish. `linc_get_hal` reports
  `program_ok` either way, so a green sim run is not proof.
- When cycle start is inert, read the chain, not the state: `halui.mode.is-auto`,
  `halui.program.is-paused`, `and2.cycle_gate`, `and2.cycle_run`,
  `and2.cycle_permit`, `halui.program.run` — and the `.time` counter of each
  function in it.
