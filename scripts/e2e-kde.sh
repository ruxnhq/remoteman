#!/bin/sh
# The host on KDE Plasma, end to end, with nothing real involved: a throwaway headless KWin
# with KDE's portals (scripts/kde-nested.sh), the agent in it allowed to share the clipboard,
# a window that writes down the input reaching it (scripts/input-log.py), a GTK app standing in
# for the user's programs, and the test client as the device. Checks that the device gets a
# monitor of its own size, at 120 Hz, that its click, keys and text land where it
# put them, and that the clipboard goes both ways. Leaves the live desktop alone.
#   scripts/e2e-kde.sh            (build first: cargo build --release)
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7797}"
mkdir -p "$RM_LOGS"
# The device: copy, then press a key into the app (Wayland takes a copy only from an app with
# input newer than the clipboard's owner's), then click and type. (Touch: the test client's
# --input-demo with scripts/input-log.py; its script has no touch step.)
cat > "$RM_LOGS/steps.txt" <<'EOF'
wait 2.5
clip copied on the device: příliš ✓
wait 4
key 4
wait 6
click 0.9 0.9
text é🙂
wait 3
EOF
printf 'copied on the desktop: žluťoučký kůň ✓' > "$RM_LOGS/desktop.txt"
RM_KDE_AUTHORIZE=1 "$root/scripts/kde-nested.sh" sh -c '
  bin="$RM_ROOT/target/release"
  out=Virtual-virtual-xdp-kde-dev.remoteman.Host
  "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --keep-output --allow clipboard >"$RM_LOGS/agent.log" 2>&1 &
  a=$!
  sleep 1
  "$bin/remoteman-testclient" 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --size 2752x2064 --scale 2 --refresh 120 --seconds 20 --script "$RM_LOGS/steps.txt" >"$RM_LOGS/client.log" 2>&1 &
  c=$!
  sleep 2
  kscreen-doctor -j >"$RM_LOGS/screens.json"
  # The app copies and pastes; it has the focus while it is the newest window.
  ( ( sleep 2; echo "paste-text $RM_LOGS/pasted.txt"; sleep 3; echo "copy-text $RM_LOGS/desktop.txt"; sleep 2; echo quit ) | GDK_BACKEND=wayland python3 "$RM_ROOT/crates/remoteman-desktop/examples/clipboard_helper.py" >"$RM_LOGS/app.log" 2>&1 ) &
  sleep 8
  GDK_BACKEND=wayland python3 "$RM_ROOT/scripts/input-log.py" --monitor $out >"$RM_LOGS/input.log" 2>&1 &
  i=$!
  wait $c
  kill $a $i 2>/dev/null
  wait 2>/dev/null
' >/dev/null 2>&1
fail=0
check() { if eval "$2"; then echo "ok    $1"; else echo "FAIL  $1"; fail=1; fi; }
check "a monitor of the device's size at 120 Hz" "python3 -c \"
import json,sys
o=[o for o in json.load(open('$RM_LOGS/screens.json'))['outputs'] if o['name'].startswith('Virtual-virtual')][0]
m=[m for m in o['modes'] if m['id']==o['currentModeId']][0]
sys.exit(not (m['size']=={'width':2752,'height':2064} and m['refreshRate']>119 and o['scale']==2))\""
check "pictures came" "grep -q 'stream: .*2752x2064 at 120 Hz' '$RM_LOGS/client.log'"
check "the desktop pasted the device's copy" "grep -qx 'copied on the device: příliš ✓' '$RM_LOGS/pasted.txt'"
check "the device got the desktop's copy" "grep -q 'clipboard from the host .*: copied on the desktop: žluťoučký kůň ✓' '$RM_LOGS/client.log'"
check "the click landed where it was put" "grep -q '^press 1 0.900 0.900' '$RM_LOGS/input.log'"
check "text arrived" "grep -q '^key eacute' '$RM_LOGS/input.log' && grep -q '^key U+1F642' '$RM_LOGS/input.log'"
[ $fail = 0 ] && echo "PASS (logs in $RM_LOGS)" || { echo "logs in $RM_LOGS"; sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep -E 'WARN|ERROR' | cut -c29-200; }
exit $fail
