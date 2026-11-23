#!/bin/sh
# Cuts the Android app's icons and TV banner, and the pictures Play's listing asks for, from the
# viewer's master icon (assets/icon/viewer.svg), as apple/Support/make-icons.swift does for
# Apple. The SVG is the source; the mark is taken from it as it is, never redrawn here.
#   android/make-icons.sh        (needs rsvg-convert, and the Noto Sans Bold font for the name)
#
# Writes:
#   app/src/main/res/mipmap-*/ic_launcher_foreground.png
#       the adaptive icon's foreground: the white mark on a transparent 108 dp square, scaled
#       so all of it is inside the 66 dp circle no launcher mask cuts into. The background is
#       the master's blue (values/ic_launcher.xml); the monochrome layer for themed icons is the
#       foreground again (the system uses only its alpha).
#   app/src/main/res/drawable-xhdpi/banner.png
#       the 320×180 banner the TV launcher shows: the mark and the app's name.
#   play/icon-512.png, play/tv-banner-1280x720.png, play/feature-graphic-1024x500.png
#       for the Play Console's store listing (not built into the app).
set -e
cd "$(dirname "$0")"
master=../assets/icon/viewer.svg
blue=$(sed -n 's/.*<rect width="1024" height="1024" fill="\(#[0-9A-Fa-f]*\)".*/\1/p' "$master")
# The mark: the master's group (white strokes on its 1024 grid), as it is.
mark=$(sed -n '/<g /,/<\/g>/p' "$master")
[ -n "$blue" ] && [ -n "$mark" ] || { echo "can't read the mark from $master" >&2; exit 1; }
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# The mark reaches 494 units from the master's centre (measured from its pixels); the safe
# circle is 33 of the layer's 108 dp, which is 313 of 1024: a scale of 0.63, about the centre.
scale=0.63
cat > "$tmp/fg.svg" <<EOF
<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024">
<g transform="translate(512 512) scale($scale) translate(-512 -512)">
$mark
</g>
</svg>
EOF

for d in mdpi:108 hdpi:162 xhdpi:216 xxhdpi:324 xxxhdpi:432; do
    dir=app/src/main/res/mipmap-${d%%:*}
    mkdir -p "$dir"
    rsvg-convert -w "${d##*:}" -h "${d##*:}" "$tmp/fg.svg" -o "$dir/ic_launcher_foreground.png"
done

# A wide picture $1×$2: the blue, and the mark with the name beside it, centred together and
# 80% of the width. The name is Noto Sans Bold at 0.42 of the mark's grid size, which makes
# "remoteman" 5.83 of its size wide (measured); the mark's drawn part is x 100..962 of its 1024.
wide() {
    w=$1 h=$2
    set -- $(awk -v w="$w" -v h="$h" 'BEGIN {
        m = 0.8 * w / (862 / 1024 + 0.05 + 5.83 * 0.42); left = 0.1 * w
        printf "%.1f %.1f %.1f %.1f %.1f", m, 0.42 * m, left - m * 100 / 1024, (h - m) / 2, left + m * (862 / 1024 + 0.05)
    }')
    m=$1 t=$2 mx=$3 my=$4 tx=$5
    cat <<EOF
<svg xmlns="http://www.w3.org/2000/svg" width="$w" height="$h" viewBox="0 0 $w $h">
<rect width="$w" height="$h" fill="$blue"/>
<svg x="$mx" y="$my" width="$m" height="$m" viewBox="0 0 1024 1024">
$mark
</svg>
<text x="$tx" y="$(( h / 2 ))" dominant-baseline="central" fill="#fff"
      font-family="Noto Sans" font-weight="700" font-size="$t" letter-spacing="-0.01em">remoteman</text>
</svg>
EOF
}

mkdir -p app/src/main/res/drawable-xhdpi play
wide 320 180 > "$tmp/banner.svg"
rsvg-convert "$tmp/banner.svg" -o app/src/main/res/drawable-xhdpi/banner.png
wide 1280 720 > "$tmp/tv.svg"
rsvg-convert "$tmp/tv.svg" -o play/tv-banner-1280x720.png
wide 1024 500 > "$tmp/feature.svg"
rsvg-convert "$tmp/feature.svg" -o play/feature-graphic-1024x500.png
# Play's icon: the whole square, no transparency; Play rounds it, as Apple does.
rsvg-convert -w 512 -h 512 -b "$blue" "$master" -o play/icon-512.png
echo "icons, banner and Play pictures written"
