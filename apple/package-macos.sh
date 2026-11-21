#!/bin/sh
# Builds remoteman's macOS host and viewer and packs each into a drag-to-Applications disk image,
# with a .sha256 beside it ("<hex>  <file>"), in apple/build/release:
#   remoteman-host-<version>-macos.dmg     remoteman-host.app (the menu-bar host, without the camera)
#   remoteman-viewer-<version>-macos.dmg   remoteman.app (the viewer)
#
#   apple/package-macos.sh              signed ad hoc: opens on another Mac only after
#                                       System Settings > Privacy & Security > Open Anyway
#   SIGN=devid apple/package-macos.sh   Developer ID (Xcode signed in to the team as its Account
#                                       Holder or an Admin allowed to make one), hardened runtime
#   SIGN=devid NOTARIZE=1 apple/package-macos.sh
#                                       also notarized by Apple through the account Xcode is
#                                       signed in to (no password or key needed) and stapled
#
# Apple Silicon only: the core is built for aarch64-apple-darwin (build-core.sh), and the viewer
# uses Float16, which macOS has no Intel version of.
# Run apple/build-core.sh first. The version is MARKETING_VERSION in project.yml.
set -e
cd "$(dirname "$0")"
VERSION=$(awk -F'"' '/MARKETING_VERSION:/ {print $2; exit}' project.yml)
TEAM=$(awk '/DEVELOPMENT_TEAM:/ {print $2; exit}' project.yml)
OUT=build/release
WORK=build/release-work
SIGN=${SIGN:-adhoc}
[ -d build/RemotemanCore.xcframework ] || { echo "run apple/build-core.sh first"; exit 1; }
rm -rf "$OUT" "$WORK"
mkdir -p "$OUT" "$WORK"
xcodegen generate --quiet

# product scheme app
build() {
    scheme=$2; app=$3
    log="$WORK/$1.log"
    if [ "$SIGN" = devid ]; then
        xcodebuild archive -project Remoteman.xcodeproj -scheme "$scheme" -configuration Release \
            -destination 'generic/platform=macOS' -archivePath "$WORK/$1.xcarchive" \
            -derivedDataPath "$WORK/dd" -allowProvisioningUpdates \
            ARCHS=arm64 ONLY_ACTIVE_ARCH=NO ENABLE_HARDENED_RUNTIME=YES >"$log" 2>&1 \
            || { grep -E 'error:' "$log" | sort -u; exit 1; }
        cat >"$WORK/export.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>method</key><string>developer-id</string>
<key>destination</key><string>$([ "$NOTARIZE" = 1 ] && echo upload || echo export)</string>
<key>signingStyle</key><string>automatic</string>
<key>teamID</key><string>$TEAM</string>
</dict></plist>
EOF
        if [ "$NOTARIZE" = 1 ]; then
            # Sent to Apple's notary service with the account Xcode is signed in to; the app
            # comes back notarized with its ticket stapled.
            xcodebuild -exportArchive -archivePath "$WORK/$1.xcarchive" -exportOptionsPlist "$WORK/export.plist" \
                -exportPath "$WORK/$1-upload" -allowProvisioningUpdates >>"$log" 2>&1 \
                || { grep -E 'error' "$log" | sort -u; exit 1; }
            echo "$1: sent for notarization"
            tries=0
            until xcodebuild -exportNotarizedApp -archivePath "$WORK/$1.xcarchive" -exportPath "$WORK/$1" >>"$log" 2>&1; do
                tries=$((tries + 1))
                [ $tries -lt 90 ] || { echo "$1: not notarized after 45 minutes"; tail -5 "$log"; exit 1; }
                sleep 30
            done
            xcrun stapler validate "$WORK/$1/$app"
        else
            xcodebuild -exportArchive -archivePath "$WORK/$1.xcarchive" -exportOptionsPlist "$WORK/export.plist" \
                -exportPath "$WORK/$1" -allowProvisioningUpdates >>"$log" 2>&1 \
                || { grep -E 'error' "$log" | sort -u; exit 1; }
        fi
    else
        xcodebuild build -project Remoteman.xcodeproj -scheme "$scheme" -configuration Release \
            -destination 'generic/platform=macOS' -derivedDataPath "$WORK/dd" \
            ARCHS=arm64 ONLY_ACTIVE_ARCH=NO \
            CODE_SIGN_STYLE=Manual CODE_SIGN_IDENTITY=- DEVELOPMENT_TEAM= >"$log" 2>&1 \
            || { grep -E 'error:' "$log" | sort -u; exit 1; }
        mkdir -p "$WORK/$1"
        ditto "$WORK/dd/Build/Products/Release/$app" "$WORK/$1/$app"
    fi
    codesign --verify --strict --deep "$WORK/$1/$app"
}

# product app volume-name
pack() {
    app="$WORK/$1/$2"
    stage="$WORK/$1-dmg"
    rm -rf "$stage"; mkdir -p "$stage"
    ditto "$app" "$stage/$2"
    ln -s /Applications "$stage/Applications"
    dmg="remoteman-$1-$VERSION-macos.dmg"
    hdiutil create -quiet -volname "$3" -srcfolder "$stage" -fs APFS -format ULFO -ov "$OUT/$dmg"
    if [ "$SIGN" = devid ]; then
        # Xcode's Developer ID certificate may be cloud-managed, with no key in this keychain:
        # the image is then left unsigned, which macOS opens without asking; the app inside is
        # what Gatekeeper checks, and it carries its own stapled ticket.
        id=$(security find-identity -v -p codesigning | awk -F'"' "/Developer ID Application.*$TEAM/ {print \$2; exit}")
        if [ -n "$id" ]; then codesign --sign "$id" --timestamp "$OUT/$dmg"; fi
    fi
    (cd "$OUT" && shasum -a 256 "$dmg" > "$dmg.sha256")
    echo "$OUT/$dmg"
}

build host RemotemanHost remoteman-host.app
build viewer RemotemanMac remoteman.app
pack host remoteman-host.app "remoteman host $VERSION"
pack viewer remoteman.app "remoteman $VERSION"
