#!/usr/bin/env bash
# Stop LiNC simulation processes started by start-sim.sh / start-panel-sim.sh.
set -euo pipefail

RUNTIME_DIR="${XDG_RUNTIME_DIR:-/tmp}/linc"
IPC_BASE="${LINC_SIM_IPC:-$RUNTIME_DIR/panel-sim}"
STATUS=0

stop_pid() {
    local name=$1 pid_file=$2
    if [[ ! -f "$pid_file" ]]; then
        return 0
    fi
    local pid
    pid="$(cat "$pid_file" 2>/dev/null || true)"
    if [[ -z "$pid" ]] || ! kill -0 "$pid" 2>/dev/null; then
        rm -f "$pid_file"
        return 0
    fi
    echo "stopping $name (PID $pid)..."
    kill -TERM "$pid" 2>/dev/null || true
    for _ in $(seq 1 200); do
        if ! kill -0 "$pid" 2>/dev/null; then
            rm -f "$pid_file"
            echo "$name stopped"
            return 0
        fi
        sleep 0.1
    done
    echo "$name did not exit; sending KILL" >&2
    kill -KILL "$pid" 2>/dev/null || true
    rm -f "$pid_file"
    STATUS=1
}

stop_pid "LiNC display" "$RUNTIME_DIR/linc.pid"
stop_pid "LinuxCNC sim" "$RUNTIME_DIR/sim.pid"
stop_pid "panel sim" "$RUNTIME_DIR/panel-sim.pid"
rm -f "$IPC_BASE.state.json" "$IPC_BASE.display.json"
exit "$STATUS"
