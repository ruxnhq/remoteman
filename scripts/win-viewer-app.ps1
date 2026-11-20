# remoteman for Windows (crates\remoteman-viewer-app-windows), built on Windows:
#
#   powershell -ExecutionPolicy Bypass -File scripts\win-viewer-app.ps1 [-Out target\viewer-app]
#       the folder that runs with nothing installed: remoteman.exe (the engine is linked in),
#       FFmpeg's avcodec-63, avutil-61 and swresample-7, and the Windows App Runtime beside them
#       as windows-reactor-setup stages it (self-contained)
#   powershell -ExecutionPolicy Bypass -File scripts\win-viewer-app.ps1 -Msix [-Out target\viewer-msix]
#       the MSIX for the Microsoft Store: remoteman_VERSION_x64.msix in -Out, unsigned (the Store
#       signs what is uploaded), the exe built framework-dependent (REMOTEMAN_FRAMEWORK_DEPENDENT=1)
#       and the Windows App Runtime as the Store's framework package (the manifest's dependency)
#
# Needs Rust (MSVC), Build Tools with CMake (Opus), FFmpeg 9 LGPL shared in -Ffmpeg (C:\ffmpeg),
# libclang in -Libclang (C:\rm\llvm\bin), and for -Msix the Windows SDK's makeappx.exe. The first
# self-contained build on a PC downloads the runtime from NuGet into
# %LOCALAPPDATA%\windows-reactor-setup. Nothing is installed or signed.
param([string]$Out = "", [switch]$Msix, [string]$Ffmpeg = "C:\ffmpeg", [string]$Libclang = "C:\rm\llvm\bin",
      [string]$Version = "0.1.0.0")
$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$crate = Join-Path $root "crates\remoteman-viewer-app-windows"
# For cargo only, not the machine: FFmpeg, libclang, CMake.
$env:FFMPEG_DIR = $Ffmpeg
$env:LIBCLANG_PATH = $Libclang
$cmake = Get-ChildItem "${env:ProgramFiles(x86)}\Microsoft Visual Studio\*\BuildTools\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin" -ErrorAction SilentlyContinue | Select-Object -First 1
if ($cmake -and -not ($env:PATH -like "*$($cmake.FullName)*")) { $env:PATH = "$($cmake.FullName);$env:PATH" }
$ffmpegDlls = "avcodec-63.dll", "avutil-61.dll", "swresample-7.dll"

function Size([string]$path) {
  $files = Get-ChildItem -Recurse -File $path
  "{0} files, {1:N0} bytes ({2:N1} MB)" -f $files.Count, ($files | Measure-Object Length -Sum).Sum, (($files | Measure-Object Length -Sum).Sum / 1MB)
}

Push-Location $root
try {
  if ($Msix) {
    if (-not $Out) { $Out = Join-Path $root "target\viewer-msix" }
    # Framework-dependent, in a target folder of its own (the self-contained build's manifest
    # and staged runtime stay where they are), with a short name: CMake's paths for Opus run
    # past MSBuild's 260 characters under a longer one.
    $env:REMOTEMAN_FRAMEWORK_DEPENDENT = "1"
    cargo build --release --locked -p remoteman-viewer-app-windows --target-dir target\fd
    $built = $LASTEXITCODE
    Remove-Item Env:REMOTEMAN_FRAMEWORK_DEPENDENT
    if ($built -ne 0) { throw "the build failed ($built)" }
    $layout = Join-Path $Out "layout"
    if (Test-Path $layout) { Remove-Item -Recurse -Force $layout }
    New-Item -ItemType Directory -Force (Join-Path $layout "Assets") | Out-Null
    Copy-Item "target\fd\release\remoteman-app.exe" (Join-Path $layout "remoteman.exe")
    foreach ($dll in $ffmpegDlls) { Copy-Item (Join-Path $Ffmpeg "bin\$dll") $layout }
    Copy-Item (Join-Path $crate "package\Assets\*.png") (Join-Path $layout "Assets")
    $manifest = [xml](Get-Content (Join-Path $crate "package\AppxManifest.xml"))
    $manifest.Package.Identity.Version = $Version
    $manifest.Save((Join-Path $layout "AppxManifest.xml"))
    $makeappx = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\makeappx.exe" | Sort-Object FullName | Select-Object -Last 1
    if (-not $makeappx) { throw "no makeappx.exe (the Windows SDK)" }
    $package = Join-Path $Out ("remoteman_{0}_x64.msix" -f $Version)
    & $makeappx.FullName pack /o /d $layout /p $package | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "makeappx failed ($LASTEXITCODE)" }
    "made $package ({0:N1} MB); its layout: {1}" -f ((Get-Item $package).Length / 1MB), (Size $layout)
    return
  }
  if (-not $Out) { $Out = Join-Path $root "target\viewer-app" }
  cargo build --release --locked -p remoteman-viewer-app-windows
  if ($LASTEXITCODE -ne 0) { throw "the build failed ($LASTEXITCODE)" }
  # The runtime's files are the ones windows-reactor-setup copies into target\release (its
  # assets\runtime.txt, read from where cargo keeps that crate), so the list follows the pinned
  # Reactor. Not carried: Microsoft.Web.WebView2.Core.dll, which it stages too but only
  # Reactor's WebView2 control loads (the app has none).
  $meta = cargo metadata --format-version 1 --locked | ConvertFrom-Json
  $setup = $meta.packages | Where-Object { $_.name -eq "windows-reactor-setup" } | Select-Object -First 1
  $list = Get-Content (Join-Path (Split-Path $setup.manifest_path) "assets\runtime.txt") | Where-Object { $_.Trim() }
  $release = Join-Path $meta.target_directory "release"
  if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
  New-Item -ItemType Directory -Force $Out | Out-Null
  Copy-Item (Join-Path $release "remoteman-app.exe") (Join-Path $Out "remoteman.exe")
  foreach ($dll in $ffmpegDlls) { Copy-Item (Join-Path $Ffmpeg "bin\$dll") $Out }
  # Under their own names (runtime.txt has them in lower case).
  $staged = Get-ChildItem $release | Where-Object { $list -contains $_.Name.ToLowerInvariant() }
  if ($staged.Count -ne $list.Count) { throw "runtime.txt lists $($list.Count) entries, target\release has $($staged.Count) of them" }
  foreach ($entry in $staged) { Copy-Item -Recurse $entry.FullName $Out }
  "staged into {0}: {1}" -f $Out, (Size $Out)
} finally { Pop-Location }
