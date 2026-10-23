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

def spindle_actual_rpm(spindle, feedback_rps):
    """True spindle RPM from the configured feedback pin.

    `spindle.0.speed-in` is the *actual* speed feedback in rotations per
    second (LinuxCNC core-components.adoc), while `spindle.speed` from status
    is the programmed S word. Fall back to the programmed speed scaled by the
    override when no feedback pin is connected, so the display still responds
    to the override on a machine without an encoder.
    """
    if feedback_rps is not None:
        return abs(float(feedback_rps)) * 60.0
    if not spindle.get("enabled"):
        return 0.0
    return abs(float(spindle.get("speed", 0.0))) * float(spindle.get("override", 1.0))


def feed_word(s, linear_units):
    """The F word in machine linear units per minute.

    LinuxCNC's `settings[1]` is the programmed F word in *program* units: an
    inch program on a millimetre machine must be converted before the HMI
    shows it as mm/min. `status.feedrate` is the override factor, not a rate.
    """
    settings = list(getattr(s, "settings", ()) or ())
    word = float(settings[1]) if len(settings) > 1 else 0.0
    gcodes = [str(code).upper() for code in (getattr(s, "gcodes", ()) or ())]
    program_inch = "G20" in gcodes and "G21" not in gcodes
    machine_inch = str(linear_units).strip().lower().startswith("in")
    if program_inch and not machine_inch:
        word *= 25.4
    elif not program_inch and machine_inch:
        word /= 25.4
    return word


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
        # `stat.ini_filename` is empty until the status has been polled once, so
        # reading it straight from the constructor gave an empty path, an empty
        # config and therefore no `PARAMETER_FILE` — which is why offsets could
        # not be read from or written to the file.
        try:
            self.stat.poll()
        except Exception:
            pass
        ini = self.stat.ini_filename or os.environ.get("INI_FILE_NAME", "")
        self.ini_path = os.path.realpath(ini) if ini else ""
        # `strict=False`: a duplicate option in the INI made ConfigParser raise,
        # the exception below was swallowed, and the whole config came back empty
        # — no `PARAMETER_FILE`, no `TOOL_TABLE`, fallback axes. LinuxCNC itself
        # accepts these files (last value wins), so the bridge must too.
        self.config = configparser.ConfigParser(
            inline_comment_prefixes=("#", ";"), strict=False
        )
        try:
            self.config.read(self.ini_path)
        except Exception as error:
            print(f"linc bridge: could not read {self.ini_path}: {error}", file=sys.stderr)
        # `[TRAJ] COORDINATES` is written both as `X Y Z` and as the trivkins
        # shorthand `XYZ`. A bare `.split()` turns `XYZ` into the single axis
        # name "XYZ", so every `X`/`Y`/`Z` lookup missed and the HMI showed a
        # frozen 0.000 DRO with a tool marker stuck at the origin.
        self.axes = parse_coordinates(
            self.config.get("TRAJ", "COORDINATES", fallback="XYZ")
        )
        self.linear_units = self.config.get("TRAJ", "LINEAR_UNITS", fallback="mm")
        self.spindle_speed_pin = os.environ.get(
            "LINC_SPINDLE_SPEED_PIN", "spindle.0.speed-in"
        )
        self.angular_units = self.config.get("TRAJ", "ANGULAR_UNITS", fallback="degree")
        self.tool_table = self.config.get("EMCIO", "TOOL_TABLE", fallback="")
        self.param_file = self.config.get("RS274NGC", "PARAMETER_FILE", fallback="")
        if not self.param_file:
            print(
                "linc bridge: no [RS274NGC] PARAMETER_FILE in %r; offsets will not persist"
                % self.ini_path,
                file=sys.stderr,
            )
        self.parameters = self.load_parameters()
        self.program_prefix = self.config.get("DISPLAY", "PROGRAM_PREFIX", fallback=".")
        self.panel_connected_last = None
        self.last_proghome = [False, False, False, False]
        self.last_panel_heartbeat = None
        self.last_panel_heartbeat_time = 0.0
        self.pending_restore = None
        self.pending_restore_at = 0.0
        # Tool whose `G43 Hn` is still to be applied after its M6 completes.
        self.pending_height_comp = None
        self.pending_height_comp_at = 0.0
        self.last_program_path = None
        self.last_program_text = None
        self.last_program_mtime = None
        self.alarms = []
        # Offsets this session has written, so the table shows its own writes
        # before the parameter file is rewritten at exit.
        self.offset_overrides = {}
        self.rotation_overrides = {}
        # `Interp::init()` restores the parameter file but the task only drains
        # the queued SET_G5X_OFFSET while it executes a block, so motion starts
        # with zero offsets until the first MDI/program. Push them once the
        # machine is on (see sync_work_offsets).
        self.offsets_synced = False
        self.offset_sync_attempts = 0
        self.offset_sync_next = 0.0
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
                    # LinuxCNC's operator text arrives wrapped in, and sometimes
                    # separated by, newlines; they rendered as blank lines
                    # between entries in the GUI.
                    message = " ".join(str(text).split())
                    if not message:
                        continue
                    with threading.Lock():
                        self.alarms.append({"kind": severity, "text": message, "time": time.time()})
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
                        self.pending_restore_at = time.monotonic()
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
    def parameter_path(self):
        if not self.param_file:
            return None
        path = self.param_file
        if not os.path.isabs(path):
            # Resolve against the INI directory, not the bridge's own CWD: the
            # linuxcnc launcher `cd`s to the INI directory before starting the
            # task and the display, so the interpreter opens a relative
            # PARAMETER_FILE there. The bridge may be started by an HMI whose CWD
            # is anywhere (a test harness, a desktop launcher), and resolving
            # against that CWD read and wrote a different file than the
            # controller did.
            path = os.path.join(os.path.dirname(self.ini_path), path)
        return path

    def load_parameters(self):
        """Read the parameter file once, at startup.

        LinuxCNC rewrites this file when it exits, so re-reading it every poll
        gained nothing and cost a file parse per cycle; the live values it is
        missing come from the status instead.
        """
        values = {}
        path = self.parameter_path()
        if not path:
            return values
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

    def report_parameter_state(self):
        """Say what the parameter file holds against the live controller.

        A value that is written and then lost between sessions is otherwise
        invisible: the file is only read once, so the first thing to compare is
        the file against the status.
        """
        path = self.parameter_path()
        if not path:
            return
        try:
            self.stat.poll()
            live = list(getattr(self.stat, "g5x_offset", ()) or ())
            file_x = self.parameters.get(5221)
            live_x = live[0] if live else None
            if file_x is not None and live_x is not None and abs(file_x - live_x) > 1e-6:
                # Expected before the machine is on: the interpreter restores
                # the file at startup but only queues the offset, and the task
                # delivers that queue once motion is idle (see
                # sync_work_offsets, which does it after power on).
                print(
                    "linc bridge: G54 X file %.4f, controller %.4f until the "
                    "machine is powered on (%s)" % (file_x, live_x, path),
                    file=sys.stderr,
                )
            else:
                print(
                    "linc bridge: G54 X file %r controller %r (%s)"
                    % (file_x, live_x, path),
                    file=sys.stderr,
                )
        except Exception as error:
            print(f"linc bridge: could not report parameters: {error}", file=sys.stderr)

    def read_parameters(self):
        return self.parameters

    def capture_live_offsets(self):
        """Record the active system's offsets from the live status.

        The file is only useful if it matches the controller, and the controller
        is the authority on the active system: `G10 L20` computes its value, and
        a program can move G54 without telling us.
        """
        # Until sync_work_offsets has delivered the stored offsets, the
        # controller reports zeros for every system even though the file holds
        # real values. Treating those zeros as authoritative would erase the
        # saved offsets whenever a program is loaded during that window.
        if not self.offsets_synced:
            return
        try:
            self.stat.poll()
            index = int(getattr(self.stat, "g5x_index", 1) or 1)
            if not (1 <= index <= 9):
                return
            live = list(getattr(self.stat, "g5x_offset", ()) or ())
            values = {}
            for axis_index, axis in enumerate(self.axes):
                if axis_index < len(live):
                    values[axis] = live[axis_index]
            if values:
                self.offset_overrides.setdefault(index, {}).update(values)
            rotation = getattr(self.stat, "rotation_xy", None)
            if rotation is not None:
                self.rotation_overrides[index] = float(rotation)
        except Exception:
            pass

    def save_parameters(self):
        """Write the offsets this session changed back to the parameter file.

        LinuxCNC writes the whole file when it exits; doing it here as well means
        the values survive a session that is killed rather than closed, and the
        rewrite is limited to the keys this bridge owns.
        """
        path = self.parameter_path()
        if not path:
            self.last_save_error = "no [RS274NGC] PARAMETER_FILE in the INI"
            return False
        if not os.path.exists(path):
            self.last_save_error = "%s does not exist" % path
            return False
        updates = {}
        for index, values in self.offset_overrides.items():
            base = 5220 + (index - 1) * 20
            for axis_index, axis in enumerate(self.axes):
                if axis in values:
                    updates[base + 1 + axis_index] = values[axis]
            if index in self.rotation_overrides:
                # The row for a system is 5220 + (index-1)*20 .. +10: axis words
                # start at +1 and the rotation is +10. Writing the rotation at
                # +0 overwrote parameter 5220, which is the *active* system
                # index, so every write reset G54..G59 to "system 0".
                updates[base + 10] = self.rotation_overrides[index]
        if not updates:
            self.last_save_error = None
            return True
        try:
            with open(path, "r", encoding="utf-8", errors="replace") as handle:
                lines = handle.read().splitlines()
            written = set()
            for position, line in enumerate(lines):
                parts = line.split()
                if len(parts) != 2:
                    continue
                try:
                    key = int(parts[0])
                except ValueError:
                    continue
                if key in updates:
                    lines[position] = "%d\t%.6f" % (key, updates[key])
                    written.add(key)
            # Insert a missing key in numeric order: `restore_parameters` reads
            # the file expecting strictly ascending keys and fails the whole
            # restore otherwise, which leaves the interpreter with zeros.
            for key in sorted(updates):
                if key in written:
                    continue
                position = len(lines)
                for candidate, line in enumerate(lines):
                    parts = line.split()
                    if len(parts) != 2:
                        continue
                    try:
                        other = int(parts[0])
                    except ValueError:
                        continue
                    if other > key:
                        position = candidate
                        break
                lines.insert(position, "%d\t%.6f" % (key, updates[key]))
                written.add(key)
            with open(path, "w", encoding="utf-8") as handle:
                handle.write("\n".join(lines) + "\n")
        except OSError as exc:
            self.last_save_error = "could not write %s: %s" % (path, exc)
            return False
        # Read the file back: a write that is later overwritten, or one that
        # never landed, must not be reported as success.
        try:
            with open(path, "r", encoding="utf-8", errors="replace") as handle:
                written = {}
                for line in handle:
                    parts = line.split()
                    if len(parts) == 2:
                        try:
                            written[int(parts[0])] = float(parts[1])
                        except ValueError:
                            pass
            wrong = [
                "%d=%r (wrote %r)" % (key, written.get(key), value)
                for key, value in updates.items()
                if written.get(key) is None or abs(written[key] - value) > 1e-6
            ]
            if wrong:
                self.last_save_error = "%s did not keep: %s" % (path, ", ".join(wrong))
                return False
        except OSError as exc:
            self.last_save_error = "could not read back %s: %s" % (path, exc)
            return False
        self.last_save_error = None
        return True

    def read_offsets(self, params):
        """The nine work offsets (G54-G59.3), live where the controller can tell us.

        The parameter file on disk is only written when LinuxCNC exits, so
        reading it alone showed stale numbers after a `G10` write — the DRO moved
        and the table did not. `stat.g5x_offset` is the live value, but only for
        the active system, so that one is overlaid on the file values, and writes
        made through this bridge are remembered so their own rows stay current.
        """
        offsets = []
        try:
            self.stat.poll()
        except Exception:
            pass
        active = int(getattr(self.stat, "g5x_index", 1) or 1)
        live = list(getattr(self.stat, "g5x_offset", ()) or ())
        live_rotation = getattr(self.stat, "rotation_xy", None)
        for index in range(1, 10):
            base = 5220 + (index - 1) * 20
            values = {}
            for axis_index, axis in enumerate(self.axes):
                values[axis] = params.get(base + 1 + axis_index, 0.0)
            rotation = params.get(base + 10, 0.0)
            # Overlay what this session has written, then the active system from
            # the live status, which is the most trustworthy of the three.
            remembered = self.offset_overrides.get(index)
            if remembered:
                for axis, value in remembered.items():
                    values[axis] = value
                rotation = self.rotation_overrides.get(index, rotation)
            if index == active:
                for axis_index, axis in enumerate(self.axes):
                    if axis_index < len(live):
                        values[axis] = live[axis_index]
                if live_rotation is not None:
                    rotation = float(live_rotation)
            offsets.append(
                {
                    "index": index,
                    "values": values,
                    "rotation": rotation,
                }
            )
        # G92 offsets live in 5211..5219 (5210 is the "a G92 offset is active"
        # flag), not 5201.., which belong to the G54 row.
        g92 = {axis: params.get(5211 + i, 0.0) for i, axis in enumerate(self.axes)}
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
            # RAPID on the panel raises the continuous-jog ceiling.
            "rapid": bool(pin_bool("cnc_panel.rapid", False)),
            # The controller's limits override, so the HMI can show it.
            "override_limits": bool(pin_bool("halui.joint.0.override-limits", False)),
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
        tool = list(getattr(s, "tool_offset", ()) or ())
        positions = []
        for index, axis in enumerate(self.axes[:9]):
            # `actual_position` is in machine coordinates; the work position is
            # what is left after the offsets are taken off. Adding them instead
            # meant setting a work zero changed nothing on screen, because the
            # machine position moved by exactly the offset that was meant to
            # cancel it.
            mach = actual[index] if index < len(actual) else 0.0
            work = (
                mach
                - (g5x[index] if index < len(g5x) else 0.0)
                - (g92_offset[index] if index < len(g92_offset) else 0.0)
                - (tool[index] if index < len(tool) else 0.0)
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
            feedback = (
                pin(self.spindle_speed_pin)
                if index == 0 and self.spindle_speed_pin
                else None
            )
            spindles.append(
                {
                    "index": index,
                    "speed": float(spindle.get("speed", 0.0)),
                    "actual": spindle_actual_rpm(spindle, feedback),
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
                "feed": feed_word(s, self.linear_units),
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
                # `current_line` is the interpreter's line and runs ahead of the
                # tool by the look-ahead queue; `motion_line` is the line motion
                # is executing, which is the one a pane can highlight while a
                # program runs.
                "motion_line": int(getattr(s, "motion_line", 0) or 0),
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
        if origin == "run" and panel["mode"].get("mode") != "auto":
            return "panel mode is not AUTO"
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
                self.pending_restore_at = time.monotonic()
            return {"ok": True}
        except Exception as exc:
            return {"ok": False, "error": str(exc)}

    def abort(self, command):
        """Stop the running program, like the panel's K4 (halui.program.stop)."""
        try:
            self.cmd.abort()
        except Exception as error:
            return {"ok": False, "error": str(error)}
        return {"ok": True}

    def load_program(self, command):
        path = command.get("path")
        if not path or not os.path.isfile(path):
            return {"ok": False, "error": "program file does not exist"}
        self.stat.poll()
        if int(getattr(self.stat, "interp_state", 1)) != linuxcnc.INTERP_IDLE:
            return {"ok": False, "error": "interpreter is not idle"}
        try:
            # `program_open` re-initialises the interpreter, and that re-reads the
            # parameter file. Without this the file's old values replace whatever
            # the operator has set, which looked like "the offset is discarded at
            # program load".
            self.capture_live_offsets()
            self.save_parameters()
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
        # `issue` switches to MDI for the block and restores the previous mode,
        # so requiring the panel to already be in MDI rejected every write from
        # the OFFSETS page — the table then showed the typed draft and lost it
        # when the snapshot rebuilt the row.
        error = self.require("write")
        if error:
            return {"ok": False, "error": error}
        parts = [f"G10 L2 P{index}"]
        for axis, value in values.items():
            parts.append(f"{axis}{float(value):.6f}")
        if rotation is not None:
            parts.append(f"R{float(rotation):.6f}")
        result = self.issue({"origin": "write", "blocks": [" ".join(parts)]})
        if result.get("ok"):
            # The block is asynchronous and its errors arrive as operator
            # messages, so "ok" only means it was sent. Compare what the
            # controller holds afterwards, and say so when it differs — a silent
            # mismatch looked like a write that worked and a file that would not
            # persist.
            try:
                self.cmd.wait_complete(2.0)
                self.stat.poll()
            except Exception:
                pass
            active = int(getattr(self.stat, "g5x_index", 1) or 1)
            if active == index:
                live = list(getattr(self.stat, "g5x_offset", ()) or ())
                kept = {}
                for axis_index, axis in enumerate(self.axes):
                    if axis in values and axis_index < len(live):
                        kept[axis] = live[axis_index]
                wrong = [
                    "%s=%.4f (asked %.4f)" % (axis, kept[axis], float(values[axis]))
                    for axis in kept
                    if abs(kept[axis] - float(values[axis])) > 1e-4
                ]
                if wrong:
                    return {
                        "ok": False,
                        "error": "controller did not take the offset: " + ", ".join(wrong),
                    }
            self.offset_overrides.setdefault(index, {}).update(
                {str(axis).upper(): float(value) for axis, value in values.items()}
            )
            if rotation is not None:
                self.rotation_overrides[index] = float(rotation)
            # Persist at the change, so a session that is killed loses nothing.
            if not self.save_parameters():
                return {"ok": False, "error": self.last_save_error or "could not persist the offset"}
        return result

    def run_from_line(self, command):
        """Start the loaded program at a line, skipping what comes before it.

        Run only from an idle interpreter: `AUTO_RUN` on a paused program would
        resume it instead of honouring the line, and the lines before the start
        would not be skipped. The panel must be in AUTO, so the HMI never
        starts a program the panel has not enabled.
        """
        line = int(command.get("line", 0))
        if line < 1:
            return {"ok": False, "error": "line must be 1 or greater"}
        error = self.require("run")
        if error:
            return {"ok": False, "error": error}
        try:
            if int(getattr(self.stat, "task_mode", 0)) != linuxcnc.MODE_AUTO:
                self.cmd.mode(linuxcnc.MODE_AUTO)
                if not self.cmd.wait_complete(2.0):
                    return {"ok": False, "error": "could not select AUTO mode"}
            self.cmd.auto(linuxcnc.AUTO_RUN, line)
            return {"ok": True}
        except Exception as exc:
            return {"ok": False, "error": str(exc)}

    def zero_work_offset(self, command):
        """Touch off: make the current position read zero for the given axes.

        Uses `G10 L20`, which sets the offset to a value calculated to make the
        current coordinates read as given, rather than `G10 L2` which sets the
        offset directly. Sending zero is the standard "set zero here".
        """
        index = int(command.get("index", 1))
        axes = [str(axis).upper() for axis in command.get("axes", [])]
        if not axes:
            return {"ok": False, "error": "no axes selected"}
        return self.apply_work_position(index, {axis: 0.0 for axis in axes})

    def set_work_position(self, command):
        """Make the current position read the given work coordinates.

        This is the DRO entry path: the operator types `X-12.5` and the position
        must read that, so the offset is left to `G10 L20` exactly as a touch
        off is; only the target differs.
        """
        index = int(command.get("index", 1))
        values = {}
        for axis, value in (command.get("values") or {}).items():
            try:
                values[str(axis).upper()] = float(value)
            except (TypeError, ValueError):
                return {"ok": False, "error": "invalid value for axis %s" % axis}
        if not values:
            return {"ok": False, "error": "no axes given"}
        return self.apply_work_position(index, values)

    def apply_work_position(self, index, values):
        """`G10 L20 P<index> <axis><value>`: the position reads `values`.

        The controller computes the offset from the position it holds, so the
        HMI never has to derive one from a snapshot that may be behind.
        """
        parts = " ".join("%s%.6f" % (axis, value) for axis, value in values.items())
        result = self.issue({"origin": "write", "blocks": ["G10 L20 P%d %s" % (index, parts)]})
        if not result.get("ok"):
            return result
        # The block is asynchronous: wait before reading the value back, or the
        # old offset is what gets persisted.
        try:
            self.cmd.wait_complete(2.0)
        except Exception:
            pass
        try:
            self.stat.poll()
            live = list(getattr(self.stat, "g5x_offset", ()) or ())
            merged = {}
            for axis_index, axis in enumerate(self.axes):
                if axis in values and axis_index < len(live):
                    merged[axis] = live[axis_index]
            if merged:
                # Merge: a write changes only the axes it names, and the rest of
                # that row must keep what was recorded before.
                self.offset_overrides.setdefault(index, {}).update(merged)
            rotation = getattr(self.stat, "rotation_xy", None)
            if rotation is not None:
                self.rotation_overrides[index] = float(rotation)
            if not self.save_parameters():
                return {
                    "ok": False,
                    "error": self.last_save_error or "could not persist the offset",
                }
        except Exception as exc:
            return {"ok": False, "error": "position write could not be persisted: %s" % exc}
        return result

    def set_g92(self, command):
        error = self.require("write")
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

    def load_tool(self, command):
        """Load a tool; G43 Hn follows once the change has completed.

        Two `cmd.mdi()` calls in a row race the interpreter's MDI slot, so the
        change is issued alone and `poll_height_comp` applies the offset after
        the handshake. Reload first: the HMI reads the table file, the
        controller holds the copy it loaded at startup, and a `T` word for a
        tool only in the file fails as "not in the tool table".
        """
        tool = int(command.get("tool", 0))
        try:
            self.cmd.load_tool_table()
        except Exception:
            pass
        result = self.issue({"origin": "tool-change", "blocks": [f"T{tool} M6"]})
        if result.get("ok"):
            self.pending_height_comp = tool
            self.pending_height_comp_at = time.monotonic()
        return result

    def toggle_limits_override(self, command):
        """Accept the limit switches that are tripped right now.

        Motion clears its enable request when a joint sits on a switch, so a
        machine switched on while on a limit never comes up and cannot be jogged
        off it. The override is the way out, but setting it alone is not enough:
        the power-on edge has already happened, so nothing asks motion to enable
        again. This re-requests power after the override takes effect.
        """
        try:
            self.cmd.override_limits()
            self.cmd.wait_complete(1.0)
            if int(getattr(self.stat, "task_state", 0)) != linuxcnc.STATE_ON:
                self.stat.poll()
                if int(getattr(self.stat, "task_state", 0)) != linuxcnc.STATE_ON:
                    self.cmd.state(linuxcnc.STATE_ON)
            return {"ok": True}
        except Exception as exc:
            return {"ok": False, "error": str(exc)}

    def load_tool_table(self, command):
        try:
            self.cmd.load_tool_table()
        except Exception as exc:
            return {"ok": False, "error": str(exc)}
        # Re-apply the length compensation when the edited row is the tool in
        # the spindle: G43 reads the table as it executes, so a reload alone
        # leaves the old height in effect for the current tool.
        reapply = int(command.get("reapply", 0) or 0)
        if reapply > 0:
            try:
                self.stat.poll()
                current = int(getattr(self.stat, "tool_in_spindle", 0) or 0)
            except Exception:
                current = 0
            if current == reapply:
                return self.issue(
                    {"origin": "tool-change", "blocks": [f"G43 H{reapply}"]}
                )
        return {"ok": True}

    def reset_probe_cal(self, command):
        return self.issue({"origin": "probe", "blocks": ["o<probe_cal_reset> call"]})

    # -------------------------------------------------------------------- loop
    def poll_restore(self):
        if self.pending_restore is None:
            return
        # Give the queued MDI block time to start before looking at the
        # interpreter: for a few milliseconds after `cmd.mdi()` the interpreter
        # still reports IDLE, and restoring the panel's mode then aborts the
        # block — a `T` word that appeared queued but never ran.
        if time.monotonic() - self.pending_restore_at < 0.4:
            return
        self.stat.poll()
        if int(getattr(self.stat, "interp_state", 1)) == linuxcnc.INTERP_IDLE and not getattr(self.stat, "probing", False):
            try:
                self.cmd.mode(self.pending_restore)
            except Exception:
                pass
            self.pending_restore = None

    def poll_height_comp(self):
        """Apply the pending `G43 Hn` once the tool is in and the task is idle.

        A short grace period covers a change to the tool already in the
        spindle, where every state looks finished before the M6 has run.
        """
        if self.pending_height_comp is None:
            return
        if time.monotonic() - self.pending_height_comp_at < 1.5:
            return
        tool = self.pending_height_comp
        self.stat.poll()
        if int(getattr(self.stat, "tool_in_spindle", 0) or 0) != tool:
            return
        if int(getattr(self.stat, "interp_state", 1)) != linuxcnc.INTERP_IDLE:
            return
        if bool(pin_bool("iocontrol.0.tool-change", False)):
            return
        self.pending_height_comp = None
        self.issue({"origin": "tool-change", "blocks": [f"G43 H{tool}"]})

    def offsets_match_stored(self):
        """Does the live controller hold the parameter file's active offsets?"""
        try:
            self.stat.poll()
        except Exception:
            return False
        try:
            index = int(round(self.parameters.get(5220, 1.0) or 1.0))
        except Exception:
            index = 1
        index = min(max(index, 1), 9)
        base = 5200 + index * 20
        live = list(getattr(self.stat, "g5x_offset", ()) or ())
        compared = 0
        for axis_index in range(len(self.axes)):
            stored = self.parameters.get(base + 1 + axis_index)
            if stored is None:
                continue
            if axis_index >= len(live):
                return False
            if abs(live[axis_index] - stored) > 1e-6:
                return False
            compared += 1
        # Nothing in the file for this system: there is nothing to apply.
        return compared > 0

    def sync_work_offsets(self):
        """Make motion hold the parameter file's offsets after a start.

        LinuxCNC's interpreter restores the parameter file when it initializes
        (`Interp::init`), but the restored G5X offset is only queued as a
        trajectory message; the task drains that queue when it next runs
        something, so a fresh session can sit there with motion at zero and the
        DRO/offset table showing zeros for the session that was saved.

        `reset_interpreter()` re-runs `EMC_TASK_PLAN_INIT`, i.e. the same
        restore path, while the task is running, so the queue drains
        immediately; if that does not take, selecting the stored coordinate
        system with an MDI block drains it too. Either way the result is read
        back before it is believed.
        """
        if self.offsets_synced or self.offset_sync_attempts >= 30:
            return
        if not self.param_file:
            return
        now = time.time()
        if now < self.offset_sync_next:
            return
        self.offset_sync_next = now + 2.0
        try:
            self.stat.poll()
        except Exception:
            return
        if bool(getattr(self.stat, "estop", 0)):
            return
        if int(getattr(self.stat, "task_state", 0)) != linuxcnc.STATE_ON:
            return
        if int(getattr(self.stat, "interp_state", 1)) != linuxcnc.INTERP_IDLE:
            return
        index = min(max(int(round(self.parameters.get(5220, 1.0) or 1.0)), 1), 9)
        base = 5200 + index * 20
        if not any(base + 1 + i in self.parameters for i in range(len(self.axes))):
            # The file holds no offsets for the active system; nothing to apply.
            self.offsets_synced = True
            return
        self.offset_sync_attempts += 1

        # 1. Re-initialize the interpreter: it re-reads the file and re-queues
        #    the offset message, which the idle task loop then delivers.
        try:
            self.cmd.reset_interpreter()
            self.cmd.wait_complete(2.0)
            time.sleep(0.3)
        except Exception as error:
            print("linc bridge: could not re-initialize the interpreter: %s"
                  % error, file=sys.stderr)
        if self.offsets_match_stored():
            self.offsets_synced = True
            print("linc bridge: stored offsets applied by the interpreter restart",
                  file=sys.stderr)
            return

        # 2. Fallback: execute the stored coordinate system.
        code = {1: "G54", 2: "G55", 3: "G56", 4: "G57", 5: "G58",
                6: "G59", 7: "G59.1", 8: "G59.2", 9: "G59.3"}[index]
        try:
            previous = int(getattr(self.stat, "task_mode", linuxcnc.MODE_MANUAL)
                           or linuxcnc.MODE_MANUAL)
            if previous != linuxcnc.MODE_MDI:
                self.cmd.mode(linuxcnc.MODE_MDI)
                if not self.cmd.wait_complete(2.0):
                    print("linc bridge: could not enter MDI to apply %s" % code,
                          file=sys.stderr)
                    return
            self.cmd.mdi(code)
            self.cmd.wait_complete(2.0)
            if previous != linuxcnc.MODE_MDI:
                self.pending_restore = previous
                self.pending_restore_at = time.monotonic()
            time.sleep(0.3)
        except Exception as error:
            print("linc bridge: could not apply stored %s offsets: %s"
                  % (code, error), file=sys.stderr)
            return
        if self.offsets_match_stored():
            self.offsets_synced = True
            print("linc bridge: stored offsets applied by selecting %s" % code,
                  file=sys.stderr)
        elif self.offset_sync_attempts == 1:
            live = list(getattr(self.stat, "g5x_offset", ()) or ())
            print("linc bridge: stored offsets not applied yet — controller %s, file %s"
                  % (["%.4f" % v for v in live[:3]],
                     ["%.4f" % self.parameters.get(5221 + i, 0.0)
                      for i in range(len(self.axes))]),
                  file=sys.stderr)

    def run(self):
        # Announce readiness after the first snapshot.
        self.report_parameter_state()
        emit({"type": "hello", "pid": os.getpid(), "ini": self.ini_path})
        last_status = 0.0
        while True:
            self.poll_restore()
            self.poll_height_comp()
            self.sync_work_offsets()
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
                elif kind == "abort":
                    result = self.abort(command)
                elif kind == "load_program":
                    result = self.load_program(command)
                elif kind == "set_work_offset":
                    result = self.set_work_offset(command)
                elif kind == "zero_work_offset":
                    result = self.zero_work_offset(command)
                elif kind == "set_work_position":
                    result = self.set_work_position(command)
                elif kind == "run_from_line":
                    result = self.run_from_line(command)
                elif kind == "set_g92":
                    result = self.set_g92(command)
                elif kind == "set_current_tool":
                    result = self.set_current_tool(command)
                elif kind == "issue_tool_change":
                    result = self.issue_tool_change(command)
                elif kind == "load_tool":
                    result = self.load_tool(command)
                elif kind == "toggle_limits_override":
                    result = self.toggle_limits_override(command)
                elif kind == "load_tool_table":
                    result = self.load_tool_table(command)
                elif kind == "reset_probe_cal":
                    result = self.reset_probe_cal(command)
                elif kind == "shutdown":
                    self.save_parameters()
                    emit({"type": "result", "id": command_id, "ok": True})
                    break
                else:
                    result = {"ok": False, "error": f"unknown command {kind!r}"}
            except Exception:
                result = {"ok": False, "error": traceback.format_exc()}
            result["type"] = "result"
            result["id"] = command_id
            emit(result)


class _FakeStat:
    """Minimal `linuxcnc.stat` stand-in for the offset-sync logic."""

    def __init__(self, task_state):
        self.task_state = task_state
        self.estop = 0
        self.interp_state = linuxcnc.INTERP_IDLE
        self.task_mode = linuxcnc.MODE_MANUAL
        self.g5x_offset = [0.0] * 9
        self.homed = [1, 1, 1]

    def poll(self):
        return True


class _FakeCommand:
    """Records what the sync sends and optionally applies the stored offsets."""

    def __init__(self, stat, values, apply_reset, apply_mdi):
        self.stat = stat
        self.values = values
        self.apply_reset = apply_reset
        self.apply_mdi = apply_mdi
        self.modes = []
        self.blocks = []
        self.resets = 0

    def _apply(self):
        self.stat.g5x_offset = list(self.values) + [0.0] * 6

    def mode(self, mode):
        self.modes.append(mode)

    def wait_complete(self, timeout=None):
        return True

    def reset_interpreter(self):
        self.resets += 1
        if self.apply_reset:
            self._apply()

    def mdi(self, block):
        self.blocks.append(block)
        if self.apply_mdi:
            self._apply()


def _fake_bridge(task_state, values, apply_reset, apply_mdi):
    bridge = Bridge.__new__(Bridge)
    bridge.stat = _FakeStat(task_state)
    bridge.axes = ["X", "Y", "Z"]
    bridge.parameters = dict(values)
    bridge.param_file = "sim.var"
    bridge.ini_path = "/tmp/sim.ini"
    bridge.offsets_synced = False
    bridge.offset_sync_attempts = 0
    bridge.offset_sync_next = 0.0
    bridge.pending_restore = None
    bridge.pending_restore_at = 0.0
    index = min(max(int(round(values.get(5220, 1.0) or 1.0)), 1), 9)
    base = 5200 + index * 20
    bridge.cmd = _FakeCommand(
        bridge.stat,
        [values.get(base + 1 + i, 0.0) for i in range(3)],
        apply_reset,
        apply_mdi,
    )
    return bridge


def check_spindle_actual():
    spindle = {"speed": 4200.0, "override": 0.5, "enabled": 1}
    # Feedback is rotations per second, so 70 rps is 4200 rpm.
    assert abs(spindle_actual_rpm(spindle, 70.0) - 4200.0) < 1e-6
    assert abs(spindle_actual_rpm(spindle, -70.0) - 4200.0) < 1e-6
    # Without feedback the programmed speed is scaled by the override.
    assert abs(spindle_actual_rpm(spindle, None) - 2100.0) < 1e-6
    assert spindle_actual_rpm({"speed": 4200.0, "override": 0.5, "enabled": 0}, None) == 0.0
    print("linc_bridge selftest: spindle feedback rpm OK")


def check_feed_word():
    """The F word is a rate in program units, not the override factor."""

    class Stat:
        pass

    stat = Stat()
    stat.settings = [0.0, 700.0, 0.0, 0.0]
    stat.gcodes = ["G21", "G90"]
    assert feed_word(stat, "mm") == 700.0, feed_word(stat, "mm")
    stat.gcodes = ["G20", "G90"]
    assert abs(feed_word(stat, "mm") - 17780.0) < 1e-6, feed_word(stat, "mm")
    stat.settings = [0.0, 10.0, 0.0, 0.0]
    assert abs(feed_word(stat, "mm") - 254.0) < 1e-6, feed_word(stat, "mm")
    stat.gcodes = ["G21"]
    assert abs(feed_word(stat, "inch") - 10.0 / 25.4) < 1e-9, feed_word(stat, "inch")
    print("linc_bridge selftest: feed word units OK")


def check_offset_sync():
    """The stored offsets must reach motion once the machine is on, once only."""
    values = {5220: 1.0, 5221: -11.1, 5222: -22.2, 5223: -33.3}

    bridge = _fake_bridge(linuxcnc.STATE_ON, values, apply_reset=True, apply_mdi=False)
    bridge.sync_work_offsets()
    assert bridge.offsets_synced, "interpreter restart did not mark the sync done"
    assert bridge.cmd.resets == 1, bridge.cmd.resets
    assert bridge.cmd.blocks == [], "fell back to MDI although the restart worked"
    print("linc_bridge selftest: interpreter restart applies the stored offsets")

    bridge = _fake_bridge(linuxcnc.STATE_ON, values, apply_reset=False, apply_mdi=True)
    bridge.sync_work_offsets()
    assert bridge.offsets_synced and bridge.cmd.blocks == ["G54"], bridge.cmd.blocks
    print("linc_bridge selftest: selecting the stored WCS applies them as a fallback")

    bridge = _fake_bridge(linuxcnc.STATE_ESTOP_RESET, values, apply_reset=True, apply_mdi=True)
    bridge.sync_work_offsets()
    assert not bridge.offsets_synced and bridge.cmd.resets == 0 and not bridge.cmd.blocks
    assert bridge.offset_sync_attempts == 0, "a machine that is off used an attempt"
    print("linc_bridge selftest: no attempt while the machine is off")

    bridge = _fake_bridge(linuxcnc.STATE_ON, values, apply_reset=False, apply_mdi=False)
    real_sleep = time.sleep
    time.sleep = lambda _seconds: None
    try:
        for _ in range(40):
            bridge.offset_sync_next = 0.0
            bridge.sync_work_offsets()
    finally:
        time.sleep = real_sleep
    assert not bridge.offsets_synced, "marked done although motion kept zeros"
    assert bridge.offset_sync_attempts == 30, bridge.offset_sync_attempts
    print("linc_bridge selftest: offsets that never land are retried, then given up")

    bridge = _fake_bridge(
        linuxcnc.STATE_ON,
        {5220: 3.0, 5261: 1.0},
        apply_reset=False,
        apply_mdi=True,
    )
    bridge.sync_work_offsets()
    assert bridge.cmd.blocks == ["G56"], bridge.cmd.blocks
    print("linc_bridge selftest: the active WCS from the file is the one selected")

    bridge = _fake_bridge(linuxcnc.STATE_ON, values, apply_reset=True, apply_mdi=False)
    bridge.offset_overrides = {}
    bridge.rotation_overrides = {}
    bridge.capture_live_offsets()
    assert not bridge.offset_overrides, (
        "captured the controller's zeros before the stored offsets were applied")
    bridge.offsets_synced = True
    bridge.capture_live_offsets()
    assert bridge.offset_overrides == {1: {"X": 0.0, "Y": 0.0, "Z": 0.0}}, bridge.offset_overrides
    print("linc_bridge selftest: live offsets are captured only after the sync")


def check_work_position():
    """A DRO entry must go out as `G10 L20` with the typed value."""
    import tempfile

    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "sim.var")
        with open(path, "w", encoding="utf-8") as handle:
            handle.write("5220\t1.000000\n5221\t0.000000\n")
        bridge = _fake_bridge(linuxcnc.STATE_ON, {}, apply_reset=True, apply_mdi=False)
        bridge.param_file = path
        bridge.offset_overrides = {}
        bridge.rotation_overrides = {}
        # The panel/HAL checks belong to `issue`, which is exercised against a
        # real controller; this checks the block the DRO entry produces.
        sent = []
        bridge.issue = lambda command: (sent.extend(command["blocks"]), {"ok": True})[1]
        result = bridge.apply_work_position(1, {"X": 12.5})
        assert result.get("ok"), result
        assert sent == ["G10 L20 P1 X12.500000"], sent
        print("linc_bridge selftest: a position entry goes out as G10 L20 with its value")


def check_parameter_keys():
    """The file must keep its ascending order and its active-system index."""
    import tempfile

    with tempfile.TemporaryDirectory() as tmp:
        path = os.path.join(tmp, "sim.var")
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(
                "5161\t1.000000\n"
                "5220\t1.000000\n"
                "5221\t-54.350000\n"
                "5222\t-49.300000\n"
                "5223\t-27.150000\n"
                "5230\t0.000000\n"
            )
        bridge = _fake_bridge(linuxcnc.STATE_ON, {}, apply_reset=True, apply_mdi=False)
        bridge.param_file = path
        bridge.offset_overrides = {1: {"X": -60.5}}
        bridge.rotation_overrides = {1: 2.5}
        assert bridge.save_parameters(), bridge.last_save_error

        def values():
            out = {}
            for line in open(path, encoding="utf-8"):
                parts = line.split()
                if len(parts) == 2:
                    out[int(parts[0])] = float(parts[1])
            return out

        written = values()
        assert written[5220] == 1.0, written[5220]
        assert written[5230] == 2.5, written[5230]
        assert written[5221] == -60.5, written[5221]
        print("linc_bridge selftest: the rotation writes 5230, not the active system")

        # G54 X already present; G59 X is missing and must be inserted in order.
        bridge.offset_overrides = {6: {"X": 6.0}}
        assert bridge.save_parameters(), bridge.last_save_error
        keys = [int(line.split()[0]) for line in open(path, encoding="utf-8")]
        assert keys == sorted(keys), keys
        assert values()[5321] == 6.0, values()
        print("linc_bridge selftest: a missing key is inserted in numeric order")

        offsets, _g92 = bridge.read_offsets({5220: 1.0, 5221: -54.35, 5230: 2.5})
        assert offsets[0]["rotation"] == 2.5, offsets[0]["rotation"]
        print("linc_bridge selftest: the rotation is read from the rotation key")


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
    check_offset_sync()
    check_feed_word()
    check_spindle_actual()
    check_parameter_keys()
    check_work_position()


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
