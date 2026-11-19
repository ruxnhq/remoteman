#!/bin/sh
# Mirroring, with nothing real involved: a throwaway headless Mutter that has a monitor of its
# own (1920x1080) with moving content on it, the host in it, and the viewer in a second Mutter,
# asking to mirror. The viewer must be shown that monitor at its size, the desktop must get no
# extra monitor for it, and the picture in the viewer's window must be the content.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7796}"
dbus-run-session -- mutter --headless --wayland --no-x11 --wayland-display rm-view --virtual-monitor 1920x1080@60 >"$RM_LOGS/mutter-view.log" 2>&1 &
vpid=$!
dbus-run-session -- sh -c '
  bin="${RM_BIN:-$RM_ROOT/target/release}"
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e --virtual-monitor 1920x1080@60 >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  env -u DISPLAY WAYLAND_DISPLAY=rm-e2e GDK_BACKEND=wayland python3 "$RM_ROOT/scripts/animate.py" >"$RM_LOGS/content.log" 2>&1 &
  cpid=$!
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  # The viewer is in from about 9 s: the desktop is asked what monitors it has at 11 s.
  ( sleep 11; gdbus call --session --dest org.gnome.Mutter.DisplayConfig --object-path /org/gnome/Mutter/DisplayConfig --method org.gnome.Mutter.DisplayConfig.GetCurrentState 2>&1 | grep -o "Meta-[0-9]*" | sort -u | tr "\n" " " >"$RM_LOGS/monitors.txt" ) &
  env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland timeout 40 "$bin/remoteman" --state "$RM_LOGS/vstate" --size 1280x720 --scale 1 --refresh 60 --placement mirror --ui "wait=1,pair-go=127.0.0.1:$RM_PORT,wait=3,click=Connect,wait=6,shot=$RM_LOGS/mirror.png" >"$RM_LOGS/viewer.log" 2>"$RM_LOGS/viewer.err"
  sleep 0.5
  kill $cpid $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
pkill -P $vpid 2>/dev/null; kill $vpid 2>/dev/null; wait $vpid 2>/dev/null
sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep -E "mirroring" | cut -c29-200
echo "monitors on the host's desktop while mirroring: $(cat "$RM_LOGS/monitors.txt" 2>/dev/null)"
fail=0
sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep -q "mirroring this computer's main monitor width=1920 height=1080" || { echo "the host did not mirror its monitor"; fail=1; }
[ "$(cat "$RM_LOGS/monitors.txt" 2>/dev/null)" = "Meta-0 " ] || { echo "the desktop has other monitors than its own"; fail=1; }
if [ -s "$RM_LOGS/mirror.png" ]; then
    spread=$(magick "$RM_LOGS/mirror.png" -format '%[fx:standard_deviation]' info: 2>/dev/null)
    echo "the viewer's window: spread $spread"
    awk "BEGIN { exit !($spread > 0.05) }" || { echo "the viewer's picture is flat"; fail=1; }
else
    echo "no picture of the viewer's window"; fail=1
fi
[ $fail = 0 ] && { echo "PASS ($RM_LOGS/mirror.png)"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; grep -v 'Gtk-\|^$' "$RM_LOGS/viewer.err" | tail -4; exit 1
