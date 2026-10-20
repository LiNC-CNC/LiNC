# HMI Configuration

Status: design contract for the Rust HMI. The HMI binary and its loader do not
exist yet; these profiles define how they must behave.

## 1. Rules

1. **One profile per machine.** The operator selects a profile explicitly. The
   HMI must refuse to start if no profile is supplied.
2. **Shared defaults live in `configs/base.toml`.** Machine profiles extend it
   and override only what is machine-specific.
3. **The LinuxCNC INI is authoritative for machine data.** Travel limits,
   velocities, units, joint count, spindle count, ATC settings, the tool table
   and the program prefix come from the INI. They are not duplicated in TOML.
4. **The HMI never owns the panel.** It never opens `/dev/ttyUSB*`, loads the
   panel component, or writes `szgh_panel.*` outputs. It reads HAL pins and
   LinuxCNC status only.
5. **Safety policy is not a preference.** A profile cannot enable software
   E-stop, cycle control, jog, spindle, coolant or overrides.
6. **User preferences are separate.** Window geometry, last page, view angle
   and recent files live under `~/.config/cnc-hmi/`, not in the machine profile.
7. **Unknown keys are errors.** This catches typos and prevents a machine
   profile from silently not applying a setting.

## 2. Profiles

| File | Purpose |
|---|---|
| `configs/base.toml` | Shared UI, keymap, safety policy, HAL pin map, backend and preferences defaults. Not runnable alone. |
| `configs/sim.toml` | Mill simulation: `../../baiermill_sim/probe_basic.ini`, simulated SZGH panel, `rio_sim.py`. |
| `configs/baiermill.toml` | Physical mill: `../../baiermill_pb/probe_basic.ini`, real SZGH-1080 panel over RS-485. |
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
cnc-hmi --config configs/sim.toml
cnc-hmi --config configs/baiermill.toml
```

`--config` or the `CNC_HMI_CONFIG` environment variable is required. There is
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
DISPLAY = cnc-hmi --config /home/jack/CNC/cnc-hmi-design/configs/baiermill.toml
```

Simulation:

```ini
[DISPLAY]
DISPLAY = cnc-hmi --config /home/jack/CNC/cnc-hmi-design/configs/sim.toml
```

Do not change the existing `probe_basic.ini` until the `cnc-hmi` executable is
installed; Probe Basic remains the current display.

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
- `[hal] panel_component`
- `[panel] kind`, `owner`, `read_only = true`
- `[panel.link] read_only = true`
- `[safety] machine_control = "panel-only"` and all `software_* = false`

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
| ATC pockets and heights | INI `[ATC]` |
| Tool table | INI `[EMCIO] TOOL_TABLE` |
| Program prefix / open file | INI `[DISPLAY] PROGRAM_PREFIX`, `OPEN_FILE` |
| `NC_FILES` root | INI `PROGRAM_PREFIX`; `[files] nc_files = "ini"` |
| Machine graph bounds | INI travel limits / kinematics |
| Program graph bounds | Last valid preview parse; never from a syntax-error state |
| USB mount points | `udisks2` D-Bus, not config |
| Probe subroutines | INI `SUBROUTINE_PATH`; `[probe] subroutines = "ini"` |
| Probe parameters/results | LinuxCNC parameters `#3014`–`#3036`, results `#1001`–`#1009` |
| Panel/RIO HAL pin names | `configs/base.toml` |
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
| ATC | 12 pockets, Z tool-change height -194 mm |
| Tool table | `tool.tbl` |
| Real config | `../../baiermill_pb/probe_basic.ini` |
| Sim config | `../../baiermill_sim/probe_basic.ini` |

The simulation INI intentionally carries the same travel, tuning, homing order
and ATC values as the machine; only the HAL stack and paths differ.

## 6. Simulation profile

`configs/sim.toml`:

- `machine.kind = "sim"` — the HMI shows a persistent `SIM` badge.
- `linuxcnc_ini = "../../baiermill_sim/probe_basic.ini"`.
- Panel is `baiermill_sim/panel/szgh_panel_gui.py`; link is `virtual`.
- `rio` is `baiermill_sim/rio_sim.py`; no FPGA card or serial device.
- The HAL pin surface is the same `szgh_panel.*` and `rio.*` surface as the
  real machine, so the HMI uses the same code path.
