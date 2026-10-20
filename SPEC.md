# LiNC Specification

Status: design reference  
Scope: Rust `egui` HMI for LinuxCNC with SZGH-1080 physical panel and QMK CNC keyboard  
Canonical visual: `svg/machine-dashboard.svg`

---

## 1. Goals

Build LiNC, a focused, operator-grade HMI that:

- puts all machine control on the **physical SZGH panel**
- uses the **QMK CNC keyboard** for text entry, navigation, and screen selection
- keeps the screen readable at a glance and rich where detail matters
- exposes G-code, toolpath, offsets, tools, alarms, and diagnostics clearly
- remains outside the machine safety path
- can be implemented in Rust with `eframe`/`egui` without Qt, PyQt, VTK, or QtPyVCP

The visual target is industrial-controller-inspired: FANUC status discipline,
SYNTEC monitor/simulation richness, and Siemens navigation/typography. It is
not a pixel copy of any vendor's interface.

## 2. Non-goals

- No on-screen machine control panel.
- No replacement for the hardware E-stop chain.
- No multi-channel or multi-path control unless a future machine requires it.
- No vendor-specific FANUC/SYNTEC/Siemens G-code dialect support beyond what
  LinuxCNC actually implements.
- No full FANUC macro-B, Siemens cycle, or SYNTEC PLC compatibility layer.
- No 3D stock-removal simulation in the first implementation.

## 3. Hard constraints

### 3.1 Keyboard

Source:
`../qmk_firmware/cnckeyboard/keyboards/cnckeyboard/keymaps/doubletap/keymap.c`

Available navigation keys:

| Keyboard key | USB keycode | HMI action |
|---|---|---|
| `MAIN` | `F5` | Machine dashboard |
| `FILE` | `F6` | Program page; press again to cycle `SPLIT`/`CODE` |
| `TOOL` | `F7` | Tools page |
| `OFFSET` | `F8` | Offsets page |
| `WIN` | `F10` | Diagnostics |
| `ESC` | `Esc` | Back / cancel / close detail |
| `Ctrl+Q` | `Ctrl+Q` | Guarded quit |
| Arrows | — | Focus/lists/scroll/parameter adjustment |
| `PageUp` / `PageDown` | — | Page or tab navigation |
| `Home` / `End` | — | First/last row or line start/end |
| `Enter` | — | Select / apply / execute MDI |
| `Backspace` | — | Edit text |
| `MO(1)` | layer key | Symbols and mouse layer |

Hard limitations:

- No `F1`–`F4`, `F9`, `F11`, `F12`.
- No `Tab`.
- No `Shift`.
- No `Delete`.
- `UPPERCASE 0`: letters are emitted lowercase. The G-code editor/display
  normalises words to uppercase.
- F1–F8 softkey bars cannot be driven by this keymap. On-screen actions must
  be focusable with arrows/Enter and may also be clicked with the mouse layer.
- Do not depend on `Ctrl+C`/`Ctrl+V`/`Ctrl+Z`. Provide on-screen edit actions
  and line-based editing.
- `F10` may be intercepted by the window manager. The HMI runs kiosk/fullscreen
  or the WM is configured to pass it through.
- `Ctrl+Q` must not quit during motion or a running program; confirm and/or
  ignore while busy.

### 3.2 Physical panel

Source: `../szgh-panel-rust/` and `../baiermill_pb/szgh_panel_hal.py`.

The SZGH-1080 panel is connected over RS-485 (115200 8N1) and exposed as a
LinuxCNC HAL component named `szgh_panel`.

The Rust component currently exposes:

- 46 button pins
- 56 LED pins
- 2 seven-segment tool displays
- 1 two-digit status seven-segment display (`sseg_status`)
- 8 mode pins: `mode_edit`, `mode_auto`, `mode_mdi`, `mode_jog`, `mode_mpg`,
  `mode_inc`, `mode_home`, `mode_proghome`
- home/proghome outputs
- feed, rapid, and spindle override floats
- `panel_connected` and `communication_error`
- velocity inputs for jog LEDs

Panel ownership rules:

- The panel is the only machine-control surface: mode, cycle start/stop,
  single block, K4 stop, jog, rapid, spindle, coolant, tool changer, chuck,
  chip conveyor, work lamp, lube, air blow, safe door, overrides, and E-stop.
- The HMI **must not** open `/dev/ttyUSB0` while the panel component is running.
- The HMI **must not** duplicate panel controls as on-screen buttons.
- The HMI reads panel state read-only and displays it.
- The panel component and HMI are separate processes. The HMI crashing must not
  remove the panel's ability to stop the machine.
- Existing HAL glue must be migrated from `qtpyvcp.*` pins to `halui` and/or a
  small `linc` HAL component.

Special cases:

- The two-digit status display defaults to the **active work coordinate
  system**, `54`–`59`. It is the clearest use of two digits and is
  safety-relevant during setup and PROG.HOME. Show `--` when the value is
  unknown, stale or outside those offsets; do not guess.
- The status display is driven by a display-only status bridge from
  LinuxCNC `g5x_index` to `szgh_panel.sseg_status`; the HMI itself does not
  write panel outputs. Optional configured sources are spindle load percent
  and `MM`/`IN` units.
- The RS-485 protocol exposes `mode_mpg` and `mpg_sim`, but **not handwheel
  encoder counts**. Verify where the MPG encoder is wired; do not assume the
  panel link carries MPG pulses.
- `program_protect` must make the editor read-only and block offset/tool writes.
- `panel_connected == false` must show a persistent banner and block
  machine-modifying UI actions. The hardware E-stop remains independent.

### 3.3 No on-screen machine controls

Forbidden on screen:

- mode selection
- cycle start / feed hold / stop / reset
- jog controls
- spindle CW/CCW/stop/orient
- coolant on/off
- override sliders
- tool change
- software E-stop

Allowed on screen:

- read-only state indicators
- program/file management
- G-code editor and toolpath
- offset and tool tables
- MDI text entry, only when the physical panel is in MDI mode
- diagnostics, filtering, log export/copy, view clearing
- layout/view selection

