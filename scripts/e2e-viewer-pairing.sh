#!/bin/sh
# Pairing confirmed by the host's owner, with nothing real involved: a host in a throwaway
# headless Mutter whose pairing is opened over its control socket (as Remoteman Host does),
# and the viewer in a second one. The viewer must show the same code as the host holds; a
# decline must leave nothing behind on either side; an accept must pair and connect.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7796}"
cat > "$RM_LOGS/tell.py" <<'PY'
import socket, sys
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall((sys.argv[2] + "\n").encode()); s.shutdown(socket.SHUT_WR)
print(s.recv(65536).decode().strip())
PY
dbus-run-session -- mutter --headless --wayland --no-x11 --wayland-display rm-view --virtual-monitor 1920x1080@60 >"$RM_LOGS/mutter-view.log" 2>&1 &
vpid=$!
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"
  tell() { python3 -I "$RM_LOGS/tell.py" "$RM_LOGS/host.sock" "$1"; }
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --state "$RM_LOGS/state" --control "$RM_LOGS/host.sock" --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  # The owner: opens pairing, declines the device that comes (at 5 s), opens pairing again
  # and accepts it (at 11 s). What the host holds is written down just before each answer.
  ( tell "pair on"; sleep 5; tell status >"$RM_LOGS/first.json"; tell "pair decline"; sleep 1; tell status >"$RM_LOGS/declined.json"
    tell "pair on"; sleep 5; tell status >"$RM_LOGS/second.json"; tell "pair accept"; sleep 3; tell status >"$RM_LOGS/accepted.json" ) >"$RM_LOGS/owner.log" 2>&1 &
  # The viewer: tries to pair at about 2 s (declined), and again at about 8 s (accepted).
  env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland timeout 40 "$bin/remoteman" --state "$RM_LOGS/vstate" --size 1920x1080 --scale 1 --refresh 60 --ui "wait=1,pair-go=127.0.0.1:$RM_PORT,wait=5,shot=$RM_LOGS/declined.png,click=Pair,wait=7,shot=$RM_LOGS/paired.png" >"$RM_LOGS/viewer.log" 2>"$RM_LOGS/viewer.err"
  sleep 0.5
  kill $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
pkill -P $vpid 2>/dev/null; kill $vpid 2>/dev/null; wait $vpid 2>/dev/null
code() { python3 -I -c 'import json,sys; d=json.load(open(sys.argv[1])); p=d.get("pending") or {}; print(p.get("code",""), "| devices:", len(d["devices"]), "| client:", "yes" if d.get("client") else "no")' "$1" 2>/dev/null; }
echo "the host, before declining: $(code "$RM_LOGS/first.json")"
echo "the host, after declining:  $(code "$RM_LOGS/declined.json")"
echo "the host, before accepting: $(code "$RM_LOGS/second.json")"
echo "the host, after accepting:  $(code "$RM_LOGS/accepted.json")"
grep '^~ pairing code' "$RM_LOGS/viewer.log"
fail=0
shown=$(grep '^~ pairing code' "$RM_LOGS/viewer.log" | sed 's/^~ pairing code //' | sort -u)
held=$(code "$RM_LOGS/first.json" | cut -d'|' -f1 | sed 's/ $//')
[ -n "$held" ] && [ "$shown" = "$held" ] || { echo "the viewer and the host did not show the same code ($shown / $held)"; fail=1; }
[ "$(grep -c '^~ pairing code' "$RM_LOGS/viewer.log")" = 2 ] || { echo "the viewer was not shown the code both times"; fail=1; }
code "$RM_LOGS/declined.json" | grep -q "^ | devices: 0 | client: no" || { echo "a declined device was kept"; fail=1; }
code "$RM_LOGS/accepted.json" | grep -q "^ | devices: 1 | client: yes" || { echo "an accepted device was not paired and connected"; fail=1; }
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; cat "$RM_LOGS/owner.log" | head -6; grep -v 'Gtk-\|^$' "$RM_LOGS/viewer.err" | tail -4; exit 1
