# Packaging remoteman for Windows

Written 2026-11-19. How each Windows app is packaged, how to build each package, what was tested
and what was not, and what the owner still has to decide or do. Evidence (screenshots, the
package's contents) is in `/mnt/data/vms/remoteman-win/out/packaging/`.

## What the Microsoft Store allows, and what that decides

From Microsoft Learn on 2026-11-19 (Store Policies version 7.20, effective 2026-10-22). Read in
full: the Store policies, the MSI/EXE package requirements and "Prepare to package"; the
capability declarations page partly (the fetch was cut). The rows on firewall rules, startup
tasks, services' manifest element, the certificate element and the free account come from
Microsoft Learn's search excerpts of those pages, not a full reading.

| The app needs to | In a Store MSIX? | Source |
|---|---|---|
| Be a full-trust desktop app | Yes: the `runFullTrust` restricted capability, justified at submission | [App capability declarations](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/app-capability-declarations) |
| Add an inbound firewall rule | Yes: the `windows.firewallRules` extension (`desktop2:FirewallRules`), full trust | [desktop2:FirewallRules](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-desktop2-firewallrules), [packaging extensions](https://learn.microsoft.com/en-us/windows/apps/desktop/modernize/desktop-to-uwp-extensions) |
| Start with Windows | At logon only, not elevated: `windows.startupTask`, after the user has started the app once | [uap5:StartupTask](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-uap5-startuptask), [StartupTask](https://learn.microsoft.com/en-us/uwp/api/windows.applicationmodel.startuptask) |
| Run a service at boot | Per-machine services only, behind the `packagedServices` / `localSystemServices` restricted capabilities; Microsoft's capability page (as summarised, not read verbatim: the fetch was cut) says it doesn't recommend them for the Store and in most cases won't approve them | [desktop6:Service](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/uapmanifestschema/element-desktop6-service), [Prepare to package](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-prepare) |
| Install a driver | **No.** "MSIX doesn't support Windows drivers." And Store policy 10.2.4: "dependency on non-Microsoft provided drivers or NT services is not allowed but may be considered case by case for WHCP certified drivers" | [Prepare to package](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-prepare), [Store policies 10.2.4](https://learn.microsoft.com/en-us/windows/apps/publish/store-policies) |
| Put a certificate in the machine's Root store | No documented way (the old `windows.certificates` extension's scope is not stated; nothing says it reaches the machine's Root and TrustedPublisher stores, which a driver signed here needs) | [Certificate element](https://learn.microsoft.com/en-us/uwp/schemas/appxpackage/appxmanifestschema2010-v2/element-certificate) |
| Run elevated | "apps that require elevation for any part of their functionality won't be accepted into the Store"; `allowElevation` is approved only under strict criteria | [Prepare to package](https://learn.microsoft.com/en-us/windows/msix/desktop/desktop-to-uwp-prepare), [capabilities](https://learn.microsoft.com/en-us/windows/apps/package-and-deploy/app-capability-declarations) |

The Store also takes plain MSI and EXE installers (policy 10.2.9 and
[MSI/EXE package requirements](https://learn.microsoft.com/en-us/windows/apps/publish/publish-your-app/msi/app-package-requirements)),
but the Store does not sign those: "The binary and all of its Portable Executable (PE) files must
be digitally signed with a code signing certificate that chains up to a certificate issued by a
Certificate Authority (CA) that is part of the Microsoft Trusted Root Program." That is a bought
certificate, which the owner ruled out. Such an installer must also install silently (a UAC
prompt is allowed) and download nothing.

**Decided from this:**

- **The viewer goes through the Store as an MSIX.** It needs nothing the Store forbids: full trust,
  the network, a folder of its own. The Store signs it.
- **The host can't go through the Store,** in any form, as things stand. It must be elevated
  (the driver's device lets in Administrators and SYSTEM only), it needs a driver that is not
  WHCP-certified and is signed on each machine with a certificate made there, and it needs that
  certificate in the machine's Root and TrustedPublisher stores. "A Store app plus an elevated
  first-run setup" doesn't get round policy 10.2.4 (the dependency on the driver is the same,
  however it gets installed) or the elevation rule. So the host ships as **its own installer,
  outside the Store** (an Inno Setup `.exe`, unsigned), downloaded from our site.
- What would change that, for the owner: the driver WHCP-certified (through Microsoft's
  Hardware Dev Center, which as far as I know needs an EV code-signing certificate: paid; not
  looked up today), disclosed in
  the certification notes, and the host's elevated part as a packaged service; even then the
  Store decides case by case. Or a CA-signed MSI/EXE through the Store's installer route (paid
  certificate too). Neither is free.

**What the Store asks of the owner** (none of it done): a Partner Center developer account
(individual accounts are free: [open a developer account](https://learn.microsoft.com/en-us/windows/apps/publish/partner-center/open-a-developer-account));
the app's name reserved there, whose identity (`Identity Name`, `Publisher`,
`PublisherDisplayName`) replaces the placeholders in `crates/remoteman-viewer-app-windows/package/AppxManifest.xml`; the
age-rating questionnaire (IARC); a privacy policy URL (the app talks to other computers and keeps
paired computers' keys); Store listing text, screenshots and logos; a justification for
`runFullTrust` (the low-level keyboard hook, raw mouse input, Credential Manager, files to
Downloads, the engine's DLLs); and the certification itself. The manifest also declares the
`microphone` and `webcam` device capabilities (the viewer lends both to a computer in a
session); the privacy policy should say so. Not tried in a package yet.

## The viewer: an MSIX

Since 2026-11-19 the viewer is WinUI 3 written in Rust with Microsoft's windows-reactor
(`crates/remoteman-viewer-app-windows`, the view) over the engine (`crates/remoteman-viewer-windows`,
linked in as a Rust library): one program, `remoteman.exe`, no C# and no .NET. The C# view
(`windows/viewer`) and its build scripts are gone.

**Build** (on Windows: Rust with MSVC, Build Tools with CMake, FFmpeg 9 LGPL shared in
`C:\ffmpeg`, libclang in `C:\rm\llvm\bin`, the Windows SDK's `makeappx.exe` for the MSIX):

    powershell -ExecutionPolicy Bypass -File scripts\win-viewer-app.ps1 -Msix [-Out target\viewer-msix] [-Version 0.1.0.0]

It builds the exe framework-dependent (`REMOTEMAN_FRAMEWORK_DEPENDENT=1`: no Windows App Runtime
staged beside it, no manifest that looks for one there; in `target\fd`, a short name because
CMake's paths for Opus run past MSBuild's 260 characters under a longer one), lays out
`remoteman.exe`, FFmpeg's three DLLs, the assets and `crates/remoteman-viewer-app-windows/package/
AppxManifest.xml` (with the version given), and packs them with `makeappx`. That leaves
`target\viewer-msix\remoteman_0.1.0.0_x64.msix`, **unsigned** (the Store signs what is uploaded).
The manifest declares full trust, the network, the microphone and the camera as before, and a
`PackageDependency` on `Microsoft.WindowsAppRuntime.2` at 2.6.4 (the version windows-reactor's
bootstrap asks for; Reactor sets per-monitor DPI awareness itself, so no manifest in the exe is
needed). No test certificate is made and nothing is signed or installed by the script.

**What is in it: 43.6 MB to download (the C# app's: 54.9 MB), 102.6 MB installed (130 MB), 9
files:** `remoteman.exe` (9 MB: the view, the engine, Opus, the static CRT), FFmpeg's
`avcodec-63` (90 MB of the 103), `avutil-61`, `swresample-7`, four logos, the manifest. The
Windows App Runtime is the Store's framework package, installed with the app.

**The folder that runs with nothing installed** (development, tests, a download outside the
Store):

    powershell -ExecutionPolicy Bypass -File scripts\win-viewer-app.ps1 [-Out target\viewer-app]

`remoteman.exe`, FFmpeg's three DLLs and the Windows App Runtime 2.6.4 beside them, as
windows-reactor-setup stages it (the first build on a PC downloads it from NuGet into
`%LOCALAPPDATA%\windows-reactor-setup`; its list is the pinned Reactor's `assets\runtime.txt`, less
`Microsoft.Web.WebView2.Core.dll`, which only Reactor's WebView2 control loads): **209 files, 160
MB** (the C# app's folder: 456 files, 273 MB). Started from that folder with nothing on the path, a
played session (`--play`) decoded on the card and showed in the window.

**Tested on the owner's PC** (Windows 11 build 26300, RTX 5090), with no host (made-up sessions:
`--play` clips through the real decoder and drawing; the old program's walk steps for every
other state, `--ui` with `REMOTEMAN_WALK_FILE`): every page, dialog and menu in light and dark
through UI Automation, with pictures (`crates/remoteman-viewer-app-windows/test/walk.ps1`); full
screen's bar and a menu of it as the engine's draw thread saves them (`test/fullshots.ps1`);
pointer, both buttons, the wheel and keys (a letter, Tab, the arrows, Alt, F10, Space) reaching
the engine from real input; variable refresh in full screen (`test/vrr.ps1`, below). **Not
tested:** a real host (left for the owner's session), the MSIX installed (nothing is installed on
that PC), the second display's window, a touchscreen, the microphone and camera lent to a
computer.

**Variable refresh** happens in full screen only, which is a window of the engine's own over the
monitor (the app's window waits hidden behind it) presenting into an HWND flip swap chain with
tearing allowed, on a Direct3D device that does nothing else (on the engine's device, which also
decodes, the desktop showed it as an overlay at the monitor's fixed rate). Measured on DISPLAY1
(ASUS XG32UCWMG, 240 Hz, HDR on) with 100 pictures a second, a line a second: the monitor blanked
10.00 ms apart for the whole run in 10-bit SDR (17 s, the full-screen window in front), 8-bit SDR
(19 s) and 10-bit PQ, HDR10 passed through (19 s); in those two the window in front was the app's
own, hidden one, not the full-screen window. In a further run with the pointer at the top edge
(the bar out, its layer over the picture), a click on the picture and the pointer away again (the
layer taken away), it stayed at 10.00 ms throughout, the full-screen window in front. In a window
(the `SwapChainPanel`) it stays at 4.17 ms. Every drop to 4.17 ms in full screen came with another
program's window in front: the earlier report of 10-bit dropping back after a few seconds was
that. DXGI's exclusive full screen varied the refresh for a few seconds in the probe and then its
Present failed (DXGI_ERROR_INVALID_CALL), the cause not established; it isn't used, since the
borderless window holds.

**Not tested before (still true):** a clean Windows without the framework package, the Windows
App Certification Kit, an upgrade, arm64 (the engine and FFmpeg are x64 only).

**A Store submission still needs:** the identity from Partner Center in
`crates/remoteman-viewer-app-windows/package/AppxManifest.xml`; the version scheme (Store
versions keep the last field 0); real logos at every scale and the listing's art; the Windows App
Certification Kit run on the package; the privacy policy and `runFullTrust` justification above.
Then the unsigned `.msix` is uploaded as it is.
## The host: an installer of its own

**The display driver is remoteman's own** (since 2026-11-19, on the owner's word that remoteman
must not depend on Guesthouse): `windows/display-driver`, forked from Guesthouse's fork of
zako-vdd, with its own name, hardware id, control GUID, shared-object names, settings folder and
EDID, and Guesthouse's VM link taken out. Its README says what changed; its BUILDING.md how it is
built (Build Tools and the driver kit's NuGet packages, no Visual Studio driver integration).
Everything below that was run before that date ran with Guesthouse's build of the driver.

**Build on Windows** (one command; needs Build Tools with C++, Rust, Inno Setup 6 and the driver
kit's NuGet packages):

    powershell -ExecutionPolicy Bypass -File scripts\win-host-installer.ps1 [-Version 0.1.0]

It builds the host, the driver (`windows\display-driver\build.ps1`) and the installer, which it
leaves in `target\installer`.

**Build from Linux** (holding the VM; the driver must have been built in Windows first, as Linux
can't build it):

    scripts/win-host-installer.sh [VERSION]

It cross-builds `remoteman-host.exe` here, sends it with `windows/host-installer` and remoteman's
display driver (`RemotemanDisplay.dll`, `.inf`, `vdd_settings.xml` asking for three monitors, the
driver's MIT and MS-PL licences; from `$REMOTEMAN_DRIVER`, by default
`windows/display-driver/out`) into the guest and runs the command that does it inside Windows:

    powershell -File windows\host-installer\build.ps1 [-Version 0.1.0]

which needs Inno Setup 6 (installed in the VM at `C:\rm\pkgtools\inno`, version 6.7.4, from
jrsoftware.org, its signature checked) and leaves `C:\rm\installer\remoteman-host-setup-0.1.0.exe`
(3.7 MB).

**What it does,** with one UAC prompt and no terminal (`windows/host-installer/remoteman-host.iss`,
`windows/host-installer/setup.ps1`):

1. the files into `C:\Program Files\remoteman host`;
2. the display driver: a catalog over its two files, signed with a certificate made on this
   machine (`CN=remoteman display driver, this machine only`), put in the machine's Root and
   TrustedPublisher stores, its private key deleted at once so nothing else can ever be signed
   with it; the driver package added, the display device (`Root\RemotemanDisplay`) made (or, if
   one is there, updated); the driver's settings into `%ProgramData%\remoteman\display` unless an
   earlier install left some there (Administrators and SYSTEM full, LOCAL SERVICE, which the
   driver runs as, may change them, Users may read them);
3. an inbound firewall rule, "remoteman host": UDP 7791, for that program only, every profile;
4. **since 2026-11-19, a service**, "remoteman host" (automatic, LocalSystem, restarted if it
   ends), started at once (`crates/remoteman-host-windows/src/service.rs`): from boot on it keeps
   one host running as SYSTEM in the console's session, with no window, and moves it when the
   console moves to another session. That host follows the input desktop
   (`crates/remoteman-host-windows/src/desk.rs`), so it reaches the sign-in screen, the lock
   screen and Windows' permission prompts; its control pipe admits whoever is signed in at the
   console; files from a device go to that user's Downloads (refused while nobody is signed in).
   Ctrl+Alt+Del from a device goes to the service, which sends it with `SendSAS`, so the
   installer sets the policy `SoftwareSASGeneration` to allow services (and the uninstaller puts
   back what it was). An earlier install's logon task is removed. (Before that date: a
   scheduled task that started the host at each logon, elevated, in a console window that
   stopped the host when closed.) The host keeps its state (its identity, the paired devices)
   in `C:\ProgramData\remoteman\host`, readable by Administrators and SYSTEM only (its log
   through `remoteman-host ctl log` too), and asks about each new device in a window on the
   desktop (`--ask`) until the owner's app is in the installer. On a PC with screens of its
   own the driver's monitors are on the desktop only while a device is, placed against the PC's
   screens, which are written down first (`screens.txt` in the state folder) and put back
   exactly when the device leaves or by the next host after a crash. The host announces itself
   on the network for viewers to find (DNS-SD `_remoteman._udp` through Windows' own
   `DnsServiceRegister`: the LAN card's address, `v`, `fp`, `os=windows`, `mac`; `advertise.rs`;
   `remoteman-host --announce-test NAME SECONDS` tries it without the driver);
5. what it made is written to `installed.json`, after each step, so that the uninstaller takes
   away exactly that.

The uninstaller (Settings > Apps > remoteman host) stops the service (or, from an older
install, the task) and the host (by its pid), removes the service and the task, puts the
Ctrl+Alt+Del policy back, removes the firewall rule, the display device if it made it, the driver package if it
added it, its certificates, the settings folder if it made it, the host's state, and its files.
A failure in `setup.ps1` shows as a message, its reason in `setup.log`, and as exit code 1 in a
silent install (`/VERYSILENT /SUPPRESSMSGBOXES`).

**Tested in the VM:** a snapshot of the machine before (driver packages and their signers, display
devices and their drivers, monitors, the Root, TrustedPublisher, My and TrustedPeople stores,
firewall rules, tasks, services, Run keys, uninstall entries, the folders with each file's hash,
processes); a silent install as the logged-on user, elevated (exit 0, `setup.log` clean, 4 s);
the host then running in session 1 from Program Files, listening on 0.0.0.0:7791, the rule, the
task (Users, Highest), the certificate in Root and TrustedPublisher; pairing through
`remoteman-host ctl pair on` / `pair accept` and a session from the Linux test client through this
PC's forward (127.0.0.1:7801): 1280×720 HEVC, the guest's desktop with its taskbar
(`host-1-session-after-install.png`); the guest restarted: up at 9:57:04, gpupv logged on
automatically, the host started by the task at 9:57:12, and the same client got in again
without pairing, 121 fps (`host-2-session-after-reboot.png`); a silent uninstall (exit 0,
"Removed all? Yes"); the snapshot after it **identical to the one before**; the guest's desktop
1920×1080 alone, as before.

**Installing over an earlier install:** the installer first stops the running host (its task,
then the process from its folder, by pid), which holds its program file: without that a second
install failed with exit code 5. **[V]** installed twice in a row, silently: exit 0 both times,
the host running again under a new pid, then uninstalled back to the snapshot from before.
pnputil keeps a driver package it already has (same
INF and files) whatever its catalog says, so `setup.ps1` deletes the package an earlier install
added before it adds the new one; otherwise the kept package would stay signed by the
certificate the new install just replaced, which Windows no longer trusts. (That deletion did not run in the VM, where the package was
Guesthouse's, not ours.)

**Not tested: what a person sees.** Every install and uninstall here was silent
(`/VERYSILENT`), started from a task that was already elevated. The wizard's pages, the one UAC
prompt, uninstalling from Settings > Apps, and the failure path (the message, exit code 1) have
not been run or seen.

**The fresh-PC driver path [V]** (with the owner's leave, 10:26 to 10:30 PST): Guesthouse's
own driver package and display device were taken out of the VM and its settings folder moved
aside, so the VM looked like a PC that never had the driver. The installer then added a new
driver package signed by its own certificate ("remoteman display driver, this machine only"),
made the display device, wrote `C:\VirtualDisplayDriver` and recorded all three; a fresh device
paired through `ctl` and got the desktop (`host-3-session-fresh-driver.png`). The uninstaller
then removed the device, the package, the certificate and the settings folder along with the
task, the rule and the files: compared with the snapshot taken before Guesthouse's driver was
taken out, the only differences were Guesthouse's own driver (taken out on purpose) and
Windows' non-present monitor entries, which Windows keeps for every monitor it has seen. Then
Guesthouse's driver was put back with Guesthouse's `sign-and-install.ps1` from the same files
(a new per-machine certificate of Guesthouse's; the old package couldn't be exported first) and
its settings restored.

Nor has: a standard (not administrator) user, for whom the task
starts the host unelevated and it can't open the driver's device (it needs an administrator
until the host has a service of its own); a PC without automatic logon (the host starts at
logon, not at boot: no session, no host; the sign-in screen needs the service that isn't built);
a real PC with a screen, where the driver's first monitor appears as an extra screen on the
desktop; Secure Boot with a real monitor; an upgrade over an earlier install; Windows' own
firewall prompt (the rule exists before the host first listens, so none should appear).

## What an unsigned download shows a user

Neither of these was seen here (the VM's files have no "downloaded from the internet" mark, and
the tests ran the installer from an already elevated task, so no UAC prompt either). The wording
below is from memory of how Windows 11 shows them, not looked up today:

- **The viewer from the Store:** nothing. The Store signs it; it installs and updates like any
  Store app.
- **The host's installer from our site:** the browser may warn that the file "isn't commonly
  downloaded". Run, it shows Microsoft Defender SmartScreen's blue "Windows protected your PC"
  ("Microsoft Defender SmartScreen prevented an unrecognised app from starting"), with only
  "Don't run" until the user presses "More info" and then "Run anyway", which names the publisher
  as "Unknown publisher". Then the UAC prompt, in yellow rather than blue, "Do you want to allow
  this app from an unknown publisher to make changes to your device?", with the program's name
  and "Publisher: Unknown". SmartScreen's warning fades only with reputation, which an unsigned
  file builds per file, so each new version starts over.

## Decisions for the owner

1. **The host outside the Store, unsigned** (recommended, as built), with the warnings above; or
   pay for signing (an OV/EV code-signing certificate, or Azure Trusted Signing's subscription)
   to sign the installer and the host, which also opens the Store's MSI/EXE route; or WHCP-sign
   the driver (EV certificate) and ask the Store for a case-by-case exception.
2. **The driver on real PCs:** signed per machine with a certificate trusted only there, as built
   (the route that was meant for VMs), or attestation-signed by Microsoft (paid EV certificate).
3. **A host service** for standard users, the sign-in screen and Windows' own prompts: needed
   before the host works for anyone but an administrator who is logged on.
4. **Store identity:** open the Partner Center account, reserve the name, and answer the age
   rating and privacy questions.
5. **A smaller FFmpeg** for the viewer (avcodec is 90 MB of its 103 installed).

## When the owner's app (Remoteman Host) lands

The app is `crates/remoteman-host-app-windows` (`remoteman-host-app.exe`): WinUI 3 written in
Rust with Microsoft's windows-reactor (`vendor/windows-reactor`, pinned to a windows-rs commit;
`vendor/windows-reactor/VENDORED.md`), no C# or .NET. It speaks to the host only over the host's
control pipe. Its folder, built on Windows:

    powershell -File crates\remoteman-host-app-windows\stage.ps1 [-Out target\host-app]

`remoteman-host-app.exe` (4 MB, the tray icons inside it, the static CRT) and the Windows App
Runtime 2.6.4 beside it, self-contained (`windows-reactor-setup` embeds the manifest that finds
the runtime there, so nothing is installed and no `VCRUNTIME140.dll` is needed): 31 files
(`Microsoft.ui.xaml.dll`, `Microsoft.UI.Xaml.Controls.dll`, `Microsoft.WindowsAppRuntime.dll`,
DWriteCore, the composition and input DLLs, three `.pri` resource indexes), the
`Microsoft.UI.Xaml` folder, and 86 language folders of WinUI's own strings: 206 files,
64,120,183 bytes in all. The layout is kept as it is (the DLLs beside the exe). The first build on
a PC downloads the runtime from NuGet into `%LOCALAPPDATA%\windows-reactor-setup`; a build
cross-compiled from Linux (cargo-xwin) stages nothing and would need the runtime installed.

`crates/remoteman-host-app-windows/test/` drives it with no host: `fakehost.ps1` (a stand-in for
the host's pipe that keeps state and logs every word), `walk.ps1` (every page, switch, menu item
and dialog through UI Automation, with screenshots), `mica.ps1` (the backdrop with another window
active), `notify.ps1` (the Windows notifications seen).

From the host session's list (shared notes, `win-host-install.md`; branch
`claude/win-host-rest-55f8f7`, which is why the installer lives in `windows/host-installer`). The
installer then also needs to:

- carry that folder into `{app}\app` and start `remoteman-host-app.exe --background` at sign-in,
  not elevated, for every user who signs in (HKLM `...\CurrentVersion\Run`, or a Startup
  shortcut in the common Start menu), removed by the uninstaller; a second start (the Start menu
  shortcut while the icon is there) only opens the running app's window;
- agree on the host's state folder: this installer gives the host
  `C:\ProgramData\remoteman\host` (Administrators and SYSTEM only, deleted on uninstall); the
  host session's list says `%LOCALAPPDATA%\remoteman\host`. The app talks to the host over its
  pipe, so either works; per-machine fits a host started for whoever logs on;
- drop `--ask` from the task: the app now asks about new devices (`status`'s `pending`, answered
  with `pair accept` / `pair decline`);
- keep the pipe open to the signed-in user's unelevated app: today `control.rs` grants the user
  the host runs as, which is that user only while the host runs in their session.

Its list also says the host needs the VC++ runtime: no longer, with this branch's
`.cargo/config.toml` (static CRT; the host's imports were checked).
