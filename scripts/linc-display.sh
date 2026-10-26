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
# The mill simulation's post-GUI file nets cnc_panel.sseg_status and
# cnc_panel.probe. A panel-less machine (the lathe sim, whose panel binding is
# not commissioned yet) has no cnc_panel component, so it gets the wiring that
# only touches linc.*, iocontrol and halui pins.
POSTGUI="$LINC_DIR/hal/linc-sim-postgui.hal"
# `halcmd show pin` reports a missing pin on stderr and still exits 0, so test
# the listing on stdout. Redirecting stderr and trusting the status picked the
# mill file on the lathe, where the post-GUI HAL then failed on
# cnc_panel.sseg_status.
if ! halcmd show pin cnc_panel.sseg_status 2>/dev/null | grep -q 'cnc_panel\.sseg_status'; then
    POSTGUI="$LINC_DIR/hal/linc-lathe-postgui.hal"
fi
halcmd -f "$POSTGUI"

wait "$LINC_PID"
