# Remoteman Host's folder as the installer carries it: remoteman-host-app.exe and the Windows App
# Runtime beside it (self-contained, so the PC needs nothing installed), built here on Windows:
#   powershell -File crates\remoteman-host-app-windows\stage.ps1 [-Out target\host-app]
# The runtime's files are the ones windows-reactor-setup copies into target\release (its
# assets\runtime.txt, read from where cargo keeps that crate), so the list follows the pinned
# Reactor. Not carried: Microsoft.Web.WebView2.Core.dll, which reactor-setup stages too but only
# Reactor's WebView2 control loads (the app has none). The first build on a PC downloads the
# runtime from NuGet into %LOCALAPPDATA%\windows-reactor-setup.
param([string]$Out = "")
$root = (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
if (-not $Out) { $Out = Join-Path $root "target\host-app" }
Push-Location $root
try {
  cargo build --release --locked -p remoteman-host-app-windows
  if ($LASTEXITCODE -ne 0) { throw "the build failed ($LASTEXITCODE)" }
  $meta = cargo metadata --format-version 1 --locked | ConvertFrom-Json
  $setup = $meta.packages | Where-Object { $_.name -eq "windows-reactor-setup" } | Select-Object -First 1
  $list = Get-Content (Join-Path (Split-Path $setup.manifest_path) "assets\runtime.txt") | Where-Object { $_.Trim() }
  $release = Join-Path $meta.target_directory "release"
  if (Test-Path $Out) { Remove-Item -Recurse -Force $Out }
  New-Item -ItemType Directory -Force $Out | Out-Null
  Copy-Item (Join-Path $release "remoteman-host-app.exe") $Out
  # Under their own names (runtime.txt has them in lower case).
  $staged = Get-ChildItem $release | Where-Object { $list -contains $_.Name.ToLowerInvariant() }
  if ($staged.Count -ne $list.Count) { throw "runtime.txt lists $($list.Count) entries, target\release has $($staged.Count) of them" }
  foreach ($entry in $staged) { Copy-Item -Recurse $entry.FullName $Out }
  $files = Get-ChildItem -Recurse -File $Out
  "staged into {0}: {1} files, {2:N0} bytes" -f $Out, $files.Count, ($files | Measure-Object Length -Sum).Sum
} finally { Pop-Location }
