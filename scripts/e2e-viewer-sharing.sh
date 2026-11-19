#!/bin/sh
# The Linux viewer's clipboard and files, with nothing real involved: the host in a throwaway
# headless Mutter, allowed to share the clipboard and take files (into a folder of the test's,
# not Downloads), with an ordinary GTK app standing in for the user's programs; the viewer in a
# second one, driven by steps. A line copied in the viewer is pasted by the app on the host; a
# line copied by the app arrives on the viewer's clipboard; a file sent from the viewer arrives
# whole.
root=$(dirname "$(dirname "$(readlink -f "$0")")")
export RM_ROOT="$root" RM_LOGS="${RM_LOGS:-$(mktemp -d)}" RM_PORT="${RM_PORT:-7795}"
mkdir -p "$RM_LOGS/config" "$RM_LOGS/downloads"
printf 'XDG_DOWNLOAD_DIR="%s"\n' "$RM_LOGS/downloads" > "$RM_LOGS/config/user-dirs.dirs"
printf 'copied on the desktop: žluťoučký kůň ✓' > "$RM_LOGS/desktop.txt"
head -c 3000000 /dev/urandom > "$RM_LOGS/remoteman-test-file.bin"
rm -f "$RM_LOGS/pasted.txt"
dbus-run-session -- mutter --headless --wayland --no-x11 --wayland-display rm-view --virtual-monitor 1920x1080@60 >"$RM_LOGS/mutter-view.log" 2>&1 &
vpid=$!
dbus-run-session -- sh -c '
  bin="${RM_BIN:-$RM_ROOT/target/release}"
  mutter --headless --wayland --no-x11 --wayland-display rm-e2e >"$RM_LOGS/mutter.log" 2>&1 &
  mpid=$!
  sleep 2
  XDG_CONFIG_HOME="$RM_LOGS/config" "$bin/remoteman-agent" --listen 127.0.0.1:$RM_PORT --pair --state "$RM_LOGS/state" --allow files,clipboard --keep-output >"$RM_LOGS/agent.log" 2>&1 &
  apid=$!
  sleep 1
  # The viewer is in from about 9 s, shares the clipboard at 12 s and copies its line at 14 s.
  # The app on the host pastes it at 16.5 s and copies its own line at 18 s.
  ( sleep 6.5; ( sleep 9; echo "paste-text $RM_LOGS/pasted.txt"; sleep 1.5; echo "copy-text $RM_LOGS/desktop.txt"; sleep 5; echo quit ) | env -u DISPLAY WAYLAND_DISPLAY=rm-e2e GDK_BACKEND=wayland python3 "$RM_ROOT/crates/remoteman-desktop/examples/clipboard_helper.py" >"$RM_LOGS/app.log" 2>&1 ) &
  cpid=$!
  env -u DISPLAY WAYLAND_DISPLAY=rm-view GDK_BACKEND=wayland timeout 40 "$bin/remoteman" --state "$RM_LOGS/vstate" --size 1920x1080 --scale 1 --refresh 60 --ui "wait=1,pair-go=127.0.0.1:$RM_PORT,wait=3,click=Connect,wait=3,share,wait=1,copy=copied in the viewer: příliš ✓,wait=7,clipboard?,sendfile=$RM_LOGS/remoteman-test-file.bin,wait=3,shot=$RM_LOGS/sharing.png" >"$RM_LOGS/viewer.log" 2>"$RM_LOGS/viewer.err"
  sleep 0.5
  kill $cpid $apid 2>/dev/null; sleep 0.5; kill $mpid 2>/dev/null
  wait 2>/dev/null
' 2>&1 | grep -vE "dbus-daemon|fusermount|Gdk-Message|A connection to the bus|portal|keyring|^$"
pkill -P $vpid 2>/dev/null; kill $vpid 2>/dev/null; wait $vpid 2>/dev/null
fail=0
echo "the app on the host pasted:   $(cat "$RM_LOGS/pasted.txt" 2>/dev/null)"
echo "the viewer's clipboard held:  $(grep '^~ this clipboard holds' "$RM_LOGS/viewer.log" | sed 's/^[^:]*: //')"
[ "$(cat "$RM_LOGS/pasted.txt" 2>/dev/null)" = "copied in the viewer: příliš ✓" ] || { echo "the host did not get what was copied in the viewer"; fail=1; }
grep -q '^~ this clipboard holds: copied on the desktop: žluťoučký kůň ✓' "$RM_LOGS/viewer.log" || { echo "the viewer did not get what was copied on the host"; fail=1; }
if cmp -s "$RM_LOGS/remoteman-test-file.bin" "$RM_LOGS/downloads/remoteman-test-file.bin"; then echo "the file arrived whole: $(stat -c %s "$RM_LOGS/downloads/remoteman-test-file.bin") bytes"; else echo "the file did not arrive whole"; ls -la "$RM_LOGS/downloads"; fail=1; fi
[ $fail = 0 ] && { echo "PASS"; exit 0; }
echo "FAIL (logs in $RM_LOGS)"; sed 's/\x1b\[[0-9;]*m//g' "$RM_LOGS/agent.log" | grep -E 'WARN|ERROR|clipboard|copy|file' | cut -c29-200 | head -10; grep -v 'Gtk-\|^$' "$RM_LOGS/viewer.log" "$RM_LOGS/viewer.err" | tail -6; head -6 "$RM_LOGS/app.log"; exit 1
