#!/bin/sh
# Spike S4: can remoteman get a headless GNOME session out of GDM, the way GNOME's own remote
# login does, and stream it?
#
# It needs root, because only root (or group gdm) may ask GDM for a remote display. Everything
# it does is to a throwaway user, "rmtest", never to your own account or session:
#
#   sudo scripts/spike-s4.sh start    make the user, ask GDM for a session for it with no
#                                     password ("preauthenticated"), wait for the session, and
#                                     run the remoteman host inside it on 127.0.0.1:7795
#   sudo scripts/spike-s4.sh status   show the session and the host
#   sudo scripts/spike-s4.sh stop     end the session, delete the user and its files
#
# Risk: this talks to the same GDM that owns your running desktop. GDM's own notes list past
# bugs where a bad request brought it down along with every session, so save your work first.
set -eu
U=rmtest
PORT=7795
BIN=/usr/local/libexec/remoteman-agent-s4
root=$(dirname "$(dirname "$(readlink -f "$0")")")
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }

session_of() { loginctl list-sessions --no-legend | awk -v u="$U" '$3 == u && $5 != "manager" {print $1}' | head -1; }
as_user() { uid=$(id -u "$U"); sudo -u "$U" env XDG_RUNTIME_DIR=/run/user/"$uid" DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/"$uid"/bus "$@"; }

show() {
    s=$(session_of)
    if [ -z "$s" ]; then echo "no session for $U"; return 1; fi
    echo "session $s:"; loginctl show-session "$s" -p Name -p Remote -p Class -p Type -p State -p Service -p Seat | sed 's/^/  /'
    echo "Mutter in it: ScreenCast API version $(as_user gdbus call --session --dest org.gnome.Mutter.ScreenCast --object-path /org/gnome/Mutter/ScreenCast --method org.freedesktop.DBus.Properties.Get org.gnome.Mutter.ScreenCast Version 2>&1 | tr -d '(<>,)')"
    echo "host: $(systemctl --user --machine="$U"@.host is-active remoteman-s4 2>&1)"
}

case "${1:-}" in
start)
    [ -x "$root/target/release/remoteman-agent" ] || { echo "build first: cargo build --release"; exit 1; }
    id "$U" >/dev/null 2>&1 || useradd --create-home --shell /bin/bash --comment "remoteman test user" "$U"
    install -D -m 755 "$root/target/release/remoteman-agent" "$BIN"
    if [ -n "$(session_of)" ]; then
        echo "$U already has a session; using it"
    else
        echo "asking GDM for a headless session for $U ..."
        # GDM 51.1 answers "Display destroyed before being exported" here even when it goes on
        # to make the session (seen 2026-11-17), so the answer isn't trusted: the session is.
        gdbus call --system --dest org.gnome.DisplayManager --object-path /org/gnome/DisplayManager/RemoteDisplayFactory \
            --method org.gnome.DisplayManager.RemoteDisplayFactory.CreateRemoteDisplay \
            "{'remote-id': <objectpath '/dev/remoteman/s4'>, 'preauthenticated-user': <'$U'>}" || echo "(GDM reported an error; checking for the session anyway)"
    fi
    i=0; while [ -z "$(session_of)" ] && [ $i -lt 40 ]; do sleep 0.5; i=$((i + 1)); done
    [ -n "$(session_of)" ] || { echo "GDM made no session for $U within 20 s"; journalctl -u gdm -n 15 --no-pager; exit 1; }
    # Give GNOME Shell a moment to come up before asking it for anything.
    i=0; while ! as_user gdbus call --session --dest org.gnome.Mutter.ScreenCast --object-path /org/gnome/Mutter/ScreenCast --method org.freedesktop.DBus.Peer.Ping >/dev/null 2>&1 && [ $i -lt 40 ]; do sleep 0.5; i=$((i + 1)); done
    systemd-run --quiet --user --machine="$U"@.host --unit remoteman-s4 --collect "$BIN" --listen 127.0.0.1:$PORT --pair --state /home/"$U"/.local/state/remoteman
    sleep 1
    show
    echo "ready: the host in $U's session listens on 127.0.0.1:$PORT (pairing on)"
    ;;
status)
    show
    echo "GDM's journal, last lines:"; journalctl -u gdm -n 8 --no-pager | sed 's/^/  /'
    echo "the host's log, last lines:"; journalctl _UID="$(id -u "$U")" -n 12 --no-pager 2>/dev/null | grep -i remoteman | tail -8 | sed 's/^/  /'
    ;;
stop)
    systemctl --user --machine="$U"@.host stop remoteman-s4 2>/dev/null || true
    loginctl terminate-user "$U" 2>/dev/null || true
    sleep 2
    userdel --remove "$U" 2>/dev/null && echo "user $U deleted" || echo "user $U was not there"
    rm -f "$BIN"
    echo "sessions left for $U: $(loginctl list-sessions --no-legend | awk -v u="$U" '$3 == u' | wc -l)"
    ;;
*)
    sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'
    ;;
esac
