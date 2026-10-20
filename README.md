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
| Program — code-focused with mini graph | `svg/program-code.svg` |
| Diagnostics | `svg/diagnostics.svg` |

Only the accepted versions are included. The earlier vendor-style studies and
intermediate layout explorations have been removed.

The SVGs use theme variables (`--surface`, `--fg`, `--muted`, `--border`,
`--accent`, `--success`, `--warning`, `--error`, `--c1`…`--c6`) and adapt to the
viewer's theme.

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
- **G-code is token-highlighted** with a rich but restrained palette:
  G-codes, M-codes, axis letters, numbers, feed/speed, tools, parameters,
  comments, and line numbers each have a distinct treatment.
- **Diagnostics is the detailed, read-only view** for panel link, NML/HAL,
  motion health, I/O, and event logs. It does not expose machine controls.
- **Spindle load is explicit** and must come from real drive feedback, not
  spindle override or commanded speed.

## Repository layout

```text
cnc-hmi-design/
├── README.md
├── SPEC.md
└── svg/
    ├── machine-dashboard.svg
    ├── program-split.svg
    ├── program-code.svg
    └── diagnostics.svg
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
3. Implement the Rust `egui` shell and theme tokens.
4. Implement the Machine dashboard.
5. Implement the Program `SPLIT`/`CODE` views with the G-code highlighter.
6. Implement Offsets, Tools, Diagnostics, and Settings.
7. Integrate LinuxCNC via PyO3 first, with a `MachineBackend` boundary that can
   later be replaced by a C++ NML bridge.
8. Verify keyboard-only navigation, panel disconnect handling, program
   protection, MDI mode gating, and the physical E-stop path.
