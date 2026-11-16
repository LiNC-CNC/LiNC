# Limit-switch recovery

Status: implemented and verified end to end in the LinuxCNC simulation. Unit
and UI tests pass. The run found one wiring bug, now fixed: the simulated
post-GUI file never netted `sig_rio_machine-on-switch_bit` to `linc.machine-on`,
so `cabinet_on` read FALSE and every recovery would have been refused.

## 1. Problem

An unhomed machine has no soft limits, so a jog can reach a hard limit switch.
LinuxCNC then does three things:

1. `check_for_faults()` sets the joint error flag and `emcmotInternal->enabling = 0`
   (`src/emc/motion/control.c:811-820`, 2.9.10).
2. `set_operating_mode()` disables motion, clears the joint enable, cancels homing
   and clears `MOTION_ENABLE`.
3. The reported task state becomes `ESTOP_RESET`, because `determineState()`
   derives it from `motion.traj.enabled` (`src/emc/task/emctask.cc:419-430`).

Recovery needs a fresh `EMC_TASK_SET_STATE(ON)`, which reaches motion only as
`EMCMOT_ENABLE`. On this machine there are exactly two possible sources:

- the cabinet machine-on switch, through `halui.machine.on` — a **level** input
  that halui samples only on a rising edge (`halui.cc:1652-1654`). The switch is
  a maintained latch (ON latches, OFF latches, no momentary pulse), so pressing
  ON again produces no edge. The only way to get one is OFF then ON, and that
  falling edge is wired to end the LiNC session
  (`baiermill-linc/hal/linc-postgui.hal:47-53`), after which
  `linc-session.sh:102-103` powers the PC off. `linc.machine-on` is not
  `halui.machine.is-on`, so it does not drop by itself on a limit fault: the
  session survives, the screen stays live, and the machine stays dead.
- the HMI, through NML — `NmlCommand::SetState(On)`, which
  `ToggleLimitsOverride` already sends.

Two bindings exist for that command and both are defective in different ways:

- `[keys] limits_override = "Ctrl+Shift+O"` (`configs/base.toml:217`, a global
  consume in `shell/keyboard.rs:383-387`) cannot be typed: the QMK keyboard sets
  `UPPERCASE 0` (`qmk_firmware/.../doubletap/keymap.c:3`), so it never emits
  Shift.
- The Manual and Homing screens also toggle it on a plain `O`
  (`screens/manual.rs:190-201`, through `handle_zero_keys`, active in JOG / INC /
  MPG / HOME / PROG.HOME). That one is reachable. It has no `[keys]` entry, is
  absent from the shortcut pane (`shell/hints.rs:343-364` lists `X/Y/Z`, `A`, the
  view chords and nothing about limits), has no on-screen text and no test. It
  is live behaviour nobody can discover.

So the panel can free a joint today; the operator cannot find out how, cannot
see the override state change, and the key frees the joint without offering the
bounded motion that makes it safe. The existing `Ctrl+Shift+O` bindings are
dead weight and should be removed or rebound.

Homing does not help. `EMC_JOINT_HOME` is silently dropped while motion is
disabled (`src/emc/motion/command.c:1380`), so `HOME X` and `HOME ALL` are dead
keys. Even with motion enabled, homing only backs off when the *home* switch is
already active (`homing.c:838-844`); against the home-side limit it searches
into the switch and aborts (`homing.c:183-190`).

## 2. Decisions

Settled in discussion:

- Offer both **ASSIST** (HMI arms the override, operator jogs) and **RECOVER**
  (HMI issues one bounded increment, repeated up to a cap).
- Step 5 mm at 5 mm/s, at most 4 steps (20 mm).
- Local only: no MCP tool, no network/remote trigger.
- No HMI homing and no `HOME X` button. HOME stays printed guidance inside the
  dialog: recovery is the only way to re-enable motion while the joint is
  tripped, and once the switch clears the panel's HOME mode works normally, so a
  button would add a second sequence for no new reachable state.
- The dialog opens automatically on a trip, focuses CANCEL, and stays
  reachable from a banner after dismissal.
- Entry: `Ctrl+I` globally, or a click on the banner. Plain `O` on Manual/Homing
  opens this dialog instead of toggling the override directly, so the key has
  one story and the dialog owns the arm.
