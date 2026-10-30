#!/usr/bin/env bash
# Build the LiNC simulation AppImage.
#
# The AppImage runs `linc` (HMI) and `linc-panel` (simulated CNC panel) as two
# windows and needs no LinuxCNC installation, no HAL and no serial port. It is
# the demo build, not the machine build.
#
#   scripts/build-appimage.sh                 # zig-built, glibc 2.36 floor
#   scripts/build-appimage.sh --native        # host toolchain (newer glibc)
#   scripts/build-appimage.sh --no-build      # reuse the last stage
#   scripts/build-appimage.sh --out dist
#
# The HMI is built with `--no-default-features`, which drops the native NML
# client. The panel keeps its default features; it links no LinuxCNC library.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
OUT="$ROOT/target/appimage"
GLIBC="2.36"
MODE="auto"
BUILD=1
STRIP=1

usage() {
    sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        --native) MODE="native"; shift ;;
        --no-build) BUILD=0; shift ;;
        --glibc) GLIBC="$2"; shift 2 ;;
        --glibc=*) GLIBC="${1#*=}"; shift ;;
        --out) OUT="$2"; shift 2 ;;
        --out=*) OUT="${1#*=}"; shift ;;
        --version) VERSION="$2"; shift 2 ;;
        --version=*) VERSION="${1#*=}"; shift ;;
        --no-strip) STRIP=0; shift ;;
        -h|--help) usage ;;
        *) echo "unknown argument: $1" >&2; exit 1 ;;
    esac
done

[ -n "$VERSION" ] || { echo "cannot read the workspace version" >&2; exit 1; }
mkdir -p "$OUT"
OUT="$(cd "$OUT" && pwd)"

BIN_DIR="$ROOT/target/release"
if (( BUILD )); then
    if [ "$MODE" = "auto" ]; then
        if command -v zig >/dev/null; then
            MODE="zig"
        else
            MODE="native"
            echo "warning: zig not found; building against the host glibc" >&2
        fi
    fi
    if [ "$MODE" = "zig" ]; then
        command -v zig >/dev/null || { echo "error: zig is not installed" >&2; exit 1; }
        ZIGCC="$OUT/zigcc"
        cat >"$ZIGCC" <<EOF
#!/bin/sh
exec zig cc -target x86_64-linux-gnu.$GLIBC "\$@"
EOF
        chmod +x "$ZIGCC"
        export CARGO_TARGET_DIR="$OUT/build"
        export CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER="$ZIGCC"
        export CC_x86_64_unknown_linux_gnu="$ZIGCC"
        export CXX_x86_64_unknown_linux_gnu="$ZIGCC"
        BIN_DIR="$CARGO_TARGET_DIR/release"
        echo "building for glibc $GLIBC with zig"
        cargo build --release -p linc-app --no-default-features
        cargo build --release -p linc-panel
    else
        echo "building with the host toolchain (glibc $(ldd --version | head -1 | awk '{print $NF}'))"
        cargo build --release -p linc-app --no-default-features
        cargo build --release -p linc-panel
    fi
fi

# --no-build reuses whatever the last build produced. Prefer the zig output
# unless the caller asked for the host toolchain.
if [ "$MODE" != "native" ] && [ -x "$OUT/build/release/linc" ]; then
    BIN_DIR="$OUT/build/release"
fi

for bin in linc linc-panel; do
    [ -x "$BIN_DIR/$bin" ] || { echo "error: $BIN_DIR/$bin is missing; drop --no-build" >&2; exit 1; }
done

APPDIR="$OUT/LiNC.AppDir"
SHARE="$APPDIR/usr/share/linc"
rm -rf "$APPDIR"
install -d "$APPDIR/usr/bin" "$SHARE" \
    "$APPDIR/usr/share/icons/hicolor/256x256/apps" \
    "$APPDIR/usr/share/applications" \
    "$APPDIR/usr/share/doc/linc"

