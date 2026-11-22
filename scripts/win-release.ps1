# A release of remoteman for Windows, everything from one run and one version, on Windows:
#   powershell -ExecutionPolicy Bypass -File scripts\win-release.ps1 -Version X.Y.Z [-Out target\windows-release]
#              [-Ffmpeg C:\ffmpeg] [-Libclang C:\rm\llvm\bin]
# leaves in -Out:
#   remoteman-host-setup-X.Y.Z.exe      the host's installer (scripts\win-host-setup.ps1: the host,
#                                       its display driver, Remoteman Host, setup.ps1)
#   remoteman-viewer-setup-X.Y.Z.exe    the viewer's installer, for its user, outside the Store
#   remoteman-viewer-windows-X.Y.Z.zip  the viewer's folder, which runs with nothing installed
#   and beside each FILE, FILE.sha256: its SHA-256 in lower-case hex, two spaces, its name, a line
#   end (as sha256sum writes it; `sha256sum -c` reads it).
# The zip and the viewer's installer are made from one folder, staged once (scripts\
# win-viewer-app.ps1's, plus licences\ and version.txt), so what one installs and the other holds
# can't differ: the zip holds that folder as remoteman-viewer-windows-X.Y.Z\, the installer
# installs its files into %LOCALAPPDATA%\Programs\remoteman.
#
# -Version must be the workspace's version ([workspace.package] in Cargo.toml), which every
# program's own version information is built from: bump it there first. The version is then in
# the files' names, in each installer's archive (what its pages and Settings > Apps show) and in
# the viewer folder's version.txt.
#
# The viewer's folder carries remoteman's LICENSE (GPL-3.0-or-later) and FFmpeg's: FFmpeg is LGPL, as shared
# DLLs taken from -Ffmpeg (avcodec, avutil, swresample), so its licence text (-Ffmpeg's
# LICENSE.txt) goes beside them in licences\, with where its source is and which build it is (from
# that folder's ffmpeg.exe -version). Nothing is signed. Needs what the two scripts it runs need:
# Rust (MSVC), Build Tools with C++ and CMake, the driver kit's NuGet packages ($env:REMOTEMAN_WDK
# or C:\rm\wdk\packages), FFmpeg 9 LGPL shared in -Ffmpeg, libclang in -Libclang. What is missing
# is said before anything is built. docs/WINDOWS-PACKAGING.md has the rest.
param([Parameter(Mandatory)][string]$Version, [string]$Out = '', [string]$Ffmpeg = 'C:\ffmpeg',
      [string]$Libclang = 'C:\rm\llvm\bin')
$ErrorActionPreference = 'Stop'
$root = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
if (-not $Out) { $Out = Join-Path $root 'target\windows-release' }
$Out = [IO.Path]::GetFullPath($Out)
$work = Join-Path $root 'target\windows-release-work'

# ---- One version ----
if ($Version -notmatch '^\d+\.\d+\.\d+$') { throw "-Version ${Version}: a version is X.Y.Z" }
$line = Select-String -Path (Join-Path $root 'Cargo.toml') -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
$cargoVersion = $line.Matches[0].Groups[1].Value
if ($cargoVersion -ne $Version) {
  throw "-Version $Version isn't the workspace's version ($cargoVersion in Cargo.toml), which the programs' own version information is built from: change it there first"
}

# ---- What it needs, before anything is built ----
$missing = @()
$ffmpegDlls = 'avcodec-63.dll', 'avutil-61.dll', 'swresample-7.dll'
foreach ($dll in $ffmpegDlls) { if (-not (Test-Path (Join-Path $Ffmpeg "bin\$dll"))) { $missing += "FFmpeg's $dll in $Ffmpeg\bin" } }
if (-not (Test-Path (Join-Path $Ffmpeg 'LICENSE.txt'))) { $missing += "FFmpeg's licence, $Ffmpeg\LICENSE.txt" }
if (-not (Test-Path (Join-Path $Ffmpeg 'bin\ffmpeg.exe'))) { $missing += "$Ffmpeg\bin\ffmpeg.exe (it says which FFmpeg this is)" }
if (-not (Test-Path (Join-Path $Libclang 'libclang.dll'))) { $missing += "libclang.dll in $Libclang" }
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = if (Test-Path $vswhere) { & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath }
if (-not $vs) { $missing += 'Visual Studio Build Tools with the C++ tools' }
elseif (-not (Get-Command cmake -ErrorAction SilentlyContinue) -and -not (Test-Path "$vs\Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe")) { $missing += "CMake (Build Tools' own, in $vs)" }
$kit = if ($env:REMOTEMAN_WDK) { $env:REMOTEMAN_WDK } else { 'C:\rm\wdk\packages' }
if (-not (Get-ChildItem $kit -Directory -Filter 'microsoft.windows.wdk.x64.*' -ErrorAction SilentlyContinue)) { $missing += "the driver kit's NuGet packages in $kit" }
if ($missing) { throw "Not on this PC, so nothing was built:`n  $($missing -join "`n  ")" }

function Hash-Beside([string]$file) {
  $hex = (Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash.ToLowerInvariant()
  $name = Split-Path -Leaf $file
  [IO.File]::WriteAllText("$file.sha256", "$hex  $name`n", (New-Object Text.UTF8Encoding $false))
  [pscustomobject]@{ File = $name; Bytes = (Get-Item -LiteralPath $file).Length; SHA256 = $hex }
}

if (Test-Path $work) { Remove-Item -Recurse -Force $work }
New-Item -ItemType Directory -Force $work, $Out | Out-Null
foreach ($old in 'remoteman-host-setup', 'remoteman-viewer-setup', 'remoteman-viewer-windows') {
  Get-ChildItem $Out -File -Filter "$old-$Version.*" -ErrorAction SilentlyContinue | Remove-Item -Force
}

