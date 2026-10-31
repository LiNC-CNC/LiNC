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
| `MAIN` | `F5` | The screen the panel's mode owns: Program in `EDIT`, Manual in `JOG`/`INC`/`MPG`, Homing in `HOME`/`PROG.HOME`, and the Machine dashboard in `AUTO`/`MDI` |
| `FILE` | `F6` | Program page; press again for the Files browser, and again to return |
| `TOOL` | `F7` | Tools page |
| `OFFSET` | `F8` | Offsets page |
| `WIN` | `F10` | Toggle `SPLIT`/`CODE` on the Program page, shown as `▢` in the sidebar shortcuts; `Ctrl+▢` cycles the toolpath view |
| `ESC` | `Esc` | Back / cancel / close detail |
| `Ctrl+D` | `Ctrl+D` | Diagnostics (unless the editable Program editor uses it for duplicate line) |
| `Ctrl+H` | `Ctrl+H` | G-code Help |
| `Ctrl+Q` | `Ctrl+Q` | Guarded quit |
| Arrows | — | Focus/lists/scroll/parameter adjustment |
| `PageUp` / `PageDown` | — | Page or tab navigation |
| `Home` / `End` | — | First/last row or line start/end |
| `Enter` | — | Select / apply / issue the MDI block |
| `Backspace` | — | Edit text |
| `MO(1)` | layer key | Symbols and mouse layer |

The six function keys are the only page keys the keymap can send, so the
remaining destinations take combinations: `Ctrl+D` for Diagnostics and
`Ctrl+H` for G-code Help, and `Ctrl+Q` for quit. `[keys]` in the profile binds all of them; the values in the
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
| `F5` `F6` `F7` `F8` `Ctrl+D` `Ctrl+H` | Go to the mode's page / Program / Tools / Offsets / Diagnostics / G-code Help |
| `M` | Open the full-screen messages dialog (`Enter`/`Esc`/`CLOSE` closes it) |
| `Esc` | Back, cancel, close detail; while a probe-trip fault is latched it resets and clears it; on a page that is only a view (Help, Diagnostics) it returns to the page it came from first; while a program is active it stops and resets it, the same as the panel's K4 |
| `Ctrl+Q` | Guarded quit |
| Arrows, `PageUp`/`PageDown`, `Home`/`End` | Focus, page, first/last item |
| `Enter` | Select / apply / issue the MDI block |
| `Backspace` | Edit text |
| `Ctrl+O` | Open the Files page, except on Program where `FILE` does it |

**Page-local** — consumed only while that page is in front:

| Page | Keys |
|---|---|
| Machine (MAIN) | `Ctrl+F10`/`Ctrl+▢` cycle the view ISO/X/Y/Z, `Ctrl+B` show/hide the program bounds, `Ctrl+M` REMAINING view, `Ctrl+0` fit the toolpath, `Ctrl+L` clear the live trail, `Ctrl+R` run from line, `Ctrl+G` go to a line number |
| Machine (MAIN), MDI pane | `Enter` issue and commit the block, `Up`/`Down` recall a buffer line into the input, `Ctrl+Up`/`Ctrl+Down` move the selected line in the buffer (`Ctrl` because the panel keyboard has no Shift), `Ctrl+G` go to a buffer line, `Ctrl+R` run the selected line, `Ctrl+Backspace` delete the selected line or clear the input, `Ctrl+S` save the buffer to a new `.ngc` file under `PROGRAM_PREFIX`, `Esc` clear the uncommitted input; while the pane is up these replace the Machine bindings in the row above |
| Program | `FILE` open the file browser, `Ctrl+S` save, `Ctrl+Z`/`Ctrl+Y` undo/redo, `Ctrl+A`/`Ctrl+C`/`Ctrl+X`/`Ctrl+V` editing, `Ctrl+D` duplicate line, `Ctrl+K` delete line, `Ctrl+Up`/`Ctrl+Down` move line, `WIN`/`F10` toggle `SPLIT`/`CODE`, `Ctrl+R` run from line, `Ctrl+G` go to a line number, `Ctrl+F10` cycle the view ISO/X/Y/Z, `Ctrl+B` show/hide the program bounds, `Ctrl+M` REMAINING view, `Ctrl+0` fit the toolpath, `Ctrl+L` clear the live trail |
| Files | `Left`/`Right` switch pane, `Up`/`Down` move, `PageUp`/`PageDown` page, `Enter` open or copy to `NC_FILES`, `C` copy to the other pane, `R` rename in place, `N` new program, `Ctrl+R` refresh, `E` eject, `Delete`/`Backspace` delete |
| Help | `/` or `Ctrl+F` focus search, `Up`/`Down` select, `Enter` open the reference, `Backspace` back in the reference dialog |
| Offsets | `Up`/`Down` select a row, `X`/`Y`/`Z` edit the DRO, `A` activate the selected work system |
| Tools | Arrows select a row, `Enter` edit the selected cell, `T` load the selected row's tool (`Tn M6` + `G43 Hn`), `Ctrl+T` type a tool number and load it |
| Probe | Arrows select a row |

Every page that draws a toolpath (Machine, Program, Manual, Homing and the
Offsets preview) pans with `Ctrl+Left`/`Ctrl+Right`/`Ctrl+Up`/`Ctrl+Down` and
zooms with `Ctrl++`/`Ctrl+-` while the pointer is over the toolpath card. The
chords are hover-scoped because they already belong to something else away from
the graph: `Ctrl+Left`/`Ctrl+Right` moves region focus and `Ctrl+Up`/`Ctrl+Down`
moves the selected line in the G-code editor and the MDI buffer. Wheel zoom
works the same way. Zooming in stops when the card's width spans 0.01 mm, so a
feature at the display's resolution can still be inspected; zooming out stops
where the fitted machine envelope is a few pixels across. `Ctrl+M` toggles
REMAINING on the same pages: it is page-scoped like the zoom, because no text
field and no other page claims it.

A page-local binding is only listed in the sidebar's `SHORTCUTS` pane while its
page is in front (the pane scrolls when a page has more commands than the
sidebar holds); nav keys are never listed. The list is grouped by purpose
(`VIEW`, `PROGRAM`) with half a text line between entries; the view toggles read
`▢` and `Ctrl+▢`. The bottom bar carries only live machine state.

While a dialog or the manual tool-change prompt is open it owns the keyboard:
its focused buttons and fields handle Enter, arrows and typing, the shell keeps
only Escape for a dismissible dialog, and navigation or page shortcuts do not
act on the page behind it. A dialog that exists to fill a field puts that
focus in the field the moment it opens, because there is no click-to-focus and
no Tab on this panel; a dialog with no field puts it on its **safe button** —
`CANCEL`, `KEEP` or the single `OK`/`CLOSE` — so the ring is always visible
somewhere and a reflex Enter never deletes, overwrites, quits or starts
anything. A field starts empty or selects its whole value, so the same Enter
accepts only what the operator typed. The
TOOLS page's typed tool-number dialog is one of those dialogs and is listed in
`LincApp::overlay_open()`; it takes its own `Enter` and `Esc` before the table
draws, because a refused number must keep it open and say why.

### 3.2 Physical panel

Source: existing panel driver sources, migrated into LiNC's `linc-panel` crate.

The CNC control panel is connected over RS-485 (115200 8N1) and exposed as a
LinuxCNC HAL component named `cnc_panel`.

The integrated panel service exposes:

- 58 button pins: the mill panel's 46 plus the 12 lathe-only keys
  (`spd2_cw`, `spd2_sp`, `spd2_ccw`, `spd_jog`, `feed_brake`, `tail`, `chip`,
  `tool_chg`, `mst`, `jog_cn`, `jog_cp`, `c_s`)
- 60 LED pins: the mill panel's 56 plus the lathe alarm column (`led_nc`,
  `led_plc`, `led_mmc`, `led_emg`)
- 2 seven-segment tool displays
- 1 two-digit status seven-segment display (`sseg_status`)
- 8 mode pins: `mode_edit`, `mode_auto`, `mode_mdi`, `mode_jog`, `mode_mpg`,
  `mode_inc`, `mode_home`, `mode_proghome`
- home/proghome outputs
- feed, rapid, and spindle override floats
- `panel_connected` and `communication_error`
- velocity inputs for jog LEDs

The pin surface is one surface for both panel faces: the lathe-only pins exist
on a mill too and simply carry no signal. The simulator window draws the face
the profile asks for — `machine.gui = "lathe"` or `[panel] driver =
"lathe-sim"` selects the lathe face, whose key legends, NC/PLC/MMC/EMG alarm
lamps and TOOL / TOOL COMP. / STATUS windows differ from the mill's.

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
  The allowlist is `linc.hmi-alive`, `linc.tool-change-confirm`,
  `linc.status-display`, `linc.jog-increment`, `linc.jog-speed` and
  `linc.machine-off`. It never writes arbitrary `cnc_panel.*` or
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
QMK keyboard --USB--> linc (Rust egui) --NML--> LinuxCNC
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
  `linc.hmi-alive`, `linc.tool-change-confirm`, `linc.status-display`,
  `linc.jog-increment`, `linc.jog-speed` and `linc.machine-off`.
- `linuxcnc`: task/motion/IO. Authoritative machine state.
- HAL glue: standard `halui`, the `linc` component, and `linc-panel`.

### 4.2 Rust workspace

