#!/bin/sh
# Make remotemand this computer's host: a system service that owns the port and the paired
# devices, with an agent in every graphical session (the login screen's included), so that a
# paired device reaches the desktop whether its owner is logged in or not: logged in, it gets
# their session; not logged in (after a restart, say), it is shown the login screen and lands
# in the session once someone has logged in on it.
#
# It needs root, and changes how this computer is shared:
# - the host that ran as a user service (remoteman-host.service) is switched off for the user
#   who runs this (the one sudo was called by); its key and its paired devices are copied to
#   /var/lib/remoteman, so every device stays paired and sees the same computer;
# - remotemand starts at boot, before anyone has logged in, and listens on the same port;
# - an agent starts in every session from now on. It runs as that session's user.
#
#   sudo scripts/install-daemon.sh install          the login screen for a device when nobody is logged in
#   sudo scripts/install-daemon.sh install key      no login screen: a paired device is logged in
#                                                   straight away, with no password (the login keyring
#                                                   stays locked)
#   sudo scripts/install-daemon.sh status
#   sudo scripts/install-daemon.sh remove           back to the user service; /var/lib/remoteman is kept
#
# Build first, as yourself: cargo build --release -p remoteman-daemon -p remoteman-agent
#
# The host's AppImage runs this too (packaging/appimage/AppRun-host), with RM_PAYLOAD naming
# the folder that holds the two programs and the libraries that travel with them (in lib/),
# and RM_UNITS the folder of the unit files. RM_DESTDIR, for checking the layout: put the
# files under that folder and touch nothing else (no root, no services).
set -eu
root=$(dirname "$(dirname "$(readlink -f "$0")")")
PAYLOAD=${RM_PAYLOAD:-$root/target/release}
UNITS=${RM_UNITS:-$root/packaging/systemd}
DEST=${RM_DESTDIR:-}
LIB=/usr/local/lib/remoteman
STATE=/var/lib/remoteman
UNIT=$DEST/etc/systemd/system/remotemand.service
AGENT=/etc/systemd/user/remoteman-agent.service
WANTS=/etc/systemd/user/gnome-session.target.wants
# The programs, the libraries that travel with them if there are any, and the two unit files.
place() {
    how=$1
    install -d -m 755 "$DEST$LIB" "$DEST$WANTS" "$(dirname "$UNIT")"
    install -m 755 "$PAYLOAD/remotemand" "$PAYLOAD/remoteman-agent" "$DEST$LIB/"
    if [ -d "$PAYLOAD/lib" ]; then
        rm -rf "$DEST$LIB/lib"
        cp -r "$PAYLOAD/lib" "$DEST$LIB/lib"
    fi
    sed -e "s|^ExecStart=/usr/lib/remoteman/remotemand.*|ExecStart=$LIB/remotemand --user $U --login $how|" "$UNITS/system/remotemand.service" > "$UNIT"
    sed -e "s|=/usr/lib/remoteman/remoteman-agent|=$LIB/remoteman-agent|" "$UNITS/user/remoteman-agent.service" > "$DEST$AGENT"
    ln -sf "$AGENT" "$DEST$WANTS/remoteman-agent.service"
}
if [ -n "$DEST" ]; then
    [ "${1:-}" = install ] || { echo "RM_DESTDIR is for install only"; exit 1; }
    U=${SUDO_USER:-$(id -un)}
    place "${2:-screen}"
    echo "laid out under $DEST; nothing else was touched"
    exit 0
fi
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
# Whose desktop: who called sudo, or who was asked for the password by pkexec.
U=${SUDO_USER:-}
[ -n "$U" ] || [ -z "${PKEXEC_UID:-}" ] || U=$(id -un "$PKEXEC_UID")
[ -n "$U" ] && [ "$U" != root ] || { echo "run with sudo from the account whose desktop is to be shared"; exit 1; }
UID_=$(id -u "$U")
HOME_=$(getent passwd "$U" | cut -d: -f6)
# A command in that user's own systemd, if they are logged in.
as_user() { if [ -d "/run/user/$UID_" ]; then runuser -u "$U" -- env XDG_RUNTIME_DIR="/run/user/$UID_" DBUS_SESSION_BUS_ADDRESS="unix:path=/run/user/$UID_/bus" "$@"; else return 0; fi; }

