---
artifact_contract: ce-unified-plan/v1
product_contract_source: ce-plan-bootstrap
execution: code
---

# The interpreter check does not hold the frame

## Goal Capsule

**Objective.** Loading or saving a program never stalls the HMI while `rs274`
reads it, and the program interlock stays shut for exactly as long as a verdict
is outstanding.

**Means.** `linc_core::verify::check_program` moves onto a worker thread. The HMI
keeps the generation of the request, polls the channel once a frame, and applies
the verdict only if it still belongs to the newest request. A load waits for its
verdict before it is sent to the backend; a save's verdict arrives after the
write and only moves the interlock.

**Authority.** This plan. `SPEC.md` §6.7 (947-953), `CONFIGURATION.md:1010-1017`
and `AGENTS.md:69` describe the check as part of the load path and are updated by
U6; until they are, they describe the old shape.

**Stop conditions.** Stop and report rather than continue if: the interlock can
be observed open in any frame between a starting check and its verdict; a
superseded verdict can still reach `send`; or `program.verify_on_load = false`
stops loading a file.

**Execution profile.** One PR. U1 must not ship without U2: a worker without the
gate opens a window where the interlock reads the previous text's verdict.

## Product Contract

### Requirements

- **R1.** No frame blocks on `rs274`. `load_program` and `save_program` return
  without waiting for the interpreter, whatever `verify_timeout_ms` says.
- **R2.** A load the interpreter refuses is not sent to the backend, and its
  fault is shown the way it is today (buffer open, issue marked, toast).
- **R3.** `program_ready()` is false from the moment a check starts until its
  verdict is applied — for a load and for a save alike. A refusal keeps it false
  afterwards, as now.
- **R4.** A verdict that belongs to a superseded request is dropped and has no
  effect: no load, no gate change, no toast.
- **R5.** A save no longer waits: the file is written and the gate follows the
  verdict when it lands; a refused save still says `SAVED — …` and keeps the
  gate shut.
- **R6.** With `program.verify_on_load = false` nothing changes: no job, no
  channel, the load is sent immediately, the gate reads as it does today.
- **R7.** An editing keystroke cancels an outstanding save-verdict, because that
  verdict was about text that no longer exists. It does not cancel a load.

**Acceptance examples.** (a) A 200 MB program with `verify_timeout_ms = 60000`:
the frame rate does not change while the verdict is outstanding. (b) A file the
interpreter refuses: no `LoadProgram` command is ever sent, and the toast names
the line. (c) Load a clean file, then edit the buffer before the verdict lands:
the verdict is discarded, the gate stays shut. (d) Save in EDIT with a fault: the
file is written, `SAVED — LN n: …` appears when the verdict lands, and
`linc.program-ok` never went true in between.

**Out of scope.** The process-wide `INTERPRETER_LOCK` stays: it serialises two
checks, and it no longer holds the frame while it waits. The `Unavailable` answer
for a machine without `rs274` stays a pass, as documented.

## Planning Contract

### Key Technical Decisions

- **The verdict is a value, not a call.** `refusal_from_check(&MachineCheck) ->
  Option<String>` is extracted from `syntax_refusal` so the decision can be
  tested without an interpreter, and the worker only ever produces the value.
- **Generation, not cancellation.** Bumping a counter is enough to discard a
  stale verdict; the thread finishes on its own and its `send` fails harmlessly
  when the receiver is dropped. No join, no shared flag, no teardown ordering.
- **The job lives beside the other per-page work.** `ProgramUi` already holds
  the preview and the machine verdict; it carries `verify: Option<VerifyJob>` and
  the generation. `state.rs` already hosts `FilesUi.mount_job`, the same shape.
- **A deferred load keeps `pending_load = true`.** The Files page already treats
  that as "a load is on its way", which is still true while the check runs; the
  refusal path clears it as it does now.
- **Polling is per frame, in `sync`.** `poll_verify()` sits next to
  `poll_backend()`; the check has no deadline of its own to schedule.

### Assumptions

- `rs274` is a short-lived child and the temp file it reads is deleted when the
  job's closure drops it.
- A save that the check refuses is still written: the write is the operator's
  decision, the check is the interlock's.

## Implementation Units

### U0. Make the decision testable

Extract `refusal_from_check(&MachineCheck) -> Option<String>` out of
`syntax_refusal`, unchanged in wording (`NOT LOADED  LN n: …`, the `MACHINE`
fallback, the crash sentence). Unit-test the five outcomes: `Refused` and
`Crashed` refuse, `Ok`, `Timeout` and `Unavailable` pass.

### U1. Run the check on a worker

`VerifyKind::{Load, Save}`, `VerifyJob { kind, path, generation, started, rx }`,
`start_verify(...)` spawning a named thread (`linc-verify`) that owns the text and
a cloned `Profile` slice it needs (INI path, tool table, timeout), and
`poll_verify()` doing `try_recv` with a disconnected channel read as
`Unavailable` (a check that never answered is not a refusal).

### U2. Hold the gate while a verdict is outstanding

`program_ready()` gains `self.program.verify.is_none()`.
`program_block_reason()` gains the "CHECKING" sentence ahead of the verdict
readings. `mark_editor_changed` drops a `Save` job and bumps the generation.

### U3. Defer the load

`load_program` keeps the sync lint + preview refusal (they are cheap and they are
what makes a broken file open in the editor), then either sends `LoadProgram`
(no verification configured) or starts a `Load` job. The verdict arrives in
`poll_verify`: refusal → `refuse_load`, pass → `send(LoadProgram)` +
`pending_load = true`.

### U4. Let the save answer later

`verify_saved_text` starts a `Save` job with `machine_check = None` (the old
verdict is about the old text) and calls `sync_program_ok()`, which shuts the
gate until the answer lands. The verdict sets `machine_check`, toasts a refusal,
and syncs the gate.

### U5. Tests

A `settle(app)` helper polls `poll_verify()` with a deadline, so a test that
needs the verdict says so. Existing load and save tests call it. New:
`a_load_is_deferred_until_the_verdict_lands`, `a_refused_verdict_refuses_the_load`,
`a_superseded_verdict_is_dropped`, `the_gate_is_shut_while_the_check_runs`,
`a_check_that_never_answers_does_not_refuse`, plus the U0 unit tests.

### U6. Correct the written model

`SPEC.md` §6.7, `CONFIGURATION.md`'s `verify_on_load` row (drop "costs about
40 ms", say the frame is not held), `AGENTS.md`'s `linc-core::verify` line.

## Verification Contract

```sh
cargo test -p linc-ui            # the shell tests, including the new ones
cargo test --workspace           # nothing else moved
cargo clippy --workspace --all-targets
rustfmt --check crates/linc-ui/src/shell/*.rs crates/linc-ui/src/state.rs
```

The freeze itself is shown by the absence of a `LoadProgram` command in the frame
that asked for the load, and by the gate being shut in that same frame.

## Definition of Done

- Every requirement above has a test that fails without its unit.
- `cargo test --workspace` and `cargo clippy --workspace --all-targets` are clean.
- SPEC, CONFIGURATION and AGENTS describe the asynchronous check.
- No `check_program` call remains on the UI thread (`grep -n check_program
  crates/linc-ui/src` finds only the worker closure).

## Appendix

The defect, as the review stated it: `rs274` runs inline for up to
`verify_timeout_ms` (2 000 ms by default, 60 000 ms by validation), inside the
frame that handles the load or the save. The panel service is a separate process,
so the machine stays stoppable — but the display does not repaint, and its guarded
quit cannot be answered, for that whole time.