```text
LiNC/
  crates/
    linc-core/        # state, commands, backend trait, validation
    linc-linuxcnc/    # native NML client + HAL reader; no Python
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

The LinuxCNC backend is **in-process native NML**: `linuxcnc-nml` owns the
`emcCommand`/`emcStatus`/`emcError` channels and `HalReader` reads panel pins.
There is no Python helper and no JSON transport. `linuxcnc-grpc` is only for a
remote panel, not the local HMI.

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
- It may live on a second monitor. The mill face's calibration/mockup is
  `svg/sim-panel.svg`.
- The profile selects the face. `machine.gui = "lathe"` (or `[panel] driver =
  "lathe-sim"`) draws the lathe panel transcribed from the lathe photo:
  C/C-S/X/Y/Z/A/B jog keys with RAPID, MPG SIM/SAFE DOOR/CORRIDOR,
  S.B.K/B.D.T/OPT, M.LK/MST/D.R.N, K1–K4, SPD.2.CW/SP/CCW (second spindle,
  only with `[lathe] sub_spindle = true`), SPD.CW/SP/CCW, SPD. JOG, FEED BRAKE, LUB, CHUCK, TAIL, CHIP, COOLANT,
  TOOL CHG. and WORKLAMP, with NC/PLC/MMC/EMG/RUN/READY alarm lamps and
  TOOL / TOOL COMP. / STATUS windows. The mill face is unchanged. Turret and
  live-tool drive stay unwired, so those keys grey out until
  `hal/linc-lathe.hal` wires them.

### 4.5 HAL wiring

- `hal/linc.hal` loads `linc-panel` and wires `cnc_panel.*` to `halui` modes,
  cycle start/pause/stop, jog, spindle, coolant/lube, homing, overrides, status
  LEDs and tool displays. The panel's jog buttons jog continuously in JOG only;
  INC jogs one step per press through the increment pins, and MPG is
  handwheel-only.
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
  feed, work offset, panel link, alarm/message counts, render rate (`NN FPS`,
  warning colour below 30), time.
- **No page titles.** The navigation names the page and the top bar names the
  machine state, so a heading above the content only costs vertical space.
  Each page keeps an invisible focus anchor at the top so arrow-key navigation
  has a starting point. Banners stay: they carry live state (E-stop, panel
  offline, program protect), not page descriptions.
- **Left navigation**: seven rows in one list — Machine, Program, Tools, Offsets
  with an icon and a two-line label, then Diagnostics, G-code Help and Settings as
  single-line rows without an icon. Rows keep a 44 px target while the painted pill
  is 32 px (22 px for a single-line row), separated by 3 px, so the list costs the
  vertical space the shortcuts pane needs. Keyboard-focusable; `MAIN`/`FILE`/
  `OFFSET`/`TOOL`/`WIN` jump directly, `Ctrl+D`/`Ctrl+H` reach the two key-only
  pages. `[ui] pages` still selects and orders which rows appear.
- **DRO readout**: one line — axis name at the coordinate's own size, the value,
  and a dimmed `M …` machine value that is dropped whole when it would touch the
  value. The card carries the DTG line and move bar underneath, nothing above.
  The value is the **commanded** position by default, which is where the
  interpreter thinks the tool is; `[ui] dro_position = "actual"` shows the
  feedback position instead. DTG stays the controller's distance to go, and the
  control paths (touch-off, zero, DRO entry) always use the actual/work values.
- **Central page**: active screen.
- **Bottom action bar**: real keycaps (`MAIN`, `FILE`, `OFFSET`, `TOOL`, `WIN`,
  `ESC`), context actions including `HELP`, and a compact read-only machine
  status line (E-stop, machine on/off, the wired probe input, `OVR F/S/R`,
  program protect, alarm count). `MACHINE OFF` is a fault and blinks at 2 Hz
  between the error colour and a dimmed error colour; `MACHINE ON` stays steady.
  The probe field reads `PROBE READY`,
  `PROBE TRIPPED` or `PROBE ?` from `probe.input_tripped` and asks on stale
  data; it is a readout like MACHINE ON, with no click target and no command.
  It is drawn only while the probe is fitted — a probe configured
  (`[probe] enabled`) **and** `[probe] tool_number` equal to the tool in the
  spindle (`tool_change.current_tool`, the TOOLS page's CURRENT tool). A cutter
  in the spindle, an empty spindle or a profile without a probe leaves the field
  out entirely: no placeholder and no dimmed row, so nothing else on the bar
  shifts.
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
- 3D navigation follows Blender: middle-drag orbits (yaw about the vertical,
  pitch above the XY plane), Shift+middle-drag pans, the wheel zooms, and a
  double-click returns to the default isometric view. The camera stands on the
  operator's side, yaw 0 looking from -Y, so the rendering matches LinuxCNC:
  +X to the right and towards the viewer, +Y away into the back, +Z up. The top
  and side views keep their fixed direction; primary drag pans on every view.
- `Ctrl+F10` cycles ISO → X (looking along X) → Y (looking along Y) → Z
  (looking down); the lathe toggles between ISO and its XZ view. A fit frames
  the program alone — the tool and the live trail are excluded, since a far
  marker would shrink the part — and re-fits on load and on every view switch,
  leaving a manual zoom/pan alone in between.
- `Ctrl+B` cycles what the toolpath draws: (1) the program dimensions alone —
  the default — (2) plus the machine envelope, (3) plus the program box outline,
  (4) everything off, then back to (1). The position is saved in the preferences,
  so it survives a restart. When drawn, the boxes are solid and faint, never
  dashed, because the G0 path is dashed.
- The program bounds are marked like a technical drawing: a dimension line
  outside the upper edges of the drawing (X along max Y, Y along min X, Z up the
  near-left corner), witness lines from the corners, arrowheads at the ends,
  the X/Y/Z length in the middle and the work coordinate of each end beside its
  arrow. The text is Hershey stroke geometry (the AXIS font) laid into the plane
  it measures — X and Y flat in XY, Z in XZ — so the camera projects it with the
  part. In a side view one axis points at the viewer, so its labels are left out
  and the other two keep their baselines but roll a quarter turn to face the
  camera; an end value whose baseline would lie along the view is drawn flat,
  inside its span or just past the end, so it stays readable and clear of the
  line. There are no PROGRAM or MACHINE captions; the geometry says which is
  which. The stock block is fully transparent by
  default (`toolpath.stock_alpha = 0`) and configurable up to opaque.
- Program card, a `FEED / SPINDLE` card and a `SERVO LOAD` card beneath the
  toolpath. The two bottom cards fill the toolpath's width, so the servo card's
  right edge lines up with the graph's right edge. The feed/spindle card shows a
  large `F` and `S`, each with the
  programmed rate and override above the effective rate and its enable/on flag.
  The feed value is the programmed F word, converted from the program's G20/G21
  units to the machine's linear units per minute, so an inch program on a
  millimetre machine still reads in mm/min; the override is a separate factor,
  not the rate. The second S row is the true feedback from
  `[hal.pins] spindle_speed` (RPS converted to RPM), so turning the spindle
  override changes the displayed true RPM; when that pin is absent or reads 0
  the backend uses `spindle.N.speed-out-rps`, then the programmed speed scaled
  by the override. The override percentage is the panel value. A vector
  rotation arrow beside
  the `S` shows forward (green),
  reverse (amber) or stopped (muted dot), so the RPM value itself always stays
  positive.
  The servo card draws one bar per entry in `[ui] servo_load` (machine axes plus
  `S` for the spindle): a green (0-80 %), yellow (80-100 %) and red (100-120 %)
  ruler behind an accent progress fill, with the percentage to its right. An
  empty list hides the servo card. Controller messages are not a pane: `M`
  opens them full screen, so the log is readable without crowding the MAIN
  layout.
- The square (`F10`) replaces those two cards with a `FOLLOWING ERROR` pane:
  one lane with a trace per axis, each in its own colour, on a scale they share
  (a per-series scale would draw a quiet axis as busy as a struggling one).
  The header carries three unlabelled-then-labelled groups per axis: the live
  value, the largest absolute error since the program started, and the mean
  absolute error over the same run. The worst and the mean start again with
  each program, so a fresh run is never judged against the last one; jogging
  and MDI between programs count towards them. The lane draws the last 240
  samples, so it shows the shape while the numbers show the total.
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
- Feed/spindle card, the configured servo-load bars and live-tool load when available.
- Compact panel state in top/bottom bars only; no machine controls.

### 6.2 Program

Two layouts, selected by the header toggle or `FILE`:

#### SPLIT

Editor keys while the code has keyboard focus: `Home` / `End` move to the start
and end of the line, and `PgUp` / `PgDn` move by a screenful — or to the start
and end of the program when it is shorter than one.

- G-code editor/monitor left (~58%).
- Mill: 3D toolpath right (~42%); lathe: 2D XZ toolpath, with optional 3D when
  Y/C or live-tool paths are active. The loaded program path and live trail are
  separate colors; `CLEAR TRAIL` clears only the live trail.
- Live-tool paths use a distinct stroke from turning/rapid moves.
- Current line synchronized with the graph, and the running line marked in the
  editor the way the Machine page's G-code display marks it: an accent bar in
  the gutter plus a tinted row. There is no separate current/next pane.
- `MODAL` is its own pane under the toolpath, up to eight columns so a full set
  of active codes stays on one row. Its cells carry the group label, the active
  code and a short mnemonic for what that code means (`UNITS` / `G21` / `MM`),
  with half a line of clearance under each cell so wrapped rows read as rows.
  The Machine dashboard keeps the two-line cell — group label and code — because
  its strip has no height to spare; the code window above it is sized from what
  the strip leaves.
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
- The backend requires an idle interpreter because `AUTO_RUN` on a paused
  program would resume it rather than start at the line.

#### GO TO LINE

`Ctrl+G` asks for a line number and moves the program view there; the editor,
the path highlight and the MAIN program card follow it. While a program is
running the view follows the executing line, but a line the operator picks with
`Ctrl+G` or the arrows is kept until the next run starts, instead of being
dragged back on the next status poll.

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
  marker and must not run while a program is active. The saved file is also
  made the controller's loaded program: LinuxCNC keeps the copy it opened in
  the interpreter, so the backend reopens the file after the write and `AUTO`
  starts the saved text, not the version loaded before the edit.
- A buffer with no file yet (a program typed straight into `EDIT`) asks for a
  name instead of refusing: the dialog shows the `NC_FILES` folder it will land
  in, appends the configured G-code extension when the name has none, refuses a
  name that already exists, and the buffer keeps that path for later saves. A
  save still needs the panel in `EDIT`, an idle interpreter and `PROTECT` off;
  when those are missing the unsaved-changes dialog greys `SAVE` and names the
  condition that blocks it.
- `DISCARD` in the unsaved-changes dialog really drops the buffer: it is
  reloaded from the loaded program (or emptied when nothing is loaded) before
  the operator's destination is entered.
- Unsaved changes must be confirmed before leaving Program, opening another
  file, or quitting.
- Because the panel moves the mode itself, the HMI cannot hold `EDIT`. Leaving
  `EDIT` with a dirty buffer warns once that `AUTO` can only start the loaded
  program, and offers `DISCARD BUFFER` or `KEEP` until the operator returns
  (a save is impossible in any other mode).
- A successful load starts the view at line 1 and highlights it, whatever line
  was selected in the previous program.
- G/M/axis words are displayed uppercase; comments retain their case and the
  lexer is case-insensitive.
- Typing applies the MDI pane's spacing rule inside the editor too: a word typed
  against a non-space inserts one space in front of it, including a `G` or `M`
  block after a value word (`G1X10` → `G1 X10`, `X10G0` → `X10 G0`). Each line
  starts without a leading space, and nothing is inserted inside comments,
  expressions, or when text is pasted or recalled.

Program rules:

- G-code is token-highlighted (see §7.3).
- The graph always draws the **loaded program's full toolpath** and the
  **live toolpath** as separate layers. The program path uses the program
  colour (cut) and muted dotted rapids; the live trail uses a distinct
  `--success` stroke, solid for a cut and dotted for a rapid, so both layers
  read a rapid the same way.
- The program path is drawn in machine coordinates, the frame the tool marker
  and the live trail use: the work offset the program selects is applied, and in
  lathe `G7` diameter mode an X word is a diameter, halved exactly as the
  interpreter halves it, so the path lands where the machine goes.
- A leading rapid that only approaches the program from the live tool position
  is not drawn: the path starts at the program's first move, so no dotted
  connector crosses the part. A program whose only move is that rapid still
  draws it.
- The toolpath card's status line is the key: `— CUT`, `-- RAPID`, `— LIVE`
  and an amber `▽ TOOL`, with the view (`3D / ISO`) and the width of the
  card's viewport in millimetres on the right (`3D / ISO  •  184.00 mm`). The
  number is the drawn scale, so it follows a fit and the zoom chords alike. A
  legend box inside the graph is not drawn — it repeated that line and covered
  the drawing.
- The toolhead is an inverted amber triangle whose point sits on the tool
  position, not a shape centred on it: the operator reads the tip, and a circle
  with a crosshair left it ambiguous at display size.
- The live trail is the actual tool-tip path since the last clear, including
  program, MDI and jog motion. Each sample carries whether the interpreter's
  active motion mode was G0, which is what makes a rapid hop dotted. It is not
  the commanded program path and must never overwrite or hide the loaded
  program.
- A focusable, view-only `CLEAR TRAIL` action clears only the live trail and
  re-seeds it at the current tool tip. It does not clear the loaded program,
  selected line, offsets or machine state, and is safe while running. A
  successful new program load starts a fresh trail.
- REMAINING (`Ctrl+M`, a view toggle on every page that draws a toolpath)
  replaces the two layers with one picture of what is left to run: the live trail
  is not drawn at all, and every block the interpreter has already left behind is
  eaten out of the program path. The block being cut stays drawn until the next
  one starts, so the mouth of the hole is where the tool is. A program that ran
  to its last line has eaten the whole path; Esc (reset) rewinds the interpreter
  and brings it back, as does turning the mode off. The legend key names the hole
  (`EATEN PATH`) rather than a trail that is not there. No coordinate, offset or
  controller state changes.
- The eaten line is the interpreter's, so it is exact only while the preview is
  the running program: a dirty editor buffer in REMAINING mode draws the edited
  path against the running program's line numbers.
- The graph overlays program bounds and machine bounds. Program bounds are a
  dashed cyan box around the last valid program extents; machine bounds are a
  dashed muted envelope from the INI travel limits and kinematics. Both use the
  same view transform as the toolpath.
- Any part of the loaded program path outside the machine travel envelope is
  drawn in the `--error` red (`theme::ERROR`, `#ff5c5c`). Red therefore means
  outside the machine boundary. A move that crosses a limit is split at the
  exact crossing: the inside keeps its cut, rapid or selected-line stroke, and
  only the out-of-travel part turns solid red, so a selected line is still
  highlighted where it is reachable. A move whose endpoints are both outside
  but which passes through the envelope keeps the normal colour between its
  two crossings. The classification uses the same per-axis
  `MIN_LIMIT`/`MAX_LIMIT` box as the machine bounds.
