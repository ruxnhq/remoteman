#!/bin/sh
# The picture on a link slower than it: remotemand with a stand-in agent (20 kB frames, 120 a
# second, 19.2 Mbit/s) and the screenless test client, through scripts/slowlink.py, which
# passes what the host sends at RATE bit/s through a router's queue of QUEUE bytes. Passes if
# the frames that reach the client were captured less than a second before, at the median.
# Before the host bounded its own queue of datagrams, they fell seconds behind.
# No GPU, no compositor, no root.
#   RM_PORT=7795 (the host; the link listens on the next port)
#   RATE=15000000 QUEUE=262144 SECONDS_RUN=12
root=$(dirname "$(dirname "$(readlink -f "$0")")")
bin="${RM_BIN:-$root/target/release}"
port=${RM_PORT:-7795}
rate=${RATE:-15000000}
queue=${QUEUE:-262144}
secs=${SECONDS_RUN:-12}
tmp=$(mktemp -d)
sock="$tmp/daemon.sock"
"$bin/remotemand" --listen 127.0.0.1:$port --pair --no-advertise --state "$tmp/state" --socket "$sock" >"$tmp/daemon.log" 2>&1 &
dpid=$!
sleep 0.5
"$bin/remotemand" test-agent --socket "$sock" --label slow >"$tmp/agent.log" 2>&1 &
apid=$!
sleep 0.3
python3 -I "$root/scripts/slowlink.py" $((port + 1)) $port "$rate" "$queue" $((secs + 4)) >"$tmp/link.log" 2>&1 &
lpid=$!
sleep 0.3
"$bin/remoteman-testclient" 127.0.0.1:$((port + 1)) --pair --state "$tmp/client" --seconds "$secs" --size 1280x720 >"$tmp/client.log" 2>&1
kill $apid $dpid 2>/dev/null
wait $lpid $apid $dpid 2>/dev/null
grep -E "^frames|^rate|^capture" "$tmp/client.log"
cat "$tmp/link.log"
age=$(grep -o "^capture to received ms: median [0-9.]*" "$tmp/client.log" | grep -o "[0-9.]*$")
if [ -n "$age" ] && awk -v a="$age" 'BEGIN { exit !(a < 1000) }'; then
    echo "PASS (logs in $tmp)"
    exit 0
fi
echo "FAIL: median age ${age:-unknown} ms (logs in $tmp)"
exit 1
