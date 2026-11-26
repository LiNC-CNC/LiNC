---
artifact_contract: ce-unified-plan/v1
product_contract_source: ce-plan-bootstrap
execution: code
---

# One owner for CYCLE START

## Goal Capsule

**Objective.** Pressing CYCLE START on the panel starts or resumes exactly one
program, once, and a press LiNC refuses is not executed by anything else on the
machine.

**Means.** The HMI owns the decision and the command. The panel key stops being
a HAL edge into `halui.program.run`; the HMI reads the bit, runs its refusal
checks, and sends `MachineCommand::CycleStart` over NML the way AXIS does.

**Authority.** This plan. `SPEC.md` §6.2 (lines 945-946) and `AGENTS.md:74-75`
state the opposite ownership today and are updated by U6; until they are, they
are stale, not authoritative.

**Stop conditions.** Stop and report rather than continue if: a file this plan
edits still leaves a net carrying `cnc_panel.cycle_start` that reaches
`halui.program.run` or `halui.program.resume`; removing the chain and the
sampling fix together leaves the HMI unable to observe a press; or a profile
this plan edits fails `--check-config`.

**Execution profile.** One PR. Units land in the order given: U0 makes presses
observable, and U1-U3 must not ship without it.

## Product Contract

### Requirements

- **R1.** One press of CYCLE START produces at most one program-start command at
  the controller. No press produces two (HAL run plus NML run).
- **R2.** A press the HMI refuses (E-stop, machine off, non-AUTO, stale panel,
  missing homing, program interlock shut) sends nothing, and that refusal is
  visible on screen and in the log.
- **R3.** A paused program is resumed by the same key, under the same refusal
  checks except the idle-interpreter one.
- **R4.** `linc.program-ok` stays a published HMI output: it is in
  `LincOutput::ALLOWED`, `configs/base.toml`, `linc-hal` and SPEC §5/§11, so
  removing it is an output-contract change this plan does not make. After U3 no
  in-tree file consumes it; it stays published for a profile that adds a
  post-HMI net, and the Diagnostics page keeps showing it.
- **R5.** CYCLE STOP keeps its current HAL path, and the CYCLE OFF LED keeps its
  `halui.program.is-paused` source: pausing is not a program start.
- **R6.** The suite fails if `cnc_panel.cycle_start` becomes reachable to
  `halui.program.run` or `halui.program.resume` through any chain of nets in any
  HAL file — reachability, not a net name.

**Acceptance examples.** (a) Sim, AUTO, clean program, press → program runs, one
`cycle start sent to the controller` log line, no `halui.program.run` edge.
(b) Same with a syntax-faulted program → nothing sent, `CYCLE START REFUSED —
the interpreter refused the program: …` toast. (c) Paused program, press →
resume, one command. (d) A press observed in a snapshot flagged stale → refused
and logged; a panel that is not reporting produces no snapshot, so there is no
press to see.

**Out of scope.** The BaierMill profile's own `szgh_panel.cycle_start` chain
(`hal/baiermill-pb-postgui.hal:264-275`) keys a different panel driver through a
machine hallib; it is the same defect class and its own decision. Named as a
follow-up, and the verification greps below are scoped so its nets do not trip
them.

## Planning Contract

### Key Technical Decisions

- **KTD1 — The HMI owns cycle start.** Chosen by the operator after review
  finding #5. Rejected: HAL ownership (keeps `program.rs:112`'s model, loses the
  refusal text and the 0.4.0 feature); HMI-decides/HAL-executes (adds a new
  `linc.*` output and a pin round trip).
- **KTD2 — Presses must be observed before the HAL path goes.** The real backend
  caches the button word for one second (`linc-linuxcnc/src/panel.rs:373-377`)
  while a momentary key is held on its pin for 200 ms
  (`linc-panel/src/hal.rs:196-199`), so most presses never reach the HMI today.
  With the HAL edge gone, an unobserved press is a dead key. U0 fixes the
  sampling; it lands first and on its own it is a real improvement.
- **KTD3 — Why the published pin stays.** It is part of the declared output
  contract (R4). Deleting it would touch `LincOutput::ALLOWED`, `base.toml`,
  `linc-hal`, SPEC §5/§11 and CONFIGURATION §1/§8 for no operator gain. The plan
  does not claim a current consumer.
