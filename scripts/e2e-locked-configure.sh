#!/bin/sh
# A computer unlocked for a device stays unlocked while that device is connected, whatever the
# device asks for: a `configure` the host can't follow (a size no display can have) must not
# lock the computer under it. Nothing is locked or unlocked for real: the host only behaves as
# if its session were locked (REMOTEMAN_TEST_LOCKED_PASSWORD, as scripts/e2e-viewer-locked.sh).
# The device gets in with the password, asks for a display 1281 pixels wide (sizes must be
# even), and stays six more seconds. Checked in the host's log: it locks again once, and only
# after it had gone back to the picture before (which it does while the device is connected;
# the lock belongs to the device leaving); and the device has a picture again.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7789}"
cat > "$RM_LOGS/tell.py" <<'PY'
import socket, sys
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall((sys.argv[2] + "\n").encode()); s.shutdown(socket.SHUT_WR)
print(s.recv(65536).decode().strip())
PY
printf 'wait 3\nresize 1281x720\nwait 6\n' > "$RM_LOGS/steps.txt"
dbus-run-session -- sh -c '
  bin="${RM_BIN:-$RM_ROOT/target/release}"
  mutter --headless --wayland --no-x11 --wayland-display rm-lockcfg --virtual-monitor 1600x900@60 >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  REMOTEMAN_TEST_LOCKED_PASSWORD="open sesame" "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --control "$RM_LOGS/host.sock" >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1.5
  python3 -I "$RM_LOGS/tell.py" "$RM_LOGS/host.sock" "allow unlock password" >/dev/null
  ( sleep 4; exec env -u DISPLAY WAYLAND_DISPLAY=rm-lockcfg GDK_BACKEND=wayland python3 "$RM_ROOT/scripts/animate.py" >"$RM_LOGS/content.log" 2>&1 ) &
  cpid=$!
  "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/client" --unlock "open sesame" --size 1280x720 --scale 1 --seconds 11 --script "$RM_LOGS/steps.txt" >"$RM_LOGS/client.log" 2>&1
  sleep 1
  kill $cpid $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
log() { sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log"; }
line() { log | grep -n "$1" | head -1 | cut -d: -f1; }
unlocked=$(line "unlocked for a paired device"); asked=$(line "asked for another picture"); locked=$(line "locked again"); back=$(line "going back to the one before")
echo "the host's log: unlocked at line ${unlocked:-none}, the refused size at ${asked:-none}, going back at ${back:-none}, locked again at ${locked:-none}; $(log | grep -c "locked again") locks"
grep -E "^stream" "$RM_LOGS/client.log" | cut -c1-120
fail=0
[ -n "$unlocked" ] || { echo "the device was never let in by its password"; fail=1; }
[ -n "$asked" ] || { echo "the host never saw the configure"; fail=1; }
[ -n "$back" ] || { echo "the host never refused the size"; fail=1; }
[ -n "$locked" ] && [ -n "$back" ] && [ "$locked" -gt "$back" ] || { echo "the computer was locked while the device was still connected (or never locked again)"; fail=1; }
[ "$(log | grep -c "locked again")" = 1 ] || { echo "expected exactly one lock"; fail=1; }
[ "$(log | grep -c "couldn.t restore the picture")" = 0 ] || { echo "the picture was not restored after the refused size"; fail=1; }
[ "$(grep -c "^stream" "$RM_LOGS/client.log")" -ge 2 ] || { echo "the device was not given its picture back"; fail=1; }
[ $fail = 0 ] && { echo "PASS"; echo "logs: $RM_LOGS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; exit 1
