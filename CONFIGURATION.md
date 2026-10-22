# LiNC Configuration

Status: implemented contract for LiNC. The `linc` binary loads and validates
these profiles. `linc --check-config` performs the merge, path resolution and
safety validation without connecting to LinuxCNC or HAL.

## 1. Rules

1. **One profile per machine.** The operator selects a profile explicitly. The
   HMI must refuse to start if no profile is supplied.
2. **Shared defaults live in `configs/base.toml`.** Machine profiles extend it
   and override only what is machine-specific.
3. **The LinuxCNC INI is authoritative for machine data.** Travel limits,
   velocities, units, joint count, spindle count, ATC settings, the tool table
   and the program prefix come from the INI. They are not duplicated in TOML.
4. **Panel I/O is split from the UI.** The LiNC-owned `linc-panel` service owns
   the serial port and `cnc_panel.*` pins. The egui HMI never opens the serial
   device. LiNC may write only its declared `linc.*` HAL outputs
   (`hmi-alive`, `tool-change-confirm`, `status-display`).
5. **Safety policy is not a preference.** A profile cannot enable software
   E-stop, cycle control, jog, spindle, coolant or overrides; MDI issue is an
   explicit operator action, not a cycle-start substitute; pendant is jog-only.
6. **User preferences are separate.** Window geometry, last page, view angle
   and recent files live under `~/.config/linc/`, not in the machine profile.
7. **Unknown keys are errors.** This catches typos and prevents a machine
   profile from silently not applying a setting.
8. **Standard tool table only.** The tool table is the LinuxCNC file at
   `[EMCIO] TOOL_TABLE`; columns come from `TOOL_TABLE_COLUMNS`. `P` is required
   only when `tools.changer` uses ATC/turret and must be absent for manual.

## 2. Profiles

| File | Purpose |
|---|---|
| `configs/base.toml` | Shared UI, keymap, safety policy, HAL output policy, panel driver contract, backend and preferences defaults. Not runnable alone. |
| `configs/sim.toml` | Mill simulation: `configs/sim.ini` runs a real `linuxcnc` process with the in-repo simulation HAL (`hal/rio_sim.hal`), LiNC display and simulated CNC control panel. |
| `configs/baiermill.toml` | Physical mill: `../../baiermill_pb/probe_basic.ini`, real CNC control panel over RS-485 owned by `linc-panel`. |
| `configs/lathe-sim.toml` | Lathe simulation: `../../probe_basic_dev/configs/probe_basic_lathe/probe_basic_lathe.ini`, lathe UI capabilities. |

The profiles use:

```toml
schema_version = 1
profile = "sim"          # or "baiermill"
extends = "base.toml"
```

`extends` is resolved by the HMI loader, not by a TOML library. Paths inside a
profile are resolved relative to that profile file; `~` is expanded.

## 3. Selecting a profile

Intended command line:

```sh
./scripts/start-sim.sh
linc --config configs/baiermill.toml
```

`--config` or the `LINC_CONFIG` environment variable is required. There is
no default profile: starting with the real machine selected by accident is
worse than printing a usage error.

`--check-config` must parse, merge, resolve paths and validate the profile
without connecting to LinuxCNC or HAL.

### LinuxCNC launch

LinuxCNC passes `-ini <file>` to the display program and supports arguments in
`[DISPLAY] DISPLAY`. The HMI must let `-ini` override `machine.linuxcnc_ini`.

Real machine:

```ini
[DISPLAY]
DISPLAY = linc --config /home/jack/CNC/LiNC/configs/baiermill.toml
```

Simulation:

```ini
[DISPLAY]
DISPLAY = ../scripts/linc-display.sh
```

`scripts/start-sim.sh` launches `linuxcnc configs/sim.ini`; the INI loads
`hal/rio_sim.hal` (the software FPGA, driven by `hal/rio_sim.py`), then
`hal/linc.hal` and `hal/linc-sim.hal`. `linc-display.sh` starts the HMI with
the profile and loads `hal/linc-sim-postgui.hal` after the `linc.*` pins exist.
Every path in the INI is relative to `configs/`, so the simulation needs
nothing outside this checkout.

### HAL wiring

The LiNC HAL files are loaded around the GUI:

```ini
[HAL]
HALFILE = /home/jack/CNC/LiNC/hal/linc.hal
HALFILE = /home/jack/CNC/LiNC/hal/linc-pendant.hal   ; real machine only
POSTGUI_HALFILE = /home/jack/CNC/LiNC/hal/linc-postgui.hal

[DISPLAY]
DISPLAY = linc --config /home/jack/CNC/LiNC/configs/baiermill.toml
```

