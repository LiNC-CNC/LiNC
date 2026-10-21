#!/usr/bin/env bash
# Start the LiNC simulation with a real LinuxCNC process.
#
#   ./scripts/start-sim.sh                 foreground (Ctrl+C stops LinuxCNC)
#   ./scripts/start-sim.sh --detach        background, PID in $XDG_RUNTIME_DIR/linc/sim.pid
#
# LinuxCNC loads the simulation's software FPGA HAL (hal/rio_sim.hal) and the
# LiNC panel service. LiNC is the [DISPLAY] program; linc-panel is the CNC
# control panel window.
#
# Environment:
#   LINC_SIM_CONFIG   profile (default: configs/sim.toml)
#   LINC_SIM_INI      LinuxCNC INI (default: configs/sim.ini)
#   LINC_BIN          linc binary (default: target/release/linc)
#   LINC_LOG_LEVEL    RUST_LOG for linc/linc-panel (default: the binaries' own
#                     filter — LiNC crates at info, everything else at warn)
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
PROFILE="${LINC_SIM_CONFIG:-$ROOT/configs/sim.toml}"
INI="${LINC_SIM_INI:-$ROOT/configs/sim.ini}"
BIN="${LINC_BIN:-$ROOT/target/release/linc}"
PANEL_BIN="${LINC_PANEL_BIN:-$ROOT/target/release/linc-panel}"
RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp}/linc"
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/linc"
PID_FILE="$RUNTIME_DIR/sim.pid"
PANEL_PID_FILE="$RUNTIME_DIR/panel-sim.pid"
LINC_PID_FILE="$RUNTIME_DIR/linc.pid"
LOG_FILE="$STATE_DIR/sim.log"
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
if [[ ! -x "$BIN" || ! -x "$PANEL_BIN" ]]; then
    echo "building LiNC release binaries..." >&2
    (cd "$ROOT" && cargo build --release -p linc-app -p linc-panel)
fi

DETACH=0
if [[ "${1:-}" == "--detach" ]]; then
    DETACH=1
    shift
fi

export LINC_CONFIG="$PROFILE"
export LINC_PANEL_PID_FILE="$PANEL_PID_FILE"
export LINC_DISPLAY_PID_FILE="$LINC_PID_FILE"
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
rm -f "$PANEL_PID_FILE" "$LINC_PID_FILE"

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

if (( DETACH )); then
    nohup linuxcnc "$INI" "$@" >"$LOG_FILE" 2>&1 &
    PID=$!
    echo "$PID" >"$PID_FILE"
    sleep 2
    if ! kill -0 "$PID" 2>/dev/null; then
        echo "LinuxCNC failed to start; see $LOG_FILE" >&2
        tail -n 20 "$LOG_FILE" >&2 || true
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
else
    trap '"$SCRIPT_DIR/stop-sim.sh"' EXIT INT TERM
    linuxcnc "$INI" "$@"
fi
