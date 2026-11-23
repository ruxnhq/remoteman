// remoteman for Android and Android TV: the viewer. The connection and the decoder are Rust
// (crates/remoteman-android), built into src/main/jniLibs by ../build-core.sh.
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
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
        // The benchmark: the app plus recorded streams (android/bench) and a benchmark screen,
        // which Firebase Test Lab runs as a game loop. Kept apart for its size.
        create("bench") {
            initWith(getByName("debug"))
            applicationIdSuffix = ".bench"
            matchingFallbacks += listOf("debug")
        }
    }
    buildFeatures { compose = true }
    sourceSets {
        getByName("bench").assets.directories.add("../bench/assets")
    }
}

dependencies {
    // The screens: Jetpack Compose, Material 3 on phones and tablets, Compose for TV on TVs.
    val compose = platform("androidx.compose:compose-bom:2026.09.00")
    implementation(compose)
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-core:1.7.8")
    implementation("androidx.tv:tv-material:1.1.0")
    // Scanning a computer's pairing code (phones): the camera and ML Kit's barcode reader,
    // whose model is in the app (no Play services needed).
    implementation("androidx.camera:camera-camera2:1.7.0")
    implementation("androidx.camera:camera-lifecycle:1.7.0")
    implementation("androidx.camera:camera-view:1.7.0")
    implementation("androidx.camera:camera-mlkit-vision:1.7.0")
    implementation("com.google.mlkit:barcode-scanning:17.3.0")
}
