# remoteman for Android and Android TV

The viewer for phones, tablets and TVs. One app; Kotlin around the shared Rust core.

- `crates/remoteman-android` (Rust, `libremoteman_android.so`): the connection (the shared
  core's client role) and the video decoder. Frames go from the core's thread straight into a
  MediaCodec decoder that draws into the app's `Surface`; the picture never passes through Java.
- `android/app` (Kotlin): the screens, input, and `Core.kt`, the JNI surface.

## Building

Needs Android Studio's SDK (platform 37), the NDK (SDK Manager → SDK Tools → NDK),
`cargo install cargo-ndk`, and `rustup target add aarch64-linux-android x86_64-linux-android`.

```sh
android/build-core.sh                     # the Rust side, into app/src/main/jniLibs
cd android && JAVA_HOME=/opt/android-studio/jbr ./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

Or open `android/` in Android Studio after running `build-core.sh` once.

## Trying it against a throwaway host

`scripts/headless-host.sh` listens on 127.0.0.1:7799, which the emulator reaches as
`10.0.2.2:7799` (the app's default there). It pairs with whoever comes first. The emulator only
decodes in software (dav1d for AV1 10-bit; it has no 10-bit HEVC), so the app asks for 1280×720
at 60 there; real devices ask for their screen's size and refresh rate and use only hardware
decoders.

On this PC (KDE) the GNOME `scripts/headless-host.sh` can't run (no mutter); use `main`'s
`scripts/kde-nested.sh` with an agent on 127.0.0.1:7799 and `scripts/animate.py`, and tick
"Mirror" in the app: an extra display there stays empty, and an unchanging display sends one
frame.

Learned in the first run (2026-11-23):
- Android 17 refuses an app's sockets to local addresses (sendmsg EPERM) without
  `ACCESS_LOCAL_NETWORK`, a runtime permission: the app asks before its first connection.
- The emulator shows 10-bit (P010) decoder output wrongly: the picture appears twice side by
  side in false colours. Its software AV1 decoder also manages only about 37 of 60 frames a
  second at 1080p. Judge the picture and its timing on a real device.

`adb logcat -s remoteman` shows the stream, the decoder chosen, and every 300th picture with
its age (capture on the host to release to the screen).
