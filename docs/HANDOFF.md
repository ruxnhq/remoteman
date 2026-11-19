# Handoff: the Windows host and viewer

Written 2026-11-18 about 09:15 PST, when the owner paused the work to change accounts. Everything was stopped cleanly: no builder, VM or test host is running. `docs/PLAN.md`, section "Windows host and viewer (started 2026-11-18)", has the findings and numbers; this file says where the work stopped and how to pick it up. Check anything here against the machine before relying on it.

## What the owner asked for, in order

1. Plan the Windows host and viewer, with a design of the interface first. Done: the design was approved ("design is fine").
2. Research AMD and Intel GPU support, and whether the Windows apps can be built and tested in the VMs of the kvm-gpupv project. Both done; in the plan.
3. "Yes setup vm", then "Install whatever u need. If it helps we can install it on host too". Done.
4. "Yes start viewer first then host don't stop". In work when paused.
5. "can u pause the work", then "keep everything in handoff". This file.

The owner also pointed out that Guesthouse has a display driver that might help (it does: see below) and that this PC has an AMD iGPU (a Radeon 610M) that could help with AMD work.

## The design

The canvas is at https://claude.ai/artifact/7kX4Xi9zT5ov3pYYqrqAgb, but it is private to the account that made it. Its sources are in `docs/design/windows/` (13 artboards as `.dc.html` and `canvas.json`): each file's `<style>` block has the colour tokens for light and dark and the control metrics, and the markup has the exact wording. Nobody has checked it rendered. It was drawn from the wording of the Linux viewer, the Mac host and the iPad app; the three earlier design pages were not found.

## What is committed and verified

On `main`:
- `328d708`: `crates/remoteman-core/examples/watch.rs`, a screenless client of the core alone (it builds wherever the core does), and `scripts/headless-host.sh`, a throwaway Linux host on 127.0.0.1:7799 for a viewer elsewhere.
- `61f2859`: `crates/remoteman-viewer-windows`, the viewer's first slice (`remoteman-windows.exe`). Verified by the main session in the VM.
- `927bf37` and this commit: the plan and this handoff.

On branch `worktree-agent-af375683c4a419185` (worktree `.claude/worktrees/agent-af375683c4a419185`), not merged into `main`:
- `b4778ea` to `ea249fc`: `crates/remoteman-host-windows` (`remoteman-host.exe`) and a Direct3D 11 input for `crates/remoteman-encode`. The first slice, verified: it streams the VM's monitor, and a Linux client on this PC connected to it across the VM's NAT.

## What was stopped partway (may not build)

