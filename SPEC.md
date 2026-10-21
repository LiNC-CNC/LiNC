# LiNC Specification

Status: design reference  
Scope: Rust `egui` HMI for LinuxCNC with a CNC control panel and QMK CNC keyboard
Canonical visual: `svg/machine-dashboard.svg`

---

## 1. Goals

Build LiNC, a focused, operator-grade HMI that:

- puts machine control on the **physical CNC control panel**, with explicit
  Probe-Basic-compatible MDI/probe/tool commands from the HMI
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
| `FILE` | `F6` | Program page; press again for the Files browser, and again to return |
| `TOOL` | `F7` | Tools page |
| `OFFSET` | `F8` | Offsets page |
| `WIN` | `F10` | Toggle `SPLIT`/`CODE` on the Program page, shown as `▢` in the hint row |
| `ESC` | `Esc` | Back / cancel / close detail |
| `Ctrl+Esc` | `Ctrl+Esc` | Diagnostics |
| `Ctrl+Q` | `Ctrl+Q` | Guarded quit |
| Arrows | — | Focus/lists/scroll/parameter adjustment |
| `PageUp` / `PageDown` | — | Page or tab navigation |
| `Home` / `End` | — | First/last row or line start/end |
| `Enter` | — | Select / apply / execute MDI |
| `Backspace` | — | Edit text |
| `MO(1)` | layer key | Symbols and mouse layer |

The six function keys are the only page keys the keymap can send, so the
remaining destinations take combinations: `Ctrl+Esc` for Diagnostics and
`Ctrl+Q` for quit. `[keys]` in the profile binds all of them; the values in the
table are the defaults in `configs/base.toml`.

Hard limitations:

- No `F1`–`F4`, `F9`, `F11`, `F12`.
- No `Tab`.
- No `Shift`.
- No `Delete`.
- `UPPERCASE 0`: letters are emitted lowercase. The G-code editor/display
  normalises words to uppercase.
- F1–F8 softkey bars cannot be driven by this keymap. On-screen actions must
  be focusable with arrows/Enter and may also be clicked with the mouse layer.
- Standard editing shortcuts are supported: `Ctrl+C`, `Ctrl+X`, `Ctrl+V`,
  `Ctrl+Z`, `Ctrl+Y`, `Ctrl+A`, `Ctrl+S`, `Ctrl+O` and `Ctrl+R`.
  The QMK keymap has a Ctrl key but no Shift/Tab/Delete; on-screen actions
  remain available for mouse-layer use.
- `F10` may be intercepted by the window manager. The HMI runs kiosk/fullscreen
  or the WM is configured to pass it through.
- `Ctrl+Q` must not quit during motion or a running program; confirm and/or
  ignore while busy.

#### Shortcut scope

Shortcuts are split into two classes. Treat this as a rule when adding one:
a shortcut that only has meaning on one page must not be consumed anywhere
else, so it cannot shadow another page's command.

**Global** — these work from every page:

| Key | Action |
|---|---|
| `F5` `F6` `F7` `F8` `Ctrl+Esc` | Go to Machine / Program / Tools / Offsets / Diagnostics |
| `Esc` | Back, cancel, close detail |
| `Ctrl+Q` | Guarded quit |
| Arrows, `PageUp`/`PageDown`, `Home`/`End` | Focus, page, first/last item |
| `Enter` | Select / apply / issue MDI |
| `Backspace` | Edit text |
| `Ctrl+O` | Open the Files page, except on Program where `FILE` does it |

**Page-local** — consumed only while that page is in front:

| Page | Keys |
|---|---|
| Program | `Ctrl+S` save, `Ctrl+Z`/`Ctrl+Y` undo/redo, `Ctrl+A`/`Ctrl+C`/`Ctrl+X`/`Ctrl+V` editing, `Ctrl+D` duplicate line, `Ctrl+K` delete line, `Ctrl+Up`/`Ctrl+Down` move line, `WIN`/`F10` toggle `SPLIT`/`CODE`, `Ctrl+R` run from line, `Ctrl+0` fit the toolpath, `Ctrl+L` clear the live trail |
| Files | `Left`/`Right` switch pane, `Enter` open or copy to `NC_FILES`, `Backspace` up a folder, `Ctrl+U` copy to USB, `Ctrl+N` copy to `NC_FILES`, `Ctrl+R` refresh, `Delete`/`Ctrl+X` delete |
| MDI | `Enter` issue, `Up`/`Down` history, `Esc` clear the unissued block |
| Help | `/` focus search, `Backspace` back through link history |
| Offsets, Tools, Probe | Arrows select a row |

A page-local binding is only shown in the bottom hint row while its page is in
front; nav keys are never listed.

### 3.2 Physical panel

Source: existing panel driver sources, migrated into LiNC's `linc-panel` crate.

The CNC control panel is connected over RS-485 (115200 8N1) and exposed as a
LinuxCNC HAL component named `cnc_panel`.

The integrated panel service exposes:

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

- The panel is the primary machine-control surface: mode, cycle start/stop,
  single block, K4 stop, jog, rapid, spindle, coolant, tool changer, chuck,
  chip conveyor, work lamp, lube, air blow, safe door, overrides, and E-stop.
- Panel driver code is part of the LiNC repository (`linc-panel`), not an
  external dependency. It runs as a supervised process separate from the egui
  UI, so an HMI crash does not remove panel control.
- The HMI **must not** open `/dev/ttyUSB0`; the panel service owns the serial
  port. The HMI **must not** duplicate panel controls as on-screen buttons.
- LiNC **may write declared HAL outputs** through its `linc` HAL component.
  The initial allowlist is `linc.hmi-alive`, `linc.tool-change-confirm` and
  `linc.status-display`. It never writes arbitrary `cnc_panel.*` or
  machine-motion pins.
- The panel driver is modular: a `PanelDriver` trait plus a capabilities
  descriptor (buttons, LEDs, displays, mode encoding, override encoding,
  pendant support) lets future panel types and transports replace the panel
  implementation without changing the UI/backend contract.
- Existing HAL glue must be migrated from `qtpyvcp.*` pins to `halui`, the
  `linc` component and `linc-panel` pins.

Mode and override encoding:

- Mode and override switches are **steady one-hot/analog levels**, not
  pulses. Exactly one `mode_*` input must be active; zero or multiple active
  inputs resolve to `UNKNOWN` and fail closed. No pulse latching is required.
