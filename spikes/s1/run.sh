#!/bin/sh
# Spikes S1/S2: throwaway headless Mutter on a private D-Bus session, a GTK window as moving content,
# and capture-spike reading the virtual monitor. Leaves the live desktop alone.
#   ./run.sh [WIDTH HEIGHT FPS SECONDS none|hevc|av1]
here=$(dirname "$(readlink -f "$0")")
export RM_ARGS="${1:-2752} ${2:-2064} ${3:-120} ${4:-10} ${5:-none}" RM_HERE="$here"
exec dbus-run-session -- sh -c '
  mutter --headless --wayland --no-x11 --wayland-display rm-spike >"${RM_MUTTER_LOG:-/dev/null}" 2>&1 &
  mpid=$!
  sleep 2
  fifo=$(mktemp -u); mkfifo "$fifo"
  python3 "$RM_HERE/virtual_monitor.py" >"$fifo" &
  ppid=$!
  read node <"$fifo"; rm -f "$fifo"
  ( sleep 1.5; exec env -u DISPLAY WAYLAND_DISPLAY=rm-spike GDK_BACKEND=wayland python3 "$RM_HERE/animate.py" >"${RM_CUBE_LOG:-/dev/null}" 2>&1 ) &
  cpid=$!
  "$RM_HERE/../capture-spike/target/release/capture-spike" $node $RM_ARGS 2>&1
  rc=$?
  kill $cpid $ppid 2>/dev/null; kill $mpid 2>/dev/null
  wait 2>/dev/null
  exit $rc
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
