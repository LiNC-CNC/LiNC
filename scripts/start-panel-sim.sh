#!/usr/bin/env bash
# Start the separate simulated CNC control panel window.
#
#   ./scripts/start-panel-sim.sh            foreground
#   ./scripts/start-panel-sim.sh --detach   background
#
# This is the companion panel process, not part of the HMI shell. In standalone
# mode (no LinuxCNC HAL) the window demonstrates the panel controls and the
# cnc_panel pin surface; it is not wired to the standalone HMI's simulated
# machine. For an integrated run, start linc-panel under LinuxCNC's HAL and run
# the HMI with --backend linuxcnc.
#
# Environment:
#   LINC_SIM_CONFIG       profile (default: configs/sim.toml)
#   LINC_PANEL_BIN        panel binary (default: target/release/linc-panel)
#   LINC_PANEL_COMPONENT  HAL component name override (default: none)
#   LINC_SIM_HAL=1        create cnc_panel.* pins (default: --no-hal)
#   LINC_LOG_LEVEL        RUST_LOG (default: warn)
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
PROFILE="${LINC_SIM_CONFIG:-$ROOT/configs/sim.toml}"
BIN="${LINC_PANEL_BIN:-$ROOT/target/release/linc-panel}"
RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp}/linc"
STATE_DIR="${XDG_STATE_HOME:-$HOME/.local/state}/linc"
PID_FILE="$RUNTIME_DIR/panel-sim.pid"
LOG_FILE="$STATE_DIR/panel-sim.log"
IPC_BASE="${LINC_SIM_IPC:-$RUNTIME_DIR/panel-sim}"
export LINC_SIM_IPC="$IPC_BASE"
mkdir -p "$RUNTIME_DIR" "$STATE_DIR"

if [[ -z "${DISPLAY:-}" && -z "${WAYLAND_DISPLAY:-}" ]]; then
    echo "no graphical session: set DISPLAY or WAYLAND_DISPLAY" >&2
    exit 1
fi
if [[ ! -f "$PROFILE" ]]; then
    echo "profile not found: $PROFILE" >&2
    exit 1
fi
if [[ ! -x "$BIN" ]]; then
    echo "building linc-panel release binary..." >&2
    (cd "$ROOT" && cargo build --release -p linc-panel)
fi

DETACH=0
if [[ "${1:-}" == "--detach" ]]; then
    DETACH=1
    shift
fi

ARGS=(--config "$PROFILE" --sim --window)
if [[ "${LINC_SIM_HAL:-0}" != "1" ]]; then
    ARGS+=(--no-hal)
fi
export LINC_CONFIG="$PROFILE"
export RUST_LOG="${LINC_LOG_LEVEL:-warn}"

if (( DETACH )); then
    if [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
        echo "panel sim already running (PID $(cat "$PID_FILE"))"
        exit 0
    fi
    nohup "$BIN" "${ARGS[@]}" "$@" >"$LOG_FILE" 2>&1 &
    PID=$!
    echo "$PID" >"$PID_FILE"
    sleep 0.7
    if ! kill -0 "$PID" 2>/dev/null; then
        echo "panel sim failed to start; see $LOG_FILE" >&2
        rm -f "$PID_FILE"
        exit 1
    fi
    echo "panel sim started (PID $PID)"
    echo "  profile: $PROFILE"
    echo "  log:     $LOG_FILE"
    echo "  stop:    $SCRIPT_DIR/stop-sim.sh"
else
    exec "$BIN" "${ARGS[@]}" "$@"
fi
