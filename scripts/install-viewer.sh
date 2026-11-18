#!/bin/sh
# Put the Linux viewer where the desktop finds it, for this user only: the program (remoteman) in
# ~/.local/bin, its launcher entry and its icon under ~/.local/share. Build it first
# (cargo build --release -p remoteman-viewer). `scripts/install-viewer.sh remove` takes it away.
# RM_PREFIX puts it somewhere else (a package's staging folder: RM_PREFIX=$pkgdir/usr).
root=$(dirname "$(dirname "$(readlink -f "$0")")")
prefix="${RM_PREFIX:-$HOME/.local}"
id=dev.remoteman.Viewer
apps="$prefix/share/applications"
icons="$prefix/share/icons/hicolor"
if [ "$1" = remove ]; then
    rm -f "$prefix/bin/remoteman" "$apps/$id.desktop" "$icons/scalable/apps/$id.svg" "$icons/symbolic/apps/$id-symbolic.svg"
else
    [ -x "$root/target/release/remoteman" ] || { echo "build it first: cargo build --release -p remoteman-viewer" >&2; exit 1; }
    install -Dm755 "$root/target/release/remoteman" "$prefix/bin/remoteman"
    install -Dm644 "$root/packaging/linux/$id.svg" "$icons/scalable/apps/$id.svg"
    install -Dm644 "$root/packaging/linux/$id-symbolic.svg" "$icons/symbolic/apps/$id-symbolic.svg"
    # The launcher names the program by its full path, so it works whatever PATH the desktop has.
    install -Dm644 "$root/packaging/linux/$id.desktop" "$apps/$id.desktop"
    [ -n "$RM_PREFIX" ] || sed -i "s|^Exec=remoteman|Exec=$prefix/bin/remoteman|" "$apps/$id.desktop"
fi
# So the desktop notices without logging out.
[ -n "$RM_PREFIX" ] || { gtk4-update-icon-cache -q -t -f "$icons" 2>/dev/null; update-desktop-database -q "$apps" 2>/dev/null; }
exit 0
