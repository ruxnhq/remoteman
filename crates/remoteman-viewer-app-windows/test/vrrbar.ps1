# Full screen on DISPLAY1 at 100 pictures a second: the pointer goes to the top edge (the bar
# comes out, its layer over the picture), then away (the bar goes, the layer with it), a click on
# the picture; the vertical blanks a line a second all along. Input is sent only while the
# full-screen window is the one under the pointer.
param([string]$Name = "bar", [string]$Exe = "$PSScriptRoot\..\..\..\target\release\remoteman-app.exe")
$sp = $PSScriptRoot
. "$sp\inp.ps1"
Add-Type -TypeDefinition 'using System; using System.Text; using System.Runtime.InteropServices; public class Wfp { [StructLayout(LayoutKind.Sequential)] public struct P { public int X, Y; } [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(P p); [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid); [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n); [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v); }' -ErrorAction SilentlyContinue
[Wfp]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null
function Ours([int]$x, [int]$y) {
  $p = New-Object Wfp+P; $p.X = $x; $p.Y = $y
  $h = [Wfp]::WindowFromPoint($p); $pid2 = 0; [Wfp]::GetWindowThreadProcessId($h, [ref]$pid2) | Out-Null
  $c = New-Object Text.StringBuilder 64; [Wfp]::GetClassName($h, $c, 64) | Out-Null
  ($pid2 -eq $script:id) -and ($c.ToString() -eq "remoteman-engine-full")
}
function Say([string]$s) { Add-Content $log ("{0:HH:mm:ss.fff} test: {1}" -f (Get-Date), $s) }
$env:REMOTEMAN_VBLANK_SECONDS = "1"
$log = "C:\rm\shots-viewer\vrr\full-$Name.log"
$script:id = & "$sp\launch.ps1" -Exe $Exe -Log $log -Seconds 40 -More @("--pos", "200,200", "--play", "C:\rm\clips-a\t420-1920x1080.hevc", "--play-rate", "100", "--size", "1920x1080", "--watch-vblank", "--fullscreen")
Start-Sleep -Seconds 5
$was = New-Object Inp+POINT; [Inp]::GetCursorPos([ref]$was) | Out-Null
if (Ours 1920 1) {
  Say "pointer to the middle"
  [Inp]::SetCursorPos(1920, 1100) | Out-Null; [Inp]::Move(1, 1); Start-Sleep -Milliseconds 300
  Say "a click on the picture"
  [Inp]::Mouse(0x0002, 0); Start-Sleep -Milliseconds 60; [Inp]::Mouse(0x0004, 0); Start-Sleep -Milliseconds 300
  Say "pointer to the top edge"
  [Inp]::SetCursorPos(1920, 40) | Out-Null; foreach ($k in 1..14) { [Inp]::Move(0, -3); Start-Sleep -Milliseconds 30 }
  Start-Sleep -Seconds 4
  if (Ours 1920 1200) {
    Say "pointer away"
    [Inp]::SetCursorPos(1920, 1200) | Out-Null; foreach ($k in 1..5) { [Inp]::Move(2, 2); Start-Sleep -Milliseconds 30 }
  } else { Say "not ours under the pointer: nothing more sent" }
  Start-Sleep -Seconds 8
} else { Say "the full-screen window isn't under the pointer: nothing sent" }
[Inp]::SetCursorPos($was.X, $was.Y) | Out-Null
Start-Sleep -Seconds 1
Stop-Process -Id $script:id -Force -ErrorAction SilentlyContinue
"done"
