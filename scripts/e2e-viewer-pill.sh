#!/bin/sh
# The Linux viewer's pill, with nothing real involved: a host in a throwaway headless Mutter with
# moving content that prints the input it gets, and the viewer in a second one. While connected
# there is no bar across the window: the session's controls are in a pill floating at the top
# centre of the picture, as wide as its controls, and the picture has the whole window.
#
# First with the bar always shown (light, dark, a menu open, then full screen, where the pill
# hides until it is brought out, then a window made too narrow for it), then with "Show on hover": hidden, brought out, and in full
# screen a click on the pill and one on the picture are put in the way a person would. The host
# must get the second and not the first, at its place on the picture; the pill must go once the
# pointer is below it. Pictures of each are left in RM_SHOTS (default: the logs).
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7798}"
export RM_SHOTS="${RM_SHOTS:-$RM_LOGS}"
mkdir -p "$RM_LOGS/vstate" "$RM_SHOTS"
rm -f "$RM_LOGS/done" "$RM_LOGS/content.log"
dbus-run-session -- sh -c '
  bin="${RM_BIN:-$RM_ROOT/target/release}"
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  ( while :; do env -u DISPLAY WAYLAND_DISPLAY=rm-e2e GDK_BACKEND=wayland python3 "$RM_ROOT/scripts/animate.py" >>"$RM_LOGS/content.log" 2>&1; sleep 1; done ) &
  cpid=$!
  n=0; while [ ! -e "$RM_LOGS/done" ] && [ $n -lt 240 ]; do sleep 0.5; n=$((n + 1)); done
  kill $cpid $apid 2>/dev/null; pkill -P $cpid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' >/dev/null 2>&1 &
hpid=$!
sleep 3
dbus-run-session -- sh -c '
  bin="${RM_BIN:-$RM_ROOT/target/release}"
  mutter --headless --wayland --no-x11 --wayland-display rm-view --virtual-monitor 1920x1080@60 >"$RM_LOGS/mutter-view.log" 2>&1 &
  mpid=$!
  sleep 2
  run() { env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland timeout 50 "$bin/remoteman" --state "$RM_LOGS/vstate" --size 1920x1080 --scale 1 --refresh 60 "$@"; }
  S="$RM_SHOTS"
  # Always shown.
  run --hover-bar no --ui "wait=1,pair-go=127.0.0.1:$RM_PORT,wait=3,click=Connect,wait=4,light,figures,wait=0.5,pill?,place?,shot=$S/pill-always-light.png,dark,wait=0.5,shot=$S/pill-always-dark.png,light,menu=display,wait=0.8,shot=$S/pill-menu.png,menu=none,fullscreen,wait=1.5,pill?,bar,wait=0.8,pill?,place?,shot=$S/pill-fullscreen.png,fullscreen,wait=1,size=560x420,wait=1.5,pill?,shot=$S/pill-narrow.png" >"$RM_LOGS/viewer.log" 2>"$RM_LOGS/viewer.err"
  # Show on hover. Once the pill has said where it is in full screen, the pointer goes to the
  # top edge, clicks on the pill'"'"'s end, and then clicks on the picture well below it.
  ( n=0; until [ "$(grep -c "^~ pill" "$RM_LOGS/viewer-hover.log" 2>/dev/null)" -ge 3 ] || [ $n -ge 250 ]; do sleep 0.2; n=$((n + 1)); done
    at=$(grep "^~ pill" "$RM_LOGS/viewer-hover.log" | sed -n 3p | sed "s/.* at \([0-9]*\),\([0-9]*\) size \([0-9]*\) x \([0-9]*\),.*/\1 \2 \3 \4/")
    set -- $at
    python3 -I "$RM_ROOT/scripts/inject.py" move=-4000,-4000 wait=0.5 move=$(($1 + 4)),$(($2 + $4 / 2)) wait=0.5 click wait=1 move=$((960 - $1 - 4)),$((700 - $2 - $4 / 2)) wait=0.5 click wait=1 >"$RM_LOGS/inject.log" 2>&1 ) &
  run --hover-bar yes --ui "wait=1,connect=127.0.0.1:$RM_PORT,wait=5,pill?,shot=$S/pill-hover-hidden.png,bar,wait=0.8,pill?,shot=$S/pill-hover-out.png,fullscreen,wait=1.5,pill?,wait=6,pill?,place?" >"$RM_LOGS/viewer-hover.log" 2>"$RM_LOGS/viewer-hover.err"
  kill $mpid 2>/dev/null
  wait 2>/dev/null
