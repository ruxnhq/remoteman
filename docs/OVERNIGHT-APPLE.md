# The night of 2026-11-18: the Apple side

The Mac session's record for `docs/OVERNIGHT.md`, stream 5. Branch `apple/phase1-client`. Times are
the Mac's clock (PST), which was about three minutes behind the PC's that night. A file of its
own so that it can't collide with the main session's edits to `docs/PLAN.md`.

## Fixed, each with what showed it

- **The stream stopped decoding on FFmpeg every 2048 frames (the owner's "hangs for half a
  second").** Found by the main session in a recording. VideoToolbox's HEVC encoder wraps its
  picture count every 2048 frames, and the frame at the wrap names four reference pictures where
  every other names one; FFmpeg has dropped three of them and decodes nothing more until a
  keyframe. Apple's decoder lets it pass, so no Apple viewer ever showed it. The host now forces a
  keyframe every 2000 frames (first and second display). *Test:* the host's stream dumped with
  `--dump`, 1600×1000 at 144 Hz: without, `ffmpeg -f null` decodes exactly 2048 frames and prints
  1300 error lines; with, all 2706 frames and none. From the PC, 158 s at 3840×2160: 0 keyframe
  requests (12 in four minutes before), pictures out equal to frames in.
- **The last frame of a burst could be lost.** The encoder takes two frames at a time and a third
  was dropped. ScreenCaptureKit sends nothing while the screen is still, so when the dropped
  frame was the last of a burst the device kept an old picture until something else changed. The
  newest dropped frame now waits and goes in when a frame comes out, unless a newer one got in
  first. *Test:* a window moving for 0.4 s and still for 1.1 s, 3840×2160 at 120 Hz: 1 stale
  ending in 30 s and 1 in 90 s before, 0 after. Side effect, measured from the PC: 107 frames a
  second at 3840×2160 where it was about 90, for about 1.5 ms more age.
- **A change of size removed the virtual display and made a new one.** A configure that changes
  only size, refresh or 1×/2× now changes the display in place (extra or main placement, within
  the size the display was made for). *Test:* three changes in one session, same display id, the
  picture back 0.5 s after the configure (1.25 s for 2× as main); clicks at 0.5, 0.5 and
  0.25, 0.75 land there after a change.
- From a review of the host and the viewer session (a reader, then a second pass to disprove):
  - a second display given back and asked for again at once lost the new display (*tested:* twice
    in one session, pictures both times);
  - the first frame after a configure restart could be a delta (*tested:* a keyframe after each of
    three restarts);
  - frames from a capture that was being stopped could reach the new encoder at the old size
    (by reading; ignored now);
  - a capture that ScreenCaptureKit stops left the device with a frozen picture; the host now
    drops the device (*not tested:* needs a hand on "Stop Sharing");
  - after a retry for "logging in", the viewer showed sound and clipboard on without asking the
    host for them (by reading);
  - a session the host ended leaked the viewer's core client (by reading);
  - stopping the host left a running camera (by reading).

## Measured

- **Where the Mac host's time goes.** ScreenCaptureKit hands a frame over 0.3 to 5 ms *before*
  its display time; `capture_us` is that display time. The encoder is everything else: 15.3 ms a
  frame at 3840×2160 when nearly still, 17 to 18.5 ms busy; 8.8 ms at 2560×1440; 5.7 ms at
  1600×1000 (these last two with my own viewer decoding on the same Mac). Time in
  `rm_host_send_frame`: 0.02 ms, longest 0.23. Time in `on_input`: under 2 ms.
- **Encoder settings tried at 3840×2160, none better than what ships** (ordinary rate control,
  not real-time, no reordering, speed over quality): real-time the same; low-latency rate control
  1.65× slower; 8-bit the same; one frame in flight slower overall.
- **Espalier is the same capture and encoder** at 1250×780 or 1600×1000; it feels faster because
  it sends a fifth to an eighth of the pixels.
- **Capture pacing** (`--pace yes`: capture at a half or a third of the rate asked when the
  encoder can't keep up): an even 40 frames a second against 107 with a 28 ms p99 gap without
  it. Left off.
- **Recovery after a loss without a whole keyframe.** VideoToolbox offers long-term reference
  frames (`EnableLTR`, acknowledged tokens) only under the low-latency rate control; under the
  ordinary one `EnableLTR` and `ReferenceBufferCount` are refused (-12900). Low-latency costs
  5.7 → 9.4 ms at 1600×1000 and 8.8 → 15.0 ms at 2560×1440, on every frame, to save a 48 KB
  keyframe on the rare loss. Not built. It would be worth it only on a link that loses often.
- **A locked Mac shows a device the lock screen's moving wallpaper**: the whole picture changes
  30 times a second, 180 KB a frame at 3840×2160, 38 Mbit/s of the 40 allowed, and windows and
  input are under it. First taken for an animated wallpaper on new displays; it was the Mac's idle
  lock (locked at 23:49, and during two of my earlier runs). The everyday host unlocks for the
  owner's paired devices; a test host started with `--no-unlock` does not, so the Windows viewer's
  first sessions (00:00) saw only that wallpaper and its clicks went to the lock screen.

## New

- Picture size per computer in the viewers: this screen's own, up to 1440 or 1080 lines (the
  shorter side); on the Mac also the window's size as it changes (one configure a second after the
  window stops moving). *Tested:* the Mac's following, width only. *Not tested:* the 1440 and
  1080 choices in a session, and the pickers by hand.
