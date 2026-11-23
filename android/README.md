# remoteman for Android and Android TV

The viewer for phones, tablets and TVs. One app; Kotlin around the shared Rust core.

- `crates/remoteman-android` (Rust, `libremoteman_android.so`): the connection (the shared
  core's client role), the video decoder and the sound. Frames go from the core's thread straight
  into a MediaCodec decoder that draws into the app's `Surface`; the picture never passes through
  Java, nor does the sound (Opus packets into a jitter buffer that an AAudio stream pulls from).
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
  touch; a mouse is a mouse; hardware keys go by scan code; game controllers are the
  computer's controllers (below). Phone: a small handle at the top opens the controls (keyboard,
  sound, statistics, disconnect); Back does too. TV: the remote's Back opens a bar (back to the
  picture, sound, statistics, disconnect) instead of leaving. Back, Home, volume and the like
  never go to the computer; the volume keys set the volume the computer's sound plays at.
- **Sound** (when the computer offers it, on unless turned off in Settings or the session's
  controls): see "Sound" below.
- **Settings**: where this device's display goes on the computer (mirror, extra, main, only;
  also per computer), the resolution and refresh rate asked for (default: the screen's own;
  1280×720 at 60 on the emulator), statistics, the computer's sound.
- **Leaving the app** ends the session (the host sees a clean close); coming back shows
  "Disconnected" with Connect again.
