#!/usr/bin/env bash
# LiNC 3D simulator: HAL startup wrapper.
#
# hal/linc-sim3d.hal starts this script with
#   loadusr -Wn linc-sim3d ../scripts/sim3d-hal.sh
# LinuxCNC runs HAL files from the INI's directory (configs/), so this script
# resolves the repository root from its own path rather than from the working
# directory, then execs the simulator with paths relative to that root.
#
# The simulator creates the linc-sim3d HAL component and samples the machine
# position. When this host has a display it also opens the 3D window, in the
# same process, so the rig shows what the machine is being told; closing the
# window keeps sampling and never interrupts a probe move. A headless rig sets
# LINC_SIM3D_WINDOW=0 and gets no window; LINC_SIM3D_WINDOW=1 (or --window)
# forces one even when no display is detected. This script passes the
# environment through unchanged and needs no other logic for that.

set -euo pipefail

script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/.." && pwd)

for profile in release debug; do
    binary="$repo_root/target/$profile/linc-sim3d"
    if [[ -x "$binary" ]]; then
        cd -- "$repo_root"
        exec "$binary" --hal --config configs/sim3d.toml
    fi
done

echo "sim3d-hal.sh: no linc-sim3d binary found" >&2
echo "  looked for: $repo_root/target/release/linc-sim3d" >&2
echo "              $repo_root/target/debug/linc-sim3d" >&2
echo "  build it with: cargo build --release -p linc-sim3d" >&2
exit 1
