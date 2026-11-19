#!/bin/sh
# The viewer remembering a locked computer's password, with nothing real involved: a host told
# to behave as if locked (REMOTEMAN_TEST_LOCKED_PASSWORD), and the viewer on a bus of its own
# with a keyring of its own (a GNOME Keyring started for the test in a throwaway home: the
# real keyring is on another bus and is never touched). The password is typed once and kept;
# the next time the viewer is asked, it answers by itself.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7796}"
command -v gnome-keyring-daemon >/dev/null || { echo "SKIP: no gnome-keyring-daemon"; exit 0; }
cat > "$RM_LOGS/tell.py" <<'PY'
import socket, sys
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall((sys.argv[2] + "\n").encode()); s.shutdown(socket.SHUT_WR)
print(s.recv(65536).decode().strip())
PY
mkdir -p "$RM_LOGS/home"
dbus-run-session -- sh -c '
  bin="${RM_BIN:-$RM_ROOT/target/release}"
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  REMOTEMAN_TEST_LOCKED_PASSWORD="open sesame" "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --control "$RM_LOGS/host.sock" --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  echo "$! $mpid" > "$RM_LOGS/host.pids"
  wait
' >/dev/null 2>&1 &
sleep 4
tell() { python3 -I "$RM_LOGS/tell.py" "$RM_LOGS/host.sock" "$1" >/dev/null; }
# The viewer pairs while the computer lets paired devices in by their key; then it asks.
tell "allow unlock key"
HOME="$RM_LOGS/home" XDG_DATA_HOME="$RM_LOGS/home/data" XDG_CONFIG_HOME="$RM_LOGS/home/config" XDG_CACHE_HOME="$RM_LOGS/home/cache" dbus-run-session -- sh -c '
  bin="${RM_BIN:-$RM_ROOT/target/release}"
  # A keyring of the test own, unlocked with a password of the test own.
  printf "test" | gnome-keyring-daemon --unlock --components=secrets >/dev/null 2>&1
  # (It stays running; the newest process of that name is it. It is stopped by that number.)
  kpid=$(pgrep -n -x gnome-keyring-d)
  mutter --headless --wayland --no-x11 --wayland-display rm-view --virtual-monitor 1920x1080@60 >"$RM_LOGS/mutter-view.log" 2>&1 &
  vpid=$!
  sleep 2
  view() { log=$1; shift; env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland timeout 40 "$bin/remoteman" --state "$RM_LOGS/vstate" --size 1920x1080 --scale 1 --refresh 60 --ui "$1" >"$RM_LOGS/$log.log" 2>&1; }
  view pair "wait=1,pair-go=127.0.0.1:$RM_PORT,wait=4"
  python3 -I "$RM_LOGS/tell.py" "$RM_LOGS/host.sock" "allow unlock password" >/dev/null
  view typed "wait=1,connect=127.0.0.1:$RM_PORT,wait=3,unlock=open sesame,wait=5,size?"
  secret-tool search application dev.remoteman.Viewer >"$RM_LOGS/kept.txt" 2>&1
  view kept "wait=1,connect=127.0.0.1:$RM_PORT,wait=6,size?"
  kill $vpid $kpid 2>/dev/null
  wait 2>/dev/null
' >/dev/null 2>&1
read -r apid mpid < "$RM_LOGS/host.pids"
kill "$apid" 2>/dev/null; sleep 0.5; kill "$mpid" 2>/dev/null; wait 2>/dev/null
grep -h '^~ password\|^~ unlocking\|^~ window' "$RM_LOGS/typed.log" "$RM_LOGS/kept.log" 2>/dev/null
echo "in the test keyring: $(grep -c '^secret = ' "$RM_LOGS/kept.txt") entry, labelled: $(grep '^label = ' "$RM_LOGS/kept.txt" | head -1 | cut -c9-)"
fail=0
grep -q '^~ password kept in the keyring: true' "$RM_LOGS/typed.log" || { echo "the typed password was not kept"; fail=1; }
grep -q '^secret = open sesame' "$RM_LOGS/kept.txt" || { echo "the keyring does not hold it"; fail=1; }
grep -q '^~ unlocking with the password kept in the keyring' "$RM_LOGS/kept.log" || { echo "the kept password was not used the next time"; fail=1; }
grep -q '^~ window 1280' "$RM_LOGS/kept.log" || { echo "the viewer was not in a session after using the kept password"; fail=1; }
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; grep -v "Gtk-\|^$\|nearby" "$RM_LOGS/typed.log" | tail -4; exit 1
