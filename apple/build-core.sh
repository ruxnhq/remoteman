#!/bin/sh
# Build the Rust client core for the Apple targets and wrap it as an XCFramework that Swift
# imports as `RemotemanCore`. Run from anywhere; output is apple/build/RemotemanCore.xcframework.
#   apple/build-core.sh            macOS, iOS and the iOS simulator
#   apple/build-core.sh mac        macOS only (quicker while working on the Mac viewer)
set -e
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
export MACOSX_DEPLOYMENT_TARGET=14.0
export IPHONEOS_DEPLOYMENT_TARGET=17.0

case "${1:-all}" in
    mac) targets="aarch64-apple-darwin" ;;
    *) targets="aarch64-apple-darwin aarch64-apple-ios aarch64-apple-ios-sim" ;;
esac

out=apple/build/RemotemanCore.xcframework
headers=apple/build/core-headers
rm -rf "$out" "$headers"
mkdir -p "$headers"
cp crates/remoteman-core/include/remoteman_core.h "$headers/"
cat > "$headers/module.modulemap" <<EOF
module RemotemanCore {
    header "remoteman_core.h"
    export *
}
EOF

args=""
for t in $targets; do
    cargo build --release -p remoteman-core --target "$t"
    args="$args -library target/$t/release/libremoteman_core.a -headers $headers"
done
# shellcheck disable=SC2086
xcodebuild -create-xcframework $args -output "$out"
