#!/bin/sh
# Finding a host on the network, with nothing real involved: a host in a throwaway headless
# Mutter that listens on this computer alone but advertises itself (under a made-up name, for
# the seconds the test runs: it does appear to other devices on this network, with this
# computer's network cards, and can't be connected to from them), and the viewer in a second
# Mutter. The viewer must list it as nearby before pairing, and after pairing know it as online
# and have its network cards remembered for waking. When the host is gone it must leave the list.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7796}" RM_NAME="remoteman test $$"
systemctl is-active -q avahi-daemon || { echo "SKIP: no Avahi on this computer"; exit 0; }
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --keep-output --advertise --advertise-as "$RM_NAME" >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  # Gone 19 s after the viewer starts.
  sleep 22
  kill $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' >/dev/null 2>&1 &
hpid=$!
sleep 3
dbus-run-session -- sh -c '
  mutter --headless --wayland --no-x11 --wayland-display rm-view --virtual-monitor 1920x1080@60 >"$RM_LOGS/mutter-view.log" 2>&1 &
  mpid=$!
  sleep 2
  env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland timeout 40 "$RM_ROOT/target/release/remoteman" --state "$RM_LOGS/vstate" --ui "wait=3,reach?=127.0.0.1:$RM_PORT,pair-go=127.0.0.1:$RM_PORT,wait=3,back,wait=3,reach?=127.0.0.1:$RM_PORT,wait=10,reach?=127.0.0.1:$RM_PORT" >"$RM_LOGS/viewer.log" 2>"$RM_LOGS/viewer.err"
  kill $mpid 2>/dev/null
  wait 2>/dev/null
' >/dev/null 2>&1
wait $hpid
# (Other hosts on the network, and the card count, are left out of what is printed.)
grep '^~ ' "$RM_LOGS/viewer.log" | grep -E "nearby: .*remoteman test|is " | uniq | sed -E "s/nearby: .*(remoteman test [0-9]+ at port [0-9]+).*/nearby: ... \1 .../"
fail=0
first=$(grep -n "^~ nearby: .*$RM_NAME at port $RM_PORT" "$RM_LOGS/viewer.log" | head -1 | cut -d: -f1)
[ -n "$first" ] || { echo "the viewer did not find the host"; fail=1; }
readings=$(grep "^~ 127.0.0.1:$RM_PORT is " "$RM_LOGS/viewer.log")
[ "$(echo "$readings" | sed -n 1p | cut -d';' -f1)" = "~ 127.0.0.1:$RM_PORT is not known" ] || { echo "an unpaired computer was called something"; fail=1; }
echo "$readings" | sed -n 2p | grep -q "is online; [1-9][0-9]* network cards remembered" || { echo "the paired host was not known as online, or its cards were not remembered"; fail=1; }
echo "$readings" | sed -n 3p | grep -q "is not known; [1-9]" || { echo "the host was still called online after it had gone"; fail=1; }
# Once paired it is no longer among the unpaired.
tail -1 "$RM_LOGS/viewer.log" >/dev/null
grep "^~ nearby:" "$RM_LOGS/viewer.log" | tail -1 | grep -q "$RM_NAME" && { echo "the host was still listed as nearby at the end"; fail=1; }
left=$(avahi-browse -tp _remoteman._udp 2>/dev/null | grep -c "remoteman.032test")
[ "$left" = 0 ] || { echo "the test's advertisement was left on the network"; fail=1; }
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; grep -v 'Gtk-\|^$' "$RM_LOGS/viewer.err" | tail -5; exit 1
