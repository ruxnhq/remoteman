#!/bin/bash
# Builds the Windows host's installer in the VM, from this PC, in one command:
#   scripts/win-host-installer.sh [VERSION]
# 1. cross-builds remoteman-host.exe with cargo-xwin (docs/HANDOFF.md, "The VM and the tools");
# 2. sends it, windows/host-installer and the display driver's files (Guesthouse's user-mode display
#    driver, from $GUESTHOUSE_DRIVER, by default the VM's copy) into the guest (C:\rm\hostpkg);
# 3. runs windows/host-installer/build.ps1 there, which leaves C:\rm\installer\remoteman-host-setup-VERSION.exe.
# Hold the VM first (tools/vm.sh take NAME). Inside Windows alone, build.ps1 is the command.
# docs/WINDOWS-PACKAGING.md has the rest.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
T=/mnt/data/vms/remoteman-win
version=${1:-0.1.0}
driver=${GUESTHOUSE_DRIVER:-$T/program/display-driver}
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
mkdir -p "$work/host/native"
(cd "$root" && XWIN_ACCEPT_LICENSE=1 cargo xwin build --release --locked \
   --target x86_64-pc-windows-msvc -p remoteman-host-windows)
cp "$root/target/x86_64-pc-windows-msvc/release/remoteman-host.exe" "$work/host/native/"
for f in GuesthouseDisplay.dll GuesthouseDisplay.inf vdd_settings.xml LICENSE LICENSE-MS-PL; do cp "$driver/$f" "$work/host/native/"; done
# The host can give a device a monitor of its own and a second display: the driver's first
# monitor plus two more (the host keeps those out of the desktop until a device takes one).
sed -i 's|<count>1</count>|<count>3</count>|' "$work/host/native/vdd_settings.xml"
cp "$root"/windows/host-installer/{build.ps1,setup.ps1,remoteman-host.iss} "$work/host/"
tar -C "$work" -czf "$work/hostpkg.tar.gz" host
bash $T/tools/vm.sh put "$work/hostpkg.tar.gz" >/dev/null
bash $T/tools/vm.sh run '
  $ErrorActionPreference = "Stop"
  Remove-Item -Recurse -Force C:\rm\hostpkg -ErrorAction SilentlyContinue
  New-Item -ItemType Directory -Force C:\rm\hostpkg | Out-Null
  tar.exe -xzf C:\rm\hostpkg.tar.gz -C C:\rm\hostpkg --strip-components 1
  & powershell -NoProfile -ExecutionPolicy Bypass -File C:\rm\hostpkg\build.ps1 -Version '"$version"' 2>&1 | Select-Object -Last 20
' 600
