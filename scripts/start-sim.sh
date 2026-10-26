#!/usr/bin/env bash
# Start the LiNC simulation with a real LinuxCNC process.
#
#   ./scripts/start-sim.sh                 foreground (Ctrl+C stops LinuxCNC)
#   ./scripts/start-sim.sh --detach        background, PID in $XDG_RUNTIME_DIR/linc/sim.pid;
#                                          the session log is also followed on the
#                                          terminal so LinuxCNC's output stays visible
#
# Optional start-up, in this order (each step implies the one before it):
#   --release-estop   start the simulated cabinet with the E-stop released
#   --power-on        and with POWER ON pressed, so the machine is on
#   --home            home every axis
#   --load [FILE]     load a program; FILE defaults to the demo mill program
#   --float           float the HMI window under Hyprland at 1280x720
#
# --plc starts the ladder PLC service: it exports LINC_PLC=1 and the HMI then
# spawns target/release/linc-plc (owning linc_plc.*) and loads
# hal/linc-plc-postgui.hal. --no-plc forces it off for one run (LINC_PLC=0) even
# when the profile sets [plc] enabled = true. The PLC is off by default, and
# neither flag implies any other start-up step; LINC_PLC=1|0|true|false|on|off in
# the environment does the same as the flag.
#
# --no-sim3d starts the same rig without the 3D simulator: it loads
# configs/sim-no3d.ini (configs/sim.ini minus the linc-sim3d HAL file), and the
# simulator binary is neither required nor built. An explicit LINC_SIM_INI is a
# deliberate choice, so it still wins over the flag's default.
#
# The E-stop and POWER ON keys are panel inputs, so they are asked of the panel
# (`LINC_SIM_READY`) rather than pressed behind its back; start-sim.sh then runs
# scripts/sim-ready.py for the steps that need the controller.
#
# LinuxCNC loads the simulation's software FPGA HAL (hal/rio_sim.hal) and the
# LiNC panel service. LiNC is the [DISPLAY] program; linc-panel is the CNC
# control panel window.
#
# Environment:
#   LINC_SIM_CONFIG   profile (default: configs/sim.toml)
#   LINC_SIM_INI      LinuxCNC INI (default: configs/sim.ini)
#   LINC_BIN          linc binary (default: target/release/linc)
#   LINC_SIM3D_BIN    3D simulator HAL binary (default: target/release/linc-sim3d)
#   LINC_LOG_LEVEL    RUST_LOG for linc/linc-panel (default: the binaries' own
#                     filter — LiNC crates at info, everything else at warn)
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
PROFILE="${LINC_SIM_CONFIG:-$ROOT/configs/sim.toml}"
INI="${LINC_SIM_INI:-$ROOT/configs/sim.ini}"
BIN="${LINC_BIN:-$ROOT/target/release/linc}"
PANEL_BIN="${LINC_PANEL_BIN:-$ROOT/target/release/linc-panel}"
SIM3D_BIN="${LINC_SIM3D_BIN:-$ROOT/target/release/linc-sim3d}"
PLC_BIN="${LINC_PLC_BIN:-$ROOT/target/release/linc-plc}"
RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp}/linc"
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/linc"
PID_FILE="$RUNTIME_DIR/sim.pid"
PANEL_PID_FILE="$RUNTIME_DIR/panel-sim.pid"
LINC_PID_FILE="$RUNTIME_DIR/linc.pid"
PLC_PID_FILE="$RUNTIME_DIR/plc.pid"
LOG_FILE="$STATE_DIR/sim.log"
# `linuxcnc -r` stops the launcher from moving its own stdout/stderr into these
# files, so the session output goes to the terminal (foreground) or to
# $LOG_FILE (detached). Keep the print file only as a fallback for a launcher
# that redirected before we could.
PRINT_FILE="$HOME/linuxcnc_print.txt"
mkdir -p "$RUNTIME_DIR" "$STATE_DIR"

if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
    echo "no graphical session: set DISPLAY or WAYLAND_DISPLAY" >&2
    exit 1
fi
for file in "$PROFILE" "$INI"; do
    if [[ ! -f "$file" ]]; then
        echo "file not found: $file" >&2
        exit 1
    fi
done

