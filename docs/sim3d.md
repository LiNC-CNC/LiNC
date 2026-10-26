# linc-sim3d: the 3D machine simulator

`linc-sim3d` stands in for the BaierMill when there is no machine to touch. It
models the machine body, the touch probe and the milling cutters, loads
workpieces from STEP files, and publishes probe contact to LinuxCNC HAL through
its own HAL component, also called `linc-sim3d`.

## What it is and what it is not

It **is**:

- a kinematic picture of the machine: body boxes sized from the INI's axis
  limits, a travel envelope, and the tool that hangs from the spindle;
- a touch-probe model: a ball with a stylus, whose contact is computed against
  the workpiece meshes and published on `linc-sim3d.probe-out`;
- a collision check for cutters: the swept cutter capsule against the parts and
  the machine envelope;
- a viewer: a perspective 3D window drawn with egui, with a side panel for the
  workpieces, the placement, the camera and the jog controls.

It is **not**:

- a material-removal simulator. Nothing is cut. The workpiece keeps its shape
  however deep a cutter goes into it; the cutter only reports a collision;
- a machine controller. It never commands motion, never touches the E-stop, the
  spindle or the programme, and never writes a HAL input pin of its own;
- a replacement for `configs/sim.ini`. The INI stays the single source of truth
  for travel, the tool table and its columns.

## Architecture

```
  LinuxCNC HAL                        linc-sim3d process
  ────────────                        ──────────────────
  joint.N.pos-fb ─┐
  tool-number ────┼──> pos-x/y/z, tool-number, tool-offset
                  │                                   │
                  │                       ┌───────────▼───────────┐
                  │                       │ PositionSource        │  HAL pins, or
                  │                       │  (Hal | UI jog)       │  the window's
                  │                       └───────────┬───────────┘  jog controls
                  │                                   │ Positions
                  │                       ┌───────────▼───────────┐
                  │                       │ engine::step_once     │
                  │                       │  tip = z + tool Z      │
                  │                       └───────────┬───────────┘
                  │                                   │
                  │                    ┌──────────────┴───────────────┐
                  │                    │ SimModel (RwLock)            │
                  │                    │  MachineModel | workpieces   │
                  │                    │  BVH per part, tool table    │
                  │                    └──────┬────────────────┬──────┘
                  │                           │ Snapshot       │ Actions
                  │                           ▼                ▲
                  │                    ┌──────────────┐  ┌─────┴──────┐
  probe-out ◄─────┼────────────────────┤ Sim3dSignal  │  │ egui panel │
  collision ◄─────┤                    │ HalSignal /  │  │ viewer     │
  tip-x/y/z ◄─────┘                    │ MockSignal   │  └────────────┘
                                       └──────────────┘
```

Short list, by crate module:

| Module | Owns |
|---|---|
| `config` | `configs/sim3d.toml`: the INI to stand in for, the probe, the cutters, the workpiece list, the HAL section |
| `machine` | the body boxes, travel and limit planes, built from the INI's axis limits |
| `tools` | probe and cutter geometry from the tool table, with the config overrides |
| `step` | STEP import through the OCCT helper or `freecadcmd`, with a disk cache |
| `bvh`, `mesh`, `types` | mesh queries: nearest point on a triangle soup for a sphere or a capsule |
| `model` | `SimModel` (machine + workpieces), `SharedState` (model, snapshot, action queue), the contact queries |
| `engine` | `step_once` (one tick), `run_engine` (the sampling loop), `run_hal` (the strict headless HAL mode) |
| `sink` | `Sim3dSignal`: the four facts the simulator publishes |
| `hal` | the `linc-sim3d` HAL component, its pin table, `HalSignal`, `HalPositionSource`, and `Rig` (publish and sample, with or without a window) |
| `viewer` | the perspective camera, the scene baked from the model, the egui mesh |
| `ui` | the side panel: status, position source, workpieces, placement, camera |
| `browser` | the in-app file picker (STEP and setup files) and the recent-setup store |

## HAL pins

The component is `linc-sim3d`; every pin takes exactly one signal.
`hal/linc-sim3d.hal` is the wiring, and `hal::PINS` is the pin table the
component publishes (a test compares the two).

