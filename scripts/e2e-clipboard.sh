#!/bin/sh
# The clipboard, end to end, with nothing real involved: a throwaway headless Mutter, the agent
# in it allowed to share the clipboard, an ordinary GTK app standing in for the user's programs
# (crates/remoteman-desktop/examples/clipboard_helper.py), and the test client as the device.
# The device copies a line and the app pastes it; the app copies a line and the device gets it.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7796}"
printf 'copied on the desktop: žluťoučký kůň ✓' > "$RM_LOGS/desktop.txt"
printf 'wait 3\nclip copied on the device: příliš ✓\nwait 9\n' > "$RM_LOGS/steps.txt"
rm -f "$RM_LOGS/pasted.txt"
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"
  mutter --headless --wayland --no-x11 --wayland-display rm-clip-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --allow clipboard >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  # The app needs the virtual monitor, which exists once the client is in. It pastes what the
  # device copied (at 3 s), then copies a line of its own.
  ( sleep 2.5; ( sleep 4; echo "paste-text $RM_LOGS/pasted.txt"; sleep 1.5; echo "copy-text $RM_LOGS/desktop.txt"; sleep 3; echo quit ) | env -u DISPLAY WAYLAND_DISPLAY=rm-clip-e2e GDK_BACKEND=wayland python3 "$RM_ROOT/crates/remoteman-desktop/examples/clipboard_helper.py" >"$RM_LOGS/app.log" 2>&1 ) &
  cpid=$!
  "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --seconds 13 --size 1920x1080 --scale 1 --script "$RM_LOGS/steps.txt" >"$RM_LOGS/client.log" 2>&1
  sleep 0.5
  kill $cpid $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
fail=0
echo "the app pasted:  $(cat "$RM_LOGS/pasted.txt" 2>/dev/null)"
echo "the device got:  $(grep 'clipboard from the host' "$RM_LOGS/client.log" | sed 's/^[^}]*}: //')"
[ "$(cat "$RM_LOGS/pasted.txt" 2>/dev/null)" = "copied on the device: příliš ✓" ] || { echo "the app did not paste what the device copied"; fail=1; }
grep -q 'clipboard from the host .*: copied on the desktop: žluťoučký kůň ✓' "$RM_LOGS/client.log" || { echo "the device did not get what the app copied"; fail=1; }
grep -q '^frames [1-9]' "$RM_LOGS/client.log" || { echo "no picture"; fail=1; }
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep -E 'WARN|ERROR|clipboard' | cut -c29-200; cat "$RM_LOGS/app.log" | head -8; exit 1