' >/dev/null 2>&1
touch "$RM_LOGS/done"
wait $hpid
grep -h '^~ \(pill\|window\)' "$RM_LOGS/viewer.log" "$RM_LOGS/viewer-hover.log"
grep "input: click" "$RM_LOGS/content.log"
fail=0
# `~ pill out B, at X,Y size W x H, asks for width N; window WW x WH; picture's place PW x PH at PX,PY; figures F`
check() { # which line, of which run; whether out
  run=$1; n=$2; want=$3; narrow=$4
  line=$(grep "^~ pill" "$RM_LOGS/$run" | sed -n "${n}p")
  [ -n "$line" ] || { echo "no reading $n of $run"; fail=1; return; }
  set -- $(echo "$line" | sed "s/^~ pill out \([a-z]*\), at \(-*[0-9]*\),\(-*[0-9]*\) size \([0-9]*\) x \([0-9]*\), asks for width \([0-9]*\); window \([0-9]*\) x \([0-9]*\); picture.s place \([0-9]*\) x \([0-9]*\) at \(-*[0-9]*\),\(-*[0-9]*\); figures \(.*\)/\1 \2 \3 \4 \5 \6 \7 \8/")
  out=$1; x=$2; y=$3; w=$4; h=$5; asks=$6; ww=$7; wh=$8; rest=$(echo "$line" | sed "s/.*picture.s place \([0-9]*\) x \([0-9]*\) at \(-*[0-9]*\),\(-*[0-9]*\);.*/\1 \2 \3 \4/")
  [ "$out" = "$want" ] || { echo "$run reading $n: the pill was out $out, wanted $want"; fail=1; }
  [ "$rest" = "$ww $wh 0 0" ] || { echo "$run reading $n: the picture does not have the whole window ($rest for $ww x $wh)"; fail=1; }
  [ "$out" = true ] || return
  mid=$((2 * x + w - ww)); [ $mid -ge -2 ] && [ $mid -le 2 ] || { echo "$run reading $n: the pill is not in the middle ($x + $w of $ww)"; fail=1; }
  [ "$y" = 8 ] || { echo "$run reading $n: the pill is $y from the top"; fail=1; }
  [ "$x" -ge 0 ] && [ $((x + w)) -le "$ww" ] || { echo "$run reading $n: the pill ($x + $w) is cut off by the window ($ww)"; fail=1; }
  [ -n "$narrow" ] || [ "$w" = "$asks" ] || { echo "$run reading $n: the pill is $w wide, its controls ask for $asks"; fail=1; }
  [ "$w" -lt "$ww" ] && [ "$h" -ge 30 ] && [ "$h" -le 60 ] || { echo "$run reading $n: the pill is $w x $h"; fail=1; }
  fy=$(echo "$line" | sed -n "s/.*figures Some((\(-*[0-9]*\), \(-*[0-9]*\),.*/\2/p")
  [ -z "$fy" ] || [ "$fy" -ge $((y + h)) ] || { echo "$run reading $n: the figures ($fy) are not below the pill"; fail=1; }
}
check viewer.log 1 true
check viewer.log 2 false
check viewer.log 3 true
# A window made narrower than the pill: the window stays wide enough for it.
check viewer.log 4 true narrow
check viewer-hover.log 1 false
check viewer-hover.log 2 true
check viewer-hover.log 3 true
check viewer-hover.log 4 false
[ "$(grep -c "^~ window.*the middle of the window is (0.5, 0.5) of the picture" "$RM_LOGS/viewer.log" "$RM_LOGS/viewer-hover.log" | awk -F: '{ s += $2 } END { print s }')" = 3 ] || { echo "the middle of the window is not the middle of the picture"; fail=1; }
clicks=$(grep -c "input: click" "$RM_LOGS/content.log")
[ "$clicks" = 1 ] || { echo "the host got $clicks clicks, wanted the one on the picture and not the one on the pill"; fail=1; }
at=$(grep "input: click" "$RM_LOGS/content.log" | head -1 | sed 's/.* at //')
cx=${at%,*}; cy=${at#*,}
[ -n "$at" ] && [ $((cx - 960)) -ge -3 ] && [ $((cx - 960)) -le 3 ] && [ $((cy - 700)) -ge -3 ] && [ $((cy - 700)) -le 3 ] || { echo "the click on the picture reached the host at $at, not 960,700"; fail=1; }
[ $fail = 0 ] && { echo "PASS (pictures in $RM_SHOTS)"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; grep -hv 'Gtk-\|^$\|nearby' "$RM_LOGS/viewer.err" "$RM_LOGS/viewer-hover.err" | tail -5; exit 1
