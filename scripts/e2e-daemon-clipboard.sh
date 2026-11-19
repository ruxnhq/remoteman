#!/bin/sh
# The owner switching the clipboard off while it is being shared, on a host that is the daemon,
# with nothing real involved: remotemand on the loopback (run as this user, no root), a
# throwaway headless Mutter with an agent attached to it, an ordinary GTK app standing in for
# the user's programs, and the test client as the device. While the clipboard is allowed, what
# the app copies must reach the device; once the owner has switched it off at the agent's
# control socket (as Remoteman Host does), the app's next copy must not, and the sharing must
# be taken down as soon as the device does anything.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7795}"
export RM_SOCK="${XDG_RUNTIME_DIR:-/tmp}/remoteman-e2e-$$.sock" RM_CTL="${XDG_RUNTIME_DIR:-/tmp}/remoteman-e2e-$$-owner.sock"
bin="$root/target/release"
cat > "$RM_LOGS/tell.py" <<'PY'
import socket, sys
s = socket.socket(socket.AF_UNIX); s.settimeout(5); s.connect(sys.argv[1]); s.sendall((sys.argv[2] + "\n").encode()); s.shutdown(socket.SHUT_WR)
print(s.recv(65536).decode().strip())
PY
tell() { python3 -I "$RM_LOGS/tell.py" "$RM_CTL" "$1"; }
printf 'while it was allowed ✓' > "$RM_LOGS/desktop1.txt"
printf 'after it was switched off' > "$RM_LOGS/desktop2.txt"
# The device shares its clipboard at 3 s; at 12 s, after the owner's switch, it moves its pointer.
printf 'wait 3\nclip copied on the device\nwait 9\nmove 0.5 0.5\nwait 2\n' > "$RM_LOGS/steps.txt"
"$bin/remotemand" --listen 127.0.0.1:$RM_PORT --state "$RM_LOGS/state" --socket "$RM_SOCK" >"$RM_LOGS/daemon.log" 2>&1 &
dpid=$!
dbus-run-session -- sh -c '
  mutter --headless --wayland --no-x11 --wayland-display rm-dclip >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$RM_ROOT/target/release/remoteman-agent" --daemon --socket "$RM_SOCK" --control "$RM_CTL" --state "$RM_LOGS/agent-state" --allow clipboard --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  echo "$apid $mpid" > "$RM_LOGS/session.pids"
  # The app needs the virtual monitor, which exists once the device is in ("go"). It copies a
  # line 7 s into the session of the device and another at 11 s.
  while [ ! -e "$RM_LOGS/go" ]; do sleep 0.1; done
  sleep 2.5
  ( sleep 4.5; echo "copy-text $RM_LOGS/desktop1.txt"; sleep 4; echo "copy-text $RM_LOGS/desktop2.txt"; sleep 3; echo quit ) | env -u DISPLAY WAYLAND_DISPLAY=rm-dclip GDK_BACKEND=wayland python3 "$RM_ROOT/crates/remoteman-desktop/examples/clipboard_helper.py" >"$RM_LOGS/app.log" 2>&1
  wait $apid
' >/dev/null 2>&1 &
spid=$!
sleep 4
fail=0
[ "$(tell 'pair on')" = ok ] || { echo "pairing did not open"; fail=1; }
"$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/client" --seconds 15 --size 1280x720 --scale 1 --script "$RM_LOGS/steps.txt" >"$RM_LOGS/client.log" 2>&1 &
cpid=$!
# A device new to a daemon waits for its owner: let it in, and its session starts.
sleep 1.5
[ "$(tell 'pair accept')" = ok ] || { echo "the device was not let in"; fail=1; }
touch "$RM_LOGS/go"
# At 9.5 s: after the first copy has arrived, before the second is made.
sleep 9.5
said=$(tell 'allow clipboard off')
wait $cpid
read -r apid mpid < "$RM_LOGS/session.pids"
kill "$apid" 2>/dev/null; sleep 0.5; kill "$mpid" "$dpid" 2>/dev/null; wait 2>/dev/null
rm -f "$RM_SOCK" "$RM_CTL"
log() { sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log"; }
echo "the device got: $(grep 'clipboard from the host' "$RM_LOGS/client.log" | sed 's/^[^}]*}: //' | tr '\n' '|')"
log | grep -i "clipboard" | cut -c29-120
[ "$said" = ok ] || { echo "the owner's switch was not taken: $said"; fail=1; }
grep -q 'clipboard from the host .*: while it was allowed ✓' "$RM_LOGS/client.log" || { echo "the device did not get what was copied while it was allowed"; fail=1; }
grep -q 'after it was switched off' "$RM_LOGS/client.log" && { echo "the device got what was copied after the clipboard was switched off"; fail=1; }
log | grep -q "the clipboard is no longer shared with the device" || { echo "the sharing was not taken down"; fail=1; }
grep -q '^frames [1-9]' "$RM_LOGS/client.log" || { echo "no picture"; fail=1; }
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; log | grep -E 'WARN|ERROR' | cut -c29-200 | head -5; head -5 "$RM_LOGS/app.log"; exit 1