- `panel_link_stats = "none"` because there is no serial link to measure.
- The simulated E-stop is owned by the panel GUI, not the HMI.

Launch:

```sh
cnc-hmi --config configs/sim.toml
```

This profile must never be used to command real hardware.

## 7. BaierMill physical profile

`configs/baiermill.toml`:

- `machine.kind = "real"` — no `SIM` badge.
- `linuxcnc_ini = "../../baiermill_pb/probe_basic.ini"`.
- Panel owner is `szgh-panel-hal`; the panel driver owns `/dev/ttyUSB0` at
  115200 8N1.
- `port`, `baud` and `cycle_time_ms` are **diagnostic metadata only**. The HMI
  must not open the port. The real authority for the link remains
  `hallib/szgh_panel.hal` and the panel driver.
- The same `szgh_panel.*` pins feed the HMI and the existing panel logic.
- The physical E-stop and machine-on chain remain hard-wired; the HMI has no
  software E-stop substitute.
- `panel_link_stats = "optional"` — the Diagnostics page shows counters when
  the panel driver exposes them and otherwise reports "unavailable".

Launch:

```sh
cnc-hmi --config configs/baiermill.toml
```

## 8. Safety policy

`base.toml` locks:

```toml
[safety]
machine_control = "panel-only"
software_estop = false
software_cycle_control = false
software_jog = false
software_spindle = false
software_coolant = false
software_overrides = false
program_edit_while_running = "read-only"
program_protect = "panel"
mdi_requires_mode = "mdi"
quit_requires_idle = true
confirm_quit = true
```

The loader must reject a profile that changes these to weaker values. The HMI
may still edit offsets, tool data and the program when idle, subject to
`program_protect` and LinuxCNC state.

Machine commands remain on the panel and pendant. `Ctrl+Q` is guarded and must
not quit while the machine is moving or a program is running.

## 9. Preferences

Runtime UI state is stored separately:

```text
~/.config/cnc-hmi/<profile>.preferences.toml
```

Allowed preference keys include window geometry, last page, program view,
toolpath view angle, recent files and visible panels. Machine identity,
HAL pins, safety flags and the LinuxCNC INI path are never read from
preferences.

`--config`/machine profile changes require a restart. Preferences may be
written while running.

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
| `[probe]` | `enabled`, `tool_number`, `wcs`, `subroutines`, `clearance`, `probe_depth`, `fast_feed`, `slow_feed`, `probe_diameter`, `max_travel`, `calibration_diameter`, `prepare_only` |
| `[toolpath]` | `show_grid`, `show_program_bounds`, `show_machine_bounds`, `show_program_labels`, `show_machine_labels`, `show_program_ticks`, `show_machine_ticks`, `perspective`, `multitool_colors`, `highlight_selected_line` |
| `[keys]` | `machine`, `program`, `program_cycle_view`, `tools`, `offsets`, `diagnostics`, `back`, `quit`, `focus_next`, `focus_prev`, `activate`, `open`, `save`, `undo`, `redo`, `find`, `select_all`, `help_search` |
| `[help]` | `source`, `bundle`, `search`, `history_limit` |
| `[safety]` | `machine_control`, `software_estop`, `software_cycle_control`, `software_jog`, `software_spindle`, `software_coolant`, `software_overrides`, `program_edit_while_running`, `program_protect`, `save_requires_idle`, `confirm_discard`, `mdi_requires_mode`, `quit_requires_idle`, `confirm_quit` |
| `[backend]` | `kind`, `transport`, `status_hz`, `hal_hz`, `connect_timeout_ms` |
| `[hal]` | `read_only`, `panel_component`, `panel_mode_prefix` |
| `[hal.pins]` | `panel_connected`, `panel_error`, `program_protect`, `feed_override`, `rapid_override`, `spindle_override`, `spindle_load`, `spindle_speed` |
| `[panel]` | `owner`, `kind`, `read_only` |
| `[panel.link]` | `read_only`, `kind`, optional `port`, `baud`, `data_bits`, `parity`, `stop_bits`, `cycle_time_ms` |
| `[preferences]` | `file` |
| `[diagnostics]` | `event_log_capacity`, `panel_link_stats` |
| `[simulation]` | `enabled`, optional `panel`, `rio`, `estop`, `panel_window`, `panel_title`, `panel_scale` |