DETACH=0
RELEASE_ESTOP=0
POWER_ON=0
HOME_AXES=0
LOAD_PROGRAM=""
FLOAT_WINDOW=0
NO_SIM3D=0
PLC_FLAG=""
READY_TIMEOUT="${LINC_READY_TIMEOUT:-60}"
ARGS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --detach) DETACH=1 ;;
        --release-estop) RELEASE_ESTOP=1 ;;
        --power-on) POWER_ON=1 ;;
        --home) HOME_AXES=1 ;;
        --load)
            LOAD_PROGRAM="$ROOT/configs/gcode/linc-demo-mill.ngc"
            # An optional path: `--load part.ngc`, or nothing for the demo.
            if [[ -n "${2:-}" && "${2:0:1}" != "-" ]]; then
                LOAD_PROGRAM="$2"
                shift
            fi
            ;;
        --load=*) LOAD_PROGRAM="${1#--load=}" ;;
        --float) FLOAT_WINDOW=1 ;;
        --no-sim3d) NO_SIM3D=1 ;;
        --plc) PLC_FLAG=1 ;;
        --no-plc) PLC_FLAG=0 ;;
        --timeout) READY_TIMEOUT="${2:?--timeout needs seconds}"; shift ;;
        *) ARGS+=("$1") ;;
    esac
    shift
done

# --no-sim3d runs the same rig without the 3D simulator. An explicit
# LINC_SIM_INI is a deliberate choice, so it wins over the flag's default.
if (( NO_SIM3D )) && [[ -z "${LINC_SIM_INI:-}" ]]; then
    INI="$ROOT/configs/sim-no3d.ini"
    if [[ ! -f "$INI" ]]; then
        echo "file not found: $INI" >&2
        exit 1
    fi
fi

# The build guard sits below the option parsing because it depends on it: the
# normal rig also needs the simulator binary, --no-sim3d neither needs nor
# builds it.
NEED_SIM3D=1
(( NO_SIM3D )) && NEED_SIM3D=0
if [[ ! -x "$BIN" || ! -x "$PANEL_BIN" || ! -x "$PLC_BIN" || ( $NEED_SIM3D -eq 1 && ! -x "$SIM3D_BIN" ) ]]; then
    echo "building LiNC release binaries..." >&2
    if (( NEED_SIM3D )); then
        (cd "$ROOT" && cargo build --release -p linc-app -p linc-panel -p linc-sim3d -p linc-plc)
    else
        (cd "$ROOT" && cargo build --release -p linc-app -p linc-panel -p linc-plc)
    fi
fi

# The power-up sequence only makes sense whole: POWER ON releases the mushroom
# (as the panel's own key does), and homing or loading needs the machine on.
(( HOME_AXES )) && POWER_ON=1
[[ -n "$LOAD_PROGRAM" ]] && POWER_ON=1
(( POWER_ON )) && RELEASE_ESTOP=1
if (( RELEASE_ESTOP )); then
    if (( POWER_ON )); then
        export LINC_SIM_READY=power
    else
        export LINC_SIM_READY=estop
    fi
fi

export LINC_CONFIG="$PROFILE"
export LINC_PANEL_PID_FILE="$PANEL_PID_FILE"
export LINC_DISPLAY_PID_FILE="$LINC_PID_FILE"
# The PLC is off unless the profile or one of these flags turns it on; the HMI
# reads LINC_PLC first and only then [plc] enabled. The pid file is where both
# the display wrapper and stop-sim.sh find the daemon.
if [[ -n "$PLC_FLAG" ]]; then
    export LINC_PLC="$PLC_FLAG"
fi
export LINC_PLC_BIN="$PLC_BIN"
export LINC_PLC_PID_FILE="$PLC_PID_FILE"
# Only override the filter when asked; the binaries default to LiNC at info and
# third-party crates at warn, so an exported `RUST_LOG=info` here would turn the
# D-Bus stack back on and bury the log.
if [[ -n "${LINC_LOG_LEVEL:-}" ]]; then
    export RUST_LOG="$LINC_LOG_LEVEL"
fi
export PATH="$ROOT/target/release:$ROOT/scripts:$PATH"