- **`wip/windows-viewer-ui`, commit `19b06c4`** (one commit on top of `main` at `927bf37`'s parent): the viewer's designed interface, about 4,600 lines: a Direct2D and DirectWrite kit of our own in `src/ui/`, `app.rs`, `pages.rs`, `menus.rs`, and `store.rs`, `status.rs`, `features.rs` ported from the Linux viewer. The builder had just said "Now the Linux viewer's logic" when it was stopped. Nothing of it has been reviewed, and it has not been built since the builder's own last check.
- **`worktree-agent-af375683c4a419185`, commit `53a3d11`**: the start of the host's second slice, 32 lines across the NVENC shim and wrapper, `pipeline.rs` and the Linux test client. Barely begun.

The builders were subagents of the session that wrote this; another account can't resume them. Their briefs, for whoever continues:

**Viewer interface** (in `crates/remoteman-viewer-windows` only). Decided: drawn by ourselves with Direct2D and DirectWrite through the `windows` crate, no toolkit, no second language, nothing beyond MIT/Apache; text fields may be Win32 EDIT controls if that is much cheaper. Order: (1) the kit: layout in device-independent pixels, text, tokens for both themes, button, toggle, radio row, drop-down, card with rows, menu and flyout, dialog, text field, progress ring, tag, scrolling, hover/pressed/focused/disabled, keyboard focus, tooltips, icons from the design's SVG paths; (2) the computers list; (3) pairing by address (the six-digit code step built but gated); (4) connecting with the core's four stages and the failure pages, classified as the Linux viewer's `status.rs` does; (5) the session: our own title-bar command bar with correct hit-testing, the same bar floating in full screen, the figures panel, and the menus that work today (Send keys, Display placement, Picture quality and bitrate limit, Show figures, full screen, Disconnect), asking the host for a new size on resize; (6) Settings and settings for one computer. Controls for what the product can't do yet stay where the design has them, greyed out with "Not available yet", switched on in one file. Checks: a `--ui STEPS --shots DIR` walk that saves a PNG per screen and state, each looked at beside the design; make the exe a GUI-subsystem program. Things to check, not trust: Direct2D needs the device made with BGRA support and may not draw into the 10-bit back buffer (draw the interface into a BGRA8 texture and composite it, or another arrangement that keeps the video 10-bit and copy-free); `Present` must not wait inside the device lock.

**Host, second slice** (in the worktree branch). (1) Close what the first runs left open, using the Linux test client from the PC (`target/release/remoteman-testclient 127.0.0.1:7801 ...`, host listening on 0.0.0.0:7791): a click, the wheel and its pixels-per-notch scale against the Linux host, relative motion, text outside the layout, a live bitrate change and a size change through `configure`, dropped-datagram runs at several rates (one in 40 gave the client nothing: explain it). (2) HEVC used 1 MB where AV1 used 28 MB for the same ten seconds at a 40 Mbit/s target: find out why and fix it if it is ours. (3) A client that doesn't ask for the cursor channel sees no pointer: draw it into the picture. (4) Clipboard and files as the Linux host has them, behind `--allow clipboard,files`. (5) Sound through WASAPI loopback and Opus (the Linux crate links the system's libopus; Windows needs another route that still cross-builds). Later: per-device monitors (needs our own fork of the driver; in a Guesthouse guest the monitor is made at start and `DESTROYMONITOR` is ignored), HDR, the tray interface to the design (from the viewer's kit, which should then become a shared crate), a service for the sign-in screen and Windows' own prompts.

After both: merge the host branch and the interface branch into `main` (both change `Cargo.lock`), confirm `cargo build --release --locked` on Linux, and record it in the plan.

## The VM and the tools

- `/mnt/data/vms/remoteman-win`: remoteman's own Windows 11 VM, a reflink copy of Guesthouse's test base `offload-test`, on lab slot 2, off. Its claim is a row in `~/.cache/kvm-gpupv-main/notes/handoff.md` (local commits `750ff791`, `79daef9c`, not pushed, with no AI attribution, as that repo's rules ask). The owner's `windows-11` and `offload-test` were never touched.
- `tools/vm.sh take NAME | give NAME | start | stop | run 'POWERSHELL' [SECONDS] | put FILE... | get GUESTPATH LOCAL`: one user of the VM at a time through take and give; `run` is SYSTEM in session 0, one command at a time; `put` lands files in `C:\rm`; `get` prints a harmless "base64: invalid input". Each start holds 16 GiB with the lab for 3 minutes, so a quick restart waits.
- `tools/desk.py 'POWERSHELL' [SECONDS] [--shot LOCAL.png] [--shot-at S]`: runs a snippet in the guest's logged-in desktop session (user `gpupv`), which a window with a swap chain, `SendInput` and the host need. No output comes back: use `--log` and files. It was edited after its last run and not run since.
- In the guest: Rust 1.100 in `C:\rust`, Visual Studio Build Tools 2022 (C++ and the Windows SDK), FFmpeg 9.0 LGPL shared in `C:\ffmpeg`, work files and logs in `C:\rm`. A firewall rule "remoteman-host" allows UDP 7791 in for `C:\rm\remoteman-host.exe`. No git, no WDK.
- A UDP forward: this PC's 127.0.0.1:7801 reaches the guest's 7791. The guest reaches this PC's loopback as 10.0.2.2.
- On this PC: the `x86_64-pc-windows-msvc` target and `cargo-xwin` 0.23.1 (Microsoft's CRT and SDK libraries, licence accepted with `XWIN_ACCEPT_LICENSE=1`), and the same FFmpeg build at `/mnt/data/vms/remoteman-win/ffmpeg`.
- Builds: `FFMPEG_DIR=/mnt/data/vms/remoteman-win/ffmpeg XWIN_ACCEPT_LICENSE=1 cargo xwin build --release --locked --target x86_64-pc-windows-msvc -p remoteman-viewer-windows` (and `-p remoteman-host-windows`, which needs no FFmpeg). Give `FFMPEG_DIR` inline and never export it: a Linux build with it set rebuilds the Linux programs against the Windows FFmpeg.
- Evidence: `/mnt/data/vms/remoteman-win/out/` (the viewer's screenshots) and `out/host/` (frames decoded from the Windows host's stream; `from-linux-client.png` is the cross-machine one).

## Rules that held throughout

- Never test against the everyday Linux host on port 7791: it would add a monitor to the owner's real desktop. Use `scripts/headless-host.sh` (it pairs only with the first unknown device after a start with a fresh `RM_LOGS`).
- Guest GPU work runs on the same RTX 5090 as the live Linux host, so VM runs stay out of the Mac session's measuring windows. It was not running on 2026-11-18, so it got no notice of the day's runs.
- The lab's rules for the VMs: no sudo, only our own copy, kill only what you started and by pid, nothing edited under `~/.cache/kvm-gpupv-main` beyond the claim row.
- Another session of the owner's had the `intel-replay` VM running during this work; it was left alone.

## Open questions for the owner

- The viewer's program is `remoteman-windows.exe`, because `remoteman` is the Linux viewer's name in a workspace build. Packaging can ship it as `remoteman.exe`, or the name can be settled otherwise.
- Signing the display driver for real PCs. The self-signed, per-machine route was only ever meant for VMs.
- Whether to make a second Windows VM on the Radeon 610M for AMD work (the lab has run Windows guests on it; whether hardware encode works in one was not looked into).
- Nothing of today's work is pushed.
