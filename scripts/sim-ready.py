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

  --estop         make the controller leave E-stop
  --on            and power it on (implies --estop)
  --home          home every axis (implies --on)
  --load FILE     open a G-code program (implies --on)
  --float         float the HMI window under Hyprland at 1280x720
  --timeout SECS  how long to wait for each step (default 60)

The panel releases the mushroom itself, but its request reaches the controller as
a short HAL pulse, and at boot that pulse can arrive before the task is ready to
accept it — the panel then shows the mushroom out while the HMI still shows
E-STOP. So the reset is also queued on the command channel here, where it waits
for the task instead of racing it.

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
        "--estop",
        action="store_true",
        help="make the controller leave E-stop",
    )
    parser.add_argument(
        "--on",
        action="store_true",
        help="power the controller on (implies --estop)",
    )
    parser.add_argument("--home", action="store_true", help="home every axis")
    parser.add_argument("--load", metavar="FILE", help="open a G-code program")
    parser.add_argument(
        "--float",
        action="store_true",
        help="float the HMI window under Hyprland at 1280x720",
    )
    parser.add_argument(
        "--ini",
        metavar="FILE",
        help="LinuxCNC INI of the session; without it the NML defaults are guessed",
    )
    parser.add_argument(
        "--settle",
        type=float,
        default=3.0,
        help="seconds to let the session start before asking for anything (default 3)",
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
    if args.on:
        args.estop = True
    return args


def task(timeout, ini):
    """The LinuxCNC command channel, retried until the NML channels are up."""
    try:
        import linuxcnc
    except ImportError:
        sys.exit(
            "sim-ready: python3 cannot import linuxcnc; run it with the "
            "interpreter LinuxCNC installed"
        )
    if ini:
        # The python module talks to the NML file named in `linuxcnc.nmlfile`,
        # whose default is the machine-wide one. A session started from an INI
        # gets its own; point the module at the same file and run from the INI's
        # directory, as every LinuxCNC component does.
        ini = os.path.abspath(ini)
        if not os.path.exists(ini):
            sys.exit(f"sim-ready: no such INI: {ini}")
        try:
            section = linuxcnc.ini(ini)
            nml = section.find("EMC", "NML_FILE")
            if nml:
                linuxcnc.nmlfile = nml
        except Exception as error:
            sys.exit(f"sim-ready: cannot read {ini}: {error}")
        os.environ["INI_FILE_NAME"] = ini
        os.chdir(os.path.dirname(ini))
        log(f"talk to LinuxCNC through {ini} (nml {linuxcnc.nmlfile})")
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


def all_homed(stat):
    """True when every configured joint reports homed.

    `stat.homed` is a fixed 16-slot tuple; the slots past `stat.joints` belong to
    joints this machine does not have and never home, so the whole tuple is
    never all ones.
    """
    joints = stat.joints or len(stat.homed)
    return all(int(homed) for homed in stat.homed[:joints])


def unhomed(stat):
    joints = min(stat.joints or len(stat.homed), len(stat.homed))
    return [index for index in range(joints) if not stat.homed[index]]


def hal_value(pin):
    """One HAL pin's value, for the failure report."""
    result = subprocess.run(
        ["halcmd", "getp", pin], capture_output=True, text=True, check=False
    )
    if result.returncode != 0:
        return "unavailable"
    return result.stdout.strip().split()[-1] if result.stdout.strip() else "?"


def report_blocked(stat):
    """Say what the session looks like when a step does not happen."""
    for pin in (
        "cnc_panel.estop",
        "rio.estop-in.bit",
        "halui.estop.activate",
        "or2.estop_reset.out",
        "iocontrol.0.user-enable-out",
        "iocontrol.0.emc-enable-in",
        "halui.estop.is-activated",
    ):
        print(f"sim-ready:   {pin} = {hal_value(pin)}", file=sys.stderr)
    print(
        f"sim-ready:   task_state = {stat.task_state}, estop = {stat.estop}",
        file=sys.stderr,
    )


def leave_estop(linuxcnc, stat, command, timeout, settle):
    """Ask the task to leave E-stop until it does.

    The panel's own release reaches the controller as a 150 ms HAL pulse. At
    boot that pulse can be sent before the task is accepting commands, so the
    panel shows the mushroom out and the controller stays in E-stop. A command
    on the NML channel is queued and re-sent here, which takes the timing out
    of it.
    """
    stat.poll()
    if not stat.estop:
        return
    # Give the session the moment it needs to have iocontrol, halui and the
    # panel all running before asking for anything: the reset sent during the
    # first HAL cycles is what the panel's own pulse loses.
    if settle > 0:
        log(f"waiting {settle:g} s for the session to settle")
        time.sleep(settle)
    deadline = time.monotonic() + timeout
    last_sent = 0.0
    attempt = 0
    while True:
        stat.poll()
        if not stat.estop:
            log("E-stop released")
            return
        now = time.monotonic()
        if now - last_sent >= 1.0:
            attempt += 1
            log(f"asking the controller to leave E-stop (attempt {attempt})")
            command.state(linuxcnc.STATE_ESTOP_RESET)
            last_sent = now
        if now >= deadline:
            print(
                "sim-ready: the controller stayed in E-stop; "
                "is the panel's mushroom released?",
                file=sys.stderr,
            )
            report_blocked(stat)
            sys.exit(1)
        time.sleep(0.1)


def power_on(linuxcnc, stat, command, timeout):
    stat.poll()
    if is_on(linuxcnc, stat):
        return
    command.state(linuxcnc.STATE_ON)
    command.wait_complete(timeout)
    try:
        wait_for(
            stat,
            lambda s: is_on(linuxcnc, s),
            "the machine to power on (POWER ON in the panel)",
            timeout,
        )
    except SystemExit:
        report_blocked(stat)
        raise
    log("machine on")


def set_mode(linuxcnc, stat, command, mode, what, timeout):
    """Ask for a mode and wait for the task to report it.

    `command.wait_complete()` is deliberately not used anywhere here: it waits on
    the command channel's echo, and a mode change is reported by the task state
    anyway. Waiting on what the task says it is doing cannot miss and cannot
    block on a channel the panel is also using.
    """
    command.mode(mode)
    wait_for(stat, lambda s: s.task_mode == mode, what, timeout)


def repeat_until(command, stat, predicate, act, what, timeout):
    """Send `act` until `predicate(stat)` holds, then return.

    For commands that are safe to repeat: a request can arrive while the task is
    still settling and be dropped — the same reason the panel's boot-time E-stop
    pulse does nothing. Do not use it for homing: a second HOME restarts the
    sequence and the joints flip between homed and not, over and over.
    """
    deadline = time.monotonic() + timeout
    sent = 0.0
    while True:
        stat.poll()
        if predicate(stat):
            return
        now = time.monotonic()
        if now - sent >= 2.0:
            act()
            sent = now
        if now >= deadline:
            sys.exit(f"sim-ready: timed out after {timeout:g} s waiting for {what}")
        time.sleep(0.1)


def bring_up(args):
    linuxcnc, stat, command = task(args.timeout, args.ini)
    if args.estop:
        leave_estop(linuxcnc, stat, command, args.timeout, args.settle)
    if args.on:
        power_on(linuxcnc, stat, command, args.timeout)
    if args.home:
        stat.poll()
        if all_homed(stat):
            log("axes already homed")
        else:
            home_axes(linuxcnc, stat, command, args)
    if args.load:
        open_program(linuxcnc, stat, command, args)


def home_axes(linuxcnc, stat, command, args):
    log("homing every axis")
    set_mode(linuxcnc, stat, command, linuxcnc.MODE_MANUAL, "MANUAL mode", args.timeout)
    # One HOME, then wait. Re-sending while a joint is moving restarts the
    # sequence rather than helping it, and the joints then flip between homed and
    # not for as long as the retries keep coming.
    command.home(-1)
    try:
        wait_for(stat, all_homed, "every axis to finish homing", args.timeout)
    except SystemExit:
        print(f"sim-ready: joints not homed: {unhomed(stat)}", file=sys.stderr)
        raise
    log("axes homed")


def open_program(linuxcnc, stat, command, args):
    path = os.path.abspath(args.load)
    if not os.path.exists(path):
        sys.exit(f"sim-ready: no such program: {path}")
    name = os.path.basename(path)
    stat.poll()
    if os.path.basename(stat.file or "") == name:
        log(f"{name} already loaded")
        return
    log(f"opening {name}")
    set_mode(linuxcnc, stat, command, linuxcnc.MODE_AUTO, "AUTO mode", args.timeout)
    repeat_until(
        command,
        stat,
        lambda s: os.path.basename(s.file or "") == name,
        lambda: command.program_open(path),
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
    if args.estop or args.on or args.home or args.load:
        bring_up(args)
    if args.float:
        float_window(args)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except KeyboardInterrupt:
        sys.exit(130)
