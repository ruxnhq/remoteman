#!/bin/sh
# A computer that is still logging its user in when a device comes (as the Mac host does when
# it unlocks or logs in for a paired device), with nothing real involved: the host is told to
# turn its first two clients away with "logging in" (REMOTEMAN_TEST_LOGGING_IN=2; the device
# that pairs is the first). The viewer must say so, try again by itself, and be in on the
# third try without anyone pressing anything.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7796}"
dbus-run-session -- mutter --headless --wayland --no-x11 --wayland-display rm-view --virtual-monitor 1920x1080@60 >"$RM_LOGS/mutter-view.log" 2>&1 &
vpid=$!
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  REMOTEMAN_TEST_LOGGING_IN=3 "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  # Pairing is the first client (turned away, but paired). Then the viewer connects: turned
  # away twice more, 3 s apart, and in on the third try.
  env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland timeout 40 "$bin/remoteman" --state "$RM_LOGS/vstate" --size 1920x1080 --scale 1 --refresh 60 --ui "wait=1,pair-go=127.0.0.1:$RM_PORT,wait=3" >"$RM_LOGS/viewer-pair.log" 2>&1
  env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland timeout 40 "$bin/remoteman" --state "$RM_LOGS/vstate" --size 1920x1080 --scale 1 --refresh 60 --ui "wait=1,connect=127.0.0.1:$RM_PORT,wait=2,shot=$RM_LOGS/waiting.png,wait=9,shot=$RM_LOGS/in.png,size?" >"$RM_LOGS/viewer.log" 2>"$RM_LOGS/viewer.err"
  sleep 0.5
  kill $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
pkill -P $vpid 2>/dev/null; kill $vpid 2>/dev/null; wait $vpid 2>/dev/null
grep '^~ logging in' "$RM_LOGS/viewer.log" | tr '\n' ' '; echo
fail=0
[ "$(grep '^~ logging in' "$RM_LOGS/viewer.log" | tr '\n' ' ')" = "~ logging in: try 1 ~ logging in: try 2 " ] || { echo "the viewer was not told twice that the computer is logging in"; fail=1; }
sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep -q "session: client name" || { echo "the viewer never got in"; fail=1; }
grep -q '^~ window 1280' "$RM_LOGS/viewer.log" || { echo "the viewer was not in a session at the end"; fail=1; }
[ $fail = 0 ] && { echo "PASS ($RM_LOGS/waiting.png)"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; grep -v 'Gtk-\|^$\|nearby' "$RM_LOGS/viewer.log" "$RM_LOGS/viewer.err" | tail -6; exit 1