- Override values are sampled directly from their steady pins.
- The simulator uses the same one-hot/steady contract as the real panel.

Pendant:

- The WHB04B-6 pendant is a **jog-only input device**. Its handwheel, axis
  select and jog-enable functions are used only while the panel mode is `JOG`.
- Pendant mode selection, cycle start/hold/stop, spindle, coolant, override,
  program, macro and probe buttons are ignored at the HAL layer. They must not
  be wired into LiNC or LinuxCNC control paths.
- The pendant never becomes a second mode/cycle surface. Outside `JOG`, its
  jog input is ignored too.

Special cases:

- The two-digit status display defaults to the **active work coordinate
  system**, `54`–`59`. It is the clearest use of two digits and is
  safety-relevant during setup and PROG.HOME. Show `--` when the value is
  unknown, stale or outside those offsets; do not guess.
- The HMI reads `g5x_index` and writes `linc.status-display`; the panel service
  maps it to `cnc_panel.sseg_status`. Optional configured sources are spindle
  load percent and `MM`/`IN` units.
- The RS-485 protocol exposes `mode_mpg` and `mpg_sim`, but **not handwheel
  encoder counts**. Verify where the MPG encoder is wired; do not assume the
  panel link carries MPG pulses.
- `program_protect` must make the editor read-only and block offset/tool writes.
- The panel E-stop mushroom is an **operator-owned latching input**. The panel
  service must never derive it from a controller lamp: doing so lets the
  controller clear an E-stop the operator pressed, and races the first poll
  after the press. Pressing it must latch visibly on the panel itself, light
  the `MC` alarm lamp, and reach the controller through the machine HAL
  (`cnc_panel.estop` -> `rio.sim-estop` -> `halui.estop.activate` in the
  simulation). The HMI shows a persistent `EMERGENCY STOP ACTIVE` banner and
  blocks machine-modifying actions while it is latched.
- **Releasing** the mushroom clears the latch and asks the controller to leave
  E-stop, but must **not** power the machine on: that is a separate `POWER ON`
  action. The two are separate one-shots — `cnc_panel.estop_reset` (release)
  and `cnc_panel.power_on` (`POWER ON`) — ORed into `halui.estop.reset`, so
  releasing alone never latches `rio.sim-power-on`. After a release the panel
  shows `POWER OFF` as the active power state.
- `panel_connected == false` must show a persistent banner and block
  machine-modifying UI actions. The hardware E-stop remains independent.

### 3.3 No on-screen machine controls

Forbidden on screen:

- mode selection (except the automatic temporary MDI switch used by
  Probe-Basic-compatible command execution)
- general cycle start / feed hold / stop / reset
- general jog controls
- spindle CW/CCW/stop/orient
- coolant on/off
- override sliders
- software E-stop

Allowed on screen:

- read-only state indicators
- program/file management
- G-code editor and toolpath
- offset and tool tables
- MDI text entry and explicit command issue
- explicit `RUN PROBE` and tool-change actions that issue their specific MDI
  block after validation; these are not general cycle-start controls
- diagnostics, filtering, log export/copy, view clearing
- layout/view selection

## 4. Architecture

```text
CNC control panel --RS-485--> linc-panel (LiNC panel service) --> HAL --> halui/LinuxCNC
QMK keyboard --USB--> linc (Rust egui) --NML or PyO3--> LinuxCNC
WHB pendant --USB--> LinuxCNC jog input only (non-jog ignored)
LinuxCNC status --------------------------------> linc
linc.* HAL outputs <-----------------------------> linc
cnc_panel.* HAL pins <--------------------------> linc-panel
```

### 4.1 Processes

- `linc-panel`: LiNC-owned panel service. Owns the serial port, runs a modular
  `PanelDriver`, produces button/mode/override pins, consumes LED/7-segment/
  velocity pins. Separate process from the UI so an egui crash does not remove
  panel control.
- `linc`: Rust `egui` application and machine backend. Owns no motion hardware.
  Loads the `linc` HAL component and writes only its declared outputs:
  `linc.hmi-alive`, `linc.tool-change-confirm`, `linc.status-display`.
- `linuxcnc`: task/motion/IO. Authoritative machine state.
- HAL glue: standard `halui`, the `linc` component, and `linc-panel`.

### 4.2 Rust workspace

```text
LiNC/
  crates/
    linc-core/        # state, commands, backend trait, validation
    linc-linuxcnc/    # PyO3 official module; optional C++ NML bridge
    linc-hal/         # typed HAL FFI + linc.* output allowlist
    linc-panel/       # PanelDriver trait; generic panel and simulator drivers
    linc-ui/          # egui shell, theme, screens, widgets
    linc-view/        # 2D/3D toolpath and sparklines
    linc-app/         # main, config, logging
```

`linc-panel` keeps panel technology behind a small contract:

```rust
pub trait PanelDriver {
    fn poll(&mut self) -> Result<PanelSnapshot, PanelError>;
    fn write(&mut self, outputs: &PanelOutputs) -> Result<(), PanelError>;
    fn capabilities(&self) -> PanelCapabilities;
}
```

`PanelCapabilities` declares buttons, LEDs, displays, mode encoding, override
encoding and pendant support. The CNC control panel driver uses steady one-hot modes and
steady overrides; the simulator uses the same contract. Future panels can add
drivers without changing `linc-core` or the UI.

### 4.3 Backend boundary

```rust
pub trait MachineBackend: Send + 'static {
    fn snapshot(&self) -> MachineSnapshot;
    fn send(&mut self, command: MachineCommand) -> Result<(), MachineError>;
}
```

The UI never talks to NML or HAL directly. A worker thread owns the backend,
publishes snapshots over a channel, and writes typed HAL outputs through
`linc-hal`. There is no generic arbitrary-pin write API.

Preferred first bridge: **PyO3 + official `linuxcnc` module**, matching the
Probe Basic execution semantics. The C++ NML bridge is a later replacement if
Python must be removed. `linuxcnc-grpc` is only for a remote panel, not the
local HMI.

### 4.4 Simulation panel

The simulation build runs the same `linc-panel` driver contract with a
simulated CNC control panel. It is not part of the HMI shell and is never
present in the real-machine build.

- Reproduces the CNC control panel: POWER, E-stop, MODE, feed/rapid/spindle
  overrides, axis/alarm lamps, tool displays and key matrix.