## 4. Architecture

```text
SZGH panel --RS-485--> szgh-panel-hal (Rust HAL component) --> HAL --> halui/LinuxCNC
QMK keyboard --USB--> linc (Rust egui) --NML or PyO3--> LinuxCNC
LinuxCNC status --------------------------------> linc
szgh_panel.* HAL pins ---------------------------> linc (read-only)
```

### 4.1 Processes

- `szgh-panel-hal`: owns the serial port, produces button/mode/override pins,
  consumes LED/7-segment/velocity pins. Separate process from the HMI.
- `linc`: Rust `egui` application. Owns no machine-control hardware.
- `linuxcnc`: task/motion/IO. Authoritative machine state.
- HAL glue: standard `halui` plus a small `linc` component for
  app-specific inputs/snapshots.

### 4.2 Rust workspace

```text
LiNC/
  crates/
    linc-core/        # state, commands, backend trait, validation
    linc-linuxcnc/    # PyO3 official module; optional C++ NML bridge
    linc-hal/         # panel-state snapshot, HAL FFI, linc pins
    linc-ui/          # egui shell, theme, screens, widgets
    linc-view/        # 2D/3D toolpath and sparklines
    linc-app/         # main, config, logging
```

### 4.3 Backend boundary

```rust
pub trait MachineBackend: Send + 'static {
    fn snapshot(&self) -> MachineSnapshot;
    fn send(&mut self, command: MachineCommand) -> Result<(), MachineError>;
}
```

The UI never talks to NML or HAL directly for machine commands. A worker thread
owns the backend and publishes snapshots over a channel.

Preferred first bridge: **PyO3 + official `linuxcnc` module**. It is stable and
already handles NML serial/echo. The C++ NML bridge is a later replacement if
Python must be removed. `linuxcnc-grpc` is only for a remote panel, not the
local HMI.

### 4.4 Simulation panel

The simulation build also runs a separate operator-panel window. It is not part
of the HMI shell and is never present in the real-machine build.

- Reproduces the SZGH-1080 front panel: POWER, E-stop, MODE, feed/rapid/spindle
  overrides, axis/alarm lamps, tool displays and key matrix.
- Creates the same `szgh_panel` HAL component/pin surface as the real driver,
  plus simulation-only power/E-stop inputs.
- The HMI only reads HAL/status; all simulated machine commands originate in
  this companion window or the keyboard, never in the HMI central page.
- It may live on a second monitor. The calibration/mockup is
  `svg/sim-panel.svg`.
- A lathe-specific panel mockup still needs turret and live-tool controls;
  until then, the lathe simulation profile runs without a panel window.

## 5. Application shell

Fixed shell, no right-hand status column:

- **Top status bar**: machine name, physical mode, program, tool, spindle,
  feed, work offset, panel link, alarm/message counts, time.
- **Left navigation**: Machine, Program, Offsets, Tools, Diagnostics, G-code
  Help. Keyboard-focusable; `MAIN`/`FILE`/`OFFSET`/`TOOL`/`WIN` jump directly.
- **Central page**: active screen.
- **Bottom action bar**: real keycaps (`MAIN`, `FILE`, `OFFSET`, `TOOL`, `WIN`,
  `ESC`), context actions including `HELP`, and a compact read-only machine
  status line (`OVR F/S/R`, program protect, E-stop, alarm count).
- **No persistent panel-status column.** Panel detail belongs in Diagnostics.

Focus rules:

- Visible focus ring at all times.
- Arrow keys move focus within the current region.
- `Enter` activates; `Esc` backs out.
- No hover-only affordances.
- No Tab dependency.
- On-screen actions have a minimum 44 px target at 1920×1080.

## 6. Screens

### 6.1 Machine

Default page. The layout is selected by `machine.gui`.

#### Mill

- **3-axis default**; optional 4th/5th axis (`A/B/C` or `U/V/W`) when present
  in the LinuxCNC INI. Axis order and count come from `COORDINATES`/axis mask,
  not a duplicated list.
- Axis cards expand from 3 to 5: 3 large cards at 3-axis; compact cards at
  4/5-axis. Never invent an axis that the INI does not expose.
- Large 3D toolpath: stock, loaded program path, live tool trail in a distinct
  color, rapid path, tool marker, axes, zoom, program/machine bounds and
  selected-line highlight. `CLEAR TRAIL` clears only the live trail.
- Program card, feed/spindle-load gauges, machine health and alarms.
- ATC state in Tools/Diagnostics; mill tool table uses pocket/orientation.

#### Lathe

- Base X/Z axes; optional C spindle axis and Y axis when exposed by the INI.
- 2D XZ toolpath by default; add a 3D view only when Y/C or live-tool paths
  make it useful. Program/machine bounds and selected line remain visible.
- Main spindle plus optional sub-spindle; C-axis mode and live-tool state are
  explicit in the top/bottom status and machine card.
- Power turret card: station number, clamped/unclamped/indexing state, tool
  description, live-tool RPM/direction and axial/radial orientation.
- Feed/spindle-load gauges and live-tool load when available.
- Compact panel state in top/bottom bars only; no machine controls.

### 6.2 Program

Two layouts, selected by the header toggle or `FILE`:

#### SPLIT

- G-code editor/monitor left (~58%).
- Mill: 3D toolpath right (~42%); lathe: 2D XZ toolpath, with optional 3D when
  Y/C or live-tool paths are active. The loaded program path and live trail are
  separate colors; `CLEAR TRAIL` clears only the live trail.
- Live-tool paths use a distinct stroke from turning/rapid moves.
- Current line synchronized with the graph.
- Current/next block and modal summary below the editor.

#### CODE

- G-code editor dominant (~80%).
- Mini XY graph top-right.
- Program info card below the mini graph: cursor/current block, next block,
  modal summary, buffer state and line count.
- Current line synchronized with the mini graph.
- The CODE layout is reused for editing; there is no separate editor window.

#### EDIT state

The physical panel's `EDIT` position selects Program, switches to the CODE
layout and focuses the editor. The layout and graph stay the same; the buffer
becomes editable.

