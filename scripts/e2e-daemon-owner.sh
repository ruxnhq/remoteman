#!/bin/sh
# The owner's side of a host that is the daemon, with nothing real involved: remotemand on the
# loopback (run as this user, no root), one throwaway headless Mutter with an agent attached to
# it, and the owner's requests made at the agent's control socket, as Remoteman Host makes them.
# Pairing must ask and show a code; a declined device must not be kept; an accepted one gets
# its display; sharing switched off must turn a paired device away and on again let it in;
# a revoked device is gone.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7794}"
export RM_SOCK="${XDG_RUNTIME_DIR:-/tmp}/remoteman-e2e-$$.sock" RM_CTL="${XDG_RUNTIME_DIR:-/tmp}/remoteman-e2e-$$-owner.sock"
bin="$root/target/release"
cat > "$RM_LOGS/tell.py" <<'PY'
import socket, sys
s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall((sys.argv[2] + "\n").encode()); s.shutdown(socket.SHUT_WR)
print(s.recv(65536).decode().strip())
PY
tell() { python3 -I "$RM_LOGS/tell.py" "$RM_CTL" "$1"; }
field() { tell status | python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)" 2>/dev/null; }
client() { log="$RM_LOGS/device-$1.log"; shift; "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --state "$RM_LOGS/client" --seconds 3 --size 1280x720 --scale 1 "$@" >"$log" 2>&1; grep -E "^stream|refused|ended the session" "$log" | head -1 | cut -c1-140; }
"$bin/remotemand" --listen 127.0.0.1:$RM_PORT --state "$RM_LOGS/state" --socket "$RM_SOCK" >"$RM_LOGS/daemon.log" 2>&1 &
dpid=$!
dbus-run-session -- sh -c '
  mutter --headless --wayland --no-x11 --wayland-display rm-owner >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$RM_ROOT/target/release/remoteman-agent" --daemon --socket "$RM_SOCK" --control "$RM_CTL" --state "$RM_LOGS/agent-state" --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  echo "$apid $mpid" > "$RM_LOGS/session.pids"
  wait $apid
' >/dev/null 2>&1 &
spid=$!
sleep 4
fail=0
want() { [ "$2" = "$3" ] || { echo "not as expected: $1 (got: $2)"; fail=1; }; }
want "the agent says a daemon is behind it" "$(field 'd["daemon"], len(d["devices"]), d["paused"]')" "True 0 False"
# Declined.
want "pairing opens" "$(tell 'pair on')" ok
( client declined --pair >"$RM_LOGS/out-declined" ) &
cpid=$!
sleep 1.5
code=$(field '(d["pending"] or {}).get("code")')
echo "the code shown for the waiting device: $code"
case "$code" in [0-9][0-9][0-9]\ [0-9][0-9][0-9]) ;; *) echo "no code for the waiting device"; fail=1 ;; esac
want "the decline is taken" "$(tell 'pair decline')" ok
wait $cpid
want "a declined device is not kept" "$(field 'len(d["devices"])')" 0
# Accepted.
tell 'pair on' >/dev/null
( client accepted --pair >"$RM_LOGS/out-accepted" ) &
cpid=$!
sleep 1.5
want "the accept is taken" "$(tell 'pair accept')" ok
wait $cpid
echo "accepted: $(cat "$RM_LOGS/out-accepted")"
grep -q "^stream" "$RM_LOGS/out-accepted" || { echo "an accepted device got no display"; fail=1; }
want "the accepted device is kept" "$(field 'len(d["devices"]), d["devices"][0]["name"]')" "1 testclient"
# Sharing off and on.
want "sharing switches off" "$(tell 'sharing off')" ok
want "the agent says so" "$(field 'd["paused"]')" True
off=$(client paused)
echo "while off: $off"
case "$off" in *"sharing is switched off"*) ;; *) echo "a device was not turned away while sharing was off"; fail=1 ;; esac
want "sharing switches on" "$(tell 'sharing on')" ok
on=$(client back)
case "$on" in stream*) ;; *) echo "a paired device was not let in after sharing came back (got: $on)"; fail=1 ;; esac
# A setting of this session's, and a device revoked.
want "files are allowed" "$(tell 'allow files on')" ok
want "the setting holds" "$(field 'd["files"]')" True
want "the device is revoked" "$(tell "revoke $(field 'd["devices"][0]["fingerprint"]')")" ok
want "nobody is paired any more" "$(field 'len(d["devices"])')" 0
# The agent first (the session's shell ends with it), then its Mutter, then the daemon.
read -r apid mpid < "$RM_LOGS/session.pids"
kill "$apid" 2>/dev/null; sleep 0.5; kill "$mpid" $dpid 2>/dev/null; wait 2>/dev/null
rm -f "$RM_SOCK" "$RM_CTL"
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/daemon.log" | grep -E "WARN|ERROR" | cut -c29-180 | head -5; exit 1
