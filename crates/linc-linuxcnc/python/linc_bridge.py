#!/usr/bin/env python3
"""LiNC LinuxCNC bridge helper.

Speaks newline-delimited JSON on stdin/stdout. It uses the official `linuxcnc`
and `hal` Python modules and never opens the panel serial port.

The Rust side treats this process as a backend transport. All machine-control
operations are initiated by explicit typed commands; there is no generic MDI or
HAL escape hatch in the protocol.
"""
from __future__ import annotations

import configparser
import json
import os
import queue
import select
import sys
import threading
import time
import traceback

import linuxcnc
import hal

STATUS_HZ = 20.0
HAL_BITMAP_HZ = 1.0
HAL_BITMAP_PERIOD = 1.0 / HAL_BITMAP_HZ

MODE_NAMES = [
    ("mode_edit", "edit"),
    ("mode_auto", "auto"),
    ("mode_mdi", "mdi"),
    ("mode_jog", "jog"),
    ("mode_mpg", "mpg"),
    ("mode_inc", "inc"),
    ("mode_home", "home"),
    ("mode_proghome", "proghome"),
]

BUTTON_PINS = [
    ("program_protect", 0), ("estop", 7), ("cycle_start", 14), ("cycle_stop", 15),
    ("rapid", 63), ("mlk", 55), ("flush", 47), ("spd_sp", 39), ("k2", 34),
    ("jog_xn", 62), ("bdt", 54), ("spd_cts", 46), ("spd_cw", 38), ("arm_man", 30),
    ("jog_yp", 61), ("sbk", 53), ("coolant", 45), ("mag_ccw", 37), ("pot_dn", 29),
    ("jog_zp", 60), ("mpg_sim", 52), ("pt_la", 44), ("mag_cw", 36), ("chuck", 28),
    ("jog_an", 59), ("jog_ap", 51), ("air_blow", 43), ("spd_ori", 35),
    ("chip_ccw", 27), ("jog_bp", 58), ("jog_zn", 50), ("worklamp", 42),
    ("k1", 34), ("chip_sp", 26), ("safe_door", 57), ("jog_yn", 49), ("drn", 41),
    ("corridor", 33), ("chip_cw", 25), ("k4", 17), ("jog_bn", 56), ("jog_xp", 48),
    ("opt", 40), ("lub", 32), ("spd_ccw", 24), ("k3", 16),
]

LED_PINS = [
    ("led_rapid", 0), ("led_mlk", 1), ("led_flush", 2), ("led_spd_sp", 3),
    ("led_k2", 4), ("led_c", 5), ("led_cycle_off", 6), ("led_jog_xn", 8),
    ("led_bdt", 9), ("led_spd_cts", 10), ("led_spd_cw", 11), ("led_arm_man", 12),
    ("led_b", 13), ("led_cycle_on", 14), ("led_jog_yp", 16), ("led_sbk", 17),
    ("led_coolant", 18), ("led_mag_ccw", 19), ("led_pot_dn", 20), ("led_a", 21),
    ("led_ready", 22), ("led_jog_zp", 24), ("led_mpg_sim", 25), ("led_pt_la", 26),
    ("led_mag_cw", 27), ("led_chuck", 28), ("led_z", 29), ("led_run", 30),
    ("led_jog_an", 32), ("led_jog_ap", 33), ("led_air_blow", 34),
    ("led_spd_ori", 35), ("led_chip_ccw", 36), ("led_y", 37),
    ("led_airblow_alarm", 38), ("led_jog_bp", 40), ("led_jog_zn", 41),
    ("led_worklamp", 42), ("led_k1", 43), ("led_chip_sp", 44), ("led_x", 45),
    ("led_oil_alarm", 46), ("led_safe_door", 48), ("led_jog_yn", 49),
    ("led_drn", 50), ("led_corridor", 51), ("led_chip_cw", 52), ("led_k4", 53),
    ("led_gear_alarm", 54), ("led_jog_bn", 56), ("led_jog_xp", 57),
    ("led_opt", 58), ("led_lub", 59), ("led_spd_ccw", 60), ("led_k3", 61),
    ("led_mc_alarm", 62),
]


def emit(payload):
    try:
        sys.stdout.write(json.dumps(payload, separators=(",", ":")) + "\n")
        sys.stdout.flush()
    except Exception:
        pass


