#!/usr/bin/env python3
"""Finish bringing a simulation up, after start-sim.sh has launched it.

The cable-side of a start-up belongs to the panel: `LINC_SIM_READY` decides how
far through its power-up sequence the simulated cabinet begins, because the
E-stop mushroom and the POWER ON key are panel inputs. Everything left needs
the controller: waiting for it to come on, homing the axes, opening a program.
That is what this script does, through LinuxCNC's own command channel.

It is also where the window is put where the operator wants it: a 1280x720
float under Hyprland rather than a tile that gets stretched to the screen.

Run by start-sim.sh; it is not a user-facing command, but every step is a flag
so start-sim.sh can pass on exactly what was asked for:

  --on            wait for the controller to leave E-stop and power on
  --home          home every axis (implies --on)
  --load FILE     open a G-code program (implies --on)
  --float         float the HMI window under Hyprland at 1280x720
  --timeout SECS  how long to wait for each step (default 60)

Exits non-zero with a plain reason when a step does not happen.
"""

import argparse
import json
import os
import subprocess
import sys
import time

# The shell is designed at 1280x720 and refuses to be smaller (SPEC §5), so
# that is the size the window is floated at.
WINDOW_SIZE = (1280, 720)


def log(message):
    print(f"sim-ready: {message}", flush=True)


def parse(argv):
    parser = argparse.ArgumentParser(
        description="Bring a started LinuxCNC simulation to a ready state."
    )
    parser.add_argument(
        "--on",
        action="store_true",
        help="wait for the controller to leave E-stop and power on",
    )
    parser.add_argument("--home", action="store_true", help="home every axis")
    parser.add_argument("--load", metavar="FILE", help="open a G-code program")
    parser.add_argument(
        "--float",
        action="store_true",
        help="float the HMI window under Hyprland at 1280x720",
    )
    parser.add_argument(
        "--window-match",
        default="linc",
        help="case-insensitive class or title text of the HMI window (default: linc)",
    )
    parser.add_argument(
        "--timeout",
        type=float,
        default=60.0,
        help="seconds to wait for each step (default 60)",
    )
    args = parser.parse_args(argv)
    if args.home or args.load:
        args.on = True
    return args


def task(timeout):
    """The LinuxCNC command channel, retried until the NML channels are up."""
    try:
        import linuxcnc
    except ImportError:
        sys.exit(
            "sim-ready: python3 cannot import linuxcnc; run it with the "
            "interpreter LinuxCNC installed"
        )
    deadline = time.monotonic() + timeout
    reason = "no answer"
    while True:
        try:
            stat = linuxcnc.stat()
            stat.poll()
            return linuxcnc, stat, linuxcnc.command()
        # Without a running task the module raises its own `error`, and on a
        # missing NML buffer it can raise a bare SystemError from the C side.
        except Exception as error:
            reason = str(error) or type(error).__name__
            if time.monotonic() >= deadline:
                sys.exit(
                    f"sim-ready: cannot reach LinuxCNC ({reason}); "
                    "is the simulation running?"
                )
            time.sleep(0.25)


def wait_for(stat, predicate, what, timeout):
    deadline = time.monotonic() + timeout
    while True:
        stat.poll()
        if predicate(stat):
            return
        if time.monotonic() >= deadline:
            sys.exit(f"sim-ready: timed out after {timeout:g} s waiting for {what}")
        time.sleep(0.1)


def is_on(linuxcnc, stat):
    return not stat.estop and stat.task_state == linuxcnc.STATE_ON


