# remoteman for Android and Android TV

The viewer for phones, tablets and TVs. One app; Kotlin around the shared Rust core.

- `crates/remoteman-android` (Rust, `libremoteman_android.so`): the connection (the shared
  core's client role) and the video decoder. Frames go from the core's thread straight into a
  MediaCodec decoder that draws into the app's `Surface`; the picture never passes through Java.
- `android/app` (Kotlin): the screens, input, and `Core.kt`, the JNI surface.

## The interface

Native to Android, as the other platforms are to theirs: Jetpack Compose with Material 3 on
phones and tablets (the device's own colours on Android 12+), Compose for TV
(`androidx.tv:tv-material`) on TVs, chosen at start by `android.software.leanback`. One
activity, one model (`AppModel`: a stack of screens, the computers, the settings, the session);
`phone/` and `tv/` draw it.

- **Computers**: the saved computers (`Computers.kt`, in SharedPreferences), newest use first.
  Phone: tap connects, the ⋮ menu has the computer's settings and Forget. TV: a row of cards, OK
  connects, holding OK opens the computer's settings.
- **Adding**: by address (and an optional name), or on a phone by scanning the computer's
  pairing code (CameraX + ML Kit's bundled barcode model; no Play services). A
  `remoteman://pair?…` link opens the app and does the same, on phones and TVs. A code's key is
  checked on the first connection: another computer answering there is turned away.
- **Connecting** is one screen with the picture (it owns the SurfaceView, whose surface the
  decoder needs before connecting starts): the steps, the host's pairing code while its owner
  confirms, the authenticator code if the host asks (`onSecondFactor`), and the end in plain
  words with what to do next: try again, connect again, pair again (the host revoked this
  device), or forget a changed key.
- **Session**: the picture full screen and immersive, fitted to its proportions. Touch is
  touch; a mouse is a mouse; hardware keys go by scan code. Phone: a small handle at the top
  opens the controls (keyboard, statistics, disconnect); Back does too. TV: the remote's Back
  opens a bar (back to the picture, statistics, disconnect) instead of leaving. Back, Home,
  volume and the like never go to the computer.
- **Settings**: where this device's display goes on the computer (mirror, extra, main, only;
  also per computer), the resolution and refresh rate asked for (default: the screen's own;
  1280×720 at 60 on the emulator), statistics.
- **Leaving the app** ends the session (the host sees a clean close); coming back shows
  "Disconnected" with Connect again.
- Not yet (each has a marked place in the session's controls): sound, microphone, camera,
  clipboard, files, finding computers on the network, online state in the list.

Versions: Compose BOM 2026.09.00 (2026.11.01 wants compileSdk 37.1, not installed here),
tv-material 1.1.0, the Compose compiler plugin 2.4.21, which raises AGP 9.5.1's built-in
Kotlin (2.2.10) to the same version.

## Building

Needs Android Studio's SDK (platform 37), the NDK (SDK Manager → SDK Tools → NDK),
`cargo install cargo-ndk`, and `rustup target add aarch64-linux-android x86_64-linux-android`.

```sh
android/build-core.sh                     # the Rust side, into app/src/main/jniLibs
cd android && JAVA_HOME=/opt/android-studio/jbr ./gradlew assembleDebug
adb install -r app/build/outputs/apk/debug/app-debug.apk
```

Or open `android/` in Android Studio after running `build-core.sh` once.

For Google Play (the release bundle, the upload key, the icon and TV banner, and what is done
in Play Console): [RELEASE.md](RELEASE.md).

## Trying it against a throwaway host

`scripts/headless-host.sh` listens on 127.0.0.1:7799, which the emulator reaches as
`10.0.2.2:7799` (the app's default there). It pairs with whoever comes first. The emulator only
decodes in software (dav1d for AV1 10-bit; it has no 10-bit HEVC), so the app asks for 1280×720
at 60 there; real devices ask for their screen's size and refresh rate and use only hardware
decoders.

Pairing with a code (the host waiting for its owner) needs the agent's control socket, since
`--pair` on the command line trusts the first device without asking:

```sh
python3 - <<'PY'     # or socat; "pair on", then "status" shows the code, then "pair accept"
import socket, os
s = socket.socket(socket.AF_UNIX); s.connect(os.environ["XDG_RUNTIME_DIR"] + "/remoteman-agent-7799.sock")
s.sendall(b"pair on\n"); s.shutdown(socket.SHUT_WR); print(s.recv(4096).decode())
PY
```

`revoke FINGERPRINT` there shows the app's "no longer trusts this device", `drop` "ended the
session" (both tried).

On this PC (KDE) the GNOME `scripts/headless-host.sh` can't run (no mutter); use `main`'s
`scripts/kde-nested.sh` with an agent on 127.0.0.1:7799 and `scripts/animate.py`, and keep
the app's placement on "Mirror its screen" (the default): an extra display there stays empty,
and an unchanging display sends one frame.

Learned in the first run (2026-11-23):
- Android 17 refuses an app's sockets to local addresses (sendmsg EPERM) without
  `ACCESS_LOCAL_NETWORK`, a runtime permission: the app asks before its first connection.
- The emulator shows 10-bit (P010) decoder output wrongly: each half of the screen holds the
  whole picture, sharp, in false colours, which is P010 read as 8-bit. It does so with both
  `-gpu swiftshader_indirect` and `-gpu host`, so it's the emulator's display path, not the
  decoder. Judge colours on a real device.
- Rates at 1080p 60 AV1 10-bit, mirrored, software dav1d: phone emulator about 37 fps with
  swiftshader, about 55 with `-gpu host`; Google TV emulator (API 36, `-gpu host`) about 60,
  29 frames dropped in 20 s, 16 to 20 ms from capture to the screen.
- On a TV the address field took focus and the on-screen keyboard covered Connect. Now Connect
  takes focus when an address is there, and the keyboard's own key connects.

TV emulator: `system-images;android-36;google-tv;x86_64`, AVD `Google_TV_1080p` (device
`tv_1080p`). Drive it with `adb shell input keyevent DPAD_DOWN|DPAD_CENTER|BACK`.

`adb logcat -s remoteman` shows the stream, the decoder chosen, and every 300th picture with
its age (capture on the host to release to the screen).

## The interface on the emulators (2026-11-23)

Against `remoteman-agent` in `kde-nested.sh`, mirroring, with pairing confirmed through the
control socket:
- Phone (`Medium_Phone_API_37.0`), by touch: empty list → add by address → the pairing code
  (the same as the host's) → confirmed → the picture with the handle, controls, statistics,
  the on-screen keyboard → Disconnect → the list, with the computer's name and system now
  known. Also: a key the device kept from an earlier host ("forget the key and pair again"),
  a revoked device ("no longer trusts this phone" → Pair again → code → back in), Home and
  back ("Disconnected" → Connect again), a pairing link with the wrong key (turned away), the
  camera screen (the emulator's virtual room; no QR code was read), the local-network
  permission asked afresh (refused: a note on the list; allowed: it connects), the host's
  `drop` ("Someone at the computer ended it"), a tap on the picture reaching it (the controls
  close), the bench build still running to "done".
- TV (`Google_TV_1080p`), by D-pad only: the same from an empty list to the code, the picture,
  Back for the bar, Disconnect, the list; holding OK for a computer's settings and Forget
  (its key file goes too); Settings and a choice list; Home and back; typing an address:
  the D-pad passes over the fields without opening the keyboard, OK opens it, its Go key
  connects (`TvTextField`); Cancel while the code shows and the host accepting after (the late
  connection is closed).
- Not checked: reading a real QR code, the second factor (the agent has no switch for it), text
  arriving on the computer from the on-screen keyboard, a hardware keyboard, mouse or a real
  remote's scan codes, a pairing link on a TV, rotation.
