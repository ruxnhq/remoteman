#!/bin/sh
# Spike S5: the whole remote login, by hand. GDM gives a headless login screen; remotemand (the
# new system daemon) streams it to a client; the client logs in on it; GDM starts the user's
# session; the daemon moves the client over to it without the client reconnecting.
#
# It needs root: only root (or group gdm) may ask GDM for a remote display, and the agents run
# as other users (GDM's login-screen user, and the test user). Everything it does is to the
# throwaway user "rmtest" and to a daemon of its own on 127.0.0.1:7795. Your own account, your
# session and the remoteman host you use every day are not touched.
#
#   sudo scripts/spike-s5.sh start    make "rmtest" with a random password (kept in a file only
#                                     you and root can read), start the test daemon and a
#                                     watcher, and ask GDM for a login screen
#   sudo scripts/spike-s5.sh login    ask GDM for one more login screen (to test coming back
#                                     to a session that is already running)
#   sudo scripts/spike-s5.sh status   GDM's remote displays, the sessions, the daemon's agents
#   sudo scripts/spike-s5.sh stop     end those sessions, stop everything, delete "rmtest" and
#                                     every file this made (and what spike S4 left behind)
#
# Risk: this talks to the same GDM that owns your running desktop. GDM's own notes list past
# bugs where a bad request brought it down along with every session, so save your work first.
set -eu
U=rmtest
PORT=7795
LIB=/usr/local/libexec/remoteman-s5
RUN=/run/remoteman-s5
STATE=/var/lib/remoteman-s5
SOCK=$RUN/agent.sock
PREFIX=/dev/remoteman/s5
root=$(dirname "$(dirname "$(readlink -f "$0")")")
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
ctl() { "$LIB/remotemand" ctl --socket "$SOCK" "$@" 2>&1; }

# GDM's remote displays that are ours, one a line: "object-path remote-id session-id".
displays() {
    busctl --system --json=short call org.gnome.DisplayManager /org/gnome/DisplayManager/Displays org.freedesktop.DBus.ObjectManager GetManagedObjects 2>/dev/null | python3 -c '
import json, sys
try: objects = json.load(sys.stdin)["data"][0]
except Exception: sys.exit(0)
for path, ifaces in objects.items():
    d = ifaces.get("org.gnome.DisplayManager.RemoteDisplay")
    if d is None: continue
    rid = d.get("RemoteId", {}).get("data", "")
    if rid.startswith(sys.argv[1]): print(path, rid, d.get("SessionId", {}).get("data", "") or "-")
' "$PREFIX"
}

# Start an agent in a user's own systemd, where the session's Mutter is reachable.
agent_as() {
    systemctl --user --machine="$1"@.host is-active --quiet remoteman-s5-agent 2>/dev/null && return 0
    systemd-run --quiet --user --machine="$1"@.host --unit remoteman-s5-agent --collect "$LIB/remoteman-agent" --daemon --socket "$SOCK"
}

# The daemon's number for the agent running as uid $1 (empty if it hasn't attached).
agent_id() { ctl status | sed -n "s/^agent \([0-9]*\): uid $1 .*/\1/p" | tail -1; }

new_login() {
    n=$(( $(displays | wc -l) + 1 ))
    id="$PREFIX/$(date +%s)$n"
    echo "asking GDM for a login screen ($id) ..."
    # GDM 51.1 can answer with an error and still make the display (seen in S4), so the answer
    # isn't trusted: the watcher looks at GDM's list of displays.
    gdbus call --system --dest org.gnome.DisplayManager --object-path /org/gnome/DisplayManager/RemoteDisplayFactory \
        --method org.gnome.DisplayManager.RemoteDisplayFactory.CreateRemoteDisplay \
        "{'remote-id': <objectpath '$id'>, 'hostname': <'127.0.0.1'>}" || echo "(GDM reported an error; the watcher will see whether a display came anyway)"
}

show() {
    echo "GDM's remote displays of this test:"
    displays | while read -r path rid sid; do
        if [ "$sid" = - ]; then echo "  $rid: no session yet"; continue; fi
        echo "  $rid: session $sid, $(loginctl show-session "$sid" -p Name -p Class -p Type -p State -p Remote 2>/dev/null | tr '\n' ' ')"
    done
    echo "sessions of $U: $(loginctl list-sessions --no-legend | awk -v u="$U" '$3 == u {printf "%s(%s) ", $1, $5}')"
    echo "the daemon:"; ctl status | sed 's/^/  /'
}

