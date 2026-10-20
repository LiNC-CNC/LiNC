# CNC HMI Design

Design reference for a Rust `egui` HMI on top of LinuxCNC, for a machine with:

- a **SZGH-1080 physical control panel** connected over RS-485, driven by the
  Rust HAL component in `../szgh-panel-rust/`
- a **QMK Syntec-style CNC keyboard** defined in
  `../qmk_firmware/cnckeyboard/keyboards/cnckeyboard/keymaps/doubletap/keymap.c`
- **no on-screen machine control panel**
- FANUC / SYNTEC / Siemens-inspired information architecture, without copying
  any vendor's proprietary artwork

This repository is design-only. It contains the specification and the accepted
SVG mockups; it does not yet contain the Rust HMI implementation.

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
| Homing screen | `svg/homing.svg` |
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
travel, velocities, units, spindle/ATC data and the tool table. The HMI never
owns the panel serial port; it reads the `szgh_panel.*` HAL pins.

```sh
cnc-hmi --config configs/sim.toml
cnc-hmi --config configs/lathe-sim.toml
cnc-hmi --config configs/baiermill.toml
```

See [`CONFIGURATION.md`](CONFIGURATION.md) for the merge rules, schema, safety
policy and launch integration.

## Design decisions locked in

- **Dark graphite + cool-blue optics**, with blue used for interaction,
  active state, toolpath, and live data. Green/amber/red are reserved for
  machine state and alarms.
- **The physical panel is the only machine-control surface.** The GUI has no
  mode buttons, cycle start/hold/stop, jog buttons, spindle/coolant controls,
  override sliders, or software E-stop.
- **The QMK keyboard is the only text/navigation device.** There are no
  F1–F8 softkeys. `MAIN`, `FILE`, `OFFSET`, `TOOL`, `WIN`, `ESC`, arrows,
  PageUp/PageDown, Home/End, and Enter drive the UI. On-screen actions are
  focusable with arrows/Enter.
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
  WCS/clearance/feed/tip parameters, then `PREPARE MDI`. It reuses the G38.2
  subroutine family, reads results from `#1001`–`#1009`, and requires the
  physical CYCLE START key before any probing motion.
- **MDI is a mode-driven screen**: the panel's MDI position opens it, focuses
  the command line and shows DRO/modal/history. `Enter` stages one block; only
  physical CYCLE START runs it. Leaving MDI discards an unrun staged block.
- **Homing is panel-only, exactly like the current machine.** HOME mode shows
  per-axis/sequence state; `RAPID` homes all and the axis jog keys home their
  axis through the panel's `home-*` pins. PROG.HOME maps the same keys to
  `G0 X0Y0Z0` / axis zero. The HMI never supplies a home command or button.
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
cnc-hmi-design/
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
    ├── homing.svg
    └── sim-panel.svg
```

## Viewing the SVGs

Any browser or SVG viewer can open the files. For command-line rendering:

```sh
rsvg-convert -o /tmp/machine.png svg/machine-dashboard.svg
```

## Next implementation steps

1. Fix and build `../szgh-panel-rust/` with the HAL feature enabled.
2. Add the read-only panel diagnostics counters required by the Diagnostics
   screen.
3. Implement the Rust `egui` shell, theme tokens, and the layered config loader.
4. Implement the Machine dashboard.
5. Implement the Program `SPLIT`/`CODE` views, G-code highlighter and editor.
6. Implement the mill and lathe variants, including dynamic 3–5 axis cards,
   lathe live-tool/power-turret state, Offsets, Tools, Diagnostics, Settings,
   the G-code Help reference browser, the Files/USB transfer page, the Touch
   Probe interface, the mode-driven MDI screen, and the panel-only Homing
   screen.
7. Integrate LinuxCNC via PyO3 first, with a `MachineBackend` boundary that can
   later be replaced by a C++ NML bridge.
8. Verify keyboard-only navigation, panel disconnect handling, program
   protection, MDI mode gating, and the physical E-stop path.