`linc.hal` loads the `linc-panel` service and wires `cnc_panel.*` to
`halui`, jog, spindle, coolant, homing and overrides. `linc-postgui.hal` runs
after the GUI has created the `linc.*` component and wires
`linc.status-display`, `linc.tool-change-confirm` and `linc.hmi-alive`.
`linc-pendant.hal` is real-machine only and wires jog/handwheel inputs.

`LINC_CONFIG` must select the same profile as `--config`:

```sh
export LINC_CONFIG=/home/jack/CNC/LiNC/configs/baiermill.toml
```

The core HAL expects the generic `cnc_panel` component. Rename or alias the
legacy branded panel HAL component before loading it unchanged.

## 4. Merge and validation contract

Loading order:

1. Read the machine profile.
2. Resolve `extends` relative to the profile and read `base.toml`.
3. Deep-merge tables: profile values win; nested tables merge; arrays are
   replaced, not concatenated.
4. Resolve path-valued fields relative to the profile file.
5. Apply CLI overrides, currently only `-ini`.
6. Validate required keys, enums, safety policy and path existence.
7. Reject unknown keys.

Required profile fields:

- `schema_version = 1`
- `profile`
- `machine.name`
- `machine.kind` = `"sim"` or `"real"`
- `machine.gui` = `"mill"` or `"lathe"`
- `machine.linuxcnc_ini`
- `[hal] panel_component`, `output_prefix`, `allowed_outputs`
- `[panel] kind`, `driver`, `owner`, `mode_encoding`, `override_encoding`
- `[panel.link] read_only = true`
- `[safety] machine_control = "panel-with-explicit-mdi"` and all
  `software_* = false`

`base.toml` has no `[machine]` table, so it cannot be used as a profile.

## 5. Source-of-truth map

| Data | Source |
|---|---|
| Kinematics, joint count | INI `[KINS]` |
| Axis names / coordinate order | INI `[TRAJ] COORDINATES` |
| Travel limits | INI `[AXIS_*]` / `[JOINT_*]` |
| Velocities and acceleration | INI `[AXIS_*]` / `[JOINT_*]` |
| Units | INI `[TRAJ] LINEAR_UNITS`, `ANGULAR_UNITS` |
| Spindle count / maximum speed | INI `[TRAJ] SPINDLES`, `[DISPLAY] MAX_SPINDLE_SPEED` |
| Tool table | Standard LinuxCNC file at INI `[EMCIO] TOOL_TABLE`; columns from `TOOL_TABLE_COLUMNS` |
| Tool pocket/station | Standard `P` column; required only for ATC/turret |
| ATC pockets / tool-change heights | INI `[ATC]`; `[tools] atc_pockets = "ini"` |
| Tool-change motion | Existing `M6` remap / `toolchange.ngc`; HMI issues `Tn M6` via `issue_mdi` |
| MDI/probe/tool execution | Probe Basic `issue_mdi` semantics; no staging/physical cycle start |
| Program prefix / open file | INI `[DISPLAY] PROGRAM_PREFIX`, `OPEN_FILE` |
| `NC_FILES` root | INI `PROGRAM_PREFIX`; `[files] nc_files = "ini"` |
| Machine graph bounds | INI travel limits / kinematics |
| Program graph bounds | Last valid preview parse; never from a syntax-error state |
| USB mount points | `udisks2` D-Bus, not config |
| Probe subroutines | INI `SUBROUTINE_PATH`; `[probe] subroutines = "ini"` |
| Probe parameters/results | LinuxCNC parameters `#3014`–`#3036`, results `#1001`–`#1009` |
| MDI command history | HMI-local ring buffer; not a machine log |
| Homing sequence | INI `HOME_SEQUENCE`, search/latch/index settings |
| Homing commands | `cnc_panel.home-*` / `proghome-*` pins and physical keys; never HMI |
| PROG.HOME path | Panel `proghome-*` -> existing Probe Basic `issue_mdi`; HMI observes |
| Panel/RIO HAL pin names | `configs/base.toml` |
| Core HAL wiring | `hal/linc.hal` |
| Post-GUI HMI outputs | `hal/linc-postgui.hal` |
| Optional pendant wiring | `hal/linc-pendant.hal` |
| WHB pendant | Jog/handwheel inputs only in JOG mode; all other functions ignored |
| Panel status 7-segment | `linc.status-display` -> `cnc_panel.sseg_status`; `[panel.status]` |
| Machine identity and panel ownership | machine profile |
| UI layout, theme, keymap, toolpath options | `base.toml` + overrides |
| G-code help content | bundled `linuxcnc-gcode` reference, generated from linuxcnc.org |
| Window geometry, last page, recent files | user preferences file |

