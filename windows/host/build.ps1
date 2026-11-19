# Builds Remoteman Host (the owner's app) into one folder that runs with nothing installed:
#   powershell -File build.ps1 [-Out C:\rm\hostapp]
# Run inside Windows (the VM) with the .NET 10 SDK in C:\dotnet (or on the path), as
# windows/viewer/build.ps1 is. scripts/win-host-app.sh does it from Linux.
param([string]$Out = "C:\rm\hostapp")
$ErrorActionPreference = "Stop"
$dotnet = if (Test-Path C:\dotnet\dotnet.exe) { "C:\dotnet\dotnet.exe" } else { "dotnet" }
if (-not $env:DOTNET_CLI_HOME) { $env:DOTNET_CLI_HOME = "$env:LOCALAPPDATA\remoteman-build" }
$env:DOTNET_CLI_TELEMETRY_OPTOUT = 1
$env:DOTNET_NOLOGO = 1
Set-Location $PSScriptRoot
& $dotnet publish RemotemanHost.csproj -c Release -r win-x64 -p:Platform=x64 -o $Out -nologo -clp:"ErrorsOnly;Summary"
if ($LASTEXITCODE -ne 0) { throw "the build failed ($LASTEXITCODE)" }
"built into $Out"
