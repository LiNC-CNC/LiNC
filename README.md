# LiNC

LiNC is a Rust `egui` HMI for LinuxCNC, designed for a machine with:

- a **SZGH-1080 physical control panel** connected over RS-485, driven by the
  Rust HAL component in `../szgh-panel-rust/`
- a **QMK Syntec-style CNC keyboard** defined in
  `../qmk_firmware/cnckeyboard/keyboards/cnckeyboard/keymaps/doubletap/keymap.c`
- **no on-screen machine control panel**
- FANUC / SYNTEC / Siemens-inspired information architecture, without copying
  any vendor's proprietary artwork

The product name is **LiNC**; the executable, Rust workspace crates and HAL
component use the lowercase identifier `linc`. This repository is design-only.
It contains the specification and the accepted SVG mockups; it does not yet
contain the Rust HMI implementation.

## Accepted references

| Purpose | File |
|---|---|
| Machine dashboard | `svg/machine-dashboard.svg` |
| Program — G-code and graph side by side | `svg/program-split.svg` |
| Program — editable code-focused editor with mini graph | `svg/program-code.svg` |
| Diagnostics | `svg/diagnostics.svg` |
| G-code Help | `svg/gcode-help.svg` |
| Files / USB transfer | `svg/files.svg` |
| Lathe dashboard | `svg/lathe-dashboard.svg` |
| Touch probe interface | `svg/probe.svg` |
| MDI screen | `svg/mdi.svg` |
| Manual JOG/INC/MPG | `svg/manual.svg` |
| Homing screen | `svg/homing.svg` |
| Tools / tool change | `svg/tools.svg` |
| Simulator panel (separate window) | `svg/sim-panel.svg` |

Only the accepted versions are included. The earlier vendor-style studies and
intermediate layout explorations have been removed.

The SVGs use theme variables (`--surface`, `--fg`, `--muted`, `--border`,
`--accent`, `--success`, `--warning`, `--error`, `--c1`…`--c6`) and adapt to the
viewer's theme.

## Configuration

The HMI is configured per machine, with explicit profile selection:

| Profile | File |
|---|---|
| Shared defaults | `configs/base.toml` |
| BaierMill mill simulation | `configs/sim.toml` |
| BaierMill physical mill | `configs/baiermill.toml` |
| Lathe simulation | `configs/lathe-sim.toml` |

The machine profile points at the LinuxCNC INI, which remains authoritative for
travel, velocities, units, spindle/ATC data and the tool table. The panel
driver code is part of LiNC and runs as the supervised `linc-panel` service; the
egui HMI never opens the serial port. `linc-panel` exposes `szgh_panel.*`, and
LiNC writes only its declared `linc.*` HAL outputs.

```sh
linc --config configs/sim.toml
linc --config configs/lathe-sim.toml
linc --config configs/baiermill.toml
```

See [`CONFIGURATION.md`](CONFIGURATION.md) for the merge rules, schema, safety
policy and launch integration.

## Design decisions locked in

- **Dark graphite + cool-blue optics**, with blue used for interaction,
  active state, toolpath, and live data. Green/amber/red are reserved for
  machine state and alarms.
- **The physical panel is the primary machine-control surface.** The GUI has no
  general mode buttons, cycle start/hold/stop, jog buttons, spindle/coolant
  controls, override sliders, or software E-stop. Specific MDI, probe and
  tool-change commands are explicit exceptions.
- **The panel driver is LiNC-owned and modular.** `linc-panel` runs as a
  supervised process, decodes steady one-hot modes and steady overrides, and
  hides panel technology behind a driver/capability trait. Only it owns RS-485.
- **The WHB04B-6 pendant is jog-only.** Its wheel/axis select work only in JOG
  mode; pendant mode, cycle, spindle, coolant, override, program and macro
  controls are ignored.
- **LiNC writes only declared HAL outputs.** `linc.hmi-alive`,
  `linc.tool-change-confirm` and `linc.status-display`; there is no generic
  arbitrary-pin write API.
- **The QMK keyboard is the only text/navigation device.** There are no
  F1–F8 softkeys. `MAIN`, `FILE`, `OFFSET`, `TOOL`, `WIN`, `ESC`, arrows,
  PageUp/PageDown, Home/End, and Enter drive the UI. Standard Ctrl editing
  shortcuts work; on-screen actions are focusable with arrows/Enter.
- **Program has two layouts**: `SPLIT` (G-code + graph) and `CODE`
  (G-code dominant + mini graph). `FILE` enters Program and cycles the layout.
- **Program `CODE` is also the G-code editor.** The physical panel's `EDIT`
  position opens it with focus, dirty/save state, undo/redo, find and a
  debounced live preview. Edits and saves are gated by idle state and program
  protect; running programs stay read-only.
- **Graph overlays program and machine bounds** and highlights the selected
  line. Edits update the graph only after the preview syntax check succeeds;
  invalid input keeps the last valid graph and never moves the machine.
- **Two machine variants share one shell.** Mill is 3-axis by default and
  expands to 4/5 axes from the INI; lathe is X/Z with optional C/Y, live
  tooling and power-turret stations. Variant-specific screens are Machine,
  Program graph, Offsets, Tools and Diagnostics; Files and Help are shared.
- **The simulator panel is a separate companion window**, not part of the HMI.
  It reproduces the SZGH-1080 controls and HAL pin surface; the real HMI never
  exposes those controls.
