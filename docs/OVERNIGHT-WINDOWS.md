# The night of 2026-11-20: Windows host and viewer, and what every host shares

The owner, about 23:30 PST on 2026-11-19: work on everything for the Windows host and viewer and what affects
every host, except a relay, printing, recording and the Store release; also the Mac host, the
Mac viewer and the Mac and iPad viewers' items (asked of the Mac session). Back in 7-8 hours.
Use cheap agents where they'll do; weekly usage was 81%, the ceiling is 98%.

Usage at the start: weekly (all models) 81%, 5-hour 3%, extra usage $30.19. New work stops at
about 95%.

Each item ends as one of: **done and verified** (how), **built, not verified** (why), or **not
started**. Agents work in worktrees and commit there; merged into `main` here after review.

## Rules for the night
- No installs and nothing elevated (no UAC): the driver, the service and the installer are
  built only; one installer and a checklist are left for the morning.
- Nothing connects to the installed host (7791); its sharing stays off. No displays added,
  removed or moved on the real desktop. The installed viewer is backed up before it is replaced.
- No pictures of the owner's screens. Mirroring the PC's own screens is checked by frame counts
  only (no `--record`, no `--shot`); HDR metadata on synthetic test streams only.
- New protocol is negotiated in the hello and off otherwise: the owner's installed iPad and Mac
  apps and hosts are older builds.
- One writer at a time in `remoteman-core`, `docs/PROTOCOL.md` and the viewers' `features.rs`.

## Items

