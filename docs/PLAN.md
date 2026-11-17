# remoteman plan

remoteman makes an iPad or iPhone a real display, keyboard and touchscreen for this Linux PC. The PC gives the device a virtual monitor with the device's own pixels and refresh rate and encodes it on the GPU. The device decodes it on its GPU and sends input back. GNOME on Wayland comes first. KDE, and hosts that aren't Linux (the Mac side is half built in espalier), come later.

Status: design only, nothing is built. Written 2026-11-17 on this PC: GNOME 51.1, RTX 5090 on driver 620.70, kernel 7.3, PipeWire 1.6.

Every claim carries a mark:
- **[V]** checked today on this machine, or read in the upstream source at the installed version.
- **[R]** from web research with sources. The Apple side reported on 2026-11-17, and the claims that carry weight were re-read at the source.
- **[M]** from memory, not confirmed. The Linux and KDE research reported later the same day and is folded in, so few of these remain.
- **[?]** not known. A Phase 0 spike settles it.

## Decided (2026-11-17)

- **HDR:** option (a). SDR virtual displays on a 10-bit, PQ-capable pipeline; HDR by mirroring for now; the Mutter patch in Phase 6.
- **Sessions:** option (a). Remote login and attach, picked automatically.
- **Login:** the login screen first, Face ID as the daily default, key only as a setting.
- **Signing:** the Apple developer account on the Mac. The Mac session ("Mac session") builds and signs.
- **Two apps.** A **client** app and a **host** app, each its own product, sharing only the Rust core and the wire spec. Today that is one client (iPad and iPhone) and one host (Linux, GNOME). Any client must work with any host, so more of either (Mac, Windows, Android clients; KDE, Mac, Windows hosts) are additions and never rewrites. Nothing platform-specific goes into the protocol: it carries display descriptions, HID usages and MIME types, not GNOME or iOS notions.

- **The Mac is a host from the start (2026-11-17).** The iPad and iPhone app controls the Mac as well as the PC. The Mac host is its own phase right after the walking skeleton (Phase 2 below), built from espalier's Mac screen code and speaking this protocol.

Still to hear: which iPad and iPhone, and how the Mac session gets the code (suggested: a private GitHub repository).

## Decisions that were yours

### 1. HDR and virtual displays don't come together on stock GNOME 51

**[V]** Mutter 51.1 offers HDR screencasts (10-bit, BT.2020 with PQ) only for physical monitors. A virtual monitor has no colour mode at all, and a remote-login session has nothing but virtual monitors.

- **(a) Recommended.** Virtual displays are SDR for now. The pipeline is still built 10-bit and PQ-capable from end to end. HDR works when the iPad mirrors one of your physical HDR monitors. HDR on virtual displays arrives through a Mutter patch we write in Phase 6 and send upstream.
- **(b)** Patch Mutter before anything else. HDR from day one, but a patched compositor on your daily machine from the start, rebuilt at every Mutter update.
- **(c)** Never patch. HDR in mirror mode only.

The catch in mirror mode, which (a) and (c) both lean on: a monitor has to be on to be mirrored. So HDR today means that physical monitor stays lit and shows your unlocked desktop to the room while you're connected. It also means the monitor's 4K picture scaled to the iPad, not the iPad's own pixels.

### 2. What happens when you're already logged in at the PC

**[V]** GDM 51.1 never runs two graphical sessions for one user, and nothing in it brings a running local session to a remote client (`gdm-manager.c`):
- You log in remotely and a headless session of yours exists: GDM joins you to it. That is how reconnecting works.
- You log in remotely and a local session of yours exists: GDM has no display to hand over. The login screen shows "Session Already Running" with Cancel and Force Stop **[R]**; Force Stop ends the other session and closes its apps.
- `CreateUserDisplay` simply fails if a graphical session is active.

So "remote login" only gives you a desktop when you are not logged in at the PC, or at the price of that session.

- **(a) Recommended.** Both ways in, picked automatically. No session: remote login into a headless session. Session running: attach to it. The table under "Two ways in" says what each does to your apps and screens.
- **(b)** Remote login only. If you're logged in at the PC, that session has to be logged out first, and its apps close.
- **(c)** Attach only. No system service. The PC must already be logged in (for example by autologin) for the iPad to reach it.

### 3. How you get past the login from the iPad

- **Login screen.** GNOME's own login screen shows on the iPad and you type the password. Works with any PAM setup and unlocks the keyring. Built first, because it needs nothing special.
- **Face ID.** The password sits in the device Keychain behind Face ID and goes over the encrypted link. Our agent in the login-screen session answers GDM's password prompt with it. Unlocks the keyring. **[?]** whether a helper in that session may answer the prompt (spike S4).
- **Key only.** **[V]** GDM 51 can start a session for an "already authenticated" user with no password. The paired device key is the proof, and nothing secret ever leaves the device. **[?]** The login keyring probably stays locked, so the first app that wants a stored secret asks for the password. That is inferred from GDM's notes on fingerprint login, not tested (S4).

Recommended: the login screen first, Face ID as the daily default, key only as a setting.

### 4. Smaller things I need to know

