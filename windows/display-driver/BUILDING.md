# Building the display driver

You build it in Windows. Nothing here gives you a signed driver: remoteman host's installer signs the driver's catalog on each machine that installs it, with a certificate made there (see below).

## What you need

- **Visual Studio Build Tools** 2022 or later with "Desktop development with C++" (the MSVC x64 compiler). Visual Studio's own driver-kit integration is not needed.
- **The Windows Driver Kit as NuGet packages**, all of one version, each unpacked into a folder named `<id>.<version>` (a `.nupkg` is a zip):
  - `Microsoft.Windows.WDK.x64`
  - `Microsoft.Windows.SDK.CPP`
  - `Microsoft.Windows.SDK.CPP.x64`

  From nuget.org, for example `https://api.nuget.org/v3-flatcontainer/microsoft.windows.wdk.x64/10.0.28000.2526/microsoft.windows.wdk.x64.10.0.28000.2526.nupkg`. 10.0.28000.2526 is the version it was first built with. You accept Microsoft's licence for the kit yourself; don't put it in this repo.

## Building

    powershell -ExecutionPolicy Bypass -File build.ps1 -Kit C:\path\to\packages

(or set `REMOTEMAN_WDK` to that folder). It compiles the sources `RemotemanDisplay\RemotemanDisplay.vcxproj` lists with the project's settings (UMDF 2.25, IddCx 1.10, universal, C++17, `/EHa`), links against the kit's UMDF and IddCx stubs and `OneCoreUAP.lib`, and stamps the INF. `out\` then holds `RemotemanDisplay.dll`, `RemotemanDisplay.inf`, `vdd_settings.xml` and both licences: what the host's installer carries. `scripts\win-host-installer.ps1` builds this, the host and the installer in one go.

Not done by `build.ps1`: the Spectre-mitigated runtime (Build Tools has it only if you add its component) and code analysis. The `.sln` and `.vcxproj` still open in Visual Studio with the driver kit installed.

## Installing

Use remoteman host's installer. By hand, as an administrator, `windows\host-installer\setup.ps1 install` does the same from a folder laid out as the installer lays it out. Either way it:

1. Makes a new certificate, just for this machine.
2. Puts it in the machine's Trusted Root and Trusted Publishers stores.
3. Signs the driver's catalog with it.
4. Deletes the private key.
5. Adds the driver package and makes the `Root\RemotemanDisplay` device.

Step 2 is the part to think about. A trusted root is powerful: while its key exists, it could vouch for anything on that machine. That's why step 4 is there. The key can't be exported, it's deleted before the driver goes in, and nothing is ever signed with it again. Every install makes its own certificate, and the uninstaller takes it away again.

Windows trusts this only on that machine. For other machines without that step the driver would need Microsoft's attestation signing, which needs a paid EV certificate.