# The host's port in this computer's firewall: `firewall allow` lets devices on the network
# reach it, `firewall deny` takes that back. ufw and firewalld are the two that distributions
# switch on; with neither running there is nothing to do. A failure is said and not fatal: the
# host still works for whoever opens the port themselves.
PORT=7791
firewall() {
    if command -v ufw >/dev/null 2>&1 && ufw status 2>/dev/null | grep -q "^Status: active"; then
        if [ "$1" = allow ]; then
            ufw allow "$PORT/udp" comment "remoteman host" >/dev/null && echo "the firewall (ufw) lets UDP $PORT in" || echo "couldn't open UDP $PORT in ufw: devices on the network may not reach this computer"
        else
            ufw delete allow "$PORT/udp" >/dev/null 2>&1 || true
        fi
    fi
    if command -v firewall-cmd >/dev/null 2>&1 && firewall-cmd --state >/dev/null 2>&1; then
        if [ "$1" = allow ]; then
            firewall-cmd --quiet --permanent --add-port="$PORT/udp" && firewall-cmd --quiet --reload && echo "the firewall (firewalld) lets UDP $PORT in" || echo "couldn't open UDP $PORT in firewalld: devices on the network may not reach this computer"
        else
            firewall-cmd --quiet --permanent --remove-port="$PORT/udp" 2>/dev/null && firewall-cmd --quiet --reload || true
        fi
    fi
}

show() {
    echo "the service: $(systemctl is-active remotemand.service 2>/dev/null || true), $(systemctl is-enabled remotemand.service 2>/dev/null || true)"
    [ -x "$LIB/remotemand" ] && "$LIB/remotemand" ctl status 2>&1 | sed 's/^/  /'
    echo "$U's agent: $(as_user systemctl --user is-active remoteman-agent.service 2>/dev/null || true); the old user service: $(as_user systemctl --user is-active remoteman-host.service 2>/dev/null || true)"
    journalctl -u remotemand.service -n 6 --no-pager -o cat 2>/dev/null | sed 's/\x1b\[[0-9;]*m//g; s/^/  /'
}

case "${1:-}" in
install)
    how=${2:-screen}
    case "$how" in screen|key) ;; *) echo "install [screen|key]"; exit 1 ;; esac
    for b in remotemand remoteman-agent; do [ -x "$PAYLOAD/$b" ] || { echo "build first, as yourself: cargo build --release -p remoteman-daemon -p remoteman-agent"; exit 1; }; done
    [ -e /run/remoteman/password ] && { echo "the remote-login test is still installed: sudo scripts/remote-login-test.sh stop first"; exit 1; }
    place "$how"
    # The key and the paired devices of the host that ran as the user, the first time only.
    install -d -m 700 "$STATE"
    if [ ! -e "$STATE/host" ] && [ -d "$HOME_/.local/state/remoteman/host" ]; then
        cp -a "$HOME_/.local/state/remoteman/host" "$STATE/host"
        chown -R root:root "$STATE/host"
        echo "copied $U's host key and paired devices to $STATE (the originals stay where they are)"
    fi
    # The two can't both hold the port: the user service goes first.
    as_user systemctl --user disable --now remoteman-host.service 2>/dev/null || true
    systemctl daemon-reload
    # Devices on the network must reach the port, and nobody should have to open a terminal
    # for it afterwards: let it through whichever firewall is switched on here.
    firewall allow
    # Restarted, not just started: installing over a running daemon is how it is updated.
    systemctl enable remotemand.service
    systemctl restart remotemand.service
    as_user systemctl --user daemon-reload
    as_user systemctl --user restart remoteman-agent.service || true
    sleep 2
    show
    echo "installed ($how). Remoteman Host in the top bar goes on working; restart it if it shows nothing."
    ;;
status)
    show
    ;;
remove)
    as_user systemctl --user stop remoteman-agent.service 2>/dev/null || true
    systemctl disable --now remotemand.service 2>/dev/null || true
    firewall deny
    rm -f "$WANTS/remoteman-agent.service" "$AGENT" "$UNIT"
    rmdir "$WANTS" 2>/dev/null || true
    rm -rf "$LIB"
    systemctl daemon-reload
    as_user systemctl --user daemon-reload
    # Back to the host that runs as the user, if it is installed.
    if [ -e "$HOME_/.config/systemd/user/remoteman-host.service" ]; then
        as_user systemctl --user enable --now remoteman-host.service || true
        echo "$U's own host: $(as_user systemctl --user is-active remoteman-host.service 2>/dev/null || true)"
    fi
    echo "removed. $STATE (the daemon's copy of the key and paired devices) is kept; devices paired while the daemon ran are not known to $U's own host."
    ;;
*)
    sed -n '2,24p' "$0" | sed 's/^# \{0,1\}//'
    ;;
esac
