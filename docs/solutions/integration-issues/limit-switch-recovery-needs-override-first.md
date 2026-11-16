---
title: Recovery from a hard limit needs the override armed before motion is enabled
date: 2026-11-12
category: integration-issues
module: Limit-switch recovery
problem_type: integration_issue
component: api_layer
symptoms:
  - "A joint on a hard limit stays disabled after the switch is cleared by a jog command"
  - "Enabling motion first re-faults on the next servo cycle"
  - "The simulated cabinet reports `cabinet_on = false` and recovery refuses with CABINET IS OFF"
root_cause: missing_workflow_step
resolution_type: code_fix
severity: high
related_components: ["LinuxCNC backend", "sim HAL", "UI dialog"]
tags: [linuxcnc, hard-limits, override, nml, halui, recovery]
---

# Recovery from a hard limit needs the override armed before motion is enabled

## Problem

An unhomed machine has no soft limits, so a jog can reach a hard limit. LinuxCNC
then sets the joint error flag, disables motion and reports `ESTOP_RESET`.
`EMC_JOINT_HOME` is dropped while motion is disabled, the cabinet machine-on
switch is a maintained latch that cannot produce the rising edge `halui` needs,
and `HOME` is therefore dead too. The HMI is the only path back.

## Symptoms

- The task state reads `ESTOP_RESET`; `motion.enabled` is false and
  `halui.machine.is-on` is false, while `linc.machine-on` stays true.
- `HOME X` and `HOME ALL` do nothing.
- Pressing POWER OFF and ON is not a recovery: the falling edge ends the LiNC
  session and powers off the PC.

## What Didn't Work

- Enabling motion before arming the override. `check_for_faults()` runs every
  servo cycle and the joint is still on the switch, so it disables motion again
  immediately.
- A joint-only jog on a homed machine. A homed machine sits in TELEOP, where the
  jog index is the axis letter (X=0, Y=1, Z=2), not the INI axis-list position;
  the two differ on a lathe (`[X, Z]`).
- Waiting for the limit to clear before re-arming. LinuxCNC clears the override
  mask when the jog it allowed ends, so each bounded step needs its own arm and
  enable.

## Solution

`linc-core::recovery` owns the step (5 mm at 5 mm/s, at most four steps), the
sign (away from the tripped side), and the gate order. `RecoverLimit { axis }`
carries no direction, distance or speed: the backend refreshes the controller,
derives the side from `JointStatus.min_hard_limit` / `max_hard_limit`, maps the
axis to its joint, arms `EMC_JOINT_OVERRIDE_LIMITS`, enables motion through
`EMC_TASK_SET_STATE(ON)`, polls `motion.enabled`, then issues one
`EMC_JOG_INCR` in the index space the current trajectory mode requires. The
dialog repeats that per step and stops at the cap.

The simulated post-GUI file must net `sig_rio_machine-on-switch_bit` to
`linc.machine-on` exactly as the real machine does. Without it the pin exists
but is unconnected, reads false, and the cabinet gate refuses every recovery.

## Why This Works

The override mask is what lets `check_for_faults()` ignore the tripped limit, so
it must be in motion before the enable. A bounded increment has no dead-man
problem: a stalled HMI leaves the axis parked after the current 5 mm, and the
override is cleared by LinuxCNC when that move ends. Deriving the side and the
index space in the backend means a caller cannot drive the machine into the
switch, and the four-step cap bounds a welded switch to 20 mm.

## Prevention

- Keep the order arm → wait → enable → verify → jog in one backend helper; do
  not let a UI path send the NML commands itself.
- Use `JointStatus.override_limits` per joint, not
  `halui.joint.0.override-limits`, which describes joint 0 only.
- Run `tools/check-hal-nets.py` after touching HAL wiring, and verify the
  simulated `linc.machine-on` pin is true before testing recovery in the sim.