- Editor buffer with cursor, selection, line numbers, current-line highlight and
  live token highlighting.
- Dirty marker in the header and buffer card; `SAVE` is accented until saved.
- `Ctrl+S` saves; `Ctrl+Z`/`Ctrl+Y` undo/redo; `Ctrl+F` searches;
  `Ctrl+O` opens; `Ctrl+A/C/X/V` select/cut/copy/paste.
- No `Tab`, `Shift` or `Delete` key: use spaces, the mouse layer for selection,
  Backspace, and cut/paste. Line-based operations are required.
- The mini graph updates from a separate debounced preview parser; invalid
  G-code keeps the last valid graph and marks the line. Preview never moves the
  machine.
- Saving is explicit; no autosave. Save writes the loaded file, clears the dirty
  marker and must not run while a program is active.
- Unsaved changes must be confirmed before leaving Program, opening another
  file, or quitting.
- G/M/axis words are displayed uppercase; comments retain their case and the
  lexer is case-insensitive.

Program rules:

- G-code is token-highlighted (see §7.3).
- The graph always draws the **loaded program's full toolpath** and the
  **live toolpath** as separate layers. The program path uses the program
  colour (cut) and muted dashed rapids; the live trail uses a distinct
  `--success` stroke. The legend must identify both.
- The live trail is the actual tool-tip path since the last clear, including
  program, MDI and jog motion. It is not the commanded program path and must
  never overwrite or hide the loaded program.
- A focusable, view-only `CLEAR TRAIL` action clears only the live trail and
  re-seeds it at the current tool tip. It does not clear the loaded program,
  selected line, offsets or machine state, and is safe while running. A
  successful new program load starts a fresh trail.
- The graph overlays program bounds and machine bounds. Program bounds are a
  dashed cyan box around the last valid program extents; machine bounds are a
  dashed muted envelope from the INI travel limits and kinematics. Both use the
  same view transform as the toolpath.
- The selected/current G-code line is highlighted in the graph: its segment or
  arc uses a brighter stroke and a line-number tag, distinct from the execution
  marker.
- On editor changes, wait for the debounce, run the preview syntax check, then
  replace the graph only if it succeeds. On failure, keep the last valid graph,
  mark the offending line and show a syntax error; never move the machine.
- While running, the editor is read-only and auto-scrolls to the current line.
- Edits and saves are allowed only when stopped, `program_protect` is off and
  the panel is in `EDIT`.
- The physical panel's **PROGRAM PROTECT** switch is authoritative. While it
  is on, the controller is editing-locked: no cursor edits, saves, offset/tool
  table writes, file imports, or probe-result applies. Show a persistent
  `PROTECT ON — EDITING LOCKED` state and disable the affected actions.
- Protect must fail closed: an unknown or stale protect input is treated as ON.
  Every write rechecks it in the backend; a value sampled in the UI is not
  sufficient.
- If protect turns on with unsaved changes, keep the buffer and dirty marker,
  do not discard or auto-revert, and keep it read-only until the switch is off.
- Program load remains readable; program protect blocks writing, not viewing.
- `FILE` enters Program; outside EDIT it cycles `SPLIT`/`CODE`; in EDIT it
  selects CODE.
- `LOAD` or `Ctrl+O` opens Files; `ESC` returns to Program.
- A successful file load always returns to Program in `SPLIT` view, even if
  the previous layout was `CODE` or the panel is in `EDIT`.
- The header segmented control is also focusable.
- No on-screen cycle start; loading a program does not start it.

### 6.3 Offsets

- Work offsets G54–G59.
- G92 offset.
- Tool offsets.
- Mill: X/Y/Z and rotary offsets; lathe: X/Z geometry and wear, tool nose
  radius, tip orientation, C/Y offsets and turret station.
- Current axis values and distance-to-go.
- Read-only while running; blocked when program protect is active.
- Edits validated against LinuxCNC limits and units.
- A `PROBE` action opens the Touch Probe page; probing results can update the
  selected work offset only after an explicit `APPLY RESULT`.

### 6.4 Tools

Primary quick tool-change surface; no MDI entry required.

- Tool table: number, description, diameter/length or lathe geometry/wear,
  pocket/station and type. Selected row or a typed target number becomes the
  target tool.
- Current tool, next/prepared tool and spindle state are always visible.
- `STAGE CHANGE` prepares `Tn M6` through the internal command path and shows
  `WAITING FOR PHYSICAL CYCLE START`. It never executes the change.
- `T0` unload and manual-change confirmation are supported. `SET CURRENT`
  (`M61 Qn`) is available for a tool already in the spindle but is explicitly
  labeled as not a tool change.
- Manual changer: after CYCLE START, show `REMOVE current / INSTALL target` and
  a `CONFIRM INSTALLED` action. Confirmation uses the dedicated tool-change
  backend/HAL output, not an arbitrary HAL write.
- ATC/turret: optional and lower priority. Use the existing `toolchange.ngc`
  and `m6_tool_call_*` routines; the HMI only stages the `M6` and renders state.
  Show carousel/rack pockets, current pocket, clamp/unclamp and faults when
  exposed. ATC disabled/unavailable states are explicit.
- Lathe: power-turret station grid, axial/radial live-tool flag, live-tool RPM
  range/direction and driven-tool state.
- Tool table writes and staging require idle; table writes also require program
  protect off. Running programs leave the page read-only.

### 6.5 Diagnostics

Read-only detail view. Tabs:

| Tab | Content |
|---|---|
| Overview | Health cards, active alarms/messages, panel summary, motion health, event log |
| Panel Link | RS-485 transport, cycle time, last RX, RX/TX counts, CRC errors, timeouts, reconnects, mode/overrides, raw button/LED bitmaps, 7-segment values |
| HAL / NML | NML command/status/error channel state, ages, echo serial, HAL components/pins, thread health |
| Motion | Following error, commanded vs actual, servo load, spindle load/speed, limits/homing |
| Turret / Live Tool | Turret station, clamp/index state, live-tool drive, RPM/direction, axial/radial, C-axis |
| I/O | Panel LEDs, machine inputs, coolant/lube/door/ATC/turret states, HAL pin browser |
| Logs | Searchable event history, severity/source filters, export/copy |