- Not yet (each has a marked place in the session's controls): microphone, camera,
  clipboard, files, finding computers on the network, online state in the list; rumble back
  to a controller (not in the protocol yet).

## Sound

`crates/remoteman-android/src/sound.rs`. The app asks for the sound (`Message::Sound`) only of
a computer that lists `sound`. Packets go from the core's thread into `remoteman-audio`'s
`JitterBuffer` (the one the Linux and Windows viewers use), and an AAudio output stream
(48 kHz float, stereo as the host's `sound_format` says; exclusive and low-latency where the
device has them, usage Game) pulls from it in its data callback, which decodes a frame when its
turn comes. So nothing of the sound crosses JNI but the switch and the figures.

- **The playout buffer** holds 3 frames of 10 ms (30 ms) before it plays, and grows a frame at a
  time (up to 80 ms) when packets keep arriving just after their turn. A lost packet (a hole in
  `seq`) is made up with Opus's concealment; after the sender pauses, two made-up frames fade
  out, then silence, and it starts again at its depth with the next packet. Too much held
  (a burst after a stall) is dropped down to its depth.
- **AAudio's own buffer** is set to two bursts. Underruns are counted, not chased.
- **Sound focus**: the app asks for it as a game does. A call or another app taking it silences
  the computer's sound (packets still go through the buffer, so it comes back in time); coming
  back to the app asks again.
- **Another output** (headphones, a Bluetooth headset coming or going) ends the AAudio stream
  with "disconnected"; a new one is opened on the new output.
- **Bluetooth** adds its own delay (often 100 to 200 ms with A2DP), which no buffer here can take
  away. The statistics show it: "out" is AAudio's measure from handing a sample over to it
  being heard, from the stream's timestamps.
- The statistics line (Settings → Show statistics) has `sound: buffer · out · concealed ·
  late · underruns`; every 5 s logcat has it with the depth, the loudest sample played and
  what AAudio granted.

libopus is built from source for Android (`opusic-sys`'s bundled copy, with cmake and the NDK's
toolchain file from `ANDROID_NDK_HOME`, which `build-core.sh` sets): it needs `cmake`.

## Game controllers

`Gamepads.kt`. A controller (`SOURCE_GAMEPAD` or `SOURCE_JOYSTICK`) is an Xbox-style controller
on the computer, when the computer lists `gamepad` (docs/PROTOCOL.md, "Game controllers"; the
Windows host has it, the Linux and Mac hosts don't yet). Up to four, numbered in the order
they're first used.

- Buttons: A, B, X, Y, L1/R1 as shoulders, THUMBL/THUMBR, Start, Select as Back/View, Mode as
  Guide; the D-pad from the hat (`AXIS_HAT_X/Y`) or from D-pad keys of the controller.
- Sticks: X/Y left; Z/RZ right (RX/RY on controllers that have no Z); the device's own dead zone
  (`MotionRange.flat`, at least 0.05). Triggers: `LTRIGGER/RTRIGGER` or `BRAKE/GAS`, whichever
  the device has; L2/R2 keys as full triggers on controllers without analog ones.
- Each change goes at once with the whole state; it's repeated every 100 ms while anything is
  off neutral. A controller unplugged is unplugged on the computer.
- Back stays this device's, a controller's own Back key too: on a TV the remote's Back opens the
  bar while a controller is in use. With only a controller in hand, **Start and Select
  together** open it. While the bar or the controls are up, the controllers held go back to
  neutral on the computer, and a controller moves the focus as usual (A is OK, B is Back).
- A computer without `gamepad` gets nothing of controllers; their buttons do what Android
  does with them (B is Back, so it opens the bar).

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

## Testing sound and controllers

The Linux agent sends sound but takes no controllers, so `crates/remoteman-testclient` has a
host for this, `padhost`: it lists `sound` and `gamepad`, plays a 1 kHz tone (Opus, paced by
the clock) when asked for sound, prints every controller state, and pairs with the first device.

```sh
cargo run -p remoteman-testclient --bin padhost -- --listen 127.0.0.1:7798   # app: 10.0.2.2:7798
#   --drop 50       leave out every 50th packet (the app conceals it)
#   --pause 1:5     no sound for 1 s in every 5, as a quiet desktop
```

Controllers on an emulator: `adb shell input gamepad keyevent BUTTON_A` (buttons only), or a
virtual Xbox 360 pad with sticks, triggers and hat through the system's `uinput` tool, which
the shell user may run (no root): `adb push android/testing/xbox360-pad.evemu /data/local/tmp/
&& adb shell uinput /data/local/tmp/xbox360-pad.evemu` (it moves the sticks, the triggers, the
hat and A, lets go, presses Start+Select, and unplugs). The file is JSON lines: one `register` (vid 0x045e, pid 0x028e,
`UI_SET_EVBIT` [1, 3], the BTN_ keys, ABS 0-5, 16, 17 with their `abs_info`), then `inject`
(`[type, code, value, ..., 0, 0, 0]`) and `delay` lines; the pad is unplugged when it ends.

The real agent's sound: run it with `--keep-output` (else it makes its speaker the desktop's
output, and this PC's own sound goes to the emulator), and play into its speaker alone:
`pw-play --target remoteman-speaker tone.wav`. Keep the emulators on `-no-audio`: with audio
on they'd play on this PC's speakers.

## Sound and controllers on the emulators (2026-11-23)

Phone (an AVD like `Medium_Phone_API_37.0`) and `Google_TV_1080p`, `-gpu host -no-audio`:
- **Sound** from `padhost` and from the real agent (in `kde-nested.sh`, a 440 Hz tone played
  into its speaker): the agent was asked and made its speaker; the app played every packet, the
  loudest sample out of the callback matching the tone (0.51 for 0.5, 0.49 for 16000/32768).
  With `--drop 50 --pause 1:5`, what was concealed matched what was left out (about 9 frames
  in 5 s: 8 dropped, 1 or 2 at each pause's end), and the tone came back after every pause.
  The switch in the TV bar and the phone's controls turned it off and on (the agent's speaker
  went and came back).
- **The delay it adds**: the playout buffer held 10 to 35 ms (sampled every 5 s; depth 3 frames).
  On the phone emulator it grew to 5 frames once (35 to 53 ms): the emulator pulls in 20 ms
  bursts, and two packets just missed their turn. AAudio there: shared, not low-latency
  (the emulator has neither), burst 960 frames (20 ms), buffer 1920 (40 ms), and 108 to 127 ms
  from AAudio to the (null) output. A device with a low-latency path has bursts of 2 to 5 ms, so
  its own part is 4 to 10 ms plus the hardware's; not measured here.
- **Controllers**, phone and TV, against `padhost`: every button with its XInput bit, from
  `input gamepad keyevent` and a `uinput` Xbox 360 pad; sticks (right and up positive, the right
  stick from Z/RZ), triggers from LTRIGGER/RTRIGGER as 1.0 and 0.5, the hat as D-pad bits, the
  100 ms repeat while held, neutral when released, "unplugged" when the pad went. Start+Select
  opened the bar (and the pad went neutral there). On the TV the remote's Back opened the bar
  while A and the stick were held, and the computer got neutral at once; B on the computer, not
  leaving. Against the agent (no `gamepad`), B opened the bar.
- Not checked: real speakers, Bluetooth headsets and switching outputs mid-stream (the
  disconnect path), exclusive/low-latency AAudio, sound focus taken by a call, real controllers
  (Xbox, DualSense, 8BitDo, the Shield's; which axes each puts its right stick and triggers on),
  more than two controllers at once.

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
