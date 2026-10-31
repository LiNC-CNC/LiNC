---
title: Saving a program must reopen it, or AUTO runs the pre-edit text
date: 2026-10-30
category: integration-issues
module: LinuxCNC backend and program save
problem_type: integration_issue
component: api_layer
symptoms:
  - "AUTO starts the text that was loaded before the edit, not the saved file"
  - "The file on disk holds the edit, yet the interpreter runs the old program"
  - "Reopening after the save zeroed the stored G5X work-offset row"
root_cause: missing_workflow_step
resolution_type: code_fix
severity: high
related_components: ["sim backend", "editor save path"]
tags: [linuxcnc, nml, program-open, save, work-offsets, cycle-start]
---

# Saving a program must reopen it, or AUTO runs the pre-edit text

## Problem

The editor wrote the buffer to disk, cleared the dirty marker, and reported the
save as done. Cycle start then ran the version that had been loaded before the
edit, because LinuxCNC's interpreter keeps the program it opened in its own
state. The operator saw a saved file and a machine that executed something else.

## Symptoms

- `AUTO` produces the old motion after a save that visibly changed the file.
- Reading the saved file back shows the new text, so nothing looks wrong at the
  filesystem level.
- A first attempt that reopened the file inside `SaveProgram` cleared the active
  `G5X` row: `ProgramOpen` resets the interpreter, and the row it leaves behind
  is all zeros until a program publishes one.

## What Didn't Work

- **Writing the file only.** The write was already atomic (`temp` + `persist` +
  `sync_all`), and the backend test asserting the file bytes passed. The file
  was correct; the interpreter's copy was stale. The missing step was on the
  controller side, where the file bytes were never re-read.
- **Copying the open sequence inline into the save arm.** The `OpenProgram`
  command arm already carried the correct sequence (`capture_live_offsets`,
  `save_parameters`, `ProgramOpen`, `wait_complete`). Repeating it in `SaveProgram`
  without that first step is what zeroed the stored offset row.

## Solution

Extract the open sequence into one method, then call it after the write:

```rust
/// Open `canonical` in the interpreter, publishing the live offsets first.
///
/// `ProgramOpen` resets the interpreter, and the active `G5X` row it leaves
/// behind is all zero until the program publishes one. The live row is
/// written to the parameter file before the reset; opening without that
/// step replaced a stored row with zeros.
fn open_program(&mut self, canonical: PathBuf) -> Result<(), MachineError> {
    self.capture_live_offsets();
    self.save_parameters()?;
    self.nml_send(&NmlCommand::ProgramOpen(canonical))?;
    self.wait_complete(Duration::from_secs(2))?;
    Ok(())
}
```

`SaveProgram` persists the temp file, then calls `self.open_program(canonical)?`.
The simulator mirrors the contract: its save replaces the program state, so a
snapshot reports the saved path and the saved content, and its test asserts
exactly that.

`SaveProgram` keeps using `validate_write` rather than
`validate_controller_write`: writing a file and reopening it moves no axis, and
losing an operator's buffer to a latched probe trip would be the worse fault.

## Why This Works

The interpreter's program buffer is refreshed only by an open. Once the file is
written, opening it makes the saved text both the on-disk artifact and the
controller's loaded program, so `AUTO` has one program to start. Publishing the
live offsets before the reset preserves the row the open would otherwise blank.

## Prevention

- A backend test asserts the loaded program, not only the file bytes: after
  `SaveProgram`, the snapshot's `program.path` and `program.content` match what
  was written. File-level assertions cannot catch a stale interpreter.
- One method owns the open sequence. Any command that must make a file the
  loaded program calls `open_program`, so the offset-publishing step cannot be
  lost by copying four lines and dropping the first.
- `SPEC.md` states the coupling where an implementer reads it: the saved file is
  also made the controller's loaded program, because LinuxCNC keeps the copy it
  opened in the interpreter.

## Related Issues

- The simulator save path (`crates/linc-core/src/sim.rs`) and the native backend
  (`crates/linc-linuxcnc/src/lib.rs`) must keep the same post-save contract; a
  divergence makes standalone sim testing miss backend regressions.