- The selected/current G-code line is highlighted in the graph: its segment or
  arc uses a brighter stroke and a line-number tag, distinct from the execution
  marker. The tag sits at the middle of the move (the end of one block is the
  start of the next), once per line.
- Canned drilling cycles are previewed as geometry: G81/G82/G83/G73 move X/Y
  at the current Z, rapid to the R plane, feed from R to the Z depth (the solid
  cutting move), and rapid to clear Z (G98: the initial Z when it is above R,
  otherwise R; G99: R). If the tool starts below R, Z moves to R before X/Y.
  R is the retract plane. Each block with X/Y is another hole; G80 or an
  ordinary motion mode cancels the cycle. G83 pecking and G82 dwelling are not
  drawn as separate moves. A polar `@`/`^` pair names the hole too, and `L`
  repeats the cycle once per pass, stepping the polar radius/angle in
  incremental mode and repeating the same hole in absolute mode.
- Cutter compensation is previewed as the tool centre, not the programmed
  contour. G41 keeps the tool centre a tool radius to the left of the
  programmed direction and G42 to the right, in the G17/G18/G19 plane. The
  radius comes from the D word's tool in the tool table, or from the tool in
  the spindle when there is no D word; G41.1/G42.1 carry the radius in D
  directly. Consecutive moves are joined where their offset lines cross, an
  arc keeps its centre and has its radius moved by the tool radius, and the
  first compensated move is the lead-in from the programmed point. A path the
  tool cannot follow is reported rather than drawn: a lead-in shorter than the
  radius, a corner too sharp to join, a tool wider than the arc it must follow,
  or a D word naming a tool with no radius on file. The block is then drawn on
  the programmed line and compensation re-engages on the next move.
- The read-only status parameters a program reads are live, not zero: they
  follow the blocks the preview has already planned, in execution order, so a
  macro that saves `#<_x>`/`#<_z>` and measures its cut from them draws the cut
  where the machine would make it. The preview publishes `#<_x>`-`#<_w>` (the
  work position, in the program's units) and `#<_abs_x>`-`#<_abs_w>` (the same
  with the work offset left out) with their `#5420`-`#5428` and
  `#5021`-`#5029` mirrors, the modal flags (`#<_metric>`, `#<_imperial>`,
  `#<_absolute>`, `#<_incremental>`, `#<_plane>`, `#<_motion_mode>`,
  `#<_ccomp>`, `#<_coord_system>`, `#<_tool_offset>`, `#<_retract_r_plane>`,
  `#<_retract_old_z>`, the feed and speed modes, `#<_ijk_absolute_mode>`,
  `#<_lathe_diameter_mode>`/`#<_lathe_radius_mode>`), the feed, speed, tool and
  pocket parameters, the coolant and spindle flags and the override flags,
  `#<_value>` and `#<_value_returned>` from the last `return`/`endsub`, plus
  `#<_line>`, `#<_call_level>`, `#<_vmajor>`, `#<_vminor>` and
  `#<_remap_level>`. `#<_task>` reads 0: the preview is the interpreter that is
  not part of milltask, which is what probe macros test it for. A parameter this
  preview cannot know, such as a probe result a remap names, still reads 0
  rather than a guess. Four limits are stated rather than hidden: the live Z is
  the retract plane across a canned cycle that names more than one hole, the
  absolute position ignores a `G10` the same program wrote, `#<_metric_machine>`
  answers with the program's units because the preview is given the program and
  not the machine's setup, and `#<_current_tool>`/`#<_current_pocket>` follow the
  last T word because the preview has no tool changer to ask.
- On editor changes, wait for the debounce, run the preview syntax check, then
  replace the graph only if it succeeds. On failure, keep the last valid graph,
  mark the offending line and show a syntax error; never move the machine.
- While running, the editor is read-only and auto-scrolls to the current line,
  centring its whole wrapped block rather than its first row. The MAIN program
  card does the same, measured in the rows the lines really take, so wrapped
  code above the executing line cannot push it down the card.
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