- **Which iPad and iPhone.** The numbers here assume an iPad Pro 13-inch (2752×2064, 120 Hz). The model decides the resolution and the codecs. **[R]** Apple's spec pages list AV1 decode for the A18 and A19 families and the M4 and M5 iPad Pro, and not for the M1 iPad Pro or M2 iPad Air.
- **Apple signing.** Which Apple developer account signs the app. **[R]** A free account is enough, since nothing here needs a paid-only entitlement. It means re-signing every 7 days, which the Mac session can do as part of a rebuild, and at most 3 devices. A paid one lasts a year.
- **How the Mac session gets the code.** This folder isn't a git repository yet. Suggested: a private GitHub repository like espalier's, which this session pushes and the Mac session clones.

## What this machine and GNOME 51 do today

| Area | Finding | Mark |
|---|---|---|
| Desktop | GNOME Shell and Mutter 51.1 on Wayland. Two ASUS XG32UCWMG at 3840×2160, 240 Hz, VRR, both in BT.2100 (HDR) mode, both on the RTX 5090. The AMD iGPU drives nothing. | [V] |
| Virtual monitors | `ScreenCast.RecordVirtual` creates one. Size and refresh rate follow the PipeWire format the consumer asks for, up to 1000 Hz. HiDPI scale comes from the stream property `org.gnome.preferred-scale`. `is-platform` makes it count as a real monitor, not as "the screen is being shared". Asking for a new size resizes it. | [V] source |
| HDR capture | Monitor streams offer xRGB/xBGR 2101010 with BT.2020 primaries and the PQ transfer. Virtual streams never do, and the colour manager skips virtual monitors. | [V] source |
| Cursor, damage | The stream carries the cursor as metadata (position and bitmap) and the damaged regions of each frame. | [V] source |
| Input | Mutter's `RemoteDesktop` session takes key codes and keysyms, relative and absolute pointer, buttons, smooth and stepped scroll, multi-touch, and a clipboard with MIME types. It also hands out a libei connection. libei 1.6 has pointer, absolute pointer, keyboard, touch, scroll, button and text devices. It has no tablet or stylus device and no gesture events. | [V] |
| Lock screen | In a local session GNOME Shell blocks all remote access whenever the session isn't in the unlocked user mode, and closes sessions that are running. Headless sessions are exempt. | [V] source (`main.js`) |
| GDM | `RemoteDisplayFactory.CreateRemoteDisplay` creates a headless login screen, or with `preauthenticated-user` the user's session directly. The bus allows it for root, group `gdm`, and gnome-remote-desktop's own user through its policy file; there is no polkit check on it. `CreateUserDisplay` is polkit-gated and fails if the user has a graphical session. Only sessions marked `X-GDM-CanRunHeadless` qualify, and GNOME is the only one installed. | [V] |
| GNOME's remote login | gnome-remote-desktop's system service is enabled and running here. Its design: a system daemon owns the port, a per-session helper (a user unit in `gnome-session.target`) asks for the client, and the RDP client is redirected to it with a token. | [V] source |
| Encoder | Measured at 2752×2064 (iPad Pro 13-inch) with low-latency settings, frames fed from CPU memory, so these are floors and not NVENC's limit: HEVC Main10 4:2:0 at 172 fps, HEVC 4:4:4 10-bit at 95 fps, AV1 4:2:0 10-bit at 271 fps. AV1 4:4:4 is refused. The driver also exposes Vulkan Video encode for H.264, H.265 and AV1. | [V] measured |
| espalier | Already does this on this PC: portal ScreenCast, PipeWire DMA-BUF, Vulkan, CUDA, NVENC, at 3.6 ms per 4K frame. It lets NVENC convert RGB to YUV, which uses BT.601. | [V] read |

## Ground rules

- **Personal use first**, like espalier. Private APIs are fine where they're worth it: Mutter's ScreenCast and RemoteDesktop D-Bus interfaces (marked private, no compatibility promise) and GDM's RemoteDisplayFactory. Each sits behind a version check in one crate.
- **GPU end to end.** No pixel touches CPU memory on the PC. Hardware decode on the device.
- **The device's panel is the monitor.** The virtual display has the device's exact pixels, scale and refresh rate, so nothing is scaled anywhere.
- **Rust for everything except the Apple UI.** One shared Rust core runs on both ends.
- **One wire spec**, `docs/PROTOCOL.md`, with test vectors. It is the contract between the Linux session and the Mac session.
- **Clean-room.** gnome-remote-desktop, Sunshine, Moonlight and RustDesk are behaviour references only. Never copy their code.
- **Measure, don't claim.** A stats overlay shows fps, bitrate, capture-to-present latency and loss. Every phase ends on numbers.

## Design

```
 iPad / iPhone app                            Linux PC
┌───────────────────────────┐    input     ┌──────────────────────────────────────────────┐
│ Swift: UI, input capture  │ ───────────▶ │ remotemand      system service, own user      │
│ Rust core: transport      │   QUIC/UDP   │   the UDP port · trust store · session broker │
│ VideoToolbox: decode      │ ◀─────────── │      │ D-Bus                 ▲ Unix socket    │
│ Metal: present at 120 Hz  │ video, audio │      ▼                       │                │
└───────────────────────────┘              │     GDM ──── starts ───▶ login session        │
                                           │                          ├ GNOME Shell, Mutter │
                                           │                          └ remoteman-agent     │
                                           │  virtual monitor → PipeWire DMA-BUF →         │
                                           │  Vulkan convert → NVENC → agent → remotemand  │
                                           └──────────────────────────────────────────────┘
```

