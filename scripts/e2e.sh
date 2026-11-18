#!/bin/sh
# End-to-end test with nothing real involved: a throwaway headless Mutter on a private D-Bus
# session, the agent inside it on the loopback, moving content, and the screenless test client.
# Leaves the live desktop alone.
#   scripts/e2e.sh [test client options, e.g. --decoders hevc444 --seconds 10 --input-demo]
# RM_MUTTER_ARGS adds options to that Mutter, e.g. "--virtual-monitor 1920x1080@60" to give it a
# screen of its own, standing in for a PC's real one.
# RM_CONTENT_ARGS goes to the moving content, e.g. "--monitor Meta-1" to pin it to a monitor.
# RM_AGENT_ARGS adds options to the agent (e.g. "--allow files"); RM_AGENT_ENV sets variables
# for it (e.g. "XDG_CONFIG_HOME=/some/folder", to point its Downloads folder elsewhere).
# RM_KILL_AGENT_AFTER=N kills the agent with no warning N seconds after it starts.
# Logs land in $RM_LOGS (default: a fresh temporary folder, printed at the end).
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7799}"
export RM_CLIENT_ARGS="$*"
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e $RM_MUTTER_ARGS >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  env $RM_AGENT_ENV "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" $RM_AGENT_ARGS >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  # The content needs the virtual monitor, which exists once the client is in.
  ( sleep 2.5; exec env -u DISPLAY WAYLAND_DISPLAY=rm-e2e GDK_BACKEND=wayland python3 "$RM_ROOT/scripts/animate.py" $RM_CONTENT_ARGS >"$RM_LOGS/content.log" 2>&1 ) &
  cpid=$!
  # For crash tests: kill the agent outright this many seconds in.
  [ -n "$RM_KILL_AGENT_AFTER" ] && ( sleep "$RM_KILL_AGENT_AFTER"; kill -9 $apid ) &
  # For layout tests: what the monitors look like while the client is in, and after.
  ( sleep 6; gdctl show >"$RM_LOGS/monitors-during.txt" 2>&1 ) &
  "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" $RM_CLIENT_ARGS
  rc=$?
  sleep 1; gdctl show >"$RM_LOGS/monitors-after.txt" 2>&1
  sleep 0.5
  kill $cpid $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
  exit $rc
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
echo "logs: $RM_LOGS"