No fault reset, acknowledge, cycle, jog, override, or E-stop controls here.
`CLEAR VIEW` clears only the UI log view, never machine faults.

### 6.6 G-code Help

Offline LinuxCNC G-code reference, available without network access and without
stopping a running program.

- Quick-reference index grouped by Motion, Canned cycles, M-codes, O-words and
  Other codes.
- Search matches code, parameters, description and meaning. Code matching
  ignores spaces and commas, so `g2g3` finds `G2, G3`.
- Detail pane: title, description, parameter table, figures, notes and related
  codes. Internal links navigate within the reference; Backspace returns through
  link history.
- Current bundle: 125 sections, 89 index rows, 14 figures, generated from the
  pinned LinuxCNC online documentation.
- Read-only; no machine-control actions. The top-level HELP action opens this
  page. No dedicated F1–F8 key.

### 6.7 Files

Two-pane file browser opened from `LOAD` or `Ctrl+O`; `ESC` returns to the
previous page.

- Left pane: removable USB media. Auto-mount on insert via `udisks2` over
  D-Bus; no `sudo`, no shell commands.
- Right pane: `NC_FILES`, resolved from LinuxCNC `PROGRAM_PREFIX`; never a
  duplicated path in HMI config.
- Show device label/capacity, free space, mount state (`MOUNTING`, `MOUNTED`,
  `NO USB`, `ERROR`) and a clear `EJECT`.
- Mount removable filesystems with `nosuid,nodev,noexec`; never auto-run files
  from USB.
- Copy selected regular files both ways: `COPY TO USB` from `NC_FILES` and
  `COPY TO NC_FILES` from USB. Folder navigation only; no recursive folder copy
  in v1.
- Copy requires free space, rejects symlinks/path escapes, confirms overwrite,
  writes to a temporary file in the destination and renames atomically.
- Copy progress and errors are visible; `EJECT` is disabled while a copy is
  active.
- `EJECT` unmounts and powers off the device; never force-unmount by default.
  Show `SAFE TO REMOVE` after success.
- `LOAD` accepts only configured G-code extensions from `NC_FILES`; it loads
  but never starts the program. Loading and writes to `NC_FILES` are disabled
  while running, and imports are blocked by program protect.
- On a successful load: close Files, switch to Program, force the `SPLIT`
  layout, fit the toolpath and highlight the current line. This overrides the
  remembered Program layout, including `CODE`. On failure, stay in Files and
  show the error.
- If the editor has unsaved changes, confirm discard before replacing the
  loaded program.
- Delete, rename, format and folder-copy are intentionally out of scope for v1.

### 6.8 Touch Probe

Mill-variant interface for touch probing, opened from Offsets. It reuses the
existing Probe Basic G-code subroutines and math and follows Probe Basic's
category-first workflow: choose a routine group, then pick the operation card.

- Category strip, matching Probe Basic: Outside Corners, Inside Corners,
  Boss and Pocket, Ridge and Valley, Edge Angle, Rotary Axis, Surface/Z,
  Calibrate and Probe Help.
- Each category opens a grid of illustrated operation cards plus a selected
  operation, diagram and G-code call. Work offset, probe mode and parameters
  stay visible across categories.
- Edge operations (X±, Y±, Z surface) remain available within the relevant
  category; calibration includes round/square boss/pocket and reset.
- Parameters mirror `touch_probe_param_update.ngc`: probe tool number,
  fast/slow/traverse feed, max XY/Z distance, XY/Z clearance, extra depth, step
  off width, edge width, diameter hints, probe mode, WCS rotation, calibration
  offset/diameter and square-calibration axis.
- Work-offset selector G54–G59. `MEASURE ONLY` must be available in addition to
  applying the result to the selected offset.
- `PREPARE MDI` builds/queues the selected `o<probe_...> call` and its argument
  values. It does not move the machine. The operator must press the physical
  CYCLE START key.
- Probe motion requires: MDI mode, homed axes, probe tool T99 in the spindle,
  no active program, and valid parameters. Errors keep the previous result and
  show the failing condition.
- Read results from the subroutine outputs (`#1001`–`#1009`) and show measured
  value, error, selected WCS, and history.
- `APPLY RESULT` writes the selected work offset only when idle and program
  protect is off. Calibration routines update the probe calibration offset;
  `RESET CAL` clears it through `probe_cal_reset.ngc`.
- The page has no cycle-start, jog, override or E-stop controls. The physical
  panel remains the only motion command source.

### 6.9 MDI

Mode-driven screen. Moving the physical mode selector to `MDI` opens this page
immediately, focuses the command line and shows `PANEL MODE: MDI`; moving out
closes it and discards any staged, unrun block. No left-nav item is added.

Industry-standard layout:

- Large single-block command input with line number and G-code token colours.
- Quick-insert chips for G0/G1/G2/G3, M3/M5/M6/M8/M9, G54, G38.2 and T;
  variant-specific chips for lathe (G7/G8, G95/G96).
- Position/DRO and modal G-code panel.
- Staged-command panel with syntax status, requirements and last result.
- Local MDI history with timestamps; Up/Down recalls prior commands.

Rules:

- `Enter` or `STAGE MDI` stages one block; it never starts motion.
- Only the physical CYCLE START key runs a staged block.
- Staging requires panel MDI mode, homed axes, idle interpreter and no active
  program. A syntax error keeps the previous staged/last result and marks the
  input.
- Leaving MDI, pressing `ESC`, or clearing discards the staged block.
- History is HMI-local, bounded (`mdi.history_limit`), and not a machine log.
- No on-screen cycle start, jog, override, spindle, coolant or E-stop controls.

### 6.10 Homing

Mode-driven screen. Selecting `HOME` or `PROG.HOME` on the physical panel opens
it automatically; leaving those positions closes it. The title/context changes
between `HOMING` (machine home) and `PROGRAM ZERO` (`PROG.HOME`). The HMI only
observes, explains and logs the panel behavior; it never issues a home command.

