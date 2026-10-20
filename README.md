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
| BaierMill simulation | `configs/sim.toml` |
| BaierMill physical machine | `configs/baiermill.toml` |

The machine profile points at the LinuxCNC INI, which remains authoritative for
travel, velocities, units, spindle/ATC data and the tool table. The HMI never
owns the panel serial port; it reads the `szgh_panel.*` HAL pins.

```sh
cnc-hmi --config configs/sim.toml
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
│   └── baiermill.toml
└── svg/
    ├── machine-dashboard.svg
    ├── program-split.svg
    ├── program-code.svg
    ├── diagnostics.svg
    ├── gcode-help.svg
    └── files.svg
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
6. Implement Offsets, Tools, Diagnostics, Settings, the G-code Help reference
   browser, and the Files/USB transfer page.
7. Integrate LinuxCNC via PyO3 first, with a `MachineBackend` boundary that can
   later be replaced by a C++ NML bridge.
8. Verify keyboard-only navigation, panel disconnect handling, program
   protection, MDI mode gating, and the physical E-stop path.
