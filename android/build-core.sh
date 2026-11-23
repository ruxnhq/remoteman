#!/bin/sh
# Build the Rust side of the Android app (crates/remoteman-android: the shared core and the
# decoder) into android/app/src/main/jniLibs, for phones and TVs (arm64, and armeabi-v7a for the
# many TV boxes and older phones with a 32-bit system) and the emulator (x86_64).
#   android/build-core.sh            release
#   android/build-core.sh debug
# Needs the NDK (Android Studio: SDK Manager, SDK Tools, NDK), cargo-ndk, and the Rust targets
# aarch64-linux-android, armv7-linux-androideabi and x86_64-linux-android.
set -e
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
sdk="${ANDROID_HOME:-$HOME/Android/Sdk}"
if [ -z "$ANDROID_NDK_HOME" ]; then
    ANDROID_NDK_HOME=$(ls -d "$sdk"/ndk/* 2>/dev/null | sort -V | tail -1)
    export ANDROID_NDK_HOME
fi
[ -d "$ANDROID_NDK_HOME" ] || { echo "no NDK under $sdk/ndk" >&2; exit 1; }
profile=--release
[ "$1" = debug ] && profile=
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 -P 29 -o android/app/src/main/jniLibs build $profile -p remoteman-android
