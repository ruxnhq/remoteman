#!/bin/sh
# The Linux viewer decoding with VA-API on an AMD (or Intel) GPU and showing the picture, with
# nothing real involved. The host and its moving content run in one throwaway headless Mutter
# as in e2e-viewer.sh (on whatever GPU that Mutter picks); the viewer's own Mutter is made to
# render on the other GPU (copies of udev's records tagging it `mutter-device-preferred-primary`,
# bound in a bwrap), so GTK draws there too and the decoder's surfaces stay on that GPU.
#   scripts/e2e-viewer-vaapi.sh [hevc|av1]   (default hevc)
#   RM_NODE=/dev/dri/renderD128   the GPU (default: the first that isn't NVIDIA's)
# Passes if the decoder is VA-API's, frames come, pictures come out and nearly all are shown,
# and none had to be copied through the CPU. The 200th picture is saved as shot.png.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7783}"
export RM_CODEC="${1:-hevc}"
node="${RM_NODE:-}"
if [ -z "$node" ]; then
  for n in /dev/dri/renderD*; do
    [ "$(cat /sys/class/drm/${n##*/}/device/vendor)" = 0x10de ] || { node=$n; break; }
  done
fi
[ -n "$node" ] || { echo "SKIP: no GPU here but NVIDIA's"; exit 0; }
name=${node##*/}
echo "viewer's GPU: $node ($(cat /sys/class/drm/$name/device/vendor)), codec $RM_CODEC"
mkdir -p "$RM_LOGS/udev"
binds=""
for dev in /sys/class/drm/$name /sys/class/drm/$name/device/drm/card*; do
  id="c$(cat $dev/dev)"
  { cat /run/udev/data/$id 2>/dev/null; printf 'G:mutter-device-preferred-primary\nQ:mutter-device-preferred-primary\n'; } >"$RM_LOGS/udev/$id"
  binds="$binds --bind $RM_LOGS/udev/$id /run/udev/data/$id"
done
# The viewer's side: its own Mutter on that GPU, and the viewer in it.
dbus-run-session -- bwrap --bind / / --dev-bind /dev /dev --proc /proc $binds sh -c '
  bin="${RM_BIN:-$RM_ROOT/target/release}"
  mutter --headless --wayland --no-x11 --wayland-display rm-view-va --virtual-monitor 1920x1080@60 >"$RM_LOGS/mutter-view.log" 2>&1 &
  vpid=$!
  sleep 5
  env -u DISPLAY WAYLAND_DISPLAY=rm-view-va GDK_BACKEND=wayland REMOTEMAN_HWDEC=vaapi "$bin/remoteman" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/viewer-state" --size 1920x1080 --scale 1 --refresh 60 --decoders $RM_CODEC --quit-after 22 --dump "$RM_LOGS/shot.png" >"$RM_LOGS/viewer.out" 2>"$RM_LOGS/viewer.err"
  kill $vpid; wait 2>/dev/null
' >/dev/null 2>&1 &
side=$!
# The host's side, as in e2e-viewer.sh.
dbus-run-session -- sh -c '
  bin="${RM_BIN:-$RM_ROOT/target/release}"
  mutter --headless --wayland --no-x11 --wayland-display rm-host-va >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  ( sleep 5; exec env -u DISPLAY WAYLAND_DISPLAY=rm-host-va GDK_BACKEND=wayland python3 "$RM_ROOT/scripts/animate.py" >"$RM_LOGS/content.log" 2>&1 ) &
  cpid=$!
  sleep 32
  kill $cpid $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' >/dev/null 2>&1
wait $side
grep -E "selected primary|selected as primary" "$RM_LOGS/mutter-view.log" | sed 's/.*: //'
grep -E "ten seconds|copying|GTK won|can.t draw" "$RM_LOGS/viewer.err" | cut -c1-330
cat "$RM_LOGS/viewer.out"
fail=0
grep -q "$node selected primary" "$RM_LOGS/mutter-view.log" || { echo "the viewer's Mutter did not render on $node"; fail=1; }
line=$(grep "in ten seconds" "$RM_LOGS/viewer.err" | tail -1)
echo "$line" | grep -q "(vaapi) .* via no copy" || { echo "the decoder was not VA-API's, or its pictures were copied"; fail=1; }
grep -qE "copying them instead|GTK won" "$RM_LOGS/viewer.err" && { echo "pictures were copied through the CPU"; fail=1; }
came=$(echo "$line" | grep -o "[0-9]* pictures came out" | grep -o "^[0-9]*")
shown=$(echo "$line" | grep -o "[0-9]* were shown" | grep -o "^[0-9]*")
[ "${came:-0}" -gt 400 ] || { echo "too few pictures in the last ten seconds: ${came:-none}"; fail=1; }
[ "${shown:-0}" -ge $(( ${came:-1} * 9 / 10 )) ] || { echo "only ${shown:-none} of ${came:-none} pictures shown"; fail=1; }
[ -s "$RM_LOGS/shot.png" ] || { echo "no picture saved"; fail=1; }
[ $fail = 0 ] && { echo "PASS (logs in $RM_LOGS)"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; tail -5 "$RM_LOGS/viewer.err"; exit 1