- ASSIST re-arms automatically while the dialog is open, with a 60 s timeout.
- `ToggleLimitsOverride` becomes `ArmLimitsOverride`. The NML helper always
  sends `EMC_JOINT_OVERRIDE_LIMITS` with `joint = 0`, which arms the mask and
  cannot disarm it (`taskintf.cc:749-760`), so the old name never described the
  command. The dialog is the only caller after the `O` rebind; the dead
  `Ctrl+Shift+O` chord goes.

## 3. Why the shape is forced

- **Override must be armed before motion is enabled.** `check_for_faults()` runs
  every servo cycle and the joint is still tripped, so enabling first re-faults
  instantly. `ToggleLimitsOverride` already uses the correct order
  (`OverrideLimits` → `wait_complete` → `SetState(On)` if not ON).
- **One bounded increment per command, not a continuous jog.** A continuous jog
  issued by the HMI has no dead-man: if the UI or backend worker stalls, the
  axis keeps travelling with limits overridden. `NmlCommand::JogIncrement` is
  bounded task-side, so a dead HMI leaves the axis parked.
- **The override mask does not survive the move.** LinuxCNC clears it when the
  jog finishes (`control.c:1453-1457`), and the joint re-faults if it is still
  on the switch. So the recovery loop must re-arm and re-enable per step, and a
  cap is mandatory — otherwise a welded switch becomes unbounded travel.
- **Direction, side and jog mode are derived, never supplied.** A caller that can
  name a direction can drive the machine into the switch.
- **Don't block the backend worker.** `send()` runs on `linc-backend` and
  snapshot publishing is skipped while it runs (`backend.rs:191-220`). One step
  per command; the loop lives in the dialog, driven by snapshots.

## 4. New surface

### 4.1 `linc-core::recovery` (new module)

```rust
pub const STEP_MM: f64 = 5.0;
pub const FEED_MM_S: f64 = 5.0;
pub const MAX_STEPS: u8 = 4;

pub enum LimitSide { Min, Max }

impl LimitSide {
    /// Signed increment that moves away from the tripped switch.
    pub fn away(self) -> f64;      // Max => -1.0, Min => +1.0
}

pub struct RecoveryPlan { pub side: LimitSide, pub increment: f64, pub feed: f64 }

pub fn plan(side: LimitSide) -> RecoveryPlan;
```

One source of truth for the dialog text, the backend command and the tests.

### 4.2 Snapshot

`linc-core/src/model.rs`:

```rust
// MachineState
pub struct LimitState {
    pub min_hard: bool, pub max_hard: bool,
    pub min_soft: bool, pub max_soft: bool,
}
// Replaces `limits: Vec<String>`; `limit_labels()` renders the same spelling.
pub limits: AxisMap<LimitState>,
pub overridden: AxisMap<bool>,          // per-joint override mask

// MachineSnapshot
pub cabinet_on: Option<bool>,           // linc.machine-on; None when absent

impl MachineSnapshot {
    pub fn limit_tripped(&self, axis: &str) -> Option<LimitSide>;
    pub fn tripped_axes(&self) -> Vec<(String, LimitSide)>;
    pub fn any_limit_overridden(&self) -> bool;
}
```

- `LimitState` carries all four bits because the status bar already shows soft
  limits beside hard ones. Recovery reads `hard_side()` only.
- `Snapshot.limits_overridden` is now derived from the per-joint map, not from
  `halui.joint.0.override-limits`. The panel reader dropped that pin: it only
  ever described joint 0.
- The labels must keep their current spelling. Today the joint masks map
  `min_hard/max_hard` to `"X-"`/`"X+"` and `min_soft/max_soft` to
  `"X MIN"`/`"X MAX"` (`snapshot.rs:1390-1409`, `linuxcnc-nml/status.rs:469`).
  The dialog's `Min`/`Max` side is the hard pair, so the banner must not call a
  hard limit `MIN`/`MAX` while the status bar calls it `-`/`+`.
- `linc-linuxcnc`: map `JointStatus.min_hard_limit` / `max_hard_limit` through the
  existing axis↔joint mapping in `snapshot.rs` (it already maps `homed` and
  `homing` there).