- **Boots in the safe state a real cabinet is found in**: E-stop latched and
  power OFF. The operator releases the mushroom and presses `POWER ON` to bring
  the machine up, so the power-up sequence is exercised instead of skipped.
  Starting powered-on and un-latched made the machine look ready when it was
  not, and hid the E-stop reset path entirely.
- Creates the same `cnc_panel` HAL component/pin surface as the real driver,
  plus simulation-only power/E-stop inputs.
- Modes and overrides use the same steady one-hot/steady contract as the real
  panel.
- The HMI reads HAL/status and may issue its explicit MDI/probe/tool commands;
  simulated machine commands otherwise originate in the panel window or
  keyboard. It never fabricates panel feedback.
- It may live on a second monitor. The calibration/mockup is
  `svg/sim-panel.svg`.
- A lathe-specific panel mockup still needs turret and live-tool controls;
  until then, the lathe simulation profile runs without a panel window.

### 4.5 HAL wiring

- `hal/linc.hal` loads `linc-panel` and wires `cnc_panel.*` to `halui` modes,
  cycle start/pause/stop, jog, spindle, coolant/lube, homing, overrides, status
  LEDs and tool displays.
- `hal/linc-postgui.hal` is loaded after the GUI has created `linc.*` and wires
  `linc.status-display` to `cnc_panel.sseg_status`,
  `linc.tool-change-confirm` to `iocontrol.0.tool-changed`, and
  `linc.hmi-alive` as a heartbeat.
- `hal/linc-pendant.hal` is optional and real-machine only. It wires pendant
  axis-select/handwheel/jog-enable inputs gated by `cnc_panel.mode_jog`; it does
  not wire pendant mode, cycle, spindle, coolant, override, program or macro
  controls.
- `LINC_CONFIG` must select the same profile used by the `linc` display.
- The core HAL expects the generic `cnc_panel` component/pins; the live machine
  must rename or alias its legacy panel component first.

## 5. Application shell

Fixed shell, no right-hand status column:

- **Top status bar**: machine name, physical mode, program, tool, spindle,
  feed, work offset, panel link, alarm/message counts, time.
- **No page titles.** The navigation names the page and the top bar names the
  machine state, so a heading above the content only costs vertical space.
  Each page keeps an invisible focus anchor at the top so arrow-key navigation
  has a starting point. Banners stay: they carry live state (E-stop, panel
  offline, program protect), not page descriptions.
- **Left navigation**: Machine, Program, Tools, Offsets, Diagnostics, G-code
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
- The minimum window size is **1280×720**. The shell and every screen are
  designed and optimized for that baseline; do not compress the layout below
  it. The whole shell scales proportionally above 720p, preserving the SVG's
  label/value/display font ratios.
- On-screen actions have a minimum 44 px target at 1280×720.

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
- ATC state in Tools/Diagnostics; mill tool table uses orientation and, only
  when ATC is enabled, the standard `P` pocket column.

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
- Current line synchronized with the graph, and the running line marked in the
  editor the way the Machine page's G-code display marks it: an accent bar in
  the gutter plus a tinted row. There is no separate current/next pane.
- `MODAL` is its own pane under the toolpath, using the same abbreviated cells
  as the Machine dashboard, up to eight columns so a full set of active codes
  stays on one row.
- `FIT` (`Ctrl+0`) reframes the toolpath and `CLEAR TRAIL` (`Ctrl+L`) clears
  only the live trail; both are page-local commands, not buttons.

#### CODE

- G-code editor dominant (~76%); the `PATH` thumbnail column beside it is
  deliberately narrow, because CODE is an editing layout.
- The thumbnail carries no colour legend: it would cover most of a graph that
  small.
- `MODAL` pane below the thumbnail, in at most four columns; the column is too
  narrow for more and the codes then wrap onto a second row.
- Current line synchronized with the thumbnail.
- The CODE layout is reused for editing; there is no separate editor window.

#### RUN FROM LINE

`RUN FROM LINE` (`Ctrl+R`, page-local to Program) starts the loaded program at
the selected line. A line number is the one program-start input the panel cannot
supply, so this is the only program start the HMI issues. When a condition blocks
it, the reason is raised as a notice rather than the key doing nothing.

- Runs only when the program is loaded, the interpreter is idle, the machine is
  ON, no E-stop is latched, every axis is homed, the panel state is current and
  the panel is in `AUTO`. The panel therefore remains the authority over whether
  the machine may run.
- A confirmation names the line and its block, and states that the lines before
  it are skipped — work and tool offsets, feed, spindle and coolant are not
  restored — and that motion starts from the current position.
- The bridge requires an idle interpreter because `AUTO_RUN` on a paused program
  would resume it rather than start at the line.

#### EDIT state

The physical panel's `EDIT` position selects Program, switches to the CODE
layout and focuses the editor. The layout and graph stay the same; the buffer
becomes editable.

- Editor buffer with cursor, selection, line numbers, current-line highlight and
  live token highlighting.
- Dirty marker in the header and buffer card; `SAVE` is accented until saved.
- `Ctrl+S` saves; `Ctrl+Z`/`Ctrl+Y` undo/redo; `Ctrl+A/C/X/V`
  select/cut/copy/paste. There is no find bar: search without a visible query
  field and match list was a shortcut that appeared to do nothing.
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
  `--success` stroke.
- The toolpath card's status line is the key: `— CUT`, `-- RAPID`, `— LIVE`
  and an amber `○ TOOL`, with the view (`3D / ISO`) and `ZOOM` on the right. A
  legend box inside the graph is not drawn — it repeated that line and covered
  the drawing.
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
- `LOAD` or `Ctrl+O` opens Files; `ESC` returns to Program. `FILE` (`F6`)
  toggles between Program and Files.
- A successful file load always returns to Program, in `SPLIT` view when the
  machine is idle, even if the previous layout was `CODE` or the panel is in
  `EDIT`.
- The header segmented control is also focusable.
- No on-screen cycle start; loading a program does not start it.

### 6.3 Offsets

- Work offsets G54–G59.
- Tool offsets. G92 is deliberately not exposed here: it is a transient offset
  that silently shifts every WCS, and a field for it invited the operator to
  leave the machine in a state neither the DRO nor a program could explain.
- Mill: X/Y/Z and rotary offsets; lathe: X/Z geometry and wear, tool nose
  radius, tip orientation, C/Y offsets and turret station.