- Work offsets G54–G59.3: all nine systems the controller numbers (G59.1–G59.3 included), with a rotation column.
- The toolpath preview is drawn in machine space in the work system the program
  selects (`G54`..`G59.3`, first selection in the file), falling back to the
  active system when the program selects none, so the part sits at the origin it
  will be cut from. The preview beside the table frames every actor — the
  program, the live trail, the tool tip, the machine origin and both work-origin
  markers — so an offset far from the part is still on screen; its height
  matches the table so the two panes end on the same line. The machine origin
  gets a muted triad, joined to the active work origin by a dashed green line
  with an arrowhead at the work end. A non-active row that is selected (or being
  edited) shows its system's origin as a translucent blue triad with a blue
  dashed arrow from the machine origin, and keeps doing so until another row is
  selected; selecting the active row adds only its name to the active marker.
- Tool offsets. G92 is deliberately not exposed here: it is a transient offset
  that silently shifts every WCS, and a field for it invited the operator to
  leave the machine in a state neither the DRO nor a program could explain.
- Mill: X/Y/Z and rotary offsets; lathe: X/Z geometry and wear, tool nose
  radius, tip orientation, C/Y offsets and turret station.
- A DRO row on top: the active system's work coordinates, large, with the
  machine coordinate small underneath. A work value is editable in place and
  accepts arithmetic (`-12.5`, `10+2.5`); committing writes the position with
  `G10 L20`, so the DRO then reads the entered value and the offset moves to
  match. It is a work coordinate, so it is not checked against machine travel.
- `A` makes the highlighted table row the active work system (`G54`..`G59.3`)
  without leaving the page. The binding is page-local to Offsets: Manual keeps
  `A` for ZERO ALL and Probe keeps it for APPLY RESULT. The controller selects
  the system with an MDI `G5x`; the HMI verifies the active index it reports
  and mirrors it into the parameter file. An open cell editor or dialog owns
  the key first.
- Current axis values and distance-to-go.
- Read-only while running; blocked when program protect is active.
- Offsets edited in the table are validated against LinuxCNC limits and units.
- `OFFSET` (F8) cycles from Offsets into the probe tabs; there is no separate
  Touch Probe button. Probing runs in position-only mode; `A` applies the last
  result to the selected work offset.

### 6.4 Tools

Primary quick tool-change surface; no MDI entry required.

- Tool table backend is the **standard LinuxCNC tool table**, read/written at
  the path in INI `[EMCIO] TOOL_TABLE`. Columns come from
  `TOOL_TABLE_COLUMNS`; the HMI uses that schema directly.
- **Pocket column:** shown only when an ATC/turret changer is used; a
  `manual` changer hides it in the Tools page. The `P` word stays in the file
  for every changer: LinuxCNC keeps one tool per `P` value, and a file without
  `P` words loads only its last tool (every other `T` word then fails as "not
  in the tool table"). An `atc`/`turret` profile requires `P` in
  `TOOL_TABLE_COLUMNS`; the loader refuses to start if it is missing.
- The page is the table: there is no separate change pane and no changer
  pane. Current and prepared tool are shown above it as `CURRENT; Tn` and
  `PREPARED; Tn` (the numbers in the axis-label amber).
- Tool table: number, description, dimensions/geometry, optional
  pocket/station and type. The type is LiNC's label, not machine data: the
  LinuxCNC tool table has no type column, so it is inferred from the geometry
  and can be overridden per tool with the TYPE selector, which is kept in the
  preferences file.
- `T` loads the selected row's tool: `Tn M6` followed by `G43 Hn` in one
  backend command, so the length compensation is in effect as soon as the
  change completes. The shortcut does not fire while a cell is being edited.
- `Ctrl+T` (`[keys] tool_number`, page-local like `T`) opens a small dialog
  that types a tool number directly, for an operator who knows the number and
  does not want to cursor to the row. It opens on one line of digits prefilled
  with the tool in the spindle and the whole value selected, so `Enter` alone
  re-loads that tool and typing replaces it. `Enter` confirms, `Esc` cancels,
  `Backspace` edits and the digits append; there is no click-to-focus and no
  Tab, so the field takes the keyboard on the frame it opens, and gives it back
  when the dialog closes. The dialog shows the numbers the table accepts and
  `0 = NO TOOL`, and it names the problem on a refusal (`T7 is not in the tool
  table`, an empty entry, a non-numeric one) without closing. Only a tool
  number the table carries (a tool number, not a row index) or `0` is accepted.
  Confirming issues the same `LoadTool` command bare `T` issues, under the same
  `issue_allowed` precondition, whose reason is reported inside the dialog; a
  success toasts like the row load. `0` is `T0 M6` plus `G43 H0`: the spindle
  ends up empty with no length compensation in effect. It is part of
  `LincApp::overlay_open()`, so the table, the cell editor and the shell's
  global keys stand down while it is open.
- `Ctrl+N` writes a new row with the next free tool number; the configured
  probe tool number is skipped, and an ATC/turret table gets the next free
  pocket. `Backspace` asks for confirmation and then deletes the selected row.
  The probe tool and the active tool (the one in the spindle) are refused, and
  nothing is deleted before confirmation.
- Editing a cell selects the whole value (Home/End move inside it). Enter
  writes the row, reloads the tool table into the controller and re-applies
  `G43` when the edited row is the tool in the spindle. Escape restores the
  previous value. The open cell keeps the selection border's size and turns
  red; the same applies to the OFFSETS table cells.
- `T0` unload and manual-change confirmation are supported.
- Manual changer: after `M6` pauses for the manual change, a modal prompt
  appears on every page — not only on Tools — showing `INSTALL target`,
  `REMOVE current` and the tool's remark. It follows the `M6` request, so it
  cannot be dismissed; the only action is confirming that the tool is in.
- Confirming is a handshake, not motion, so it is allowed while the program is
  running or paused, and it does not need cycle start. It raises the declared
  `linc.tool-change-confirm` HAL output and **holds it** until `iocontrol.0.tool-change`
  drops — a pulse shorter than the EMCIO cycle can be missed. It is not an
  arbitrary HAL write.
- The acknowledgement token is derived from a counter that increments on the
  rising edge of the request, so it is stable for one change episode and a late
  press cannot confirm the next one.
- Cycle start is gated on `halui.program.is-idle`; a program that is reading or
  waiting for a tool change is neither idle nor paused, and starting it again
  produces `EMC_TASK_PLAN_RUN` errors. Resume is the command for a paused
  program.
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

The Turret / Live Tool tab appears only with `[diagnostics] turret_tab = true`
on a profile whose `[tools] changer` is `atc` or `turret`; for every other
profile it is absent from the strip and from the PgUp/PgDn cycle.

No fault reset, acknowledge, cycle, jog, override, or E-stop controls here.
`CLEAR VIEW` clears only the UI log view, never machine faults.

#### Controller messages

LinuxCNC's operator errors arrive on the NML error queue (`emcError`) and become
alarms: an `ERROR NML` row in this table, an entry in `MESSAGES`, an increment of
the `ALARM`/`MSG` counters in the top bar, and a red toast — which lives 12
seconds rather than the 5 a status note gets, because the operator is still
reading the console when it appears. An interpreter error that stops a program
(`emc/task/emctask.cc: interp_error`, e.g. `Cannot do g1 with zero feed rate`)
takes exactly this path; the console line is not the only witness.

One aborted block prints the same failure several times. Identical texts inside a
one-second burst are one entry; a repeat after that window is the operator trying
again and is always reported again. Silently swallowing the second attempt made a
stopped program look like an HMI that missed the error.

### 6.6 G-code Help

Offline LinuxCNC G-code reference, available without network access and without
stopping a running program.

- One quick-reference table grouped by Motion, Canned cycles, M-codes, O-words
  and Other codes, with Probe Basic's columns: Code, Parameters, Description,
  Meaning. Group rows span the table; the focused row carries the selection band
  and rim bar.
- Search matches code, parameters, description and meaning. Code matching
  ignores spaces and commas, so `g2g3` finds `G2, G3`.
- `Enter` or a double-click opens the full section in a near-fullscreen dialog:
  title, description, parameter table, figures, notes and related codes. Up/Down
  and PageUp/PageDown scroll the document; `Esc` or `CLOSE` closes it. At the
  bottom, Down enters the RELATED chips: the arrow keys choose one, `Enter`
  opens it, and `Enter` with no chip chosen closes the dialog. Related links
  navigate inside it and `Backspace` returns through that history. It blocks the
  page behind it while open.
- The dialog renders the generated documentation natively; there is no HTML
  engine or webview. It keeps the source's inline markup (bold, italic, inline
  code and internal links), and an inline link opens its target by the same
  history path as a RELATED chip. A link to a section this bundle does not carry
  is drawn as plain text, so nothing clickable is dead.
- Code blocks are syntax-coloured with one egui `LayoutJob`: G/M words, o-words,
  axis letters, numbers, parameter references and comments take Probe Basic's
  `_SYNTAX_REMAP` colours, and a comment is italic.
- Warning, Note, Tip, Important and Caution admonitions each keep their own
  label, accent and background, so none collapses into one warning banner.
- A documentation table draws a header band, alternating zebra rows, one cell
  pad on each side of every column, centred header cells and right-aligned
  numeric cells. The pinned bundle carries single-cell layout tables only, so
  today the header band and zebra rows show on the index table.
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
  duplicated path in HMI config. The square (`F10`) key flips it to `RECENT`,
  the programs opened most recently, newest first, from
  `preferences.recent_files` (deduplicated, bounded by `[files]` history).
  `Enter` loads a recent row. `C`, `R` and delete act on a directory row, so the
  recent view does not offer them; the left pane is unaffected.
