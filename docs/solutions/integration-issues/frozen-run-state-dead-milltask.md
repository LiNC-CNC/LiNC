---
title: A frozen RUN state means the controller task died, not a progress bug
date: 2026-11-26
category: integration-issues
module: LinuxCNC backend status
problem_type: integration_issue
component: api_layer
symptoms:
  - "The run row and bottom bar keep saying RUNNING at 94% long after the last block"
  - "The STATE cell says RUN and the line counter never leaves the last executed line"
  - "linuxcnc.stat() returns the same interp_state, exec_state and distance_to_go on every poll"
  - "halui.program.is-running stays TRUE while nothing moves"
root_cause: backend_process_death
resolution_type: environment_setup
severity: high
related_components: ["NML status", "milltask", "HAL pins", "run row", "progress display"]
tags: [linuxcnc, milltask, sigbus, nml, stale-status, progress, coredumpctl, diagnostics]
---

# A frozen RUN state means the controller task died, not a progress bug

## Problem
After a simulated program finished, the HMI still showed `RUNNING` at 94% and the
top bar still showed `RUN`. The program was over; the controller was not. LinuxCNC's
`milltask` had died, and the NML status buffer kept serving its last values, so every
surface that reads the buffer repeated them.

## Symptoms
- The run row above the editor said `RUNNING` and the bar sat at 94%; the bottom bar
  showed the same percent.
- The line counter stuck at the last block (`34/36`), with the block text frozen.
- Repeated `linuxcnc.stat()` calls returned identical values: `interp_state` `WAITING`,
  `exec_state` `EXEC_WAITING_FOR_MOTION_AND_IO`, a non-zero `distance_to_go`, and the
  same joint feedback on every poll.
- `halui.program.is-running` was `TRUE`, `halui.program.is-idle` was `FALSE`, and
  `linc.program-ok` was `TRUE` — the same last-written HAL values.

## What Didn't Work
- **Reading the run-state logic for a bug.** `snapshot.task.running` is derived from the task
  mode, `interp_state` and `exec_state` in `crates/linc-linuxcnc/src/snapshot.rs`; the
  100% reading comes from `ProgramUi::progress` in `crates/linc-ui/src/state.rs`, which
  returns `1.0` for a run `on_run_finished` marked complete and otherwise passes the
  controller's ratio through (34/36 here). Both are correct, and
  `on_run_finished` runs only on a true-to-false transition of `task.running`. A frozen
  status never produces that transition, so the display had nothing to react to.
- **Polling `linuxcnc.stat()`.** The fields looked alive because they had plausible
  values. Only comparing two polls showed they were frozen.
- **Trusting HAL pins.** `halui.program.is-running`, `motion.in-position` and
  `spindle.0.on` are computed by LinuxCNC userspace from the same NML data, so they
  repeat the last state just as faithfully.
- **Suspecting the newly added end-of-program percent.** That path needs the stop
  transition too; it had not run.

## Solution
Check the task process before changing any display code:

```bash
pgrep -x milltask        # empty
coredumpctl list milltask
```

`coredumpctl info milltask` showed the process died with `Signal: 7 (BUS)` and this
stack:

```text
test_and_set_bit (libtooldata.so.0)
tooldata_get (libtooldata.so.0)
EMC_TOOL_STAT::operator=
EMC_IO_STAT::operator=
main
```

Two cores existed five minutes apart. The HMI log had recorded no command near either
crash; the crash is inside LinuxCNC's tool-data mmap handling, not in LiNC code.

Restart the controller and rerun the same program:

```bash
./scripts/stop-sim.sh
./scripts/start-sim.sh --detach --release-estop --power-on --home --load --float
```

The rerun kept `milltask` alive to the end, completed in 90 seconds, and the HMI then
showed `STOP`, a full bar and `100%` — the completion path works once the controller
publishes the stop.

## Why This Works
The NML status is a shared buffer that `milltask` and `io` publish; LiNC polls it and
renders it. There is no heartbeat on that publisher in the HMI (the panel link has its
own heartbeat, and the HMI's own `linc.hmi-alive` output says the HMI is alive, but
neither observes the controller). An NML *read error* is flagged as stale, yet a dead
publisher behind a healthy buffer is invisible. When the publisher dies, the buffer
keeps its last contents and every reader — LiNC, HAL pins, `linuxcnc.stat()` — sees the
same frozen values. No amount of display logic can distinguish "still running" from
"nobody is writing" without a liveness signal, so the process check is the decisive
test.

This case also explains why `linc.program-ok` stayed `TRUE`: it is an output of the
HMI's own gating, not a statement that the task is alive.

## Prevention
- When the state says running but the machine looks finished, check the publisher
  first: `pgrep -x milltask`, then `coredumpctl info milltask`. Treat two identical
  status polls with no motion as the tell.
- Keep the core: `coredumpctl info milltask` names the faulting library and stack, which
  is what separates an upstream crash from an HMI bug.
- Do not make completion depend on a stale buffer. A future liveness check could flag
  "controller not responding" when the snapshot stops changing while `task.running` is
  true; that is a product change, not a display-math fix, and it should surface a fault
  rather than silently marking the program complete.
- If the tool-data mmap crash recurs, record the stack and the LinuxCNC version before
  restarting. The crash is in `libtooldata`/`~/.tool.mmap`, shared by `io` and
  `milltask`; LiNC does not write that file.

## Related Issues
- [Saving a program must reopen it, or AUTO runs the pre-edit text](linuxcnc-save-must-reopen-program.md)
  — the same theme of the HMI trusting a copy while LinuxCNC holds its own state.
- [Recovery from a hard limit needs the override armed before motion is enabled](limit-switch-recovery-needs-override-first.md)
  — another case where a status field does not match the machine.
- [A HAL interlock fails silently unless it is also added to a thread](hal-gate-needs-addf.md)
  — a silent interlock that looks wired but never runs.
