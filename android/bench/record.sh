#!/bin/sh
# Record the streams the Android benchmark plays (android/bench/recordings, not in git): what a
# client is handed, frame by frame (RMREC1), plus the bare stream for FFmpeg's reference.
#   android/bench/record.sh HOST:PORT
# The host should be a throwaway one with moving content that pairs with anyone, for example
# main's scripts/kde-nested.sh running an agent with --pair and scripts/animate.py. Each
# recording is its own connection (a recording has one keyframe, at its start).
set -e
cd "$(dirname "$0")/../.."
host="${1:?usage: record.sh HOST:PORT}"
out=android/bench/recordings
state=$(mktemp -d)
mkdir -p "$out"
cargo build --release -q -p remoteman-testclient
tc=target/release/remoteman-testclient
# name, decoders, size, placement, seconds
while read -r name dec size place secs; do
    echo "== $name"
    $tc "$host" --pair --state "$state" --decoders "$dec" --size "$size" --scale 1 --refresh 60 \
        --placement "$place" --seconds "$secs" --record "$out/$name.rmrec" --out "$out/$name.bare" 2>&1 | grep -E "stream:|frames [0-9]"
done <<LIST
av1-1080p60 av1 1920x1080 mirror 4
hevc-1080p60 hevc 1920x1080 mirror 4
av1-2160p60 av1 3840x2160 only 3
hevc-2160p60 hevc 3840x2160 only 3
LIST
rm -rf "$state"
ls -la "$out"