- Show device label/capacity, free space and mount state (`MOUNTING`, `MOUNTED`,
  `NO USB`, `ERROR`). No button row: `E` ejects.
- Mount removable filesystems with `nosuid,nodev,noexec`; never auto-run files
  from USB.
- Two equal panes, no page header and no button row: every action is a key.
  `NC_FILES` holds the focus on entry, because the program list is what
  `LOAD`/`FILE` is for; `Left`/`Right` move the focus between the panes — the
  focused pane's title turns accent and the other pane's row band dims;
  `Up`/`Down` move the
  selection and `PageUp`/`PageDown` move a page, with the list scrolling the
  cursor into view; `Enter` opens a program from `NC_FILES` or copies a file from USB
  into `NC_FILES`; `C` copies the selected entry to
  the other pane; `R` renames it in place (the NAME cell becomes a field with the
  name without its extension selected; Enter commits, Escape restores);
  `Ctrl+R` refreshes; `E` ejects; `Delete` (or `Backspace`, since the CNC
  keyboard has no Delete key) deletes the selected file after a confirmation
  naming its full path. `Ctrl+C`/`Ctrl+X`/`Ctrl+V` stay with text editing and
  are not Files actions; the `..` row opens the parent folder.
- The lists are rows like the tool table: NAME/SIZE/MODIFIED header, a striped
  row per entry, the selected row's band and rim bar, and no column selection.
- Inside a directory the list starts with a `..` row. It is selectable with the
  arrows and `Enter` opens the parent; a single click selects it, a double click
  opens it. At the `NC_FILES` root it is hidden, because that is where the
  configured tree starts; `[files] show_parent_above_root = true` shows it there
  and allows browsing above `PROGRAM_PREFIX` (loading a program still requires a
  file inside `NC_FILES`).
- Copy and delete selected regular files with `C`: from `NC_FILES` to USB, from
  USB to `NC_FILES`. Folder navigation only; no recursive folder copy in v1.
- Copy requires free space, rejects symlinks/path escapes, confirms overwrite,
  writes to a temporary file in the destination and renames atomically.
- Copy progress and errors are visible; `E` is refused while a copy is active.
- `E` unmounts the filesystem and then powers off the drive; never
  force-unmount by default. Show `SAFE TO REMOVE` after success.
- The overwrite dialog keeps the keyboard on its two buttons: arrows cycle
  between OVERWRITE and CANCEL and cannot move focus to the page behind it.
- `N` creates a new program in the current `NC_FILES` directory, named
  `untitled.ngc` (or the next free `untitled-N.ngc`), containing a header
  comment, a modal setup line with the machine's units and the active work
  system (`G21 G90 G54 G17 G40 G49 G80`), `G94`, blank body space, `M5` and
  `M30`. It then opens the name in rename mode. No motion, tool call or spindle
  start is written. Creation is blocked by program protect. The new row is
  scrolled into view before its name field takes the keyboard, so a list longer
  than the pane does not hide the field the operator is typing into.
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
existing Probe Basic G-code subroutines and math, vendored verbatim under
`subroutines/probe_basic`, and follows Probe Basic's category-first workflow:
choose a routine group, then pick the operation card.

- Category strip, matching Probe Basic: Outside Corners, Inside Corners,
  Boss and Pocket, Ridge and Valley, Edge Angle, Calibrate and
  Probe Help. Rotary-axis probing is not offered. The tabs are passive
  indicators: they are not buttons and take no keyboard focus.
- `OFFSET` (F8) cycles Offsets → Outside → Inside → Boss + Pocket → Ridge +
  Valley → Edge Angle → Calibrate → Help → Offsets. `Esc` returns to Offsets
  from any probe tab. `PgUp`/`PgDn` also cycle the probe tabs.
- An editable X/Y/Z DRO sits above the parameter card, spanning the same left
  and right edges. Its work value is edited in place; the machine coordinate is
  shown underneath. `X`/`Y`/`Z` open the matching DRO, `Enter` writes the work
  position with `G10 L20`, and `Esc` cancels.
- Each category opens a grid of illustrated operation cards. The DRO,
  parameters and results stay on the left, the routine matrix is centred and
  the probe symbol is on the right. Work offsets remain on the Offsets screen rather than being
  duplicated here; the active WCS is shown in the parameter header. The two
  left-hand cards share one right edge; the probe pane fills the body height
  and bottom-aligns with the routine matrix.
- No routine is selected on entry or when a tab changes. Pressing a number is
  the only way to highlight a routine; `Enter` before that reports that a
  routine must be selected.
- Edge operations (X±, Y±, Z surface) remain available within the relevant
  category; the Z surface routine is the centre card (`5`). Calibration
  includes round/square boss/pocket and reset.
- Parameters mirror `touch_probe_param_update.ngc`: probe tool number,
  fast/slow/traverse feed, max XY/Z distance, XY/Z clearance, extra depth, step
  off width, edge width, diameter hints, probe mode, WCS rotation, calibration
  offset/diameter and square-calibration axis. The calibration-only fields
  live in the Calibrate panel.
- Probing always uses Probe Basic's position-only mode. The result is recorded
  but not written to the WCS by the subroutine; `A` is the explicit shortcut
  that applies the last result to the active work offset (PROTECT must be off).
- Parameter entry follows the Offsets table: click a field to select it, the
  arrow keys move between fields, `Enter` opens the inline editor, `Enter`
  again commits the value and `Esc` cancels. Arithmetic expressions are
  accepted, as in the offset cells. On Boss + Pocket and Ridge + Valley, the
  operation-specific hint fields in the routine pane are stacked one per row,
  with a shared label column and a shared input column. `Ctrl+P` selects the
  first visible hint there instead of the first common parameter; Up/Down move
  one stacked hint at a time, and Left/Right do not leave the single column.
  On Calibrate, Down from the last dimension moves the focus to the CAL ON
  buttons; Left/Right choose one, Enter runs it, and Up from the first button
  returns to the dimensions.
- `Enter` builds the selected `o<probe_...> call` and its argument values,
  then issues it immediately using Probe Basic's `issue_mdi` semantics:
  temporary MDI mode, command sent, previous task mode restored when the
  interpreter returns idle. This is the explicit operator action; there is no
  separate RUN button and no physical-CYCLE-START step.
- Probe motion requires machine ON, homed axes, probe tool T99 in the spindle,
  idle interpreter, no active program and valid parameters. A syntax/validation
  error issues nothing and marks the failing condition.
- Read results from the subroutine outputs (`#1001`–`#1012`): X-/X+/Y-/Y+ and
  Z- probed positions, widths and centres, averaged diameter, edge delta and
  edge angle. `U` issues `UPDATE PROBE PARAMS`; `Ctrl+R`, `Ctrl+X` and
  `Ctrl+Y` issue `RESET ALL DATA`, `X DATA RESET` and `Y DATA RESET`. The
  results panel carries no buttons: those keys are the whole surface, and the
  shortcuts rail lists them.
- Calibration routines update the probe calibration offset; `RESET CAL` clears
  it through `probe_cal_reset.ngc`.
- The page has no general cycle-start, jog, override or E-stop controls. The
  specific `Enter` action is the only motion initiator, and the hardware
  E-stop remains independent.

### 6.9 MDI

There is no MDI page. Selecting `MDI` on the physical panel opens the MAIN page
with the right-hand `PROGRAM` card replaced by the MDI pane; any other panel
position restores the program card. The pane is keyed to the panel mode, not to
page entry, so returning to MAIN while the panel still sits in MDI shows it
again. No left-nav item is added.

Layout, top to bottom:

- Block listing: the MDI blocks entered so far, in the order they were issued,
  with the selection band on the row the arrow keys point at.
- Syntax-highlighted single-line input. It holds focus by default
  (`mdi.auto_focus`), so a block can be typed as soon as the pane appears.
- One-line result/error strip under the input: the last issue result, or the
  validation error that stopped it.
- MODAL band at the bottom of the card, under the strip: the same MODAL card
  AUTO shows, same widget and same columns, so the active codes read the same on
  both pages. It is anchored to the card's rect and takes its height from the
  code count, so a tall band shrinks the block listing and never moves the band.
- The quick-command button row is gone: no chip row, no `RUN SELECTED` button.

The pane's card stays exactly in the slot the `PROGRAM` card would have filled;
every part is laid out from the card's rect in the same frame, never from a
height measured on the previous one.

Keys, all page-local to the pane:

- `Enter` issues the block through Probe Basic's `issue_mdi` semantics and
  commits it to the buffer (`mdi.execute_on_enter`). A block recalled with
  `Up`/`Down` overwrites that same buffer line in place; otherwise `Enter`
  appends a new line.
- `Up`/`Down` move the selection through the buffer and load that line into the
  input for editing. Recall is in place: the line stays visible in the listing
  while it is being edited, and the text cursor lands at its end.
- `Ctrl+Up`/`Ctrl+Down` swap the selected buffer line with the one above or
  below it. The selection follows the moved line, so a second press keeps moving
  the same block; at the first and last line the key does nothing (no wrap, no
  pop), with no selection it does nothing, and it never issues a command, opens
  a dialog or edits the input. `Ctrl`, not `Shift`: the panel keyboard has no
  Shift key. `[keys] mdi_line_up`/`mdi_line_down` rebind it; a profile that sets
  one to a bare `Up`/`Down` is rejected like an unparsable chord, so it can never
  shadow recall.
