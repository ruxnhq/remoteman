# Builds the Windows host's installer, the one downloadable file remoteman-host-setup-VERSION.exe,
# on Windows, in one command:
#   powershell -ExecutionPolicy Bypass -File scripts\win-host-setup.ps1 [-Version 0.1.0] [-Out FOLDER]
#                                                                       [-Payload FOLDER] [-NoApp]
# 1. builds remoteman-host.exe (cargo, MSVC; CMake from Visual Studio Build Tools, for Opus);
# 2. builds remoteman's display driver (windows\display-driver\build.ps1: Build Tools and the
#    driver kit's NuGet packages, in $env:REMOTEMAN_WDK or C:\rm\wdk\packages);
# 3. builds Remoteman Host, the app (crates\remoteman-host-app-windows\stage.ps1), if its crate
#    is here and -NoApp isn't given;
# 4. gathers the files to install (the payload) in the out folder's payload\:
#      remoteman-host.exe, setup.ps1 (windows\host-installer), driver\pkg\RemotemanDisplay.dll
#      and .inf, driver\vdd_settings.xml and the driver's two licences, and app\ (the app and
#      the Windows App Runtime beside it);
# 5. builds the installer (crates\remoteman-host-installer-windows) and packs the Windows App
#    Runtime its pages need and the payload into it (remoteman-host-setup --pack): the runtime
#    is the same as the app's and is carried once.
# -Payload FOLDER packs that folder as the payload instead of steps 1 to 4 (it must have
# setup.ps1). docs/WINDOWS-PACKAGING.md has the rest.
param([string]$Version = '', [string]$Out = '', [string]$Payload = '', [switch]$NoApp)
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
if (-not $Out) { $Out = Join-Path $root 'target\installer' }
New-Item -ItemType Directory -Force $Out | Out-Null
if (-not $Version) {
  $line = Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
  $Version = $line.Matches[0].Groups[1].Value
}

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
if (Test-Path $vswhere) {
  $vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
  $cmake = "$vs\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin"
  if (-not (Get-Command cmake -ErrorAction SilentlyContinue) -and (Test-Path $cmake)) { $env:PATH = "$cmake;$env:PATH" }
}

Push-Location $root
try {
  if (-not $Payload) {
    & cargo build --release --locked -p remoteman-host-windows -p remoteman-vcam-windows
    if ($LASTEXITCODE -ne 0) { throw "the host's build failed ($LASTEXITCODE)" }
    $driverOut = Join-Path $root 'windows\display-driver\out'
    & powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'windows\display-driver\build.ps1') -Out $driverOut
    if ($LASTEXITCODE -ne 0) { throw "the driver's build failed ($LASTEXITCODE)" }
    $appStage = Join-Path $root 'crates\remoteman-host-app-windows\stage.ps1'
    $withApp = (Test-Path $appStage) -and -not $NoApp
    $appOut = Join-Path $Out 'app-folder'
    if ($withApp) {
      & powershell -NoProfile -ExecutionPolicy Bypass -File $appStage -Out $appOut
      if ($LASTEXITCODE -ne 0) { throw "the app's build failed ($LASTEXITCODE)" }
    }
    $Payload = Join-Path $Out 'payload'
    if (Test-Path $Payload) { Remove-Item -Recurse -Force $Payload }
    New-Item -ItemType Directory -Force "$Payload\driver\pkg", "$Payload\vcam" | Out-Null
    Copy-Item (Join-Path $root 'target\release\remoteman-host.exe'), (Join-Path $root 'windows\host-installer\setup.ps1') $Payload
    Copy-Item (Join-Path $root 'target\release\remoteman_vcam_windows.dll') "$Payload\vcam"
    Copy-Item (Join-Path $driverOut 'RemotemanDisplay.dll'), (Join-Path $driverOut 'RemotemanDisplay.inf') "$Payload\driver\pkg"
    foreach ($f in 'vdd_settings.xml', 'LICENSE', 'LICENSE-MS-PL') { Copy-Item (Join-Path $driverOut $f) "$Payload\driver" }
    if ($withApp) { Copy-Item -Recurse $appOut "$Payload\app" }
  }
  if (-not (Test-Path (Join-Path $Payload 'setup.ps1'))) { throw "$Payload has no setup.ps1" }

  & cargo build --release --locked -p remoteman-host-installer-windows
  if ($LASTEXITCODE -ne 0) { throw "the installer's build failed ($LASTEXITCODE)" }
  # The Windows App Runtime the pages need: what windows-reactor-setup staged beside the
  # installer in target\release, the files its runtime.txt lists (not the WebView2 DLL it also
  # stages, which only Reactor's WebView2 control loads).
  $meta = & cargo metadata --format-version 1 --locked | ConvertFrom-Json
  $setupCrate = $meta.packages | Where-Object { $_.name -eq 'windows-reactor-setup' } | Select-Object -First 1
  $list = Get-Content (Join-Path (Split-Path $setupCrate.manifest_path) 'assets\runtime.txt') | Where-Object { $_.Trim() }
  $release = Join-Path $meta.target_directory 'release'
  $runtime = Join-Path $Out 'runtime'
  if (Test-Path $runtime) { Remove-Item -Recurse -Force $runtime }
  New-Item -ItemType Directory -Force $runtime | Out-Null
  $staged = Get-ChildItem $release | Where-Object { $list -contains $_.Name.ToLowerInvariant() }
  if ($staged.Count -ne $list.Count) { throw "runtime.txt lists $($list.Count) entries, target\release has $($staged.Count) of them" }
  foreach ($entry in $staged) { Copy-Item -Recurse $entry.FullName $runtime }

  $setup = Join-Path $Out "remoteman-host-setup-$Version.exe"
  $exe = Join-Path $release 'remoteman-host-setup.exe'
  $report = Join-Path $Out 'pack.txt'
  # The installer is a windowed program: started this way so that it is waited for.
  $p = Start-Process -FilePath $exe -ArgumentList @('--pack', '--runtime', "`"$runtime`"", '--payload', "`"$Payload`"", '--version', $Version, '--out', "`"$setup`"") `
    -NoNewWindow -Wait -PassThru -RedirectStandardOutput $report
  Get-Content $report
  if ($p.ExitCode -ne 0) { throw "packing failed ($($p.ExitCode))" }
} finally { Pop-Location }
