#!/bin/sh
# Runs bundletool, Google's tool for App Bundles, taken from Google's Maven repository through
# Gradle (the same version AGP uses), so nothing is downloaded by hand. For trying a release
# bundle the way Play will cut it for a device:
#   android/bundletool.sh build-apks --bundle=android/app/build/outputs/bundle/release/app-release.aab \
#       --output=/tmp/remoteman.apks --overwrite --aapt2=$ANDROID_HOME/build-tools/36.0.0/aapt2 \
#       --ks=$HOME/.android/debug.keystore --ks-key-alias=androiddebugkey --ks-pass=pass:android
#   android/bundletool.sh install-apks --apks=/tmp/remoteman.apks --device-id=emulator-5554 --adb=$ANDROID_HOME/platform-tools/adb
set -e
version=1.18.3
here=$(cd "$(dirname "$0")" && pwd)
dir=${XDG_CACHE_HOME:-$HOME/.cache}/remoteman-bundletool-$version
if [ ! -s "$dir/classpath" ]; then
    mkdir -p "$dir"
    echo 'rootProject.name = "bundletool"' > "$dir/settings.gradle.kts"
    cat > "$dir/build.gradle.kts" <<EOF
repositories { google(); mavenCentral() }
val bt by configurations.creating
dependencies {
    bt("com.android.tools.build:bundletool:$version")
    // Its device commands (get-device-spec, install-apks) also need Kotlin's library.
    bt("org.jetbrains.kotlin:kotlin-stdlib:2.4.21")
}
tasks.register("classpath") { val f = bt; doLast { println("CP=" + f.asPath) } }
EOF
    JAVA_HOME=${JAVA_HOME:-/opt/android-studio/jbr} "$here/gradlew" -q -p "$dir" classpath \
        | sed -n 's/^CP=//p' > "$dir/classpath"
fi
java=${JAVA_HOME:-/opt/android-studio/jbr}/bin/java
exec "$java" -cp "$(cat "$dir/classpath")" com.android.tools.build.bundletool.BundleToolMain "$@"