- Current axis values and distance-to-go.
- Read-only while running; blocked when program protect is active.
- Edits validated against LinuxCNC limits and units.
- A `PROBE` action opens the Touch Probe page; probing results can update the
  selected work offset only after an explicit `APPLY RESULT`.

### 6.4 Tools

Primary quick tool-change surface; no MDI screen required.

- Tool table backend is the **standard LinuxCNC tool table**, read/written at
  the path in INI `[EMCIO] TOOL_TABLE`. Columns come from
  `TOOL_TABLE_COLUMNS`; the HMI uses that schema directly.
- **Pocket column:** present only when an ATC/turret changer is used. A
  `manual` changer has no pocket column and the Tools page hides it. An
  `atc`/`turret` profile requires a standard `P` column; the loader refuses to
  start if it is missing.
- Tool table: number, description, dimensions/geometry, optional pocket/station
  and type. Selected row or a typed target number becomes the target tool.
- Current tool, prepared/next tool and spindle state are always visible.
- `CHANGE TOOL` issues `Tn M6` immediately using Probe Basic's `issue_mdi`
  semantics: machine ON, homed axes, idle interpreter, no active program, then
  temporary MDI mode and automatic restore. There is no staging and no
  physical-CYCLE-START step.
- `T0` unload and manual-change confirmation are supported. `SET CURRENT`
  (`M61 Qn`) is available for a tool already in the spindle but is explicitly
  labeled as not a tool change.
- Manual changer: after `M6` pauses for the manual change, show
  `REMOVE current / INSTALL target` and `CONFIRM INSTALLED`. The confirmation
  writes the declared `linc.tool-change-confirm` HAL output, which is wired to
  the manual-toolchange chain; it is not an arbitrary HAL write.
- ATC/turret: optional and lower priority. Use the existing `toolchange.ngc`
  and `m6_tool_call_*` routines, updated to use the standard tool-table pocket
  column rather than ad-hoc pocket parameters. Show carousel/rack pockets,
  current pocket, clamp/unclamp and faults when exposed.
- Lathe: power-turret station grid, axial/radial live-tool flag, live-tool RPM
  range/direction and driven-tool state. A turret profile also requires the
  standard pocket/station column.
- Tool table writes and tool-change issuing require idle; table writes also
  require program protect off. Running programs leave the page read-only.

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
- Two equal panes, no page header and no button row: every action is a key.
  `Left`/`Right` move between the panes; `Up`/`Down` move the selection;
  `Enter` opens a program from `NC_FILES` or copies a file from USB into
  `NC_FILES`; `Backspace` goes up a folder; `Ctrl+U` copies to USB and `Ctrl+N`
  copies to `NC_FILES`; `Delete` (or `Ctrl+X`, since the CNC keyboard has no
  Delete key) deletes the selected file after a confirmation naming its full
  path; `Ctrl+R` refreshes.
- Copy and delete selected regular files: `Ctrl+U` from `NC_FILES` and `Ctrl+N`
  from USB. Folder navigation only; no recursive folder copy in v1.
- Copy requires free space, rejects symlinks/path escapes, confirms overwrite,
  writes to a temporary file in the destination and renames atomically.
- Copy progress and errors are visible; `EJECT` is disabled while a copy is
  active.
- `EJECT` unmounts and powers off the device; never force-unmount by default.
  Show `SAFE TO REMOVE` after success.
- `Enter` on a program accepts only configured G-code extensions from
  `NC_FILES`; it loads but never starts the program. Loading and writes to `NC_FILES` are disabled
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
- `RUN PROBE` builds the selected `o<probe_...> call` and its argument values,
  then issues it immediately using Probe Basic's `issue_mdi` semantics:
  temporary MDI mode, command sent, previous task mode restored when the
  interpreter returns idle. The click is the explicit operator action; there is
  no second stage and no physical-CYCLE-START step.
- Probe motion requires machine ON, homed axes, probe tool T99 in the spindle,
  idle interpreter, no active program and valid parameters. A syntax/validation
  error issues nothing and marks the failing condition.
- Read results from the subroutine outputs (`#1001`–`#1009`) and show measured
  value, error, selected WCS, and history.
- `APPLY RESULT` writes the selected work offset only when idle and program
  protect is off. Calibration routines update the probe calibration offset;
  `RESET CAL` clears it through `probe_cal_reset.ngc`.
- The page has no general cycle-start, jog, override or E-stop controls. The
  specific `RUN PROBE` action is the only motion initiator, and the hardware
  E-stop remains independent.

### 6.9 MDI

Mode-driven screen. Moving the physical mode selector to `MDI` opens this page
immediately, focuses the command line and shows `PANEL MODE: MDI`; moving out
closes it and discards unissued input. No left-nav item is added.

Industry-standard layout:

- Large single-block command input with line number and G-code token colours.
- Quick-insert chips for G0/G1/G2/G3, M3/M5/M6/M8/M9, G54, G38.2 and T;
  variant-specific chips for lathe (G7/G8, G95/G96).
- Position/DRO and modal G-code panel.
- Last-issued block and result panel with syntax status, requirements and
  error/result.
- Local MDI history with timestamps. Up/Down selects entries; `Enter` on a
  selected row, `Ctrl+Enter`, or `RUN SELECTED` re-issues that exact block
  through the same `issue_mdi` validation. This mirrors Probe Basic's history
  selection/run workflow without a queue or mouse.

Rules:

- `Enter` or `ISSUE MDI` issues one block through Probe Basic's `issue_mdi`
  semantics. This is an explicit on-screen command action, not a general
  cycle-start control.
- Issue requires machine ON, homed axes, idle interpreter and no active
  program. A syntax error issues nothing and marks the input.
- The command may temporarily select MDI and restores the previous task mode
  when the interpreter returns idle. No physical CYCLE START is involved.
- Leaving MDI, pressing `ESC`, or clearing discards unissued input; it does not
  cancel a command already issued.
- History is HMI-local, bounded (`mdi.history_limit`), and not a machine log.
- No on-screen jog, override, spindle, coolant or E-stop controls.

### 6.10 Homing

Mode-driven screen. Selecting `HOME` or `PROG.HOME` on the physical panel opens
it automatically; leaving those positions closes it. The title/context changes
between `HOMING` (machine home) and `PROGRAM ZERO` (`PROG.HOME`). The HMI only
observes, explains and logs the panel behavior; it never issues a home command.

