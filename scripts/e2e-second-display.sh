#!/bin/sh
# A second display in one session, with nothing real involved: a throwaway headless Mutter with
# a monitor of its own, the host in it, and the test client as a device that asks for a second
# display beside its first. Each of the device's two displays gets a window that prints the
# clicks it sees. The device must get a second picture with a keyframe; the desktop must have
# three monitors meanwhile and two again afterwards; a click sent for the second display must
# land in the second's window, and one sent for the first in the first's.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7794}"
printf 'wait 2\ndisplay2 1280x720\nwait 7\ntarget 1\nmove 0.5 0.5\nwait 0.4\nclick 0.5 0.5\nwait 1\ntarget 0\nmove 0.25 0.25\nwait 0.4\nclick 0.25 0.25\nwait 1\ndisplay2 off\nwait 2\n' > "$RM_LOGS/steps.txt"
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"
  monitors() { gdbus call --session --dest org.gnome.Mutter.DisplayConfig --object-path /org/gnome/Mutter/DisplayConfig --method org.gnome.Mutter.DisplayConfig.GetCurrentState 2>&1 | grep -o "Meta-[0-9]*" | sort -u | tr "\n" " "; }
  content() { env -u DISPLAY WAYLAND_DISPLAY=rm-two GDK_BACKEND=wayland python3 "$RM_ROOT/scripts/animate.py" --monitor "$1" --id "$2" >"$RM_LOGS/content-$2.log" 2>&1 & }
  mutter --headless --wayland --no-x11 --wayland-display rm-two --virtual-monitor 1600x900@60 >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/client" --seconds 17 --size 1920x1080 --scale 1 --refresh 60 --script "$RM_LOGS/steps.txt" >"$RM_LOGS/client.log" 2>&1 &
  cpid=$!
  # The second display is asked for 2 s in: both are there by 5 s. The clicks come from 9 s.
  sleep 5
  monitors >"$RM_LOGS/monitors.txt"
  content Meta-1 first
  content Meta-2 second
  wait $cpid
  monitors >"$RM_LOGS/monitors-after.txt"
  kill $(jobs -p) 2>/dev/null; kill $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' >/dev/null 2>&1
grep -E "^stream|^second" "$RM_LOGS/client.log" | cut -c1-120
echo "monitors with both displays: $(cat "$RM_LOGS/monitors.txt") | after: $(cat "$RM_LOGS/monitors-after.txt")"
echo "the first display's window:  $(grep -h '^window\|input: click' "$RM_LOGS/content-first.log" | tr '\n' ' ')"
echo "the second display's window: $(grep -h '^window\|input: click' "$RM_LOGS/content-second.log" | tr '\n' ' ')"
fail=0
grep -q "^second display: .* 1280x720" "$RM_LOGS/client.log" || { echo "the device was not told of its second picture"; fail=1; }
grep -Eq "^second display: [0-9]+ frames, [1-9][0-9]* keyframes" "$RM_LOGS/client.log" || { echo "the device got no second picture"; fail=1; }
[ "$(cat "$RM_LOGS/monitors.txt")" = "Meta-0 Meta-1 Meta-2 " ] || { echo "the desktop did not have three monitors"; fail=1; }
[ "$(cat "$RM_LOGS/monitors-after.txt")" = "Meta-0 Meta-1 " ] || { echo "the second monitor was not taken away"; fail=1; }
grep -q "input: click at 640,360" "$RM_LOGS/content-second.log" || { echo "the click for the second display did not land in its window"; fail=1; }
grep -q "input: click at 480,270" "$RM_LOGS/content-first.log" || { echo "the click for the first display did not land in its window"; fail=1; }
[ "$(grep -c 'input: click' "$RM_LOGS/content-first.log")" = 1 ] && [ "$(grep -c 'input: click' "$RM_LOGS/content-second.log")" = 1 ] || { echo "a click landed on the wrong display as well"; fail=1; }
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep -E "WARN|ERROR" | cut -c29-200 | head -5; exit 1
