#!/usr/bin/env python3
r"""
rio_sim.py -- userspace LinuxCNC HAL component that replaces the realtime
``rio.so`` FPGA driver (baiermill_pb/riocomp.c) with a pure-software simulation.

The machine's HAL file loads this with ``loadusr -W ./rio_sim.py`` and then uses
the ``rio.*`` pin names exactly as before, so the pin surface below is a
drop-in match for the generated driver.

Pin surface
-----------
The 182 pins in ``EXTRACTED_PINS`` were mechanically extracted from the
machine's ``riocomp.c`` (in the probe_basic checkout) with the regex::

    hal_(pin|param)_(bit|float|s32|u32)_newf\(\s*(HAL_\w+),\s*&(\(data->\w+\)),\s*comp_id,\s*"([^"]+)"


Each entry is ``(name, type, direction)`` where *type* is in
{bit,float,s32,u32} and *direction* is in {IN,OUT,IO}, mirroring the real
driver's HAL_IN / HAL_OUT / HAL_IO convention:

* HAL_IN  -> driven by LinuxCNC into rio (machine -> card).
* HAL_OUT -> written by rio for LinuxCNC (card -> machine).
* HAL_IO  -> bidirectional (the encoder index handshake).

Note on parameters: riocomp.c contains **no** ``hal_param_*_newf`` calls; the
``*_scale`` / ``*_offset`` signals (e.g. ``x-joint.velocity-scale``,
``encoder-x.position-offset``, ``spindle.dty-scale``) are ordinary HAL_IN float
*pins*.  They are therefore created here as pins (``HAL_FLOAT``), not as HAL
params -- this is a faithful match, not a substitution.

Simulation semantics
--------------------
The model integrates with *real* elapsed time (``time.perf_counter()``), never
assuming the nominal cycle period.  Run ``--help`` for the CLI.
"""

import argparse
import os
import signal
import sys
import time

COMPONENT_NAME = "rio"
DEFAULT_CYCLE = 0.002  # seconds