**Layout base.** The Homing screen uses the *same layout base* as the Manual
(JOG / INC / MPG) screen: a persistent `PANEL MODE:` line, a dominant
`TOOLPATH / LIVE POSITION` graph card with `CLEAR TRAIL`, `FIT` and the view
cycle, and a right-hand readout column whose first card is the large
per-axis work/machine/DTG readout. Only the readout content and the context
cards differ:

- the readout is headed `MACHINE POSITION` and each axis row carries its homing
  state as a badge (`HOMED`, `HOMING`, `NOT HOMED`, `UNKNOWN`, `NOT
  CONFIGURED`) plus the INI sequence/search/latch/index detail;
- the context card reports the panel mode, axes homed, sequence state, home
  order, `NO_FORCE_HOMING` and limit state;
- the panel-mapping card documents the exact physical-key behaviour.

Keeping one base means the two mode-driven screens cannot drift apart; a change
to the live graph or the coordinate readout applies to both.

- Axis rows show `HOMED`, `HOMING`, `NOT HOMED`, the INI `HOME_SEQUENCE` and the
  search/latch/index state. Optional axes are shown only when configured.
- Selecting `HOME` on the panel must also select the **MANUAL** task mode in
  HAL (`cnc_panel.mode_home` -> `halui.mode.manual`). LinuxCNC refuses
  `halui.joint.N.home` / `halui.home-all` with
  `can't do that (EMC_JOINT_HOME) in auto mode with the interpreter idle`
  whenever the task mode is AUTO or MDI, so without this net the home keys do
  nothing unless the operator happens to be in MANUAL already. Probe Basic
  reaches the same result through its `home-mode` QtPyVCP signal; LiNC has no
  QtPyVCP layer, so the mode request is wired directly in HAL.
  `PROG.HOME` must **not** assert MANUAL: it issues its `G0` through the
  `issue_mdi` path, which selects MDI itself.
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
    `issue_mdi` action; the HMI must not intercept it or add a cycle-start
    gate. It still requires machine ON, all axes homed and interpreter
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
override input stays on the physical panel, with the WHB pendant used only for
jog in JOG mode.

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
- Design for a minimum 1280×720 viewport; scale proportionally above it and
  preserve 44 px targets. Do not reflow into a smaller layout. Use the SVG type
  ramp (8–30 px labels/values/displays) so relative font sizes match the
  accepted mockups.

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
- `FILE`: Program, or the Files browser when Program is already in front.
- `TOOL`: Tools.
- `OFFSET`: Offsets.
- `WIN`: cycle `SPLIT`/`CODE`, page-local to Program (see 3.1 shortcut scope).
- `Ctrl+Esc`: Diagnostics.
- `HELP` action: G-code Help; no dedicated function key.
- `LOAD` / `Ctrl+O`: Files, when idle and allowed.
- `PROBE` action: Touch Probe from Offsets; explicit `RUN PROBE` issues MDI.
- MDI mode: `Enter` issues the block; `Esc` clears unissued input; `Up`/`Down`
  select history; `Enter` on a focused history row or `Ctrl+Enter` runs the
  selected history entry through the same validation. No physical CYCLE START
  is required.
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
`FILE` active in Program, `Ctrl+Esc` reaching Diagnostics, etc.

### 8.2 Mode and command gating

- The physical panel mode switch is authoritative for panel functions. The GUI
  displays panel mode and LinuxCNC task mode; if they differ, show both only
  when useful.
- The MDI screen is available when the panel is in MDI mode; probe and
  tool-change pages may still issue their specific commands through
  `issue_mdi`, which temporarily selects MDI and restores the prior mode.
- HOME/PROG.HOME open the Homing screen; all homing/program-zero motion
  originates in the panel's home/proghome pins and keys. The HMI never homes an
  axis and never intercepts the panel's existing direct `issue_mdi` PROG.HOME
  path.
- JOG/INC/MPG open the Manual screen with the large graph and coordinate
  readout. Jog, increment and handwheel motion remain panel/pendant input only.
  **Increment selection**: `INC` steps come from the machine INI's
  `[DISPLAY] INCREMENTS` list, so the HMI offers exactly the values LinuxCNC was
  configured with (the same list Axis and Probe Basic use). The selector lives
  in the HMI's `INC` context card and publishes `linc.jog-increment`, wired to
  `halui.axis.L.increment` in the post-GUI HAL file. The list's leading `JOG`
  entry is continuous and is not offered as a step: continuous is the panel's
  `JOG` mode, not a zero step. Jog keys are routed by panel mode — continuous
  to `halui.axis.L.plus/minus` (level) and incremental to
  `halui.axis.L.increment-plus/minus` (edge triggered).
  **Jog speed** is a percentage of the machine INI's
  `[DISPLAY] DEFAULT_LINEAR_VELOCITY`, set with a slider in the mode context
  card. The HMI publishes the result in machine units per second on
  `linc.jog-speed`, wired to `halui.axis.jog-speed` in the post-GUI HAL file, so
  the machine HAL needs no multiplier and the operator can slow jogging without
  editing the config. The percentage is remembered in preferences.
  In HAL these positions select the **MANUAL** task mode as well as teleop:
  `halui.axis.L.plus/minus` (`EMC_AXIS_JOG`) is refused unless the task mode is
  MANUAL. `halui.axis.jog-speed` defaults to 0 and must be set from the INI
  (LiNC uses `[DISPLAY] DEFAULT_LINEAR_VELOCITY`); without it the jog pins are
  asserted and the machine does not move. `halui.mode.manual` is one pin, so
  the JOG and HOME requests are ORed into a single request.
- MDI, probe and tool-change actions use Probe Basic's `issue_mdi` semantics:
  after an explicit operator action and validation (machine ON, homed, idle, no
  active program), the HMI may temporarily select MDI, issue the specific
  command, and restore the previous task mode. This is an intentional exception
  to panel-exclusive machine control; it is not a general cycle-start action.
- Leaving the MDI screen clears unissued input only; an already issued command
  is not cancelled.
- Program start is physical CYCLE START only. The HMI never starts AUTO.
- The WHB pendant contributes jog/handwheel input only in JOG mode; all other
  pendant controls are ignored.
- Overrides are physical switches; the GUI displays values.
- Jog is physical only.
- Program protect is a physical panel input. It blocks every controller-side
  write and is treated as ON when unknown or stale.
