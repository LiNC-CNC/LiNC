# Build image for LiNC's Debian packages.
#
#   docker build --build-arg RELEASE=bookworm -t linc-deb-bookworm packaging/debbuild
#
# Driven by scripts/build-deb-docker.sh, which also runs the build. The package
# list is the same on bookworm and trixie: both ship linuxcnc-uspace 2.9 with the
# static liblinuxcnc and NML headers, and the LinuxCNC headers need boost.
#
# Rust comes from rustup rather than the distribution: bookworm ships rustc 1.63
# and trixie 1.85, while egui 0.37 declares rust-version 1.95.
ARG RELEASE=bookworm
FROM debian:${RELEASE}

# `curl | sh` below must not hide a failed download (hadolint DL4006).
SHELL ["/bin/bash", "-o", "pipefail", "-c"]

ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential ca-certificates curl debhelper dpkg-dev file git pkg-config \
        libboost-dev \
        libegl1-mesa-dev libgl1-mesa-dev \
        libwayland-dev libx11-dev libxcursor-dev libxi-dev libxkbcommon-dev \
        libxrandr-dev \
        linuxcnc-uspace linuxcnc-uspace-dev \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
      | sh -s -- -y --no-modify-path --profile minimal --default-toolchain stable \
    && chmod -R a+rX /usr/local/rustup /usr/local/cargo

# The container runs as the caller's uid; CARGO_HOME and the target directory are
# bind-mounted to writable cache paths by the driver.
WORKDIR /work
