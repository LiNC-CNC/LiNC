# LiNC

LiNC is a Rust `egui` HMI for LinuxCNC, designed for a machine with:

- a **CNC control panel** connected over RS-485, driven by LiNC's
  `linc-panel` service
- a **QMK Syntec-style CNC keyboard** defined in
  `../qmk_firmware/cnckeyboard/keyboards/cnckeyboard/keymaps/doubletap/keymap.c`
- **no on-screen machine control panel**
- FANUC / SYNTEC / Siemens-inspired information architecture, without copying
  any vendor's proprietary artwork

The product name is **LiNC**; the executable, Rust workspace crates and HAL
component use the lowercase identifier `linc`. The specification and accepted
SVG mockups are in this directory; the Rust implementation is in `crates/`.

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

The SVGs are self-contained: they use the literal graphite theme colors and a
full dark canvas instead of unresolved CSS custom properties, so `eog`,
librsvg and browsers render the same image without a theme stylesheet.

## Build and run

```sh
cargo build --release
./scripts/start-sim.sh          # foreground; Ctrl+C stops the session
./scripts/start-sim.sh --detach # background
./scripts/stop-sim.sh

# ready to run: released E-stop, powered on, homed, demo part loaded, window
# floated at 1280x720 for Hyprland
./scripts/start-sim.sh --detach --release-estop --power-on --home --load --float
```

The native LinuxCNC backend links `liblinuxcnc`/`libnml` and the sibling
`../linuxcnc-nml` crate, so `cargo build` needs the LinuxCNC development files
and a C++ compiler. The simulator uses `SimBackend` at runtime but the binary
still links NML.

`start-sim.sh` launches a real LinuxCNC process with the in-repo simulation
HAL (`hal/rio_sim.hal`) and the LiNC HMI/panel pair. The cabinet still boots the
way a real one is found — E-stop latched, power off — unless you ask for more:
`--release-estop`, `--power-on`, `--home`, `--load [FILE]` and `--float` each
imply the step before, because the E-stop mushroom and POWER ON belong to the
panel and cannot be pressed behind its back. The panel releases the mushroom
itself; the controller's reset is queued on LinuxCNC's command channel after a
short settle, since the panel's HAL pulse at boot can arrive before the task is
ready to take it (which left the panel showing the mushroom out and the HMI
still showing E-STOP). If a step does not happen, `sim-ready` prints the HAL
pins that are holding it. The HMI enforces a minimum 1280×720 window,
scales proportionally above it, and preserves the SVG font ratios. `linc --check-config --config configs/baiermill.toml` validates
a profile, its LinuxCNC INI and safety policy without opening the display or
HAL.

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
egui HMI never opens the serial port. `linc-panel` exposes `cnc_panel.*`, and
LiNC writes only its declared `linc.*` HAL outputs.

```sh
./scripts/start-sim.sh                       # real LinuxCNC + sim HAL
linc --config configs/baiermill.toml         # physical machine / its display entry
```

See [`CONFIGURATION.md`](CONFIGURATION.md) for the merge rules, schema, safety
policy and launch integration.

`[ui] scale` is a multiplier on the 1280×720 baseline: `"auto"`/`"1.0"` is the
design size, and `"0.8"`, `"80%"` or `"1.25x"` force a factor in `0.5..3.0`.
The shell scales itself from the physical viewport, so desktop HiDPI scaling
does not shrink the text. Settings shows the configured value and offers a
temporary runtime override. The HMI enforces a 1280×720 minimum window size.
The standalone simulation panel uses `[simulation] panel_scale` independently.

## Architecture

The LinuxCNC backend runs behind the typed `MachineBackend` boundary. The
current HMI is an in-process `NmlBackend`: `linuxcnc-nml` owns `emcStatus`,
`emcCommand` and `emcError`, and the read-only `HalReader` reads panel and
spindle pins. There is no helper process, JSON protocol or Python runtime on the
control path. See [`docs/architecture.md`](docs/architecture.md) for the
implemented path, the removed Python helper, and the alternatives that were
rejected. For a visual walkthrough, open the self-contained
[HTML architecture guide](docs/architecture.html).

The native NML client links `liblinuxcnc`/`libnml`, so a build needs the
LinuxCNC development files and the sibling `../linuxcnc-nml` checkout. The
panel service remains a separate process and the only owner of
`/dev/ttyUSB0`.

