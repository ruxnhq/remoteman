#!/bin/sh
# A throwaway host for a viewer on another machine or in a VM to connect to: a headless Mutter on
# a private D-Bus session with the agent inside it and moving content, for a while. Leaves the
# live desktop and the everyday host alone.
#   scripts/headless-host.sh [SECONDS]      (default 600)
# RM_PORT (default 7799), RM_LISTEN (default 127.0.0.1, which a QEMU guest reaches as 10.0.2.2),
# RM_AGENT_ARGS, RM_MUTTER_ARGS and RM_LOGS as in e2e.sh. It pairs with whoever comes first.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7799}" RM_LISTEN="${RM_LISTEN:-127.0.0.1}"
export RM_SECONDS="${1:-600}"
mkdir -p "$RM_LOGS"
echo "logs: $RM_LOGS"
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"
  mutter --headless --wayland --no-x11 --wayland-display rm-headless $RM_MUTTER_ARGS >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen $RM_LISTEN:$RM_PORT --pair --state "$RM_LOGS/state" $RM_AGENT_ARGS >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  # The content needs a monitor, and the only one is the client'"'"'s: keep trying until one is in.
  ( while :; do env -u DISPLAY WAYLAND_DISPLAY=rm-headless GDK_BACKEND=wayland python3 "$RM_ROOT/scripts/animate.py" $RM_CONTENT_ARGS >>"$RM_LOGS/content.log" 2>&1; sleep 1; done ) &
  cpid=$!
  echo "listening on $RM_LISTEN:$RM_PORT for $RM_SECONDS s"
  sleep "$RM_SECONDS"
  kill $cpid $apid 2>/dev/null; pkill -P $cpid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
