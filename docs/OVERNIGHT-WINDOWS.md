# The night of 2026-11-20: Windows host and viewer, and what every host shares

The owner, about 03:30 PST: work on everything for the Windows host and viewer and what affects
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
| Conceal lost sound packets (Opus PLC, in-band FEC) | in work |
| Windows key mapping (`key_mapping`, greyed) | not started |
| "Getting in" choice (`way_in`, greyed) | not started |
| The host's cursor shape in a window | not started |
| HEVC 4:4:4 decoded on the card (Y410 drawn) | not started |
| Variable refresh in a window | not started (feasibility first) |
| Full screen's menus off the old Direct2D kit | not started |

### Windows host
| Item | State |
|---|---|
| Mirroring the PC's own screens | not started |
| HDR mastering and light-level metadata; HEVC 4:4:4 in HDR | not started |
| The driver unplugs a device's monitor when it leaves (`DESTROYMONITOR`) | not started (build only) |
| A display scale for the device's monitor | not started |
| Steam's sound devices from the user's Steam, speakers switched while a device listens | not started |
| A camera for the PC (virtual camera) | not started |
| AMD and Intel encoders with loss recovery | **not testable here** (the Radeon iGPU has no driver; nothing to be installed) |

### Every host (the shared core, `remoteman-core`)
| Item | State |
|---|---|
| Bitrate following the network | not started |
| Parity for lost fragments | not started |
| Static refinement of a still picture | not started |
| Per-device permissions, view-only | not started |
| A log of connections, and a notice | not started |
| Keeping the host awake while connected, locking it on disconnect | not started |
| A second factor for unattended access | not started (needs a design) |
| Game controllers | not started |
| More than one viewer at once | not started |

### Apple (the Mac session)
Asked at 03:35 PST: the Mac host's loss recovery without a keyframe; the Mac and iPad viewers
presenting each frame as it arrives; the Mac viewer's HDR menu item; Apple Pencil (a pen event in
the protocol, the Windows host injecting it). Its share of the budget: about 5 points.