- The GUI may load programs, edit G-code, edit offsets/tools, and issue MDI,
  probe and tool-change commands within the gating above.
- LiNC writes only its declared `linc.*` HAL outputs; it never writes arbitrary
  motion, spindle, coolant, cycle or `cnc_panel.*` pins.

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
    pub meta: SnapshotMeta,              // sequence, age, stale flag
    pub panel_mode: PanelMode,           // one-hot decode; Unknown fails closed
    pub task: TaskState,                 // Task/Interp state, mode, paused
    pub machine: MachineState,           // on, estop, homed, limits, fault
    pub positions: AxisMap<PositionSet>, // work, machine, DTG per INI axis
    pub offsets: WorkOffsetSet,          // G54-G59, G92, rotation, axis count
    pub tool_table: ToolTableState,      // standard LinuxCNC table + columns
    pub spindles: Vec<SpindleState>,     // main + sub/live tool
    pub feed: FeedState,
    pub program: ProgramState,           // path, revision, line, dirty, running
    pub tool_change: ToolChangeState,    // idle / issuing / manual prompt
    pub probe: ProbeState,               // last result, timestamp, calibration
    pub atc: Option<AtcState>,           // pockets from standard P column
    pub pendant: PendantState,           // WHB jog-only snapshot
    pub panel: PanelSnapshot,
    pub hal: HalOutputs,                 // linc.* output state
    pub alarms: Vec<Alarm>,
}

pub struct PositionSet {
    pub work: f64,
    pub machine: f64,
    pub dtg: f64,
}

pub struct ToolTableState {
    pub path: PathBuf,
    pub columns: Vec<ToolColumn>,        // from TOOL_TABLE_COLUMNS
    pub rows: Vec<ToolRow>,              // P is Some only with ATC/turret
    pub revision: u64,                   // detect external edits
}

pub struct ToolRow {
    pub tool: u16,
    pub pocket: Option<u16>,
    pub description: String,
    pub geometry: ToolGeometry,          // mill length/diameter or lathe X/Z/I/J
    pub wear: ToolWear,
    pub nose_radius: Option<f64>,
    pub orientation: Option<u8>,
}

pub enum MachineCommand {
    IssueMdi { block: String, origin: MdiOrigin },
    IssueToolChange { tool: u16 },
    ConfirmManualToolChange { token: ToolChangeToken },
    SetCurrentTool { tool: u16 },
    LoadProgram { path: PathBuf, expected_revision: Option<u64> },
    SaveProgram { path: PathBuf, expected_revision: u64 },
    SetWorkOffset { index: u8, values: AxisMap<f64>, rotation: Option<f64> },
    SetToolTableRow { row: ToolRow, expected_revision: u64 },
    ApplyProbeResult { result: ProbeResult, target: ProbeTarget },
    ResetProbeCalibration,
    SetHalOutput { output: LincOutput, value: HalValue },
}