### Windows viewer
| Item | State |
|---|---|
| Conceal lost sound packets (Opus PLC, in-band FEC) | **done, unit-tested** (c991c14): libopus replaces FFmpeg for sound; gaps from sequence numbers, up to 100 ms made up, the next packet's FEC copy used; tests with a synthetic sine and 1-in-10 loss. Not heard through WASAPI |
| Windows key mapping (`key_mapping`, greyed) | **done, unit-tested** (72f7998): a "Windows key" stand-in (Right Alt, Right Ctrl or Caps Lock act as Super, as in the Linux viewer), before the Mac Ctrl/Command swap; the row seen through UI Automation. No keys sent to a host (the Mac test host is locked) |
| "Getting in" choice (`way_in`, greyed) | **done, unit-tested** (72f7998): per computer, "The computer decides" or "Only a session that is already running" (`attach_only`); only the Linux daemon has more than one way in; not run against it |
| The host's cursor shape in a window | **done, checked** (6a8c125): worn over the picture through `WM_SETCURSOR` on the app's windows; against the Mac test host, `GetCursorInfo` read the host's 56x80 cursor (hotspot 10,10) over the picture and the system arrow elsewhere. Hiding it (width 0) not seen |
| HEVC 4:4:4 decoded on the card | **done on NVIDIA** (b01fc1a): FFmpeg's NVDEC decodes it and the planes are copied on the card into the drawing's textures: 4K in about 5 ms a picture against 13-16 on the processor; matches the processor's picture to 1 code value. Other cards stay on the processor (FFmpeg offers no D3D11 4:4:4). Y410/AYUV drawing is there for when it does |
| Variable refresh in a window | **not testable tonight**: the only monitor connected is the ROG, which has no variable refresh (full screen doesn't vary on it either); the XG32UCWMGs are off. To run with one connected: `--play` a 100 fps clip with `--watch-vblank`, full screen first (10.00 ms), then a flip-model child window over the panel |
| Full screen's menus off the old Direct2D kit | **done, checked through UI Automation** (61704ca): full screen's bar is the app's own WinUI bar in a borderless topmost window of its own over the picture (shown at the top edge, hidden 2 s later), so its items are the window's by construction; toggles write the settings. Not seen by eye; variable refresh keeping its own plane not measured (the ROG has none); touch and keyboard in its menus not tried. `--kit-bar` brings the old bar back. Full screen's pages (Settings, About) are still the old kit's |

### Windows host
| Item | State |
|---|---|
| Mirroring the PC's own screens | **done, run unelevated** (2387e21): Desktop Duplication of the PC's main screen when a device asks for `mirror`; a test host (`--test-mirror-only`, port 7795) streamed this PC's 3840x2160 screen to `watch` (278 frames in 25 s, 0 lost, age 4.8 ms) — frame counts only, nothing saved. Not run: HDR (FP16), the lock screen/UAC as SYSTEM, the cursor channel, switching mirror/extend mid-session |
| HDR mastering and light-level metadata; HEVC 4:4:4 in HDR | **done at the encoder** (11a7e33): ST 2086 + MaxCLL/MaxFALL on keyframes (HEVC SEI; AV1 metadata OBUs written by us, NVENC's own fields gave nothing); HEVC 4:4:4 PQ allowed. ffprobe shows the side data on synthetic streams of all three. Not run in a session |
| The driver unplugs a device's monitor when it leaves (`DESTROYMONITOR`) | **built, not installed** (59db529): `DESTROYMONITOR n` departs the monitor, new `PLUGMONITOR n` brings it back with the same identity; the host plugs a device's monitor in when it comes and unplugs it when it leaves (PC with screens only); driver 0.1.1.0. Morning test plan in the merge's branch report (below) |
| A display scale for the device's monitor | **built** (92cb5b2): after the device's monitor is up, its Windows scale is set to the step nearest the device's `scale` (`DisplayConfigSetDeviceInfo`, -4), never on the PC's own screens or when mirroring. The GET was read on this PC (the ROG: 100-350 %, 300 % recommended); the SET needs the driver's monitor (elevated), not run |
| Steam's sound devices from the user's Steam, speakers switched while a device listens | **built** (7564e3c): the installer looks for Steam's devices and says how to get them (Steam packs the drivers inside `drivers.exe`, nothing to give pnputil); while a device listens the default output becomes Steam Streaming Speakers and is put back after (`speakers.json` survives a crash), switchable. Not run (no Steam devices on this PC; no default device changed) |
| A camera for the PC (virtual camera) | **built** (a9d240e): `remoteman-vcam-windows`, a media source DLL the installer registers (HKLM), fed through shared memory by the host, which decodes the device's H.264 with Windows' own decoder; `camera` offered only when registered. Tested in-process (black, then test frames, then black); Windows' camera service refused a per-user registration, so the real path waits for an install |
| AMD and Intel encoders with loss recovery | **built, run on NVIDIA only** (fc9670c): a Direct3D 12 HEVC encoder (`--encoder d3d12`, not the default): recovery without a keyframe from kept pictures (frames 60-62 cut, 117 decode clean), the rate changed live without an IDR, 4.7 ms a frame. Needs a run on an AMD and an Intel card before it becomes their default; AV1 not done |

### Every host (the shared core, `remoteman-core`)
| Item | State |
|---|---|
| Bitrate following the network | **done, loopback-tested** (defe4fd): the core's host cuts ×0.8 on a loss report, climbs ~6 %/s after 2 s clean, up to the device's limit; the Windows host applies it live; the Mac host through `rm_host_set_bitrate_callback` (the Mac session wires it once `main` is pushed). Not run with a real host |
| Parity for lost fragments | **done, loopback-tested** (defe4fd): datagram kind 7, XOR over groups of 10 fragments, asked for in the hello (the core's client always asks); every host built with this core sends it. One datagram in 19 dropped: 30 frames of 20 kB came whole. Not run with a real host |
| Static refinement of a still picture | **Mac host: done, measured** (Mac session, 6c21c9f on `apple/phase1-client`: after 0.4 s still, the last frame again at 8x the bitrate and quality 1.0; a 4K page of small text from 24.3 to 28.6 dB PSNR; `--refine no`). **Windows host: done at the encoder** (e50a609): after 0.4 s still, the last frame again at constant QP (24/28/32 by picture size) as a P-frame, then the rate back; synthetic text 14.6 → 38.5 dB, the next frame 40 bytes keeping it. Not run in a session |
| Per-device permissions, view-only | **done, loopback-tested** (5d1a537): `input`, `clipboard`, `files`, `sound`, `mic`, `camera` per device (`permissions.json`), enforced in the core for every host (features filtered per device, everything not allowed dropped); `ctl device NAME view-only on`, tray toggles per device; the Windows viewer sends nothing and says "View only". The tray and viewer were compiled, not run |
| A log of connections, and a notice | **done, loopback-tested** (5d1a537): `connections.log` (JSON lines: paired, connected, left with duration, refused), `ctl connections`, the tray's "Recent connections" and a notification when a device connects ("Tell me when a device connects", on) |
| Keeping the host awake while connected, locking it on disconnect | Mac host: already there (a display-sleep assertion per session; "Lock this Mac when the device leaves", off by default). **Windows host: built** (7564e3c): a power request (system + display) held per session; "Lock this PC when a device disconnects" (off by default) in `ctl set lock` and the tray. The power request was made and released in a test; the lock not run |
| A second factor for unattended access | **done, loopback-tested** (5d76107): off by default; when on, a paired device is asked for an authenticator app's code after its key (`second_factor` in the hello), 5 wrong in 10 min locks that device out for 10 min, "Don't ask again on this device for 30 days"; `ctl second-factor on/confirm/off/forget`, a Security section in the tray (the key in groups of four, no QR code), the viewer's dialog. Also: a new device asking to pair with nobody signed in is refused after 30 s. Tray and viewer dialogs compiled, not run |
| Game controllers | **built, host side off** (d5ba32c, ecbbf1a): input event 10 (XInput's buttons, triggers, sticks), the Windows viewer reads XInput controllers while its window is in front. The host's injection (Windows' InputInjector) returned OK but no reader ever saw the virtual pad, so the host doesn't offer `gamepad` unless `REMOTEMAN_GAMEPAD=1`. A working Windows side needs a virtual controller driver |
| Apple Pencil and Windows pens (input event 9, the Mac session's format) | **built, loopback-tested** (cef3e0c): event 9 in the protocol and core (`rm_client_pen`, `rm_input` grows at its end), the Windows host injects a synthetic pen (made and destroyed in a test, nothing injected). The iPad side and the Mac host's tablet events wait for the push. The Windows viewer doesn't send pen yet |
| More than one viewer at once | in work: a host setting (takes over, as today / watches / can control too); a joiner gets the lead's stream |

### Apple (the Mac session)
Asked at 23:35 PST; its share of the budget about 5 points. Its answer:
- The Mac host's loss recovery without a keyframe (long-term references): **left as measured**.
  VideoToolbox gives them only under its low-latency rate control, which costs 5.7-15 ms on
  every frame to save a keyframe on the rare loss; parity covers loss more cheaply.
- The Mac and iPad viewers presenting each frame as it arrives: **built and measured earlier,
  no faster** (iPad median 15.0 against 14.3 ms); the next idea (CAMetalDisplayLink) was put
  "later" by the owner (docs/OVERNIGHT-APPLE.md).
- The Mac viewer's HDR menu item: **done** on `apple/phase1-client` (6b1e619).
- Apple Pencil: its proposed format (feature `pen`, input event 9: phase, buttons, x, y,
  pressure, tilt x and y, twist) goes into PROTOCOL.md and the core here; the Mac host can post
  tablet events once the core has it; the Windows host injects it as pen input.
- Also on its list: a quick reconnect refused after 15 s, and a harmless error after resizes
  (Mac host).

## Side effects
- Python 3.15 was installed per-user (`%LOCALAPPDATA%\Python\pythoncore-3.15-64`, 23:32) the first
  time `python` was run tonight, by the Python install manager, which the owner had installed
  for this work (21:23, from the Store). Nothing else was installed.

## Notes
- `main` is not pushed (the owner's rule), so the Mac session can't merge tonight's core changes
  until the owner pushes; it builds and wires them (`rm_host_set_bitrate_callback`, the pen) then.
- The Mac locked itself at 23:28; its test host 7803 shows the lock screen, so nothing was typed
  or clicked into it tonight.

## What stopped the night
Windows Update restarted the PC at 00:09 PST (`MoUsoCoreWorker`, event 1074), and with it this
session and its five running agents; nothing ran from then until 07:30. In flight then: the
virtual camera, the Direct3D 12 encoder, NVDEC for 4:4:4, the second factor and full screen's
menus. Not reached: more than one viewer at once.

## The morning
At 07:40 the driver's 3200x2400 monitor was Windows' main display at 0,0 with no device
connected and sharing off, the PC's three screens left of it: Windows had rebuilt the desktop at
the 00:09 restart (the XG32UCWMGs asleep and disconnected then), and the installed (old) host
doesn't take the driver's monitors off. `C:
m	oolsix-screens.ps1` put DISPLAY1 back at 0,0 as
the main screen. The new host's watch between devices does this by itself.

## Installing the new host (for the owner)
`C:\rm\installer-2026-11-20-main\remoteman-host-setup-0.1.1.exe`, built from a9d240e (host 0.1.1,
the display driver 0.1.1.0 that unplugs its monitors between devices, the virtual camera's DLL).
One UAC prompt.
1. With all three screens on, run it.
2. Settings > System > Display should list only the PC's three screens (no remoteman display).
3. Turn sharing on in Remoteman Host (the tray app).
4. From the iPad: an extra display (the default placement); the picture comes; disconnect; the
   screens are as before and Settings still lists only the three.
5. From the iPad: Mirror shows the PC's main screen as it is.
6. Not yet "Main display" or "Only display": the layout test (`--layout-test main`, `back`) runs
   first with all three screens on.
7. If the screens go wrong: Win+R, `powershell -ExecutionPolicy Bypass -File C:\rm\tools\fix-screens.ps1`.
New in the tray: per-device switches (view only, clipboard, files, sound, microphone, camera),
recent connections and a notice when a device connects, "Lock this PC when a device disconnects"
(off), "Silence this PC's speakers while a device listens" (needs Steam's sound devices).
