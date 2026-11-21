#!/bin/sh
# Inside the build container (packaging/appimage/build.sh runs it): put the built programs and
# the libraries that travel with them into two folders laid out as AppImages are, and make
# each into one file: the AppImage runtime with the folder, squashed, behind it.
set -eu
src=/src
out=$src/target/appimage
bin=$out/build/release
linux=$src/packaging/linux
version=${RM_VERSION:-0}

# The libraries that go inside: FFmpeg's, and the three it or the programs need that not
# every computer has. Everything else `ldd` names must be on the computer already.
libs() {
    dest=$1; shift
    mkdir -p "$dest"
    for program in "$@"; do
        ldd "$program" | awk '$3 ~ /^\// { print $3 }' | grep -E '^/opt/ffmpeg/|/lib(dav1d|openh264|opus)\.so' | while read -r lib; do
            cp -L "$lib" "$dest/$(basename "$lib")"
        done
    done
    # FFmpeg's own libraries name each other: they find each other beside themselves.
    for lib in "$dest"/*.so*; do patchelf --set-rpath '$ORIGIN' "$lib"; done
}

# The licences of what is inside, in usr/share/licenses/remoteman: remoteman's, FFmpeg's (its
# text, and which build it is and where its source is, kept by the Dockerfile), and for each
# other library its Ubuntu package's copyright file.
licences() {
    dest=$1/usr/share/licenses/remoteman; libdir=$2
    mkdir -p "$dest"
    cp "$src/LICENSE" "$dest/remoteman-LICENSE.txt"
    cp /opt/ffmpeg/share/licenses/ffmpeg/COPYING.LGPLv2.1 "$dest/FFmpeg-LICENSE.txt"
    cp /opt/ffmpeg/share/licenses/ffmpeg/LICENSE.md "$dest/FFmpeg-LICENSE.md"
    {
        cat <<EOF
FFmpeg in remoteman

remoteman's video goes through FFmpeg's libraries, the libav*.so and libsw*.so files packed
with it in this AppImage. They are FFmpeg's, built from its release's source without changes,
under the licence in FFmpeg-LICENSE.txt (GNU Lesser General Public License, version 2.1 or
later; FFmpeg-LICENSE.md is FFmpeg's own note on its licences). Each may be replaced by another
build of FFmpeg with the same names: run the AppImage with --appimage-extract, replace the files
in squashfs-root, and start squashfs-root/AppRun.

Where its source is: https://ffmpeg.org/download.html, and the source of every release and
commit in its git repository, https://git.ffmpeg.org/ffmpeg.git (mirrored at
https://github.com/FFmpeg/FFmpeg).

This build:
EOF
        sed 's/^/  /' /opt/ffmpeg/share/licenses/ffmpeg/build.txt
    } > "$dest/FFmpeg-SOURCE.txt"
    for lib in "$libdir"/*.so*; do
        case $(basename "$lib") in libav* | libsw*) continue ;; esac
        package=$(dpkg -S "$(readlink -f "/usr/lib/x86_64-linux-gnu/$(basename "$lib")")" | cut -d: -f1)
        cp "/usr/share/doc/$package/copyright" "$dest/$package-copyright.txt"
    done
}

pack() {
    dir=$1; name=$2
    # Without the debugging information: it is most of a program's size.
    find "$dir/usr/bin" "$dir/usr/lib" -type f \( -perm -u+x -o -name '*.so*' \) ! -name '*.sh' -exec strip --strip-debug {} +
    rm -f "$out/$name" "$out/$name.sha256"
    mksquashfs "$dir" "$out/$name.squashfs" -root-owned -noappend -comp zstd -quiet -no-progress
    cat /opt/appimage-runtime "$out/$name.squashfs" > "$out/$name"
    rm "$out/$name.squashfs"
    chmod 755 "$out/$name"
    # As sha256sum writes it and `sha256sum -c` reads it: "<hex>  <name>".
    (cd "$out" && sha256sum "$name" > "$name.sha256")
}

# The viewer.
v=$out/Remoteman.AppDir
rm -rf "$v"
install -Dm755 "$bin/remoteman" "$v/usr/bin/remoteman"
libs "$v/usr/lib" "$bin/remoteman"
patchelf --set-rpath '$ORIGIN/../lib' "$v/usr/bin/remoteman"
licences "$v" "$v/usr/lib"
install -Dm644 "$linux/dev.remoteman.Viewer.desktop" "$v/usr/share/applications/dev.remoteman.Viewer.desktop"
install -Dm644 "$linux/dev.remoteman.Viewer.svg" "$v/usr/share/icons/hicolor/scalable/apps/dev.remoteman.Viewer.svg"
install -Dm644 "$linux/dev.remoteman.Viewer-symbolic.svg" "$v/usr/share/icons/hicolor/symbolic/apps/dev.remoteman.Viewer-symbolic.svg"
cp "$linux/dev.remoteman.Viewer.desktop" "$v/"
cp "$linux/dev.remoteman.Viewer.svg" "$v/"
ln -s dev.remoteman.Viewer.svg "$v/.DirIcon"
install -m755 "$src/packaging/appimage/AppRun-viewer" "$v/AppRun"
desktop-file-validate "$v/dev.remoteman.Viewer.desktop"
# The names say they are GNOME's: both are GTK and libadwaita programs, and the host shares a
# GNOME session (through Mutter).
pack "$v" "remoteman-viewer-$version-gnome-x86_64.AppImage"

# The host: Remoteman Host, and under usr/lib/remoteman what the whole-computer install
# copies as it is (the daemon, the agent, their libraries).
h=$out/Remoteman_Host.AppDir
rm -rf "$h"
install -Dm755 "$bin/remoteman-tray" "$h/usr/bin/remoteman-tray"
install -Dm755 "$bin/remoteman-agent" "$h/usr/lib/remoteman/remoteman-agent"
install -Dm755 "$bin/remotemand" "$h/usr/lib/remoteman/remotemand"
libs "$h/usr/lib/remoteman/lib" "$bin/remoteman-agent" "$bin/remoteman-tray"
patchelf --set-rpath '$ORIGIN/lib' "$h/usr/lib/remoteman/remoteman-agent"
licences "$h" "$h/usr/lib/remoteman/lib"
install -Dm755 "$src/scripts/install-daemon.sh" "$h/usr/lib/remoteman/install-daemon.sh"
mkdir -p "$h/usr/share/remoteman"
cp -r "$src/packaging/systemd" "$h/usr/share/remoteman/systemd"
install -Dm644 "$linux/dev.remoteman.Host.desktop" "$h/usr/share/applications/dev.remoteman.Host.desktop"
install -Dm644 "$linux/dev.remoteman.Host.svg" "$h/usr/share/icons/hicolor/scalable/apps/dev.remoteman.Host.svg"
for icon in "" -connected -paused -failed; do install -Dm644 "$linux/dev.remoteman.Host$icon-symbolic.svg" "$h/usr/share/icons/hicolor/symbolic/apps/dev.remoteman.Host$icon-symbolic.svg"; done
cp "$linux/dev.remoteman.Host.desktop" "$h/"
cp "$linux/dev.remoteman.Host.svg" "$h/"
ln -s dev.remoteman.Host.svg "$h/.DirIcon"
install -m755 "$src/packaging/appimage/AppRun-host" "$h/AppRun"
desktop-file-validate "$h/dev.remoteman.Host.desktop"
pack "$h" "remoteman-host-$version-gnome-x86_64.AppImage"
