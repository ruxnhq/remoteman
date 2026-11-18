#!/bin/sh
# The host's control socket, with nothing real involved: a throwaway headless Mutter, the agent
# in it on its own port (so its socket is not the everyday host's), and the test client as the
# device. While the device is connected with the clipboard shared, the owner's side asks for
# the status, switches the clipboard and files off (the device must be told, the clipboard must
# stop, a file sent afterwards must be refused), and drops the device (it must be told the
# owner ended it). A second connection is revoked (it must be told it is no longer trusted).
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7793}"
export RM_SOCK="${XDG_RUNTIME_DIR:-/tmp}/remoteman-agent-$RM_PORT.sock"
mkdir -p "$RM_LOGS/config" "$RM_LOGS/downloads"
printf 'XDG_DOWNLOAD_DIR="%s"\n' "$RM_LOGS/downloads" > "$RM_LOGS/config/user-dirs.dirs"
head -c 20000 /dev/urandom > "$RM_LOGS/remoteman-test-refused.bin"
cat > "$RM_LOGS/ask.py" <<'PY'
import socket, sys, os
s = socket.socket(socket.AF_UNIX); s.connect(os.environ["RM_SOCK"]); s.sendall((" ".join(sys.argv[1:]) + "\n").encode())
out = b""
while True:
    b = s.recv(65536)
    if not b: break
    out += b
sys.stdout.write(out.decode())
PY
printf 'wait 2\nclip remoteman-test-control\nwait 6\nfile %s\nwait 8\n' "$RM_LOGS/remoteman-test-refused.bin" > "$RM_LOGS/steps.txt"
dbus-run-session -- sh -c '
  bin="$RM_ROOT/target/release"; ask() { python3 "$RM_LOGS/ask.py" "$@"; }
  mutter --headless --wayland --no-x11 --wayland-display rm-control-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  XDG_CONFIG_HOME="$RM_LOGS/config" "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --allow files,clipboard --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --seconds 30 --size 1920x1080 --scale 1 --script "$RM_LOGS/steps.txt" >"$RM_LOGS/client1.log" 2>&1 &
  c1=$!
  sleep 5
  ask status >"$RM_LOGS/status1.json"
  ask allow clipboard off >"$RM_LOGS/answers.txt"; ask allow files off >>"$RM_LOGS/answers.txt"
  sleep 5
  ask status >"$RM_LOGS/status2.json"
  ask drop >>"$RM_LOGS/answers.txt"
  wait $c1
  "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --state "$RM_LOGS/state" --seconds 20 --size 1920x1080 --scale 1 >"$RM_LOGS/client2.log" 2>&1 &
  c2=$!
  sleep 4
  fp=$(python3 -c "import json,sys; print(json.load(open(sys.argv[1]))[\"devices\"][0][\"fingerprint\"])" "$RM_LOGS/status2.json")
  ask revoke "$fp" >>"$RM_LOGS/answers.txt"
  wait $c2
  ask status >"$RM_LOGS/status3.json"
  ask nonsense >>"$RM_LOGS/answers.txt"
  kill $apid 2>/dev/null; sleep 0.7; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
fail=0; no() { echo "$1"; fail=1; }
py() { python3 -c "import json,sys; s=json.load(open(sys.argv[1])); print($2)" "$RM_LOGS/$1" 2>/dev/null; }
echo "status while connected: client $(py status1.json 's["client"]["name"]'), $(py status1.json 's["client"]["width"]')x$(py status1.json 's["client"]["height"]'), $(py status1.json 's["client"]["picture"]'); clipboard $(py status1.json 's["clipboard"]'), files $(py status1.json 's["files"]'); devices $(py status1.json 'len(s["devices"])')"
[ "$(py status1.json 's["client"]["name"]')" = testclient ] || no "the status didn't name the connected device"
[ "$(py status1.json 's["clipboard"], s["files"]')" = "True True" ] || no "the status didn't show clipboard and files allowed"
[ "$(py status2.json 's["clipboard"], s["files"]')" = "False False" ] || no "the switches didn't go off"
grep -q '"t":"features"' "$RM_LOGS/client1.log" && ! grep '"t":"features"' "$RM_LOGS/client1.log" | tail -1 | grep -qE 'clipboard|files' || no "the device wasn't told that clipboard and files are off"
sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep -q 'the clipboard is no longer shared' || no "the clipboard went on being shared"
grep -q '"t":"file_result".*"ok":false' "$RM_LOGS/client1.log" || no "a file sent after files were switched off wasn't refused"
[ -z "$(ls "$RM_LOGS/downloads")" ] || no "the refused file was written all the same"
grep -q 'the host ended the connection' "$RM_LOGS/client1.log" || no "a dropped device wasn't told the owner ended it"
grep -q 'no longer trusted' "$RM_LOGS/client2.log" || no "a revoked device wasn't told it is no longer trusted"
[ "$(py status3.json 'len(s["devices"]), s["client"]')" = "0 None" ] || no "after the revoke the status still lists the device or a client"
grep -q 'files=off' "$RM_LOGS/state/host-settings" && grep -q 'clipboard=off' "$RM_LOGS/state/host-settings" || no "the switches weren't remembered"
[ ! -e "$RM_SOCK" ] || no "the control socket was left behind"
echo "answers: $(tr '\n' ';' < "$RM_LOGS/answers.txt" | cut -c1-200)"
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; grep -E 'host says|Error|closed|ended' "$RM_LOGS/client1.log" "$RM_LOGS/client2.log" | cut -c1-200 | tail -8; exit 1
