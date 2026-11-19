#!/bin/sh
# A locked computer, with nothing locked or unlocked for real: the host is told to behave as
# if its session were locked, with a password of the test's (REMOTEMAN_TEST_LOCKED_PASSWORD;
# the real session's lock and the system's password check are never touched). Its owner sets
# what a paired device needs; the viewer and the test client are the devices.
#   off: a paired device is refused, with the reason.
#   password: the viewer is asked for it, a wrong one is turned down, the right one gets in,
#     as the only display; a device that asks to mirror gets the computer's own monitor.
#   key: a paired device gets in with nothing asked.
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
  tell() { python3 -I "$RM_LOGS/tell.py" "$RM_LOGS/host.sock" "$1" >/dev/null; }
  # A device: its first word names its log; the rest are options of the test client.
  client() { log="$RM_LOGS/device-$1.log"; shift; "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --state "$RM_LOGS/client" --seconds 4 "$@" >"$log" 2>&1; grep -E "^stream|refused|ended the session" "$log" | head -1 | cut -c1-150; }
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e --virtual-monitor 1600x900@60 >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  REMOTEMAN_TEST_LOCKED_PASSWORD="open sesame" "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --control "$RM_LOGS/host.sock" --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  # Pairing is not what is tested: the computer lets devices in by their key while they pair.
  tell "allow unlock key"
  echo "key, pairing:      $(client pair --pair)"
  tell "allow unlock off"
  echo "off:               $(client off)"
  tell "allow unlock password"
  echo "password, none:    $(client none)"
  echo "password, wrong:   $(client wrong --unlock nonsense)"
  echo "password, right:   $(client right --unlock "open sesame" --size 1280x720 --scale 1)"
  echo "password, mirror:  $(client mirror --unlock "open sesame" --size 1280x720 --scale 1 --placement mirror)"
  # The viewer is paired first (pairing opened by the owner, who confirms it after 4 s).
  tell "pair on"
  ( sleep 4; tell "pair accept" ) &
  # Then: asked for the password, answers wrongly, then rightly, and is in.
  env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland timeout 40 "$bin/remoteman" --state "$RM_LOGS/vstate" --size 1920x1080 --scale 1 --refresh 60 --ui "wait=1,pair-go=127.0.0.1:$RM_PORT,wait=6,shot=$RM_LOGS/pairing.png" >"$RM_LOGS/viewer-pair.log" 2>&1
  env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland timeout 40 "$bin/remoteman" --state "$RM_LOGS/vstate" --size 1920x1080 --scale 1 --refresh 60 --ui "wait=1,connect=127.0.0.1:$RM_PORT,wait=3,shot=$RM_LOGS/asked.png,unlock=nonsense,wait=3,shot=$RM_LOGS/wrong.png,unlock=open sesame,wait=4,shot=$RM_LOGS/in.png,size?" >"$RM_LOGS/viewer.log" 2>"$RM_LOGS/viewer.err"
  sleep 0.5
  kill $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$" | tee "$RM_LOGS/devices.txt"
pkill -P $vpid 2>/dev/null; kill $vpid 2>/dev/null; wait $vpid 2>/dev/null
log() { sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log"; }
echo "the host: $(log | grep -c "unlocked for a paired device") unlocks, $(log | grep -c "locked again") locks again, $(log | grep -c "a wrong password") wrong passwords"
fail=0
want() { grep -q "$1" "$RM_LOGS/devices.txt" || { echo "not as expected: $2"; fail=1; }; }
want "^key, pairing:      stream" "a device with the key setting was not let in"
want "^off:               .*set not to be unlocked" "a locked computer set to off let a device in, or gave no reason"
want "^password, none:    .*locked: This computer is locked. Enter the password" "no password was asked for"
want "^password, wrong:   .*locked: That password is not right" "a wrong password was not turned down"
want "^password, right:   stream.* 1280x720" "the right password did not get the device its display"
want "^password, mirror:  stream.* 1600x900" "a device that asked to mirror was not shown the computer's monitor"
log | grep -q "own screens stay off while it is here" || { echo "the unlocked computer's own screens were not switched off"; fail=1; }
# The viewer's second run: refused twice (no password, the wrong one), then in.
[ "$(log | grep -c "client name=blackbox\|client name=$(cat /etc/hostname)")" -ge 1 ] || { echo "the viewer never got in"; fail=1; }
[ -s "$RM_LOGS/in.png" ] || { echo "no picture of the viewer after unlocking"; fail=1; }
[ $fail = 0 ] && { echo "PASS ($RM_LOGS/asked.png, wrong.png, in.png)"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; log | grep -E "WARN|ERROR" | cut -c29-200 | head -6; grep -v 'Gtk-\|^$' "$RM_LOGS/viewer.err" | tail -4; exit 1