- The host's once-a-second line: capture ms, encode ms, age when sent, time in send, largest
  frame, keyframes sent, longest wait for the screen, dropped / sent late / left stale, frames the
  core did not take, keyframe requests, input events and the longest. Once a session it says when
  it is asked for more than it encodes in a frame time.
- For measuring: `--log`, `--dump`, `--whole-every`, `--keep-last`, `--test-bursts`, `--encoder`,
  `--pace` on the host; `--follow-test`, `window:`, `picture:` in the viewer.

## Windows, each way (00:00 to 00:25)

- **Windows viewer to a Mac host** (a second Mac host on the LAN, port 7803, a key and state of
  its own; the everyday host was not touched): pairs (code 886 574 on both sides), HEVC Main10 at
  1920×1080 and 3840×2160, left clicks land at 0.500, 0.501 and 0.250, 0.751, a scroll and the
  keys r, m, 1 arrive, a configure to another size changes the display in place in 0.54 s, sound
  arrives (2872 packets in 30 s by its count, a test tone), 0 lost and 0 keyframe requests. Its
  first run lost 1 to 3 frames in the VM's network. Its name arrives as "W-".
- **Mac viewer to the Windows host** (over Tailscale; the PC's firewall drops the port from the
  LAN): pairs (code 905 071), AV1 4:2:0 10-bit at 1920×1080 decoded in hardware in 2.1 to 2.4 ms,
  HEVC when only HEVC is offered, 60 frames a second shown while the screen moves, age 12 ms, a
  right click opens the menu at the point aimed at, the Windows key and typed letters arrive in
  order, a configure to 1600×900 works. No sound packet reached the Mac viewer in three sessions
  (a silent VM, or not sent: not settled). The Windows host sends no bitrate status.
- *Not tried either way:* a second display, main and only placements, clipboard, files,
  microphone, camera, long runs.

## Went wrong

- My loopback tests ran over the owner's session (21:58 to 22:00) and over a test session from
  the PC (23:18 to 23:19): the encoder is shared, and theirs slowed to 23 to 30 ms a frame. Test
  hosts now log to a file of their own and the test script refuses to start while a device is
  connected to the everyday host.

## Still open, and what needs the owner

- No AV1 from the Mac: it has no AV1 encoder (hardware: HEVC, H.264, ProRes, JPEG).
- 3840×2160 at 120 Hz is past the M4's encoder: 107 frames a second at 18 ms each. Fewer pixels
  is the only thing that makes it quicker (Picture size).
- By hand, on real devices: rotation and Split View against the in-place resize on an iPad; the
  Picture size pickers; the iPhone did not take tonight's build (not reachable at 23:30; the iPad
  did).
- Everything listed as untested in `docs/PLAN.md` for the Apple side before tonight still is.
