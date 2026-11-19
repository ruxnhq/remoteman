#!/bin/sh
# Checks the core's host role through its C interface: a host written in C (fakehost.c) against
# the real test client and a viewer written in C (fakeclient.c), on the loopback. Needs `cargo build --release` first.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
tmp=$(mktemp -d)
gcc -O1 -I "$root/crates/remoteman-core/include" "$root/crates/remoteman-core/tests/fakehost.c" "$root/target/release/libremoteman_core.a" -lm -lpthread -ldl -o "$tmp/fakehost" || exit 1
"$tmp/fakehost" 127.0.0.1:7797 "$tmp/host" >"$tmp/host.log" 2>&1 &
hpid=$!
sleep 0.5
"$root/target/release/remoteman-testclient" 127.0.0.1:7797 --pair --state "$tmp/client" --seconds 5 --cursor --input-demo --size 1920x1080 --refresh 120
echo "--- refused when it should be:"
"$root/target/release/remoteman-testclient" 127.0.0.1:7797 --state "$tmp/client" --seconds 2 --size 4096x2160 2>&1 | grep -E "ended|refused|frames"
echo "--- a viewer written in C, with a second display:"
gcc -O1 -I "$root/crates/remoteman-core/include" "$root/crates/remoteman-core/tests/fakeclient.c" "$root/target/release/libremoteman_core.a" -lm -lpthread -ldl -o "$tmp/fakeclient" || exit 1
# With the test client's identity: the host stopped pairing once that had paired.
"$tmp/fakeclient" 127.0.0.1:7797 "$tmp/client"
result=$?
sleep 0.3
kill $hpid 2>/dev/null
echo "--- host said:"
cat "$tmp/host.log"
rm -rf "$tmp"
exit $result
