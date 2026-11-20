# Builds the Windows host's installer on Windows itself, in one command (scripts/win-host-installer.sh
# is the same from Linux into the VM):
#   powershell -ExecutionPolicy Bypass -File scripts\win-host-installer.ps1 [-Version 0.1.0] [-Out FOLDER]
# 1. builds remoteman-host.exe (cargo, MSVC; CMake from Visual Studio Build Tools for Opus);
# 2. builds remoteman's display driver (windows/display-driver/build.ps1: Build Tools and the
#    driver kit's NuGet packages, in $env:REMOTEMAN_WDK or C:\rm\wdk\packages);
# 3. stages both beside windows/host-installer's files in a work folder and runs its build.ps1
#    (Inno Setup 6), which leaves remoteman-host-setup-VERSION.exe in -Out (default target\installer).
# docs/WINDOWS-PACKAGING.md has the rest.
param([string]$Version = '0.1.0', [string]$Out = '')
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot
if (-not $Out) { $Out = Join-Path $root 'target\installer' }

$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
$cmake = "$vs\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin"
if (-not (Get-Command cmake -ErrorAction SilentlyContinue) -and (Test-Path $cmake)) { $env:PATH = "$cmake;$env:PATH" }

Push-Location $root
try {
  & cargo build --release --locked -p remoteman-host-windows -p remoteman-vcam-windows
  if ($LASTEXITCODE -ne 0) { throw "the host's build failed ($LASTEXITCODE)" }
} finally { Pop-Location }

$driverOut = Join-Path $root 'windows\display-driver\out'
& powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'windows\display-driver\build.ps1') -Out $driverOut
if ($LASTEXITCODE -ne 0) { throw "the driver's build failed ($LASTEXITCODE)" }

$work = Join-Path ([IO.Path]::GetTempPath()) "remoteman-hostpkg-$PID"
New-Item -ItemType Directory -Force "$work\native" | Out-Null
try {
  Copy-Item (Join-Path $root 'windows\host-installer\*') $work
  Copy-Item (Join-Path $root 'target\release\remoteman-host.exe'), (Join-Path $root 'target\release\remoteman_vcam_windows.dll') "$work\native\"
  foreach ($f in 'RemotemanDisplay.dll', 'RemotemanDisplay.inf', 'vdd_settings.xml', 'LICENSE', 'LICENSE-MS-PL') { Copy-Item (Join-Path $driverOut $f) "$work\native\" }
  & powershell -NoProfile -ExecutionPolicy Bypass -File "$work\build.ps1" -Version $Version -Out $Out
  if ($LASTEXITCODE -ne 0) { throw "the installer's build failed ($LASTEXITCODE)" }
} finally { Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue }
