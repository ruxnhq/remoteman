# Builds the host's installer, remoteman-host-setup-VERSION.exe, inside Windows (the VM):
#   powershell -File build.ps1 [-Version 0.1.0] [-Out C:\rm\installer]
# Needs Inno Setup 6 (ISCC.exe: C:\rm\pkgtools\inno, or Program Files) and, in .\native\,
# remoteman-host.exe and the display driver's files (GuesthouseDisplay.dll, GuesthouseDisplay.inf,
# vdd_settings.xml, the driver's two licences). scripts/win-host-installer.sh puts them there
# and runs this from Linux.
param([string]$Version = "0.1.0", [string]$Out = "C:\rm\installer")
$ErrorActionPreference = "Stop"
Set-Location $PSScriptRoot
$iscc = @("C:\rm\pkgtools\inno\ISCC.exe", "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe", "$env:ProgramFiles\Inno Setup 6\ISCC.exe") |
  Where-Object { Test-Path $_ } | Select-Object -First 1
if (-not $iscc) { throw "Inno Setup 6 is not installed (https://jrsoftware.org/isdl.php)" }
Remove-Item -Recurse -Force stage, out -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force stage\driver\pkg | Out-Null
Copy-Item native\remoteman-host.exe, setup.ps1 stage\
Copy-Item native\GuesthouseDisplay.dll, native\GuesthouseDisplay.inf stage\driver\pkg\
Copy-Item native\vdd_settings.xml, native\LICENSE, native\LICENSE-MS-PL stage\driver\
& $iscc /Q "/DAppVersion=$Version" remoteman-host.iss
if ($LASTEXITCODE -ne 0) { throw "ISCC failed ($LASTEXITCODE)" }
New-Item -ItemType Directory -Force $Out | Out-Null
$setup = Get-ChildItem out\*.exe | Select-Object -First 1
Copy-Item $setup.FullName $Out -Force
"made $(Join-Path $Out $setup.Name) ($([math]::Round($setup.Length / 1MB, 1)) MB)"