case "${1:-}" in
start)
    for b in remotemand remoteman-agent; do [ -x "$root/target/release/$b" ] || { echo "build first: cargo build --release"; exit 1; }; done
    # A session left by spike S4 would turn the first login into "coming back": start clean.
    systemctl --user --machine="$U"@.host stop remoteman-s4 2>/dev/null || true
    loginctl terminate-user "$U" 2>/dev/null || true
    id "$U" >/dev/null 2>&1 || useradd --create-home --shell /bin/bash --comment "remoteman test user" "$U"
    install -d -m 755 "$LIB" "$RUN"
    install -m 755 "$root/target/release/remotemand" "$root/target/release/remoteman-agent" "$LIB/"
    # A password for the login screen, never shown: the test client types it from this file.
    umask 077
    pw=$(head -c 24 /dev/urandom | base64 | tr -dc 'a-zA-Z0-9' | cut -c1-16)
    echo "$U:$pw" | chpasswd
    printf '%s\n' "$pw" > "$RUN/password"; unset pw
    chown "${SUDO_UID:-0}" "$RUN/password"
    umask 022
    systemctl stop remoteman-s5-daemon remoteman-s5-watch 2>/dev/null || true
    systemd-run --quiet --unit remoteman-s5-daemon --collect "$LIB/remotemand" --listen 127.0.0.1:$PORT --pair --state "$STATE" --socket "$SOCK"
    systemd-run --quiet --unit remoteman-s5-watch --collect "$(readlink -f "$0")" watch
    sleep 1
    new_login
    sleep 6
    show
    echo "ready: the test daemon listens on 127.0.0.1:$PORT (pairing on); the password file is $RUN/password"
    ;;
login)
    new_login; sleep 6; show
    ;;
watch)
    # Stands in for what the daemon will do itself: follow GDM's displays, put an agent in each
    # new session, and point the daemon at it.
    seen=""
    end=$(( $(date +%s) + 3600 ))
    while [ "$(date +%s)" -lt "$end" ]; do
        displays | while read -r path rid sid; do
            [ "$sid" = - ] && continue
            case " $seen " in *" $sid "*) continue ;; esac
            user=$(loginctl show-session "$sid" -p Name --value 2>/dev/null) || continue
            class=$(loginctl show-session "$sid" -p Class --value 2>/dev/null)
            [ -n "$user" ] || continue
            echo "display $rid has session $sid ($class, user $user)"
            agent_as "$user" || { echo "  couldn't start an agent as $user"; continue; }
            uid=$(id -u "$user"); i=0; aid=""
            while [ -z "$aid" ] && [ $i -lt 40 ]; do sleep 0.5; aid=$(agent_id "$uid"); i=$((i + 1)); done
            [ -n "$aid" ] || { echo "  the agent of $user never attached"; continue; }
            out=$(ctl switch "$aid")
            case "$out" in
                moving*) echo "  moved the connected client to agent $aid ($user)" ;;
                *) ctl prefer "$aid" >/dev/null; echo "  the next client goes to agent $aid ($user) [$out]" ;;
            esac
            echo "$sid" >> "$RUN/seen"
        done
        seen=$(cat "$RUN/seen" 2>/dev/null | tr '\n' ' ')
        sleep 0.5
    done
    ;;
status)
    show
    echo "the watcher's log:"; journalctl -u remoteman-s5-watch -n 12 --no-pager -o cat 2>/dev/null | sed 's/^/  /'
    echo "the daemon's log:"; journalctl -u remoteman-s5-daemon -n 12 --no-pager -o cat 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g; s/^/  /'
    echo "GDM's journal, last lines:"; journalctl -u gdm -n 8 --no-pager -o cat | sed 's/^/  /'
    ;;
stop)
    systemctl stop remoteman-s5-watch 2>/dev/null || true
    displays | while read -r path rid sid; do
        [ "$sid" = - ] && continue
        user=$(loginctl show-session "$sid" -p Name --value 2>/dev/null) || continue
        systemctl --user --machine="$user"@.host stop remoteman-s5-agent 2>/dev/null || true
        echo "ending session $sid ($user)"; loginctl terminate-session "$sid" 2>/dev/null || true
    done
    systemctl --user --machine="$U"@.host stop remoteman-s4 2>/dev/null || true
    loginctl terminate-user "$U" 2>/dev/null || true
    systemctl stop remoteman-s5-daemon 2>/dev/null || true
    sleep 2
    userdel --remove "$U" 2>/dev/null && echo "user $U deleted" || echo "user $U was not there"
    rm -rf "$LIB" "$RUN" "$STATE" /usr/local/libexec/remoteman-agent-s4
    echo "left: $(displays | wc -l) remote displays of this test, $(loginctl list-sessions --no-legend | awk -v u="$U" '$3 == u' | wc -l) sessions of $U"
    ;;
*)
    sed -n '2,22p' "$0" | sed 's/^# \{0,1\}//'
    ;;
esac