- `linc-core::sim`: all false by default; a test hook sets a side.
- `cabinet_on`: read in `linc-linuxcnc` from `linc.machine-on` (the same pin the
  app's quit guard reads). `None` in the sim, which must be treated as allowed.

### 4.3 Command

`linc-core/src/command.rs`:

```rust
/// One bounded step off a tripped hard limit. The backend re-reads the
/// controller, derives the side, the sign and the jog mode itself, and refuses
/// unless `axis` is the only tripped axis.
RecoverLimit { axis: String },
```

`ToggleLimitsOverride` is replaced by `ArmLimitsOverride { axis }`: the existing
helper only ever arms, and the recovery dialog is its only caller once plain `O`
opens the dialog. The axis is carried so the backend can re-check the same gate
list; the NML helper still masks every currently tripped limit.

Header comment becomes:

```text
//! Machine-control commands (cycle, jog, spindle, coolant, overrides, E-stop)
//! are intentionally not representable, with one exception: `RecoverLimit`
//! issues a single bounded increment away from a tripped hard limit. It cannot
//! express a direction, a distance, a speed or a free jog.
```

### 4.4 Backend execution (`linc-linuxcnc`)

`MachineCommand::RecoverLimit { axis }`:

1. `refresh()` and validate (see §6). Refuse with a specific `MachineError`.
2. Derive `side` from that joint's `min/max_hard_limit`, mapping the axis to its
   joint. Refuse if the axis has no tripped limit or another axis is also tripped.
3. `OverrideLimits`, `wait_complete(1s)`.
4. If the task is not ON, `SetState(On)`, then poll the status until motion is
   enabled (bounded, ~1 s) before sending anything else. Verify in sim that a
   jog sent in the same cycle as the enable is not dropped.
5. `JogIncrement` with `increment = STEP_MM * side.away()`,
   `velocity = FEED_MM_S`, and `joint_mode` from the controller's motion mode
   (`FREE` → joint/index, `TELEOP` → axis/index). A homed machine sits in TELEOP,
   so joint-only jogging would be refused there.
6. Return `Ok(())` once the move is issued. The dialog watches the switch.

Never waits for the move to finish. The increment is the bound.

## 5. UI

### 5.1 Banner

Reuse the probe-trip pattern (`shell/gates.rs`, `chrome.rs:438`):

```text
LIMIT SWITCH — X ON +LIMIT — MOTION DISABLED. RECOVER (Ctrl+I)
```

The banner opens the dialog after it has been dismissed. Two ways in:

- the banner is clickable (the panel has a mouse layer);
- `Ctrl+I` globally, through `[keys] limit_recovery`. Keep
  `#[serde(alias = "limits_override")]` so an out-of-tree config still parses;
  `KeysConfig` is `deny_unknown_fields` and the field has no default.

Plain `O` on Manual/Homing opens the same dialog and no longer toggles the
override directly. Both entry points derive the axis from `tripped_axes()`:
with none tripped they toast `NO LIMIT SWITCH TRIPPED` and open nothing; with
more than one they open the dialog on the first and the gate shows
`MORE THAN ONE AXIS IS ON A LIMIT`. The Manual/Homing shortcut pane gains
`(O, "LIMIT RECOVERY")`. `Ctrl+Shift+O` is retired.

### 5.2 Dialog

`Modal::LimitRecovery(LimitRecovery)` in `state.rs`, `kind()` =
`"limit-recovery"` so a rewrite keeps the keyboard where the operator left it.

Content:

```text
LIMIT SWITCH TRIPPED
X IS ON THE +LIMIT. MOTION IS DISABLED AND HOME WILL NOT WORK UNTIL IT IS RE-ENABLED.
X CAN ONLY MOVE NEGATIVE FROM HERE.

RECOVER   POWER ON, LIMITS OVERRIDE, JOG X- 5.0 mm AT 5 mm/s
          REPEATED UP TO 4 STEPS (20 mm), STOPPING WHEN THE SWITCH CLEARS.
ASSIST    ARM THE OVERRIDE, THEN YOU HOLD X- ON THE PANEL.
CANCEL

DO NOT PRESS POWER OFF: ON THIS MACHINE THAT ENDS THE SESSION AND POWERS OFF THE PC.
```

- `RECOVER` is the accent button, disabled with a one-line reason when a gate
  fails. `ASSIST` is the secondary action. `CANCEL` takes the focus on the first
  frame and Enter never recovers.
- Auto-opens once per `(axis, side)`. Dismissal leaves the banner; the banner key
  or `Ctrl+I` reopens it.
- No `HOME X` button: the HOME sentence is guidance. Once the switch clears,
  home from the panel's HOME mode.

### 5.3 RECOVER loop

State: `{ axis, side, steps_done, phase, message }`.
`phase ∈ { Confirming, Stepping, Waiting, Done }`.

1. On confirm: send `RecoverLimit { axis }`.
2. On `CommandResult` error: show the reason, phase `Done`.
3. On success: wait for `!snapshot.task.motion_active` (timeout ~5 s → `JogStop`,
   error).
4. Re-read `limit_tripped(axis)` with a fresh snapshot:
   - `None` → cleared. Close the dialog, toast `X CLEARED AFTER n STEPS`.
   - `Some(same)` → `steps_done += 1`. At `MAX_STEPS` → stop and report
     `STILL TRIPPED AFTER 4 STEPS — SWITCH MAY BE FAULTY, MOTION LEFT DISABLED`.
     Otherwise issue the next step automatically.
   - `Some(other)` → stop, report `OPPOSITE LIMIT TRIPPED — RECOVERY STOPPED`.
5. `STOP` sends `AbortProgram` (the existing typed reset; there is no `JogStop`
   command and no reason to add one for a bounded increment) and ends the loop.
   Closing the dialog ends the loop.
6. The override is never left armed: LinuxCNC clears it when the move ends, and
   the dialog must not re-arm it after `Done`.

The last step leaves the machine in whatever state LinuxCNC chooses: if the
switch is still tripped the joint faults again and motion is disabled. That is
the correct safe end state for a failed recovery.

### 5.4 ASSIST

1. Same gates. Arms the override and enables motion.
2. Shows `OVERRIDE ARMED — HOLD X- ON THE PANEL. RELEASE WHEN THE SWITCH CLEARS`,
   with live `X +LIMIT` / `X CLEARED`.
3. Re-arms automatically after each jog (the mask clears when the move ends)
   **only while** the view is open, the same single axis is tripped, and the
   panel is still in a manual mode. There is no `ARM` press. Re-read before
   every re-arm: `EMC_JOINT_OVERRIDE_LIMITS` masks every currently tripped
   limit (`command.c:687-698`), so a second trip during assist would otherwise
   be masked too.
4. Disarms on clear, CANCEL/dialog close, mode change, or a 60 s timeout.
5. Issues no motion at all. The physical jog key remains the dead-man.

## 6. Gates

Checked at dialog open *and* again in the backend for every command.

| Gate | Source | Refusal |
| --- | --- | --- |
| Cabinet on | `cabinet_on != Some(false)` | `CABINET IS OFF` |
| E-stop released | `machine.estop` | `EMERGENCY STOP ACTIVE` |
| Panel live and current | `panel.connected`, `meta.stale` | `PANEL OFFLINE` |
| Manual mode | `panel.mode ∈ {Jog, Inc, Mpg}` | `SELECT JOG ON THE PANEL` |
| No program running | `!task.running`, `task.interp == Idle` | `ABORT THE PROGRAM FIRST` |
| Spindle stopped | all `spindle.on == false` | `STOP THE SPINDLE` |
| Probe not tripped | `!probe_trip_active()` | `RESET THE PROBE TRIP FIRST` |
| No homing in progress | `machine.homing` | `HOMING IN PROGRESS` |
| No pending tool change | `tool_change.status` | `FINISH THE TOOL CHANGE` |
| Exactly one tripped limit | `tripped_axes().len() == 1` | `MORE THAN ONE AXIS IS ON A LIMIT` |
| Axis matches | `tripped_axes()[0].0 == axis` | `X IS NOT ON A LIMIT` |

Not required: machine ON (the fault clears it), all axes homed, program unloaded.

## 7. Tests

Unit:

- `recovery::plan`: sign per side, step, feed, cap.
- Snapshot: joint hard limits map to the right axis; sim defaults false;
  `cabinet_on` absent = allowed.
- Backend gate matrix: one test per refusal, and one per success path.
- Backend derives `joint_mode` from motion mode; a caller-supplied direction or
  distance is not representable.
- Backend refuses when the named axis is not tripped, and when two axes are.
- Dialog: CANCEL focused on the first frame; RECOVER disabled with the right
  reason; the loop stops at the cap; STOP aborts; success closes and leaves no
  override; opposite-side trip aborts.
- Banner text per state.
- The Manual/Homing plain `O` key opens the recovery dialog in each of JOG,
  INC, MPG, HOME and PROG.HOME, does nothing on the other pages, and toasts
  when no axis is tripped. It sends no command itself. It has no test today;
  the discovery work depends on it, so it needs one.
- `ArmLimitsOverride` is arm-only: sending it twice leaves the mask armed, and
  the dialog never relies on a toggle to disarm.
- `linc-core::sim`: `RecoverLimit` clears a tripped limit after the configured
  number of steps, so the loop can be tested.

End to end (LinuxCNC sim, `./scripts/start-sim.sh`):

The sim derives `min/max-*.bit` from the commanded position against the INI
limits plus `sim-lim-margin`, and joint feedback is the command itself
(`hal/rio_sim.py:534-563`, `hal/rio_sim.hal:575-577`). On an unhomed axis the
soft limits are inactive, so jogging past them trips the simulated hard limit.

1. `./scripts/start-sim.sh --detach --release-estop` (no `--home`), panel in JOG.
2. Jog X− until the hard limit trips.
3. Confirm: motion disabled, task state reset, `linc.machine-on` still 1, HOME X
   does nothing.
4. Open the dialog, RECOVER, confirm the axis clears in 1–4 steps and the
   override is off afterwards.
5. Repeat, choosing ASSIST: hold X+, confirm the switch clears and the override
   disarms.
6. Force a stuck switch case (leave the axis parked, send RecoverLimit manually
   with the panel mode in AUTO) and confirm the refusal text.

Result (2026-11-12, `sim-no3d.ini`, powered on, unhomed, panel JOG):

- The trip landed at X −368.0 with `limit[0] = 1`, motion disabled, task state
  `ESTOP_RESET`, `linc.machine-on` TRUE and `halui.machine.is-on` FALSE.
  `c.home(0)` left `homed = (0,0,0)` and the task state unchanged: HOME is dead
  while motion is disabled, as expected.
- Auto-open raised the banner and the dialog; Enter on the first frame hit
  CANCEL and recovered nothing.
- RECOVER cleared the switch in one step: X −368.024 → −363.024 (exactly 5 mm),
  override back to FALSE when the jog ended, motion ON, dialog closed.
- ASSIST armed the override with the switch still tripped (`on = true`,
  `overrideX = true`); a jog cleared the switch; the override cleared at the end
  of the jog and the dialog closed.
- With the panel in AUTO, RECOVER and ASSIST were disabled with
  `SELECT JOG ON THE PANEL (JOG, INC OR MPG)`, and a direct backend call
  returned `WRONG MODE: panel mode does not permit this action` for both
  `RecoverLimit` and `ArmLimitsOverride`.

## 8. Docs to change

- `SPEC.md:1350` §6.10: new `6.10.1 Limit-switch recovery` with the rules above.
- `SPEC.md:1790-1800` §11: add the exception to "Validate machine ON…" and note
  that recovery never issues a home command.
- `SPEC.md:1345` §6.9: "no on-screen jog, override…" gains the recovery
  exception, with the same narrow wording as MDI / probe / tool change.
- `command.rs` header (above).
- `configs/base.toml:216-217`: comment for the new chord and the recovery key.
- `CONFIGURATION.md` `[keys]` table.
- `AGENTS.md`: the "No on-screen mode, cycle, jog, spindle, coolant, override or
  software E-stop" invariant gains the exception list entry.
- After verification: a `docs/solutions/` entry on the maintained-latch recovery
  constraint (edge-triggered `halui.machine.on` + session-ending falling edge).

## 9. Non-goals

- No general jog surface, no on-screen override control outside this dialog.
- No HMI homing, no `EMC_JOINT_HOME`, no unhome.
- No automatic recovery without an explicit confirm.
- No remembered "safe position": none exists on an unhomed machine.
- No MCP tool and no remote trigger. The recovery jog goes over NML and does not
  pass through the HAL jog gates, so any future interlock must be re-checked in
  the backend, not assumed from `halui`.
- Recovery cannot fix a welded or miswired switch; it reports and stops.