if [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
    echo "LiNC/LinuxCNC sim already running (PID $(cat "$PID_FILE"))"
    exit 0
fi
rm -f "$PANEL_PID_FILE" "$LINC_PID_FILE" "$PLC_PID_FILE"

# A session that is killed rather than closed can leave LiNC, halui, milltask and
# io behind even after linuxcncsvr exits. The survivors keep stale diagnostic
# sockets and spam "status buffer is not valid" against the next session, so
# clear them when no LinuxCNC server is running. This script is for the
# simulation; a real machine must not have a leftover panel without LinuxCNC.
if [[ "${LINC_KEEP_STALE:-0}" != 1 ]] && ! pgrep -x linuxcncsvr >/dev/null 2>&1; then
    for name in halui milltask io linuxcnc linc linc-panel linc-plc; do
        if pgrep -x "$name" >/dev/null 2>&1; then
            echo "stopping stale $name process from a killed session"
            pkill -TERM -x "$name" >/dev/null 2>&1 || true
        fi
    done
    sleep 1
    for name in halui milltask io linuxcnc linc linc-panel linc-plc; do
        pkill -KILL -x "$name" >/dev/null 2>&1 || true
    done
fi

# A session that is killed rather than closed leaves the System V semaphores and
# shared memory LinuxCNC created for its NML channels behind. The next session
# then reports "SHMEM: Can't take semaphore" and can no longer reach the task, so
# clear them when no LinuxCNC server is running. `LINC_KEEP_IPC=1` skips this,
# and nothing is removed while a session is alive.
if [[ "${LINC_KEEP_IPC:-0}" != 1 ]] && ! pgrep -x linuxcncsvr >/dev/null 2>&1; then
    stale=0
    while read -r id; do
        [[ -n "$id" ]] && ipcrm -s "$id" >/dev/null 2>&1 && stale=$((stale + 1))
    done < <(ipcs -s 2>/dev/null | awk -v u="$USER" '$3 == u {print $2}')
    while read -r id; do
        [[ -n "$id" ]] && ipcrm -m "$id" >/dev/null 2>&1 && stale=$((stale + 1))
    done < <(ipcs -m 2>/dev/null | awk -v u="$USER" '$3 == u {print $2}')
    if (( stale > 0 )); then
        echo "cleared $stale stale LinuxCNC IPC object(s) from a killed session"
    fi
    rm -f "$HOME/.linuxcnc/lock"
fi

# Everything the simulation needs lives in this checkout. LinuxCNC chdirs to
# the INI directory itself, which is also where it keeps its parameter file;
# start from there so a relative path in this script matches.
cd "$ROOT/configs"

# The steps that need the controller, once it is up. LinuxCNC's [DISPLAY] starts
# the HMI, so the window exists a few seconds after the process does; sim-ready
# waits for it.
sim_ready() {
    local args=(--timeout "$READY_TIMEOUT" --ini "$INI")
    (( RELEASE_ESTOP )) && args+=(--estop)
    (( POWER_ON )) && args+=(--on)
    (( HOME_AXES )) && args+=(--home)
    [[ -n "$LOAD_PROGRAM" ]] && args+=(--load "$LOAD_PROGRAM")
    (( FLOAT_WINDOW )) && args+=(--float)
    if (( ${#args[@]} == 2 )); then
        return 0
    fi
    python3 "$SCRIPT_DIR/sim-ready.py" "${args[@]}"
}

if (( DETACH )); then
    # -r: keep LinuxCNC's own output in $LOG_FILE instead of linuxcnc_print.txt.
    nohup linuxcnc -r "$INI" "${ARGS[@]}" >"$LOG_FILE" 2>&1 &
    PID=$!
    echo "$PID" >"$PID_FILE"
    sleep 2
    if ! kill -0 "$PID" 2>/dev/null; then
        echo "LinuxCNC failed to start; see $LOG_FILE and $PRINT_FILE" >&2
        tail -n 20 "$LOG_FILE" >&2 || true
        tail -n 20 "$PRINT_FILE" >&2 || true
        rm -f "$PID_FILE"
        exit 1
    fi
    echo "LiNC + LinuxCNC sim started (PID $PID)"
    echo "  ini:     $INI"
    echo "  profile: $PROFILE"
    echo "  panel:   $PANEL_PID_FILE"
    echo "  display: $LINC_PID_FILE"
    echo "  log:     $LOG_FILE"
    echo "  stop:    $SCRIPT_DIR/stop-sim.sh"
    if [[ -t 1 ]]; then
        # Keep the operator's terminal useful after the script exits. The tail
        # ends with LinuxCNC; closing the terminal only stops following.
        tail -n +1 -f --pid="$PID" "$LOG_FILE" &
        echo "  output:  following $LOG_FILE (Ctrl+C stops following; the sim keeps running)"
    fi
    if (( RELEASE_ESTOP || FLOAT_WINDOW )); then
        # The session is in the background, so its log is the only place these
        # steps will be read.
        if ! sim_ready 2>&1 | tee -a "$LOG_FILE"; then
            echo "simulation started, but the ready steps failed; see $LOG_FILE" >&2
            exit 1
        fi
    fi
else
    trap '"$SCRIPT_DIR/stop-sim.sh"' EXIT INT TERM
    echo "starting LinuxCNC: $INI"
    echo "  output: this terminal (linuxcnc -r)"
    if (( RELEASE_ESTOP || FLOAT_WINDOW )); then
        # linuxcnc blocks in the foreground, so the ready steps run beside it
        # and report to this terminal.
        sim_ready &
    fi
    # `-r` leaves LinuxCNC's output on this terminal, so a failed start says
    # why instead of only pointing at a print file.
    set +e
    linuxcnc -r "$INI" "${ARGS[@]}"
    status=$?
    set -e
    if (( status != 0 )); then
        echo "LinuxCNC exited with status $status" >&2
    fi
    exit "$status"
fi
