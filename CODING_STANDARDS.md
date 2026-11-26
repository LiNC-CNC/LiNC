# Coding standards

Rules code in this repository follows. Each one names a defect class that
reached the machine or the screen in past work, so a reviewer can see it in a
diff and an author can avoid it while writing. No lint, type checker, or CI job
enforces these; `cargo test --workspace` covers a subset.

AGENTS.md keeps the operating detail — commands, crate map, page conventions,
traps. This file states the conditions that decide whether a change is correct.

## HAL wiring

- A HAL function needs three lines: `loadrt` with its instance name, the `net`
  that uses it, and `addf` into a thread. A function that is created and netted
  but never added is never called, keeps its initial value for ever, and raises
  no error. Prove it runs from the function instance's own counter, never from
  the pin it drives.
- Every output the HMI path writes must be published by something: the writer
  call itself, or the backend loop reading it from the snapshot. A variant
  parked in an arm that no worker handles is dropped without a trace.
- A HAL pin takes exactly one signal. Where two sources must drive one
  single-pin input, OR them through a dedicated gate rather than attaching a
  second net; a second `net` to one pin is a wiring fault, not a merge.
- A jog increment must reach both the axis pin and the joint pin for the same
  motion mode. halui reads the axis pins for a teleop jog and the joint pins for
  a joint or FREE jog, so a missing half leaves the increment keys dead in one
  mode and alive in the other.
- A gate pin must accept the bit its producer publishes. A type mismatch leaves
  the gate permanently false and looks like a logic bug at the consumer.
- A HAL change is made in the file the machine profile resolves, not in the
  file of the same purpose under `hal/`. A profile names its own HAL files, and a
  machine's post-GUI file can come from another repository entirely, so a change
  to the repository's copy is unverified until that profile is read and its
  installed file is changed. Confirm the result on the restarted machine's pins,
  never on the repository's nets. See
  `docs/solutions/integration-issues/hal-edit-reaches-the-machine-through-its-profile.md`.
- Wire `linc.*` nets only in the post-GUI HAL files. A pre-GUI net on a pin that
  does not exist yet fails at load, and the failure names the pin, not the file.

## Controller boundary

- Saving a program must reopen it in the interpreter before the save is reported
  done. The interpreter keeps the program it opened in its own state, so a
  buffer written to disk alone leaves AUTO running the pre-edit text.
- Write work offsets in absolute machine units. A display unit carried into a
  write lands the offset off by the machine's scale factor.
- Read tool, parameter, and status state back only after the interpreter is
  idle. A read taken during motion returns the previous value, and the follow-up
  move then uses the stale one.
- Recovery from a hard limit arms the override before motion is enabled.
  Enabling motion first re-faults on the next servo cycle and leaves the joint
  disabled with no way back from the HMI.
- E-stop is not the same event as losing machine-on power. A chain that keeps
  power through the switch must not report a power drop, and a session must not
  be torn down when only the machine-on latch falls.
- An `extends` chain resolves completely before any key is read. A partially
  resolved profile reads as a missing key rather than as a broken chain.

## G-code

- Mirror the interpreter's own grammar and parameter scope. Where the reading
  differs from LinuxCNC — named parameters, o-word forms, assignment, loop
  constructs — the divergence is the defect, so read the controller's own
  programs as the test corpus instead of the subset the preview happens to
  handle.
- Draw preview geometry in the frame the interpreter runs in. Work-offset
  frames, diameter programming on a lathe, and cycle passes all place the tool
  somewhere other than the raw block coordinates imply.
- A program bound covers every arc sweep and repeat pass it draws, or the fit
  command clips a path that is on screen.

## Panel

- A protocol reply that arrives out of step resyncs the link and keeps it up.
  Dropping the link turns a recoverable desync into an offline panel; the panel
  state is the operator's only view of whether the hardware is alive.

## UI

- Keyboard focus is handed back explicitly when an edit closes, a modal closes,
  or the page changes. egui has no click-to-focus and the panel keyboard has no
  Tab, so a widget that keeps the ring after it is done makes every other
  keyboard path unreachable for the rest of the session.
- Consume a page-local key only after the page and the focus state are tested.
  `consume_key` swallows the key even when the rest of the condition is false,
  so an over-broad guard eats a global key everywhere.
- Lay out from rects inside one frame. Measure text rather than hardcoding
  widths, bound a child from its card's rect, and never feed a measured height
  into the next frame's layout; a measurement carried across frames oscillates.
- Keep interactive targets at or above the design floor. A window that can be
  made smaller than the design size, or a target below it, is unreachable by the
  panel keyboard and pointer rather than merely tight.

## Data and files

- Generated parameter and state files stay untracked. A machine writes them, and
  a committed copy overwrites the operator's file on the next checkout.

## Maintaining this file

A rule belongs here only when a defect of that class reached the machine or the
screen in this repository — a review finding the author fixed, a fix commit, a
cleanup that removed the pattern across the codebase, or a solution record under
`docs/solutions/` — and no check already enforces it. Commands, setup, and
anything a test or a script enforces belong in AGENTS.md or the check itself.

State the rule as a condition that stays true: no commit hashes, no counts, no
symbol names that a refactor will rename. Group it under the area that owns the
failure. When a rule's full story is a solution record, link it instead of
restating the investigation. Retire a rule when a check starts enforcing it or
the code it guarded is gone, and name the evidence for the retirement.