# ---------------------------------------------------------------------------
# Pin surface (mechanically extracted from riocomp.c -- do not hand-edit)
# ---------------------------------------------------------------------------
EXTRACTED_PINS = [
    ("sys-status", "bit", "OUT"),
    ("sys-enable", "bit", "IN"),
    ("sys-enable-request", "bit", "IN"),
    ("machine-on", "bit", "IN"),
    ("sys-simulation", "bit", "IN"),
    ("duration", "float", "OUT"),
    ("SlotEN.bit", "bit", "IN"),
    ("x-joint.velocity-scale", "float", "IN"),
    ("x-joint.velocity-offset", "float", "IN"),
    ("x-joint.velocity", "float", "IN"),
    ("x-joint.position-scale", "float", "IN"),
    ("x-joint.position-offset", "float", "IN"),
    ("x-joint.position", "float", "OUT"),
    ("x-joint.position-abs", "float", "OUT"),
    ("x-joint.position-s32", "s32", "OUT"),
    ("x-joint.position-u32-abs", "u32", "OUT"),
    ("x-joint.enable", "bit", "IN"),
    ("y-joint.velocity-scale", "float", "IN"),
    ("y-joint.velocity-offset", "float", "IN"),
    ("y-joint.velocity", "float", "IN"),
    ("y-joint.position-scale", "float", "IN"),
    ("y-joint.position-offset", "float", "IN"),
    ("y-joint.position", "float", "OUT"),
    ("y-joint.position-abs", "float", "OUT"),
    ("y-joint.position-s32", "s32", "OUT"),
    ("y-joint.position-u32-abs", "u32", "OUT"),
    ("y-joint.enable", "bit", "IN"),
    ("z-joint.velocity-scale", "float", "IN"),
    ("z-joint.velocity-offset", "float", "IN"),
    ("z-joint.velocity", "float", "IN"),
    ("z-joint.position-scale", "float", "IN"),
    ("z-joint.position-offset", "float", "IN"),
    ("z-joint.position", "float", "OUT"),
    ("z-joint.position-abs", "float", "OUT"),
    ("z-joint.position-s32", "s32", "OUT"),
    ("z-joint.position-u32-abs", "u32", "OUT"),
    ("z-joint.enable", "bit", "IN"),
    ("min-x.bit", "bit", "OUT"),
    ("min-x.bit-not", "bit", "OUT"),
    ("max-x.bit", "bit", "OUT"),
    ("max-x.bit-not", "bit", "OUT"),
    ("min-y.bit", "bit", "OUT"),
    ("min-y.bit-not", "bit", "OUT"),
    ("max-y.bit", "bit", "OUT"),
    ("max-y.bit-not", "bit", "OUT"),
    ("min-z.bit", "bit", "OUT"),
    ("min-z.bit-not", "bit", "OUT"),
    ("max-z.bit", "bit", "OUT"),
    ("max-z.bit-not", "bit", "OUT"),
    ("ls-a.bit", "bit", "OUT"),
    ("ls-a.bit-not", "bit", "OUT"),
    ("ls-b.bit", "bit", "OUT"),
    ("ls-b.bit-not", "bit", "OUT"),
    ("spindle-alarm.bit", "bit", "OUT"),
    ("spindle-alarm.bit-not", "bit", "OUT"),
    ("spindle-fault.bit", "bit", "OUT"),
    ("spindle-fault.bit-not", "bit", "OUT"),
    ("probe.bit", "bit", "OUT"),
    ("probe.bit-not", "bit", "OUT"),
    ("machine-on-switch.bit", "bit", "OUT"),
    ("machine-on-switch.bit-not", "bit", "OUT"),
    ("alarm-x.bit", "bit", "OUT"),
    ("alarm-x.bit-not", "bit", "OUT"),
    ("alarm-y.bit", "bit", "OUT"),
    ("alarm-y.bit-not", "bit", "OUT"),
    ("alarm-z.bit", "bit", "OUT"),
    ("alarm-z.bit-not", "bit", "OUT"),
    ("worklamp.bit", "bit", "IN"),
    ("tool-change.bit", "bit", "IN"),
    ("coolant.bit", "bit", "IN"),
    ("oil-lube.bit", "bit", "IN"),
    ("spindle-forward.bit", "bit", "IN"),
    ("spindle-reverse.bit", "bit", "IN"),
    ("spindle-estop-disabled.bit", "bit", "IN"),
    ("spindle-position-mode.bit", "bit", "IN"),
    ("spindle-position-home.bit", "bit", "IN"),
    ("encoder-x.indexenable", "bit", "IO"),
    ("encoder-x.indexout", "bit", "OUT"),
    ("encoder-x.indexout-not", "bit", "OUT"),
    ("encoder-x.indexout-reset", "bit", "OUT"),
    ("encoder-x.indexout-wait", "bit", "OUT"),
    ("encoder-x.position-scale", "float", "IN"),
    ("encoder-x.position-offset", "float", "IN"),
    ("encoder-x.position", "float", "OUT"),
    ("encoder-x.position-abs", "float", "OUT"),
    ("encoder-x.position-s32", "s32", "OUT"),
    ("encoder-x.position-u32-abs", "u32", "OUT"),
    ("encoder-x.rps", "float", "OUT"),
    ("encoder-x.rpm", "float", "OUT"),
    ("encoder-y.indexenable", "bit", "IO"),
    ("encoder-y.indexout", "bit", "OUT"),
    ("encoder-y.indexout-not", "bit", "OUT"),
    ("encoder-y.indexout-reset", "bit", "OUT"),
    ("encoder-y.indexout-wait", "bit", "OUT"),
    ("encoder-y.position-scale", "float", "IN"),
    ("encoder-y.position-offset", "float", "IN"),
    ("encoder-y.position", "float", "OUT"),
    ("encoder-y.position-abs", "float", "OUT"),
    ("encoder-y.position-s32", "s32", "OUT"),
    ("encoder-y.position-u32-abs", "u32", "OUT"),
    ("encoder-y.rps", "float", "OUT"),
    ("encoder-y.rpm", "float", "OUT"),
    ("encoder-z.indexenable", "bit", "IO"),
    ("encoder-z.indexout", "bit", "OUT"),
    ("encoder-z.indexout-not", "bit", "OUT"),
    ("encoder-z.indexout-reset", "bit", "OUT"),
    ("encoder-z.indexout-wait", "bit", "OUT"),
    ("encoder-z.position-scale", "float", "IN"),
    ("encoder-z.position-offset", "float", "IN"),
    ("encoder-z.position", "float", "OUT"),
    ("encoder-z.position-abs", "float", "OUT"),
    ("encoder-z.position-s32", "s32", "OUT"),
    ("encoder-z.position-u32-abs", "u32", "OUT"),
    ("encoder-z.rps", "float", "OUT"),
    ("encoder-z.rpm", "float", "OUT"),
    ("home-x.bit", "bit", "OUT"),
    ("home-x.bit-not", "bit", "OUT"),
    ("home-y.bit", "bit", "OUT"),
    ("home-y.bit-not", "bit", "OUT"),
    ("home-z.bit", "bit", "OUT"),
    ("home-z.bit-not", "bit", "OUT"),
    ("servo-driver-brake.bit", "bit", "OUT"),
    ("servo-driver-brake.bit-not", "bit", "OUT"),
    ("spindle-power-off.bit", "bit", "IN"),
    ("x-power-off.bit", "bit", "IN"),
    ("y-power-off.bit", "bit", "IN"),
    ("z-power-off.bit", "bit", "IN"),
    ("spindle.dty-scale", "float", "IN"),
    ("spindle.dty-offset", "float", "IN"),
    ("spindle.dty", "float", "IN"),
    ("spindle.enable", "bit", "IN"),
    ("spindle-encoder.indexenable", "bit", "IO"),
    ("spindle-encoder.indexout", "bit", "OUT"),
    ("spindle-encoder.indexout-not", "bit", "OUT"),
    ("spindle-encoder.indexout-reset", "bit", "OUT"),
    ("spindle-encoder.indexout-wait", "bit", "OUT"),
    ("spindle-encoder.position-scale", "float", "IN"),
    ("spindle-encoder.position-offset", "float", "IN"),
    ("spindle-encoder.position", "float", "OUT"),
    ("spindle-encoder.position-abs", "float", "OUT"),
    ("spindle-encoder.position-s32", "s32", "OUT"),
    ("spindle-encoder.position-u32-abs", "u32", "OUT"),
    ("spindle-encoder.rps", "float", "OUT"),
    ("spindle-encoder.rpm", "float", "OUT"),
    ("spindle-modbus.spindle_speed-scale", "float", "IN"),
    ("spindle-modbus.spindle_speed-offset", "float", "IN"),
    ("spindle-modbus.spindle_speed", "float", "OUT"),
    ("spindle-modbus.spindle_speed-abs", "float", "OUT"),
    ("spindle-modbus.spindle_speed-s32", "s32", "OUT"),
    ("spindle-modbus.spindle_speed-u32-abs", "u32", "OUT"),
    ("spindle-modbus.spindle_speed_valid", "bit", "OUT"),
    ("spindle-modbus.spindle_speed_valid-not", "bit", "OUT"),
    ("spindle-modbus.spindle_speed_errors", "float", "OUT"),
    ("spindle-modbus.spindle_load-scale", "float", "IN"),
    ("spindle-modbus.spindle_load-offset", "float", "IN"),
    ("spindle-modbus.spindle_load", "float", "OUT"),
    ("spindle-modbus.spindle_load-abs", "float", "OUT"),
    ("spindle-modbus.spindle_load-s32", "s32", "OUT"),
    ("spindle-modbus.spindle_load-u32-abs", "u32", "OUT"),
    ("spindle-modbus.spindle_load_valid", "bit", "OUT"),
    ("spindle-modbus.spindle_load_valid-not", "bit", "OUT"),
    ("spindle-modbus.spindle_load_errors", "float", "OUT"),
    ("spindle-modbus.spindle_position-scale", "float", "IN"),
    ("spindle-modbus.spindle_position-offset", "float", "IN"),
    ("spindle-modbus.spindle_position", "float", "OUT"),
    ("spindle-modbus.spindle_position-abs", "float", "OUT"),
    ("spindle-modbus.spindle_position-s32", "s32", "OUT"),
    ("spindle-modbus.spindle_position-u32-abs", "u32", "OUT"),
    ("spindle-modbus.spindle_position_valid", "bit", "OUT"),
    ("spindle-modbus.spindle_position_valid-not", "bit", "OUT"),
    ("spindle-modbus.spindle_position_errors", "float", "OUT"),
    ("spindle-modbus.spindle_status-scale", "float", "IN"),
    ("spindle-modbus.spindle_status-offset", "float", "IN"),
    ("spindle-modbus.spindle_status", "float", "OUT"),
    ("spindle-modbus.spindle_status-abs", "float", "OUT"),
    ("spindle-modbus.spindle_status-s32", "s32", "OUT"),
    ("spindle-modbus.spindle_status-u32-abs", "u32", "OUT"),
    ("spindle-modbus.spindle_status_valid", "bit", "OUT"),
    ("spindle-modbus.spindle_status_valid-not", "bit", "OUT"),
    ("spindle-modbus.spindle_status_errors", "float", "OUT"),
    ("estop-in.bit", "bit", "OUT"),
    ("estop-in.bit-not", "bit", "OUT"),
]