- Typing a word against anything that is not a space, a tab or the start
  of the line inserts one space first: `G1X10` arrives as `G1 X10`, `X10Y5` as
  `X10 Y5`, `X10G0` as `X10 G0`, and `M3 S1200` as typed. The words are
  `X Y Z A B C U V W F S` plus the `G` and `M` command words, either case. The
  same rule runs in the `EDIT` buffer, per line. Nothing is inserted inside a
  `(...)` comment or a `[...]` expression, the caret ends after the typed
  character, a replaced selection is replaced as a whole, and a block recalled
  from the buffer keeps its text until the operator actually types. The rule is
  unconditional: it has no config key.
- `Ctrl+G` opens the same go-to-line editor as AUTO and selects a line in the
  buffer instead of a line in the loaded program.
- `Ctrl+R` issues the selected buffer line directly, with no confirmation
  dialog.
- `Ctrl+Bksp` deletes the selected buffer line; while no such line is in the
  buffer yet it clears the input instead.
- `Ctrl+S` opens the save dialog: a file name field and an `add pre/postamble`
  checkbox. It writes the buffer to a new `.ngc` file under the INI's
  `PROGRAM_PREFIX`. Ticked, the box writes the `[mdi] save_preamble` lines
  above the buffer and the `[mdi] save_postamble` lines below it.
- `Esc` clears the uncommitted input only.

Rules:

- Issue requires machine ON, homed axes, idle interpreter and no active
  program. A syntax error issues nothing and marks the input.
- The command may temporarily select MDI and restores the previous task mode
  when the interpreter returns idle. No physical CYCLE START is involved.
- The buffer lives for the app session: leaving MDI keeps it, and returning to
  the pane shows the same blocks. Only `Esc` clears state, and it clears the
  uncommitted input alone; no command already issued is cancelled.
- MAIN's toolpath draws the block being edited as an overlay over the loaded
  program: its move starts at the machine's current position, in the pane's own
  colour (`theme::MDI_PREVIEW`), dashed for a rapid and solid for a cut, the way
  the program path already distinguishes them. An empty, over-long or unparsable
  block draws nothing extra — the strip carries the error and the pane would
  refuse the block. The overlay is parsed once per change of the input, the
  current position or the modal state, not once per frame.
- The buffer is a temporary HMI-local working list, bounded by
  `mdi.history_limit`, and not a machine log. It reaches disk only through
  `Ctrl+S`.
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
  `PROG.HOME` must **not** assert MANUAL: its keys run `[HALUI] MDI_COMMAND`
  blocks through `halui.mdi-command-NN`, which needs the MDI task mode, so the
  dial position requests `halui.mode.mdi` (ORed with the MDI key's request,
  because `halui.mode.mdi` takes one signal).
- HOME mode mapping, exactly as `baiermill_pb`:
  - `RAPID` -> `home-all`;
  - `X+`/`X-` -> `home-x`;
  - `Y+`/`Y-` -> `home-y`;
  - `Z+`/`Z-` -> `home-z`.
  - Individual home outputs are disabled while any axis is homing; the panel
    driver/postgui performs the gating.
- PROG.HOME mode mapping. The keys are not jogs: each is one MDI block, and
  the HMI issues it. The pins and blocks are one table
  (`linc_core::homing::proghome_keys`), which is also what the page documents:
  - `RAPID` -> `proghome-all` -> `o<whome> call`: machine `Z0` first, then work
    `X0 Y0`. A single `G0 X0Y0Z0` would plunge X and Y at whatever Z the tool
    is at;
  - `X+`/`X-` -> `proghome-x` -> `G0 X0`;
  - `Y+`/`Y-` -> `proghome-y` -> `G0 Y0`;
  - `Z+`/`Z-` -> `proghome-z` -> `G0 Z0`.
  - One block per press: the backend watches the four pins on the poll it
    already makes and acts on the rising edge only, so a held key does not
    repeat the move.
  - The block goes through the HMI's ordinary MDI write path, which switches to
    MDI and puts the panel's mode back (`issue_blocks`). It is refused, and
    logged, while the machine is off, unhomed, protected or running a program.
  - **The dial position must not request a task mode.** Asking for MDI here
    (one build did, because `halui.mdi-command` needs it) left the task in MDI
    and halui then ignored every jog and home request, which is the manual
    keys dead in all other modes.
  - This is program zero in the current WCS, not machine home and not `G53`.
  - X/Y/Z only; optional A/C axes are not moved by these panel actions,
    matching the current Probe Basic handlers.
  - The key is the trigger, not a cycle start: one block, no cycle-start gate.
    It still requires machine ON, all axes homed and the interpreter idle, or
    the press is refused and logged.
- Homing order comes from the INI; the current machine is Z first, then X/Y.
  `NO_FORCE_HOMING = 0` means programs/MDI are blocked until homed.
- The page shows machine position, limits, homed count and sequence completion.
  It provides no on-screen home, unhome, jog or override buttons.

### 6.11 Manual modes: JOG / INC / MPG

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
  - `MPG`: active axis, handwheel scale and step mode. The `[DISPLAY]
    INCREMENTS` row belongs to `INC`; MPG selects its step on the handwheel, so
    the row and its `1`..`9` keys are absent there. The RS-485 link does not
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
- `Ctrl+D`: Diagnostics (the editable Program editor owns `Ctrl+D` for duplicate line); `Ctrl+H`: G-code Help.
- `HELP` action: G-code Help; no dedicated function key.
- `LOAD` / `Ctrl+O`: Files, when idle and allowed.
- `OFFSET`: cycles Offsets → probe tabs → Offsets. `Enter` runs the selected
  routine; there is no separate Touch Probe action.
- MDI mode: the panel position opens the MAIN page with the MDI pane in place
  of the `PROGRAM` card. `Enter` issues and commits the block, `Up`/`Down`
  recall a buffer line into the input, `Ctrl+Up`/`Ctrl+Down` move the selected
  line in the buffer, `Ctrl+G` goes to a buffer line, `Ctrl+R` runs the selected
  line, `Ctrl+Bksp` deletes the selected line or clears the input, `Ctrl+S`
  saves the buffer, `Esc` clears the uncommitted input. No physical CYCLE START
  is required. `Ctrl+Up`/`Ctrl+Down` are `Ctrl` rather than `Shift` because
  the panel keyboard has no Shift key; a profile may rebind them, but a bare
  arrow is rejected so it cannot shadow recall.
- `ESC`: back/cancel; on Help, clear search or return from detail history.
- Arrows: focus/list/scroll.
- `/`: focus G-code Help search when Help is active.
- `PageUp`/`PageDown`: tabs/pages; a page in the tool table and file lists.
- `Home`/`End`: first/last item.
- `Enter`: select/apply/MDI execute; on Help, open the selected code; in Files,
  open a folder or load an `NC_FILES` file.
- `Backspace`: edit; on Help detail, go back through link history.
- `Ctrl+R`: refresh the Files listing.
- `MO(1)`: symbols/mouse.

No F1–F8 mapping. The bottom keycap highlight reflects the current page:
`FILE` active in Program, `Ctrl+D` reaching Diagnostics, `Ctrl+H` G-code Help,
etc.

### 8.2 Mode and command gating

- The physical panel mode switch is authoritative for panel functions. The GUI
  displays panel mode and LinuxCNC task mode; if they differ, show both only
  when useful.
- The MDI pane replaces the `PROGRAM` card on the MAIN page while the panel is
  in MDI mode; it is keyed to the panel mode, so returning to MAIN from another
  page while still in MDI shows it again. Selecting AUTO brings up MAIN from
  every page: leaving MDI restores the normal read-only program card there, and
  the run is started from the machine page. Probe and tool-change pages may still
  issue their specific commands through `issue_mdi`, which temporarily selects
  MDI and restores the prior mode.
- HOME/PROG.HOME open the Homing screen; all homing/program-zero motion
  originates in the panel's home/proghome pins and keys. The HMI never homes an
  axis: the home keys drive halui directly. The PROG.HOME keys are MDI blocks
  and the HMI issues them itself (§6.10), one per press, because the halui
  alternative needed a task mode the dial must not ask for.
- JOG/INC/MPG open the Manual screen with the large graph and coordinate
  readout. Jog, increment and handwheel motion remain panel/pendant input only.
  **Increment selection**: `INC` steps come from the machine INI's
  `[DISPLAY] INCREMENTS` list, so the HMI offers exactly the values LinuxCNC was
  configured with (the same list Axis and Probe Basic use). The selector lives
  in the HMI's `INC` context card and publishes `linc.jog-increment`, wired to
  `halui.axis.L.increment` **and** `halui.joint.N.increment` in the post-GUI
  HAL file. Both pin sets must carry the step: halui reads the axis pins for a
  teleop jog and the joint pins for a joint/FREE jog, and an unwired pin turns
  every INC key into a zero-length step. The list's leading `JOG`
  entry is continuous and is not offered as a step: continuous is the panel's
  `JOG` mode, not a zero step. Jog keys are routed by panel mode — continuous
  to `halui.axis.L.plus/minus` (level) and incremental to
  `halui.axis.L.increment-plus/minus` (edge triggered) in teleop, and to the
  matching `halui.joint.N` pins in joint/FREE, selected by
  `halui.mode.is-teleop` / `is-joint`.
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
  the JOG, INC, MPG and HOME requests are ORed into a single request: INC and
  MPG jog just like JOG and are refused in AUTO or MDI for the same reason.
- MDI, probe and tool-change actions use Probe Basic's `issue_mdi` semantics:
  after an explicit operator action and validation (machine ON, homed, idle, no
  active program), the HMI may temporarily select MDI, issue the specific
  command, and restore the previous task mode. This is an intentional exception
  to panel-exclusive machine control; it is not a general cycle-start action.