## HAL integration

The HAL wiring is included:

| File | Purpose |
|---|---|
| `hal/linc.hal` | Core pre-GUI wiring: panel service, modes, cycle, jog, spindle, coolant, homing, overrides and panel LEDs |
| `hal/linc-postgui.hal` | Post-GUI wiring for `linc.status-display`, `linc.tool-change-confirm` and `linc.hmi-alive` |
| `hal/linc-pendant.hal` | Optional real-machine WHB04B-6 jog-only wiring |

Use:

```ini
[HAL]
HALFILE = /home/jack/CNC/LiNC/hal/linc.hal
HALFILE = /home/jack/CNC/LiNC/hal/linc-pendant.hal   ; real machine only
POSTGUI_HALFILE = /home/jack/CNC/LiNC/hal/linc-postgui.hal
```

`LINC_CONFIG` must name the same profile used by the `linc` display:

```sh
export LINC_CONFIG=/home/jack/CNC/LiNC/configs/baiermill.toml
linc --config "$LINC_CONFIG"
```

The core HAL expects the generic `cnc_panel` component and pins. The live
machine HAL must rename or alias the legacy panel component before using this
file unchanged.

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
  It reproduces the panel controls and HAL pin surface; the real HMI never
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
├── Cargo.toml
├── README.md
├── CONFIGURATION.md
├── SPEC.md
├── configs/
│   ├── base.toml
│   ├── sim.toml
│   ├── lathe-sim.toml
│   └── baiermill.toml
├── crates/
│   ├── linc-app/         # CLI, backend selection, window and event loop
│   ├── linc-core/        # config, INI, model, commands, HAL, sim, IPC
│   ├── linc-hal/         # typed linc.* HAL output component
│   ├── linc-linuxcnc/    # native LinuxCNC NML backend
│   ├── linc-panel/       # RS-485 panel service and simulator panel
│   ├── linc-ui/          # egui shell, pages, keyboard model
│   └── linc-view/        # toolpath and lathe graph rendering
├── hal/
│   ├── linc.hal
│   ├── linc-postgui.hal
│   └── linc-pendant.hal
├── scripts/
│   ├── start-sim.sh
│   ├── start-panel-sim.sh
│   ├── sim-ready.py       # start-up steps the launcher waits for
│   └── stop-sim.sh
├── tools/
│   └── generate_help.py
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

## Implementation status

The workspace implements the specification in simulation. The shipped mill
profile runs end to end; the lathe UI is implemented, but `lathe-sim.toml`
intentionally refuses to load until its INI/tool table has the standard `P`
column required by a power turret (see `CONFIGURATION.md`).

- `linc` loads and validates a machine profile, renders the fixed shell, and
  switches pages from the panel mode and keyboard.
- The mill and lathe Machine, Program `SPLIT`/`CODE`, Offsets, Tools,
  Diagnostics, Settings, G-code Help, Files, Touch Probe, MDI, Manual and
  Homing screens are implemented.
- The Program editor has syntax highlighting, undo/redo/find, dirty/save
  state, a debounced live preview, program-protect locking and the live trail.
- `linc-panel` owns the panel driver, HAL pins and the standalone simulator
  panel. Simulation HMI and panel exchange state over two atomic JSON files so
  panel modes, power, E-stop, overrides and protect drive the HMI, while the
  HMI drives the panel status and tool displays.
- The typed `linc.*` HAL component, the native LinuxCNC backend, the WHB04B-6 pendant
  wiring and the generated G-code help index are present.
- `linc-mcp` exposes the HMI and panel diagnostic sockets to an agent: state,
  gates, logs, events, panel frames and transitions, motion history, crash
  records, a screen model, a diagnostic bundle, and an opt-in simulator control
  surface (`--allow-sim-control` on both processes). It never opens the panel
  serial port, and no tool can reach a real panel.

`cargo test --workspace` covers config merging, INI parsing, command
validation, editor/preview behavior, graph bounds, panel protocol, HAL output
allowlisting and the simulator backend.

## Remaining hardware verification

The software paths are implemented; the items that still need a real machine
or commissioned LinuxCNC session are listed in
[`CONFIGURATION.md`](CONFIGURATION.md#12-remaining-hardware-verification).
