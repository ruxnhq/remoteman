#!/bin/sh
# The Linux viewer against the Linux host, with nothing real involved: the host and moving
# content in one throwaway headless Mutter, the viewer's window in a second one (each on its own
# D-Bus session). Leaves the live desktop alone.
#   scripts/e2e-viewer.sh [viewer options, e.g. --decoders hevc444 --quit-after 10 --dump out.png]
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7798}"
export RM_VIEWER_ARGS="${*:---quit-after 10}"
# The viewer's own screen.
dbus-run-session -- mutter --headless --wayland --no-x11 --wayland-display rm-view --virtual-monitor 2752x2064@120 >"$RM_LOGS/mutter-view.log" 2>&1 &
vpid=$!
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  ( sleep 3; exec env -u DISPLAY WAYLAND_DISPLAY=rm-e2e GDK_BACKEND=wayland python3 "$RM_ROOT/scripts/animate.py" >"$RM_LOGS/content.log" 2>&1 ) &
  cpid=$!
  env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland "$bin/remoteman" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --size 2752x2064 --scale 2 --refresh 120 $RM_VIEWER_ARGS 2>"$RM_LOGS/viewer.err"
  rc=$?
  sleep 0.5
  kill $cpid $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
  exit $rc
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
# End the viewer's compositor (a child of that dbus-run-session), then the session itself.
pkill -P $vpid 2>/dev/null; kill $vpid 2>/dev/null; wait $vpid 2>/dev/null
echo "logs: $RM_LOGS"