- Axis cards show `HOMED`, `HOMING`, `NOT HOMED`, the INI `HOME_SEQUENCE` and the
  search/latch/index state. Optional axes are shown only when configured.
- HOME mode mapping, exactly as `baiermill_pb`:
  - `RAPID` -> `home-all`;
  - `X+`/`X-` -> `home-x`;
  - `Y+`/`Y-` -> `home-y`;
  - `Z+`/`Z-` -> `home-z`.
  - Individual home outputs are disabled while any axis is homing; the panel
    driver/postgui performs the gating.
- PROG.HOME mode mapping, exactly as `baiermill_pb`:
  - `RAPID` -> `proghome-all` -> `issue_mdi("G0 X0Y0Z0")`;
  - `X+`/`X-` -> `proghome-x` -> `G0 X0`;
  - `Y+`/`Y-` -> `proghome-y` -> `G0 Y0`;
  - `Z+`/`Z-` -> `proghome-z` -> `G0 Z0`.
  - This is program zero in the current WCS, not machine home and not `G53`.
  - X/Y/Z only; optional A/C axes are not moved by these panel actions,
    matching the current Probe Basic handlers.
  - The panel-owned path issues MDI immediately through the existing
    `issue_mdi` action; the HMI must not intercept, restage or add a cycle-start
    gate to it. It still requires machine ON, all axes homed and interpreter
    idle, or the action is refused.
- Homing order comes from the INI; the current machine is Z first, then X/Y.
  `NO_FORCE_HOMING = 0` means programs/MDI are blocked until homed.
- The page shows machine position, limits, homed count and sequence completion.
  It provides no on-screen home, unhome, jog or override buttons.

### 6.11 Settings

- Theme/units/display density.
- Keyboard reference.
- Machine profile/read-only INI summary.
- Panel link information.
- About/support.

### 6.12 Manual modes: JOG / INC / MPG

Mode-driven screen shared by the panel's `JOG`, `INC` and `MPG` positions.
Entering any of those positions opens it; leaving them closes it. No left-nav
item is added. The screen is read-only: all jog, increment, handwheel, mode and
override input stays on the physical panel.

- **Large graph window** is the dominant element: mill 3D or lathe XZ
  toolpath, axes, grid, stock/program bounds where available, the loaded
  program path, live tool marker and a short position trail in a distinct
  color. The view follows the machine position without jumping; the last
  program path stays visible as a muted reference.
- `CLEAR TRAIL` is a view-only, focusable action in the graph header; it clears
  only the past live trail and re-seeds at the current position. It never
  clears the loaded program or changes machine state.
- **Large, readable coordinates** are always visible beside the graph:
  work coordinates (primary), machine coordinates (secondary) and
  distance-to-go, with units and sign. Work coordinates use a large monospace
  face; machine/DTG values may be smaller but must remain legible at the target
  display.
- Optional axes follow the INI. Lathe X is shown in the configured
  radius/diameter convention, with the convention labeled.
- The active axis is highlighted from the panel's selected-axis state. Unknown
  selection is shown as `—`, never guessed.
- Mode context:
  - `JOG`: `CONTINUOUS`, active axis, jog speed and rapid state.
  - `INC`: increment size, active axis and remaining step where available.
  - `MPG`: active axis, handwheel scale and step mode. The RS-485 link does not
    carry encoder counts; do not fabricate a handwheel count or velocity.
- A persistent `PANEL MODE: JOG|INC|MPG` label plus `NO ON-SCREEN JOG`
  reminder. No mode buttons, jog buttons, override sliders or E-stop.
- The graph updates at the normal 10–20 Hz position rate; no animation loop is
  required while idle. When the panel mode or axis selection is unknown after
  reconnect, show `UNKNOWN` until a fresh panel state arrives.

## 7. Visual system

### 7.1 Theme

Dark graphite with cool-blue active optics.

| Token | Value | Use |
|---|---|---|
| `--surface` | `#141619` | panels/windows |
| `--fg` | `#e8eaed` | primary text |
| `--muted` | `#9aa0a6` | labels, inactive icons |
| `--border` | `#30353a` | panel edges/tracks |
| `--accent` | `#4da3ff` | active, focus, toolpath, gauges |
| `--success` | `#3ddc84` | online, normal, OK |
| `--warning` | `#ffb020` | messages, hold, elevated load |
| `--error` | `#ff5c5c` | alarms/faults |

Rules:

- Blue is interaction/observation; not alarm state.
- Active chips use dark text on blue.
- Red only for actual faults.
- No decorative gradients or idle animation.
- Use shapes/text as well as color for state.

### 7.2 Typography

- Labels: sans-serif, 9–15 px equivalent.
- Numeric/DRO: monospace, tabular figures, 10–24 px.
- Section headers: sans-serif bold.
- No condensed low-contrast text for critical values.
- Scale proportionally at 1920×1080; preserve 44 px targets.

### 7.3 G-code syntax highlighting

| Token | Variable | Value |
|---|---|---|
| G-codes | `--c1` | `#67e8f9` cyan |
| M-codes | `--c2` | `#c084fc` violet |
| Axis letters | `--c3` | `#fbbf24` amber |
| Numbers | `--fg` | `#e8eaed` ivory |
| Feed/speed | `--c4` | `#34d399` green |
| Tool | `--c5` | `#fb923c` orange |
| Parameters/radius | `--c6` | `#f472b6` pink |
| Program number | `--c6` | `#f472b6` |
| Comments | `--muted` | `#94a3b8`, italic |
| Line numbers | `--muted` | `#6b7280` |
| Current block | `--accent` | 16% background + left bar |

Implementation:

- Use a G-code lexer, not one regex.
- Token classes: G, M, axis letter, number, F/S, T, R/I/J/K/parameters, `#`
  variables, comments, program number, line number.
- Keep numeric values neutral; do not rainbow numbers.
- Highlight visible lines only; cache tokens by buffer revision.
- Use `ropey` for the editor buffer.
- Current/next blocks in side cards use the same token colours.
- Do not use `--error` for syntax; reserve it for real errors.

