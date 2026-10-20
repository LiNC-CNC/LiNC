# CNC HMI Specification

Status: design reference  
Scope: Rust `egui` HMI for LinuxCNC with SZGH-1080 physical panel and QMK CNC keyboard  
Canonical visual: `svg/machine-dashboard.svg`

---

## 1. Goals

Build a focused, operator-grade CNC HMI that:

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
  small `cnc_hmi` HAL component.

Special cases:

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
QMK keyboard --USB--> cnchmi (Rust egui) --NML or PyO3--> LinuxCNC
LinuxCNC status --------------------------------> cnchmi
szgh_panel.* HAL pins ---------------------------> cnchmi (read-only)
```

### 4.1 Processes

- `szgh-panel-hal`: owns the serial port, produces button/mode/override pins,
  consumes LED/7-segment/velocity pins. Separate process from the HMI.
- `cnchmi`: Rust `egui` application. Owns no machine-control hardware.
- `linuxcnc`: task/motion/IO. Authoritative machine state.
- HAL glue: standard `halui` plus a small `cnc_hmi` component for
  app-specific inputs/snapshots.

### 4.2 Rust workspace

```text
cnc-hmi/
  crates/
    cnc-core/        # state, commands, backend trait, validation
    cnc-linuxcnc/    # PyO3 official module; optional C++ NML bridge
    cnc-hal/         # panel-state snapshot, HAL FFI, cnc_hmi pins
    cnc-ui/          # egui shell, theme, screens, widgets
    cnc-view/        # 2D/3D toolpath and sparklines
    cnc-app/         # main, config, logging
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

## 5. Application shell

Fixed shell, no right-hand status column:

- **Top status bar**: machine name, physical mode, program, tool, spindle,
  feed, work offset, panel link, alarm/message counts, time.
- **Left navigation**: Machine, Program, Offsets, Tools, Diagnostics.
  Keyboard-focusable; `MAIN`/`FILE`/`OFFSET`/`TOOL`/`WIN` jump directly.
- **Central page**: active screen.
- **Bottom action bar**: real keycaps (`MAIN`, `FILE`, `OFFSET`, `TOOL`, `WIN`,
  `ESC`), context actions, and a compact read-only machine status line
  (`OVR F/S/R`, program protect, E-stop, alarm count).
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

Default page.

- Four axis cards: X, Y, Z, A; actual position, distance-to-go, load bar.
- Large 3D toolpath: stock, cut path, rapid path, tool marker, axes, zoom.
- Program card: current block, next block, modal G-codes, progress, elapsed.
- Feed and spindle-load gauges.
- Machine health: servo load per axis, coolant/lube state.
- Alarm/message summary.
- Compact panel state in top/bottom bars only.

### 6.2 Program

Two layouts, selected by the header toggle or `FILE`:

#### SPLIT

- G-code editor/monitor left (~58%).
- 3D toolpath right (~42%).
- Current line synchronized with the graph.
- Current/next block and modal summary below the editor.

#### CODE

- G-code editor dominant (~80%).
- Mini XY graph top-right.
- Program info card below the mini graph: current, next, modal, progress.
- Current line synchronized with the mini graph.

Program rules:

- G-code is token-highlighted (see §7.3).
- Current line uses an accent background and left bar; token colours remain.
- While running, the editor is read-only and auto-scrolls to the current line.
- Edits are allowed only when stopped and `program_protect` is off.
- `FILE` enters Program; pressing `FILE` again cycles `SPLIT`/`CODE`.
- The header segmented control is also focusable.
- No on-screen cycle start; loading a program does not start it.

### 6.3 Offsets

- Work offsets G54–G59.
- G92 offset.
- Tool offsets.
- Current axis values and distance-to-go.
- Read-only while running; blocked when program protect is active.
- Edits validated against LinuxCNC limits and units.

### 6.4 Tools

- Tool table: number, description, diameter, length, pocket.
- Current tool and next tool.
- ATC state when available.
- Read-only while running; program protect blocks writes.

### 6.5 Diagnostics

Read-only detail view. Tabs:

| Tab | Content |
|---|---|
| Overview | Health cards, active alarms/messages, panel summary, motion health, event log |
| Panel Link | RS-485 transport, cycle time, last RX, RX/TX counts, CRC errors, timeouts, reconnects, mode/overrides, raw button/LED bitmaps, 7-segment values |
| HAL / NML | NML command/status/error channel state, ages, echo serial, HAL components/pins, thread health |
| Motion | Following error, commanded vs actual, servo load, spindle load/speed, limits/homing |
| I/O | Panel LEDs, machine inputs, coolant/lube/door/ATC states, HAL pin browser |
| Logs | Searchable event history, severity/source filters, export/copy |

