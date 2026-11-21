#!/bin/sh
# Build remoteman's two AppImages: the viewer, and the host (Remoteman Host with the agent and
# the daemon inside it). The programs are built in a container of Ubuntu 25.10
# (packaging/appimage/Dockerfile), so they run there and on anything newer; this computer's
# own build in target/release is not touched. Needs docker, and the Rust toolchain from rustup
# (it is lent to the container, with the crates already downloaded).
#
#   packaging/appimage/build.sh        both, into target/appimage/: remoteman-viewer-VERSION-gnome-
#                                      x86_64.AppImage and remoteman-host-VERSION-gnome-x86_64.AppImage,
#                                      each with FILE.sha256 beside it
#
# What goes inside: the programs, FFmpeg (built in the container with only what remoteman
# uses, LGPL), dav1d, OpenH264 and Opus, and their licences (usr/share/licenses/remoteman,
# with FFmpeg-SOURCE.txt saying which FFmpeg it is and where its source is). What stays the
# computer's own: GTK and libadwaita
# (4.20 and 1.6 at least), PipeWire, the graphics drivers and everything they load.
set -eu
root=$(dirname "$(dirname "$(dirname "$(readlink -f "$0")")")")
out="$root/target/appimage"
toolchain=$(dirname "$(dirname "$(rustup which cargo)")")
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
mkdir -p "$out/cargo-home/registry" "$out/cargo-home/git"
docker build -q -t remoteman-appimage-build -f "$root/packaging/appimage/Dockerfile" "$root" >/dev/null
run() {
    docker run --rm -u "$(id -u):$(id -g)" -v "$root:/src" -v "$toolchain:/rust:ro" \
        -v "$cargo_home/registry:/src/target/appimage/cargo-home/registry" -v "$cargo_home/git:/src/target/appimage/cargo-home/git" \
        -e CARGO_HOME=/src/target/appimage/cargo-home -e CARGO_TARGET_DIR=/src/target/appimage/build -e HOME=/tmp \
        -e PATH=/rust/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin -e RM_VERSION="$version" \
        -w /src remoteman-appimage-build "$@"
}
run cargo build --release --offline -p remoteman-viewer -p remoteman-tray -p remoteman-agent -p remoteman-daemon
run sh packaging/appimage/assemble.sh
ls -l "$out"/*.AppImage
