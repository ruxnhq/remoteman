#!/bin/sh
# Remoteman Host tells its owner when a device connects, with nothing real involved: a host in a
# throwaway headless Mutter with a control socket, Remoteman Host on a private bus of its own
# (so the one in the top bar is left alone) with its choices kept in a folder of the test's, and
# the test client as the device. Also saves a picture of the window's Sharing page.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7797}"
mkdir -p "$RM_LOGS/tstate/remoteman"
echo on > "$RM_LOGS/tstate/remoteman/tray-notify"
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e --virtual-monitor 1280x800@60 >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --control "$RM_LOGS/host.sock" --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  env -u DISPLAY WAYLAND_DISPLAY=rm-e2e GDK_BACKEND=wayland XDG_STATE_HOME="$RM_LOGS/tstate" XDG_CONFIG_HOME="$RM_LOGS/tconfig" REMOTEMAN_HOST_SOCKET="$RM_LOGS/host.sock" REMOTEMAN_HOST_UNIT=remoteman-none.service \
    "$bin/remoteman-tray" --shot "$RM_LOGS/sharing.png" --after 14 >"$RM_LOGS/tray.log" 2>&1 &
  tpid=$!
  sleep 3
  "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/client" --seconds 4 >"$RM_LOGS/client.log" 2>&1
  # A second device asks to pair while pairing is open from here: the question comes up, and
  # is answered from elsewhere (the control socket) while it is showing, which must take the
  # question away and nothing else.
  tell() { printf "%s\n" "$1" | python3 -I -c "import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall(sys.stdin.buffer.read()); s.shutdown(1); s.recv(4096)" "$RM_LOGS/host.sock"; }
  tell "pair on"
  "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/client2" --seconds 3 >"$RM_LOGS/client2.log" 2>&1 &
  sleep 2.5
  tell "pair accept"
  wait $tpid
  kill $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
grep -E "notified|saved|icon" "$RM_LOGS/tray.log"
grep -q "^asking: pair with" "$RM_LOGS/tray.log" || { echo "the question about a new device never came up"; echo "FAIL (logs in $RM_LOGS)"; exit 1; }
grep -q "^saved " "$RM_LOGS/tray.log" || { echo "Remoteman Host did not live to the end (it crashed when the question was answered elsewhere?)"; echo "FAIL (logs in $RM_LOGS)"; tail -5 "$RM_LOGS/tray.log"; exit 1; }
[ "$(grep -c '^notified: ' "$RM_LOGS/tray.log")" -ge 1 ] && { echo "PASS ($RM_LOGS/sharing.png)"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; tail -5 "$RM_LOGS/tray.log"; exit 1