# ---- 1. The viewer's folder, staged once ----
$folderName = "remoteman-viewer-windows-$Version"
$viewer = Join-Path $work $folderName
& powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'scripts\win-viewer-app.ps1') -Out $viewer -Ffmpeg $Ffmpeg -Libclang $Libclang
if ($LASTEXITCODE -ne 0) { throw "the viewer's build failed ($LASTEXITCODE)" }
if (-not (Test-Path (Join-Path $viewer 'remoteman.exe'))) { throw "the viewer's build left no remoteman.exe in $viewer" }
$licences = Join-Path $viewer 'licences'
New-Item -ItemType Directory -Force $licences | Out-Null
Copy-Item (Join-Path $root 'LICENSE') (Join-Path $licences 'remoteman-LICENSE.txt')
Copy-Item (Join-Path $Ffmpeg 'LICENSE.txt') (Join-Path $licences 'FFmpeg-LICENSE.txt')
$said = & (Join-Path $Ffmpeg 'bin\ffmpeg.exe') -hide_banner -version 2>&1 | ForEach-Object { "$_" }
$first = $said | Select-Object -First 1
$configuration = ($said | Where-Object { $_ -like 'configuration:*' } | Select-Object -First 1)
$commit = if ($first -match '-g([0-9a-f]{7,40})\b') { $Matches[1] } else { '' }
# Its licence's name: the text's first two lines ("GNU LESSER GENERAL PUBLIC LICENSE", its version).
$licenceName = ((Get-Content (Join-Path $Ffmpeg 'LICENSE.txt') -TotalCount 6 | ForEach-Object { $_.Trim() } | Where-Object { $_ } | Select-Object -First 2) -join ', ')
$source = @(
  'FFmpeg in remoteman',
  '',
  "remoteman decodes video with FFmpeg's libraries, the shared DLLs beside remoteman.exe:",
  "$($ffmpegDlls -join ', '). They are FFmpeg's, not changed, under the licence in",
  "FFmpeg-LICENSE.txt ($licenceName).",
  'Each may be replaced by another build of FFmpeg with the same names.',
  '',
  'Where its source is: https://ffmpeg.org/download.html, and the source of every release and',
  'commit in its git repository, https://git.ffmpeg.org/ffmpeg.git (mirrored at',
  'https://github.com/FFmpeg/FFmpeg).',
  '',
  "This build, as its ffmpeg.exe says it:",
  "  $first"
)
if ($commit) { $source += "  (git commit $commit of FFmpeg's repository)" }
if ($configuration) { $source += "  $configuration" }
[IO.File]::WriteAllText((Join-Path $licences 'FFmpeg-SOURCE.txt'), (($source -join "`r`n") + "`r`n"), (New-Object Text.UTF8Encoding $false))
[IO.File]::WriteAllText((Join-Path $viewer 'version.txt'), "remoteman $Version`r`n", (New-Object Text.UTF8Encoding $false))

# ---- 2. The zip of that folder ----
# Entry by entry, named with forward slashes as ZIP wants them (Windows PowerShell's
# ZipFile.CreateFromDirectory wrote backslashes, which other systems' unzip keeps in the names).
$zip = Join-Path $Out "$folderName.zip"
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$stream = [IO.File]::Open($zip, [IO.FileMode]::Create)
$archive = New-Object IO.Compression.ZipArchive($stream, [IO.Compression.ZipArchiveMode]::Create)
try {
  foreach ($file in Get-ChildItem -Recurse -File $viewer | Sort-Object FullName) {
    $name = "$folderName/" + $file.FullName.Substring($viewer.Length + 1).Replace('\', '/')
    [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $file.FullName, $name, [IO.Compression.CompressionLevel]::Optimal) | Out-Null
  }
} finally { $archive.Dispose(); $stream.Dispose() }

# ---- 3. The host's installer ----
$hostWork = Join-Path $work 'host'
& powershell -NoProfile -ExecutionPolicy Bypass -File (Join-Path $root 'scripts\win-host-setup.ps1') -Version $Version -Out $hostWork
if ($LASTEXITCODE -ne 0) { throw "the host's installer failed ($LASTEXITCODE)" }
$hostSetup = Join-Path $Out "remoteman-host-setup-$Version.exe"
Copy-Item (Join-Path $hostWork "remoteman-host-setup-$Version.exe") $hostSetup

# ---- 4. The viewer's installer, the same folder packed ----
# win-host-setup.ps1 built both installers and gathered the Windows App Runtime their pages need
# (hostWork\runtime); the viewer's folder has the same runtime beside remoteman.exe, carried once.
$meta = & cargo metadata --format-version 1 --locked --manifest-path (Join-Path $root 'Cargo.toml') | ConvertFrom-Json
$viewerSetup = Join-Path $Out "remoteman-viewer-setup-$Version.exe"
$exe = Join-Path $meta.target_directory 'release\remoteman-viewer-setup.exe'
$report = Join-Path $work 'viewer-pack.txt'
$p = Start-Process -FilePath $exe -ArgumentList @('--pack', '--runtime', "`"$(Join-Path $hostWork 'runtime')`"", '--payload', "`"$viewer`"", '--version', $Version, '--out', "`"$viewerSetup`"") `
  -NoNewWindow -Wait -PassThru -RedirectStandardOutput $report
Get-Content $report
if ($p.ExitCode -ne 0) { throw "packing the viewer's installer failed ($($p.ExitCode))" }

# ---- 5. The hashes ----
$made = foreach ($file in $hostSetup, $viewerSetup, $zip) { Hash-Beside $file }
"`nremoteman $Version for Windows, in ${Out}:"
$made | Format-Table -AutoSize | Out-String -Width 200
