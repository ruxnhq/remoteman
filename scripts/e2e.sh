#!/bin/sh
# End-to-end test with nothing real involved: a throwaway headless Mutter on a private D-Bus
# session, the agent inside it on the loopback, moving content, and the screenless test client.
# Leaves the live desktop alone.
#   scripts/e2e.sh [test client options, e.g. --decoders hevc444 --seconds 10 --input-demo]
# Logs land in $RM_LOGS (default: a fresh temporary folder, printed at the end).
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7799}"
export RM_CLIENT_ARGS="$*"
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  # The content needs the virtual monitor, which exists once the client is in.
  ( sleep 2.5; exec env -u DISPLAY WAYLAND_DISPLAY=rm-e2e GDK_BACKEND=wayland python3 "$RM_ROOT/scripts/animate.py" >"$RM_LOGS/content.log" 2>&1 ) &
  cpid=$!
  "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" $RM_CLIENT_ARGS
  rc=$?
  sleep 0.5
  kill $cpid $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
  exit $rc
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
echo "logs: $RM_LOGS"
