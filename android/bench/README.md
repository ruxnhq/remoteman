# The decoder benchmark

For devices nobody here has. The bench build of the app (`assembleBench`, app id
`com.ruxn.remoteman.bench`, shown as "remoteman bench") carries four recorded streams and
plays each three ways:

1. **timing**: through the app's own decoder (Rust, MediaCodec into a Surface) at 60 a second,
   each picture timed from being handed to the decoder to its release to the screen;
2. **screen**: what the screen then shows, copied (PixelCopy) and compared with FFmpeg's
   picture of the same frame. At least 25 dB is right (a Galaxy S24 Ultra showing it right reads about 29), under 15 wrong, between: look at
   `files/screen-NAME.png`. The emulators fail this (about 2 dB): they show 10-bit pictures as 8-bit;
3. **pixels**: decoded into memory, each frame's MD5 against FFmpeg's (bit-exact or not).

It also lists every AV1 and HEVC decoder with what it claims (hardware, 10-bit, 4K at 60 and
120, low-latency mode, P010 output). Results: on the screen, in logcat (tag
`remoteman-bench`), in the app's `files/bench.json`, through the Share button, and to Firebase
Test Lab's results when it runs the app as a game loop.

The recordings are 1080p and 2160p at 60, AV1 and HEVC, 10-bit 4:2:0, from the Linux host
(NVENC) showing `scripts/animate.py`.

## Building

The recordings aren't in git (about 40 MB). Make them against a throwaway host that pairs with
anyone, then the references, then the APK:

```sh
# in a main checkout: scripts/kde-nested.sh with an agent on 127.0.0.1:7799 (--pair) and animate.py
android/bench/record.sh 127.0.0.1:7799      # android/bench/recordings: .rmrec and .bare
android/bench/reference.sh                  # FFmpeg's .md5 and .png; copies into android/bench/assets
android/build-core.sh
cd android && JAVA_HOME=/opt/android-studio/jbr ./gradlew assembleBench
# app/build/outputs/apk/bench/app-bench.apk
```

## Running it

- **A tester's phone or TV:** install `app-bench.apk` (allow installs from unknown sources, or
  `adb install`), open "remoteman bench", wait for "done" (a minute or two), press Share results
  and send the text. On a TV with no share target, `adb logcat -s remoteman-bench` or
  `adb shell run-as com.ruxn.remoteman.bench cat files/bench.json`.
- **Firebase Test Lab** (real devices in Google's data centres; needs a Firebase project, a
  few runs a day on the free plan):

  ```sh
  gcloud firebase test android models list --filter=form=PHYSICAL
  gcloud firebase test android run --type=game-loop --app=app/build/outputs/apk/bench/app-bench.apk \
      --device model=MODEL,version=API --timeout=10m
  ```

  The app writes its JSON to the file Test Lab gives it; it ends up in the run's results
  bucket with the logcat and a video of the screen.
- **The emulators:** `adb shell am start -n com.ruxn.remoteman.bench/com.ruxn.remoteman.BenchActivity`.

## On the emulator (2026-11-23, phone, API 37, `-gpu host`)

Only software decoders: dav1d (AV1 10-bit) bit-exact on every frame checked, at 1080p and 2160p;
the HEVC decoders (goldfish and Android's own) do 8-bit only, so HEVC is skipped. Screen: about
2 dB, wrong (the 10-bit display problem). Times are the emulator's CPU, not a device's.