## 8. Interaction model

### 8.1 Keyboard navigation

- `MAIN`: Machine.
- `FILE`: Program; press again to cycle `SPLIT`/`CODE`.
- `TOOL`: Tools.
- `OFFSET`: Offsets.
- `WIN`: Diagnostics.
- `HELP` action: G-code Help; no dedicated function key.
- `LOAD` / `Ctrl+O`: Files, when idle and allowed.
- `PROBE` action: Touch Probe from Offsets; prepare only, never start motion.
- MDI mode: `Enter` stages the block; `Esc` clears it; `Up`/`Down` recall
  history; physical CYCLE START runs it.
- `ESC`: back/cancel; on Help, clear search or return from detail history.
- Arrows: focus/list/scroll.
- `/`: focus G-code Help search when Help is active.
- `PageUp`/`PageDown`: tabs/pages.
- `Home`/`End`: first/last item.
- `Enter`: select/apply/MDI execute; on Help, open the selected code; in Files,
  open a folder or load an `NC_FILES` file.
- `Backspace`: edit; on Help detail, go back through link history; in Files, go
  up one folder.
- `Ctrl+R`: refresh the Files listing.
- `MO(1)`: symbols/mouse.

No F1–F8 mapping. The bottom keycap highlight reflects the current page:
`FILE` active in Program, `WIN` active in Diagnostics, etc.

### 8.2 Mode and command gating

- The physical panel mode switch is authoritative.
- The GUI displays panel mode and LinuxCNC task mode; if they differ, show both
  only when useful.
- MDI is enabled only when the panel is in MDI mode; the MDI screen opens on
  that selector position and closes when it leaves.
- HOME/PROG.HOME open the Homing screen; all homing/program-zero motion
  originates in the panel's home/proghome pins and keys. The HMI never homes an
  axis and never intercepts the panel's existing direct `issue_mdi` PROG.HOME
  path.
- JOG/INC/MPG open the Manual screen with the large graph and coordinate
  readout. Jog, increment and handwheel motion remain panel-only.
- The HMI MDI screen stages only; its block runs only after physical CYCLE
  START. The PROG.HOME panel action is a separate, existing panel behavior and
  is not routed through that staging screen.
- Program start is physical CYCLE START only.
- Overrides are physical switches; the GUI displays values.
- Jog is physical only.
- Program protect is a physical panel input. It blocks every controller-side
  write and is treated as ON when unknown or stale.
- The GUI may load programs, edit G-code, edit offsets/tools, and run MDI
  within the gating above.

### 8.3 Panel offline

If `panel_connected` is false:

- show a red/amber banner immediately;
- block machine-modifying UI actions;
- keep diagnostics available;
- never replace the panel controls with screen controls;
- hardware E-stop remains independent.

## 9. Data model

```rust
pub struct MachineSnapshot {
    pub mode: Mode,
    pub state: MachineState,
    pub positions: Axes<f64>,
    pub dtg: Axes<f64>,
    pub offsets: WorkOffsets,
    pub tool: ToolState,
    pub spindle: SpindleState,
    pub feed: FeedState,
    pub program: ProgramState,
    pub alarms: Vec<Alarm>,
    pub panel: PanelSnapshot,
}

pub enum MachineCommand {
    Mdi(String),
    LoadProgram(PathBuf),
    SetWorkOffset { index: u8, offset: [f64; 3] },
    SetToolOffset { tool: u16, length: f64, diameter: f64 },
}
```

Machine-control commands (mode, cycle, jog, spindle, coolant, overrides) are
not part of the HMI command set.

Panel snapshot should include at least:

- connected/error
- mode
- feed/rapid/spindle override
- program protect
- E-stop
- tool/next tool
- raw button/LED bitmaps
- RX/TX, CRC errors, timeouts, last RX age, reconnects

The current Rust port exposes connection/error and decoded pins but not the
serial counters and raw bitmaps; those must be added for Diagnostics.

## 10. Performance

- Status snapshot: 10–20 Hz.
- Current-line sync: 10–20 Hz.
- Sparklines/event charts: 1 Hz or on data change.
- 3D toolpath: render only when visible; 30 fps maximum.
- No continuous 60 fps repaint when idle.
- No blocking file/serial/NML I/O on the UI thread.
- Cache text layout and visible-line tokens.
- Event log in a ring buffer; persist asynchronously, not on every event.

## 11. Safety

- The hardware E-stop chain is the only safety-rated stop.
- A software E-stop button is forbidden.
- No direct HAL writes for machine actions; use LinuxCNC commands/halui.
- Validate mode/state before MDI and program/offset/tool writes.
- MDI blocks are staged only; the HMI never auto-runs MDI. Physical CYCLE
  START is the only execution path.
- Homing is panel-only: no HMI home/unhome command, button or key. The HMI
  displays sequence/state and must not interfere with the panel's home pins or
  its PROG.HOME `issue_mdi` actions.
- Probing routines are prepared as MDI; only the physical CYCLE START key may
  start probe motion. Never auto-start a probe routine.
- Respect program protect and running state. Program protect is a panel
  switch, is rechecked by the backend on every write, and fails closed when
  unknown or stale.
- Manual modes are read-only: no HMI jog, increment, handwheel, mode or
  override command exists.
- Do not hide alarms behind tabs; persistent banner plus Diagnostics.
- HMI crash must not stop the machine or remove panel control.
- Panel disconnect must be visible and actionable by the operator.
- Guard `Ctrl+Q`; never quit during motion or a running program.

## 12. Acceptance criteria

For this design repository:

- `SPEC.md` and `README.md` exist.
- Only the accepted source SVGs are committed.
- Canonical mill Machine, lathe Machine, Program SPLIT, Program CODE,
  Diagnostics, G-code Help and Files screens are represented.
- Mill layout supports a 3-axis default and optional 4/5-axis expansion.
- Lathe layout represents X/Z, optional C/Y, live tooling and a power turret.
- The simulation-only SZGH-1080 panel is represented separately from the HMI.
- The Touch Probe screen represents routine selection, WCS/parameters,
  prepared-MDI state, results/apply and calibration without a software
  cycle-start control.