- Leaving MDI keeps the buffer for the app session; `Esc` clears the
  uncommitted input only, and an already issued command is not cancelled.
- **Probe trip.** LinuxCNC aborts the move and reports one of four operator
  errors when `motion.probe-input` rises during a non-probe move, a homing
  move, a joint jog or a coordinate jog. LiNC treats that report as a latched
  machine fault, not as a transient toast: the exact controller text goes to
  the Messages page and a persistent `PROBE TRIPPED` banner carries it on every
  page. While it is latched, MDI, probe, tool-change, jog-parameter and
  offset/tool writes fail closed and no command is sent, and a trip raised
  during a program or a move switches the machine off through
  `linc.machine-off` so the panel cannot jog into the probe either. The banner
  does not time out; it clears only when the operator
  performs the controller reset (`Esc`, which works from any page including
  DIAGNOSTICS, or the banner's `RESET (ESC)` button), which sends
  `EMC_TASK_ABORT` — the same action that clears LinuxCNC's motion error.
  Re-powering afterwards needs a rising edge on `halui.machine.on`: on the mill
  the cabinet machine-on switch is the only writer, so recovery is toggling
  that switch. The off half of that toggle does not end the LiNC session while
  the trip's `linc.machine-off` level stands; the session stays up with the
  latched fault so the operator can reset it and see the machine re-power. A
  switch-off after the reset ends the session as usual. There is no software
  E-stop and no jog lockout.
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
    pub offsets: WorkOffsetSet,          // G54-G59.3, G92, rotation, axis count
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
    pub probe_trip: Option<ProbeTripFault>, // latched; clears only on reset
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
yet: the panel service runs with `--no-ipc` under LinuxCNC and the native
backend reports the HAL link as unavailable for these counters, so Diagnostics
shows them as unavailable rather than zero. Wiring them over IPC or dedicated
HAL pins is still open.

## 10. Performance

- Status snapshot: 10–20 Hz.
- Current-line sync: 10–20 Hz.
- Sparklines/event charts: 1 Hz or on data change.
- 3D toolpath: render only when visible; 30 fps maximum.
- No continuous 60 fps repaint when idle.
- No blocking file/serial/NML I/O on the UI thread.
- HAL outputs: `hmi-alive` 2 Hz heartbeat, `status-display` on WCS change or
  5 Hz, `tool-change-confirm` edge-driven only, `machine-off` on the rising and
  falling edge of a latched probe trip.
- Cache text layout and visible-line tokens.
- Event log in a ring buffer; persist asynchronously, not on every event.

## 11. Safety

- The hardware E-stop chain is the only safety-rated stop.
- A software E-stop button is forbidden.
- LiNC may write only the declared `linc.*` HAL outputs
  (`hmi-alive`, `tool-change-confirm`, `status-display`, `jog-increment`,
  `jog-speed`, `machine-off`). It never writes arbitrary HAL pins or machine
  motion/spindle/coolant/cycle pins.
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
- A probe trip reported by LinuxCNC latches a persistent banner and blocks
  motion/MDI/probe/tool/offset commands until the operator's reset clears it.
  The banner has no timeout. The clear action is the controller's
  `EMC_TASK_ABORT`. A trip raised while the program is paused **and nothing is
  moving** leaves the machine on, so the operator can trip the probe by hand
  and see that the input is wired; the banner reports which of the two outcomes
  happened.
- The trip also switches the machine off. The panel jogs straight into `halui`,
  which accepts a jog only while the task state is ON, so the HMI cannot block
  a jog by refusing it — it switches the machine off instead. The response is
  the `linc.machine-off` level, ORed onto the single `halui.machine.off` pin:
  true for as long as the trip is latched, false once it is reset. halui acts on
  the rising edge, so the held level switches the machine off once and a re-power
  while the trip stands is not undone. The response is wired for the mill and the
  mill simulation; the lathe profile creates the pin but has no consumer yet, so
  a lathe probe trip is not switched off.

  **The machine is left on only when the trip is raised with the program paused
  and nothing moving.** Feed hold mid-block is a held queue and nothing was
  aborted that had to be stopped; that is the case the operator provokes on
  purpose — a finger on the probe, machine at rest, to prove the input is
  wired — so cutting the power there would punish the one test the machine
  needs. Every other combination switches it off: not paused, or paused and
  moving. The paused flag alone cannot carry this decision, because `control.c`
  reports a probe hit during a *jog* as `Probe tripped during non-probe move`
  when a paused program still holds the trajectory queue, and `task.paused`
  stays set while the operator switches to MANUAL. Excusing that jog would let
  it run on into the probe, which is the crash this response exists to stop.
  So motion decides — `motion.current_vel` or `motion.probing`, the same
  predicate as `task.motion_active` — and the pause is the tie-breaker for the
  still case only. The banner follows the same split: only the still-and-paused
  trip says `MACHINE LEFT ON`, and both wordings name `RESET (ESC)`.
  The decision comes from the state at the moment of the trip, which the HMI
  records on the fault when the message arrives: the controller aborts the
  program and stops the motion as part of the trip, so by the next poll both
  answers are gone. The evidence is therefore the poll *before* the message —
  one poll interval old, which is what the paused-and-moving rule has to be
  read with. No `motion.jog-inhibit`, no jog-stop and no other motion-based
  heuristic is added.
- Re-powering after a trip needs a rising edge on `halui.machine.on`. On the
  mill the only writer is the cabinet machine-on switch, so recovery is
  toggling that switch. While the trip's `linc.machine-off` level stands, the
  off half of that toggle is not a session shutdown: LiNC keeps running with
  the latched fault, and the operator resets it from the banner or Esc. A
  switch-off after the reset ends the session as usual. LiNC adds no jog
  lockout, no `motion.jog-inhibit` and no software E-stop.
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
- Selecting the panel MDI position opens the MAIN page with the `PROGRAM` card
  replaced by the MDI pane: block listing, syntax-highlighted input with default
  focus, the one-line result strip and the MODAL band AUTO shows, inside the same
  slot the program card fills. `Enter` issues and commits through `issue_mdi`,
  `Ctrl+R` re-runs the selected buffer line with no confirmation dialog,
  `Ctrl+Up`/`Ctrl+Down` reorder the buffer, a value word typed against a digit
  gets its space, the block being edited is drawn over MAIN's toolpath, and there
  is no quick-command button row.
- The Homing/Program Zero screen opens at HOME/PROG.HOME, shows per-axis/sequence
  state and documents the exact physical-panel key mapping without providing
  home controls.
- PROG.HOME retains the current machine's `issue_mdi` behavior: RAPID calls
  `o<whome>` (machine `Z0`, then work `X0 Y0`), axis keys send `G0 X0/Y0/Z0` in
  the current WCS, X/Y/Z only. The HMI issues them itself, one block per press,
  through the same MDI path as its other writes.
- The Tools screen represents the standard LinuxCNC tool table, current/next
  tool, explicit `Tn M6` issue, manual confirmation and optional ATC/turret
  state; the pocket column appears only when a changer uses it.
- INC and MPG show the `[DISPLAY] INCREMENTS` steps as one row of numbered
  chips, the selected one filled; `1`..`n` pick that step and the row highlights
  it. Continuous JOG shows no row and takes no digits, because it ignores the
  step.
- The Manual screen opens at JOG/INC/MPG, shows a large graph plus large work
  coordinates, machine coordinates and DTG, and provides no on-screen jog or
  mode control.
- Program protect ON locks program editing, saves, offset/tool writes, file
  imports and probe applies; unsaved changes are preserved, not discarded.
- The panel's status seven-segment display shows the active `G54`–`G59` offset
  as `54`–`59`, and `--` when unknown; it never fabricates a value. The
  extended systems have no two-digit name, so `G59.1`–`G59.3` also shows `--`.
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

0. The shell's page arithmetic and egui's real bottom panel disagree by 17 px:
   `BOTTOM_BAR_HEIGHT` puts the page bottom at `viewport.bottom() - 36 = 684` in a
   720 window, while `Panel::bottom(..).exact_size(36)` lands at 701..737. The
   MAIN page compensates with an explicit `+15 / -2` in `screens/machine.rs`;
   every other page and every card bottom still derive from the nominal 684.
   Reconciling them is a `page_bottom` change plus a re-tune of that page.

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
6. Native NML is the LinuxCNC backend; keep the `linuxcnc-nml` sibling crate and
   the LinuxCNC development files in the build and packaging checks.
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
22. Add turret/live-tool drive to the lathe simulator panel (the lathe face
    itself exists, selected by the profile, with those keys greyed) and its
    `svg/` mockup; `svg/sim-panel.svg` is the mill CNC control panel.
23. Port the Probe Basic parameter contract (`#3014`–`#3036`) and result contract
    (`#1001`–`#1009`), map every routine to its G-code call, and verify that
    `RUN PROBE` issues the call through `issue_mdi` only after the explicit
    action and state validation.
24. Decide lathe touch-probe/tool-setter support separately; the first Touch
    Probe page is mill-oriented (X/Y/Z and WCS).
25. Implement MDI issue through Probe Basic `issue_mdi`: syntax validation,
    buffer recall into the input, clearing the uncommitted input on `Esc`, and
    restoration of the prior task mode when the interpreter returns idle.
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
    `hmi-alive`, `tool-change-confirm`, `status-display`, `jog-increment`,
    `jog-speed`, `machine-off`. Verify ownership,
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
| `svg/probe.svg` | Touch probe — outside corners: keyboard-first Probe Basic 3×3 matrix (numpad 1–9 highlight, Enter run), passive offset-key category indicator, Probe Basic parameter/result block, work offsets and live probe LED |
| `svg/probe-inside-corners.svg` | Touch probe — inside corners: four inside corners, straight X/Y edges and Z centre in the same 3×3 matrix |
| `svg/probe-boss-pocket.svg` | Touch probe — boss and pocket: six round/rect boss and pocket routines plus the stacked diam/X/Y hint fields |
| `svg/probe-ridge-valley.svg` | Touch probe — ridge and valley: six ridge/valley routines plus the stacked X/Y hint fields |
| `svg/probe-edge-angle.svg` | Touch probe — edge angle: four corner and four top-edge angle routines with the WCO-rotation note |
| `svg/probe-calibrate.svg` | Touch probe — calibrate: calibration offset, 2×2 round/square boss/pocket routines, diameter/width fields and reset |
| `svg/probe-help.svg` | Touch probe — help: topic list, step-off-width explanation and dimensioned probe diagram |
| `svg/mdi.svg` | MDI pane: block listing, syntax-highlighted single-line input, command issue via `issue_mdi`, the one-line result/error strip and the MODAL band under it |
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
- `configs/sim.toml` — mill simulation profile pointing at `configs/sim.ini`,
  the in-repo simulation HAL (`hal/rio_sim.hal`, `hal/rio_sim.py`) and the
  simulated CNC control panel.
- `configs/baiermill.toml` — physical mill profile pointing at
  `../../baiermill_pb/probe_basic.ini`, with the real CNC control panel owned by
  `linc-panel`.
- `configs/lathe-sim.toml` — lathe simulation profile pointing at
  `configs/lathe-sim.ini`, which boots LiNC against Probe Basic's lathe HAL
  and subroutines in `../probe_basic_dev`; `gui = "lathe"` and
  turret/live-tool capabilities.
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

## 16. Diagnostic socket and MCP server

`linc` publishes a read-only diagnostic state for agents. It is state plus a
shared gate report, not a second control path: nothing here can move an axis,
start a program or write an offset.

### 16.1 Socket and discovery

- `linc` creates `hmi.sock` under `$LINC_DIAG_DIR`, else
  `$XDG_RUNTIME_DIR/linc/<instance>/`, else `/tmp/linc-<uid>/<instance>/`.
  `LINC_INSTANCE` names the instance and defaults to `default`.
- The directory is mode `0700`, the socket `0600`; same-user only, with no
  in-band authentication.
- `instance.json` records schema version, pid, role, source, socket path, start
  time, profile and build. A reader validates the pid before connecting and a
  socket file is only removed when nothing answers on it.
- `linc --no-diag` and `linc-panel --no-diag` start without their sockets.
- Each role writes `instance-<role>.json`: the two share one directory, and a
  single `instance.json` would be overwritten by whichever started last.

### 16.2 Protocol

Unix stream socket, JSON-RPC 2.0, one JSON object per line, 8 MB frame limit.
Every response is an envelope: `schema_version`, `source`, `process` (pid, role,
build), `sequence`, `captured_at_ms`, `age_ms`, `stale`, `dropped` and `data`.
`age_ms` is computed at response time from `captured_at_ms`; `stale` follows the
source's threshold, and a write decision fails closed on a stale envelope.

Methods implemented: `diag.hello`, `diag.ping`, `diag.snapshot`, `diag.summary`,
`diag.snapshot_at`, `diag.snapshot_diff`, `diag.timeline`, `diag.ui`,
`diag.screen_model`, `diag.why_blocked`, `diag.offsets`, `diag.tools`,
`diag.program`, `diag.logs`, `diag.events`, `diag.config`, `diag.metrics`,
`diag.motion_history`, `diag.nml`, `diag.hal`, `diag.hal_pins`,
`diag.hal_signals`, `diag.io`, `diag.crash_records`, `diag.export_bundle`,
`diag.screenshot`, `diag.ui_action`, `diag.subscribe`, `diag.unsubscribe`,
`diag.cancel`.
Unknown methods return `NOT_FOUND` and keep the connection open.

The bus retains 60 snapshots at 1 Hz (a minute of history) for
`diag.snapshot_at` and `diag.snapshot_diff`; the diff reports changed leaf paths
and ignores fields that move on every sample (`meta.age_ms`, `live_trail`,
`program.content`, ...), so a diff is a diagnosis rather than noise. It also
retains 1200 motion samples at 10 Hz (two minutes) for `diag.motion_history`;
the response is evenly downsampled to `max_samples` and eviction increments
`dropped.motion`. A subscribed connection receives `diag.notify` at most every
250 ms when the sequence moves.

`diag.screenshot` asks the UI thread for a capture and waits up to 3 s. The
capture goes through egui's screenshot callback, which runs after the frame is
painted — reading the window's frame buffer during layout returns the cleared
buffer. The PNG is written to the export directory and returned as base64.

`diag.export_bundle` writes thirteen JSON files (hello, summary, snapshot,
gates, offsets, tools, config, logs, events, panel, ui, motion, crashes) to a
fixed directory: `$LINC_EXPORT_DIR`, else `$XDG_STATE_HOME/linc/exports`, else
`~/.local/state/linc/exports`. Every string value is rewritten so the
operator's home path becomes `~`; nothing else is redacted.

The bus publishes on every backend sample and refreshes at 5 Hz, so `age_ms`
stays small while the window is idle: `egui` repaints on demand, and a publisher
without that refresh would look stale between frames.

### 16.3 Gates

`diag.why_blocked` is the single answer to "why is this blocked": panel link and
communication errors, unknown panel mode, e-stop, power, fault, protect, unhomed
axes, a busy interpreter, tool-table load errors, a missing program, a mode
mismatch and a missing WCS row. Each blocker carries a stable code, severity,
source and, where one exists, a remediation. The list is exhaustive rather than
first-failure, so an agent sees the whole picture.

### 16.4 `linc-mcp`

MCP over stdio, built without the UI, HAL, LinuxCNC or the panel driver. Tools
are prefixed `linc_` and read-only except where noted: `linc_ping`,
`linc_get_build_info`, `linc_get_environment`, `linc_health_check`,
`linc_get_state_summary`, `linc_get_snapshot`, `linc_get_snapshot_at`,
`linc_diff_snapshots`, `linc_get_timeline`, `linc_why_blocked`,
`linc_get_ui_state`, `linc_get_program`, `linc_get_offsets`, `linc_get_tools`,
`linc_get_config`, `linc_get_logs`, `linc_get_events`, `linc_get_metrics`,
`linc_get_motion_history`, `linc_get_screen_model`, `linc_get_nml`,
`linc_get_hal`, `linc_get_hal_pins`, `linc_get_hal_signals`, `linc_get_io`,
`linc_get_crash_records`, `linc_get_panel_state`, `linc_get_panel_frames`,
`linc_get_panel_events`, `linc_compare_panel_views`, `linc_test_wait_for`,
`linc_screenshot` (returns MCP image content), `linc_export_bundle` (writes only
under the export directory) and `linc_ui_action`. The UI action tool queues an
HMI intent — go to a page, cycle the view, show or hide the bounds, fit, clear
the trail, select a line — and the UI applies it before the frame draws, so an
agent can drive verification without synthetic X input. It changes the HMI's
view, never the machine. `linc_test_wait_for` polls one allowlisted path
(`panel.*`, `sim.*`, `machine.*`, `task.*`, `program.*`, `alarms.length`,
`ui.page`) with `eq`/`ne`/`lt`/`lte`/`gt`/`gte`/`contains`; there is no JSONPath,
regex or evaluation.
Resources mirror the same state by URI; prompts start the two diagnostic
sequences in `docs/MCP-SERVER-HANDOFF.md`.

Every call is audited (stderr plus `--audit <file>`), including a refused write.
The only write surface is a simulated panel, and it is closed unless the panel
service was started with `--allow-sim-control` *and* the server with
`--allow-sim-control --sim`; either alone answers `WRITE_DISABLED` or
`SIM_CONTROL_DISABLED`. There is no tool that moves a machine or touches a real
panel.

The panel service publishes its own socket (role `panel`) with `diag.panel`,
`diag.panel_frames`, `diag.panel_events` and `diag.metrics`: link state, mode,
overrides, buttons, protect, a 2048-frame raw ring with CRC verdicts and a ring
of state transitions. `linc-mcp` connects to it when it is running and reports
`PANEL_NOT_RUNNING` when it is not. A simulated panel with control armed also
answers `diag.sim_state`, `diag.sim_set`, `diag.sim_buttons`, `diag.sim_fault`,
`diag.sim_clear_faults` and `diag.sim_reset`. Faults are `disconnect`,
`timeout`, `crc_error`, `invalid_mode`, `protect_unknown` and `stale`; the first
four are link faults, the next two input faults, and `stale` freezes publication
so the envelope age grows.

Crash records are written by a panic hook in `linc`, `linc-panel` and
`linc-mcp` to `$XDG_STATE_HOME/linc/crash-<role>-<pid>.json` and read back by
`linc_get_crash_records` and the bundle. `linc_get_nml` reports the Python
backend's pid, liveness, request/reply/error counters, latency, stderr tail and
a fatal reason; the simulator answers `available: false`. `linc_get_hal` reports
the `linc.*` outputs and heartbeat; `linc_get_hal_pins` and
`linc_get_hal_signals` answer `available: false` with the reason that the build
does not link the full HAL C API, and never fabricate pins.