# ---------------------------------------------------------------------------
# Sim-extension pins (sim-only; do not exist on the real driver)
# ---------------------------------------------------------------------------
# (name, type, direction)
SIM_EXTENSION_PINS = [
    # True joint position in mm (netted from joint.N.motor-pos-fb by the sim .hal).
    ("sim-pos-x", "float", "IN"),
    ("sim-pos-y", "float", "IN"),
    ("sim-pos-z", "float", "IN"),
    # Simulated E-stop mushroom (1 = pressed) and probe contact (1 = contact).
    ("sim-estop", "bit", "IN"),
    ("sim-probe", "bit", "IN"),
    # Home-switch position (0.0 -> default 0.0), window (0.0 -> default 1.0 mm).
    ("sim-home-pos-x", "float", "IN"),
    ("sim-home-pos-y", "float", "IN"),
    ("sim-home-pos-z", "float", "IN"),
    ("sim-home-window", "float", "IN"),
    # Limit-switch distance beyond the INI soft limit (0.0 -> default 1.0 mm).
    ("sim-lim-margin", "float", "IN"),
    # Travel of one motor revolution (0.0 -> default 0.25 mm): index pulse spacing.
    ("sim-index-dist-x", "float", "IN"),
    ("sim-index-dist-y", "float", "IN"),
    ("sim-index-dist-z", "float", "IN"),
    # Momentary panel power buttons; latch the machine power switch.
    ("sim-power-on", "bit", "IN"),
    ("sim-power-off", "bit", "IN"),
    # Simulated tool-changer hardware (driven from motion.digital-out-*).
    ("sim-atc-out-sol", "bit", "IN"),
    ("sim-atc-in-sol", "bit", "IN"),
    ("sim-atc-motor-cw", "bit", "IN"),
    ("sim-atc-motor-ccw", "bit", "IN"),
    # ATC sensors (wired to motion.digital-in-*).
    ("sim-atc-out-sensor", "bit", "OUT"),
    ("sim-atc-in-sensor", "bit", "OUT"),
    ("sim-atc-index", "bit", "OUT"),
]

ALL_PINS = EXTRACTED_PINS + SIM_EXTENSION_PINS

# Default axis soft limits (mm) -- the real INI values, used as a fallback when
# the INI file is not available.  (baiermill_pb/probe_basic.ini [AXIS_X/Y/Z])
DEFAULT_LIMITS = {
    "x": (-372.0, 5.0),
    "y": (-346.0, 4.0),
    "z": (-253.0, 19.0),
}

AXES = ("x", "y", "z")

# Full-scale spindle speed (rpm) used to convert VFD duty percent -> rpm.
# Overridden from the INI ([DISPLAY] MAX_SPINDLE_SPEED) when available.
DEFAULT_MAX_SPINDLE_SPEED = 12000.0


def read_axis_limits():
    """Read [AXIS_X/Y/Z] MIN_LIMIT/MAX_LIMIT from the INI, else use defaults.

    The INI path is taken from the ``INI_FILE_NAME`` environment variable that
    LinuxCNC exports to every loaded component.  Any failure (missing var,
    unimportable ``linuxcnc``, unparseable value) falls back to DEFAULT_LIMITS.
    """
    limits = {a: DEFAULT_LIMITS[a] for a in AXES}
    try:
        ini_path = os.environ.get("INI_FILE_NAME")
        if not ini_path or not os.path.exists(ini_path):
            return limits
        import linuxcnc  # noqa: PLC0415 -- only needed in this path
        ini = linuxcnc.ini(ini_path)
        for a, sec in zip(AXES, ("AXIS_X", "AXIS_Y", "AXIS_Z")):
            lo = ini.find(sec, "MIN_LIMIT")
            hi = ini.find(sec, "MAX_LIMIT")
            if lo is not None:
                limits[a] = (float(lo), limits[a][1])
            if hi is not None:
                limits[a] = (limits[a][0], float(hi))
    except Exception:
        pass
    return limits


