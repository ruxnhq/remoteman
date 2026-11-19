#!/bin/sh
# Put the Linux host where this user's desktop finds it: the host program in
# ~/.local/lib/remoteman, its service (remoteman-host.service) with systemd's user units,
# Remoteman Host (remoteman-tray: the top-bar icon and the window) in ~/.local/bin with its
# launcher entry and icons. Build first:
#   cargo build --release -p remoteman-agent -p remoteman-tray
# Nothing is started or switched on by this: sharing begins when its owner turns it on in
# Remoteman Host, and from then on at sign-in. `scripts/install-host.sh remove` stops sharing
# and takes it all away again (paired devices and settings in ~/.local/state/remoteman stay).
root=$(dirname "$(dirname "$(readlink -f "$0")")")
prefix="$HOME/.local"
units="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
apps="$prefix/share/applications"
icons="$prefix/share/icons/hicolor"
id=dev.remoteman.Host
if [ "$1" = remove ]; then
    systemctl --user disable --now remoteman-host.service 2>/dev/null
    rm -f "$units/remoteman-host.service" "$prefix/bin/remoteman-tray" "$prefix/lib/remoteman/remoteman-agent" "$apps/$id.desktop" "$icons/scalable/apps/$id.svg" "$icons/symbolic/apps/$id-symbolic.svg" "${XDG_CONFIG_HOME:-$HOME/.config}/autostart/$id.desktop"
    for state in connected paused failed; do rm -f "$icons/symbolic/apps/$id-$state-symbolic.svg"; done
else
    for program in remoteman-agent remoteman-tray; do
        [ -x "$root/target/release/$program" ] || { echo "build it first: cargo build --release -p remoteman-agent -p remoteman-tray" >&2; exit 1; }
    done
    # Over a running host's program: the new file takes the name, the old one runs on until
    # the service is restarted.
    install -Dm755 "$root/target/release/remoteman-agent" "$prefix/lib/remoteman/remoteman-agent.new" && mv "$prefix/lib/remoteman/remoteman-agent.new" "$prefix/lib/remoteman/remoteman-agent"
    install -Dm755 "$root/target/release/remoteman-tray" "$prefix/bin/remoteman-tray"
    install -Dm644 "$root/packaging/linux/$id.svg" "$icons/scalable/apps/$id.svg"
    install -Dm644 "$root/packaging/linux/$id-symbolic.svg" "$icons/symbolic/apps/$id-symbolic.svg"
    for state in connected paused failed; do install -Dm644 "$root/packaging/linux/$id-$state-symbolic.svg" "$icons/symbolic/apps/$id-$state-symbolic.svg"; done
    install -Dm644 "$root/packaging/linux/$id.desktop" "$apps/$id.desktop"
    sed -i "s|^Exec=remoteman-tray|Exec=$prefix/bin/remoteman-tray|" "$apps/$id.desktop"
    install -Dm644 "$root/packaging/systemd/user/remoteman-host.service" "$units/remoteman-host.service"
    sed -i "s|^ExecStart=/usr/lib/remoteman/remoteman-agent|ExecStart=$prefix/lib/remoteman/remoteman-agent|" "$units/remoteman-host.service"
fi
systemctl --user daemon-reload
gtk4-update-icon-cache -q -t -f "$icons" 2>/dev/null; update-desktop-database -q "$apps" 2>/dev/null
exit 0
