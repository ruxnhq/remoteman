#!/bin/sh
# The Linux viewer follows the host's switches while connected, with nothing real involved: a
# host in a throwaway headless Mutter that allows neither the clipboard nor files, the viewer in
# a second one. The host's owner switches both on over its control socket, then off again; the
# viewer's sharing menu offers them, then doesn't, without connecting again. A line copied in the
# viewer while the clipboard is shared reaches the host; one copied after the owner switched it
# off is not sent.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7796}"
mkdir -p "$RM_LOGS"
cat > "$RM_LOGS/tell.py" <<'PY'
import socket, sys
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall((sys.argv[2] + "\n").encode()); s.shutdown(socket.SHUT_WR)
print(sys.argv[2], "->", s.recv(4096).decode().strip())
PY
dbus-run-session -- mutter --headless --wayland --no-x11 --wayland-display rm-view --virtual-monitor 1920x1080@60 >"$RM_LOGS/mutter-view.log" 2>&1 &
vpid=$!
dbus-run-session -- sh -c '
  bin="${RM_BIN:-$RM_ROOT/target/release}"
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --control "$RM_LOGS/host.sock" --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  # The viewer asks what is offered at about 9.5 s and 14.5 s, shares the clipboard and copies a
  # line at about 16 s, copies another at about 20.5 s and asks once more.
  ( sleep 11.5; for what in clipboard files; do python3 -I "$RM_LOGS/tell.py" "$RM_LOGS/host.sock" "allow $what on"; done
    sleep 7;    for what in clipboard files; do python3 -I "$RM_LOGS/tell.py" "$RM_LOGS/host.sock" "allow $what off"; done ) >"$RM_LOGS/owner.log" 2>&1 &
  env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland timeout 40 "$bin/remoteman" --state "$RM_LOGS/vstate" --size 1920x1080 --scale 1 --refresh 60 --ui "wait=1,pair-go=127.0.0.1:$RM_PORT,wait=3,click=Connect,wait=3,offers?,wait=4.5,offers?,share,copy=copied while shared,wait=3.5,copy=copied after it was switched off,wait=1,offers?" >"$RM_LOGS/viewer.log" 2>"$RM_LOGS/viewer.err"
  sleep 0.5
  kill $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
pkill -P $vpid 2>/dev/null; kill $vpid 2>/dev/null; wait $vpid 2>/dev/null
cat "$RM_LOGS/owner.log"
grep '^~ the ' "$RM_LOGS/viewer.log"
want="clipboard false, files false
clipboard true, files true
clipboard false, files false"
got=$(grep '^~ the sharing menu' "$RM_LOGS/viewer.log" | sed 's/^[^:]*: //; s/, shared.*//')
copies=$(sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep -c "a copy on the device is on the desktop")
echo "copies the host took from the viewer: $copies"
sessions=$(sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep -c 'session: client name')
[ "$got" = "$want" ] && [ "$sessions" = 1 ] && [ "$copies" = 1 ] && { echo "PASS (one session throughout)"; exit 0; }
echo "FAIL (sessions: $sessions; logs in $RM_LOGS)"; grep -v 'Gtk-\|^$' "$RM_LOGS/viewer.err" | tail -5; exit 1
