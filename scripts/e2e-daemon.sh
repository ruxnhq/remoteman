#!/bin/sh
# The daemon path with nothing real involved: remotemand on the loopback, and two throwaway
# headless Mutters (each on its own D-Bus session) with an agent attached to it, standing in for
# a login screen's session and a user's. The screenless test client connects, and part way
# through the daemon moves it from one agent to the other without the client reconnecting.
#   scripts/e2e-daemon.sh [test client options]
# RM_SWITCH_AFTER=N moves the client N seconds in (default 6; 0: never).
# Logs land in $RM_LOGS (default: a fresh temporary folder, printed at the end).
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7794}"
# A socket's path must be short, which a logs folder may not be.
export RM_SOCK="${XDG_RUNTIME_DIR:-/tmp}/remoteman-e2e-$$.sock" RM_SWITCH_AFTER="${RM_SWITCH_AFTER:-6}"
bin="$root/target/release"
"$bin/remotemand" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --socket "$RM_SOCK" >"$RM_LOGS/daemon.log" 2>&1 &
dpid=$!
# One desktop session with an agent: $1 its name, $2 seconds until its moving content starts.
session() {
  RM_NAME=$1 RM_CONTENT_AFTER=$2 dbus-run-session -- sh -c '
    mutter --headless --wayland --no-x11 --wayland-display rm-$RM_NAME >"$RM_LOGS/mutter-$RM_NAME.log" 2>&1 &
    mpid=$!
    sleep 2
    "$RM_ROOT/target/release/remoteman-agent" --daemon --socket "$RM_SOCK" >"$RM_LOGS/agent-$RM_NAME.log" 2>&1 &
    apid=$!
    ( sleep "$RM_CONTENT_AFTER"; exec env -u DISPLAY WAYLAND_DISPLAY=rm-$RM_NAME GDK_BACKEND=wayland python3 "$RM_ROOT/scripts/animate.py" >"$RM_LOGS/content-$RM_NAME.log" 2>&1 ) &
    cpid=$!
    trap "kill $cpid $apid 2>/dev/null; sleep 0.3; kill $mpid 2>/dev/null" TERM INT
    wait $apid
  ' >/dev/null 2>&1 &
}
# "first" attaches first (agent 1), "second" after it (agent 2): the client starts on the newest.
session first $((RM_SWITCH_AFTER + 5)); s1=$!
sleep 1
session second 6; s2=$!
sleep 3
"$bin/remotemand" ctl --socket "$RM_SOCK" status
if [ "$RM_SWITCH_AFTER" != 0 ]; then
  ( sleep "$RM_SWITCH_AFTER"; "$bin/remotemand" ctl --socket "$RM_SOCK" switch 1; sleep 1.5; "$bin/remotemand" ctl --socket "$RM_SOCK" status ) &
fi
"$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" "$@"
rc=$?
sleep 0.5
"$bin/remotemand" ctl --socket "$RM_SOCK" status
pkill -TERM -P $s1 2>/dev/null; pkill -TERM -P $s2 2>/dev/null; kill $s1 $s2 2>/dev/null
sleep 1
kill $dpid 2>/dev/null
wait 2>/dev/null
rm -f "$RM_SOCK"
echo "logs: $RM_LOGS"
exit $rc
