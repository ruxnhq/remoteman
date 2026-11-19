#!/bin/sh
# The device's camera, end to end, with nothing real involved: a throwaway headless Mutter, the
# agent in it, and the test client as the device, sending a made-up H.264 clip (a test pattern,
# 1280x720 at 30 a second) as its camera. A reader takes a picture from the camera that appears
# on the desktop, the way a camera program would, and the picture must be the test pattern and
# not black. The camera appears on this user's PipeWire for the seconds the test runs; the
# reader names it as its target and takes no other, so a real webcam is never opened.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7798}"
enc=libx264; ffmpeg -hide_banner -encoders 2>/dev/null | grep -q libx264 || enc=libopenh264
ffmpeg -v error -y -f lavfi -i "testsrc2=size=1280x720:rate=30" -t 5 -c:v $enc -bf 0 -g 60 -pix_fmt yuv420p -bsf:v h264_mp4toannexb -f h264 "$RM_LOGS/clip.h264" || { echo "SKIP: no ffmpeg with a software H.264 encoder"; exit 0; }
printf 'wait 2\ncamerafile %s\nwait 1\n' "$RM_LOGS/clip.h264" > "$RM_LOGS/steps.txt"
rm -f "$RM_LOGS/camera.png"
dbus-run-session -- sh -c '
  bin="${RM_BIN:-$RM_ROOT/target/release}"
  mutter --headless --wayland --no-x11 --wayland-display rm-camera-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  # The camera exists from 2 s in and shows the clip for 5 s: take a picture from it at 5 s.
  ( sleep 5
    node=$(sed "s/\x1b\[[0-9;]*m//g" "$RM_LOGS/agent.log" | grep -o "node=[^ ]*" | tail -1 | cut -d= -f2 | tr -d \")
    echo "camera node: $node" >"$RM_LOGS/read.log"
    [ -n "$node" ] && exec timeout 5 gst-launch-1.0 -q pipewiresrc target-object="$node" stream-properties="props,node.dont-fallback=true,node.dont-reconnect=true,node.dont-move=true" num-buffers=10 ! videoconvert ! pngenc snapshot=false ! multifilesink location="$RM_LOGS/camera.png" >>"$RM_LOGS/read.log" 2>&1 ) &
  "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --seconds 10 --size 1920x1080 --scale 1 --script "$RM_LOGS/steps.txt" >"$RM_LOGS/client.log" 2>&1
  sleep 0.5
  kill $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
grep -E '^camera:' "$RM_LOGS/client.log"
sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep -E "camera" | cut -c29-220
fail=0
grep -qE '^camera: sent 150 frames' "$RM_LOGS/client.log" || { echo "the device did not send the clip"; fail=1; }
if [ -s "$RM_LOGS/camera.png" ]; then
    # The test pattern is colourful: a black or flat picture has almost no spread.
    look=$(magick "$RM_LOGS/camera.png" -format '%wx%h spread %[fx:standard_deviation]' info: 2>/dev/null)
    echo "picture read from the camera: $look"
    case "$look" in 1280x720*) ;; *) echo "the picture is not 1280x720"; fail=1 ;; esac
    spread=$(echo "$look" | awk '{print $3}')
    awk "BEGIN { exit !($spread > 0.15) }" || { echo "the picture is flat: not the test pattern"; fail=1; }
else
    echo "no picture could be read from the camera"; cat "$RM_LOGS/read.log" 2>/dev/null | head -5; fail=1
fi
left=$(pw-cli ls Node 2>/dev/null | grep -c 'node.name = ".*camera.*remoteman')
[ "$left" = 0 ] || { echo "$left remoteman camera nodes left behind"; fail=1; }
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep -E 'WARN|ERROR' | cut -c29-220 | head -8; exit 1
