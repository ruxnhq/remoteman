#!/bin/sh
# The reference results for android/bench/recordings, made with FFmpeg on this computer:
#   NAME.md5   one line per frame: the MD5 of the decoded frame as 10-bit 4:2:0 planes,
#              little-endian (yuv420p10le), what any correct decoder gives bit for bit
#   NAME.png   the last frame, in BT.709 limited range to RGB, scaled to 480x270, for
#              comparing with what the device actually shows on its screen
#   android/bench/reference.sh
set -e
cd "$(dirname "$0")/recordings"
for bare in *.bare; do
    name=${bare%.bare}
    ffmpeg -v error -y -i "$bare" -pix_fmt yuv420p10le -f framemd5 - | grep -v '^#' | awk -F', *' '{print $6}' >"$name.md5"
    ffmpeg -v error -y -i "$bare" -vf "scale=480:270:flags=area:in_color_matrix=bt709:in_range=tv:out_range=pc,format=rgb24" -update 1 "$name.png"
    echo "$name: $(wc -l <"$name.md5") frames"
done
# What the bench build of the app carries (app/build.gradle.kts, buildTypes "bench").
mkdir -p ../assets/recordings
cp *.rmrec *.md5 *.png ../assets/recordings/