| Pin | Direction | Type | Meaning |
|---|---|---|---|
| `linc-sim3d.pos-x` | in | float | joint X in machine mm |
| `linc-sim3d.pos-y` | in | float | joint Y in machine mm |
| `linc-sim3d.pos-z` | in | float | joint Z in machine mm |
| `linc-sim3d.tool-offset` | in | float | live tool offset on top of the tool table; normally 0 and `setp`-ed |
| `linc-sim3d.tool-number` | in | s32 | the tool in the spindle, from iocontrol |
| `linc-sim3d.probe-out` | out | bit | the modelled ball touches a visible workpiece |
| `linc-sim3d.collision` | out | bit | a cutter hit a part, or the machine envelope was left |
| `linc-sim3d.tip-x` | out | float | modelled tip X, machine mm |
| `linc-sim3d.tip-y` | out | float | modelled tip Y, machine mm |
| `linc-sim3d.tip-z` | out | float | modelled tip Z, machine mm |

`collision` and the three `tip-*` pins are diagnostics: read them in `halshow`.
Nothing downstream depends on them.

### The probe input is an OR, and it has one reader

`motion.probe-input` is a single pin, so the two ways a touch probe can fire are
ORed through `or2.probe_sim3d` (created once in `hal/linc.hal`, wired in
`hal/linc-sim3d.hal`):

```
net sig_rio_probe_bit  => or2.probe_sim3d.in0      # the panel's PROBE key
net sim3d-probe-out linc-sim3d.probe-out => or2.probe_sim3d.in1   # modelled contact
net probe-input or2.probe_sim3d.out => motion.probe-input
```

- The **panel PROBE key** path is `net sim-probe cnc_panel.probe => rio.sim-probe`
  in `hal/linc-sim-postgui.hal`; `scripts/rio_sim.py` turns it into
  `sig_rio_probe_bit`. It is a human pressing the key: exactly what a real touch
  probe does.
- The **modelled contact** path is `linc-sim3d.probe-out`, high while the ball
  is within `probe.trigger_slack_mm` of a visible workpiece.

`motion.probe-input` therefore has exactly one reader - the OR - and no other HAL
file may link it. On a real machine the machine HAL owns the probe input and
`hal/linc-sim3d.hal` is not loaded.

## Coordinates

Everything is machine millimetres - the joint position LinuxCNC reports, not the
programme's work coordinates - with X to the right, Y to the back and Z up, and
the origin at machine X0Y0Z0.

```
tip = [ pos-x, pos-y, pos-z + tool Z offset ]
```

The tool Z offset is the first of: the live `linc-sim3d.tool-offset` pin when it
is non-zero, the tool table row's `Z` word, the `[probe].tip_offset_mm` (probe)
or `[tools].fallback_tip_offset_mm` (cutter) from the config. The probe tip is
the **centre of the ball**, so a ball of radius 1.5 mm reports contact at
1.5 mm above the surface it touches, and `--selftest` checks exactly that
(radius-corrected).

The machine body is a set of boxes sized from the INI: the table spans the X/Y
travel, the top of the table sits 20 mm below the bottom of Z travel, and the
column, head, spindle and holder hang off the top of Z travel. Change the INI
limits and the model moves with them.

## Adding a STEP workpiece

```toml
[[workpieces]]
name = "vice jaw blank"                       # optional; the file stem otherwise
path = "../parts/vice.step"                   # relative to configs/sim3d.toml
translate_mm = [-170.0, -180.0, -258.0]       # machine mm
rotate_deg = [0.0, 0.0, 0.0]                  # X, then Y, then Z
visible = true                                # draw it and probe it
```

The import goes through an OCCT helper or `freecadcmd`; the mesh is cached on
disk, keyed by the file's path and modification time, so the second load is
fast. A file that will not import is reported as a fault and skipped: one bad
path never leaves the simulator without a machine.

