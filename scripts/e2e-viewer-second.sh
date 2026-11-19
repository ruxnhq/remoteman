#!/bin/sh
# The Linux viewer's second display, with nothing real involved: a host in a throwaway headless
# Mutter with moving content on each of the device's displays, and the viewer in a second
# Mutter. The viewer adds a second display from its Display menu's line: a second window must
# open and show that display's picture at its size, and removing it must close the window and
# take the monitor away on the host. The side chosen for the computer must reach the host.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7796}"
# The viewer already knows this computer, with "A second display goes: Above" chosen for it.
mkdir -p "$RM_LOGS/vstate"
printf '[computer]\nname=test\naddress=127.0.0.1:%s\nsecond-side=above\n' "$RM_PORT" > "$RM_LOGS/vstate/viewer-computers"
dbus-run-session -- mutter --headless --wayland --no-x11 --wayland-display rm-view --virtual-monitor 1920x1080@60 >"$RM_LOGS/mutter-view.log" 2>&1 &
vpid=$!
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"
  monitors() { gdbus call --session --dest org.gnome.Mutter.DisplayConfig --object-path /org/gnome/Mutter/DisplayConfig --method org.gnome.Mutter.DisplayConfig.GetCurrentState 2>&1 | grep -o "Meta-[0-9]*" | sort -u | tr "\n" " "; }
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  # The viewer is in from about 7 s and adds the second display at 10 s; content goes onto it
  # at 13 s; it is looked at around 17 s and removed at 18 s.
  ( sleep 13; monitors >"$RM_LOGS/monitors.txt"; env -u DISPLAY WAYLAND_DISPLAY=rm-e2e GDK_BACKEND=wayland python3 "$RM_ROOT/scripts/animate.py" --monitor Meta-1 --id second >"$RM_LOGS/content.log" 2>&1 ) &
  env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland timeout 45 "$bin/remoteman" --state "$RM_LOGS/vstate" --size 1920x1080 --scale 1 --refresh 60 --ui "wait=1,pair-go=127.0.0.1:$RM_PORT,wait=3,click=Connect,wait=3,second?,second,wait=7,second?,second,wait=2,second?" >"$RM_LOGS/viewer.log" 2>"$RM_LOGS/viewer.err"
  monitors >"$RM_LOGS/monitors-after.txt"
  kill $(jobs -p) 2>/dev/null; kill $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' >/dev/null 2>&1
pkill -P $vpid 2>/dev/null; kill $vpid 2>/dev/null; wait $vpid 2>/dev/null
grep '^~ second' "$RM_LOGS/viewer.log"
echo "host monitors with the second display: $(cat "$RM_LOGS/monitors.txt" 2>/dev/null) | after: $(cat "$RM_LOGS/monitors-after.txt" 2>/dev/null)"
sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep "second display" | cut -c29-140
fail=0
readings=$(grep '^~ second display' "$RM_LOGS/viewer.log")
[ "$(echo "$readings" | sed -n 1p)" = "~ second display: none" ] || { echo "there was a second display before one was asked for"; fail=1; }
shown=$(echo "$readings" | sed -n 2p | sed -n 's/.*a window, \([0-9]*\) pictures shown, 1920 x 1080$/\1/p')
[ "${shown:-0}" -ge 30 ] || { echo "the second window did not show the second display's picture at its size"; fail=1; }
[ "$(echo "$readings" | sed -n 3p)" = "~ second display: none" ] || { echo "the second window was not closed"; fail=1; }
[ "$(cat "$RM_LOGS/monitors.txt" 2>/dev/null)" = "Meta-0 Meta-1 " ] || { echo "the host did not have two monitors for the device"; fail=1; }
# (Taken away while the session went on: the host says so. What the desktop has after the
# viewer has left depends on how far its leaving has got, and scripts/e2e-second-display.sh
# counts the monitors in mid-session.)
sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep -q "has a second display.*side=Above" || { echo "the host was not asked for the side chosen for this computer"; fail=1; }
sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep -q "second display is gone" || { echo "the host did not take the second display away"; fail=1; }
grep -q "Meta-1" "$RM_LOGS/monitors-after.txt" 2>/dev/null && { echo "the second monitor stayed on the host"; fail=1; }
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; grep -v 'Gtk-\|^$\|nearby' "$RM_LOGS/viewer.err" | tail -5; exit 1