def read_max_spindle_speed():
    """Read [DISPLAY] MAX_SPINDLE_SPEED from the INI, else use the default.

    Same INI discovery as ``read_axis_limits``; any failure falls back to
    DEFAULT_MAX_SPINDLE_SPEED (12000 rpm).
    """
    try:
        ini_path = os.environ.get("INI_FILE_NAME")
        if ini_path and os.path.exists(ini_path):
            import linuxcnc  # noqa: PLC0415 -- only needed in this path
            val = linuxcnc.ini(ini_path).find("DISPLAY", "MAX_SPINDLE_SPEED")
            if val is not None:
                return float(val)
    except Exception:
        pass
    return DEFAULT_MAX_SPINDLE_SPEED


# ---------------------------------------------------------------------------
# HAL pin backing stores (dry-run uses a dict, otherwise a real HAL component)
# ---------------------------------------------------------------------------
class DryStore:
    """Pin store backed by a plain dict -- no HAL session required."""

    def __init__(self):
        self.values = {}

    def add(self, name, typ, dircode):
        self.values[name] = 0.0

    def read(self, name):
        return self.values[name]

    def write(self, name, val):
        self.values[name] = val

    def ready(self):
        pass

    def exit(self):
        pass


class HalStore:
    """Pin store backed by a real ``hal.component``."""

    def __init__(self, halmod, name):
        self.h = halmod.component(name)
        self.types = {
            "bit": halmod.HAL_BIT,
            "float": halmod.HAL_FLOAT,
            "s32": halmod.HAL_S32,
            "u32": halmod.HAL_U32,
        }
        self.dirs = {
            "IN": halmod.HAL_IN,
            "OUT": halmod.HAL_OUT,
            "IO": halmod.HAL_IO,
        }

    def add(self, name, typ, dircode):
        self.h.newpin(name, self.types[typ], self.dirs[dircode])

    def read(self, name):
        return self.h[name]

    def write(self, name, val):
        self.h[name] = val

    def ready(self):
        self.h.ready()

    def exit(self):
        self.h.exit()


def make_store(dry):
    if dry:
        return DryStore()
    import hal  # noqa: PLC0415 -- only needed for a real HAL session
    return HalStore(hal, COMPONENT_NAME)


# ---------------------------------------------------------------------------
# Small time-delay helper for the ATC pneumatic sensors
# ---------------------------------------------------------------------------
class Delay:
    """Returns the input value as it was ``delay`` seconds ago."""

    def __init__(self, delay_s):
        self.delay = delay_s
        self.hist = []  # list of (t, value), ascending time

    def push(self, t, val):
        self.hist.append((t, val))
        cutoff = t - self.delay
        # drop samples that are no longer needed
        while len(self.hist) > 1 and self.hist[1][0] <= cutoff:
            self.hist.pop(0)

    def get(self, t):
        cutoff = t - self.delay
        val = 0.0
        for (tt, vv) in self.hist:
            if tt <= cutoff:
                val = vv
            else:
                break
        return val


