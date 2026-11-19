# Builds the WinUI 3 viewer into one folder that runs with nothing installed:
#   powershell -File build.ps1 [-Out C:\rm\app]
# Run inside Windows (the VM) with the .NET 10 SDK in C:\dotnet (or on the path). The engine,
# remoteman_engine.dll, and FFmpeg's DLLs are taken from .\native\ if they are there
# (scripts/win-viewer-winui.sh puts them there).
param([string]$Out = "C:\rm\app")
$ErrorActionPreference = "Stop"
$dotnet = if (Test-Path C:\dotnet\dotnet.exe) { "C:\dotnet\dotnet.exe" } else { "dotnet" }
if (-not $env:DOTNET_CLI_HOME) { $env:DOTNET_CLI_HOME = "$env:LOCALAPPDATA\remoteman-build" }
$env:DOTNET_CLI_TELEMETRY_OPTOUT = 1
$env:DOTNET_NOLOGO = 1
Set-Location $PSScriptRoot
& $dotnet publish Remoteman.csproj -c Release -r win-x64 -p:Platform=x64 -o $Out -nologo -clp:"ErrorsOnly;Summary"
if ($LASTEXITCODE -ne 0) { throw "the build failed ($LASTEXITCODE)" }
"built into $Out"
