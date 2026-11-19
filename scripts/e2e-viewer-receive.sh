#!/bin/sh
# A file from the host to the Linux viewer, with nothing real involved: a host in a throwaway
# headless Mutter that allows files, the viewer in a second one with a folder of the test's as
# its place for received files (not Downloads). The host's owner sends a file over its control
# socket; it must arrive whole in the viewer's folder, and the host must hear that it did.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7796}"
mkdir -p "$RM_LOGS/vstate" "$RM_LOGS/received"
printf 'folder=%s\n' "$RM_LOGS/received" > "$RM_LOGS/vstate/viewer-settings"
head -c 3000000 /dev/urandom > "$RM_LOGS/from the host.bin"
cat > "$RM_LOGS/tell.py" <<'PY'
import socket, sys
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall((sys.argv[2] + "\n").encode()); s.shutdown(socket.SHUT_WR)
print(s.recv(65536).decode().strip())
PY
dbus-run-session -- mutter --headless --wayland --no-x11 --wayland-display rm-view --virtual-monitor 1920x1080@60 >"$RM_LOGS/mutter-view.log" 2>&1 &
vpid=$!
dbus-run-session -- sh -c '
  bin="${RM_BIN:-$RM_ROOT/target/release}"
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --control "$RM_LOGS/host.sock" --allow files --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  # The viewer is in from about 9 s; the file is sent at 11 s and asked after at 15 s.
  ( sleep 11; python3 -I "$RM_LOGS/tell.py" "$RM_LOGS/host.sock" "send $RM_LOGS/from the host.bin"; sleep 4; python3 -I "$RM_LOGS/tell.py" "$RM_LOGS/host.sock" status ) >"$RM_LOGS/owner.log" 2>&1 &
  env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland timeout 40 "$bin/remoteman" --state "$RM_LOGS/vstate" --size 1920x1080 --scale 1 --refresh 60 --ui "wait=1,pair-go=127.0.0.1:$RM_PORT,wait=3,click=Connect,wait=11" >"$RM_LOGS/viewer.log" 2>"$RM_LOGS/viewer.err"
  sleep 0.5
  kill $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
pkill -P $vpid 2>/dev/null; kill $vpid 2>/dev/null; wait $vpid 2>/dev/null
head -1 "$RM_LOGS/owner.log"
grep '^~ received' "$RM_LOGS/viewer.log" | sed "s|$RM_LOGS/||"
fail=0
[ "$(head -1 "$RM_LOGS/owner.log")" = ok ] || { echo "the host did not start sending"; fail=1; }
if cmp -s "$RM_LOGS/from the host.bin" "$RM_LOGS/received/from the host.bin"; then echo "the file arrived whole: $(stat -c %s "$RM_LOGS/received/from the host.bin") bytes"; else echo "the file did not arrive whole"; ls -la "$RM_LOGS/received"; fail=1; fi
grep -q '"sent":{"answered":1,"ok":true' "$RM_LOGS/owner.log" || { echo "the host did not hear that the file arrived"; fail=1; }
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; tail -2 "$RM_LOGS/owner.log" | cut -c1-300; grep -v 'Gtk-\|^$' "$RM_LOGS/viewer.err" | tail -4; exit 1