- The MDI screen opens at the panel MDI position and represents command staging,
  DRO/modal state, history and physical-cycle-start gating.
- The Homing/Program Zero screen opens at HOME/PROG.HOME, shows per-axis/sequence
  state and documents the exact physical-panel key mapping without providing
  home controls.
- PROG.HOME retains the current machine's `issue_mdi` behavior: RAPID sends
  `G0 X0Y0Z0`, axis keys send `G0 X0/Y0/Z0`, in the current WCS, X/Y/Z only.
- The Tools screen represents the tool table, current/next tool, a staged
  `Tn M6` change, manual confirmation and an explicit optional ATC/turret
  section, without requiring MDI entry.
- The Manual screen opens at JOG/INC/MPG, shows a large graph plus large work
  coordinates, machine coordinates and DTG, and provides no on-screen jog or
  mode control.
- Program protect ON locks program editing, saves, offset/tool writes, file
  imports and probe applies; unsaved changes are preserved, not discarded.
- The panel's status seven-segment display shows the active `G54`–`G59` offset
  as `54`–`59`, and `--` when unknown; it never fabricates a value.
- Hardware constraints for the QMK keyboard and SZGH panel are documented.
- No SVG shows an on-screen machine control panel.
- G-code mockups show token-level syntax highlighting.
- The Program CODE screen shows an editable buffer state: cursor, dirty marker,
  save action and mini-graph preview.
- The Files screen represents `NC_FILES` and removable USB side by side, with
  explicit copy, eject, mount-state and load actions.
- Loading an `NC_FILES` file returns to Program in `SPLIT` view with the
  toolpath fitted and the program not started.
- The Program graph shows the loaded program path and the live trail in
  distinct colors, with a focusable `CLEAR TRAIL` action that clears only the
  trail and re-seeds at the current position.
- The Program graph shows program bounds, machine bounds and a selected-line
  highlight; editing updates it only after a successful syntax check.
- Layered config profiles exist for the simulation and the physical BaierMill.
- The profiles reference the LinuxCNC INI as the source of machine data rather
  than duplicating limits, velocities, units, spindle/ATC data or tooling.

For the future implementation:

- Keyboard-only navigation works without Tab, Shift, Delete, or F1–F8.
- Panel and HMI are separate processes; only the panel owns RS-485.
- No on-screen machine controls exist.
- Program protect, panel offline, and MDI mode gating are enforced.
- Diagnostics shows real panel-link statistics and event history.
- Physical E-stop remains independent.

## 13. Open issues

1. `szgh-panel-rust` does not currently build with HAL enabled:
   - `Cargo.toml` patches to missing
     `patches/linuxcnc-hal-rs/linuxcnc-hal-sys`;
   - without the patch, crates.io `linuxcnc-hal-sys` fails in bindgen against
     current glibc headers.
2. The Rust panel port opens/closes the serial port every cycle. It should own
   one `Box<dyn SerialPort>` for process lifetime.
3. The HAL loads `./szgh-panel-hal -c 0.01`, but the protocol sleeps roughly
   14 ms per transaction; choose 0.02–0.05 s or reduce delays.
4. Add Diagnostics counters and raw bitmaps to `szgh-panel-hal`.
5. Verify the MPG encoder path; it is not carried by the RS-485 driver.
6. Choose the LinuxCNC bridge: PyO3 first, C++ NML later.
7. Confirm spindle-load source (`rio.spindle-modbus.spindle_load-abs` or VFD
   equivalent); never derive it from override.
8. Map panel `EDIT`/`HOME`/`PROGHOME` modes to UI state; they are not all
   LinuxCNC task modes.
9. Design the editor for no Shift/Tab/Delete; line-based operations are
   required.
10. Verify theme contrast and color-blind distinguishability on the target
    display.
11. Implement the layered config loader and `linc --check-config`;
    `configs/*.toml` currently define the contract only.
12. Unify the real panel driver's serial port and cycle time with
    `configs/baiermill.toml`; today they are hard-coded in the driver and
    `hallib/szgh_panel.hal`.
13. Convert the generated Python G-code help data
    (`../baiermill_pb/user_tabs/gcode_help/gcode_docs.py`) into a Rust-friendly
    bundle (JSON/TOML), pin the LinuxCNC docs version, and preserve
    documentation/image attribution.
14. Implement the G-code editor: `ropey` buffer, cursor/selection, undo/redo,
    find, atomic save, external-change detection, unsaved-change confirmation
    and a debounced preview parser. The preview must not use the machine
    interpreter or move the machine.
15. Verify editor usability with no `Shift`, `Tab` or `Delete`: mouse-layer
    selection, Backspace, cut/paste and line-based operations.
16. The panel mode pins are 0.5 s pulses, not a persistent mode index. The HMI
    must latch them to know when the selector is in `EDIT`; preferably the
    panel driver should expose a persistent mode state as well.
17. USB auto-mount depends on `udisks2` and D-Bus/polkit. The service is
    currently inactive on the development box; verify/enable it on the machine
    and test insert, mount failure, busy eject and power-off.
18. Implement file copy with canonical path checks, symlink rejection, free
    space checks, overwrite confirmation, temporary-file-plus-rename, progress
    and copy/eject mutual exclusion. Never invoke a shell for file operations.
19. Implement graph bounds and selected-line overlay: project program extents
    and INI machine limits into the current view, map selected lines to
    segments/arcs, and make preview updates conditional on a successful
    syntax check.
20. Map lathe power-turret station/clamp/index and live-tool drive/RPM/direction
    HAL pins; the current lathe simulator exposes neither. Test static and
    driven stations, axial/radial orientation and turret indexing interlocks.
21. Verify the mill 3/4/5-axis card layout against the actual INI axis mask and
    validate optional-axis hiding at runtime.
22. Add a lathe-specific simulator panel mockup with turret/live-tool controls;
    `svg/sim-panel.svg` is the mill SZGH-1080 panel.
