#!/usr/bin/env bash
# Build LiNC Debian packages.
#
# Default: one package per architecture, amd64 and arm64. The host arch builds
# natively; arm64 needs the Rust target and a cross linker. Everything is
# staged under target/debs/<arch>/ and packed with dpkg-deb, so debhelper is not
# required for these packages (debian/ is still there for `dpkg-buildpackage`).
#
#   scripts/build-deb.sh                     # amd64 + arm64
#   scripts/build-deb.sh --arch arm64        # one arch
#   scripts/build-deb.sh --stage             # stage only, no dpkg-deb needed
#   scripts/build-deb.sh --version 0.2.0 --out dist
#
# The LinuxCNC backend links liblinuxcnc/libnml, so the build host needs the
# LinuxCNC development files (or a target sysroot for a cross build) and the
# sibling ../linuxcnc-nml checkout.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

ARCHES=(amd64 arm64)
OUT="target/debs"
VERSION=""
STAGE_ONLY=0
RUN_TESTS=1
JOBS=""

usage() {
    sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        --arch) ARCHES=("$2"); shift 2 ;;
        --arch=*) ARCHES=("${1#*=}"); shift ;;
        --out) OUT="$2"; shift 2 ;;
        --out=*) OUT="${1#*=}"; shift ;;
        --version) VERSION="$2"; shift 2 ;;
        --version=*) VERSION="${1#*=}"; shift ;;
        --stage) STAGE_ONLY=1; shift ;;
        --no-test) RUN_TESTS=0; shift ;;
        --jobs) JOBS="$2"; shift 2 ;;
        -h|--help) usage ;;
        *) echo "unknown argument: $1" >&2; usage ;;
    esac
done

if [ -z "$VERSION" ]; then
    VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
fi
[ -n "$VERSION" ] || { echo "cannot read the workspace version" >&2; exit 1; }

cargo_jobs=()
[ -n "$JOBS" ] && cargo_jobs=(--jobs "$JOBS")
HOST="$(rustc -vV | sed -n 's/^host: //p')"

# `env` with an empty argument list is the clean way to apply an optional
# cross-compilation variable.
run_cargo() {
    local arch="$1"; shift
    local extra=()
    local cross
    cross="$(linker_env_for_arch "$arch")"
    if [ -n "$cross" ]; then
        extra=("$cross")
    fi
    env "${extra[@]}" cargo "$@"
}

target_for_arch() {
    case "$1" in
        amd64|x86_64) echo "x86_64-unknown-linux-gnu" ;;
        arm64|aarch64) echo "aarch64-unknown-linux-gnu" ;;
        *) return 1 ;;
    esac
}

linker_env_for_arch() {
    case "$1" in
        amd64|x86_64) echo "" ;;
        arm64|aarch64) echo "CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc" ;;
    esac
}

require_target() {
    local arch="$1" target="$2"
    if ! rustc --print target-list | grep -qx "$target"; then
        echo "error: rustc does not know $target" >&2
        exit 1
    fi
    if [ "$arch" != "amd64" ] && ! rustup target list --installed 2>/dev/null | grep -qx "$target"; then
        echo "error: Rust target $target is not installed." >&2
        echo "       rustup target add $target" >&2
        exit 1
    fi
    if [ "$arch" = "arm64" ] && ! command -v aarch64-linux-gnu-gcc >/dev/null; then
        echo "error: aarch64 cross linker not found (needed for $target)." >&2
        echo "       On Debian: apt install gcc-aarch64-linux-gnu" >&2
        exit 1
    fi
}