def bring_up(args):
    linuxcnc, stat, command = task(args.timeout)
    if args.on:
        # The panel released the mushroom and pressed POWER ON; the controller
        # only has to get there. Nothing here presses anything: if the machine
        # does not come up, the panel is still holding a key.
        wait_for(
            stat,
            lambda s: is_on(linuxcnc, s),
            "the machine to come on (E-stop released and POWER ON in the panel)",
            args.timeout,
        )
        log("machine on")
    if args.home:
        command.mode(linuxcnc.MODE_MANUAL)
        command.wait_complete(args.timeout)
        command.home(-1)
        command.wait_complete(args.timeout)
        wait_for(
            stat,
            lambda s: all(int(homed) for homed in s.homed),
            "every axis to finish homing",
            args.timeout,
        )
        log("axes homed")
    if args.load:
        path = os.path.abspath(args.load)
        if not os.path.exists(path):
            sys.exit(f"sim-ready: no such program: {path}")
        command.mode(linuxcnc.MODE_AUTO)
        command.wait_complete(args.timeout)
        command.program_open(path)
        command.wait_complete(args.timeout)
        name = os.path.basename(path)
        wait_for(
            stat,
            lambda s: os.path.basename(s.file or "") == name,
            f"{name} to load",
            args.timeout,
        )
        log(f"program loaded: {name}")


def hyprctl(*args):
    return subprocess.run(
        ["hyprctl", *args], capture_output=True, text=True, check=False
    )


def find_window(match, timeout):
    """The HMI client's address, waiting for the window to map.

    The class is the client's app id; for an egui window that is the profile's
    window title, which is why the default match is `linc`. Titles are not
    searched: a terminal sitting in the checkout has LiNC in its title too.
    """
    deadline = time.monotonic() + timeout
    while True:
        result = hyprctl("-j", "clients")
        if result.returncode == 0:
            try:
                clients = json.loads(result.stdout)
            except json.JSONDecodeError:
                clients = []
            for client in clients:
                if match in str(client.get("class", "")).lower():
                    return client.get("address")
        if time.monotonic() >= deadline:
            return None
        time.sleep(0.25)


def lua_dispatch(code):
    """Run one dispatcher through `hyprctl eval`; Hyprland 0.56 takes Lua here."""
    result = hyprctl("eval", code)
    if result.returncode != 0 or "error" in (result.stdout + result.stderr).lower():
        reason = (result.stdout or result.stderr).strip() or "no output"
        sys.exit(f"sim-ready: hyprctl eval failed: {reason}")


def classic_dispatch(dispatcher, argument):
    """Older Hyprland: the same two actions as plain dispatchers."""
    result = hyprctl("dispatch", dispatcher, *([argument] if argument else []))
    if result.returncode != 0 or "ok" not in result.stdout.lower():
        reason = (result.stdout or result.stderr).strip() or "no output"
        sys.exit(f"sim-ready: hyprctl dispatch {dispatcher} failed: {reason}")


def float_window(args):
    if not os.environ.get("HYPRLAND_INSTANCE_SIGNATURE"):
        log("not under Hyprland; leaving the window placement alone")
        return
    address = find_window(args.window_match.lower(), args.timeout)
    if not address:
        sys.exit(
            f"sim-ready: no window with class matching {args.window_match!r} "
            f"appeared within {args.timeout:g} s"
        )
    width, height = WINDOW_SIZE
    if hyprctl("eval", "return 1").returncode == 0:
        # 0.56 and later: build a dispatcher with the window selector and dispatch
        # it. `dispatch` alone only constructs one and does nothing.
        lua_dispatch(
            f"hl.dispatch(hl.dsp.window.float({{ window = 'address:{address}', "
            "action = 'set' }))"
        )
        lua_dispatch(
            f"hl.dispatch(hl.dsp.window.resize({{ window = 'address:{address}', "
            f"x = {width}, y = {height} }}))"
        )
    else:
        # Before the Lua dispatchers, a window selector is fetched by focusing
        # the window and the following dispatchers act on the active one.
        classic_dispatch("focuswindow", f"address:{address}")
        classic_dispatch("setfloating", None)
        classic_dispatch("resizeactive", f"exact {width} {height}")
    log(f"window floated at {width}x{height}")


def main(argv):
    args = parse(argv)
    if args.on or args.home or args.load:
        bring_up(args)
    if args.float:
        float_window(args)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except KeyboardInterrupt:
        sys.exit(130)
