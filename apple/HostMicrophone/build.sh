#!/bin/sh
# Builds remoteman's microphone driver (RemotemanMicrophone.driver) into ../build/microphone and
# runs its self-test, which loads the driver in an ordinary process: nothing is installed.
# The host app's Settings installs it (into /Library/Audio/Plug-Ins/HAL, with the owner's password).
set -e
cd "$(dirname "$0")"
OUT=../build/microphone
D="$OUT/RemotemanMicrophone.driver"
rm -rf "$D"
mkdir -p "$D/Contents/MacOS"
cp Info.plist "$D/Contents/Info.plist"
clang -O2 -Wall -Wextra -arch arm64 -arch x86_64 -mmacosx-version-min=14.0 -bundle \
    -framework CoreAudio -framework CoreFoundation \
    -o "$D/Contents/MacOS/RemotemanMicrophone" RemotemanMicrophone.c
# Signed with the team's identity when there is one, else ad hoc (the audio server takes either).
ID=$(security find-identity -v -p codesigning 2>/dev/null | awk -F'"' '/Apple Development|Developer ID Application/ {print $2; exit}')
codesign --force --sign "${ID:--}" "$D" 2>&1 | sed 's/^/codesign: /'
clang -O1 -Wall -framework CoreAudio -framework CoreFoundation -o "$OUT/selftest" selftest.c
"$OUT/selftest" "$D/Contents/MacOS/RemotemanMicrophone"
echo "built $D"
