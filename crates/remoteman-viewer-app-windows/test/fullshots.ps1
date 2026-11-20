# Full screen's bar and a menu of it, as the engine's draw thread saves them (the picture's stage
# and the interface's layer), with the made-up session of the walk.
param([string[]]$Themes = @("light", "dark"), [string]$Exe = "$PSScriptRoot\..\..\..\target\release\remoteman-app.exe")
$sp = $PSScriptRoot
foreach ($theme in $Themes) {
  $walkFile = Join-Path $env:TEMP "remoteman-walk-step.txt"
  Set-Content $walkFile ""
  $env:REMOTEMAN_WALK_FILE = $walkFile
  $log = "C:\rm\shots-viewer\full-$theme.log"
  $id = & "$sp\launch.ps1" -Exe $Exe -Log $log -State "C:\rm\state-viewer" -Seconds 120 -More @("--ui", "all", "--shots", "C:\rm\shots-viewer", "--$theme", "--pos", "-7500,-1900", "--window", "1100x720")
  Remove-Item Env:REMOTEMAN_WALK_FILE
  $dir = "C:\rm\shots-viewer\$theme"
  $send = { param($w) Set-Content $walkFile $w; Start-Sleep -Milliseconds 1500 }
  & $send "hover"
  & $send '{"do":"fullscreen"}'
  Start-Sleep -Seconds 2
  & $send ('{"do":"shot","path":"' + ($dir -replace '\\', '\\') + '\\25-fullscreen-bar.png"}')
  & $send "hover-keys"
  & $send ('{"do":"shot","path":"' + ($dir -replace '\\', '\\') + '\\26-fullscreen-keys.png"}')
  & $send '{"do":"fullscreen"}'
  Start-Sleep -Seconds 1
  Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
  "full screen pictures: $theme"
}
