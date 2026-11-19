#!/bin/sh
# The way-in choice, as far as it goes without root: a daemon with no desktop session attached
# (nobody logged in, as it were). A device that asks only for a session that is already running
# must be turned away saying so; the same device without that wish gets the daemon's usual
# answer for having nowhere to send it (with root and GDM that is where the login screen
# comes; scripts/remote-login-test.sh covers that).
root=$(dirname "$(dirname "$(readlink -f "$0")")")
RM_LOGS="${RM_LOGS:-$(mktemp -d)}"; RM_PORT="${RM_PORT:-7794}"
sock="${XDG_RUNTIME_DIR:-/tmp}/remoteman-e2e-$$.sock"
bin="${RM_BIN:-$root/target/release}"
"$bin/remotemand" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --socket "$sock" >"$RM_LOGS/daemon.log" 2>&1 &
dpid=$!
sleep 1
client() { "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/client" --seconds 2 "$@" 2>&1 | grep -E "^stream|refused|ended the session" | head -1 | cut -c1-170; }
only=$(client --attach-only)
any=$(client)
kill $dpid 2>/dev/null; wait 2>/dev/null; rm -f "$sock"
echo "asking only for a running session: $only"
echo "asking for any way in:             $any"
fail=0
case "$only" in *"nobody is logged in at this computer"*) ;; *) echo "the device was not told that nobody is logged in"; fail=1 ;; esac
case "$any" in *"set not to use its login screen"*) echo "a device that takes any way in was turned away as if it didn't"; fail=1 ;; esac
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; exit 1
