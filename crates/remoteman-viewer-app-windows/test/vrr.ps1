# Variable refresh in the app's full screen, with no input sent: a played session started in full
# screen (--fullscreen) on DISPLAY1 at -Rate pictures a second, the vertical blanks a line a
# second, ended by --seconds (and a kill by pid after).
param([string]$Name, [string]$Clip = "C:\rm\clips-a\t420-1920x1080.hevc", [string]$Colour = "bt709", [int]$Rate = 100,
      [string]$Format = "10", [int]$Seconds = 20, [string]$Exe = "$PSScriptRoot\..\..\..\target\release\remoteman-app.exe")
$sp = "$PSScriptRoot"
$env:REMOTEMAN_VBLANK_SECONDS = "1"
$env:REMOTEMAN_FULL_FORMAT = $Format
$log = "C:\rm\shots-viewer\vrr\full-$Name.log"
$more = @("--pos", "200,200", "--play", $Clip, "--play-colour", $Colour, "--play-rate", "$Rate", "--size", "1920x1080", "--watch-vblank", "--fullscreen")
$id = & "$sp\launch.ps1" -Exe $Exe -Log $log -Seconds $Seconds -More $more
$p = Get-Process -Id $id -ErrorAction SilentlyContinue
if ($p) { if (-not $p.WaitForExit(($Seconds + 15) * 1000)) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue } }
Remove-Item Env:REMOTEMAN_FULL_FORMAT -ErrorAction SilentlyContinue
"done $Name"
