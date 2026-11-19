#!/bin/sh
# Each paired device belongs to a user, with nothing real involved and no root: remotemand on
# the loopback (run as this user, who is to it what root is to the system's), and two stand-in
# agents (`remotemand test-agent`) that really are two other users to the kernel: each runs in
# a user namespace as one of this user's further users (/etc/subuid), which is what the daemon
# reads off the socket. Their `remotemand ctl` is asked the same way, as the agent in a session
# asks for its owner.
#   alice pairs a device, bob pairs a device, and one is paired from before users were kept.
# Each device must reach its own user's session and never the other's, with the other's agent
# attached and its own gone; each user must see, revoke and switch off only their own; and
# whose a device is must outlive the daemon. Then the agent of a session ending under a
# connected device: the device is taken up by its user's next agent, or dropped.
#   Not covered here (it needs root and GDM): the login screen, and a login on it.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
RM_LOGS="${RM_LOGS:-$(mktemp -d)}"; RM_PORT="${RM_PORT:-7796}"
sock="${XDG_RUNTIME_DIR:-/tmp}/remoteman-e2e-$$.sock"
bin="${RM_BIN:-$root/target/release}"
ALICE=1 BOB=2
# Run something as another user: number N of this user's further users, able to reach this
# user's files (the socket, the programs) and nothing more.
as() { n=$1; shift; unshare --map-users=auto --map-groups=auto --map-user=0 --map-group=0 setpriv --reuid "$n" --regid "$n" --clear-groups --inh-caps +dac_override --ambient-caps +dac_override --bounding-set -all,+dac_override "$@"; }
as $ALICE true 2>/dev/null || { echo "SKIP: this user can't be two more users here (no range in /etc/subuid, or no unshare, newuidmap and setpriv)"; exit 0; }
ctl() { who=$1; shift; as "$who" "$bin/remotemand" ctl --socket "$sock" "$@"; }
admin() { "$bin/remotemand" ctl --socket "$sock" "$@"; }
field() { python3 -I -c "import json,sys; d=json.load(sys.stdin); print($1)" 2>/dev/null; }
daemon() { "$bin/remotemand" --listen 127.0.0.1:$RM_PORT --state "$RM_LOGS/state" --socket "$sock" --user "$(id -un)" "$@" >>"$RM_LOGS/daemon.log" 2>&1 & dpid=$!; sleep 0.7; }
# agent WHO LABEL: its pid is in $apid.
agent() { as "$1" "$bin/remotemand" test-agent --socket "$sock" --label "$2" >"$RM_LOGS/agent-$2.log" 2>&1 & apid=$!; sleep 0.4; }
# How many devices an agent has been given so far.
given() { grep -c ": client testclient " "$RM_LOGS/agent-$1.log"; }
# device NAME [options]: connect as the device NAME; says the first thing that came of it.
device() { name=$1; shift; "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --state "$RM_LOGS/device-$name" --seconds "${SECONDS_:-2}" --size 1280x720 --scale 1 "$@" >"$RM_LOGS/device-$name.log" 2>&1; grep -E "^stream|refused|ended the session|Error" "$RM_LOGS/device-$name.log" | head -1 | cut -c1-160; }
fail=0
want() { [ "$2" = "$3" ] || { echo "not as expected: $1 (got: $2)"; fail=1; }; }
like() { case "$2" in *"$3"*) ;; *) echo "not as expected: $1 (got: $2)"; fail=1 ;; esac; }

# A device from before users were kept: paired with no user said. It is the `--user` user's,
# who has no session here, so it is turned away although two other users' sessions are there.
daemon --pair
agent $ALICE alice; alice=$apid
agent $BOB bob; bob=$apid
like "a device with no user of its own is the --user user's" "$(device old --pair)" "isn't logged in on this computer"

# Alice pairs one (let in at once), bob another (asked about, with a code).
like "alice opens pairing" "$(ctl $ALICE pair on)" "pairing is on"
like "bob can't open pairing over alice's" "$(ctl $BOB pair on)" "another user is pairing"
like "alice's device reaches a session" "$(device alice --pair)" "stream"
want "bob opens pairing" "$(ctl $BOB pair ask)" ok
( device bob --pair >"$RM_LOGS/out-bob" ) &
cpid=$!
sleep 1.2
want "alice sees no device waiting" "$(ctl $ALICE owner | field 'd["pairing"], d["pending"]')" "False None"
want "alice can't accept bob's device" "$(ctl $ALICE pair accept)" "no device is waiting"
code=$(ctl $BOB owner | field 'd["pending"]["code"]')
echo "the code bob is shown for his device: $code"
case "$code" in [0-9][0-9][0-9]\ [0-9][0-9][0-9]) ;; *) echo "bob is shown no code for his waiting device"; fail=1 ;; esac
want "bob accepts it" "$(ctl $BOB pair accept)" ok
wait $cpid
like "bob's device reaches a session" "$(cat "$RM_LOGS/out-bob")" "stream"
want "each device went to its own user's session, once" "$(given alice) $(given bob)" "1 1"

