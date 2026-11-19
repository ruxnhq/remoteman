#!/bin/bash
# Builds the WinUI 3 Windows viewer in the VM, from this PC, in one command:
#   scripts/win-viewer-winui.sh [--no-engine] [--msix]
# (--msix: the Store's package instead, into C:\rm\msix, signed with a test certificate
#  of the guest's own so that it can be installed there; docs/WINDOWS-PACKAGING.md)
# 1. cross-builds the engine (remoteman_engine.dll, crates/remoteman-viewer-windows) with
#    cargo-xwin, as the old program is built (docs/HANDOFF.md, "The VM and the tools");
# 2. sends windows/viewer, the engine and FFmpeg's DLLs into the guest (C:\rm\winui);
# 3. runs windows/viewer/build.ps1 there, which leaves the whole app in C:\rm\app.
# Hold the VM first (tools/vm.sh take NAME). Inside Windows alone, build.ps1 is the command.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
T=/mnt/data/vms/remoteman-win
work=$(mktemp -d); trap 'rm -rf "$work"' EXIT
mkdir -p "$work/viewer/native"
engine=1; build='-Out C:\rm\app'
for a in "$@"; do case $a in --no-engine) engine=0 ;; --msix) build='-Packaged -TestCert -Out C:\rm\msix' ;; esac; done
if [ $engine = 1 ]; then
  (cd "$root" && FFMPEG_DIR=$T/ffmpeg XWIN_ACCEPT_LICENSE=1 cargo xwin build --release --locked \
     --target x86_64-pc-windows-msvc -p remoteman-viewer-windows --lib)
  cp "$root/target/x86_64-pc-windows-msvc/release/remoteman_engine.dll" "$work/viewer/native/"
  # Only the FFmpeg libraries the engine loads: avcodec and what it links (avutil,
  # swresample). avformat, avfilter, avdevice and swscale are not needed.
  for d in avcodec-63 avutil-61 swresample-7; do cp "$T/ffmpeg/bin/$d.dll" "$work/viewer/native/"; done
fi
(cd "$root/windows/viewer" && tar --exclude=bin --exclude=obj --exclude=native -cf - .) | tar -xf - -C "$work/viewer"
tar -C "$work" -czf "$work/winui.tar.gz" viewer
bash $T/tools/vm.sh put "$work/winui.tar.gz" >/dev/null
bash $T/tools/vm.sh run '
  $ErrorActionPreference = "Stop"
  if (Test-Path C:\rm\winui) { Get-ChildItem C:\rm\winui -Exclude obj,bin,native | Remove-Item -Recurse -Force }
  if ('$engine' -eq "1") { Remove-Item -Recurse -Force C:\rm\winui\native -ErrorAction SilentlyContinue }
  New-Item -ItemType Directory -Force C:\rm\winui | Out-Null
  tar.exe -xzf C:\rm\winui.tar.gz -C C:\rm\winui --strip-components 1
  $env:DOTNET_CLI_HOME = "C:\rm\dnhome"; $env:NUGET_PACKAGES = "C:\rm\nuget"
  & powershell -NoProfile -ExecutionPolicy Bypass -File C:\rm\winui\build.ps1 '"$build"' 2>&1 | Select-Object -Last 40
' 1500
