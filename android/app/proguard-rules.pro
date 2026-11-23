# R8's rules for the release build, beyond the defaults (proguard-android-optimize.txt, which
# already keeps native methods' names).
#
# The Rust side (crates/remoteman-android) finds things by name, which R8 can't see:
# - its exported functions are Java_com_ruxn_remoteman_Core_<method>: Core's class name and its
#   native methods stay as they are;
# - it calls the listener's onStage, onPairing, onStream, onClosed and onSecondFactor by name
#   and signature (JNIEnv::call_method), on whatever object implements Core.Listener.
-keep class com.ruxn.remoteman.Core {
    native <methods>;
}
-keep interface com.ruxn.remoteman.Core$Listener { *; }
-keepclassmembers class * implements com.ruxn.remoteman.Core$Listener {
    public void on*(...);
}

# ML Kit (the phone's code reader) makes its component registrars by reflection, from names in
# its manifest, through their no-argument constructors. Its own rules keep the classes but, in
# R8's full mode, not those constructors: without this, opening the scanner crashes in
# BarcodeScanning.getClient (seen on the 0.1.0 release build, 2026-11-23).
-keep class * implements com.google.firebase.components.ComponentRegistrar {
    <init>();
}
