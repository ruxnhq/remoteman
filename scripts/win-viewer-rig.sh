#!/bin/sh
# A throwaway host for testing a viewer on another machine or in a VM by hand or by script,
# with what scripts/headless-host.sh doesn't have: a control socket (as Remoteman Host uses it:
# "pair on", "pair accept", "allow unlock password", "send PATH"), a monitor of its own to
# mirror, a password it pretends to be locked with, and either moving content or an ordinary app
# that copies and pastes when told to. Leaves the live desktop and the everyday host alone.
#   scripts/win-viewer-rig.sh NAME PORT SECONDS content|clipboard [--pair]
# Logs go to $RM_LOGS/NAME; the control socket is $XDG_RUNTIME_DIR/rmv-NAME.sock (a socket's
# path has to be short). In clipboard mode the app reads commands from $RM_LOGS/NAME/app.in
# (crates/remoteman-desktop/examples/clipboard_helper.py) and answers in app.log.
# Talk to the host with:  scripts/win-viewer-rig.sh tell NAME "pair on"
root=$(dirname "$(dirname "$(readlink -f "$0")")")
if [ "$1" = tell ]; then
  exec python3 -I -c 'import socket, sys
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall((sys.argv[2] + "\n").encode()); s.shutdown(socket.SHUT_WR)
print(s.recv(65536).decode().strip())' "$XDG_RUNTIME_DIR/rmv-$2.sock" "$3"
fi
export RM_ROOT="$root" RM_NAME="$1" RM_PORT="$2" RM_SECONDS="${3:-600}" RM_MODE="${4:-content}" RM_PAIR="$5"
export RM_LOGS="${RM_LOGS:-$(mktemp -d)}/$1" RM_SOCK="$XDG_RUNTIME_DIR/rmv-$1.sock"
mkdir -p "$RM_LOGS"; rm -f "$RM_SOCK"
echo "logs: $RM_LOGS"
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"
  mutter --headless --wayland --no-x11 --wayland-display "rmv-$RM_NAME" --virtual-monitor 1600x900@60 >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  REMOTEMAN_TEST_LOCKED_PASSWORD="open sesame" "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT $RM_PAIR --state "$RM_LOGS/state" --allow clipboard,files --keep-output --control "$RM_SOCK" >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  if [ "$RM_MODE" = clipboard ]; then
    : >"$RM_LOGS/app.in"
    ( tail -f "$RM_LOGS/app.in" | env -u DISPLAY WAYLAND_DISPLAY="rmv-$RM_NAME" GDK_BACKEND=wayland python3 "$RM_ROOT/crates/remoteman-desktop/examples/clipboard_helper.py" >"$RM_LOGS/app.log" 2>&1 ) &
  else
    ( while :; do env -u DISPLAY WAYLAND_DISPLAY="rmv-$RM_NAME" GDK_BACKEND=wayland python3 "$RM_ROOT/scripts/animate.py" >>"$RM_LOGS/content.log" 2>&1; sleep 1; done ) &
  fi
  cpid=$!
  echo "listening on 127.0.0.1:$RM_PORT for $RM_SECONDS s"
  sleep "$RM_SECONDS"
  kill $cpid $apid 2>/dev/null; pkill -P $cpid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
rm -f "$RM_SOCK"
