# Full screen's bar and its menus (the app's WinUI ones, in a window of their own over the top of
# the engine's full-screen window), through UI Automation and the log, on the made-up session of
# the walk (--ui). Full screen is up for seconds only; the pointer is never moved. Prints each
# menu's items with their enabled / checked / chosen state, toggles the variable refresh rate and
# the system shortcuts from the bar and looks for the log's word, and says who has the foreground.
# (The bar goes two seconds after it was brought out, so each step brings it out again: the engine's
# "bar-edge" command stands for the pointer reaching the top edge.)
param([string]$Exe = "$PSScriptRoot\..\..\..\target\release\remoteman-app.exe", [string]$Out = "C:\rm\shots-viewer", [string]$Clip = "", [string[]]$More = @())
$sp = $PSScriptRoot
. "$sp\uia.ps1"
Add-Type -TypeDefinition 'using System; using System.Text; using System.Runtime.InteropServices; public class Fg { [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow(); [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassName(IntPtr h, StringBuilder s, int n); public static string Class(IntPtr h) { var s = new StringBuilder(64); GetClassName(h, s, 64); return s.ToString(); } }' -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $Out | Out-Null
$walkFile = Join-Path $env:TEMP "remoteman-walk-step-fs.txt"
Set-Content $walkFile ""
$env:REMOTEMAN_WALK_FILE = $walkFile
$log = Join-Path $Out "fsbar.log"
$report = Join-Path $Out "fsbar.txt"
Remove-Item $report -ErrorAction SilentlyContinue
$script:k = 0
function Rep([string]$s) { Add-Content $report $s }
function Send([string]$w) { Set-Content $walkFile $w; Start-Sleep -Milliseconds 1200 }
function Peek { $script:k++; Send ('{"do":"bar-edge","n":' + $script:k + '}') }
function Logged([string]$pattern, [int]$seconds = 5) { for ($i = 0; $i -lt $seconds * 5; $i++) { if ((Get-Content $log -ErrorAction SilentlyContinue) -match $pattern) { return $true }; Start-Sleep -Milliseconds 200 }; $false }
function Items {
  foreach ($top in Ui-Tops) {
    $type = [System.Windows.Automation.AutomationElement]::ControlTypeProperty
    $mi = New-Object System.Windows.Automation.OrCondition((New-Object System.Windows.Automation.PropertyCondition($type, [System.Windows.Automation.ControlType]::MenuItem)), (New-Object System.Windows.Automation.PropertyCondition($type, [System.Windows.Automation.ControlType]::RadioButton)), (New-Object System.Windows.Automation.PropertyCondition($type, [System.Windows.Automation.ControlType]::Slider)))
    foreach ($e in $top.FindAll([System.Windows.Automation.TreeScope]::Subtree, $mi)) {
      $state = ""
      $t = Ui-Pattern $e ([System.Windows.Automation.TogglePattern]::Pattern); if ($t) { $state += " toggle=" + $t.Current.ToggleState }
      $s = Ui-Pattern $e ([System.Windows.Automation.SelectionItemPattern]::Pattern); if ($s) { $state += " selected=" + $s.Current.IsSelected }
      Rep ("    {0,-60} enabled={1}{2}" -f $e.Current.Name, $e.Current.IsEnabled, $state)
    }
  }
}
function Fore { $h = [Fg]::GetForegroundWindow(); $p = 0; [UiNative]::GetWindowThreadProcessId($h, [ref]$p) | Out-Null; $o = ""; if ($p -eq $global:UiPid) { $o = ", ours" }; "{0} (pid {1}{2})" -f [Fg]::Class($h), $p, $o }
# Escape closes a menu, if the bar window (and so its menu) has the keyboard; nothing is sent to anyone else's window.
function Esc { if ((Fore) -like "WinUIDesktopWin32WindowClass*ours*") { Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait("{ESC}"); Start-Sleep -Milliseconds 600 } }
function Safely([scriptblock]$what) { try { & $what } catch { Rep "error: $($_.Exception.Message)" } }
$id = $null
try {
  if ($Clip) {
    # A played clip as the session (what a computer that sends HDR, and offers what the list says, would be).
    $words = @("--ui", "none", "--shots", "$Out\fs", "--play", $Clip, "--play-colour", "bt2020-pq", "--play-rate", "60", "--size", "1920x1080", "--play-features", "placement,mirror,configure,sound,clipboard,files,displays")
  } else {
    $words = @("--ui", "all", "--shots", "$Out\fs")
  }
  $id = & "$sp\launch.ps1" -Exe $Exe -Log $log -State "C:\rm\state-viewer" -Seconds 45 -More ($words + @("--dark", "--pos", "-7500,-1900", "--window", "1100x720", "--screen-hdr", "yes") + $More)
  $global:UiPid = $id
  Rep "viewer pid $id"
  if ($Clip) { [void](Logged "play: " 8) } else { Send "session"; [void](Logged "walk: session$") }
  Send '{"do":"fullscreen"}'
  Rep ("full screen up: " + (Logged "full screen: a window of its own" 5))
  Send '{"do":"bar-edge"}'
  Rep ("bar out: " + (Logged "full screen bar: out" 5))
  Rep ("foreground: " + (Fore))
  $bar = Ui-Wait "Session bar" 5
  Rep ("session bar found: " + [bool]$bar)
  if ($bar) {
    foreach ($b in $bar.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)) {
      if ($b.Current.ControlType -in @([System.Windows.Automation.ControlType]::Button, [System.Windows.Automation.ControlType]::SplitButton)) {
        Rep ("  button {0,-36} enabled={1}" -f $b.Current.Name, $b.Current.IsEnabled)
      }
    }
  }
  foreach ($menu in "Send keys", "Display", "Picture", "Clipboard and files", "More") {
    Peek
    Rep ("menu: $menu")
    Safely { [void](Ui-Expand $menu) }
    Safely { Items }
    Rep ("  foreground: " + (Fore))
    Esc
  }
  Rep "toggling from the bar"
  Peek
  Safely { [void](Ui-Expand "Display") }
  Safely { [void](Ui-Invoke "HDR") }
  Start-Sleep -Milliseconds 600
  Peek
  Safely { [void](Ui-Expand "Display") }
  Safely { [void](Ui-Invoke "Variable refresh rate") }
  Start-Sleep -Milliseconds 600
  Peek
  Safely { [void](Ui-Expand "Send keys") }
  Safely { [void](Ui-Invoke "Send system shortcuts to the computer") }
  Start-Sleep -Milliseconds 600
  Peek
  Safely { [void](Ui-Expand "Display") }
  Safely { Items }
  Esc
  Rep "log lines about them:"
  Get-Content $log | Select-String -Pattern "setting:|variable refresh|full screen" | ForEach-Object { Rep ("    " + $_.Line) }
  Peek
  Safely { Rep ("leave: " + (Ui-Invoke "Leave full screen (F11)")) }
  Rep ("left: " + (Logged "full screen: left" 5))
} finally {
  Remove-Item Env:REMOTEMAN_WALK_FILE -ErrorAction SilentlyContinue
  if ($id) { Stop-Process -Id $id -Force -ErrorAction SilentlyContinue }
  Remove-Item "$Out\fs" -Recurse -Force -ErrorAction SilentlyContinue
}
Get-Content $report
