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
# The image is reused when it is already loaded and carries the current
# Dockerfile hash, so a machine that received one with `docker save ... | ssh ...
# docker load` starts building without the apt and rustup round trip. --rebuild
# forces the build anyway.
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
REBUILD=0
DOCKERFILE="$ROOT/packaging/debbuild/Dockerfile"
KNOWN=(bookworm trixie)

usage() {
    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        bookworm|trixie) RELEASES=("$1"); shift ;;
        --arch) ARCH="$2"; shift 2 ;;
        --arch=*) ARCH="${1#*=}"; shift ;;
        --with-tests) TESTS=1; shift ;;
        --source-build) SOURCE_BUILD=1; shift ;;
        --rebuild) REBUILD=1; shift ;;
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

# Reuse a loaded image unless its Dockerfile changed: the apt install and the
# rustup download are the slow parts of a container build. The image carries the
# hash of the Dockerfile it was built from, because mtimes do not survive a
# fresh clone or a docker save/load round trip.
DOCKERFILE_HASH="$(sha256sum "$DOCKERFILE" | cut -d' ' -f1)"

image_is_current() {
    [ "$REBUILD" = 0 ] || return 1
    docker image inspect "$1" >/dev/null 2>&1 || return 1
    built_from="$(docker image inspect -f '{{index .Config.Labels "linc.debbuild.source"}}' "$1")"
    [ "$built_from" = "$DOCKERFILE_HASH" ]
}

for release in "${RELEASES[@]}"; do
    image="linc-deb-$release"
    cache="$ROOT/target/debbuild/$release"
    mkdir -p "$cache/cargo" "$cache/target" "$cache/home"

    if image_is_current "$image"; then
        echo "==> $release: using the loaded $image (--rebuild to rebuild it)"
    else
        echo "==> $release: building $image"
        docker build -q -t "$image" \
            --label "linc.debbuild.source=$DOCKERFILE_HASH" \
            --build-arg "RELEASE=$release" \
            -f "$DOCKERFILE" "$ROOT/packaging/debbuild" >/dev/null
    fi

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
        # dist/<release>/ is this script's own output: drop the previous debs so a
        # stale version cannot be read as the one just built.
        rm -f "$ROOT/dist/$release"/linc_*.deb
        # dpkg-buildpackage writes beside the checkout, not into dist/.
        mv "$PARENT"/linc*.deb "$ROOT/dist/$release/"/ 2>/dev/null || true
        rm -f "$PARENT"/linc_*.changes "$PARENT"/linc_*.buildinfo
    else
        echo "==> $release: staging and packaging"
        out="dist/$release"
        rm -f "$ROOT/$out"/linc_*.deb
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
