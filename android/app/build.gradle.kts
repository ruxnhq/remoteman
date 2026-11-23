// remoteman for Android and Android TV: the viewer. The connection and the decoder are Rust
// (crates/remoteman-android), built into src/main/jniLibs by ../build-core.sh.
plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
}

// The upload key for Google Play (Play App Signing holds the app's own key; this one only proves
// an upload is ours). Its place and passwords come from ~/.gradle/gradle.properties or the
// environment, never from this repository; see ../RELEASE.md. Without them a release build is
// unsigned (bundletool and the emulator sign with the debug key instead).
fun secret(name: String): String? =
    providers.gradleProperty(name).orNull ?: providers.environmentVariable(name).orNull
val uploadStore = secret("REMOTEMAN_UPLOAD_STORE_FILE")

android {
    namespace = "com.ruxn.remoteman"
    compileSdk = 37
    // For stripping the Rust library's debug info from the APK.
    ndkVersion = "30.0.16248370"

    defaultConfig {
        applicationId = "com.ruxn.remoteman"
        minSdk = 29
        targetSdk = 37
        // The version, in one place: ../gradle.properties. Every upload to Play needs a higher
        // versionCode than the last, even on a testing track.
        versionCode = providers.gradleProperty("remoteman.versionCode").get().toInt()
        versionName = providers.gradleProperty("remoteman.versionName").get()
        ndk { abiFilters += listOf("arm64-v8a", "x86_64") }
    }

    signingConfigs {
        if (uploadStore != null) {
            create("upload") {
                storeFile = file(uploadStore)
                storePassword = secret("REMOTEMAN_UPLOAD_STORE_PASSWORD")
                keyAlias = secret("REMOTEMAN_UPLOAD_KEY_ALIAS") ?: "upload"
                keyPassword = secret("REMOTEMAN_UPLOAD_KEY_PASSWORD") ?: storePassword
            }
        }
    }

    buildTypes {
        // What goes to Play: `./gradlew bundleRelease` makes app/build/outputs/bundle/release/
        // app-release.aab. R8 shrinks the Kotlin (proguard-rules.pro keeps what Rust calls by
        // name); the Rust library goes in stripped, with its symbol table beside it in the
        // bundle for Play's crash reports.
        release {
            isMinifyEnabled = true
            isShrinkResources = true
            proguardFiles(getDefaultProguardFile("proguard-android-optimize.txt"), "proguard-rules.pro")
            ndk { debugSymbolLevel = "SYMBOL_TABLE" }
            if (uploadStore != null) signingConfig = signingConfigs.getByName("upload")
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
