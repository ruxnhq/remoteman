#!/bin/sh
# The Linux viewer's input settings, with nothing real involved: a host in a throwaway headless
# Mutter with a window that prints the input it gets, and the viewer in a second one, set to
# capture the pointer, reverse scrolling and use Caps Lock as Ctrl. Input is put into the
# viewer's Mutter the way a remote-desktop program would (scripts/inject.py, on that Mutter's
# private bus). The viewer must send the pointer on by what the mouse moved, the host must see
# Ctrl for Caps Lock and the wheel reversed, and Ctrl+Alt must let the pointer go. (Touches
# are not covered: Mutter takes injected ones only against a running screen recording.)
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7796}"
mkdir -p "$RM_LOGS/vstate"
printf 'shortcuts=yes\nctrl-key=caps-lock\nscroll-reversed=yes\ncapture-pointer=yes\n' > "$RM_LOGS/vstate/viewer-settings"
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  ( while :; do env -u DISPLAY WAYLAND_DISPLAY=rm-e2e GDK_BACKEND=wayland python3 "$RM_ROOT/scripts/animate.py" >>"$RM_LOGS/content.log" 2>&1; sleep 1; done ) &
  cpid=$!
  sleep 52
  kill $cpid $apid 2>/dev/null; pkill -P $cpid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' >/dev/null 2>&1 &
hpid=$!
sleep 3
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"
  mutter --headless --wayland --no-x11 --wayland-display rm-view --virtual-monitor 1920x1080@60 >"$RM_LOGS/mutter-view.log" 2>&1 &
  mpid=$!
  sleep 2
  # The pointer is put in the middle of the window at 12 s; moved by 300,120 in small steps
  # at 20 s; then Caps Lock, a, and one notch of the wheel; Ctrl+Alt at 31 s. The viewer says
  # how far it has sent the pointer between each (its steps run a few seconds late at most).
  ( sleep 12; python3 -I "$RM_ROOT/scripts/inject.py" move=-4000,-4000 move=960,540 wait=7 moves=60,5,2 wait=0.5 key=58 wait=0.5 key=30 wheel=1 wait=8 down=29 down=56 up=56 up=29 wait=1 >"$RM_LOGS/inject.log" 2>&1 ) &
  env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland timeout 55 "$bin/remoteman" --state "$RM_LOGS/vstate" --size 1920x1080 --scale 1 --refresh 60 --ui "wait=1,pair-go=127.0.0.1:$RM_PORT,wait=3,click=Connect,wait=7,pointer?,wait=8,pointer?,wait=9.5,pointer?,wait=4" >"$RM_LOGS/viewer.log" 2>"$RM_LOGS/viewer.err"
  kill $mpid 2>/dev/null
  wait 2>/dev/null
' >/dev/null 2>&1
wait $hpid
grep '^~ ' "$RM_LOGS/viewer.log"
grep -E "input: (key|scroll|touch|click)" "$RM_LOGS/content.log" | sort | uniq -c
grep "input: pointer" "$RM_LOGS/content.log" | sed -n '1p;$p'
cat "$RM_LOGS/inject.log" 2>/dev/null | head -3
fail=0
# How far the pointer was sent between the first two readings: the 60 steps of 5,2.
sent=$(grep '^~ pointer captured: true, held now: true' "$RM_LOGS/viewer.log" | sed 's/.*sent //' | tr ',\n' '  ' | awk '{ print $3 - $1 "," $4 - $2 }')
echo "sent on while captured: $sent"
[ "$sent" = "300,120" ] || { echo "the pointer was not captured, or not sent by what it moved"; fail=1; }
grep -q '^~ shortcuts released: true' "$RM_LOGS/viewer.log" || { echo "Ctrl+Alt did not release"; fail=1; }
grep -q '^~ pointer captured: false' "$RM_LOGS/viewer.log" || { echo "the pointer was not let go"; fail=1; }
# Ctrl twice: once for Caps Lock, once in Ctrl+Alt.
[ "$(grep -c 'input: key Control_L' "$RM_LOGS/content.log")" = 2 ] || { echo "Caps Lock did not arrive as Ctrl"; fail=1; }
grep -q 'input: key a ' "$RM_LOGS/content.log" || { echo "the a key did not arrive"; fail=1; }
grep -q 'input: scroll 0.0,-' "$RM_LOGS/content.log" || { echo "the wheel did not arrive reversed"; fail=1; }
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; grep -v 'Gtk-\|^$' "$RM_LOGS/viewer.err" | tail -5; exit 1
