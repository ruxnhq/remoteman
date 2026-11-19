#!/bin/sh
# The Linux viewer playing the computer's sound, with nothing real involved and nothing heard:
# the host in a throwaway headless Mutter (told to leave the desktop's output alone), the viewer
# in a second one with sound on, playing into a silent output made for the test. A 1 kHz tone is
# played into the device's speaker on the host; what comes out of the viewer is recorded from
# the silent output and must be that tone. The silent output and the two streams are on this
# user's sound system for the seconds the test runs; no default is changed.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7796}"
ffmpeg -v error -y -f lavfi -i "sine=frequency=1000:duration=3" -ac 2 -ar 48000 "$RM_LOGS/tone.wav"
out=$(pactl load-module module-null-sink sink_name=remoteman-test-out sink_properties=device.description=remoteman-test-out) || { echo "SKIP: no silent output could be made (pactl)"; exit 0; }
rm -f "$RM_LOGS/heard.wav"
dbus-run-session -- mutter --headless --wayland --no-x11 --wayland-display rm-view --virtual-monitor 1920x1080@60 >"$RM_LOGS/mutter-view.log" 2>&1 &
vpid=$!
dbus-run-session -- sh -c '
  bin="${RM_BIN:-$RM_ROOT/target/release}"
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  # The viewer is up from about 4 s in: the tone goes into the device speaker at 6 s, and what
  # the viewer plays is recorded from the silent output from 5.5 s for 4.5 s.
  ( sleep 6; exec pw-play --target remoteman-speaker "$RM_LOGS/tone.wav" >"$RM_LOGS/play.log" 2>&1 ) &
  ( sleep 5.5; exec timeout 4.5 pw-record -P stream.capture.sink=true --target remoteman-test-out --rate 48000 --channels 1 --format s16 "$RM_LOGS/heard.wav" >"$RM_LOGS/record.log" 2>&1 ) &
  env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland "$bin/remoteman" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --size 1920x1080 --scale 1 --refresh 60 --sound --sound-to remoteman-test-out --quit-after 11 >"$RM_LOGS/viewer.log" 2>"$RM_LOGS/viewer.err"
  sleep 0.5
  kill $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
pkill -P $vpid 2>/dev/null; kill $vpid 2>/dev/null; wait $vpid 2>/dev/null
pactl unload-module "$out" 2>/dev/null
grep -E '^sound:|^pictures' "$RM_LOGS/viewer.log"
heard=$(python3 - "$RM_LOGS/heard.wav" <<'PY'
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
echo "played by the viewer: $heard"
fail=0
grep -qE '^sound: [0-9]{3,} packets played' "$RM_LOGS/viewer.log" || { echo "the viewer played no sound"; fail=1; }
echo "$heard" | grep -qE 'about (99[0-9]|100[0-9]|101[0-9]) Hz' || { echo "what the viewer played was not the 1 kHz tone"; fail=1; }
left=$(pw-cli ls Node 2>/dev/null | grep -cE 'node.name = "(remoteman-speaker|remoteman-test-out|remoteman)"')
[ "$left" = 0 ] || { echo "$left remoteman sound nodes left behind"; fail=1; }
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; cat "$RM_LOGS/record.log" "$RM_LOGS/play.log" 2>/dev/null | head -5; grep -v 'Gtk-\|^$' "$RM_LOGS/viewer.err" | tail -4; exit 1