stage() {
    local arch="$1" target="$2" bin_dir="$3"
    local stage_dir="$OUT/$arch/linc_${VERSION}_${arch}"
    rm -rf "$stage_dir"
    install -d "$stage_dir/DEBIAN" \
        "$stage_dir/usr/bin" \
        "$stage_dir/usr/share/linc/configs" \
        "$stage_dir/usr/share/linc/hal" \
        "$stage_dir/usr/share/linc/subroutines" \
        "$stage_dir/usr/share/icons/hicolor/256x256/apps" \
        "$stage_dir/usr/share/applications" \
        "$stage_dir/usr/share/doc/linc"

    install -m755 "$bin_dir/linc" "$stage_dir/usr/bin/linc"
    install -m755 "$bin_dir/linc-panel" "$stage_dir/usr/bin/linc-panel"
    install -m755 "$bin_dir/linc-mcp" "$stage_dir/usr/bin/linc-mcp"

    # The sim profile is the self-contained example; machine profiles come from
    # the vendor, so only the sample and its INI/HAL are shipped.
    install -m644 configs/base.toml configs/sim.toml configs/sim.ini \
        "$stage_dir/usr/share/linc/configs/"
    install -m644 hal/*.hal "$stage_dir/usr/share/linc/hal/"
    cp -a subroutines/probe_basic "$stage_dir/usr/share/linc/subroutines/"
    install -m644 assets/branding/linc-icon-256.png \
        "$stage_dir/usr/share/icons/hicolor/256x256/apps/linc.png"
    install -m644 packaging/linc.desktop \
        "$stage_dir/usr/share/applications/linc.desktop"
    install -m644 README.md "$stage_dir/usr/share/doc/linc/README.md"
    install -m644 debian/copyright "$stage_dir/usr/share/doc/linc/copyright"

    local installed_kb
    installed_kb=$(du -sk "$stage_dir/usr" | cut -f1)

    local depends
    if command -v dpkg-shlibdeps >/dev/null; then
        # Native build only; a cross build cannot inspect the target's shlibs.
        depends="$(dpkg-shlibdeps -O "$stage_dir/usr/bin/linc" 2>/dev/null \
            | sed -n 's/^shlibs:Depends=//p')"
    fi
    if [ -z "${depends:-}" ]; then
        depends="libc6, libgcc-s1, libstdc++6, libgl1, libxkbcommon0, libwayland-client0, libx11-6, libxi6, libxcursor1, libxrandr2, libxcb1"
    fi
    depends="$depends, linuxcnc (>= 2.9) | linuxcnc-uspace"

    cat > "$stage_dir/DEBIAN/control" <<EOF
Package: linc
Version: $VERSION
Architecture: $arch
Maintainer: LiNC contributors <linc@example.invalid>
Installed-Size: $installed_kb
Depends: $depends
Section: misc
Priority: optional
Homepage: https://example.invalid/linc
Description: touch-first HMI for LinuxCNC
 LiNC is a Rust/egui control-panel HMI for LinuxCNC. It reads machine state and
 issues only explicit, validated commands (MDI, probe, tool change, program and
 offset writes); the physical CNC panel remains authoritative for mode, cycle,
 jog, spindle and coolant.
 .
 The package ships three programs: linc (the display), linc-panel (the panel
 service, run separately so a UI crash cannot remove panel control) and linc-mcp
 (a read-only diagnostic server for agents).
 .
 Example profiles are installed under /usr/share/linc; a machine profile is
 supplied by the vendor and selected with --config or LINC_CONFIG.
EOF

    echo "$stage_dir"
}

for arch in "${ARCHES[@]}"; do
    target="$(target_for_arch "$arch")" || {
        echo "error: unsupported architecture $arch (use amd64 or arm64)" >&2
        exit 1
    }
    require_target "$arch" "$target"
    if [ "$arch" != "amd64" ] && [ -z "${LINUXCNC_LIB_DIR:-}" ]; then
        echo "error: $arch needs an arm64 LinuxCNC sysroot for the native NML client." >&2
        echo "       Set LINUXCNC_INCLUDE_DIR and LINUXCNC_LIB_DIR to the target" >&2
        echo "       LinuxCNC headers and libraries, then rerun; do not point them at /usr/lib." >&2
        exit 1
    fi

    echo "==> building $arch ($target)"
    run_cargo "$arch" build --release --locked --target "$target" "${cargo_jobs[@]}"

    bin_dir="target/$target/release"
    if [ "$RUN_TESTS" = 1 ] && [ "$target" = "$HOST" ]; then
        echo "==> testing $arch"
        run_cargo "$arch" test --release --locked --workspace --target "$target" "${cargo_jobs[@]}"
    fi

    stage_dir="$(stage "$arch" "$target" "$bin_dir")"
    if [ "$STAGE_ONLY" = 1 ]; then
        echo "staged $stage_dir"
        continue
    fi
    command -v dpkg-deb >/dev/null || {
        echo "error: dpkg-deb is not installed; use --stage or install dpkg-dev." >&2
        exit 1
    }
    mkdir -p "$OUT"
    deb="$OUT/linc_${VERSION}_${arch}.deb"
    dpkg-deb --build --root-owner-group "$stage_dir" "$deb" >/dev/null
    echo "built $deb"
done