- **KTD4 — Remove the chain, not just the key net.** Once nothing drives
  `and2.cycle_run`, every gate in that chain computes FALSE for ever. Leaving
  them loaded is the dead-wiring defect the same review found in the BaierMill
  post-GUI file (#1), so the instances and their `addf` lines go too.
- **KTD5 — The gate tests invert.** `program_gate_hal.rs` asserts the interlock
  reaches `halui.program.run`; that becomes false by design. The file instead
  asserts graph non-connectivity between `cnc_panel.cycle_start` and the two
  halui program pins, reusing its own `Nets::connected` helper. The assertion
  must fail on the tree before U1 and pass after it; that is the check on the
  check.
- **KTD6 — `AGENTS.md`, `SPEC.md` and `program.rs`'s comment change with the
  HAL.** A reader who finds any one of them first must not learn the old model.

### Assumptions

- halui fires on both edges of its program pins, and a pin nothing nets never
  changes level, so an unwired `halui.program.run` cannot start anything. This
  is why the removal is safe rather than merely tidy.
- The profiles that load the files U1-U3 edit are the mill, the lathe and the
  simulation. `configs/baiermill-pb.ini` loads its own post-GUI file, which this
  plan leaves alone.
- `hal/linc-postgui.hal` is the real mill's post-GUI file and no in-repo profile
  loads it; U3's edit there is verified by reading, not by a run.

## Implementation Units

### U0. Observe every press

`crates/linc-linuxcnc/src/panel.rs:373-377`: read the button word on every
snapshot instead of once a second. Keep the one-second cache for the LED,
7-segment and tool-number fields, which change slowly and cost 46 pin reads.
Add a test that a button-pin level change appears on the next snapshot.
Without this unit the rest of the plan takes cycle start off the machine.

### U1. Remove the mill cycle-start chain

`hal/linc.hal`: delete `net cycle-start cnc_panel.cycle_start …:167`, and the
chain that existed only to carry it — `not-paused`, `cycle-gate`, `cycle-run`,
`cycle-permit`, `cycle-resume`, `resume-permit` — plus `and2.cycle_gate`,
`cycle_run`, `cycle_permit`, `cycle_resume`, `resume_permit` and `not.paused`
from their `loadrt` lists, and their five `addf` lines. Delete the comment
blocks at 34-37 and 168-174 that explain the removed gate.

Keep `net is-paused halui.program.is-paused => …` driving the CYCLE OFF LED
(`cnc_panel.led_cycle_off`, also netted at :382): drop only the `not.paused` and
`and2.cycle_resume.in0` links from it. Leave `or2.pause` and the CYCLE STOP net
alone (R5).

### U2. Remove the lathe cycle-start chain

`hal/linc-lathe.hal`: delete `net lathe-cycle-start …:56` and
`net lathe-cycle-permit …:57`, the `addf and2.cycle_permit` line and its instance
in the `loadrt and2` list. Rewrite the comment above them to say the HMI starts
the program and the file only pauses it.

### U3. Drop the pin nets the gates carried

`hal/linc-postgui.hal:22`, `hal/linc-sim-postgui.hal:17` and
`hal/linc-lathe-postgui.hal:23` net `linc.program-ok` into `and2.cycle_permit` /
`and2.resume_permit` inputs that no longer exist. Remove the nets and the
paragraph that explains them. The pin stays documented in U6 as published state.

### U4. Retire the gate tests, add the reachability test

`crates/linc-core/tests/program_gate_hal.rs` currently holds six tests. Retire
the four that assert the interlock reaches halui:

- `every_machine_hal_that_starts_a_program_is_gated`
- `the_mill_cycle_start_and_resume_pass_through_the_program_interlock`
- `the_lathe_run_is_gated_and_the_instance_is_declared_before_the_net`
- `the_simulation_wires_the_same_gate`

Keep `every_and2_these_files_create_is_added_to_a_thread` (it is what proves U1
and U2 removed instances and `addf` lines together) and
`the_baiermill_postgui_file_cannot_carry_the_net` (the BaierMill exemption).

Add one test: for every HAL file, `Nets::connected("cnc_panel.cycle_start",
"halui.program.run")` and the same for `halui.program.resume` must both be false.
Run it before U1: it must fail, or the assertion is not doing its job.

### U5. Keep the interlock published

No code change expected. Confirm `sync_program_ok` still publishes after load,
save, edit, unload and startup, and that no UI string claims the panel key
bypasses LiNC.

### U6. Correct the written model

- `AGENTS.md:74-75` — replace "CYCLE START itself is a HAL edge into
  `halui.program.run` and never passes through LiNC" with the HMI-owned model,
  and name the consequence: the pin no longer gates a HAL path, it is published
  state.
- `AGENTS.md:97, 114` — the `program-ok` output stays listed; its rôle changes.
- `crates/linc-ui/src/shell/program.rs:112-116` — rewrite the comment: the load
  gate is one of the HMI's own refusal checks, and the key reaches the controller
  only through `MachineCommand::CycleStart`.
- `SPEC.md:945-946` — same correction; §5/§11 output tables keep the pin.
- `CONFIGURATION.md` §1/§8 — if they describe the interlock's wiring, correct it.

## Verification Contract

```bash
cargo test --workspace
cargo build --release
./target/release/linc --config configs/sim.toml --check-config
./target/release/linc --config configs/lathe-sim.toml --check-config
./scripts/start-sim.sh --detach --release-estop --power-on --home
LINC_SIM_CONFIG=configs/lathe-sim.toml ./scripts/start-sim.sh --detach --release-estop --power-on --home
```

`--check-config` validates the TOML profile, not the HAL files; the simulator
start is what loads them, which is why both the mill and the lathe sims are in
the list. In each sim: load a clean program, press CYCLE START, confirm exactly
one `cycle start sent to the controller` line and no second command; repeat with
a syntax-faulted program and confirm the refusal toast and nothing sent; pause
and press again for one resume.

Scoped greps (the BaierMill's own chain is the named exception):

```bash
grep -n 'cnc_panel.cycle_start' hal/linc.hal hal/linc-lathe.hal hal/linc-postgui.hal hal/linc-sim-postgui.hal hal/linc-lathe-postgui.hal
```

must return nothing.

## Definition of Done

- Every unit's edit is in, in the order U0 → U1 → U2 → U3 → U4 → U6.
- `cargo test --workspace` passes, including U0's sampling test and U4's
  reachability test, with the created→added test unchanged.
- `--check-config` passes for `sim.toml` and `lathe-sim.toml`; the baiermill
  profile still fails exactly as it did before (its own known guard).
- Both simulations behave as the acceptance examples say.
- `AGENTS.md`, `SPEC.md`, `CONFIGURATION.md` and `program.rs`'s comment describe
  the HMI-owned model, and no file in the tree still claims the panel key goes
  straight to `halui`.

## Appendix

Evidence gathered from the tree at `c5f2459`, and from the document review that
shaped this revision:

- `hal/linc.hal:19` creates 49 `and2` instances including the five the chain
  uses; `:32-33` add `cycle_resume`/`cycle_run`; `:164-176` the chain; `:167` the
  key net; `:382` the CYCLE OFF LED net that shares `is-paused`.
- `hal/linc-lathe.hal:56-57` the lathe chain, `:58` the CYCLE STOP net.
- `hal/linc-postgui.hal:22`, `hal/linc-sim-postgui.hal:17`,
  `hal/linc-lathe-postgui.hal:23` net `linc.program-ok` into the gates.
- `crates/linc-linuxcnc/src/panel.rs:373-377` the one-second button cache;
  `crates/linc-panel/src/hal.rs:196-199` the 200 ms key hold for CYCLE START.
  Their interaction is why U0 exists.
- `crates/linc-linuxcnc/src/lib.rs:1547+` `MachineCommand::CycleStart` — notes
  that halui fires on both edges, which is why a HAL edge produced two commands.
- `crates/linc-ui/src/shell/gates.rs:104-141` `service_cycle_start` and its
  refusal path; `crates/linc-ui/src/shell/program.rs:112-116` the comment that
  states the opposite model and is corrected by U6.
- `crates/linc-core/tests/program_gate_hal.rs` — six tests: the four U4 retires,
  and the two it keeps.
- Document review (coherence, feasibility, adversarial) found and this revision
  corrects: the missed fourth retirement, the unscoped greps against the
  BaierMill chain, the missing `program.rs` comment, a direction test that could
  not fail, and KTD2's false claim about a current consumer of the pin.