install -m755 "$BIN_DIR/linc" "$APPDIR/usr/bin/linc"
install -m755 "$BIN_DIR/linc-panel" "$APPDIR/usr/bin/linc-panel"

# The demo must not drag the native LinuxCNC client into the archive. A build
# that still links it came from the default feature set, not this script.
if ldd "$APPDIR/usr/bin/linc" 2>/dev/null | grep -Eq 'libnml|liblinuxcnc|libtooldata'; then
    echo "error: linc still links the LinuxCNC client; rebuild with --no-default-features" >&2
    exit 1
fi

# The simulation profile and everything its INI names. The AppImage keeps the
# same layout as the repository so relative paths resolve after AppRun copies
# the tree to a writable directory.
install -d "$SHARE/configs" "$SHARE/hal" "$SHARE/subroutines"
install -m644 configs/base.toml configs/sim.toml configs/sim.ini \
    "$SHARE/configs/"
[ -f configs/tool.tbl ] && install -m644 configs/tool.tbl "$SHARE/configs/"
[ -f configs/sim.var ] && install -m644 configs/sim.var "$SHARE/configs/"
[ -f configs/rio_sim.py ] && install -m644 configs/rio_sim.py "$SHARE/configs/"
cp -a configs/gcode "$SHARE/configs/gcode"
cp -a hal/. "$SHARE/hal/"
cp -a subroutines/probe_basic "$SHARE/subroutines/probe_basic"

install -m644 assets/branding/linc-icon-256.png "$APPDIR/linc.png"
install -m644 assets/branding/linc-icon-256.png \
    "$APPDIR/usr/share/icons/hicolor/256x256/apps/linc.png"
install -m755 packaging/appimage/AppRun "$APPDIR/AppRun"
install -m644 packaging/appimage/linc.desktop "$APPDIR/linc.desktop"
install -m644 packaging/appimage/linc.desktop \
    "$APPDIR/usr/share/applications/linc.desktop"
install -m644 README.md "$APPDIR/usr/share/doc/linc/README.md"
[ -f debian/copyright ] && install -m644 debian/copyright \
    "$APPDIR/usr/share/doc/linc/copyright"

# The repository carries a few files at 0600; the AppImage is read by whatever
# user mounts it, so normalise the tree before packaging.
chmod -R a+rX "$APPDIR"
chmod a+rx "$APPDIR/AppRun"

if (( STRIP )) && command -v strip >/dev/null; then
    strip --strip-unneeded "$APPDIR/usr/bin/linc" "$APPDIR/usr/bin/linc-panel" \
        2>/dev/null || true
fi

TOOL="$OUT/appimagetool-x86_64.AppImage"
if command -v appimagetool >/dev/null; then
    TOOL="$(command -v appimagetool)"
else
    if [ ! -x "$TOOL" ]; then
        echo "downloading appimagetool"
        curl -fsSL --retry 3 --connect-timeout 20 \
            -o "$TOOL" \
            "https://github.com/AppImage/appimagetool/releases/download/continuous/appimagetool-x86_64.AppImage"
        chmod +x "$TOOL"
    fi
fi

APPIMAGE="$OUT/LiNC-${VERSION}-x86_64.AppImage"
rm -f "$APPIMAGE"
ARCH=x86_64 APPIMAGE_EXTRACT_AND_RUN=1 \
    "$TOOL" --no-appstream "$APPDIR" "$APPIMAGE" >/dev/null

if [ ! -f "$APPIMAGE" ]; then
    echo "error: appimagetool did not produce $APPIMAGE" >&2
    exit 1
fi

FLOOR="$(objdump -T "$APPDIR/usr/bin/linc" 2>/dev/null \
    | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1 || true)"
echo
echo "AppImage: $APPIMAGE"
echo "size:     $(du -h "$APPIMAGE" | cut -f1)"
[ -n "$FLOOR" ] && echo "glibc:    needs $FLOOR or newer"
echo "run:      $APPIMAGE"
echo "extract:  $APPIMAGE --appimage-extract-and-run"