In the window, the **Workpieces** panel does the same thing without editing the
file by hand: press **browse...** to pick a `.step`, `.stp` or `.stl` file out of
a directory listing, or type a path and press **Add** (a leading `~` is your home
directory, a relative path resolves against the picker's directory); select a
part to edit its placement, press *use current position as origin* to place it
under the probe, then **Save** to write the list back out: every part in the
model, in model order, with its `translate_mm`, `rotate_deg` and `visible`. A
part removed in the panel leaves the file with it. The file written is the setup
opened with **Open setup**, or the config this window was started on when none
is open; the panel prints the target under the button. Only the workpiece list
moves, so the target keeps its own machine, probe, tool, cutter and HAL
sections, and a part under the target's directory is stored relative to it.

The picker is drawn by the simulator rather than handed to a desktop portal, so
it behaves the same in a headless Xvfb run and on the desktop. It lists the
directories and the files the purpose accepts, keeps ten rows on show whatever
the listing size, has *up* and *home*, a `hidden` toggle, and a typed path for
everything else. Escape cancels it instead of closing the window.

### Opening a setup file

**Open setup...** loads another setup file's workpieces — its `[[workpieces]]`
entries, with their placement and visibility — into the running model. Only the
workpieces move: the machine body, the tool table and the HAL section were built
when the window started, and a HAL component keeps the pins it was started with,
so a setup saved from another profile contributes its parts and nothing else.
The panel says which file the workpiece list came from, and **Reload** re-reads
that file, not the config the window was started on.

Each picker reopens in the directory it last closed in, so a STEP folder is
typed once. The last eight setups opened or saved are listed under **Recent
setups** in the panel; clicking one loads it. The recent list and both
remembered directories share one store, The list lives in
`${XDG_STATE_HOME:-~/.local/state}/linc/sim3d-recent.json`, is pruned of files
that have since gone, and is written only when it changes. **Save** records the
file it wrote as recent.

## Running it

Window, no LinuxCNC needed:

```
./target/debug/linc-sim3d --config configs/sim3d.toml
```

`--config` defaults to `configs/sim3d.toml` resolved from the working directory.
The window opens on the machine from the three-quarter view, with the probe in
the spindle. Drag orbits, middle or right drag pans, the wheel zooms, and the
camera row has *fit*, *probe* (centre on the live tip), *iso*, *top* and *front*.
The moving column and milling head are hidden by default so neither hides the
tool; *machine: Z column* and *milling head* in the Camera section draw them
when the machine layout matters.

The *Position* heading always names the live source:

- **UI jog**, the default for a window started on its own: the window owns the
  position. Use the `-`/`+` buttons or the drag values for X, Y and Z, pick a
  step, and set the tool number to see a cutter instead of the probe (T99 is the
  probe in `configs/tool.tbl`).
- **HAL**, a `--hal` rig, which is the one LinuxCNC starts: the position comes
  from the `linc-sim3d` pins that `hal/linc-sim3d.hal` nets, the engine samples
  them, and the jog controls are disabled. Jog from LinuxCNC.

### The window inside a `--hal` rig

`--hal` is the mode `hal/linc-sim3d.hal` starts with `loadusr`, and on a host
with a display it opens the 3D window as well: **one process owns the HAL
component and the view**, so what you see is exactly what the machine is being
told. The window only reads the snapshot, and the sampling thread owns
`probe-out`. The component is published before the window is created, so
`loadusr -Wn` still returns as soon as the pins exist and a slow or missing
display never delays the HAL startup.

Whether that window appears is decided once, the first matching rule winning:

| Rule | Window |
|---|---|
| `--no-window` | never |
| `--window` | always, even without a display, and a window that cannot open is then an error |
| `LINC_SIM3D_WINDOW=0` / `false` / `no` / `off` | never |
| `LINC_SIM3D_WINDOW=1` / `true` / `yes` / `on` | like `--window` |
| `DISPLAY` or `WAYLAND_DISPLAY` set | yes |
| none of the above | never, sample headless |

`--window` and `--no-window` are mutually exclusive, both are rejected together
with `--selftest`, and `--no-window` needs `--hal`. Without `--hal` the window is
what the program does anyway, so `--window` there is redundant and
`--no-window` is refused rather than silently ignored.

Closing the window does **not** stop the simulator. Esc, or the window manager's
close button, closes the view; the rig logs

```
linc-sim3d: window closed; still sampling the linc-sim3d HAL component until SIGINT/SIGTERM
```

and the engine keeps sampling, so a probe move that is in progress never loses
its contact (`probe-out` stays high, `tip-*` keeps updating). SIGINT or SIGTERM
then stops the engine and exits with status 0. A headless rig - a container, or
a host with no X server - sets `LINC_SIM3D_WINDOW=0` and gets exactly the old
behaviour; `--hal --no-window` is the same thing from the command line. Because
`scripts/sim3d-hal.sh` passes the environment through, the setting can live in
whatever starts LinuxCNC.

As HAL startup under LinuxCNC (`scripts/start-sim.sh` runs `configs/sim.ini`,
whose `[HAL]` section loads `hal/linc-sim3d.hal`):

```
scripts/start-sim.sh --detach       # LinuxCNC + HAL + panel + HMI, and the 3D window
LINC_CONFIG=configs/sim.toml linc   # (the script does this for you)
```

`hal/linc-sim3d.hal` runs `scripts/sim3d-hal.sh`, which `exec`s
`target/release/linc-sim3d --hal --config configs/sim3d.toml` (or the debug
binary). With no HAL behind it, `--hal` exits non-zero with one line:

```
linc-sim3d: cannot run the HAL component (needs a running LinuxCNC HAL, liblinuxcnchal): ...
```

Selftest, no HAL and no window: a synthetic 20 x 20 x 10 mm box is placed at a
known spot, the probe sweeps straight down onto it, and each check prints a
PASS/FAIL line, including the measured trigger Z:

```
./target/debug/linc-sim3d --selftest
box  12 triangles, x -192.5..-172.5, y -180.0..-160.0, z -218.0..-208.0 mm
PASS  1 no contact at the start  (...)
measured trigger Z -206.500 mm
PASS  2 contact starts within one sample of the expected Z  (...)
PASS  3 contact clears on retract  (...)
PASS  4a a cutter at the contact position reports collision  (...)
PASS  4b the probe at the same position reports no collision  (...)
PASS  5 the machine envelope fires outside travel  (...)
```

`scripts/sim3d-hal-test.sh` is the same idea through real HAL: it runs under
`halrun`, drives the input pins with `setp`, sweeps X across a modelled
workpiece and checks `probe-out`, `collision` and `tip-x`. It needs a host
where rtapi can start, and skips with a message where `halrun` is missing.

## Performance

- The model's triangles are baked **once per model change**: normals oriented,
  flat shading (a fixed light direction dotted with the normal, plus ambient)
  written into vertex colours. A 23 k-triangle STEP part is ~70 k vertices.
- The screen mesh is rebuilt only when the camera, the viewport or the scene
  changes; a still camera costs one `Arc` clone per frame. It is sorted far to
  near and back-face culled by normal, and triangles crossing the near plane are
  dropped rather than clipped.
- The tool is a per-frame mesh of a few hundred triangles: a ball and a stylus
  for the probe, a cylinder, ball nose or bull nose for a cutter, red while it
  collides.
- The engine samples every `[hal].sample_ms` (2 ms by default) on its own
  thread; the window redraws at about 30 Hz and reads the last snapshot.
- Contact queries are BVH-accelerated per workpiece, so the cost follows the
  number of parts, not the triangle count of the largest one.

## Limitations

- No material removal, by design.
- Probe contact is only evaluated while the spindle holds the probe
  (`[probe].tool_number`); a cutter never fires the probe input, and the probe is
  never reported as a collision.
- The probe is a ball: no stylus bending, no trigger pre-travel beyond
  `trigger_slack_mm`, no repeatability, no calibration.
- A cutter collides with a capsule swept along +Z from the tip; it does not
  sweep its motion between samples, so a very fast move can pass through a thin
  wall between two ticks.
- Workpiece visibility lives in the config and the model applies it on load, so
  toggling it in the window writes the config and reloads the model.
- The window draws with egui's painter's algorithm, not a depth buffer: the
  table grid is drawn over the mesh on purpose, and every other annotation is
  drawn under it so parts occlude it.
- STEP import needs `freecadcmd` or the OCCT helper; without either, a STEP
  workpiece is skipped with a fault (and `--selftest` says so and fails).
- One sample interval for everything; there is no separation between probing and
  cutting rates.