### BaierMill values (reference only)

These are read from the INI at runtime; they are listed here only to make the
configuration reviewable.

| Item | Value |
|---|---|
| Machine | `BaierMill` |
| Kinematics | `trivkins coordinates=XYZ`, 3 joints |
| Units | millimetres / degrees |
| X travel | -372 … +5 mm |
| Y travel | -346 … +4 mm |
| Z travel | -253 … +19 mm |
| Max linear velocity | 80 mm/s |
| Max acceleration | 500 mm/s² |
| Default linear velocity | 60 mm/s |
| Spindle | 1, 0…12000 rpm, 0.5…2.0 override |
| ATC | 12 pockets, Z tool-change height -194 mm (manual changer today; add P only if ATC is enabled) |
| Tool table | `tool.tbl`, standard `TZDR` (no P in manual mode) |
| Real config | `../../baiermill_pb/probe_basic.ini` |
| Sim config | `configs/sim.ini` (loads the in-repo `hal/rio_sim.hal`) |

The simulation INI intentionally carries the same travel, tuning, homing order
and ATC values as the machine; only the HAL stack and paths differ.

## 6. Simulation profile

`configs/sim.toml`:

- `machine.kind = "sim"` — the HMI shows a persistent `SIM` badge.
- `linuxcnc_ini = "sim.ini"`, a real LinuxCNC simulation configuration.
- `sim.ini` loads `hal/rio_sim.hal` and the LiNC HAL; motion,
  I/O, homing and ATC run in LinuxCNC, not in the HMI.
- Panel driver is `cnc-panel-sim`; link is `virtual`; the panel service owns the
  simulated window and exchanges state through `cnc_panel.*` HAL pins.
