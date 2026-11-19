#!/bin/sh
# Sound and microphone, end to end, with nothing real involved: a throwaway headless Mutter, the
# agent in it (told to leave the desktop's output alone), and the test client as the device.
# A tone is played into the device's speaker on the desktop and the device says what it heard;
# the device sends a tone as its microphone and a recording from that microphone is checked.
# The two nodes appear on this user's sound system for the seconds the test runs; nothing is
# played to or recorded from any other device, and no default is changed.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7797}"
ffmpeg -v error -y -f lavfi -i "sine=frequency=1000:duration=2" -ac 2 -ar 48000 "$RM_LOGS/tone.wav"
printf 'wait 2\nsound on\nwait 5\nmic 350\nwait 1\nsound off\n' > "$RM_LOGS/steps.txt"
rm -f "$RM_LOGS/mic.wav"
dbus-run-session -- sh -c '
  bin="${RM_BIN:-$RM_ROOT/target/release}"
  mutter --headless --wayland --no-x11 --wayland-display rm-sound-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  # The speaker exists from 2 s in: play the tone into it, and only into it, at 4 s.
  ( sleep 4; exec pw-play --target remoteman-speaker "$RM_LOGS/tone.wav" >"$RM_LOGS/play.log" 2>&1 ) &
  # The microphone exists from 7 s in: record from it, and only from it, for 2.5 s.
  ( sleep 8; exec timeout 2.5 pw-record --target remoteman-microphone --rate 48000 --channels 1 --format s16 "$RM_LOGS/mic.wav" >"$RM_LOGS/record.log" 2>&1 ) &
  "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --seconds 13 --size 1920x1080 --scale 1 --script "$RM_LOGS/steps.txt" >"$RM_LOGS/client.log" 2>&1
  sleep 0.5
  kill $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
grep -E '^host says|^sound:|^heard:' "$RM_LOGS/client.log"
mic=$(python3 - "$RM_LOGS/mic.wav" <<'PY'
import sys, wave, struct
try:
    w = wave.open(sys.argv[1]); n = w.getnframes(); s = struct.unpack("<%dh" % n, w.readframes(n))
except Exception as e:
    print("no recording:", e); sys.exit()
loud = [x for x in s if abs(x) > 600]
if len(loud) < 4800: print("only silence (%d samples, peak %d)" % (n, max(map(abs, s), default=0))); sys.exit()
first = next(i for i, x in enumerate(s) if abs(x) > 600); last = n - next(i for i, x in enumerate(reversed(s)) if abs(x) > 600)
t = s[first:last]; z = sum(1 for a, b in zip(t, t[1:]) if a < 0 <= b)
print("%.2f s of sound, peak %.2f, about %.0f Hz" % (len(t) / 48000, max(map(abs, t)) / 32768, z * 48000 / len(t)))
PY
)
echo "recorded from the device's microphone: $mic"
fail=0
grep -q '"t":"sound_format","codec":"opus","rate":48000,"channels":2' "$RM_LOGS/client.log" || { echo "no sound format"; fail=1; }
grep -qE '^heard: .* about (99[0-9]|100[0-9]|101[0-9]) Hz' "$RM_LOGS/client.log" || { echo "the device did not hear the 1 kHz tone"; fail=1; }
echo "$mic" | grep -qE 'about (99[0-9]|100[0-9]|101[0-9]) Hz' || { echo "the microphone did not carry the 1 kHz tone"; fail=1; }
left=$(pw-cli ls Node 2>/dev/null | grep -cE 'node.name = "remoteman-(speaker|microphone)"')
[ "$left" = 0 ] || { echo "$left remoteman sound nodes left behind"; fail=1; }
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep -E 'WARN|ERROR|sound|microphone' | cut -c29-200; cat "$RM_LOGS/play.log" "$RM_LOGS/record.log" 2>/dev/null | head -6; exit 1