# ---------------------------------------------------------------------------
# The simulation model
# ---------------------------------------------------------------------------
class RioModel:
    """Pure-software model of the rio FPGA card.  Drives the HAL pins."""

    def __init__(self, store, limits=None, verbose=False, trace=False,
                 max_spindle_speed=None):
        self.store = store
        self.limits = limits if limits is not None else dict(DEFAULT_LIMITS)
        self.max_spindle_speed = (
            max_spindle_speed if max_spindle_speed is not None
            else DEFAULT_MAX_SPINDLE_SPEED)
        self.verbose = verbose
        self.trace = trace
        self._sim_time = 0.0

        # per-axis state
        self._joint_pos = {a: 0.0 for a in AXES}
        self._last_pos = {a: 0.0 for a in AXES}
        self._last_q = {a: None for a in AXES}   # floor(pos/index_dist) per axis
        self._rps = {a: 0.0 for a in AXES}

        # power switch latch
        self._power = 0
        self._last_pon = 0
        self._last_poff = 0

        # spindle state
        self._spindle_rpm = 0.0
        self._spindle_pos = 0.0   # signed revolutions
        self._spindle_revs = 0.0  # |revolutions| (for once-per-rev index)
        self._spindle_q = 0

        # ATC state
        self._atc_out_delay = Delay(0.3)
        self._atc_in_delay = Delay(0.3)

        # previous values for change logging
        self._prev = {}

    # -- pin convenience ---------------------------------------------------
    def _rd(self, name):
        return self.store.read(name)

    def _wr(self, name, val):
        self.store.write(name, val)

    def set(self, name, val):
        """Direct input override (used by the self-test)."""
        self.store.write(name, val)

    def get(self, name):
        return self.store.read(name)

    def _log_change(self, name, val):
        """Log a state change when --verbose is on."""
        if self.verbose and self._prev.get(name) != val:
            self._prev[name] = val
            print("[rio_sim] %s -> %r" % (name, val))

    # -- per-cycle steps ---------------------------------------------------
    def tick(self, dt):
        self._sim_time += dt
        self._tick_sys()
        self._tick_power()
        self._tick_estop_probe()
        self._tick_limits_home_toolchange()
        self._tick_alarms_brake()
        self._tick_joints_encoders(dt)
        self._tick_spindle(dt)
        self._tick_atc()
        # riocomp.c:367 -- duration is the FPGA frame timing, unused by the
        # machine HAL; here it reports the measured loop period in ms.
        self._wr("duration", dt * 1000.0)
        self._tick_passthrough()

    def _tick_sys(self):
        # Mirror riocomp.c:2103-2105 -- sys-enable-request -> sys-status.
        if self._rd("sys-enable-request") == 1:
            self._wr("sys-status", 1)
        # Mirror riocomp.c:2148-2151 -- only report ready while enabled (and
        # healthy).  sys-simulation is kept for compatibility and ignored here
        # (the real driver uses it to skip FPGA comms; we have no FPGA).
        if self._rd("sys-enable") != 1 or self._rd("sys-status") != 1:
            self._wr("sys-status", 0)

    def _tick_power(self):
        # Latch the power switch from the two panel buttons (rising edges).
        pon = 1 if self._rd("sim-power-on") else 0
        poff = 1 if self._rd("sim-power-off") else 0
        if pon and not self._last_pon:
            self._power = 1
        if poff and not self._last_poff:
            self._power = 0
        self._last_pon = pon
        self._last_poff = poff
        self._wr("machine-on-switch.bit", 1 if self._power else 0)
        self._wr("machine-on-switch.bit-not", 0 if self._power else 1)

    def _tick_estop_probe(self):
        e = 1 if self._rd("sim-estop") else 0
        self._wr("estop-in.bit", e)
        self._wr("estop-in.bit-not", 1 - e)
        self._log_change("estop-in.bit", e)
        p = 1 if self._rd("sim-probe") else 0
        self._wr("probe.bit", p)
        self._wr("probe.bit-not", 1 - p)

    def _tick_limits_home_toolchange(self):
        margin = self._rd("sim-lim-margin")
        if margin <= 0.0:
            margin = 1.0
        window = self._rd("sim-home-window")
        if window <= 0.0:
            window = 1.0
        for a in AXES:
            pos = self._rd("sim-pos-%s" % a)
            lo, hi = self.limits[a]
            # The switch sits *outside* the soft limit, as on the machine: a
            # real limit switch is beyond the travel the controller allows, so
            # the soft limit stops the axis before the switch is reached. Placed
            # inside, every jog to the limit tripped it.
            mn = 1 if pos <= lo - margin else 0
            mx = 1 if pos >= hi + margin else 0
            self._wr("min-%s.bit" % a, mn)
            self._wr("min-%s.bit-not" % a, 1 - mn)
            self._wr("max-%s.bit" % a, mx)
            self._wr("max-%s.bit-not" % a, 1 - mx)
            self._log_change("min-%s.bit" % a, mn)
            self._log_change("max-%s.bit" % a, mx)
            hp = self._rd("sim-home-pos-%s" % a)
            home = 1 if abs(pos - hp) <= window else 0
            self._wr("home-%s.bit" % a, home)
            self._wr("home-%s.bit-not" % a, 1 - home)
            self._log_change("home-%s.bit" % a, home)
        tc = 1 if self._rd("tool-change.bit") else 0
        # ls-a = "clamped" (active while drawbar closed), ls-b = "unclamped".
        self._wr("ls-a.bit", 1 - tc)
        self._wr("ls-a.bit-not", tc)
        self._wr("ls-b.bit", tc)
        self._wr("ls-b.bit-not", 1 - tc)

    def _tick_alarms_brake(self):
        # All alarms healthy (0).  Exposed as ordinary HAL_OUT pins so a future
        # sim can trip them.
        for base in ("alarm-x", "alarm-y", "alarm-z",
                     "spindle-alarm", "spindle-fault"):
            self._wr("%s.bit" % base, 0)
            self._wr("%s.bit-not" % base, 1)
        # Servo-driver brake: tied to the Z axis (joint.2) -- the braked axis
        # on this VMC [INFERENCE].  Brake on (1) while the joint is disabled.
        z_enable = 1 if self._rd("z-joint.enable") else 0
        brake = 0 if z_enable else 1
        self._wr("servo-driver-brake.bit", brake)
        self._wr("servo-driver-brake.bit-not", 1 - brake)

    def _tick_joints_encoders(self, dt):
        for a in AXES:
            # Joint/stepgen model: integrate the commanded velocity.  The
            # input is already the physical velocity in mm/s (the machine sets
            # velocity-scale to steps/mm for the real card; we do NOT apply it
            # -- see the assignment's simplification note).
            v = self._rd("%s-joint.velocity" % a)
            self._joint_pos[a] += v * dt

            pos = self._rd("sim-pos-%s" % a)
            index_dist = self._rd("sim-index-dist-%s" % a)
            if index_dist <= 0.0:
                index_dist = 0.25

            # Encoder index handshake (riocomp.c:1540-1560, 1108-1124): the
            # HAL_IO indexenable is armed by motion (joint.N.index-enable).
            # While armed, a crossing of a multiple of index_dist fires a one-
            # cycle indexout pulse, clears indexenable, and resets the joint
            # position to 0 (HOME_INDEX_NO_ENCODER_RESET = NO on this machine).
            ie = 1 if self._rd("encoder-%s.indexenable" % a) else 0
            q = int(pos / index_dist) if index_dist > 0 else 0
            self._wr("encoder-%s.indexout-wait" % a, 1 if ie else 0)
            fired = False
            if ie and index_dist > 0 and self._last_q[a] is not None \
                    and q != self._last_q[a]:
                self._wr("encoder-%s.indexout" % a, 1)
                self._wr("encoder-%s.indexout-not" % a, 0)
                self._wr("encoder-%s.indexout-reset" % a, 1)
                self._wr("encoder-%s.indexenable" % a, 0)
                self._joint_pos[a] = 0.0
                fired = True
                if self.verbose:
                    print("[rio_sim] encoder-%s index pulse @ %.4f mm"
                          % (a, pos))
            else:
                self._wr("encoder-%s.indexout" % a, 0)
                self._wr("encoder-%s.indexout-not" % a, 1)
                self._wr("encoder-%s.indexout-reset" % a, 0)
            self._last_q[a] = q

            # Joint position outputs (the stepgen-integrated value).
            jp = self._joint_pos[a]
            self._wr("%s-joint.position" % a, jp)
            self._wr("%s-joint.position-abs" % a, abs(jp))
            self._wr("%s-joint.position-s32" % a, int(jp))
            self._wr("%s-joint.position-u32-abs" % a, abs(int(jp)))

            # Encoder position == the true machine position (no dynamics).
            self._wr("encoder-%s.position" % a, pos)
            self._wr("encoder-%s.position-abs" % a, abs(pos))
            self._wr("encoder-%s.position-s32" % a, int(pos))
            self._wr("encoder-%s.position-u32-abs" % a, abs(int(pos)))

            # Motor speed: revs/s = (delta_pos / index_dist) / dt, low-passed
            # (single pole, ~10 ms) so the display is not noisy.
            delta = pos - self._last_pos[a]
            self._last_pos[a] = pos
            if dt > 0.0 and index_dist > 0.0:
                rps_raw = (delta / index_dist) / dt
            else:
                rps_raw = 0.0
            alpha = dt / (dt + 0.010)
            self._rps[a] += alpha * (rps_raw - self._rps[a])
            self._wr("encoder-%s.rps" % a, self._rps[a])
            self._wr("encoder-%s.rpm" % a, self._rps[a] * 60.0)

    def _tick_spindle(self, dt):
        dty = self._rd("spindle.dty")
        dty_scale = self._rd("spindle.dty-scale")
        dty_offset = self._rd("spindle.dty-offset")
        enable = 1 if self._rd("spindle.enable") else 0
        fwd = 1 if self._rd("spindle-forward.bit") else 0
        rev = 1 if self._rd("spindle-reverse.bit") else 0
        running = bool(enable) and (bool(fwd) != bool(rev))
        # Physical spindle chain, mirroring riocomp.c:1174-1180
        # (convert_varout32_pwmout0_dty): the dty pin carries spindle speed in
        # rpm (netted from spindle.0.speed-out).  The card computes
        #   duty_percent = dty * dty-scale + dty-offset     (0..100 %)
        # and the VFD turns percent back into rpm via MAX_SPINDLE_SPEED:
        #   rpm_cmd = duty_percent * MAX_SPINDLE_SPEED / 100.
        # Here dty-scale = 1/120, so rpm_cmd ~= dty (the INI's MAX_SPINDLE_SPEED
        # is the full-scale term).  No division: dty-scale == 0 simply yields
        # duty 0 (spindle stays stopped) -- a safe unset fallback.
        duty_percent = dty * dty_scale + dty_offset
        rpm_cmd = (duty_percent * self.max_spindle_speed / 100.0) if running \
            else 0.0
        # First-order speed ramp (time constant ~0.5 s).
        alpha = dt / (dt + 0.5)
        self._spindle_rpm += alpha * (rpm_cmd - self._spindle_rpm)
        rpm = self._spindle_rpm
        rps = rpm / 60.0

        direction = 1 if fwd else (-1 if rev else 0)
        self._spindle_pos += direction * rps * dt
        self._spindle_revs += rps * dt

        self._wr("spindle-encoder.rps", rps)
        self._wr("spindle-encoder.rpm", rpm)
        sp = self._spindle_pos
        self._wr("spindle-encoder.position", sp)
        self._wr("spindle-encoder.position-abs", abs(sp))
        self._wr("spindle-encoder.position-s32", int(sp))
        self._wr("spindle-encoder.position-u32-abs", abs(int(sp)))

        # Once-per-revolution index (riocomp.c:1735-1760), same handshake.
        sie = 1 if self._rd("spindle-encoder.indexenable") else 0
        sq = int(self._spindle_revs)
        self._wr("spindle-encoder.indexout-wait", 1 if sie else 0)
        if sie and sq != self._spindle_q:
            self._wr("spindle-encoder.indexout", 1)
            self._wr("spindle-encoder.indexout-not", 0)
            self._wr("spindle-encoder.indexout-reset", 1)
            self._wr("spindle-encoder.indexenable", 0)
            if self.verbose:
                print("[rio_sim] spindle-encoder index pulse @ %d rev" % sq)
        else:
            self._wr("spindle-encoder.indexout", 0)
            self._wr("spindle-encoder.indexout-not", 1)
            self._wr("spindle-encoder.indexout-reset", 0)
        self._spindle_q = sq

        # VFD telemetry (spindle-modbus.*) consumed by the ProbeBasic GUI.
        rpm_delta = rpm_cmd - rpm
        if running:
            load = 3.0 + 0.15 * abs(rpm_delta)
            if load > 100.0:
                load = 100.0
        else:
            load = 0.0
        status = 1 if running else 0

        self._wr("spindle-modbus.spindle_speed", rpm)
        self._wr("spindle-modbus.spindle_speed-abs", abs(rpm))
        self._wr("spindle-modbus.spindle_speed-s32", int(rpm))
        self._wr("spindle-modbus.spindle_speed-u32-abs", abs(int(rpm)))
        self._wr("spindle-modbus.spindle_speed_valid", 1)
        self._wr("spindle-modbus.spindle_speed_valid-not", 0)
        self._wr("spindle-modbus.spindle_speed_errors", 0.0)

        self._wr("spindle-modbus.spindle_load", load)
        self._wr("spindle-modbus.spindle_load-abs", abs(load))
        self._wr("spindle-modbus.spindle_load-s32", int(load))
        self._wr("spindle-modbus.spindle_load-u32-abs", abs(int(load)))
        self._wr("spindle-modbus.spindle_load_valid", 1)
        self._wr("spindle-modbus.spindle_load_valid-not", 0)
        self._wr("spindle-modbus.spindle_load_errors", 0.0)

        self._wr("spindle-modbus.spindle_position", 0.0)
        self._wr("spindle-modbus.spindle_position-abs", 0.0)
        self._wr("spindle-modbus.spindle_position-s32", 0)
        self._wr("spindle-modbus.spindle_position-u32-abs", 0)
        self._wr("spindle-modbus.spindle_position_valid", 1)
        self._wr("spindle-modbus.spindle_position_valid-not", 0)
        self._wr("spindle-modbus.spindle_position_errors", 0.0)

        self._wr("spindle-modbus.spindle_status", float(status))
        self._wr("spindle-modbus.spindle_status-abs", float(status))
        self._wr("spindle-modbus.spindle_status-s32", status)
        self._wr("spindle-modbus.spindle_status-u32-abs", status)
        self._wr("spindle-modbus.spindle_status_valid", 1)
        self._wr("spindle-modbus.spindle_status_valid-not", 0)
        self._wr("spindle-modbus.spindle_status_errors", 0.0)

        if self.verbose:
            self._log_change("spindle-encoder.rpm", round(rpm, 1))

    def _tick_atc(self):
        t = self._sim_time
        out_sol = 1 if self._rd("sim-atc-out-sol") else 0
        in_sol = 1 if self._rd("sim-atc-in-sol") else 0
        cw = 1 if self._rd("sim-atc-motor-cw") else 0
        ccw = 1 if self._rd("sim-atc-motor-ccw") else 0

        # Pneumatic position sensors: 0.3 s delay after the solenoid.
        self._atc_out_delay.push(t, out_sol)
        self._atc_in_delay.push(t, in_sol)
        self._wr("sim-atc-out-sensor", self._atc_out_delay.get(t))
        self._wr("sim-atc-in-sensor", self._atc_in_delay.get(t))

        # Rotation index pulse train: one ~20 ms pulse every 200 ms while
        # either motor input is on (macros wait for a single rising edge).
        motor_on = cw or ccw
        phase = t % 0.200
        idx = 1 if (motor_on and phase < 0.020) else 0
        self._wr("sim-atc-index", idx)

    def _tick_passthrough(self):
        # Machine -> card outputs: ordinary pins the machine writes; the model
        # does nothing with them.  In --trace mode, report changes.
        for name in ("worklamp.bit", "coolant.bit", "oil-lube.bit",
                     "tool-change.bit", "spindle-power-off.bit",
                     "x-power-off.bit", "y-power-off.bit", "z-power-off.bit",
                     "spindle-position-mode.bit", "spindle-position-home.bit",
                     "SlotEN.bit", "spindle-estop-disabled.bit"):
            if self.trace:
                self._log_change(name, self._rd(name))


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------
def parse_args(argv):
    p = argparse.ArgumentParser(
        description="Software simulation of the rio FPGA HAL component.")
    p.add_argument("--cycle", type=float, default=DEFAULT_CYCLE,
                   help="nominal loop period in seconds (default %s)"
                        % DEFAULT_CYCLE)
    p.add_argument("--dry-run", action="store_true",
                   help="no HAL session; pins in an internal dict")
    p.add_argument("--verbose", action="store_true",
                   help="log state changes (estop, limits, home, index, rpm)")
    p.add_argument("--trace", action="store_true",
                   help="log machine->card pass-through pin changes")
    p.add_argument("--list-pins", action="store_true",
                   help="print 'name type dir' for every pin and exit")
    p.add_argument("--selftest", action="store_true",
                   help="run the model self-test (no HAL) and exit")
    return p.parse_args(argv)