# Who sees what.
afp=$(ctl $ALICE owner | field 'd["devices"][0]["fingerprint"]'); bfp=$(ctl $BOB owner | field 'd["devices"][0]["fingerprint"]')
want "alice has one device, bob one, and they differ" "$(ctl $ALICE owner | field 'len(d["devices"])') $(ctl $BOB owner | field 'len(d["devices"])') $([ -n "$afp" ] && [ "$afp" != "$bfp" ] && echo differ)" "1 1 differ"
want "the daemon's own user sees all three" "$(admin owner | field 'len(d["devices"])')" 3
want "alice's list is hers alone" "$(ctl $ALICE devices | grep -c .)" 1
like "alice can't revoke bob's device" "$(ctl $ALICE revoke "$bfp")" "no trusted device starts with that"
cat "$RM_LOGS/state/device-users" | sed 's/^\(.\{12\}\)[0-9a-f]*/kept: \1…/'

# Preferring or moving across users is refused, even for the daemon's own user.
bobs=$(admin status | awk '/^agent/ { id = $2 } END { sub(":", "", id); print id }')
like "bob's agent is preferred for the next device" "$(admin prefer "$bobs")" "the next client goes to"
( SECONDS_=4 device alice >"$RM_LOGS/out-alice2" ) &
cpid=$!
sleep 1.5
like "alice's connected device can't be moved to bob's session" "$(admin switch "$bobs")" "another user's"
like "bob isn't told the name of alice's device" "$(ctl $BOB status | head -1)" "another user's device"
like "bob can't drop alice's device" "$(ctl $BOB drop)" "no client is connected"
wait $cpid
want "alice's device went to alice again, bob's agent preferred or not" "$(given alice) $(given bob)" "2 1"

# Sharing is each user's own.
want "alice switches her sharing off" "$(ctl $ALICE sharing off)" ok
like "alice's device is turned away" "$(device alice)" "sharing is switched off"
like "bob's device is not" "$(device bob)" "stream"
want "alice switches it on again" "$(ctl $ALICE sharing on)" ok

# Bob's session ends. His device is turned away, and alice's session never hears of it.
kill $bob; sleep 0.5
like "bob's device has nowhere to go" "$(device bob)" "isn't logged in on this computer"
want "nothing more reached alice's session" "$(given alice) $(given bob)" "2 2"

# Whose a device is outlives the daemon.
kill $alice $dpid; wait $alice $dpid 2>/dev/null
daemon
agent $ALICE alice2; alice=$apid
like "after a restart bob's device still isn't alice's" "$(device bob)" "isn't logged in on this computer"
like "and alice's still is" "$(device alice)" "stream"
want "as her agent saw" "$(given alice2)" 1

# The agent of the session ends under a connected device (it crashed, or was restarted), and
# another of the same user's attaches: the device carries on from that one, without
# reconnecting. Bob's agent attaching first must not get it.
( SECONDS_=8 device alice --out "$RM_LOGS/carried.bin" >"$RM_LOGS/out-carried" ) &
cpid=$!
sleep 2; kill $alice
sleep 0.5; agent $BOB bob2; bob=$apid
sleep 0.5; agent $ALICE alice3; alice=$apid
wait $cpid
runs=$(grep -a -oE '^[a-z0-9]+ frame [0-9]+' "$RM_LOGS/carried.bin" | awk '{ if ($1 != who) { printf "%s%s", sep, $1; sep = " "; who = $1 } }')
want "the device was carried from alice's old agent to her new one" "$runs" "alice2 alice3"
want "and never offered to bob's" "$(given bob2)" 0
# With no agent of its user's to take it up, it is dropped rather than left with a dead picture.
( SECONDS_=40 device alice >"$RM_LOGS/out-orphan" ) &
cpid=$!
sleep 2; kill $alice; before=$(date +%s)
wait $cpid
took=$(( $(date +%s) - before ))
like "a device left with no session is told the host ended it" "$(grep -E "ended the session" "$RM_LOGS/device-alice.log")" "ended the session"
[ "$took" -ge 15 ] && [ "$took" -le 30 ] || { echo "not as expected: the device was dropped after $took s (the daemon waits 20 s for another agent)"; fail=1; }
want "bob's agent was never given it" "$(given bob2)" 0

kill $bob $dpid 2>/dev/null; wait 2>/dev/null
rm -f "$sock"
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/daemon.log" | grep -E "WARN|ERROR" | cut -c29-200 | head -8; exit 1
