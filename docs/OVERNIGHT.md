# The night of 2026-11-18: what gets built, fixed and tested

Written before the owner left for the night, at their request. The owner wants to find, in the
morning, everything built, fixed and tested. This file is the brief every session and agent
working tonight starts from. `docs/PLAN.md` has the history; `docs/HANDOFF.md` has where the
Windows work stopped.

## Rules for everyone working tonight

- Never type, send or use the owner's passwords. No sudo. Nothing that needs an administrator.
- Don't touch the owner's real desktop session, its lock, its keyring, its audio or its
  clipboard. Test on throwaway desktops (headless Mutter in `dbus-run-session`), as the e2e
  scripts do.
- The everyday host on this PC (`remotemand.service`, port 7791) is not to be restarted,
  replaced or connected to by test clients.
- Kill processes by exact pid only. Never `pkill -f` or `pgrep -f`.
- One writer per set of files. Each stream below owns the crates named for it.
- A change is done when it has a test that fails without it and passes with it, or a measured
  number before and after. Say what was not tested in the same breath as what was.
- Usage: the owner wants no limit hit before morning and no paid extra usage. Work in long
  steps, don't re-read what you have read, don't poll; stop and report rather than loop on
  something that isn't converging.
- Commits are signed and end with the Co-Authored-By line. Each stream commits to its own
  branch; the main session merges into `main`.
- Record findings and numbers in `docs/PLAN.md` as they are made.

## Stream 1: the Linux viewer is smooth (main session; crates/remoteman-viewer)

The owner's words: the stream "hangs for half a second then goes okay", and it is slow next to
espalier.

1. Measure each frame's path with one clock: received from the core, decoded, drawn, presented
   (GdkFrameTimings). Median and worst per ten seconds in the log.
2. Find the half-second stalls. Suspects, in order: something synchronous on the window's main
   loop (the Avahi browse in `nearby.rs`, the keyring, saving the store, the once-a-second
   figures); the decoder thread blocked behind the window; the Mac's encoder dropping frames at
   4K. Evidence first: a watchdog that logs any gap of more than 100 ms between two turns of the
   main loop, and between two pictures while frames are arriving, with what was running.
3. The ten frames in 120 that are still not shown at 4K.
4. Decide from 1 whether drawing outside GTK's frame clock is worth building; build it if a
   refresh or more is to be had, otherwise record the numbers and stop.
5. The captured pointer: drawn over the picture now; check it by picture, and that movement
   goes out as it arrives rather than once a frame.
6. Heavy input for minutes against the Mac (fast movement, clicks into a harmless window the
   Mac session opens for it, keys), on the real viewer on a throwaway 4K desktop: the test that
   was not run on 2026-11-18.

## Stream 2: every bug that reading and testing can find (cheaper agents, then the main session)

One reviewer per area, read-only, each returning findings with file and line and a failing
scenario; a second pass tries to disprove each; what survives is fixed by the owner of that
crate with a test.

- crates/remoteman-core (both roles, relay, ffi): losses, restarts, a second display, shutdown.
- crates/remoteman-agent and remoteman-desktop: sessions, placement, unlock, clipboard, files.
- crates/remoteman-daemon: pairing, the login screen, handing a session over, the owner's `ctl`.
- crates/remoteman-viewer: sessions ending and starting, settings, files, the second window.
- crates/remoteman-tray and the scripts in `scripts/` and `packaging/`.

Then the whole e2e suite, twice in a row, on the final `main`, with every flake run down.

## Stream 3: what is missing on Linux (one session; crates/remoteman-daemon)

- Each device belonging to a user of its own in the daemon, and a review of the daemon for bugs.
- No AppImages and no Arch package tonight: the owner said to wait for their word.
- HDR and variable refresh stay parked: they wait on a change to Mutter.

## Stream 4: Windows (two sessions in parallel, as `docs/HANDOFF.md` lays out)

Decided by the owner tonight: both at once; the viewer ships as `remoteman.exe`; the host's
virtual display is Guesthouse's user-mode display driver under a per-machine self-signed
certificate (no paid signing, as decided on 2026-11-18), and sound and microphone go through
Valve's signed Steam Streaming devices.

- The viewer's interface (`wip/windows-viewer-ui`), in `crates/remoteman-viewer-windows`.
- The host's second slice (branch `worktree-agent-af375683c4a419185`), in
  `crates/remoteman-host-windows` and the Direct3D input of `crates/remoteman-encode`.
- Built and run in the VM at `/mnt/data/vms/remoteman-win`. One user of the VM at a time:
  `tools/vm.sh take NAME` before, `give NAME` after, and never hold it while only building.
- Streams tested across systems, every pair, each way: Windows host to the Linux viewer and
  test client; Linux host (a throwaway one, `scripts/headless-host.sh`) to the Windows viewer;
  Windows host to the Mac's viewer and the Mac's host to the Windows viewer. The Mac pairs are
  arranged by the main session with the Mac session: tell the main session when a side can
  connect.
- Both merged into `main` when they also build on Linux (`cargo build --release --locked`).
- Not possible tonight: a real Windows PC with a screen.

## Stream 6: every GPU maker (one session on Linux; the Windows sessions for Windows)

The owner, leaving: "don't forget that Nvidia/amd/intel should all work besides Mac and iOS".
Today the Linux host encodes with NVENC only; the Linux viewer decodes with NVDEC, VA-API,
Vulkan or in software. So:

- Linux host: an encoder path for AMD and Intel (VA-API: HEVC, and AV1 where the card has it)
  beside NVENC, chosen by what the session's GPU is, with the same loss recovery where the
  encoder allows it and whole keyframes where it doesn't. This PC has an AMD Radeon 610M beside
  the RTX 5090: test on it. Intel has no hardware here: build it on the same VA-API path, and
  say plainly that it has not run on Intel.
- Linux viewer: check decode on the AMD card (VA-API) with the picture shown, not only decoded.
- Windows host and viewer: no NVIDIA-only assumptions. Decode through Direct3D 11 video
  (any maker); encode through NVENC where there is NVIDIA and through the maker's own encoder
  (AMF, Quick Sync) or Media Foundation's hardware encoders otherwise. The VM has the RTX 5090
  only, so say what could not be run.

## Stream 5: Apple (the Mac session, on its own machine)

Asked of it: its own list in the same spirit (bugs by review, what is untested, recovery after
loss without a whole keyframe, the Mac viewer's and iPad's rough edges), a heavy-input window
for stream 1's test, and its figures for any second the main session names.

## In the morning

A report in `docs/PLAN.md` and in the main session: what was built, what was fixed with what
test, what was measured, what failed, and what still needs the owner's own hands (sudo
installs, real screens, real devices).
