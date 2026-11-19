# Packaging remoteman for Windows

Written 2026-11-19. How each Windows app is packaged, how to build each package, what was tested
and what was not, and what the owner still has to decide or do. Evidence (screenshots, the
package's contents) is in `/mnt/data/vms/remoteman-win/out/packaging/`.

## What the Microsoft Store allows, and what that decides

Read on Microsoft Learn on 2026-11-19 (Store Policies version 7.20, effective 2026-10-22):

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
- What would change that, for the owner: the driver WHCP-certified (attestation signing through
  Microsoft's Hardware Dev Center, which needs an EV code-signing certificate: paid), disclosed in
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
Downloads, the engine's DLLs); and the certification itself.

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
AV1, and the hardware-acceleration glue) instead of the full LGPL build; `avcodec` would go from
94 MB to a few. Not done.

**Tested in the VM** (Windows 11 build 26300, RTX 5090 by GPU-PV), with the throwaway Linux host
`RM_AGENT_ARGS=--keep-output scripts/headless-host.sh` on this PC:
installed with the test certificate (`signed Developer`); found in Start by typing "remoteman"
(`viewer-1-start-menu.png`); started from there (`viewer-2-opened.png`: Computers, "No computers
yet"); paired with 10.0.2.2:7799 (`viewer-3-pair.png`); connected, the host's moving content shown
in the window with the session's pill (`viewer-4-session.png`); stopped; `Remove-AppxPackage`:
no package left, its data folder (`LocalState\remoteman` with the client's key and the paired
host) gone with it, nothing in Credential Manager. Eight launches in a row from the app's Start
entry all ran.

**Not right yet:** one launch from Start (out of four through Start, eight through
`shell:AppsFolder`) crashed at start, with the same signature as the trimming failure
(`MoAppCrash` in `combase.dll`, `802b000a`, a XAML stowed exception). It didn't come back in
nine launches afterwards. The view now writes an unhandled exception to
`LocalState\remoteman-crash.log` in the package's folder even without `--log`, so the next one
says why. The engine now logs a panic's message before the app ends (it can't unwind out of the
C functions, and a window app has no stderr).

**Left in the VM by the test** (test tooling, not the app): the test certificate in
`LocalMachine\TrustedPeople` and its key in SYSTEM's own store, so that the next `--msix` build
can be installed again.

**Not tested:** a clean Windows without the Windows App SDK's framework package (the VM had it
from earlier work; the Store installs it with the app, a sideload needs it installed first),
the Windows App Certification Kit, an upgrade from one version to the next, arm64 (the engine and
FFmpeg are x64 only).

**A Store submission still needs:** the identity from Partner Center in the manifest; the
version scheme (Store versions must have the last field 0: `0.1.0.0` does); real logos at every
scale and the listing's art; the Windows App Certification Kit run on the package; the privacy
policy and `runFullTrust` justification above. Then the unsigned `.msix` is uploaded as it is.

## The host: an installer of its own

**Build** (from this PC, holding the VM):

    scripts/win-host-installer.sh [VERSION]

It cross-builds `remoteman-host.exe` here, sends it with `windows/host` and Guesthouse's display
driver (`GuesthouseDisplay.dll`, `.inf`, `vdd_settings.xml` set to three monitors, the driver's
MIT and MS-PL licences; from `$GUESTHOUSE_DRIVER`, by default the VM's copy) into the guest and
runs the command that does it inside Windows:

    powershell -File windows\host\build.ps1 [-Version 0.1.0]

which needs Inno Setup 6 (installed in the VM at `C:\rm\pkgtools\inno`, version 6.7.4, from
jrsoftware.org, its signature checked) and leaves `C:\rm\installer\remoteman-host-setup-0.1.0.exe`
(3.7 MB).

**What it does,** with one UAC prompt and no terminal (`windows/host/remoteman-host.iss`,
`windows/host/setup.ps1`):

1. the files into `C:\Program Files\remoteman host`;
2. the display driver: a catalog over its two files, signed with a certificate made on this
   machine (`CN=remoteman display driver, this machine only`), put in the machine's Root and
   TrustedPublisher stores, its private key deleted at once so nothing else can ever be signed
   with it; the driver package added, the display device made (or, if one is there, updated);
   the driver's settings into `C:\VirtualDisplayDriver` unless a driver already has some there;
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

**What the VM could not show:** the VM already had Guesthouse's own build of the same driver
(same INF and DLL) as a display device. `pnputil` therefore kept the existing package (`oem3.inf`,
signed by Guesthouse's certificate) instead of adding ours, and the installer only updated the
device that was there; so the uninstaller rightly left both alone. **The path every real PC
takes (a new driver package signed by our certificate, a display device made, both removed by
the uninstaller) has not run.** Nor has: a standard (not administrator) user, for whom the task
starts the host unelevated and it can't open the driver's device (it needs an administrator
until the host has a service of its own); a PC without automatic logon (the host starts at
logon, not at boot: no session, no host; the sign-in screen needs the service that isn't built);
a real PC with a screen, where the driver's first monitor appears as an extra screen on the
desktop; Secure Boot with a real monitor; an upgrade over an earlier install; Windows' own
firewall prompt (the rule exists before the host first listens, so none should appear).

## What an unsigned download shows a user

Neither of these was seen here (the VM's files have no "downloaded from the internet" mark), so
this is what Microsoft documents and what users commonly report:

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
