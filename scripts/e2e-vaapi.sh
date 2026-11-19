#!/bin/sh
# The host on an AMD (or Intel) GPU, with nothing real involved: a throwaway headless Mutter
# made to render on that GPU, the host in it encoding through VA-API, and the test client. On a
# computer with an NVIDIA card as well (which Mutter takes by itself, being the one the firmware
# booted with), Mutter is shown copies of udev's records that tag the other card
# `mutter-device-preferred-primary`, inside a bwrap that changes nothing outside it; the host
# then finds that card by the same rule. Checks: the encoder is VA-API; frames come; a keyframe
# asked for comes; a new bitrate takes effect; a new size comes as a new picture; the saved
# stream decodes in FFmpeg with no errors and every frame accounted for.
#   RM_NODE=/dev/dri/renderD128   the GPU (default: the first that isn't NVIDIA's)
#   RM_SIZE=1920x1080 RM_REFRESH=60 RM_SECONDS=16   the client's display and how long
#   RM_STEPS=FILE   the client's script instead of the one below (then only frames are checked)
#   RM_CLIENT_ARGS="--drop-one-in 500"   more options for the test client
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7781}"
export RM_SIZE="${RM_SIZE:-1920x1080}" RM_REFRESH="${RM_REFRESH:-60}" RM_SECONDS="${RM_SECONDS:-16}"
node="${RM_NODE:-}"
if [ -z "$node" ]; then
  for n in /dev/dri/renderD*; do
    [ "$(cat /sys/class/drm/${n##*/}/device/vendor)" = 0x10de ] || { node=$n; break; }
  done
fi
[ -n "$node" ] || { echo "SKIP: no GPU here but NVIDIA's"; exit 0; }
name=${node##*/}
echo "GPU: $node ($(cat /sys/class/drm/$name/device/vendor))"
# udev's records of the node and its card, tagged for Mutter.
mkdir -p "$RM_LOGS/udev"
binds=""
for dev in /sys/class/drm/$name /sys/class/drm/$name/device/drm/card*; do
  id="c$(cat $dev/dev)"
  { cat /run/udev/data/$id 2>/dev/null; printf 'G:mutter-device-preferred-primary\nQ:mutter-device-preferred-primary\n'; } >"$RM_LOGS/udev/$id"
  binds="$binds --bind $RM_LOGS/udev/$id /run/udev/data/$id"
done
if [ -n "$RM_STEPS" ]; then
  cp "$RM_STEPS" "$RM_LOGS/steps.txt"
else
  # A keyframe asked for 4 s in, a lower bitrate at 7 s, another size at 10 s.
  printf 'wait 4\nkeyframe\nwait 3\nbitrate 8000000\nwait 3\nresize 1280x720\nwait 3\n' >"$RM_LOGS/steps.txt"
fi
dbus-run-session -- bwrap --bind / / --dev-bind /dev /dev --proc /proc $binds sh -c '
  bin="${RM_BIN:-$RM_ROOT/target/release}"
  mutter --headless --wayland --no-x11 --wayland-display rm-vaapi >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  env $RM_AGENT_ENV "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  ( sleep 2.5; exec env -u DISPLAY WAYLAND_DISPLAY=rm-vaapi GDK_BACKEND=wayland python3 "$RM_ROOT/scripts/animate.py" >"$RM_LOGS/content.log" 2>&1 ) &
  cpid=$!
  "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/client" --seconds $RM_SECONDS --size $RM_SIZE --scale 1 --refresh $RM_REFRESH --decoders hevc,av1 --script "$RM_LOGS/steps.txt" --out "$RM_LOGS/stream.hevc" $RM_CLIENT_ARGS >"$RM_LOGS/client.log" 2>&1
  kill $cpid $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' >/dev/null 2>&1
log() { sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log"; }
grep -E "selected primary|selected as primary" "$RM_LOGS/mutter.log" | sed 's/.*: //'
log | grep -o "encoder ready.*" | head -2
log | grep -o "stream format.*" | head -2
grep -E "^stream|^frames|^rate|^interval|^capture|^keyframe" "$RM_LOGS/client.log"
fail=0
grep -q "$node selected primary" "$RM_LOGS/mutter.log" || { echo "Mutter did not render on $node"; fail=1; }
log | grep -q "encoder ready (VA-API" || { echo "the host did not encode through VA-API"; fail=1; }
frames=$(grep -o "^frames [0-9]*" "$RM_LOGS/client.log" | grep -o "[0-9]*")
[ "${frames:-0}" -gt $((RM_REFRESH * 4)) ] || { echo "too few frames: ${frames:-none}"; fail=1; }
# The decoder must take every frame saved, with no error.
saved=$(grep -o "^handed over [0-9]*" "$RM_LOGS/client.log" | grep -o "[0-9]*")
ffmpeg -v error -i "$RM_LOGS/stream.hevc" -f null - 2>"$RM_LOGS/decode.txt"
[ -s "$RM_LOGS/decode.txt" ] && { echo "FFmpeg's decoder complained:"; head -5 "$RM_LOGS/decode.txt"; fail=1; }
ffprobe -v error -select_streams v -show_entries frame=key_frame,width,height -of csv=p=0 "$RM_LOGS/stream.hevc" >"$RM_LOGS/frames.csv"
decoded=$(wc -l <"$RM_LOGS/frames.csv")
echo "decoded $decoded of $saved frames saved; keyframes at: $(grep -n '^1,' "$RM_LOGS/frames.csv" | sed 's/:1,/:/' | tr '\n' ' ')"
[ "$decoded" = "$saved" ] || { echo "not every frame decoded"; fail=1; }
if [ -z "$RM_STEPS" ]; then
  keys=$(grep -c '^1,' "$RM_LOGS/frames.csv")
  # The first, the one asked for, the new bitrate's (VA-API opens its encoder afresh), the new size's.
  [ "$keys" = 4 ] || { echo "wanted 4 keyframes (start, asked, bitrate, size), got $keys"; fail=1; }
  asked=$(grep -o "keyframe asked for at [0-9]*" "$RM_LOGS/client.log" | grep -o "[0-9]*$")
  # The one asked for is among the next few frames.
  sed -n "$((asked + 1)),$((asked + 6))p" "$RM_LOGS/frames.csv" | grep -q '^1,' || { echo "no keyframe soon after the one asked for at frame $asked"; fail=1; }
  log | grep -q "bitrate from=.* to=8000000" || { echo "the bitrate did not change"; fail=1; }
  grep -q "^stream: .*1280x720" "$RM_LOGS/client.log" && tail -1 "$RM_LOGS/frames.csv" | grep -q ",1280,720" || { echo "no picture of the new size"; fail=1; }
fi
[ $fail = 0 ] && { echo "PASS (logs in $RM_LOGS)"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; log | grep -E "WARN|ERROR" | cut -c1-200 | head -8; exit 1
