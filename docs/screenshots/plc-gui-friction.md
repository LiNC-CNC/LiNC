# PLC GUI friction log

What the running PLC GUI made hard, and what was done about it. The
screenshots beside this file are the acceptance frames.

## Phase 2 — watch, force, trend

- **Watch keys were dead while the editor was open.** The program-list screen
  set `keys_blocked` for the workbench too, and the watch handler treated that
  as "a dialog owns the keyboard". Only a real dialog blocks now; the pages the
  workbench draws own their keys.
- **A new numbered page chord looked dead when a POU list handler consumed it.**
  `Ctrl+N` matched `Ctrl+Shift+N` logically, so New Project inserted a rung.
  Chords with Shift are read before the plain chord.
- **Saving went somewhere else.** `Ctrl+S` always wrote a copy under
  `configs/plc/` while the page read `configs/plc-sim.json`, so comments looked
  lost. Save is in place; `Ctrl+Shift+S` is Save As.
- **The daemon fails silently on a project that does not compile.** It binds no
  I/O and keeps scanning, so every pin reads 0. The UI now validates before
  Load and shows the first error, but the daemon still only logs a warning.
- **The daemon writes the loaded project back to `--project`.** A test run
  rewrote `configs/plc-sim.json`, including a blank coil and a stray POU. Runs
  should not save what the operator did not save.
- **New rows copied the first output as their coil**, giving a second write of
  the same coil and an invalid project. A row now takes an unused writable
  symbol or creates `auxN`.
- **Trend lanes were the first eight snapshot variables**, which were all
  inputs. Lanes now cover every symbol, inputs and outputs interleaved, and the
  list scrolls instead of stopping at eight.
- **The watch list only reached the daemon after a save and a reload.** The
  local trend still traces the on-screen list, but the daemon window (the only
  one with a visible time span) needs the saved project.
- **The UI asked for `force_word` while the installed `linc-plc` did not know
  it**, and the daemon answered "unknown variant". The daemon binary is not
  built by `cargo build -p linc-app`; rebuild `linc-plc` before acceptance.
- **A flat ET lane proves less than it looks.** A timer that is not timing draws
  `ET` at 0 on a lane scaled 0..5000 (`T#5s`), so the range label, not the
  line, is the evidence that the scale comes from the preset.

Screenshots:

- `plc-watch-edit.png` — Enter edits the selected operand in place.
- `plc-force-cycle.png` — one BOOL through forced TRUE, forced FALSE, released.
- `plc-int-force.png` — `MW2` holds the typed value 123.
- `plc-trend-window-4s.png` — 4 s daemon window, boolean lanes, scaled `TOF0.ET`.
- `plc-trend-window-100s.png` — the same traces over 100 s.

## Open

- `linc-plc` still treats a non-compiling project as a warning and publishes
  empty I/O.
- `linc-plc` still persists the loaded project to `--project`.
- A new I/O pin needs a service restart before it binds; the banner says so,
  but there is no rebind action.
- ZX-derived limits remain: four instances per block type, 16 watch rows,
  8-character names.