Enums:

- `machine.kind`: `sim`, `real`
- `machine.gui`: `mill`, `lathe`
- `panel.kind`: `simulated`, `physical`, `external`
- `panel.link.kind`: `virtual`, `rs485`
- `diagnostics.panel_link_stats`: `none`, `optional`, `required`
- `files.usb_backend`: `udisks2`, `none`
- `files.overwrite`: `confirm`

## 11. Validation checklist

For this design repository:

- `base.toml` and all three machine profiles parse with a standard TOML parser;
- `sim.toml` extends `base.toml`, is `gui = "mill"`, and points at an existing INI;
- `baiermill.toml` extends `base.toml`, is `gui = "mill"`, and points at an existing INI;
- `lathe-sim.toml` extends `base.toml`, is `gui = "lathe"`, and points at an existing INI;
- the mill profiles resolve `simulation.panel` / `simulation.rio` where applicable;
- the lathe profile declares turret/live-tool capabilities and has no panel window yet;
- the real profile contains no simulated panel or `rio_sim.py` path;
- the simulation profile contains no serial device path;
- all safety flags match the policy above;
- HAL pin names exist in the current panel/RIO configurations;
- the accepted G-code Help screen is present at `svg/gcode-help.svg`;
- the accepted Files screen is present at `svg/files.svg`;
- `[files] nc_files` is `"ini"`, not a duplicated path;
- USB mount options include `nosuid`, `nodev` and `noexec`;
- `[probe] prepare_only` is `true` for the mill profiles;
- the lathe profile disables the mill-oriented touch-probe page;
- `base.toml` cannot be loaded as a runnable profile.

## 12. Open issues

1. The `cnc-hmi` binary and config loader do not exist yet. `--check-config`
   and the merge rules in this document are the implementation contract.
2. The panel driver currently hard-codes `/dev/ttyUSB0` and gets its cycle time
   from `hallib/szgh_panel.hal` (`-c 0.01`). The profile's `[panel.link]`
   fields are metadata until the driver reads a shared config.
3. The panel driver does not expose RX/TX counters yet, so
   `panel_link_stats = "optional"` will show "unavailable" on the real machine.
4. The current HAL cycle time is 0.01 s while the protocol sleeps about 14 ms
   per transaction. Raise it to 0.02–0.05 s when the driver is fixed; update
   the profile and `szgh_panel.hal` together.
5. Installing profiles outside the repository breaks relative
   `../../baiermill_pb` paths. Either preserve the sibling layout or use absolute
   paths in the installed profile.
6. The G-code help bundle currently exists only as the Python data module in
   `../baiermill_pb/user_tabs/gcode_help/`. The accepted screen is
   `svg/gcode-help.svg`, but the HMI still needs a Rust-friendly generated
   format (JSON or TOML), a pinned LinuxCNC docs version, and preserved
   documentation/image attribution.
7. The G-code editor is design-only. Still to implement: the `ropey` buffer,
   atomic save, external-change detection, unsaved-change dialog, search,
   undo/redo, and a preview parser that cannot disturb the running interpreter.
   The QMK keyboard has no `Shift`, `Tab` or `Delete`; selection must use the
   mouse layer and line-based operations.
8. Panel mode pins are pulse outputs. The HMI latches
   `szgh_panel.mode_*` to track `EDIT`; a persistent mode index from the panel
   driver would be more robust across HMI restarts.
9. USB auto-mount uses `udisks2` over D-Bus. `udisks2` is inactive on the
   development box; enable/verify it on the machine and test polkit
   authorization, mount failure, busy eject and power-off.
10. File copy implementation must reject symlinks/path escapes, check free
    space, confirm overwrite, use a temporary file plus rename, report progress,
    and prevent eject during a copy.
11. Lathe support needs HAL pin mapping for power-turret station/clamp/index and
    live-tool drive/RPM/direction; the existing lathe simulator exposes neither.
12. A lathe-specific simulator panel mockup is still needed; the current
    `svg/sim-panel.svg` is the mill SZGH-1080 panel.
13. Port the Probe Basic parameter/result contracts and G-code calls; verify
    `prepare_only` is enforced so the HMI never starts probing motion itself.