def list_pins():
    for name, typ, dircode in ALL_PINS:
        print("%s %s %s" % (name, typ, dircode))


def run_loop(store, model, cycle):
    stop = [False]

    def _sig(signum, frame):
        stop[0] = True

    signal.signal(signal.SIGINT, _sig)
    signal.signal(signal.SIGTERM, _sig)

    prev = time.perf_counter()
    try:
        while not stop[0]:
            now = time.perf_counter()
            dt = now - prev
            prev = now
            if dt <= 0.0:
                dt = cycle
            model.tick(dt)
            to_sleep = cycle - (time.perf_counter() - now)
            if to_sleep > 0.0:
                time.sleep(to_sleep)
    finally:
        store.exit()
    return 0


# ---------------------------------------------------------------------------
# Self-test (drives the model directly, no HAL)
# ---------------------------------------------------------------------------
def selftest():
    store = DryStore()
    for name, typ, dircode in ALL_PINS:
        store.add(name, typ, dircode)
    m = RioModel(store, limits=dict(DEFAULT_LIMITS), verbose=False)
    dt = 0.002
    failures = []

    def check(cond, msg):
        tag = "PASS" if cond else "FAIL"
        print("[%s] %s" % (tag, msg))
        if not cond:
            failures.append(msg)

    # -- encoder position follows sim-pos ----------------------------------
    m.set("sim-pos-x", 123.45)
    m.set("sim-pos-y", -42.0)
    m.set("sim-pos-z", 7.5)
    m.tick(dt)
    check(abs(m.get("encoder-x.position") - 123.45) < 1e-6,
          "encoder-x.position follows sim-pos-x")
    check(abs(m.get("encoder-y.position") - (-42.0)) < 1e-6,
          "encoder-y.position follows sim-pos-y")
    check(abs(m.get("encoder-z.position") - 7.5) < 1e-6,
          "encoder-z.position follows sim-pos-z")

    # -- index pulse fires once per sim-index-dist --------------------------
    m.set("sim-pos-x", 0.0)
    m.set("sim-index-dist-x", 1.0)
    m.tick(dt)  # record starting q = 0
    m.set("encoder-x.indexenable", 1)
    m.tick(dt)  # armed; q still 0
    check(m.get("encoder-x.indexout") == 0, "index not fired before crossing")
    m.set("sim-pos-x", 1.5)  # crosses 1.0
    m.tick(dt)
    check(m.get("encoder-x.indexout") == 1, "indexout pulses on crossing")
    check(m.get("encoder-x.indexenable") == 0, "indexenable cleared (handshake)")
    check(m.get("encoder-x.indexout-reset") == 1, "indexout-reset pulsed")
    check(abs(m.get("x-joint.position")) < 1e-9, "joint position reset at index")
    m.tick(dt)  # pulse is one cycle only
    check(m.get("encoder-x.indexout") == 0, "indexout is a one-cycle pulse")

    # -- limit switches trip at the INI limits ------------------------------
    m.set("sim-lim-margin", 1.0)
    m.set("sim-pos-x", -373.0)  # <= -372 + 1
    m.tick(dt)
    check(m.get("min-x.bit") == 1, "min-x trips below MIN_LIMIT + margin")
    m.set("sim-pos-x", 6.0)  # >= 5 - 1
    m.tick(dt)
    check(m.get("max-x.bit") == 1, "max-x trips above MAX_LIMIT - margin")
    m.set("sim-pos-x", 0.0)
    m.tick(dt)
    check(m.get("min-x.bit") == 0 and m.get("max-x.bit") == 0,
          "limits clear inside travel")

    # -- spindle ramps to commanded rpm (real rpm-valued dty chain) ---------
    # The machine nets spindle.0.speed-out (rpm) -> rio.spindle.dty and sets
    # rio.spindle.dty-scale = 0.00833 (~1/120); MAX_SPINDLE_SPEED = 12000.
    # M3 S3000 -> dty 3000 -> duty 25 % -> rpm_cmd 3000 -> rps 50.
    m.set("spindle.enable", 1)
    m.set("spindle-forward.bit", 1)
    m.set("spindle-reverse.bit", 0)
    m.set("spindle.dty-scale", 0.00833)
    m.set("spindle.dty-offset", 0.0)
    m.set("spindle.dty", 3000.0)
    for _ in range(3000):  # 6 s of ramp, tau 0.5 s
        m.tick(dt)
    rpm = m.get("spindle-encoder.rpm")
    rps = m.get("spindle-encoder.rps")
    speed = m.get("spindle-modbus.spindle_speed")
    check(abs(rpm - 3000.0) < 5.0,
          "spindle ramps to 3000 rpm from dty=3000 (got %.2f)" % rpm)
    check(abs(rps - 50.0) < 0.1,
          "spindle-encoder.rps settles at 50 (got %.4f)" % rps)
    check(abs(speed - 3000.0) < 5.0,
          "spindle-modbus.spindle_speed ~3000 (got %.2f)" % speed)
    check(m.get("spindle-modbus.spindle_speed_valid") == 1,
          "spindle_speed telemetry valid")

    # dty 0 -> 0 rpm (spindle stopped).
    m.set("spindle.dty", 0.0)
    for _ in range(3000):  # 6 s to ramp back down
        m.tick(dt)
    check(abs(m.get("spindle-encoder.rpm")) < 0.5,
          "spindle settles to 0 rpm with dty=0 (got %.3f)"
          % m.get("spindle-encoder.rpm"))

    # -- sys-status follows sys-enable / sys-enable-request -----------------
    m.set("sys-enable", 1)
    m.set("sys-enable-request", 1)
    m.tick(dt)
    check(m.get("sys-status") == 1, "sys-status=1 with enable+request")
    m.set("sys-enable", 0)
    m.tick(dt)
    check(m.get("sys-status") == 0, "sys-status=0 when disabled")
    m.set("sys-enable", 1)
    m.set("sys-enable-request", 0)
    m.tick(dt)
    check(m.get("sys-status") == 0, "sys-status=0 without request")

    # -- power button latch ------------------------------------------------
    m.set("sim-power-on", 1)
    m.tick(dt)
    check(m.get("machine-on-switch.bit") == 1, "power-on edge -> switch 1")
    m.set("sim-power-on", 0)
    m.tick(dt)
    check(m.get("machine-on-switch.bit") == 1, "switch stays 1 after on released")
    m.set("sim-power-off", 1)
    m.tick(dt)
    check(m.get("machine-on-switch.bit") == 0, "power-off edge -> switch 0")
    m.set("sim-power-off", 0)
    m.tick(dt)

    # -- ATC: out-sensor follows out-sol with delay ------------------------
    m.set("sim-atc-out-sol", 1)
    for _ in range(20):  # 40 ms: still within the 0.3 s delay
        m.tick(dt)
    check(m.get("sim-atc-out-sensor") == 0, "out-sensor not yet active (delay)")
    for _ in range(160):  # +320 ms (> 0.3 s delay)
        m.tick(dt)
    check(m.get("sim-atc-out-sensor") == 1, "out-sensor active after 0.3 s")
    m.set("sim-atc-out-sol", 0)
    for _ in range(160):  # +320 ms (> 0.3 s delay)
        m.tick(dt)
    check(m.get("sim-atc-out-sensor") == 0, "out-sensor clears after 0.3 s")

    # -- ATC: index pulses only while a motor input is on -------------------
    m.set("sim-atc-motor-cw", 0)
    m.set("sim-atc-motor-ccw", 0)
    saw_idx_off = False
    for _ in range(150):  # 300 ms with motors off
        m.tick(dt)
        if m.get("sim-atc-index") == 1:
            saw_idx_off = True
    check(not saw_idx_off, "no index pulses with motors off")

    m.set("sim-atc-motor-cw", 1)
    saw_idx_on = False
    for _ in range(120):  # 240 ms with CW on
        m.tick(dt)
        if m.get("sim-atc-index") == 1:
            saw_idx_on = True
    check(saw_idx_on, "index pulses while motor on")

    if failures:
        print("\nSELFTEST FAILED (%d failures)" % len(failures))
        return 1
    print("\nSELFTEST PASSED")
    return 0


def main(argv=None):
    args = parse_args(sys.argv[1:] if argv is None else argv)

    if args.list_pins:
        list_pins()
        return 0

    if args.selftest:
        return selftest()

    store = make_store(args.dry_run)
    for name, typ, dircode in ALL_PINS:
        store.add(name, typ, dircode)

    model = RioModel(store, limits=read_axis_limits(),
                     max_spindle_speed=read_max_spindle_speed(),
                     verbose=args.verbose, trace=args.trace)
    store.ready()

    return run_loop(store, model, args.cycle)


if __name__ == "__main__":
    sys.exit(main())
