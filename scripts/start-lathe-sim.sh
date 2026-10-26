#!/usr/bin/env bash
# Start the LiNC lathe simulation (Smartlathe SL36 profile).
#
#   ./scripts/start-lathe-sim.sh                 foreground
#   ./scripts/start-lathe-sim.sh --detach        background; stop with
#                                                ./scripts/stop-sim.sh
#
# Start-up flags are the mill script's, minus the 3D simulator:
#   --release-estop  --power-on  --home  --load FILE  --float  --timeout SECS
#
# A separate script, not a flag on start-sim.sh: the lathe has its own INI and
# profile, never loads the 3D simulator, has no default demo program, and its
# panel HAL binding is not commissioned yet. Everything after the defaults is
# shared - this wrapper exports the lathe defaults and runs the mill script's
# lifecycle, so stop-sim.sh, the stale-process cleanup and the ready steps stay
# in one place.
#
# The INI boots LiNC as the display against Probe Basic's lathe simulation HAL
# and subroutines (a sibling checkout), so that checkout must exist.
#
# Environment:
#   LINC_SIM_CONFIG   profile (default: configs/lathe-sim.toml)
#   LINC_SIM_INI      LinuxCNC INI (default: configs/lathe-sim.ini)
#   LINC_BIN, LINC_LOG_LEVEL, LINC_READY_TIMEOUT as in start-sim.sh
set -euo pipefail

SCRIPT_DIR="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd -- "$SCRIPT_DIR/.." && pwd)"
PROFILE="${LINC_SIM_CONFIG:-$ROOT/configs/lathe-sim.toml}"
INI="${LINC_SIM_INI:-$ROOT/configs/lathe-sim.ini}"
SIBLING="$ROOT/../probe_basic_dev"

for file in "$PROFILE" "$INI" "$ROOT/configs/lathe-sim.tbl"; do
    if [[ ! -f "$file" ]]; then
        echo "file not found: $file" >&2
        exit 1
    fi
done
if [[ ! -d "$SIBLING" ]]; then
    echo "lathe sim needs the Probe Basic checkout at $SIBLING" >&2
    echo "(it provides the simulated lathe HAL and subroutines)" >&2
    exit 1
fi

# The mill script falls back to the demo mill program for a bare --load; the
# lathe has no such program, so require an explicit file. sim-ready resolves the
# path after chdir-ing into the INI directory, so make it absolute here: a
# relative path would otherwise become configs/<path> a second time.
ARGS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --load)
            if [[ $# -lt 2 || "${2:0:1}" == "-" ]]; then
                echo "--load needs a G-code file for the lathe (there is no demo program)" >&2
                exit 1
            fi
            ARGS+=(--load "$(realpath "$2")")
            shift 2
            ;;
        --load=*)
            ARGS+=("--load=$(realpath "${1#--load=}")")
            shift
            ;;
        *)
            ARGS+=("$1")
            shift
            ;;
    esac
done

# A broken profile should fail before LinuxCNC starts, not as an HMI that
# cannot attach. The binary may not exist yet on a fresh checkout; start-sim.sh
# builds it in that case.
BIN="${LINC_BIN:-$ROOT/target/release/linc}"
if [[ -x "$BIN" ]]; then
    if ! "$BIN" --config "$PROFILE" --check-config >/dev/null 2>&1; then
        echo "profile check failed; run:" >&2
        echo "  $BIN --config $PROFILE --check-config" >&2
        exit 1
    fi
fi

# The INI loads the simulator panel (profile panel_window) and wires the
# lathe-relevant controls; keys without a lathe function stay greyed.
echo "lathe sim: simulator panel window with the lathe-relevant controls; turret/live-tool keys are not wired yet" >&2

export LINC_SIM_CONFIG="$PROFILE"
export LINC_SIM_INI="$INI"
# The panel boots in AUTO; halui's steady mode input can miss the boot edge, so
# have sim-ready put the task in AUTO explicitly (cycle start needs it).
export LINC_SIM_AUTO=1
exec "$ROOT/scripts/start-sim.sh" --no-sim3d "${ARGS[@]}"