def parse_coordinates(raw):
    """Split an INI COORDINATES value into individual axis letters.

    Accepts `X Y Z`, `XYZ`, `X,Y,Z` and mixed forms such as `XY Z`.
    """
    axes = []
    for token in str(raw or "").replace(",", " ").split():
        if len(token) > 1 and token.isalpha():
            axes.extend(token.upper())
        elif token:
            axes.append(token.upper())
    return axes or ["X", "Y", "Z"]


def pin(name, default=None):
    try:
        return hal.get_value(name)
    except Exception:
        return default


def pin_bool(name, default=None):
    value = pin(name, None)
    if value is None:
        return default
    return bool(value)


class Bridge:
    def __init__(self):
        # The Python HAL binding requires an initialized userspace component
        # before hal.get_value() can read other components' pins. This bridge
        # owns no pins; the component is only a read handle.
        self.hal_component = None
        try:
            self.hal_component = hal.component(f"linc_bridge_{os.getpid()}")
            self.hal_component.ready()
        except Exception as error:
            print(f"linc bridge: HAL read component unavailable: {error}", file=sys.stderr)
        self.stat = linuxcnc.stat()
        self.cmd = linuxcnc.command()
        self.ini_path = os.path.realpath(self.stat.ini_filename or "")
        self.config = configparser.ConfigParser(inline_comment_prefixes=("#", ";"))
        try:
            self.config.read(self.ini_path)
        except Exception:
            pass
        # `[TRAJ] COORDINATES` is written both as `X Y Z` and as the trivkins
        # shorthand `XYZ`. A bare `.split()` turns `XYZ` into the single axis
        # name "XYZ", so every `X`/`Y`/`Z` lookup missed and the HMI showed a
        # frozen 0.000 DRO with a tool marker stuck at the origin.
        self.axes = parse_coordinates(
            self.config.get("TRAJ", "COORDINATES", fallback="XYZ")
        )
        self.linear_units = self.config.get("TRAJ", "LINEAR_UNITS", fallback="mm")
        self.angular_units = self.config.get("TRAJ", "ANGULAR_UNITS", fallback="degree")
        self.tool_table = self.config.get("EMCIO", "TOOL_TABLE", fallback="")
        self.param_file = self.config.get("RS274NGC", "PARAMETER_FILE", fallback="")
        self.program_prefix = self.config.get("DISPLAY", "PROGRAM_PREFIX", fallback=".")
        self.panel_connected_last = None
        self.last_proghome = [False, False, False, False]
        self.last_panel_heartbeat = None
        self.last_panel_heartbeat_time = 0.0
        self.pending_restore = None
        self.last_program_path = None
        self.last_program_text = None
        self.last_program_mtime = None
        self.alarms = []
        self.bitmap_cache = {"buttons": 0, "leds": 0, "sseg_tool": 0, "sseg_next": 0}
        self.bitmap_last = 0.0
        self.start_error_thread()

    # ------------------------------------------------------------------ errors
    def start_error_thread(self):
        def run():
            try:
                channel = linuxcnc.error_channel()
            except Exception:
                return
            while True:
                try:
                    error = channel.poll()
                    if not error:
                        time.sleep(0.1)
                        continue
                    kind, text = error
                    severity = "error" if kind in (
                        linuxcnc.OPERATOR_ERROR if hasattr(linuxcnc, "OPERATOR_ERROR") else 11,
                        linuxcnc.NML_ERROR if hasattr(linuxcnc, "NML_ERROR") else 12,
                    ) else "info"
                    with threading.Lock():
                        self.alarms.append({"kind": severity, "text": str(text), "time": time.time()})
                        self.alarms = self.alarms[-100:]
                except Exception:
                    time.sleep(0.2)
        threading.Thread(target=run, name="linc-errors", daemon=True).start()

    def poll_proghome(self, panel):
        """Issue the fixed PROG.HOME panel commands on rising edges.

        This is the only panel-driven MDI path besides the explicit HMI
        MDI/probe/tool screens. It is deliberately fixed text: PROG.HOME asks
        for X/Y/Z only in the current WCS, never a caller-supplied block.

        SPEC 6.10 requires the same preconditions as any other issued MDI:
        machine ON, all axes homed and the interpreter idle, or the action is
        refused. The previous task mode is restored once the interpreter
        returns idle, exactly like the HMI's own issue_mdi path.
        """
        if panel.get("mode", {}).get("mode") != "proghome":
            self.last_proghome = [False, False, False, False]
            return
        pins = [
            pin_bool("cnc_panel.proghome-all", False),
            pin_bool("cnc_panel.proghome-x", False),
            pin_bool("cnc_panel.proghome-y", False),
            pin_bool("cnc_panel.proghome-z", False),
        ]
        commands = ["G0 X0Y0Z0", "G0 X0", "G0 Y0", "G0 Z0"]
        for index, (active, command) in enumerate(zip(pins, commands)):
            if active and not self.last_proghome[index]:
                # PROG.HOME is a panel action, so it must not be gated on the
                # panel mode being MDI (require("mdi") would refuse it).
                error = self.require("proghome")
                if error:
                    self.alarms.append(
                        {
                            "kind": "error",
                            "text": f"PROG.HOME {command} refused: {error}",
                            "time": time.time(),
                        }
                    )
                    self.alarms = self.alarms[-100:]
                    self.last_proghome[index] = active
                    continue
                previous = int(
                    getattr(self.stat, "task_mode", linuxcnc.MODE_MANUAL)
                    or linuxcnc.MODE_MANUAL
                )
                try:
                    if previous != linuxcnc.MODE_MDI:
                        self.cmd.mode(linuxcnc.MODE_MDI)
                        if not self.cmd.wait_complete(2.0):
                            raise RuntimeError("could not select MDI mode")
                    self.cmd.mdi(command)
                    if previous != linuxcnc.MODE_MDI and self.pending_restore is None:
                        self.pending_restore = previous
                except Exception as error:
                    self.alarms.append(
                        {
                            "kind": "error",
                            "text": f"PROG.HOME {command} rejected: {error}",
                            "time": time.time(),
                        }
                    )
                    self.alarms = self.alarms[-100:]
            self.last_proghome[index] = active

    # ---------------------------------------------------------------- snapshot
    def read_parameters(self):
        values = {}
        if not self.param_file:
            return values
        path = self.param_file
        if not os.path.isabs(path):
            path = os.path.join(os.path.dirname(self.ini_path), path)
        try:
            with open(path, "r", encoding="utf-8", errors="replace") as handle:
                for line in handle:
                    parts = line.split()
                    if len(parts) == 2 and parts[0].startswith(("100", "52", "53")):
                        try:
                            values[int(parts[0])] = float(parts[1])
                        except ValueError:
                            pass
        except OSError:
            pass
        return values

    def read_offsets(self, params):
        offsets = []
        for index in range(1, 7):
            base = 5220 + (index - 1) * 20
            values = {}
            for axis_index, axis in enumerate(self.axes):
                values[axis] = params.get(base + 1 + axis_index, 0.0)
            offsets.append(
                {
                    "index": index,
                    "values": values,
                    "rotation": params.get(base, 0.0),
                }
            )
        g92 = {axis: params.get(5201 + i, 0.0) for i, axis in enumerate(self.axes)}
        return offsets, g92

    def read_panel(self, sparse):
        active = []
        for pin_name, mode in MODE_NAMES:
            if pin_bool("cnc_panel." + pin_name, False):
                active.append(mode)
        if len(active) == 1:
            mode = {"valid": True, "mode": active[0], "active": active}
        elif active:
            mode = {"valid": False, "mode": None, "active": active}
        else:
            mode = {"valid": False, "mode": None, "active": []}
        connected = pin_bool("cnc_panel.panel_connected", False)
        error = pin_bool("cnc_panel.communication_error", True)
        heartbeat = pin("cnc_panel.heartbeat", None)
        now = time.time()
        if heartbeat is None:
            heartbeat_ok = True
        else:
            if heartbeat != self.last_panel_heartbeat:
                self.last_panel_heartbeat = heartbeat
                self.last_panel_heartbeat_time = now
            heartbeat_ok = (now - self.last_panel_heartbeat_time) < 1.5
        if not heartbeat_ok:
            connected = False
            error = True
        protect = pin_bool("cnc_panel.program_protect", None)
        protect_known = protect is not None and connected
        if protect is None:
            protect = True
        feed = pin("halui.feed-override.value")
        if feed is None:
            feed = pin("cnc_panel.feed_override")
        rapid = pin("halui.rapid-override.value")
        if rapid is None:
            rapid = pin("cnc_panel.rapid_override")
        spindle = pin("halui.spindle.0.override.value")
        if spindle is None:
            spindle = pin("cnc_panel.spindle_override")
        panel = {
            "connected": bool(connected),
            "error": bool(error),
            "mode": mode,
            "protect": bool(protect),
            "protect_known": bool(protect_known),
            "feed_override": float(feed if feed is not None else 1.0),
            "rapid_override": float(rapid if rapid is not None else 1.0),
            "spindle_override": float(spindle if spindle is not None else 1.0),
            "sseg_status": int(pin("cnc_panel.sseg_status", -1) or -1),
            "estop": bool(pin_bool("cnc_panel.estop", False)),
            "next_tool": int(pin("iocontrol.0.tool-prep-number", 0) or 0),
        }
        if sparse:
            buttons = 0
            for name, bit in BUTTON_PINS:
                if pin_bool("cnc_panel." + name, False):
                    buttons |= 1 << bit
            leds = 0
            for name, bit in LED_PINS:
                if pin_bool("cnc_panel." + name, False):
                    leds |= 1 << bit
            self.bitmap_cache = {
                "buttons": buttons,
                "leds": leds,
                "sseg_tool": int(pin("cnc_panel.sseg_magtool", 0) or 0),
                "sseg_next": int(pin("cnc_panel.sseg_spdtool", 0) or 0),
            }
            self.bitmap_last = time.time()
        panel.update(
            {
                "buttons": self.bitmap_cache["buttons"],
                "leds": self.bitmap_cache["leds"],
                "sseg_tool": self.bitmap_cache["sseg_tool"],
                "sseg_next": self.bitmap_cache["sseg_next"],
            }
        )
        return panel

    def snapshot(self):
        s = self.stat
        try:
            s.poll()
        except Exception:
            pass
        params = self.read_parameters()
        offsets, g92 = self.read_offsets(params)
        active_wcs = int(getattr(s, "g5x_index", 1) or 1)
        # Status fields differ slightly across LinuxCNC versions.
        homed = list(getattr(s, "homed", ()) or ())
        actual = list(getattr(s, "actual_position", ()) or ())
        commanded = list(getattr(s, "position", ()) or ())
        dtg = list(getattr(s, "dtg", ()) or ())
        g5x = list(getattr(s, "g5x_offset", ()) or ())
        g92_offset = list(getattr(s, "g92_offset", ()) or ())
        positions = []
        for index, axis in enumerate(self.axes[:9]):
            work = actual[index] if index < len(actual) else 0.0
            mach = work + (g5x[index] if index < len(g5x) else 0.0) + (
                g92_offset[index] if index < len(g92_offset) else 0.0
            )
            positions.append(
                {
                    "axis": axis,
                    "work": work,
                    "machine": mach,
                    "dtg": dtg[index] if isinstance(dtg, (list, tuple)) and index < len(dtg) else (dtg if isinstance(dtg, (int, float)) else 0.0),
                    "velocity": 0.0,
                }
            )
        program_path = getattr(s, "file", "") or ""
        program_text = None
        try:
            mtime = os.path.getmtime(program_path) if program_path else None
        except OSError:
            mtime = None
        if (
            program_path == self.last_program_path
            and mtime == self.last_program_mtime
        ):
            program_text = None
        elif program_path:
            try:
                with open(program_path, "r", encoding="utf-8", errors="replace") as handle:
                    program_text = handle.read()
            except OSError:
                program_text = ""
            self.last_program_path = program_path
            self.last_program_text = program_text
            self.last_program_mtime = mtime
        spindles = []
        for index, spindle in enumerate(getattr(s, "spindle", ()) or ()):
            if index >= 4:
                break
            load_pin = pin("rio.spindle-modbus.spindle_load-abs")
            if index > 0:
                load_pin = pin(f"rio.spindle{index}-modbus.spindle_load-abs", None)
            spindles.append(
                {
                    "index": index,
                    "speed": float(spindle.get("speed", 0.0)),
                    "override": float(spindle.get("override", 1.0)),
                    "direction": int(spindle.get("direction", 0)),
                    "on": bool(spindle.get("enabled", 0)),
                    "load": float(load_pin) if load_pin is not None else None,
                }
            )
        gcodes = list(getattr(s, "gcodes", ()) or ())
        mcodes = list(getattr(s, "mcodes", ()) or ())
        task_mode = int(getattr(s, "task_mode", 0) or 0)
        exec_state = int(getattr(s, "exec_state", 2) or 2)
        interp_state = int(getattr(s, "interp_state", 1) or 1)
        task_state = int(getattr(s, "task_state", 0) or 0)
        estop = bool(getattr(s, "estop", 0))
        enabled = bool(getattr(s, "enabled", False))
        if estop:
            power = "estop"
        elif task_state == linuxcnc.STATE_ON and enabled:
            power = "on"
        elif task_state == linuxcnc.STATE_ESTOP_RESET:
            power = "estop_reset"
        else:
            power = "off"
        sparse = time.time() - self.bitmap_last >= HAL_BITMAP_PERIOD
        panel = self.read_panel(sparse)
        self.poll_proghome(panel)
        homing = {}
        following_error = {}
        for index, axis in enumerate(self.axes):
            try:
                state = int(pin(f"joint.{index}.home-state", 0) or 0)
            except (TypeError, ValueError):
                state = 0
            homing[axis] = 1 <= state <= 4
            try:
                following_error[axis] = float(pin(f"joint.{index}.f-error", 0.0) or 0.0)
            except (TypeError, ValueError):
                following_error[axis] = 0.0
        probe_values = {str(n): params.get(n, 0.0) for n in range(1001, 1010)}
        return {
            "type": "snapshot",
            "time": time.time(),
            "connected": bool(getattr(s, "echo_serial_number", 0) is not None),
            "task": {
                "power": power,
                "mode": {2: "auto", 3: "mdi", 1: "manual"}.get(task_mode, "unknown"),
                "interp": {1: "idle", 2: "reading", 3: "paused", 4: "waiting"}.get(interp_state, "unknown"),
                "exec_state": exec_state,
                "paused": bool(getattr(s, "paused", False)),
                "running": task_mode == linuxcnc.MODE_AUTO and exec_state != linuxcnc.EXEC_DONE,
                "motion_active": bool(
                    abs(float(getattr(s, "current_vel", 0.0) or 0.0)) > 1e-6
                    or bool(getattr(s, "probing", False))
                ),
            },
            "machine": {
                "on": enabled,
                "estop": estop,
                "homed": {axis: bool(homed[i]) if i < len(homed) else False for i, axis in enumerate(self.axes)},
                "homing": homing,
                "limits": [],
                "fault": estop,
                "force_homing": not bool(
                    self.config.getboolean("TRAJ", "NO_FORCE_HOMING", fallback=True)
                ),
                "following_error": following_error,
                "servo_load": {axis: 0.0 for axis in self.axes},
                "commanded": {axis: commanded[i] if i < len(commanded) else 0.0 for i, axis in enumerate(self.axes)},
                "actual": {axis: actual[i] if i < len(actual) else 0.0 for i, axis in enumerate(self.axes)},
                "coolant_flood": bool(getattr(s, "flood", False)),
                "coolant_mist": bool(getattr(s, "mist", False)),
                "lube": bool(getattr(s, "lube", False)),
                "lube_level_ok": True,
                "door_closed": bool(pin_bool("cnc_panel.led_safe_door", False)),
                "tool_change_active": bool(pin_bool("iocontrol.0.tool-change", False)),
                "echo_serial": int(getattr(s, "echo_serial_number", 0) or 0),
                "command_age_ms": 0,
                "status_age_ms": 0,
                "nml_command_connected": True,
                "nml_status_connected": True,
                "hal_thread_ok": True,
            },
            "positions": positions,
            "offsets": {
                "active": active_wcs,
                "offsets": offsets,
                "g92": g92,
            },
            "spindles": spindles,
            "feed": {
                "feed": float(getattr(s, "feedrate", 0.0) or 0.0),
                "feed_override": panel["feed_override"],
                "rapid_override": panel["rapid_override"],
                "spindle_override": panel["spindle_override"],
                "units": self.linear_units,
                "feed_mode": "G94",
                "distance_mode": "G90",
                "feed_override_enabled": bool(getattr(s, "feed_override_enabled", True)),
                "spindle_override_enabled": bool(pin_bool("halui.spindle.0.override.is-on", True)),
                "adaptive_feed": bool(getattr(s, "adaptive_feed_enabled", False)),
            },
            "program": {
                "path": program_path,
                "current_line": int(getattr(s, "current_line", 0) or 0),
                "read_line": int(getattr(s, "read_line", 0) or 0),
                "text": program_text,
                "gcodes": gcodes,
                "mcodes": mcodes,
                "tool_in_spindle": int(getattr(s, "tool_in_spindle", 0) or 0),
                "paused": bool(getattr(s, "paused", False)),
                "running": task_mode == linuxcnc.MODE_AUTO and exec_state != linuxcnc.EXEC_DONE,
                "motion_active": bool(
                    abs(float(getattr(s, "current_vel", 0.0) or 0.0)) > 1e-6
                    or bool(getattr(s, "probing", False))
                ),
            },
            "panel": panel,
            "tool": {
                "current": int(getattr(s, "tool_in_spindle", 0) or 0),
                "prepared": int(pin("iocontrol.0.tool-prep-number", 0) or 0),
                "change_active": bool(pin_bool("iocontrol.0.tool-change", False)),
            },
            "probe": {
                "running": bool(getattr(s, "probing", False)),
                "tripped": bool(getattr(s, "probe_tripped", False)),
                "position": list(getattr(s, "probed_position", ()) or ()),
                "values": probe_values,
            },
            "alarms": list(self.alarms[-40:]),
            "pendant": {
                "connected": bool(pin_bool("whb.axis.x.jog-enable", False)),
                "jog_enable": bool(pin_bool("whb.axis.x.jog-enable", False)),
                "active_axis": None,
                "handwheel_scale": pin("whb.axis.x.jog-scale", None),
                "step_mode": None,
            },
        }

    # ---------------------------------------------------------------- commands
    def require(self, origin):
        s = self.stat
        s.poll()
        panel = self.read_panel(False)
        if not panel["connected"]:
            return "panel link is not available"
        if bool(getattr(s, "estop", 0)) or int(getattr(s, "task_state", 0)) != linuxcnc.STATE_ON:
            return "machine is not ON"
        if int(getattr(s, "interp_state", 1)) != linuxcnc.INTERP_IDLE:
            return "interpreter is not idle"
        homed = list(getattr(s, "homed", ()) or ())
        if not homed or not all(bool(homed[i]) for i in range(len(self.axes))):
            return "not all axes are homed"
        if origin == "mdi" and panel["mode"].get("mode") != "mdi":
            return "panel mode is not MDI"
        return None

    def issue(self, command):
        origin = command.get("origin", "mdi")
        blocks = command.get("blocks") or []
        error = self.require(origin)
        if error:
            return {"ok": False, "error": error}
        previous = int(getattr(self.stat, "task_mode", linuxcnc.MODE_MANUAL) or linuxcnc.MODE_MANUAL)
        try:
            if previous != linuxcnc.MODE_MDI:
                self.cmd.mode(linuxcnc.MODE_MDI)
                if not self.cmd.wait_complete(2.0):
                    return {"ok": False, "error": "could not select MDI mode"}
            for block in blocks:
                if not isinstance(block, str) or not block.strip():
                    return {"ok": False, "error": "empty MDI block"}
                self.cmd.mdi(block.strip())
            if previous != linuxcnc.MODE_MDI:
                self.pending_restore = previous
            return {"ok": True}
        except Exception as exc:
            return {"ok": False, "error": str(exc)}

    def load_program(self, command):
        path = command.get("path")
        if not path or not os.path.isfile(path):
            return {"ok": False, "error": "program file does not exist"}
        self.stat.poll()
        if int(getattr(self.stat, "interp_state", 1)) != linuxcnc.INTERP_IDLE:
            return {"ok": False, "error": "interpreter is not idle"}
        try:
            self.cmd.program_open(path)
            self.cmd.wait_complete(2.0)
            self.last_program_path = path
            try:
                with open(path, "r", encoding="utf-8", errors="replace") as handle:
                    self.last_program_text = handle.read()
            except OSError:
                self.last_program_text = ""
            return {"ok": True}
        except Exception as exc:
            return {"ok": False, "error": str(exc)}

    def set_work_offset(self, command):
        index = int(command.get("index", 1))
        values = command.get("values", {})
        rotation = command.get("rotation")
        error = self.require("mdi")
        if error:
            return {"ok": False, "error": error}
        parts = [f"G10 L2 P{index}"]
        for axis, value in values.items():
            parts.append(f"{axis}{float(value):.6f}")
        if rotation is not None:
            parts.append(f"R{float(rotation):.6f}")
        return self.issue({"origin": "write", "blocks": [" ".join(parts)]})

    def set_g92(self, command):
        error = self.require("mdi")
        if error:
            return {"ok": False, "error": error}
        parts = ["G92"]
        for axis, value in command.get("values", {}).items():
            parts.append(f"{axis}{float(value):.6f}")
        return self.issue({"origin": "write", "blocks": [" ".join(parts)]})

    def set_current_tool(self, command):
        tool = int(command.get("tool", 0))
        return self.issue({"origin": "tool-change", "blocks": [f"M61 Q{tool}"]})

    def issue_tool_change(self, command):
        tool = int(command.get("tool", 0))
        return self.issue({"origin": "tool-change", "blocks": [f"T{tool} M6"]})

    def load_tool_table(self, command):
        try:
            self.cmd.load_tool_table()
            return {"ok": True}
        except Exception as exc:
            return {"ok": False, "error": str(exc)}

    def reset_probe_cal(self, command):
        return self.issue({"origin": "probe", "blocks": ["o<probe_cal_reset> call"]})

    # -------------------------------------------------------------------- loop
    def poll_restore(self):
        if self.pending_restore is None:
            return
        self.stat.poll()
        if int(getattr(self.stat, "interp_state", 1)) == linuxcnc.INTERP_IDLE and not getattr(self.stat, "probing", False):
            try:
                self.cmd.mode(self.pending_restore)
            except Exception:
                pass
            self.pending_restore = None

    def run(self):
        # Announce readiness after the first snapshot.
        emit({"type": "hello", "pid": os.getpid(), "ini": self.ini_path})
        last_status = 0.0
        while True:
            self.poll_restore()
            now = time.time()
            if now - last_status >= 1.0 / STATUS_HZ:
                last_status = now
                try:
                    emit(self.snapshot())
                except Exception:
                    emit({"type": "error", "error": traceback.format_exc()})
            readable, _, _ = select.select([sys.stdin], [], [], 0.02)
            if not readable:
                continue
            line = sys.stdin.readline()
            if not line:
                break
            try:
                command = json.loads(line)
            except Exception:
                emit({"type": "result", "id": None, "ok": False, "error": "invalid JSON"})
                continue
            command_id = command.get("id")
            kind = command.get("cmd")
            try:
                if kind == "issue_mdi":
                    result = self.issue(command)
                elif kind == "load_program":
                    result = self.load_program(command)
                elif kind == "set_work_offset":
                    result = self.set_work_offset(command)
                elif kind == "set_g92":
                    result = self.set_g92(command)
                elif kind == "set_current_tool":
                    result = self.set_current_tool(command)
                elif kind == "issue_tool_change":
                    result = self.issue_tool_change(command)
                elif kind == "load_tool_table":
                    result = self.load_tool_table(command)
                elif kind == "reset_probe_cal":
                    result = self.reset_probe_cal(command)
                elif kind == "shutdown":
                    emit({"type": "result", "id": command_id, "ok": True})
                    break
                else:
                    result = {"ok": False, "error": f"unknown command {kind!r}"}
            except Exception:
                result = {"ok": False, "error": traceback.format_exc()}
            result["type"] = "result"
            result["id"] = command_id
            emit(result)


def selftest():
    """Pure-parsing checks that need no LinuxCNC session.

    `python3 linc_bridge.py --selftest`
    """
    cases = {
        "XYZ": ["X", "Y", "Z"],
        "X Y Z": ["X", "Y", "Z"],
        "X,Y,Z": ["X", "Y", "Z"],
        "XY Z": ["X", "Y", "Z"],
        "x y z": ["X", "Y", "Z"],
        "X Z": ["X", "Z"],
        "": ["X", "Y", "Z"],
    }
    for raw, expected in cases.items():
        got = parse_coordinates(raw)
        assert got == expected, f"parse_coordinates({raw!r}) = {got!r}, expected {expected!r}"
    print(f"linc_bridge selftest: {len(cases)} coordinate cases OK")


if __name__ == "__main__":
    if "--selftest" in sys.argv:
        selftest()
        raise SystemExit(0)
    try:
        Bridge().run()
    except KeyboardInterrupt:
        pass
    except Exception:
        emit({"type": "fatal", "error": traceback.format_exc()})
        raise
