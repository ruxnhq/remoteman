#!/bin/sh
# The daemon by itself, with no GPU and no compositor: remotemand on the loopback, two stand-in
# agents (made-up frames that say who sent them), and the screenless test client, which is moved
# from one agent to the other and back while connected. Passes if every frame the client got
# follows the one before it, each agent's run starting at its frame 0.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
bin="$root/target/release"
tmp=$(mktemp -d)
sock="${XDG_RUNTIME_DIR:-/tmp}/remoteman-test-$$.sock"
"$bin/remotemand" --listen 127.0.0.1:7793 --pair --state "$tmp/state" --socket "$sock" >"$tmp/daemon.log" 2>&1 &
dpid=$!
sleep 0.5
"$bin/remotemand" test-agent --socket "$sock" --label first >"$tmp/first.log" 2>&1 &
a1=$!
sleep 0.3
"$bin/remotemand" test-agent --socket "$sock" --label second >"$tmp/second.log" 2>&1 &
a2=$!
sleep 0.3
( sleep 3; "$bin/remotemand" ctl --socket "$sock" switch 1; sleep 2; "$bin/remotemand" ctl --socket "$sock" switch 2 ) >"$tmp/ctl.log" 2>&1 &
"$bin/remoteman-testclient" 127.0.0.1:7793 --pair --state "$tmp/state" --seconds 7 --size 1920x1080 --input-demo --out "$tmp/out.bin" 2>&1 | grep -E "^frames|^interval|^stream|Error"
sleep 0.3
"$bin/remotemand" ctl --socket "$sock" status | head -1
kill $a1 $a2 2>/dev/null; sleep 0.2; kill $dpid 2>/dev/null; wait 2>/dev/null
runs=$(grep -a -oE '^(first|second) frame [0-9]+' "$tmp/out.bin" | awk '{ if ($1 != who || $3 != last + 1) { if (who != "") printf "%s %s-%s, ", who, from, last; who = $1; from = $3 } last = $3 } END { printf "%s %s-%s\n", who, from, last }')
echo "frames that reached the client: $runs"
cat "$tmp/first.log" "$tmp/second.log"
case "$runs" in
    "second 0-"*", first 0-"*", second 0-"*) [ "$(echo "$runs" | tr -cd ',' | wc -c)" = 2 ] && { echo "PASS"; rm -rf "$tmp" "$sock"; exit 0; } ;;
esac
echo "FAIL (logs in $tmp)"; exit 1
