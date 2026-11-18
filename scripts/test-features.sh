#!/bin/sh
# Every feature beside the picture, once, with no GPU and no compositor: remotemand on the
# loopback, a stand-in agent that plays the host's part in a way that can be checked (see
# crates/remoteman-daemon/src/standin.rs), and the test client's script steps. The client is
# moved to a second stand-in half way, to see that what it had switched on carries over.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
bin="$root/target/release"
tmp=$(mktemp -d)
sock="${XDG_RUNTIME_DIR:-/tmp}/remoteman-feat-$$.sock"
head -c 3000000 /dev/urandom > "$tmp/payload.bin"
cat > "$tmp/steps.txt" <<STEPS
wait 1
sound on
wait 1
bitrate 25000000
mic 120
camera 45
clip hello from the device
file $tmp/payload.bin
wait 1
resize 1280x720
wait 4
sound off
wait 0.5
STEPS
"$bin/remotemand" --listen 127.0.0.1:7792 --pair --state "$tmp/state" --socket "$sock" >"$tmp/daemon.log" 2>&1 &
dpid=$!
sleep 0.5
"$bin/remotemand" test-agent --socket "$sock" --label first >"$tmp/first.log" 2>&1 &
a1=$!
sleep 0.3
"$bin/remotemand" test-agent --socket "$sock" --label second >"$tmp/second.log" 2>&1 &
a2=$!
sleep 0.3
# The client starts on "second"; it is moved to "first" while the sound is on.
( sleep 9; "$bin/remotemand" ctl --socket "$sock" switch 1 ) >"$tmp/ctl.log" 2>&1 &
"$bin/remoteman-testclient" 127.0.0.1:7792 --pair --state "$tmp/state" --seconds 14 --size 1920x1080 --script "$tmp/steps.txt" >"$tmp/client.log" 2>&1
sleep 0.3
kill $a1 $a2 2>/dev/null; sleep 0.2; kill $dpid 2>/dev/null; wait 2>/dev/null
grep -vE '^$|^rate|^interval|^capture|^host:|^script' "$tmp/client.log" | cut -c1-200
echo "--- the agents"
cat "$tmp/second.log" "$tmp/first.log" | grep -v '"t":"clipboard"' | cut -c1-200
fail=0
want() { grep -qE "$2" "$tmp/$1" || { echo "MISSING in $1: $2"; fail=1; }; }
want client.log '"t":"sound_format"'
want client.log '"t":"status","bitrate":25000000'
want client.log 'clipboard from the host .*: HELLO FROM THE DEVICE'
sum=$(sed -n 's/^sending payload.bin: 3000000 bytes, sum \(.*\)$/\1/p' "$tmp/client.log")
want client.log "\"t\":\"file_result\",\"id\":[0-9]+,\"ok\":true,\"message\":\"payload.bin: 3000000 bytes, sum $sum\""
want client.log 'stream: .* 1280x720 '
want client.log '^sound: [0-9]+ packets'
[ "$(grep -c '"t":"sound_format"' "$tmp/client.log")" = 2 ] || { echo "expected the sound to start again after the move"; fail=1; }
want second.log '120 microphone packets'
want second.log '45 camera frames \(1 keyframes\)'
want first.log 'client testclient \(1280x720\)'
[ "$(grep -c '^stream:' "$tmp/client.log")" = 3 ] || { echo "expected 3 stream messages (start, resize, move)"; fail=1; }
[ $fail = 0 ] && { echo "PASS"; rm -rf "$tmp" "$sock"; exit 0; }
echo "FAIL (logs in $tmp)"; sed 's/\x1b\[[0-9;]*m//g' "$tmp/daemon.log" | grep -E 'WARN|ERROR' | cut -c29-200; exit 1