23. Port the Probe Basic parameter contract (`#3014`–`#3036`) and result contract
    (`#1001`–`#1009`), map every routine to its G-code call, and verify that
    HMI-prepared MDI runs only after physical CYCLE START.
24. Decide lathe touch-probe/tool-setter support separately; the first Touch
    Probe page is mill-oriented (X/Y/Z and WCS).
25. Implement MDI staging with syntax validation, history recall and discard on
    mode exit; verify the HMI cannot execute MDI without physical CYCLE START.
26. Verify Homing UI against the panel's `home-*`/`proghome-*` pins and the
    existing `baiermill_pb` gating. Test Z-first homing, individual axes,
    PROG.HOME current-WCS moves, faults and optional-axis display.
27. Preserve the PROG.HOME direct `issue_mdi` path exactly (immediate panel
    action, X/Y/Z only, current WCS, no HMI staging); document the intentional
    exception to the HMI MDI staging rule.
28. Implement the manual tool-change acknowledgement path without arbitrary HAL
    writes; verify the existing manual-change prompt/confirm and `M61` set-current
    semantics.
29. Defer ATC/turret implementation. When added, reuse the existing `M6` remap
    and carousel/turret routines, expose real clamp/index state and never
    duplicate toolchange motion logic in the HMI.
30. Implement the Manual screen's JOG/INC/MPG mode latch and selected-axis
    source. The RS-485 link carries mode pulses and jog buttons, not a
    persistent selected axis; show `UNKNOWN` unless HALUI/pendant state is
    available. MPG counts must not be fabricated.
31. Implement program protect as a fail-closed backend gate: subscribe to the
    panel pin, recheck it for every program/offset/tool/file/probe write, and
    test the switch toggling with a dirty editor buffer and during an in-flight
    write.
32. Implement the loaded-program path plus live-trail layer and the
    `CLEAR TRAIL` view action. Sample actual tool-tip positions, cap the trail
    buffer, keep program and trail colors distinct, and verify that clearing
    never affects the loaded program or machine state.
33. Implement the display-only panel status bridge for the active WCS. The
    current Rust panel driver exposes only `sseg_magtool`/`sseg_spdtool`; add
    `sseg_status` and a source for `g5x_index`, with `--` on unknown/stale.

## 14. Accepted SVG manifest

Only the accepted versions are included.

| File | Description |
|---|---|
| `svg/machine-dashboard.svg` | Machine dashboard: dark graphite + blue, axis cards, 3D toolpath, program card, feed/spindle-load gauges, machine health, alarms |
| `svg/program-split.svg` | Program `SPLIT`: syntax-highlighted G-code alongside the 3D toolpath with program/machine bounds and selected-line highlight |
| `svg/program-code.svg` | Program `CODE`: editable syntax-highlighted G-code, cursor/dirty state, syntax-gated mini XY graph with bounds/selected-line highlight and program-info card; entered by panel `EDIT` |
| `svg/diagnostics.svg` | Diagnostics overview: health cards, alarms/messages, panel link detail, motion health, event log |
| `svg/gcode-help.svg` | G-code Help: offline quick-reference index, code search, detail pane, parameter tables, figures and related codes |
| `svg/files.svg` | Files: USB and `NC_FILES` panes, mount/eject status, file listing and explicit copy/load actions |
| `svg/lathe-dashboard.svg` | Lathe dashboard: X/Z/C/Y cards, XZ toolpath, power turret/live-tool card, spindle load and machine health |
| `svg/probe.svg` | Touch Probe: Probe Basic-style category strip and illustrated operation cards, WCS/parameters, prepared-MDI state, result/apply and calibration |
| `svg/mdi.svg` | MDI: single-block command, quick inserts, DRO/modal, staged state, history and physical-cycle-start gating |
| `svg/manual.svg` | Manual JOG/INC/MPG: dominant live graph, large work/machine/DTG coordinates and mode context; no on-screen jog controls |
| `svg/homing.svg` | Homing: per-axis/sequence state, HOME/PROG.HOME physical-key mapping and machine position; no software home controls |
| `svg/tools.svg` | Tools: tool table, current/next tool, staged quick change, manual confirm, optional ATC/turret state |
| `svg/sim-panel.svg` | Simulation-only SZGH-1080 panel window: power, E-stop, mode, overrides, lamps, displays (including active WCS status) and key matrix |

Superseded vendor-style studies, intermediate layout explorations, and
colour-mapped previews were intentionally removed.

## 15. Configuration

Configuration is per machine and layered:

- `configs/base.toml` — UI, keymap, safety policy, HAL pin map, backend and
  preference defaults.
- `configs/sim.toml` — mill simulation profile pointing at
  `../../baiermill_sim/probe_basic.ini`, the simulated SZGH panel and
  `rio_sim.py`.
- `configs/baiermill.toml` — physical mill profile pointing at
  `../../baiermill_pb/probe_basic.ini`, with the real SZGH-1080 panel owned by
  `szgh-panel-hal`.
- `configs/lathe-sim.toml` — lathe simulation profile pointing at
  `../../probe_basic_dev/configs/probe_basic_lathe/probe_basic_lathe.ini`,
  with `gui = "lathe"` and turret/live-tool capabilities.

Rules:

1. A profile is selected explicitly with `--config`; there is no default.
2. Profiles `extends` `base.toml`; profile tables override base, arrays replace.
3. `machine.gui` selects `mill` or `lathe`; the LinuxCNC INI remains
   authoritative for kinematics, axis count/order, travel, limits, velocities,
   units, spindle count, ATC/turret settings, tool table and program paths.
4. The HMI never owns the panel serial port and never writes machine-control
   HAL pins.
5. Safety flags are immutable from profiles: panel-only machine control, no
   software E-stop/cycle/jog/spindle/coolant/overrides.
6. Runtime UI state is stored separately under
   `~/.config/linc/<profile>.preferences.toml`.

`linc --check-config` must validate the merge and path resolution without
connecting to LinuxCNC or HAL. The full schema, launch snippets and validation
contract are in `CONFIGURATION.md`.
