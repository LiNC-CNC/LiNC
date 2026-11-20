#!/bin/sh
# Install LiNC's runtime dependencies on a Debian 12 (bookworm) or 13 (trixie)
# machine that already runs LinuxCNC.
#
#   packaging/install-deps.sh --dry-run    # show the apt command
#   packaging/install-deps.sh              # install the libraries
#
# LinuxCNC is not installed here: it comes from linuxcnc.org or the
# distribution, and its packaging already provides libnml, liblinuxcncini,
# libtooldata and linuxcnc_var, which the HMI links against.
#
# The list is the set glutin and eframe open at runtime with dlopen. They never
# appear as ELF NEEDED entries, so a package that relied on dpkg-shlibdeps alone
# would install cleanly and then fail to open a window.
set -eu

LIBS="libgl1 libegl1 libxkbcommon0 libxkbcommon-x11-0 \
libwayland-client0 libwayland-cursor0 libwayland-egl1 \
libx11-6 libxi6 libxcursor1 libxrandr2 libxcb1"

DRY_RUN=0
for arg in "$@"; do
    case "$arg" in
        --dry-run) DRY_RUN=1 ;;
        -h|--help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "unknown argument: $arg" >&2; exit 2 ;;
    esac
done

# 1. This is the Debian side of the house. Say so plainly rather than failing on
#    a missing apt-get two steps later.
if ! command -v apt-get >/dev/null 2>&1; then
    echo "error: no apt-get here; this script installs Debian packages" >&2
    exit 1
fi

release=""
if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    release="${VERSION_ID:-}"
    if [ "${ID:-}" != "debian" ]; then
        echo "warning: this is ${PRETTY_NAME:-an unknown system}, not Debian" >&2
    fi
    case "$release" in
        12|13) ;;
        "") echo "warning: cannot read the Debian release from /etc/os-release" >&2 ;;
        *) echo "warning: Debian $release is untested; bookworm and trixie are the targets" >&2 ;;
    esac
fi

# 2. LinuxCNC has to be there already: the runtime libraries come with it.
if ! dpkg-query -W -f='${Status}' linuxcnc-uspace 2>/dev/null | grep -q "install ok installed" \
    && ! command -v linuxcnc >/dev/null 2>&1; then
    cat >&2 <<'EOF'
error: LinuxCNC is not installed.

LiNC is a display for a running LinuxCNC session and does not install the
controller. Install it first (linuxcnc-uspace from this distribution, or the
linuxcnc.org repository for your release), then run this script again.
EOF
    exit 1
fi
installed="$(dpkg-query -W -f='${Version}' linuxcnc-uspace 2>/dev/null || true)"

# 3. Install the libraries. --no-install-recommends keeps it to what the window
#    actually needs; fonts come with the desktop or with linuxcnc itself.
if [ "$DRY_RUN" = 1 ]; then
    echo "would run:"
    echo "  apt-get update"
    echo "  apt-get install -y --no-install-recommends $LIBS"
    exit 0
fi

if [ "$(id -u)" -ne 0 ]; then
    if command -v sudo >/dev/null 2>&1; then
        exec sudo -- "$0" "$@"
    fi
    echo "error: run as root (or install sudo)" >&2
    exit 1
fi

echo "==> apt-get update"
apt-get update

echo "==> installing LiNC runtime libraries"
# shellcheck disable=SC2086  # LIBS is a word list on purpose
apt-get install -y --no-install-recommends $LIBS

# 4. Prove it landed rather than assuming: a package name that does not exist on
#    this release makes apt fail loudly above, but a partial install would not.
missing=""
for lib in $LIBS; do
    if ! dpkg-query -W -f='${Status}' "$lib" 2>/dev/null | grep -q "install ok installed"; then
        missing="$missing $lib"
    fi
done
if [ -n "$missing" ]; then
    echo "error: still missing:$missing" >&2
    exit 1
fi

echo
echo "ok: runtime dependencies installed${installed:+ (LinuxCNC $installed)}"
echo "next: sudo apt install ./linc_<version>_$(dpkg --print-architecture).deb"
