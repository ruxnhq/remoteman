#!/bin/sh
# The whole-computer install's layout, laid out under a folder (RM_DESTDIR): no root, nothing
# else touched. The payload's lib/ is made 700 with a folder inside it, as an AppImage
# extracted with --appimage-extract leaves it. Passes if the agent, which runs as the session's
# user, could read every library and folder that was copied.
set -eu
root=$(dirname "$(dirname "$(readlink -f "$0")")")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir -p "$tmp/payload/lib/dri" "$tmp/dest"
printf '#!/bin/sh\n' >"$tmp/payload/remotemand"
printf '#!/bin/sh\n' >"$tmp/payload/remoteman-agent"
chmod 755 "$tmp/payload/remotemand" "$tmp/payload/remoteman-agent"
echo library >"$tmp/payload/lib/libavcodec.so.62"
echo library >"$tmp/payload/lib/dri/inside.so"
chmod 600 "$tmp/payload/lib/libavcodec.so.62"
chmod 644 "$tmp/payload/lib/dri/inside.so"
chmod 700 "$tmp/payload/lib/dri" "$tmp/payload/lib"
RM_PAYLOAD="$tmp/payload" RM_UNITS="$root/packaging/systemd" RM_DESTDIR="$tmp/dest" sh "$root/scripts/install-daemon.sh" install >/dev/null
lib="$tmp/dest/usr/local/lib/remoteman/lib"
bad=$(find "$lib" \( -type d ! -perm -o=rx \) -o \( -type f ! -perm -o=r \))
[ -f "$lib/dri/inside.so" ] || { echo "FAIL: lib/ was not copied"; exit 1; }
if [ -n "$bad" ]; then
    echo "FAIL: others can't read:"
    echo "$bad" | sed "s|^$tmp/dest||"
    exit 1
fi
echo "PASS: lib/ and everything in it can be read by the session's user"
