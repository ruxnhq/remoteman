#!/bin/bash
# Builds Remoteman Host, the Windows host's WinUI 3 app (windows/host), in the VM, from this PC:
#   scripts/win-host-app.sh
# Sends windows/host into the guest (C:\rm\hostsrc) and runs its build.ps1 there, which leaves
# the app in C:\rm\hostapp. Hold the VM first (tools/vm.sh take NAME). The app needs no DLL of
# ours: it speaks to remoteman-host.exe over the host's control pipe.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
T=/mnt/data/vms/remoteman-win
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
mkdir -p "$work/host"
(cd "$root/windows/host" && tar --exclude=bin --exclude=obj -cf - .) | tar -xf - -C "$work/host"
tar -C "$work" -czf "$work/hostapp.tar.gz" host
bash $T/tools/vm.sh put "$work/hostapp.tar.gz" >/dev/null
bash $T/tools/vm.sh run '
  $ErrorActionPreference = "Stop"
  if (Test-Path C:\rm\hostsrc) { Get-ChildItem C:\rm\hostsrc -Exclude obj,bin | Remove-Item -Recurse -Force }
  New-Item -ItemType Directory -Force C:\rm\hostsrc | Out-Null
  tar.exe -xzf C:\rm\hostapp.tar.gz -C C:\rm\hostsrc --strip-components 1
  $env:DOTNET_CLI_HOME = "C:\rm\dnhome"; $env:NUGET_PACKAGES = "C:\rm\nuget"
  & powershell -NoProfile -ExecutionPolicy Bypass -File C:\rm\hostsrc\build.ps1 -Out C:\rm\hostapp 2>&1 | Select-Object -Last 40
' 1500
