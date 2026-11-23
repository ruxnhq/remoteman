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
