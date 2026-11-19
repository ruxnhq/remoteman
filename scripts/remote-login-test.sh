#!/bin/sh
# Remote login with the daemon doing all of it: remotemand asks GDM for a headless login screen
# when a device connects and its user has no session, the agent starts by itself in that
# session (a user unit enabled for every user), someone logs in on the login screen through the
# device, and the daemon moves the device to the new session and has the login screen's session
# end. Nothing here stands in for the daemon, as the watcher in spike S5 did.
#
# It needs root. Everything is done to the throwaway user "rmtest" and to a daemon of its own
# on 127.0.0.1:7795; your account, your session and the host you use every day are not
# touched. One thing reaches further: the agent's unit is enabled for every user while the test
# is installed, so a session started meanwhile (yours included, if you log out and in) would
# start an agent that attaches to this test daemon and does nothing. `stop` removes it.
#
#   sudo scripts/remote-login-test.sh start        login screen first (the default way in)
#   sudo scripts/remote-login-test.sh start key    no login screen: the device's key alone
#   sudo scripts/remote-login-test.sh status
#   sudo scripts/remote-login-test.sh stop         end the test's sessions, remove everything
#
# Risk: this talks to the same GDM that owns your running desktop; save your work first.
set -eu
U=rmtest
PORT=7795
LIB=/usr/local/libexec/remoteman-test
RUN=/run/remoteman
STATE=/var/lib/remoteman-test
SOCK=$RUN/agent.sock
UNIT=/etc/systemd/user/remoteman-agent.service
WANTS=/etc/systemd/user/gnome-session.target.wants
root=$(dirname "$(dirname "$(readlink -f "$0")")")
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
ctl() { "$LIB/remotemand" ctl --socket "$SOCK" "$@" 2>&1; }

show() {
    echo "sessions:"; loginctl list-sessions --no-legend | awk '$3 == "'"$U"'" || $3 ~ /^gdm-greeter/ {printf "  %s %s %s\n", $1, $3, $6}'
    echo "the daemon:"; ctl status | sed 's/^/  /'
    echo "the daemon's log:"; journalctl -u remoteman-test-daemon -n 14 --no-pager -o cat 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g; s/^/  /'
}

case "${1:-}" in
start)
    how=${2:-screen}
    case "$how" in screen|key) ;; *) echo "start screen|key"; exit 1 ;; esac
    for b in remotemand remoteman-agent; do [ -x "$root/target/release/$b" ] || { echo "build first: cargo build --release"; exit 1; }; done
    [ -e /run/remoteman-s5 ] && { echo "spike S5 is still installed: sudo scripts/spike-s5.sh stop first"; exit 1; }
    systemctl stop remoteman-test-daemon 2>/dev/null || true
    loginctl terminate-user "$U" 2>/dev/null || true
    id "$U" >/dev/null 2>&1 || useradd --create-home --shell /bin/bash --comment "remoteman test user" "$U"
    install -d -m 755 "$LIB" "$RUN" "$WANTS"
    install -m 755 "$root/target/release/remotemand" "$root/target/release/remoteman-agent" "$LIB/"
    # A password for the login screen, never shown: the test client types it from this file.
    umask 077
    pw=$(head -c 24 /dev/urandom | base64 | tr -dc 'a-zA-Z0-9' | cut -c1-16)
    echo "$U:$pw" | chpasswd
    printf '%s\n' "$pw" > "$RUN/password"; unset pw
    chown "${SUDO_UID:-0}" "$RUN/password"
    umask 022
    # The agent, for every graphical session from now on (the package will ship this).
    cat > "$UNIT" <<UNITFILE
[Unit]
Description=remoteman agent (test install): this session's screen for paired devices
PartOf=graphical-session.target
After=graphical-session.target

[Service]
ExecStart=$LIB/remoteman-agent --daemon --socket $SOCK
Restart=on-failure
RestartSec=2

[Install]
WantedBy=gnome-session.target
UNITFILE
    ln -sf "$UNIT" "$WANTS/remoteman-agent.service"
    systemd-run --quiet --unit remoteman-test-daemon --collect "$LIB/remotemand" --listen 127.0.0.1:$PORT --pair --state "$STATE" --socket "$SOCK" --user "$U" --login "$how"
    sleep 1
    show
    echo "ready ($how): connect a client to 127.0.0.1:$PORT; the password file is $RUN/password"
    echo "to follow along without sudo: $LIB/remotemand ctl --socket $SOCK status"
    ;;
status)
    show
    echo "GDM's journal, last lines:"; journalctl -u gdm -n 8 --no-pager -o cat | sed 's/^/  /'
    for u in $(loginctl list-sessions --no-legend | awk '$3 == "'"$U"'" || $3 ~ /^gdm-greeter/ {print $3}' | sort -u); do
        echo "agent of $u: $(systemctl --user --machine="$u"@.host is-active remoteman-agent 2>&1)"
        journalctl _UID="$(id -u "$u")" -n 40 --no-pager -o cat 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g' | grep -i 'remoteman_agent' | tail -6 | sed 's/^/  /'
    done
    ;;
stop)
    systemctl stop remoteman-test-daemon 2>/dev/null || true
    rm -f "$WANTS/remoteman-agent.service" "$UNIT"
    rmdir "$WANTS" 2>/dev/null || true
    # The test's own sessions: the user's, and headless login screens (never one on a seat).
    loginctl list-sessions --no-legend | awk '$3 ~ /^gdm-greeter/ && $4 == "-" {print $1}' | while read -r s; do
        [ "$(loginctl show-session "$s" -p Remote --value 2>/dev/null)" = yes ] || continue
        [ "$(loginctl show-session "$s" -p Class --value 2>/dev/null)" = greeter ] || continue
        echo "ending headless login screen session $s"; loginctl terminate-session "$s" 2>/dev/null || true
    done
    loginctl terminate-user "$U" 2>/dev/null || true
    # A user with a session still closing can't be deleted: wait for its processes to go.
    n=0
    while [ $n -lt 20 ] && pgrep -u "$U" >/dev/null 2>&1; do sleep 0.5; n=$((n + 1)); done
    if ! id "$U" >/dev/null 2>&1; then
        echo "user $U was not there"
    elif userdel --remove "$U" 2>/dev/null; then
        echo "user $U deleted"
    else
        echo "user $U could NOT be deleted (is something of it still running?): run this again, or: userdel --remove $U"
    fi
    rm -rf "$LIB" "$RUN" "$STATE"
    echo "left: $(loginctl list-sessions --no-legend | awk -v u="$U" '$3 == u' | wc -l) sessions of $U"
    ;;
*)
    sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'
    ;;
esac