pub enum LincOutput {
    HmiAlive,
    ToolChangeConfirm,
    StatusDisplay,
}
```

`MdiOrigin` records `Mdi`, `Probe`, `ToolChange` or `ManualToolChange` so the
backend can apply the correct preconditions and result handling. `IssueMdi`
uses Probe Basic `issue_mdi` semantics; it is not a general cycle-start
command.

`SetHalOutput` is a closed enum, not a pin-name string. The HMI may write only
the declared `linc.*` outputs; arbitrary HAL writes are not representable.

Machine-control commands (panel mode, cycle, jog, spindle, coolant, overrides)
are not part of the HMI command set. The only mode change is the temporary MDI
selection inside `IssueMdi`, which restores the prior task mode.

Panel snapshot should include at least:

- connected/error
- one-hot mode + validity
- feed/rapid/spindle override (steady)
- program protect
- E-stop
- tool/next tool
- raw button/LED bitmaps
- RX/TX, CRC errors, timeouts, last RX age, reconnects
- pendant jog-only state
- capacity/capabilities from the panel driver

`linc-panel` maintains the serial counters and raw button/LED bitmaps, and the
simulation surfaces them. On the **physical** path they do not reach the HMI
yet: the panel service runs with `--no-ipc` under LinuxCNC and the LinuxCNC
bridge still reports `link.available = false`, so Diagnostics shows the
counters as unavailable rather than zero. Wiring them over IPC or dedicated HAL
pins is still open.

## 10. Performance

- Status snapshot: 10–20 Hz.
- Current-line sync: 10–20 Hz.
- Sparklines/event charts: 1 Hz or on data change.
- 3D toolpath: render only when visible; 30 fps maximum.
- No continuous 60 fps repaint when idle.
- No blocking file/serial/NML I/O on the UI thread.
- HAL outputs: `hmi-alive` 2 Hz heartbeat, `status-display` on WCS change or
  5 Hz, `tool-change-confirm` edge-driven only.
- Cache text layout and visible-line tokens.
- Event log in a ring buffer; persist asynchronously, not on every event.

## 11. Safety

- The hardware E-stop chain is the only safety-rated stop.
- A software E-stop button is forbidden.
- LiNC may write only the declared `linc.*` HAL outputs
  (`hmi-alive`, `tool-change-confirm`, `status-display`). It never writes
  arbitrary HAL pins or machine motion/spindle/coolant/cycle pins.
- Validate machine ON, homed, idle, program protect and no active program
  before MDI, probe, tool-change and program/offset/tool writes.
- MDI, probe and tool-change commands use Probe Basic `issue_mdi` semantics:
  they execute immediately after an explicit operator action, may temporarily
  select MDI, and restore the prior task mode. There is no staged-command path
  and no physical-CYCLE-START gate for these specific commands.
- The HMI never starts an AUTO program; physical CYCLE START is the only AUTO
  start path.
- Homing is panel-only: no HMI home/unhome command, button or key. The HMI
  displays sequence/state and must not interfere with the panel's home pins or
  its PROG.HOME `issue_mdi` actions.
- Probe routines start only from the explicit `RUN PROBE` action. Never
  auto-start a probe routine on screen entry or parameter change.
- Tool changes start only from the explicit `CHANGE TOOL` action. Manual
  confirmation requires a pending change and a live panel; never auto-confirm.
- Respect program protect and running state. Program protect is a panel
  switch, is rechecked by the backend on every write, and fails closed when
  unknown or stale.
- Manual modes are read-only: no HMI jog, increment, handwheel, mode or
  override command exists. The WHB pendant is accepted for jog only.
- Do not hide alarms behind tabs; persistent banner plus Diagnostics.
- HMI crash must not stop the machine or remove panel control. The panel
  service is separate; `linc.hmi-alive` exposes UI health to HAL.
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
- The simulation-only CNC control panel is represented separately from the HMI.
- The Touch Probe screen represents routine selection, WCS/parameters,
  explicit `RUN PROBE` via Probe Basic `issue_mdi`, results/apply and
  calibration, with no separate cycle-start step.
- The MDI screen opens at the panel MDI position and represents command issue,
  DRO/modal state and history; `Enter` issues immediately through `issue_mdi`,
  and a selected history row can be re-run with `Enter`/`RUN SELECTED`.
- The Homing/Program Zero screen opens at HOME/PROG.HOME, shows per-axis/sequence
  state and documents the exact physical-panel key mapping without providing
  home controls.
- PROG.HOME retains the current machine's `issue_mdi` behavior: RAPID sends
  `G0 X0Y0Z0`, axis keys send `G0 X0/Y0/Z0`, in the current WCS, X/Y/Z only.
- The Tools screen represents the standard LinuxCNC tool table, current/next
  tool, explicit `Tn M6` issue, manual confirmation and optional ATC/turret
  state; the pocket column appears only when a changer uses it.
- The Manual screen opens at JOG/INC/MPG, shows a large graph plus large work
  coordinates, machine coordinates and DTG, and provides no on-screen jog or
  mode control.
- Program protect ON locks program editing, saves, offset/tool writes, file
  imports and probe applies; unsaved changes are preserved, not discarded.
- The panel's status seven-segment display shows the active `G54`–`G59` offset
  as `54`–`59`, and `--` when unknown; it never fabricates a value.
- LiNC exposes the declared `linc.*` HAL outputs and writes them; it never
  writes arbitrary machine or panel pins.
- Mode/override inputs are steady one-hot/steady and decoded without pulse
  latching; invalid mode state is `UNKNOWN`.
- The WHB pendant is documented and implemented as jog-only; non-jog pendant
  controls are ignored.
- `hal/linc.hal`, `hal/linc-postgui.hal` and `hal/linc-pendant.hal` exist and
  match the pin names in `configs/base.toml`.
- The pre-GUI HAL contains no `linc.*` nets; the post-GUI HAL wires all three
  declared `linc.*` outputs.
- The pendant HAL wires only jog/handwheel/axis-select paths and is gated by
  `cnc_panel.mode_jog`.
- Hardware constraints for the QMK keyboard and CNC control panel are documented.
- No SVG shows a general on-screen machine-control panel.
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

- The HMI enforces a minimum 1280×720 window and every screen is usable and
  optimized at that baseline; it does not accept a smaller viewport.
- All interactive controls are reachable and activatable with arrow keys,
  Enter and Esc only; no mouse or Tab is required.
- Keyboard-only navigation works without Tab, Shift, Delete, or F1–F8;
  standard Ctrl editing shortcuts work.
- Panel service and HMI are separate processes; only the panel service owns
  RS-485.
- No general on-screen machine-control panel exists. Explicit MDI, probe and
  tool-change commands are allowed through the validation path above.
- Program protect, panel offline, and one-hot mode validity are enforced.
- AUTO program start remains physical CYCLE START only.
- Diagnostics shows real panel-link statistics and event history.
- Physical E-stop remains independent.

## 13. Open issues

1. Port the existing panel driver into the LiNC `linc-panel` crate. The current crate
   does not build with HAL enabled:
   - `Cargo.toml` patches to missing
     `patches/linuxcnc-hal-rs/linuxcnc-hal-sys`;
   - without the patch, crates.io `linuxcnc-hal-sys` fails in bindgen against
     current glibc headers.
2. The Rust panel port opens/closes the serial port every cycle. It should own
   one `Box<dyn SerialPort>` for process lifetime.
3. The panel service currently uses a 0.01 s cycle while the protocol sleeps
   roughly 14 ms per transaction; choose 0.02–0.05 s or reduce delays.
4. Add Diagnostics counters and raw bitmaps to `linc-panel`.
5. Verify the MPG encoder path; it is not carried by the RS-485 driver.
6. Choose the LinuxCNC bridge: PyO3 first, C++ NML later.
7. Confirm spindle-load source (`rio.spindle-modbus.spindle_load-abs` or VFD
   equivalent); never derive it from override.
8. Map panel `EDIT`/`HOME`/`PROGHOME` modes to UI state; they are not all
   LinuxCNC task modes. Decode the steady one-hot mode directly; do not latch
   pulses.
9. Design the editor for standard Ctrl shortcuts with no Shift/Tab/Delete;
   line-based operations are still required.
10. Verify theme contrast and color-blind distinguishability on the target
    display.
11. Implement the layered config loader and `linc --check-config`;
    `configs/*.toml` currently define the contract only.
12. Unify the real panel driver's serial port and cycle time with
    `configs/baiermill.toml`; today they are hard-coded in the driver and
    `hallib/cnc_panel.hal`.
13. Convert the generated Python G-code help data
    (`../baiermill_pb/user_tabs/gcode_help/gcode_docs.py`) into a Rust-friendly
    bundle (JSON/TOML), pin the LinuxCNC docs version, and preserve
    documentation/image attribution.
14. Implement the G-code editor: `ropey` buffer, cursor/selection, undo/redo,
    find, atomic save, external-change detection, unsaved-change confirmation
    and a debounced preview parser. The preview must not use the machine
    interpreter or move the machine.
15. Verify editor usability with standard Ctrl shortcuts and no `Shift`, `Tab`
    or `Delete`: mouse-layer selection, Backspace, cut/paste and line-based
    operations.
16. Add a driver capability matrix and conformance tests for future panel
    types: button/LED maps, display count, mode encoding and override
    encoding. The CNC control panel uses steady one-hot modes; no pulse latching.
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
    `svg/sim-panel.svg` is the mill CNC control panel.
23. Port the Probe Basic parameter contract (`#3014`–`#3036`) and result contract
    (`#1001`–`#1009`), map every routine to its G-code call, and verify that
    `RUN PROBE` issues the call through `issue_mdi` only after the explicit
    action and state validation.
24. Decide lathe touch-probe/tool-setter support separately; the first Touch
    Probe page is mill-oriented (X/Y/Z and WCS).
25. Implement MDI issue through Probe Basic `issue_mdi`: syntax validation,
    history recall, discard of unissued input on mode exit, and restoration of
    the prior task mode when the interpreter returns idle.
26. Verify Homing UI against the panel's `home-*`/`proghome-*` pins and the
    existing `baiermill_pb` gating. Test Z-first homing, individual axes,
    PROG.HOME current-WCS moves, faults and optional-axis display.
27. Preserve the PROG.HOME direct `issue_mdi` path exactly (immediate panel
    action, X/Y/Z only, current WCS); it follows the same Probe Basic execution
    semantics as other issued MDI commands.
28. Implement the manual tool-change acknowledgement through
    `linc.tool-change-confirm`, gated on a pending change, live panel and idle
    machine. Verify the existing manual prompt/confirm and `M61` semantics.
29. Defer ATC/turret implementation. When added, use the standard tool-table
    `P` column as the pocket source and update `toolchange.ngc`/`m6_tool_call_*`
    accordingly; expose real clamp/index state and never duplicate toolchange
    motion in the HMI.
30. Implement the Manual screen's JOG/INC/MPG selected-axis source. The
    RS-485 link carries steady one-hot modes and jog buttons, not a persistent
    selected axis; show `UNKNOWN` unless HALUI/pendant state is available. MPG
    counts must not be fabricated.
31. Implement program protect as a fail-closed backend gate: subscribe to the
    panel pin, recheck it for every program/offset/tool/file/probe write, and
    test the switch toggling with a dirty editor buffer and during an in-flight
    write.
32. Implement the loaded-program path plus live-trail layer and the
    `CLEAR TRAIL` view action. Sample actual tool-tip positions, cap the trail
    buffer, keep program and trail colors distinct, and verify that clearing
    never affects the loaded program or machine state.
33. Implement `linc.status-display` for the active WCS. Add `sseg_status` to
    the panel driver and map `g5x_index` to `54`–`59`, with `--` on
    unknown/stale or extended offsets.
34. Implement the `linc` HAL component and typed output allowlist:
    `hmi-alive`, `tool-change-confirm`, `status-display`. Verify ownership,
    startup/shutdown, heartbeat expiry and that no arbitrary pin writes exist.
35. Rename or alias the existing branded panel HAL component/pins to
    `cnc_panel` and update the machine HAL, or provide compatibility nets.
36. Prune the WHB04B-6 pendant nets to jog/handwheel-only in JOG mode. Ignore
    pendant mode, cycle, spindle, coolant, override, program and macro inputs;
    add a fault test proving those controls cannot change machine state.
37. Migrate the mill tool table to a standard `P` column only when ATC is
    enabled; keep manual profiles on `TZDR`. Validate the INI
    `TOOL_TABLE_COLUMNS`/`changer` match and hide the pocket column in manual
    mode.
38. Harden untrusted file ingress: canonicalize LOAD/SAVE beneath
    `PROGRAM_PREFIX`, reject symlink escapes, use same-directory atomic
    replace, bound the preview parser (no remaps/subroutines/side effects) and
    build all HMI-generated MDI with validated numeric templates.
39. Verify Probe Basic `issue_mdi` mode switching/restoration across MDI, probe
    and tool-change actions: panel selector state, task mode display, faulted
    or canceled commands, and no stale MDI mode left behind.

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
| `svg/probe.svg` | Touch Probe: Probe Basic-style category strip and illustrated operation cards, WCS/parameters, explicit `RUN PROBE` issue, result/apply and calibration |
| `svg/mdi.svg` | MDI: single-block command, quick inserts, DRO/modal, command issue via `issue_mdi`, history and last result |
| `svg/manual.svg` | Manual JOG/INC/MPG: dominant live graph, large work/machine/DTG coordinates and mode context; no on-screen jog controls |
| `svg/homing.svg` | Homing: per-axis/sequence state, HOME/PROG.HOME physical-key mapping and machine position; no software home controls |
| `svg/tools.svg` | Tools: standard tool table, conditional pocket column, current/next tool, explicit `Tn M6`, manual confirm, optional ATC/turret state |
| `svg/sim-panel.svg` | Simulation-only CNC control panel window: power, E-stop, mode, overrides, lamps, displays (including active WCS status) and key matrix |

Superseded vendor-style studies, intermediate layout explorations, and
colour-mapped previews were intentionally removed.

## 15. Configuration

Configuration is per machine and layered:

- `configs/base.toml` — UI, keymap, safety policy, HAL output policy, panel
  driver contract, backend and preference defaults.
- `configs/sim.toml` — mill simulation profile pointing at
  `../../baiermill_sim/probe_basic.ini`, the simulated CNC control panel and
  `rio_sim.py`.
- `configs/baiermill.toml` — physical mill profile pointing at
  `../../baiermill_pb/probe_basic.ini`, with the real CNC control panel owned by
  `linc-panel`.
- `configs/lathe-sim.toml` — lathe simulation profile pointing at
  `../../probe_basic_dev/configs/probe_basic_lathe/probe_basic_lathe.ini`,
  with `gui = "lathe"` and turret/live-tool capabilities.
- `hal/linc.hal` — core pre-GUI panel/HAL wiring.
- `hal/linc-postgui.hal` — post-GUI `linc.*` output wiring.
- `hal/linc-pendant.hal` — optional real-machine pendant jog-only wiring.

Rules:

1. A profile is selected explicitly with `--config`; there is no default.
2. Profiles `extends` `base.toml`; profile tables override base, arrays replace.
3. `machine.gui` selects `mill` or `lathe`; the LinuxCNC INI remains
   authoritative for kinematics, axis count/order, travel, limits, velocities,
   units, spindle count, ATC/turret settings, tool table and program paths.
4. The panel service owns the serial port; the HMI never opens it. LiNC writes
   only its declared `linc.*` HAL outputs and never arbitrary machine pins.
5. Safety policy is immutable from profiles: panel control except explicit
   MDI/probe/tool actions, no software E-stop/cycle/jog/spindle/coolant/
   overrides, pendant jog-only.
6. Runtime UI state is stored separately under
   `~/.config/linc/<profile>.preferences.toml`.

`linc --check-config` must validate the merge and path resolution without
connecting to LinuxCNC or HAL. The full schema, launch snippets and validation
contract are in `CONFIGURATION.md`.