- **Touch Probe follows Probe Basic's category-first workflow**, reached from
  Offsets: choose a routine group, pick an illustrated operation, adjust
  WCS/clearance/feed/tip parameters, then `RUN PROBE`. It reuses the G38.2
  subroutine family, reads results from `#1001`–`#1009`, and issues the selected
  call immediately via Probe Basic `issue_mdi` after validation.
- **MDI is a mode-driven screen**: the panel's MDI position opens it, focuses
  the command line and shows DRO/modal/history. `Enter` issues one block via
  `issue_mdi`; leaving MDI discards unissued input but does not cancel an
  already-issued command.
- **JOG, INC and MPG open a shared read-only Manual screen.** A large live
  graph dominates, with large work coordinates plus machine coordinates and
  DTG. Jog/increment/handwheel input remains on the panel; the screen never
  offers jog or mode controls.
- **The toolpath graph has two distinct layers.** The loaded program path is
  drawn in program colours; the live toolpath is a separate green trail.
  `CLEAR TRAIL` clears only the live trail and re-seeds it at the current
  position.
- **The panel status 7-segment shows the active work offset** (`54`–`59`),
  because two digits fit exactly and the coordinate frame matters during
  setup. It shows `--` when unknown; spindle load or units are optional
  configured alternatives.
- **The panel's PROGRAM PROTECT switch is a hard editing lock.** While on, the
  HMI cannot edit/save G-code, write offsets/tools, import files or apply probe
  results. Protect fails closed when unknown or stale; a dirty buffer is
  preserved read-only rather than discarded.
- **Homing is panel-only, exactly like the current machine.** HOME mode shows
  per-axis/sequence state; `RAPID` homes all and the axis jog keys home their
  axis through the panel's `home-*` pins. PROG.HOME keeps the existing
  `issue_mdi` behavior: `RAPID` sends `G0 X0Y0Z0`, axis keys send `G0 X0/Y0/Z0`
  in the current WCS, X/Y/Z only. The HMI does not intercept these panel
  actions.
- **Tools is the quick-change surface.** Select a table row or type a target
  tool and `CHANGE TOOL`; the HMI issues `Tn M6` through Probe Basic
  `issue_mdi` without opening the MDI screen. Manual changes get an
  install/confirm prompt; ATC/turret support reuses the existing `M6` G-code
  and is optional. The tool table is the standard LinuxCNC format; its pocket
  column exists only when a changer uses it.
- **G-code is token-highlighted** with a rich but restrained palette:
  G-codes, M-codes, axis letters, numbers, feed/speed, tools, parameters,
  comments, and line numbers each have a distinct treatment.
- **Diagnostics is the detailed, read-only view** for panel link, NML/HAL,
  motion health, I/O, and event logs. It does not expose machine controls.
- **G-code Help is an offline reference page**, not a modal popup: searchable
  LinuxCNC index, per-code detail, parameter tables and figures. It remains
  available while a program is running and exposes no machine controls.
- **Files is a simple two-pane browser** opened from `LOAD`: removable USB on
  the left, `NC_FILES` on the right. USB drives auto-mount on insert through
  `udisks2`; copy and eject are explicit actions. Opening an `NC_FILES` file
  loads it and returns to Program in `SPLIT` view, fitted and not started. No
  shell commands, no auto-run, no machine controls.
- **Spindle load is explicit** and must come from real drive feedback, not
  spindle override or commanded speed.

## Repository layout

```text
LiNC/
├── README.md
├── CONFIGURATION.md
├── SPEC.md
├── configs/
│   ├── base.toml
│   ├── sim.toml
│   ├── lathe-sim.toml
│   └── baiermill.toml
└── svg/
    ├── machine-dashboard.svg
    ├── program-split.svg
    ├── program-code.svg
    ├── diagnostics.svg
    ├── gcode-help.svg
    ├── files.svg
    ├── lathe-dashboard.svg
    ├── probe.svg
    ├── mdi.svg
    ├── manual.svg
    ├── homing.svg
    ├── tools.svg
    └── sim-panel.svg
```

## Viewing the SVGs

Any browser or SVG viewer can open the files. For command-line rendering:

```sh
rsvg-convert -o /tmp/machine.png svg/machine-dashboard.svg
```

## Next implementation steps

1. Port and build `../szgh-panel-rust/` as the LiNC `linc-panel` crate with
   the HAL feature enabled, keeping the driver/capability abstraction.
2. Add panel diagnostics counters and raw bitmaps to `linc-panel`.
3. Implement the `linc` HAL output component and typed allowlist.
4. Implement the Rust `egui` shell, theme tokens, and the layered config loader.
5. Implement the Machine dashboard.
6. Implement the Program `SPLIT`/`CODE` views, G-code highlighter and editor.
7. Implement the mill and lathe variants, including dynamic 3–5 axis cards,
   lathe live-tool/power-turret state, Offsets, Tools, Diagnostics, Settings,
   the G-code Help reference browser, the Files/USB transfer page, the Touch
   Probe interface, the mode-driven MDI screen, the panel-only Homing screen,
   the JOG/INC/MPG Manual screen, the Tools/tool-change page, the live-trail
   clear action and the program-protect editing lock.
8. Wire `linc.status-display` to the panel WCS readout, then integrate LinuxCNC
   via PyO3 first, with a `MachineBackend` boundary that can later be replaced
   by a C++ NML bridge.
9. Verify keyboard-only navigation, panel disconnect handling, program
   protection, explicit MDI/probe/tool issue gating, pendant jog-only behavior,
   and the physical E-stop path.