### Hosts and clients

The split that keeps every pairing possible:

| Layer | What it is | Shared? |
|---|---|---|
| Wire protocol | `docs/PROTOCOL.md` | One, for everything. |
| Rust core | Transport, loss recovery, pairing, rate control. It has a **client role and a host role**, and both are usable from Rust and, through a C/UniFFI interface, from Swift. | One code base, built for each OS. |
| Host shell | Makes the display, captures, encodes, injects input. | One per OS: Linux agent (Rust: Mutter, Vulkan, NVENC), Mac host (Swift: virtual display, ScreenCaptureKit, VideoToolbox, CGEvent), later Windows. |
| Client shell | Decodes, presents, captures input. | One per OS family: Apple (Swift: VideoToolbox, Metal) for iPad, iPhone and Mac; later Linux, Windows, Android. |

A host says what it can do in its hello (codecs, chroma, HDR, virtual displays, remote login, sound, clipboard), and a client shows only that. So the Mac host can lack remote login or AV1 without the app or the protocol knowing anything about Macs.

What the Mac host is, next to the Linux one:

| | Linux (GNOME) | Mac |
|---|---|---|
| Display | Mutter virtual monitor | CGVirtualDisplay (espalier's shim **[V]** read), sized to the device, HiDPI |
| Capture and encode | PipeWire DMA-BUF, Vulkan pass, NVENC | ScreenCaptureKit and VideoToolbox HEVC (espalier: 3840×2160 in 10.5 ms on an M4 **[V]** its commit). **[M]** No AV1 encode on Apple hardware; 10-bit, HDR and 4:4:4 from VideoToolbox are to be measured. |
| Input | libei through Mutter | CGEvent, by HID usage (espalier's `Input.swift`) |
| Getting in | Remote login through GDM, or attach | Attach only: the Mac must be logged in. Its lock screen works over the stream in espalier (the user, 2026-11-17), so unlocking from the device carries over. |
| Runs as | System daemon and per-session agent | One menu-bar app or launch agent, as espalier's bridge does |

### Processes on the PC

- **`remotemand`**, a system service running as its own user. It owns the one UDP port and every client connection: TLS, packets, loss recovery, pacing, rate control. It holds the trust store. It brokers sessions: asks GDM for a headless display, watches GDM's `RemoteDisplay` objects to learn which logind session the client now belongs to, and unlocks a local session through logind. It never touches pixels or your session bus. It ships a D-Bus policy file granting the same two GDM interfaces gnome-remote-desktop's file grants **[V]**, and a polkit rule for `org.freedesktop.login1.lock-sessions` **[V]**.
- **`remoteman-agent`**, a user service in every graphical session, the login screen's included (`WantedBy=gnome-session.target`, as gnome-remote-desktop's helper **[V]**). It talks to Mutter (virtual monitors, input, clipboard, monitor layout), captures, converts, encodes, and captures sound. It connects to the daemon over a Unix socket, and the daemon identifies it by peer credentials and logind session.
- Because the daemon keeps the connection, going from the login screen to your session is a change of video source inside one connection. gnome-remote-desktop has to redirect its RDP client instead **[V]**.
- **`remoteman-agent --standalone`** listens by itself with no daemon. That is for development, and for desktops without GDM such as KDE. It can't unlock a locked session.
- **`remotemanctl`**: pair (shows a QR code), list, revoke, status.

### Two ways in

| | Remote login | Attach |
|---|---|---|
| When | You have no session on the PC. | You're logged in at the PC, locked or not. |
| How | The daemon asks GDM for a headless display. You log in on it (decision 3). | The daemon unlocks your session through logind. The agent in it adds a virtual monitor. |
| Monitors | Virtual only. | Virtual. Or a mirror of a physical one, which is the HDR path today. |
| Your physical screens | Show nothing of it. | With a virtual monitor: switched off while you're connected, put back after. When mirroring: that monitor has to stay on. |
| On disconnect | The session keeps running and locks by GNOME's own idle rules. Reconnecting resumes it **[V]**. | The virtual monitor goes, the session locks, the screens come back. |
| Your apps | Only what you start in that session. Logging in at the PC later conflicts with it, and GDM can only end one of the two **[V]**. | Your real desktop with everything open. |
| Lock screen | Works remotely **[V]**. | The session must be unlocked before any stream can start **[V]**. |
| HDR today | No **[V]**. | Yes when mirroring **[V]** source, **[?]** on NVIDIA (S7). |

The daemon picks the way in. A paired device belongs to the Unix user who paired it, so the daemon asks logind whether that user has a local session before it asks GDM for anything.

Attach mode unlocks a desktop in an empty room, so two rules hold. If the physical screens can't be switched off, the session stays locked and no stream starts, unless you chose mirroring. And the PC's own keyboard and mouse stay live while you're connected; nothing here can stop someone at the desk from using them.

Open in attach mode **[?]** (S6): switching physical monitors off moves windows onto the virtual one; whether they return to their places afterwards. And whether monitor power saving throttles the virtual monitor too (Mutter 51.1 added throttling while power saving).

### Picture path

1. **Virtual monitor.** One per device display, through `RecordVirtual` with `is-platform` and the cursor as metadata. The device's hello carries its pixels, scale, refresh rate, HDR headroom and safe area. Rotation, Split View and window resizing on the iPad renegotiate the stream, and Mutter resizes the monitor **[V]**. The same happens when the device lowers its refresh rate: **[R]** Low Power Mode and heat cap ProMotion.
2. **Capture.** PipeWire DMA-BUFs with damage regions. No damage, no frame: an idle desktop sends nothing.
3. **Convert.** One Vulkan pass turns RGB into YCbCr with the right matrix (BT.709 for SDR, BT.2020 with PQ for HDR), in 8 or 10 bits, 4:2:0 or 4:4:4, written straight into the encoder's input. This replaces espalier's two GPU copies and NVENC's BT.601 conversion.
4. **Encode.** NVENC through NVIDIA's SDK directly: no B-frames, no lookahead, a one-frame rate buffer, no periodic keyframes. FFmpeg's wrapper is enough for Phase 1 but **[R]** has no reference-frame invalidation and no long-term references, which loss recovery needs. **[R]** The 5090 has 3 NVENC engines. Split-frame encoding across them is automatic, but for HEVC only from 2112 lines up, so an iPad's 2064 lines don't get it (AV1's limit is 2048), and it switches off when sub-frame readback is used. An `Encoder` trait leaves room for VA-API, Vulkan Video, VideoToolbox (Mac host) and Media Foundation (Windows host).
5. **Codec modes**, chosen from what the device reports at connect. Apple publishes no per-chip table for 4:2:2 or 4:4:4 **[R]**, so the app finds out for itself: it tries to open a hardware-only decoder for each mode from a sample stream header (a public VideoToolbox option, iOS 17 and later **[R]**).

   | Mode | Stream | When |
   |---|---|---|
   | Default | HEVC Main10, 4:2:0, 10-bit | Every device. Whether it holds 120 fps at this size is **[?]** (S3); Apple documents no limit **[R]**. |
   | Sharp text | HEVC 4:4:4, 10-bit | Only if the device decodes it in hardware **[?]** (S3) and NVENC holds 120 fps without the CPU upload **[?]** (S2; 95 fps is the CPU-fed floor). **[R]** The signs are mixed: Moonlight's iOS client never offers 4:4:4, one fork of it does "where the device decodes it in hardware", and Apple's own 4:4:4 screen sharing is for Apple-silicon Macs only. |
   | Alternative | AV1 Main, 4:2:0, 10-bit | Devices with AV1 hardware decode (listed under decision 4) **[R]**. NVENC is faster at it (271 against 172 fps, CPU-fed). No sign of AV1 4:4:4 at either end. |
   | HDR | Either 4:2:0 10-bit stream, signalled BT.2020 PQ with mastering metadata | The source is HDR. |

   Why 4:2:0 is an acceptable default: at 2× scale the colour resolution equals the desktop's logical resolution, so coloured text suffers far less than on a 1× screen.
6. **Static refinement.** When the picture stops changing, a few more frames raise it to near-lossless, then the stream goes quiet.
7. **Cursor.** Shape and position travel on their own channel and the device draws the cursor at its own refresh rate. With a trackpad or mouse the device moves it at once and the PC confirms.

Latency targets on a good LAN, to be measured and not yet claims: convert and encode 2 to 4 ms, network 2 to 5 ms, reassembly and decode 3 to 6 ms, wait for the next refresh 0 to 8 ms. That is about 12 to 25 ms from capture to present at 120 Hz.

### Protocol (RMP)

- **QUIC over one UDP port**, the same Rust code (quinn, rustls) on both ends. TLS 1.3 with both sides pinned at pairing.
- **Reliable streams:** control (JSON, forgiving as in espalier's BRIDGE.md: unknown types and fields are ignored), input (binary, on a stream of its own so a clipboard blob can't delay a keystroke), clipboard and files.
- **Datagrams:** video, audio, cursor position. A frame is cut into fragments of about 1200 bytes carrying the frame number, fragment index and capture time.
- **Loss, in this order:** the device asks again for missing fragments while the round trip is a fraction of a frame (LAN). Reed-Solomon parity per frame, as much as measured loss calls for (slower links). A frame lost anyway: the device says which, the encoder drops its references to it and carries on from the last good frame. A keyframe is the last resort.
- **One control loop for rate.** A delay-based congestion controller plugged into quinn sets both the pacing and the encoder's target bitrate. A frame's packets are spread over part of the frame interval and never sent as one burst.
- **Sessions outlive the network.** The daemon issues a resume token. After sleep, a Wi-Fi roam or a daemon restart, the device reconnects with it and lands in the same session with no login. A stream epoch number goes up whenever the source changes (login screen to session, resize, codec change); the device then flushes its decoder and waits for the new configuration and keyframe.
- **One client per session.** The newest takes it over, as in BRIDGE.md.
- **Finding the PC:** Bonjour `_remoteman._udp`, plus stored addresses (LAN address, Tailscale name) tried together. Wake-on-LAN from the app.
- **Clock exchange** for the end-to-end latency figure.
- **On iOS** **[R]**: Apple accepts plain UDP sockets for this, and they are what quinn uses. The first packet can be refused until the Local Network prompt is answered, so the core retries. quinn can swap its socket under a live connection, which covers a move from Wi-Fi to cellular. A suspended app can lose its socket, which is what the resume token is for.
- **[?]** (S5) If quinn's datagram path costs too much on the device, there are two steps. First turn on quinn's batched send and receive for Apple, which is off by default because it uses private calls **[R]**; that is acceptable in a personal build. Then move video and audio to a plain sealed-UDP path and leave QUIC the control. The media packet format doesn't depend on QUIC, so that change stays contained.

### Input, clipboard, sound

- **On the PC:** the agent injects through Mutter's `RemoteDesktop` session and its libei connection: pointer, absolute pointer per virtual monitor, scroll, buttons, keyboard by key code, multi-touch, and text for what the soft keyboard and input methods produce (**[R]** Mutter 51.1 doesn't implement libei's new text device yet, MR !4996 is open; so text goes through Mutter's keysym call **[V]**).
- **Touch** goes through as real touch events, so GNOME's own gestures work.
- **Trackpad and mouse:** scroll with momentum, pinch, the cursor drawn locally. **[R]** Pointer lock, which gives raw motion, only works while the app is full screen and in front. In a window or Split View the pointer is sent as a position instead. UIKit's pointer events come first and GameController's mouse only for raw motion: Moonlight's iOS client notes that it misreads many mice.
- **Hardware keyboard:** keys go by physical position (USB HID usage to Linux key code), as espalier's Mac screen does. ⌘ maps to Super or Ctrl by a setting. **[R]** iOS keeps ⌘Tab for itself, and no API or entitlement lets an app take it. Screens works around it by swapping ⌘ and Ctrl or by offering another chord. **[M]** ⌘Space, ⌘H and the Globe key are probably the same. An on-screen key bar and remaps cover those.
- **iPhone:** a trackpad mode (drag moves the pointer, tap clicks, two fingers scroll) and pinch to zoom the view.
- **Pencil:** pointer only in v1. The iPad reports pressure, tilt, roll and hover **[R]**, but in a headless session the PC has no way to take them in, because libei has no tablet device **[V]**. **[R]** A stylus device (pressure, tilt, eraser) and pinch and swipe gestures are merged in libei's unreleased main branch; Mutter has no support for either yet.
- **Clipboard:** Mutter's clipboard calls on one side, `UIPasteboard` on the other. Text first, then images.
- **Sound:** the agent creates a PipeWire sink, makes it the default while connected, and sends Opus in 5 to 10 ms frames. The device plays through a 20 to 40 ms jitter buffer, timed against the video. Microphone later.

### Pairing and security

- **Pairing:** `remotemanctl pair` shows a QR code holding the PC's name, addresses, key fingerprint and a one-time secret. The app scans it and both sides store each other's key. Without a camera: compare a 6-digit code, as espalier's handshake v2 does.
- **The device key** is made in the Secure Enclave and never leaves it. Face ID can be required for each use.
- **Only paired keys get past the TLS handshake.** An unknown device reaches no login screen and no parser beyond TLS. Pairing mode is open for 2 minutes at a time.
- **The daemon** is unprivileged, sandboxed by systemd, and throttles connection attempts (gnome-remote-desktop added throttling for CVE-2025-5024 **[V]** notes).
- **Attach mode unlocks your session.** By default the device key with Face ID is enough. A setting adds your password, checked by PAM on the PC.
- **Reaching the PC from outside:** Tailscale or WireGuard for now. A relay of our own is later work.

## The app (iPad and iPhone)

- **Swift**, with SwiftUI around a Metal view. One code base, plus a macOS build of the same client for testing.
- **Runs on** iOS and iPadOS 17 or later **[R]**. That is where the hardware-only decoder option and the Metal display link start.
- **`RemotemanCore`**: the shared Rust core as an XCFramework. It holds the protocol, the transport, reassembly and loss recovery, pairing logic and stats. The Swift side never parses the wire. UniFFI generates the control interface. Frames and sound cross through a small C interface, because UniFFI copies byte buffers **[R]**.
- **Decode:** VideoToolbox, hardware only, no frame reordering, output as GPU-backed pixel buffers.
- **Present:** Metal. The decoded planes become textures with no copy (10-bit planes are read as 16-bit textures **[R]**), a shader converts to RGB and draws the cursor, and a Metal display link presents the newest frame at each refresh. 120 Hz needs nothing extra on an iPad Pro and one Info.plist key on an iPhone **[R]**. For HDR, an EDR layer in the PQ colour space with HDR10 metadata (iOS 16 and later **[R]**), tone mapped to the headroom the screen has at that moment.
- **Input:** as listed above.
- **Sound:** Opus decoded in the Rust core, played through AVAudioEngine.
- **Lifecycle:** **[R]** iOS suspends the app about 5 s after it leaves the screen. Coming back reconnects with the resume token; the target is under 1 s to the first frame. Keeping the PC's sound playing in the background is possible as a setting, and only while sound is really playing.
- **Finding PCs:** Bonjour through the system browser, with two Info.plist keys. No multicast entitlement is needed **[R]**.
- **Screens:** the PC list (Bonjour and saved), QR pairing, the session with a pull-down bar for keys and settings, a stats overlay.
- **Building on the Mac:** XcodeGen `project.yml` (as espalier's camera extension), `xcodebuild`, install with `xcrun devicectl`. The macOS build of the client lets the Mac session run it against the PC, read its stats and take screenshots, so it can check its own work without the iPad in hand.

## What comes from espalier

| Piece | Use here |
|---|---|
| `espalier-capture` Linux `capture.rs` (PipeWire stream, format negotiation) | The starting point for capture, reading Mutter's stream instead of the portal's. Its `gpu.rs` importer is not carried over: the spike measured it losing frames, and one Vulkan pass replaces it. |
| `espalier-capture` Linux `encode.rs` (FFmpeg NVENC) | Not needed: the spike's direct NVENC wrapper already works and is what Phase 1 starts from. |
| `espalier-render` Linux (hardware decode, wgpu presentation) | A Linux test client, so the PC side can be tested end to end without the Mac. |
| Bridge handshake v2, pairing code, trust store | The pairing ceremony and its commit-before-reveal idea carry over. The bytes don't: TLS inside QUIC replaces the handshake. |
| Bridge TCP record transport | Not reused. One lost packet stalls everything behind it, which is wrong for 120 fps over Wi-Fi. |
| `mac-bridge` `Screen.swift`, `ScreenEncoder.swift`, `Input.swift`, `VirtualDisplayShim` | The future Mac host. They already play the Linux agent's parts (virtual display, capture, encode, input, cursor shapes) and would speak this protocol instead. |
| `espalier-discovery` | Advertising the PC. |
| Mac screen drag in and out (`ScreenDrag.swift`, Linux `drop.rs`) | The model for files and drag and drop in Phase 7: offer, fetch on demand, drop at the pointer. |
| Low-latency findings (espalier `539a27b`, measured) | Lessons that carry over. On an M4, VideoToolbox's low-latency rate control and its "real time" flag each slow the encoder; the ordinary rate control with reordering off is about twice as fast (3840×2160 H.265: 10.5 ms against 18.7). That matters for the Mac host, and the decode side gets the same scrutiny in S3. Pointer moves are merged to one per frame and sent ahead of any click or key. The stream asks for the display's real rate. A slightly longer frame queue saves a keyframe after a hiccup. |

remoteman is its own repository. Code is copied from espalier and adapted, not linked to it.

## Repository, and who builds what

```
docs/PLAN.md            this file
docs/PROTOCOL.md        the wire spec and test vectors (first thing written in Phase 1)
crates/
  remoteman-proto       wire types and framing, no I/O            shared
  remoteman-transport   QUIC session, fragments, FEC, pacing, rate shared
  remoteman-core        the core's client and host roles for Swift (C/UniFFI)
  remoteman-desktop     DesktopBackend trait; Mutter, later portal and KWin
  remoteman-capture     PipeWire and DMA-BUF import
  remoteman-gpu         Vulkan conversion, CUDA hand-off
  remoteman-encode      Encoder trait; NVENC first
  remoteman-audio       PipeWire sink, Opus
  remoteman-agent       the per-session agent
  remoteman-daemon      remotemand
  remoteman-ctl         remotemanctl
  remoteman-testclient  Linux client for testing
apple/                  project.yml, the iOS app, the Mac host app, the macOS test client, shared Swift
packaging/              Arch package: units, sysusers, D-Bus policy, polkit rule
```

- **This session (Linux)** owns all the Rust: host, shared core, protocol spec, test client.
- **The Mac session** owns `apple/`: it builds the Rust core for the Apple targets and writes the Swift around it. Its contract is `docs/PROTOCOL.md` and the `remoteman-core` API.
- **Two seams** keep later hosts cheap: the wire spec, and the `DesktopBackend` and `Encoder` traits (GNOME, KDE, Mac and Windows differ only behind them).

## Phases

### 0. Spikes: prove the risky parts

S1 to S3 and S5 stay off your live desktop: compositor spikes run in a throwaway headless GNOME Shell on its own D-Bus session. S4, S6 and S7 need your go-ahead each time. S4 needs admin rights (a throwaway user, a process in group `gdm`) and talks to the same GDM that owns your running session; GDM's notes list past bugs where a bad request took it down with every session, so save your work first.

| | Question | Pass |
|---|---|---|
| S1 | Does a virtual monitor of 2752×2064 at scale 2 really run at 120 Hz, on the 5090? | At least 118 fps over 10 s of animation. Frames arrive as DMA-BUFs in a layout espalier's importer takes. Rendering is on the 5090 and not the iGPU (if not, Mutter's udev tag for the primary GPU fixes it). |
| S2 | What does NVENC do without the CPU upload? | 4:2:0 10-bit at 4 ms or less per frame, 99th percentile. The 4:4:4 10-bit figure at 120 fps, with and without split encode, recorded. Reference invalidation and a live bitrate change work with no keyframe. |
| S3 | What do the iPad and iPhone decode in hardware? | S2's streams through VideoToolbox and Metal: 6 ms or less per frame at 120 fps for 4:2:0 10-bit, steady 120 Hz presentation. Whether 4:4:4 and AV1 decode in hardware on each device, recorded. A PQ clip shows in EDR. |
| S4 | Can we drive GDM? | With a throwaway user, from a process in group `gdm`: a headless login screen and a pre-authenticated session both appear in logind as remote, a user unit starts in each and creates a virtual monitor, the session survives a disconnect, and logout cleans up. Also recorded: whether a helper in the login-screen session can answer GDM's password prompt, and whether the keyring is locked after a pre-authenticated login. |
| S5 | Is QUIC cheap enough on the device? | 100 Mbit/s of datagrams over Wi-Fi for 60 s: no loss beyond the link's own, under 3 ms of added delay at the 99th percentile, under a quarter of one core. Otherwise batched sockets, then sealed UDP for media. |
| S6 | Does attach behave? | Unlock to first frame in under 1 s. Physical monitors off and back with windows returning to their places. No throttling of the virtual monitor while the physical ones sleep. Needs your desktop once, with you watching. |
| S7 | HDR from a physical monitor on NVIDIA? | `RecordMonitor` on a monitor in BT.2100 mode delivers 10-bit PQ frames. Needs your desktop. |

#### Spike results so far (2026-11-17)

All runs: a throwaway headless Mutter 51.1 on a private D-Bus session, with a GPU-drawn GTK window redrawing every frame as content (it drew 120 to 121 frames a second), 10 s measured after a 4 s warm-up, preset P1, 60 Mbit/s asked. Entry point: `spikes/s1/run.sh WIDTH HEIGHT FPS SECONDS MODE`.

**S1, the virtual monitor: pass.**
- A headless Mutter picks the 5090 as its primary GPU.
- It delivers exactly 120.0 fps (frame interval median 8.33 ms, p99 8.6) as DMA-BUFs at 2752×2064 and 3840×2160, NVIDIA modifier `0x300000000606015`. 240 Hz is accepted too.

**S2, the encoder: pass.** This is the real path, written for the spike: the DMA-BUF is imported into Vulkan, one compute pass converts it to 10-bit BT.709 Y'CbCr straight into memory shared with CUDA, and NVENC is driven through NVIDIA's API (`spikes/capture-spike`: `vkconv.rs`, `shaders/convert.comp`, `shim/nvenc_shim.c`, `direct.rs`). All 10-bit, no frame lost in any run.

| Size | Mode | Convert, ms | Encode, ms (median / p99) | Capture to encoded, ms (median / p99) |
|---|---|---|---|---|
| 2752×2064 | HEVC 4:2:0 | 0.23 | 3.25 / 3.92 | 3.49 / 4.24 |
| 2752×2064 | HEVC 4:4:4 | 0.23 | 5.92 / 6.61 | 6.15 / 6.90 |
| 2752×2064 | AV1 4:2:0 | 0.23 | 1.64 / 1.86 | 1.90 / 2.12 |
| 3840×2160 | HEVC 4:2:0 | 0.26 | 2.08 / 2.54 | 2.37 / 2.85 |
| 3840×2160 | HEVC 4:4:4 | 0.25 | 3.44 / 5.28 | 3.70 / 5.60 |
| 3840×2160 | AV1 4:2:0 | 0.26 | 2.62 / 3.15 | 2.90 / 3.45 |

- **4:4:4 at 120 Hz works on the PC side.** At iPad size it takes 5.9 ms a frame, inside the 8.3 ms frame but over the 4 ms target. Whether it is offered now rests on the device (S3).
- **HEVC is slower at iPad size than at 4K**, as the research said: it only splits a frame across the 5090's encoders from 2112 lines up. AV1 splits from 2048 lines and is the fastest mode at iPad size.
- **Live bitrate change: works.** Halving the target on the running encoder lowered the output (34 to 23 Mbit/s for HEVC) with no keyframe. The content never filled the 60 Mbit/s asked, so this shows the direction and not the accuracy.
- **Reference invalidation: accepted** by HEVC and AV1, and the next frame was not a keyframe. Not yet checked: that a decoder which really lost that frame recovers cleanly.
- **Picture checked**: every stream decodes with FFmpeg and is signalled as intended (Main 10 / Rext / AV1 Main, BT.709, limited range, centre chroma for 4:2:0). Frames pulled from each look right: correct colours, sharp text, no tearing.
- **Not done:** the stream ignores Mutter's explicit-sync metadata and relies on the frame being finished when it arrives. No tearing was seen, but that isn't proof. PQ/BT.2020 signalling is wired in the wrapper and untested.

**The first attempt, kept for the record.** espalier's capture and FFmpeg NVENC, adapted to a Mutter stream, reached only about 115 fps: its DMA-BUF → Vulkan → CUDA copies average 3.2 ms and stall for up to 20 ms on about 1 frame in 12. Its 8-bit encodes took 3.1 ms (HEVC) and 1.3 ms (AV1) at iPad size. The new path replaces those copies with the 0.23 ms pass, so nothing of that importer is carried forward. It is still in the spike (modes `none`, `hevc`, `av1`) for comparison.

**S3, device decode: decoders open on the Mac, the iPad and the iPhone; frame decode and 120 fps timing are still to do** (the Mac session's report, not re-checked here).
- **Mac mini M4, macOS 26.5.2:** a hardware-only decoder session opens for all three streams at 2752×2064 10-bit: HEVC Main10 4:2:0, HEVC 4:4:4 and AV1. This is session creation only. No frame was decoded, so it says nothing yet about 120 fps or about 4:4:4 at frame time.
- **iPad Pro 13-inch (M5), iPadOS 27.2: all three open in hardware** (HEVC Main10 4:2:0, HEVC 4:4:4 10-bit, AV1 10-bit), after the Mac was upgraded to macOS 27.2.1 and Xcode 27.2; Xcode 26.6 could not pair with iPadOS 27 at all. Same limit as on the Mac: decoder creation only, no frame decoded, so 120 fps and 4:4:4 under load are still unproven. The iPhone 18 Pro Max (iOS 27.2) gave the same three results.
- **So the plan's iPad assumption holds:** 2752×2064 at 120 Hz, and both 4:4:4 and AV1 stay on the table for it.
- **Signing:** team R82PLAJ45H is a paid team and is the one to use.
- **Getting files to the Mac:** espalier's file transfer works today (files land in `~/Downloads/Espalier`). The Mac reaches GitHub as xnixy, so a private repository works for code. The Mac can't reach the PC over SSH.

Stray effects of the runs: the stock `es2gears_wayland` and `eglgears_wayland` crash inside headless Mutter (two core dumps were left in the system journal), and `vkcube` stops animating after a few seconds there, so neither is usable as test content.

### 1. Walking skeleton (attach, standalone agent, LAN)
A virtual monitor through Mutter's API, the spike's capture, Vulkan pass and direct NVENC (HEVC or AV1, 10-bit 4:2:0), QUIC with datagrams and nothing cleverer than a keyframe request, touch and hardware keyboard and pointer through libei, the cursor channel, QR pairing, the stats overlay. The Linux test client and the iPad app.
**Done when:** the desktop is usable from the iPad at native resolution and 120 fps, with capture-to-present at 25 ms or less (95th percentile) on the LAN.

### 2. Mac host
A Mac host app from espalier's `Screen.swift`, `ScreenEncoder.swift`, `Input.swift`, `Cursor.swift` and the virtual display shim, with the Rust core's host role in place of the bridge's TCP records. The same iPad app connects to it with nothing Mac-specific in it. Built by the Mac session; the Linux session supplies the core's host interface for Swift.
**Done when:** the iPad app lists both the PC and the Mac and controls either at native resolution and 120 fps, and the Linux test client can control the Mac too (which proves any client works with any host).

### 3. Remote login
`remotemand`, the GDM broker, login screen to session inside one connection, the resume token, unlock and re-lock for attach, the Arch package.
**Done when:** from a PC sitting at its login screen you reach a desktop on the iPad; a dropped connection lands back in the same session in under 1 s; attaching to a locked local session works and re-locks when you leave.

### 4. Picture and resilience
Our own Vulkan conversion, direct NVENC, re-requests and parity, the rate controller, static refinement, 4:4:4 if S2 and S3 passed, rotation and resizing.
**Done when:** 2% random loss shows no stall and puts no keyframe on the wire; the bitrate follows a throttled link within 1 s; an idle desktop sends under 100 kbit/s.

### 5. The rest of daily use
Sound, clipboard, soft keyboard and text input, the key bar and remaps, iPhone trackpad mode and zoom, Wake-on-LAN.

### 6. HDR
End to end in mirror mode first: PQ in, EDR out. Then the Mutter patch: virtual outputs that offer BT.2100 with the device's luminance, and virtual streams that offer the 10-bit PQ formats. Run it as a local package and send it upstream.

### 7. More displays and devices
A second virtual monitor on a display plugged into the iPad, Pencil as far as libei allows, files and drag and drop, microphone. **[R]** An app can fill a connected display with a scene of its own and pick its mode. Whether it can pick the refresh rate and HDR there wasn't found.

### Later
- **KDE.** Attach mode through the desktop portals first (ScreenCast with a virtual source, RemoteDesktop, libei), then KWin's own protocol (`stream_virtual_output`) for unattended use. **[R]** As of Plasma 6.8.3: virtual outputs exist and follow the stream's size; they start at 60 Hz and take custom modes; the HiDPI scale tag comes in 6.9; no HDR on them and screencasts are 8-bit (HDR screencast is open for 6.9); unattended access works by pre-authorising the app or a restore token. Remote login isn't there in 6.8 and is planned for 6.9 with Plasma Login Manager.
- **How others stand** **[R]**: Sunshine has no virtual displays on Linux (the request was closed as not planned) and uses FFmpeg for NVENC there. Nothing found combines virtual displays, a desktop session and HDR on Linux.
- **Outside the LAN** without Tailscale: a rendezvous and relay of our own.
- **More hosts and clients:** Windows host; Mac, Linux, Windows and Android clients. Each is a new shell around the same core.

## Risks

1. **Private GNOME APIs move.** GDM's remote display API changed in 49, 50 and 51 **[V]** notes. The GDM and Mutter code stays small, version-checked, in one crate.
2. **HDR on virtual displays needs a Mutter change that doesn't exist yet** **[V]**. **[R]** Nobody upstream has proposed it, for GNOME 52 or otherwise, so the patch would be ours from scratch. A colour-management rework (!5177) is open for 52 and could move the ground under it.
2a. **Attach may meet an inactive seat.** **[R]** Mutter !5353, still open, is what lets a virtual monitor render while the seat is inactive. S6 checks whether attach needs it.
3. **4:4:4 at 120 Hz is unproven at both ends** (S2, S3).
4. **The zero-copy path is NVIDIA-only at first.** AMD and Intel come with VA-API or Vulkan Video.
5. **One session per user is GNOME's rule.** A headless session and a local login for the same user exclude each other.
6. **iPadOS keeps some keyboard shortcuts** **[R]**.
7. **Pencil pressure has no path** in a headless session **[V]**.
8. **A login path that faces the network.** Mitigated by paired keys before anything else, an unprivileged sandboxed daemon, and throttling.
9. **Key-only login leaves the keyring locked** (decision 3).
