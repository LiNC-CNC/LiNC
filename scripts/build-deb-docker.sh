#!/usr/bin/env bash
# Build LiNC .debs for Debian releases in containers.
#
#   scripts/build-deb-docker.sh                  # bookworm + trixie, amd64
#   scripts/build-deb-docker.sh trixie           # one release
#   scripts/build-deb-docker.sh bookworm --with-tests
#   scripts/build-deb-docker.sh trixie --source-build   # via dpkg-buildpackage
#   scripts/build-deb-docker.sh bookworm --shell        # poke around in the image
#
# Each release gets its own image (packaging/debbuild/Dockerfile) and its own
# cargo cache under target/debbuild/<release>/, so a bookworm object never
# reaches a trixie link. Packages land in dist/<release>/.
#
# The checkout must sit beside its `linuxcnc-nml` sibling: the NML client is a
# path dependency and the whole parent directory is mounted inside at the same
# path, so `../linuxcnc-nml` resolves there as it does on the host.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PARENT="$(dirname "$ROOT")"
RELEASES=(bookworm trixie)
ARCH="amd64"
TESTS=0
SHELL_MODE=0
SOURCE_BUILD=0
KNOWN=(bookworm trixie)

usage() {
    sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        bookworm|trixie) RELEASES=("$1"); shift ;;
        --arch) ARCH="$2"; shift 2 ;;
        --arch=*) ARCH="${1#*=}"; shift ;;
        --with-tests) TESTS=1; shift ;;
        --source-build) SOURCE_BUILD=1; shift ;;
        --shell) SHELL_MODE=1; shift ;;
        -h|--help) usage ;;
        *) echo "unknown argument: $1 (releases: ${KNOWN[*]})" >&2; usage ;;
    esac
done

if [ "$SOURCE_BUILD" = 1 ] && [ "${#RELEASES[@]}" -gt 1 ]; then
    # dpkg-buildpackage writes beside the checkout, not into dist/: one release
    # at a time keeps that predictable.
    echo "error: --source-build builds one release at a time" >&2
    exit 2
fi

if [ ! -d "$PARENT/linuxcnc-nml" ]; then
    echo "error: $PARENT/linuxcnc-nml is missing; the NML client is a path dependency" >&2
    exit 1
fi

for release in "${RELEASES[@]}"; do
    image="linc-deb-$release"
    cache="$ROOT/target/debbuild/$release"
    mkdir -p "$cache/cargo" "$cache/target" "$cache/home"

    echo "==> $release: building $image"
    docker build -q -t "$image" \
        --build-arg "RELEASE=$release" \
        -f "$ROOT/packaging/debbuild/Dockerfile" "$ROOT/packaging/debbuild" >/dev/null

    run=(
        --rm
        --user "$(id -u):$(id -g)"
        -v "$PARENT:$PARENT"
        -v "$cache/cargo:/cache/cargo"
        # build-deb.sh stages from target/<triple>/release, and debian/rules
        # builds into target/release: both are the mounted cache.
        -v "$cache/target:$ROOT/target"
        -v "$cache/home:/cache/home"
        -e CARGO_HOME=/cache/cargo
        -e HOME=/cache/home
        -w "$ROOT"
        "$image"
    )

    if [ "$SHELL_MODE" = 1 ]; then
        exec docker run -it "${run[@]}" bash
    fi

    if [ "$SOURCE_BUILD" = 1 ]; then
        # debhelper path. -d: the rust toolchain is rustup's, so the apt
        # build-dependency on rustc (>= 1.95) can never be satisfied here.
        cmd="dpkg-buildpackage -b -d -uc -us"
        [ "$TESTS" = 1 ] || cmd="DEB_BUILD_OPTIONS=nocheck $cmd"
        echo "==> $release: $cmd"
        docker run "${run[@]}" bash -c "$cmd"
        mkdir -p "$ROOT/dist/$release"
        # dpkg-buildpackage writes beside the checkout, not into dist/.
        mv "$PARENT"/linc*.deb "$ROOT/dist/$release/"/ 2>/dev/null || true
        rm -f "$PARENT"/linc_*.changes "$PARENT"/linc_*.buildinfo
    else
        echo "==> $release: staging and packaging"
        out="dist/$release"
        args=(--arch "$ARCH" --out "$out")
        [ "$TESTS" = 1 ] || args+=(--no-test)
        docker run "${run[@]}" ./scripts/build-deb.sh "${args[@]}"
    fi

    for deb in "$ROOT/dist/$release"/linc_*.deb; do
        [ -e "$deb" ] || continue
        echo "==> $release: $(basename "$deb") $(du -h "$deb" | cut -f1)"
        docker run --rm -v "$ROOT/dist/$release:/d:ro" "$image" \
            dpkg-deb -f "/d/$(basename "$deb")" Depends
    done
done
