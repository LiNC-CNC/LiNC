#!/bin/sh
# LinuxCNC [DISPLAY] entry point for the LiNC simulation.
#
# LinuxCNC invokes displays as: <display> -ini <ini-file> [args]
# The HMI attaches to the running LinuxCNC session through the native NML
# backend. After the linc.* HAL pins exist, this wrapper loads the post-GUI HAL.
set -e

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$(readlink -f "$0")")" && pwd)
LINC_DIR=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
BIN="${LINC_BIN:-$LINC_DIR/target/release/linc}"
export LINC_CONFIG="${LINC_CONFIG:-$LINC_DIR/configs/sim.toml}"

INI=""
while [ "$#" -gt 0 ]; do
    case "$1" in
        -ini)
            [ "$#" -ge 2 ] || { echo "linc-display: -ini needs a file" >&2; exit 2; }
            INI=$2
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done

if [ ! -x "$BIN" ]; then
    echo "linc-display: missing $BIN (run cargo build --release)" >&2
    exit 1
fi
if [ -z "$INI" ]; then
    echo "linc-display: LinuxCNC did not pass -ini" >&2
    exit 2
fi

"$BIN" --config "$LINC_CONFIG" --ini "$INI" --backend linuxcnc &
LINC_PID=$!
if [ -n "${LINC_DISPLAY_PID_FILE:-}" ]; then
    printf '%s\n' "$LINC_PID" >"$LINC_DISPLAY_PID_FILE"
fi
trap 'kill "$LINC_PID" 2>/dev/null || true; [ -z "${LINC_DISPLAY_PID_FILE:-}" ] || rm -f "$LINC_DISPLAY_PID_FILE"' EXIT INT TERM

# The HMI creates linc.* during startup. Wait for status-display before wiring
# the post-GUI nets. A stale session without LinuxCNC is reported by linc.
for _ in $(seq 1 100); do
    if halcmd show pin linc.status-display >/dev/null 2>&1; then
        break
    fi
    sleep 0.1
done
if ! halcmd show pin linc.status-display >/dev/null 2>&1; then
    echo "linc-display: linc.* HAL pins did not appear" >&2
    exit 1
fi
# The mill simulation's post-GUI file nets cnc_panel.probe to the software
# FPGA (rio.sim-probe), which the lathe does not have. Pick by profile rather
# than by pins: the lathe now loads a panel service, so cnc_panel exists in
# both machines and a pin test cannot tell them apart.
POSTGUI="$LINC_DIR/hal/linc-sim-postgui.hal"
if [ -n "${LINC_CONFIG:-}" ] && [ -f "$LINC_CONFIG" ] && \
   grep -Eq '^[[:space:]]*gui[[:space:]]*=[[:space:]]*"lathe"' "$LINC_CONFIG"; then
    POSTGUI="$LINC_DIR/hal/linc-lathe-postgui.hal"
fi
halcmd -f "$POSTGUI"

# --- ladder PLC -------------------------------------------------------------
# The PLC is off unless the profile or start-sim.sh turned it on, and then the
# HMI has already spawned the daemon and written its pid. There is no config
# parsing here: the pid file is the switch and linc_plc.alive is the proof that
# the daemon owns its pins. A disabled PLC costs one test and nothing else.
if [ -n "${LINC_PLC_PID_FILE:-}" ]; then
    PLC_PID_FILE=$LINC_PLC_PID_FILE
elif [ -n "${XDG_RUNTIME_DIR:-}" ]; then
    PLC_PID_FILE="$XDG_RUNTIME_DIR/linc/plc.pid"
else
    PLC_PID_FILE=/tmp/linc-plc.pid
fi
if [ -f "$PLC_PID_FILE" ]; then
    PLC_READY=0
    # The daemon creates its component before hal_ready(), so the pin is there
    # as soon as it starts scanning; 5 s covers a cold project compile.
    for _ in $(seq 1 50); do
        if halcmd show pin linc_plc.alive >/dev/null 2>&1; then
            PLC_READY=1
            break
        fi
        sleep 0.1
    done
    if [ "$PLC_READY" -eq 1 ]; then
        halcmd -f "$LINC_DIR/hal/linc-plc-postgui.hal"
    else
        echo "linc-display: PLC requested ($PLC_PID_FILE) but linc_plc.alive did not appear; skipped hal/linc-plc-postgui.hal, the HMI keeps running" >&2
    fi
fi

wait "$LINC_PID"
