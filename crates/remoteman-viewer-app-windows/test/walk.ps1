# Every page, dialog and menu of the Reactor viewer, in light and dark, as pictures of its window
# only: the engine is put into each state by the old program's walk steps (--ui: a made-up list,
# no network), the menus are opened through UI Automation.
param([string[]]$Themes = @("light", "dark"), [string]$Exe = "$PSScriptRoot\..\..\..\target\release\remoteman-app.exe",
      [string]$Out = "C:\rm\shots-viewer")
$sp = "$PSScriptRoot"
. "$sp\uia.ps1"
$steps = "empty,list,pair,pair-waiting,pair-failed,code,paired,connecting,asleep,noanswer,notpaired,changed,refused,ended,loggingin,locked,settings,computer,address,forget,about,session,hover,session-settings" -split ","
function Scroll-End {
  foreach ($top in Ui-Tops) {
    $c = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::IsScrollPatternAvailableProperty, $true)
    foreach ($e in $top.FindAll([System.Windows.Automation.TreeScope]::Descendants, $c)) {
      $o = Ui-Pattern $e ([System.Windows.Automation.ScrollPattern]::Pattern)
      if ($o -and $o.Current.VerticallyScrollable) { $o.SetScrollPercent(-1, 100); Start-Sleep -Milliseconds 600; return $true }
    }
  }
  $false
}
foreach ($theme in $Themes) {
  $dir = Join-Path $Out $theme
  New-Item -ItemType Directory -Force $dir | Out-Null
  $walkFile = Join-Path $env:TEMP "remoteman-walk-step.txt"
  Set-Content $walkFile ""
  $env:REMOTEMAN_WALK_FILE = $walkFile
  $log = Join-Path $Out "walk-$theme.log"
  $id = & "$sp\launch.ps1" -Exe $Exe -Log $log -State "C:\rm\state-viewer" -Seconds 600 -More @("--ui", "all", "--shots", $dir, "--$theme", "--pos", "-7500,-1900", "--window", "1100x720")
  Remove-Item Env:REMOTEMAN_WALK_FILE
  $global:UiPid = $id
  Start-Sleep -Seconds 2
  Ui-Topmost
  $n = 0
  foreach ($step in $steps) {
    $n++
    Set-Content $walkFile $step
    for ($i = 0; $i -lt 40; $i++) { Start-Sleep -Milliseconds 150; if ((Get-Content $log -ErrorAction SilentlyContinue) -match "walk: $step$") { break } }
    Start-Sleep -Milliseconds 1200
    $name = "{0:D2}-{1}" -f $n, $step
    Ui-Shot (Join-Path $dir "$name.png")
    switch ($step) {
      "list" {
        if (Ui-Expand "More for Workshop PC") { Ui-Shot (Join-Path $dir "$name-card-menu.png"); Ui-Collapse "More for Workshop PC" }
      }
      { $_ -in "settings", "computer" } {
        if (Scroll-End) { Ui-Shot (Join-Path $dir "$name-end.png") }
      }
      "session" {
        foreach ($menu in "Send keys", "Display", "Picture", "Clipboard and files", "More") {
          if (Ui-Expand $menu) { Start-Sleep -Milliseconds 400; Ui-Shot (Join-Path $dir ("$name-" + ($menu -replace ' ', '-').ToLower() + ".png")); Ui-Collapse $menu }
        }
        if (Ui-Expand "Display") { Start-Sleep -Milliseconds 700; if (Ui-Expand "Size") { Start-Sleep -Milliseconds 400; Ui-Shot (Join-Path $dir "$name-display-size.png"); Ui-Collapse "Size" }; Ui-Collapse "Display" }
      }
    }
  }
  Stop-Process -Id $id -Force -ErrorAction SilentlyContinue
  "walked $theme"
}
