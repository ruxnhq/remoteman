#!/bin/sh
# The Linux viewer's camera, end to end, with no real camera involved. A made-up camera is
# needed for the viewer to read, so the test makes one the way the product does: a first
# throwaway host is sent a test pattern as a device's camera (by the test client) and shows it as
# a PipeWire camera. The viewer, in a headless Mutter of its own, connects to a second throwaway
# host with its camera on, reading that node and no other. A reader then takes a picture from the
# camera that appears on the second host, which must be the test pattern. The nodes exist on
# this user's PipeWire for the seconds the test runs; a real webcam is never opened.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT_A="${RM_PORT_A:-7792}" RM_PORT_B="${RM_PORT_B:-7793}"
enc=libx264; ffmpeg -hide_banner -encoders 2>/dev/null | grep -q libx264 || enc=libopenh264
ffmpeg -v error -y -f lavfi -i "testsrc2=size=1280x720:rate=30" -t 30 -c:v $enc -bf 0 -g 60 -pix_fmt yuv420p -bsf:v h264_mp4toannexb -f h264 "$RM_LOGS/clip.h264" || { echo "SKIP: no ffmpeg with a software H.264 encoder"; exit 0; }
printf 'wait 2\ncamerafile %s\nwait 1\n' "$RM_LOGS/clip.h264" > "$RM_LOGS/steps.txt"
rm -f "$RM_LOGS/camera.png"
dbus-run-session -- mutter --headless --wayland --no-x11 --wayland-display rm-view --virtual-monitor 1920x1080@60 >"$RM_LOGS/mutter-view.log" 2>&1 &
vpid=$!
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"
  node() { sed "s/\x1b\[[0-9;]*m//g" "$1" | grep -o "node=[^ ]*" | tail -1 | cut -d= -f2 | tr -d \"; }
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT_A --pair --state "$RM_LOGS/state-a" --keep-output >"$RM_LOGS/agent-a.log" 2>&1 &
  apid=$!
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT_B --pair --state "$RM_LOGS/state-b" --keep-output >"$RM_LOGS/agent-b.log" 2>&1 &
  bpid=$!
  sleep 1
  # The made-up camera: there from about 3 s after this, for 30 s.
  "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT_A --pair --state "$RM_LOGS/client-a" --seconds 34 --size 1280x720 --scale 1 --script "$RM_LOGS/steps.txt" >"$RM_LOGS/client-a.log" 2>&1 &
  cpid=$!
  sleep 5
  from=$(node "$RM_LOGS/agent-a.log")
  echo "the made-up camera: $from" >"$RM_LOGS/read.log"
  # The viewer is in the second host about 9 s after it starts, with its camera on; a picture
  # is taken from the camera there 6 s later.
  ( sleep 15
    to=$(node "$RM_LOGS/agent-b.log")
    echo "the camera on the second host: $to" >>"$RM_LOGS/read.log"
    [ -n "$to" ] && [ "$to" != "$from" ] && exec timeout 5 gst-launch-1.0 -q pipewiresrc target-object="$to" stream-properties="props,node.dont-fallback=true,node.dont-reconnect=true,node.dont-move=true" num-buffers=10 ! videoconvert ! pngenc snapshot=false ! multifilesink location="$RM_LOGS/camera.png" >>"$RM_LOGS/read.log" 2>&1 ) &
  [ -n "$from" ] && env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland timeout 40 "$bin/remoteman" --state "$RM_LOGS/vstate" --size 1920x1080 --scale 1 --refresh 60 --camera --camera-from "$from" --ui "wait=1,pair-go=127.0.0.1:$RM_PORT_B,wait=3,click=Connect,wait=14,shot=$RM_LOGS/viewer.png" >"$RM_LOGS/viewer.log" 2>"$RM_LOGS/viewer.err"
  sleep 0.5
  kill $cpid $apid $bpid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
pkill -P $vpid 2>/dev/null; kill $vpid 2>/dev/null; wait $vpid 2>/dev/null
cat "$RM_LOGS/read.log"
grep -E '^camera:' "$RM_LOGS/viewer.log"
sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent-b.log" | grep -E "camera" | cut -c29-220
fail=0
sent=$(grep -oE '^camera: [0-9]+ frames sent' "$RM_LOGS/viewer.log" | grep -oE '[0-9]+' | head -1)
[ "${sent:-0}" -ge 150 ] || { echo "the viewer sent ${sent:-no} frames of its camera"; fail=1; }
if [ -s "$RM_LOGS/camera.png" ]; then
    look=$(magick "$RM_LOGS/camera.png" -format '%wx%h spread %[fx:standard_deviation]' info: 2>/dev/null)
    echo "picture read from the camera on the second host: $look"
    case "$look" in 1280x720*) ;; *) echo "the picture is not 1280x720"; fail=1 ;; esac
    spread=$(echo "$look" | awk '{print $3}')
    awk "BEGIN { exit !($spread > 0.15) }" || { echo "the picture is flat: not the test pattern"; fail=1; }
else
    echo "no picture could be read from the camera on the second host"; fail=1
fi
left=$(pw-cli ls Node 2>/dev/null | grep -c 'node.name = ".*camera.*remoteman')
[ "$left" = 0 ] || { echo "$left remoteman camera nodes left behind"; fail=1; }
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent-b.log" | grep -E 'WARN|ERROR' | cut -c29-220 | head -6; grep -v 'Gtk-\|^$' "$RM_LOGS/viewer.log" "$RM_LOGS/viewer.err" | tail -6; exit 1
