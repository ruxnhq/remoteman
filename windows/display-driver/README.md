# remoteman's display driver

An indirect display driver (IddCx, user mode) that gives remoteman's Windows host the monitors it streams: a device that connects gets a display of its own size and refresh rate, the way the Linux host gets one from GNOME and the Mac host from macOS. Windows has no built-in way for an app to add a monitor, so remoteman carries this driver.

## Where it comes from

It's a fork of Guesthouse's `drivers/windows-display` at v0.7.0 (commit 865e22a, 2026-11-19), which is itself a fork of [zako-vdd](https://github.com/qiin2333/zako-vdd) by qiin2333 and its other authors, at v0.18.2, commit abadb3a. Nearly all of it is their work; the original copyright is in [LICENSE](LICENSE) and their README is [README-upstream.md](README-upstream.md). The folder layout is theirs, so that their fixes stay easy to take. The driver started from Microsoft's [indirect display sample](https://github.com/microsoft/Windows-driver-samples/tree/main/video/IndirectDisplay), and what's left of that is under the Microsoft Public License, in [LICENSE-MS-PL](LICENSE-MS-PL). This folder stays under the MIT licence.

## What remoteman changed

- **Its own identity**, so that it never meets another fork of zako-vdd on the same machine: the driver is `RemotemanDisplay.dll`, its hardware id `Root\RemotemanDisplay`, its control interface GUID `{07FDC362-5DEF-4362-9B0E-361FEEF0CDFF}`, its shared objects `Global\RemotemanDisplay_*`, its registry key `HKLM\SOFTWARE\remoteman\Display`, its settings folder `%ProgramData%\remoteman\display`, its log provider `remoteman.Display`, and its monitors' EDID `RMN0001`. The host (`crates/remoteman-host-windows/src/vdd.rs`) has the same values.
- **Guesthouse's VM link taken out**: the compute pass and the `D3DKMTEscape` channel that sent each frame and the cursor to a Guesthouse VM's host. On a real PC those escapes would go to the graphics card's own driver. The gamma callback still accepts Windows' colour transform and doesn't apply it, since the host reads Windows' surface as it is.
- **Kept as it was**: the frame channel (shared textures with keyed mutexes and a metadata section) and the control device's IOCTLs and text commands, which are all the host uses; the cursor's shared-memory export; HDR; the first monitor made when the driver starts.
- **ATL dropped** for WRL's `ComPtr` in the settings code, so that Visual Studio Build Tools can build it.
- `vdd_settings.xml` asks for three monitors: the first, and two the host keeps out of the desktop until a device takes one.

Guesthouse's take-over scripts and its tests of the VM link are not here.

## Building and installing

[BUILDING.md](BUILDING.md). remoteman host's installer (`windows/host-installer`) carries the built driver and installs it on each machine.

## What's been tested

Guesthouse's and zako-vdd's testing was of their builds. This fork has been built (2026-11-19) and installed on one real PC (Windows 11 build 26300, an RTX 5090, three monitors of its own) by remoteman host's installer. Before that, every run of remoteman's Windows host was in a VM, against Guesthouse's build of the driver.