No fault reset, acknowledge, cycle, jog, override, or E-stop controls here.
`CLEAR VIEW` clears only the UI log view, never machine faults.

### 6.6 Settings

- Theme/units/display density.
- Keyboard reference.
- Machine profile/read-only INI summary.
- Panel link information.
- About/support.

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
- `ESC`: back/cancel.
- Arrows: focus/list/scroll.
- `PageUp`/`PageDown`: tabs/pages.
- `Home`/`End`: first/last item.
- `Enter`: select/apply/MDI execute.
- `Backspace`: edit.
- `MO(1)`: symbols/mouse.

No F1–F8 mapping. The bottom keycap highlight reflects the current page:
`FILE` active in Program, `WIN` active in Diagnostics, etc.

### 8.2 Mode and command gating

- The physical panel mode switch is authoritative.
- The GUI displays panel mode and LinuxCNC task mode; if they differ, show both
  only when useful.
- MDI is enabled only when the panel is in MDI mode.
- Program start is physical CYCLE START only.
- Overrides are physical switches; the GUI displays values.
- Jog is physical only.
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
- Respect program protect and running state.
- Do not hide alarms behind tabs; persistent banner plus Diagnostics.
- HMI crash must not stop the machine or remove panel control.
- Panel disconnect must be visible and actionable by the operator.
- Guard `Ctrl+Q`; never quit during motion or a running program.

## 12. Acceptance criteria

For this design repository:

- `SPEC.md` and `README.md` exist.
- Only the accepted source SVGs are committed.
- Canonical Machine, Program SPLIT, Program CODE, and Diagnostics screens are
  represented.
- Hardware constraints for the QMK keyboard and SZGH panel are documented.
- No SVG shows an on-screen machine control panel.
- G-code mockups show token-level syntax highlighting.
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
11. Implement the layered config loader and `cnc-hmi --check-config`;
    `configs/*.toml` currently define the contract only.
12. Unify the real panel driver's serial port and cycle time with
    `configs/baiermill.toml`; today they are hard-coded in the driver and
    `hallib/szgh_panel.hal`.

## 14. Accepted SVG manifest

Only the accepted versions are included.

| File | Description |
|---|---|
| `svg/machine-dashboard.svg` | Machine dashboard: dark graphite + blue, axis cards, 3D toolpath, program card, feed/spindle-load gauges, machine health, alarms |
| `svg/program-split.svg` | Program `SPLIT`: syntax-highlighted G-code alongside the 3D toolpath |
| `svg/program-code.svg` | Program `CODE`: syntax-highlighted G-code dominant, with mini XY graph and program-info card |
| `svg/diagnostics.svg` | Diagnostics overview: health cards, alarms/messages, panel link detail, motion health, event log |

Superseded vendor-style studies, intermediate layout explorations, and
colour-mapped previews were intentionally removed.

## 15. Configuration

Configuration is per machine and layered:

- `configs/base.toml` — UI, keymap, safety policy, HAL pin map, backend and
  preference defaults.
- `configs/sim.toml` — simulation profile pointing at
  `../../baiermill_sim/probe_basic.ini`, the simulated SZGH panel and
  `rio_sim.py`.
- `configs/baiermill.toml` — physical-machine profile pointing at
  `../../baiermill_pb/probe_basic.ini`, with the real SZGH-1080 panel owned by
  `szgh-panel-hal`.

Rules:

1. A profile is selected explicitly with `--config`; there is no default.
2. Profiles `extends` `base.toml`; profile tables override base, arrays replace.
3. The LinuxCNC INI remains authoritative for kinematics, travel, limits,
   velocities, units, spindle count, ATC settings, tool table and program paths.
4. The HMI never owns the panel serial port and never writes machine-control
   HAL pins.
5. Safety flags are immutable from profiles: panel-only machine control, no
   software E-stop/cycle/jog/spindle/coolant/overrides.
6. Runtime UI state is stored separately under
   `~/.config/cnc-hmi/<profile>.preferences.toml`.

`cnc-hmi --check-config` must validate the merge and path resolution without
connecting to LinuxCNC or HAL. The full schema, launch snippets and validation
contract are in `CONFIGURATION.md`.
