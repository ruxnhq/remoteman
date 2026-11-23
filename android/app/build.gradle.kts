// remoteman for Android and Android TV: the viewer. The connection and the decoder are Rust
// (crates/remoteman-android), built into src/main/jniLibs by ../build-core.sh.
plugins {
    id("com.android.application")
}

android {
    namespace = "com.xnixy.remoteman"
    compileSdk = 37
    // For stripping the Rust library's debug info from the APK.
    ndkVersion = "30.0.16248370"

    defaultConfig {
        applicationId = "com.xnixy.remoteman"
        minSdk = 29
        targetSdk = 37
        versionCode = 1
        versionName = "0.1.0"
        ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }
}