- Work offsets live in `configs/sim.var` (`PARAMETER_FILE = sim.var`, resolved
  next to the INI, which is also LinuxCNC's working directory). LinuxCNC's
  interpreter restores that file at startup, but it only *queues* the G5X
  message and the task delivers queued trajectory messages when motion is idle,
  so a session that starts in E-stop keeps the DRO at zero. Once the machine is
  powered on, `linc_bridge.py` re-initializes the interpreter
  (`reset_interpreter`), which re-reads the file and delivers the offsets; it
  reads `stat.g5x_offset` back before treating the sync as done, falls back to
  an MDI `G54`..`G59.3`, and retries for about a minute. Offset writes go
  through `G10 L2`/`G10 L20`, so the interpreter owns the values and writes them
  back on exit.
- `rio` is `hal/rio_sim.py`; no FPGA card or serial device.
- The HAL pin surface is the same `cnc_panel.*` and `rio.*` surface as the
  real machine, so the HMI uses the same code path.
- Modes/overrides use steady one-hot/steady values.
- `panel_link_stats = "none"` because there is no serial link to measure.
- `INC` steps are read from `[DISPLAY] INCREMENTS` in the machine INI. The
  leading `JOG` entry means continuous and is filtered out, so the selector
  offers only finite steps.
- Jog speed is a percentage of `[DISPLAY] DEFAULT_LINEAR_VELOCITY`, published
  absolute on `linc.jog-speed`. The HMI does the scaling.
- The simulated E-stop is owned by the panel GUI, not the HMI.
- The panel window boots with the E-stop latched and power OFF, matching a real
  cabinet before power-up. Releasing the mushroom raises `cnc_panel.estop_reset`
  (wired to `halui.estop.reset`) and leaves the machine off; `POWER ON`
  separately raises `cnc_panel.power_on`, wired to `rio.sim-power-on` and ORed
  into the same reset request.

Launch:

```sh
./scripts/start-sim.sh          # starts LinuxCNC, linc-display, linc-panel
./scripts/start-sim.sh --detach
./scripts/stop-sim.sh
```

This profile must never be used to command real hardware.

## 7. BaierMill physical profile

`configs/baiermill.toml`:

- `machine.kind = "real"` — no `SIM` badge.
- `linuxcnc_ini = "../../baiermill_pb/probe_basic.ini"`.
- Panel owner is `linc-panel`; the panel service owns `/dev/ttyUSB0` at
  115200 8N1. The HMI never opens it.
- `port`, `baud` and `cycle_time_ms` are diagnostic/launch metadata for the
  panel service. The real authority for the link remains `linc-panel` and the
  HAL pin surface.
- The same `cnc_panel.*` pins feed the HMI and the existing panel logic.
- The physical E-stop and machine-on chain remain hard-wired; the HMI has no
  software E-stop substitute.
- `panel_link_stats = "optional"` — the Diagnostics page shows counters when
  the panel service exposes them and otherwise reports "unavailable".

Launch:

```sh
linc --config configs/baiermill.toml
```

## 8. Safety policy

`base.toml` locks:

```toml
### `[jog]`

Continuous jog ceilings in machine units per minute, reached at 100 %.

| Key | Default | Meaning |
|---|---|---|
| `max_speed` | `3000` | Fastest continuous jog. |
| `rapid_speed` | `6000` | Fastest continuous jog while the panel's RAPID key is held. |

The machine still clamps to its own `MAX_VELOCITY`, so a ceiling above the
machine's limit simply saturates.

The operator's jog percentage is multiplied by the feed override, so the feed
override knob slows and speeds jogging exactly as it does a program, which is
what probe_basic's `mult2.jog-speed` does. With the override at 150 % the jog
can exceed `max_speed`; that is intentional and the machine clamps it.

[safety]
machine_control = "panel-with-explicit-mdi"
software_estop = false
software_cycle_control = false
software_jog = false
software_spindle = false
software_coolant = false
software_overrides = false
program_edit_while_running = "read-only"
program_protect = "panel"
mdi_execution = "explicit-action-only"
quit_requires_idle = true
confirm_quit = true
```

The loader must reject a profile that changes these to weaker values. The HMI
may edit offsets, tool data and the program when idle, subject to
`program_protect` and LinuxCNC state. It may issue explicit MDI, probe and
tool-change commands through Probe Basic `issue_mdi`; it never starts AUTO.

`program_protect` is the physical panel switch. While ON, the controller is
editing-locked: program edits/saves, offset/tool writes, NC_FILES imports and
probe applies are disabled. An unknown or stale switch state is treated as ON.
The backend rechecks the input for every write; unsaved editor content is
preserved read-only, never discarded.

LiNC writes only `linc.hmi-alive`, `linc.tool-change-confirm` and
`linc.status-display`. No generic HAL write path exists. The loader hard-codes
USB mount flags to include `nosuid,nodev,noexec`; a profile may add flags but
never remove them.

Machine mode/cycle/spindle/coolant/override controls remain on the panel. The
WHB pendant is jog-only; its other controls are ignored. `Ctrl+Q` is guarded and
must not quit while the machine is moving or a program is running.

## 9. Preferences

Runtime UI state is stored separately:

```text
~/.config/linc/<profile>.preferences.toml
```

Allowed preference keys include window geometry, last page, program view,
toolpath view angle, recent files and visible panels. Machine identity,
HAL pins, safety flags and the LinuxCNC INI path are never read from
preferences.

`--config`/machine profile changes require a restart. Preferences may be
written while running.

### UI scale

`[ui] scale` is a multiplier on the 1280×720 baseline and is applied
continuously as the window changes size:

| Value | Effect |
|---|---|
| `"auto"` | Baseline 1.0. |
| `"1.0"` | Design size at the 1280×720 baseline. |
| `"0.8"` | 80% of the baseline layout. |
| `"80%"` | Same as `0.8`. |
| `"1.25x"` | 125% of the baseline layout. |

Accepted factors are `0.5` through `3.0`. The HMI window enforces a minimum of
1280×720, scales the shell proportionally above that baseline, and preserves
the SVG type ratios. The Settings screen shows the
configured value and offers a temporary runtime override; edit the profile to
make a scale persistent. The simulation profile ships with `"1.0"`; the shell
handles desktop scaling independently.

The standalone simulator panel has its own zoom control,
`[simulation] panel_scale`, also in `0.5..3.0`; it is independent of the HMI
scale.

## 10. Schema reference

| Table | Keys |
|---|---|
| top level | `schema_version`, `profile`, `extends` |
| `[machine]` | `name`, `kind`, `gui`, `linuxcnc_ini` |
| `[mill]` | optional `max_axes` |
| `[lathe]` | optional `turret_stations`, `power_turret`, `live_tooling`, `live_tool_max_rpm`, `c_axis`, `y_axis`, `sub_spindle` |
| `[ui]` | `theme`, `start_page`, `window_title`, `scale`, `axis_order`, `pages` |
| `[program]` | `default_view`, `load_view`, `views`, `edit_view`, `line_numbers`, `highlight_current_line`, `autosave`, `live_preview`, `preview_requires_syntax_ok`, `preview_debounce_ms`, `tab_width` |
| `[files]` | `nc_files`, `usb_backend`, `usb_automount`, `usb_mount_options`, `load_extensions`, `overwrite`, `copy_while_running` |
| `[probe]` | `enabled`, `tool_number`, `wcs`, `subroutines`, `clearance`, `probe_depth`, `fast_feed`, `slow_feed`, `probe_diameter`, `max_travel`, `calibration_diameter` |
| `[tools]` | `changer`, `atc_pockets`, `manual_confirm` |
| `[manual]` | `auto_open`, `close_on_mode_exit`, `show_machine_coordinates`, `show_dtg`, `trail` |
| `[homing]` | `auto_open`, `close_on_mode_exit` |
| `[mdi]` | `execute_on_enter`, `auto_focus`, `history_limit`, `max_command_length`, `quick_commands` |
| `[toolpath]` | `show_grid`, `show_live_trail`, `trail_max_points`, `clear_trail_action`, `show_program_bounds`, `show_machine_bounds`, `show_program_labels`, `show_machine_labels`, `show_program_ticks`, `show_machine_ticks`, `perspective`, `multitool_colors`, `highlight_selected_line` |
| `[keys]` | `machine`, `program`, `program_cycle_view`, `tools`, `offsets`, `diagnostics`, `back`, `quit`, `focus_next`, `focus_prev`, `activate`, `open`, `save`, `undo`, `redo`, `find`, `select_all`, `copy`, `cut`, `paste`, `refresh`, `mdi_issue`, `mdi_clear`, `mdi_history_up`, `mdi_history_down`, `help_search`, `goto_line`, `view_cycle` |
| `[help]` | `source`, `bundle`, `search`, `history_limit` |
| `[safety]` | `machine_control`, `software_estop`, `software_cycle_control`, `software_jog`, `software_spindle`, `software_coolant`, `software_overrides`, `program_edit_while_running`, `program_protect`, `save_requires_idle`, `confirm_discard`, `mdi_execution`, `quit_requires_idle`, `confirm_quit` |
| `[backend]` | `kind`, `transport`, `status_hz`, `hal_hz`, `connect_timeout_ms` |
| `[hal]` | `panel_component`, `linc_component`, `write_policy`, `output_prefix`, `allowed_outputs` |
| `[hal.pins]` | `panel_connected`, `panel_error`, `program_protect`, `feed_override`, `rapid_override`, `spindle_override`, `spindle_load`, `spindle_speed` |
| `[panel]` | `owner`, `kind`, `driver`, `mode_encoding`, `override_encoding`, `read_only` |
| `[panel.status]` | `source`, `unknown` |
| `[panel.link]` | `read_only`, `kind`, optional `port`, `baud`, `data_bits`, `parity`, `stop_bits`, `cycle_time_ms` |
| `[pendant]` | `enabled`, `kind`, `jog_only`, `ignored_functions` |
| `[preferences]` | `file` |
| `[diagnostics]` | `event_log_capacity`, `panel_link_stats` |
| `[simulation]` | `enabled`, optional `panel`, `rio`, `estop`, `panel_window`, `panel_title`, `panel_scale` |

Enums:

- `machine.kind`: `sim`, `real`
- `machine.gui`: `mill`, `lathe`
- `tools.changer`: `manual`, `atc`, `turret`
- `panel.kind`: `simulated`, `physical`, `external`
- `panel.driver`: `cnc-panel`, `cnc-panel-sim`, `lathe-sim`, future driver names
- `panel.mode_encoding`: `one_hot_steady`
- `panel.override_encoding`: `analog_steady`
- `panel.status.source`: `wcs`, `spindle_load`, `units`
- `panel.link.kind`: `virtual`, `rs485`
- `pendant.kind`: `whb04b6`, `none`
- `pendant.jog_only`: `true`
- `mdi_execution` (safety): `explicit-action-only`
- `hal.write_policy`: `declared-outputs-only`
- `diagnostics.panel_link_stats`: `none`, `optional`, `required`
- `files.usb_backend`: `udisks2`, `none`
- `files.overwrite`: `confirm`

## 11. Validation checklist

For this design repository:

- `base.toml` and all three machine profiles parse with a standard TOML parser;
- `sim.toml` extends `base.toml`, is `gui = "mill"`, and points at an existing INI;
- `baiermill.toml` extends `base.toml`, is `gui = "mill"`, and points at an existing INI;
- `lathe-sim.toml` extends `base.toml`, is `gui = "lathe"`, and points at an existing INI;
- the mill simulation profile uses the built-in `cnc-panel-sim` driver, points
  at `configs/sim.ini`, and starts a real LinuxCNC process through
  `scripts/start-sim.sh`;
- the lathe profile declares turret/live-tool capabilities and has no panel window yet;
- the real profile contains no simulated panel or `rio_sim.py` path;
- the simulation profile contains no serial device path;
- all safety flags match the policy above;
- HAL pin names exist in the current panel/RIO configurations;
- the accepted G-code Help screen is present at `svg/gcode-help.svg`;
- the accepted Files screen is present at `svg/files.svg`;
- `[files] nc_files` is `"ini"`, not a duplicated path;
- USB mount options include `nosuid`, `nodev` and `noexec` and the loader
  rejects their removal;
- the lathe profile disables the mill-oriented touch-probe page;
- `[manual] auto_open` is `true` for JOG/INC/MPG;
- the Manual screen exposes no software jog, increment, handwheel, mode or
  override controls;
- the accepted Manual screen is present at `svg/manual.svg`;
- `[toolpath] show_live_trail` and `clear_trail_action` are `true`;
- the graph shows loaded program and live trail in distinct colors, and
  `CLEAR TRAIL` removes only the trail;
- `[panel.status] source` is one of `wcs`, `spindle_load`, `units`;
- the status display shows the active WCS by default and `--` when unknown;
- `[panel] mode_encoding = "one_hot_steady"` and
  `override_encoding = "analog_steady"`;
- `hal/linc.hal`, `hal/linc-postgui.hal` and `hal/linc-pendant.hal` exist;
- `linc.hal` contains no `linc.*` nets before the GUI component exists;
- `linc-postgui.hal` wires all three declared `linc.*` outputs;
- `linc-pendant.hal` wires only jog/handwheel/axis-select, never pendant
  mode/cycle/spindle/coolant/override/program/macro controls;
- `[homing] auto_open` is `true` for mode-driven screens;
- the Homing screen exposes no software home/unhome controls;
- `[tools] manual_confirm` is `true`;
- the lathe profile uses `tools.changer = "turret"`;
- manual mill profiles use a standard tool table without `P`; an ATC/turret
  profile requires `P` in `TOOL_TABLE_COLUMNS`;
- `[hal] write_policy = "declared-outputs-only"` and `allowed_outputs` is
  exactly `hmi-alive`, `tool-change-confirm`, `status-display`;
- `[pendant] jog_only = true` and non-jog functions are ignored;
- `base.toml` cannot be loaded as a runnable profile.

## 12. Remaining hardware verification

The software implementation is in place; these items still need a real machine
or a commissioned LinuxCNC session:

1. The live machine HAL must expose the generic `cnc_panel` component. Rename
   or alias the legacy branded panel component before loading `hal/linc.hal`.
2. Verify the RS-485 panel's status-display frame. The service exposes
   `cnc_panel.sseg_status` and the simulator renders it, but the current legacy
   frame carries only the two tool displays.
3. Verify `cnc_panel.home_state_x/y/z` against the machine's motmod version and
   confirm that individual home/proghome keys are inhibited during homing.
4. Test `udisks2` auto-mount/eject with polkit authorization, mount failure,
   busy eject, power-off and the `nosuid,nodev,noexec` options.
5. Commission the lathe power turret/live-tool HAL mapping. Add the standard
   `P` column to the lathe INI/tool table before enabling
   `configs/lathe-sim.toml`; the loader intentionally refuses a turret profile
   without `P`.
6. Test the WHB04B-6 pendant on hardware, including the JOG-only gate and
   ignored mode/cycle/spindle/coolant/override/program/macro controls.
7. Profile paths that use `../../baiermill_pb` require the sibling checkout
   layout; use absolute paths in an installed profile.
8. Decide whether the LinuxCNC bridge should remain the supervised official
   module helper or be replaced by an embedded PyO3/NML implementation. The
   `MachineBackend` boundary is unchanged either way.
