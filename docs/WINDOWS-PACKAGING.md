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
`PublisherDisplayName`) replaces the placeholders in `windows/viewer/Package.appxmanifest`; the
age-rating questionnaire (IARC); a privacy policy URL (the app talks to other computers and keeps
paired computers' keys); Store listing text, screenshots and logos; a justification for
`runFullTrust` (the low-level keyboard hook, raw mouse input, Credential Manager, files to
Downloads, the engine's DLLs); and the certification itself. The manifest also declares the
`microphone` and `webcam` device capabilities (the viewer lends both to a computer in a
session); the privacy policy should say so. Not tried in a package yet.

## The viewer: an MSIX

**Build** (from this PC, holding the VM: `tools/vm.sh take NAME` first):

    scripts/win-viewer-winui.sh --msix

It cross-builds `remoteman_engine.dll` here, sends `windows/viewer`, the engine and three of
FFmpeg's DLLs into the guest, and runs the command that does it inside Windows:

    powershell -File windows\viewer\build.ps1 -Packaged [-Out C:\rm\msix] [-TestCert]

That leaves `C:\rm\msix\Remoteman_0.1.0.0_x64.msix`, **unsigned** (the Store signs what is
uploaded). `-TestCert` (the script passes it) also signs it with a test certificate whose subject
is the manifest's `Publisher` (`CN=PLACEHOLDER`), made once in the building account's store, and
leaves its public half as `remoteman-test.cer`, so that it can be installed for a test: import
the `.cer` into `LocalMachine\TrustedPeople`, then `Add-AppxPackage` the `.msix` as the user.

**What is in it: 54.9 MB to download, 130 MB installed, 69 files** (`msix-contents.txt`): the view
(`remoteman.exe`, `remoteman.dll`), .NET 10 trimmed, WinUI's projection, the engine (6 MB) and
FFmpeg's `avcodec-63` (94 MB of the 130), `avutil-61`, `swresample-7`. The Windows App SDK
itself is not inside: it is the Store's framework package `Microsoft.WindowsAppRuntime.2`, which
the Store installs with the app. Trimming done:

- FFmpeg: only what the engine loads (it links `avcodec` and `avutil`; `avcodec` needs
  `swresample`). `avformat`, `avfilter`, `avdevice` and `swscale` are left out (also from the
  unpackaged build).
- The Windows App SDK's parts by name instead of the `Microsoft.WindowsAppSDK` package, which
  brought its ML runtime (ONNX Runtime and DirectML, 42 MB), AI, Widgets and Search into the app.
  The unpackaged folder went from 527 files and 390 MB to 456 and 273 MB with this.
- .NET trimmed (`PublishTrimmed`, `TrimMode=partial`, packaged build only) with WinUI's projection
  kept whole: trimmed, the window's own backdrop (`StayingMica`, a subclass of a WinUI type)
  failed to load and the app closed at start. Untrimmed the package was 104.7 MB; trimmed
  without the ML parts, 54.9 MB. The one reflection helper (`Engine.Do`'s anonymous objects) is
  now generic with its properties kept.
- No `VCRUNTIME140.dll` is needed any more: the Rust programs for Windows are linked with the
  static C runtime (`.cargo/config.toml`), so the engine and the host need only the Universal
  CRT every Windows 10 and 11 has (checked: neither imports `VCRUNTIME140`).

What would shrink it most next: an FFmpeg built with only the decoders the engine uses (HEVC,
AV1, and the hardware-acceleration glue), the Opus decoder for the computer's sound, and the
camera's H.264 encoders (`h264_nvenc`, `h264_mf`, OpenH264, which this LGPL build links in:
no other DLL is needed for them) instead of the full LGPL build; `avcodec` would go from 94 MB
to a few. Not done. (The microphone's Opus encoder is libopus built into the engine by
`opusic-c`, not FFmpeg's.)

**Tested in the VM** (Windows 11 build 26300, RTX 5090 by GPU-PV), with the throwaway Linux host
`RM_AGENT_ARGS=--keep-output scripts/headless-host.sh` on this PC:
installed with the test certificate (`signed Developer`); found in Start by typing "remoteman"
(`viewer-1-start-menu.png`); started from there (`viewer-2-opened.png`: Computers, "No computers
yet"); paired with 10.0.2.2:7799 (`viewer-3-pair.png`); connected, the host's moving content shown
in the window with the session's pill (`viewer-4-session.png`); stopped; `Remove-AppxPackage`:
no package left, its data folder (`LocalState\remoteman` with the client's key and the paired
host) gone with it, nothing in Credential Manager. Eight launches in a row through `shell:AppsFolder` (what its Start
entry starts, without typing into Start) all ran.

**One launch not explained:** on the first launch after the guest's restart, the test's UI
Automation didn't find the window, though the app's process was running (it was stopped by its
pid at the end). The crash reports in Windows' log at that time were older ones, logged again at
10:00:11, before that launch at 10:00:47. The cause isn't established. Thirteen other launches
showed their window. In case one does crash, the view now writes an unhandled exception to
`LocalState\remoteman-crash.log` in the package's folder even without `--log`. The engine now
logs a panic's message before the app ends: it can't unwind out of the C functions, and a window
app has no stderr.

**Left in the VM by the test** (test tooling, not the app): the test certificate in
`LocalMachine\TrustedPeople` and its key in SYSTEM's own store, so that the next `--msix` build
can be installed again.

**Not tested:** a clean Windows without the Windows App SDK's framework package (the VM had it
from earlier work; the Store installs it with the app, a sideload needs it installed first),
the Windows App Certification Kit, an upgrade from one version to the next, arm64 (the engine and
FFmpeg are x64 only).

**A Store submission still needs:** the identity from Partner Center in the manifest; the
version scheme (as I understand it, not looked up today, Store versions keep the last field 0:
`0.1.0.0` does); real logos at every
scale and the listing's art; the Windows App Certification Kit run on the package; the privacy
policy and `runFullTrust` justification above. Then the unsigned `.msix` is uploaded as it is.

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
4. a scheduled task, "remoteman host", that starts the host at each logon, elevated, in the
   session of whoever logs on (input, the cursor and mode changes work nowhere else), restarted
   if it stops; started at once. The host keeps its state (its identity, the paired devices) in
   `C:\ProgramData\remoteman\host`, readable by Administrators and SYSTEM only, and asks about
   each new device in a window on the desktop (`--ask`) until the owner's app exists;
5. what it made is written to `installed.json`, after each step, so that the uninstaller takes
   away exactly that.

The uninstaller (Settings > Apps > remoteman host) stops the task and the host (by its pid),
removes the task, the firewall rule, the display device if it made it, the driver package if it
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
5. **A smaller FFmpeg** for the viewer (94 MB of its 130).

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
