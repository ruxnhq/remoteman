# remoteman for Apple devices

The viewer for macOS and for iPad/iPhone: one set of Swift sources in `Shared/` on the Rust client core.

## Build

Needs Xcode, XcodeGen and Rust with the targets `aarch64-apple-darwin`, `aarch64-apple-ios` and `aarch64-apple-ios-sim`.

```sh
./build-core.sh        # the core as build/RemotemanCore.xcframework (./build-core.sh mac for macOS only)
xcodegen generate
xcodebuild -project Remoteman.xcodeproj -scheme RemotemanMac -derivedDataPath build/dd build
xcodebuild -project Remoteman.xcodeproj -scheme Remoteman -destination generic/platform=iOS \
    -derivedDataPath build/dd -allowProvisioningUpdates build
```

The macOS app lands in `build/dd/Build/Products/Debug/remoteman.app`.

## Run unattended

The app takes arguments, so it can be driven from a shell and checked without looking at it:

```sh
APP=build/dd/Build/Products/Debug/remoteman.app/Contents/MacOS/remoteman
$APP --host blackbox:7791 --pair yes --seconds 8 --snapshot /tmp/shot.png   # first connection to a host
$APP --play recording.rmrec --seconds 4 --snapshot /tmp/shot.png            # a recorded stream
```

It prints the stats overlay once a second, saves what the shader drew, leaves the host cleanly and quits. Every option takes a value (`--pair yes`), because AppKit also reads the arguments.

## Time the decoder

```sh
swiftc -O Shared/Bitstream.swift Shared/VideoDecoder.swift Shared/Recording.swift Shared/Bench.swift \
    Tools/rmbench/main.swift -o build/rmbench
build/rmbench recording.rmrec ...        # fed at the stream's rate; --flat-out for as fast as it goes
```

## What is where

| File | Job |
|---|---|
| `Shared/CoreClient.swift` | The Rust core's C interface behind a Swift class. |
| `Shared/VideoDecoder.swift`, `Bitstream.swift` | VideoToolbox, hardware only: HEVC Annex-B and AV1 OBUs to 10-bit pixel buffers. |
| `Shared/Renderer.swift` | Metal: the pixel buffer's planes as textures, BT.709 to RGB, newest frame at each refresh. |
| `Shared/ViewerView.swift`, `HIDKeys.swift` | The view and its input: mouse and keyboard on macOS, touches and hardware keyboard on iOS. |
| `Shared/ViewerSession.swift` | One session: source, decoder, renderer, stats. |
| `Shared/AppUI.swift` | The SwiftUI app, the connect form, the overlay, the launch arguments. |
| `Shared/Recording.swift`, `Bench.swift`, `Tools/rmbench` | Recorded streams and decode timing. |
