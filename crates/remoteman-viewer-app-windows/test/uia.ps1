# Driving the Reactor viewer through UI Automation, scoped to one process (the owner's other
# windows are left alone). Dot-source it, then set $global:UiPid and use:
#   Ui-Wait NAME [SECONDS]   Ui-Invoke NAME   Ui-Toggle NAME   Ui-Expand NAME   Ui-Text NAME TEXT
#   Ui-Shot FILE             the process's windows (and popups) only, as a PNG
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public class UiNative {
  [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr v);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
  [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h, out RECT r);
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int size);
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h, IntPtr hdc, uint flags);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int L, T, R, B; }
}
'@ -ErrorAction SilentlyContinue
[UiNative]::SetProcessDpiAwarenessContext([IntPtr](-4)) | Out-Null
$global:UiRoot = [System.Windows.Automation.AutomationElement]::RootElement
$global:UiLog = "C:\rm\shots-viewer\ui.log"
function Ui-Say([string]$s) { Add-Content $UiLog ("{0:HH:mm:ss} {1}" -f (Get-Date), $s) }
function Ui-Tops {
  $c = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ProcessIdProperty, [int]$global:UiPid)
  $UiRoot.FindAll([System.Windows.Automation.TreeScope]::Children, $c)
}
function Ui-Find([string]$name) {
  $c = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $name)
  $text = [System.Windows.Automation.ControlType]::Text
  foreach ($top in Ui-Tops) {
    foreach ($e in $top.FindAll([System.Windows.Automation.TreeScope]::Subtree, $c)) { if ($e.Current.ControlType -ne $text) { return $e } }
  }
  foreach ($top in Ui-Tops) { foreach ($e in $top.FindAll([System.Windows.Automation.TreeScope]::Subtree, $c)) { return $e } }
  $null
}
function Ui-Wait([string]$name, [int]$seconds = 8) {
  for ($i = 0; $i -lt $seconds * 5; $i++) { $e = Ui-Find $name; if ($e) { return $e }; Start-Sleep -Milliseconds 200 }
  Ui-Say "not found: $name"; $null
}
function Ui-Pattern($e, $p) { $o = $null; if ($e -and $e.TryGetCurrentPattern($p, [ref]$o)) { return $o }; $null }
function Ui-Invoke([string]$name) {
  $e = Ui-Wait $name 5; if (-not $e) { return $false }
  $o = Ui-Pattern $e ([System.Windows.Automation.InvokePattern]::Pattern); if ($o) { $o.Invoke(); Ui-Say "invoked: $name"; Start-Sleep -Milliseconds 700; return $true }
  $o = Ui-Pattern $e ([System.Windows.Automation.TogglePattern]::Pattern); if ($o) { $o.Toggle(); Ui-Say "toggled: $name"; Start-Sleep -Milliseconds 700; return $true }
  $o = Ui-Pattern $e ([System.Windows.Automation.SelectionItemPattern]::Pattern); if ($o) { $o.Select(); Ui-Say "selected: $name"; Start-Sleep -Milliseconds 700; return $true }
  $o = Ui-Pattern $e ([System.Windows.Automation.ExpandCollapsePattern]::Pattern); if ($o) { $o.Expand(); Ui-Say "expanded: $name"; Start-Sleep -Milliseconds 700; return $true }
  Ui-Say "can't press: $name"; $false
}
function Ui-Expand([string]$name) {
  $e = Ui-Wait $name 5; if (-not $e) { return $false }
  $o = Ui-Pattern $e ([System.Windows.Automation.ExpandCollapsePattern]::Pattern); if ($o) { $o.Expand(); Ui-Say "expanded: $name"; Start-Sleep -Milliseconds 900; return $true }
  return (Ui-Invoke $name)
}
function Ui-Toggle([string]$name) {
  $e = Ui-Wait $name 5; if (-not $e) { return $false }
  $o = Ui-Pattern $e ([System.Windows.Automation.TogglePattern]::Pattern); if ($o) { $o.Toggle(); Ui-Say "toggled: $name"; Start-Sleep -Milliseconds 700; return $true }
  Ui-Say "can't toggle: $name"; $false
}
function Ui-Text([string]$name, [string]$text) {
  $e = Ui-Wait $name 5; if (-not $e) { return $false }
  $o = Ui-Pattern $e ([System.Windows.Automation.ValuePattern]::Pattern); if ($o) { $o.SetValue($text); Ui-Say "typed into $name"; Start-Sleep -Milliseconds 400; return $true }
  Ui-Say "can't type into $name"; $false
}
function Ui-Collapse([string]$name) {
  $e = Ui-Find $name; if (-not $e) { return }
  $o = Ui-Pattern $e ([System.Windows.Automation.ExpandCollapsePattern]::Pattern); if ($o) { try { $o.Collapse() } catch {}; Ui-Say "collapsed: $name"; Start-Sleep -Milliseconds 500; return }
  # A button's menu (no expand pattern): Escape, once this process has the foreground.
  . "$PSScriptRoot\inp.ps1"
  $main = (Get-Process -Id $global:UiPid).MainWindowHandle
  $p = 0
  for ($try = 0; $try -lt 4 -and $p -ne $global:UiPid; $try++) {
    [Inp]::Key(0x10, $false); [Inp]::Key(0x10, $true)
    [Inp]::SetForegroundWindow($main) | Out-Null
    Start-Sleep -Milliseconds 400
    [UiNative]::GetWindowThreadProcessId([Inp]::GetForegroundWindow(), [ref]$p) | Out-Null
  }
  if ($p -eq $global:UiPid) { [Inp]::Key(0x1B, $false); [Inp]::Key(0x1B, $true); Ui-Say "escaped: $name" } else { Ui-Say "not in front, no Escape for $name" }
  Start-Sleep -Milliseconds 500
}
function Ui-Escape { Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait("{ESC}"); Start-Sleep -Milliseconds 500 }
# The rectangle covering the process's visible top-level windows (its window and its popups).
function Ui-Rect {
  $rects = New-Object System.Collections.ArrayList
  $cb = [UiNative+EnumProc]{ param($h, $l) $p = 0; [UiNative]::GetWindowThreadProcessId($h, [ref]$p) | Out-Null; if ($p -eq $global:UiPid -and [UiNative]::IsWindowVisible($h)) { $r = New-Object UiNative+RECT; if ([UiNative]::DwmGetWindowAttribute($h, 9, [ref]$r, 16) -ne 0) { [UiNative]::GetWindowRect($h, [ref]$r) | Out-Null }; if (($r.R - $r.L) -gt 4 -and ($r.B - $r.T) -gt 4) { [void]$rects.Add($r) } }; $true }
  [UiNative]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
  if ($rects.Count -eq 0) { return $null }
  $l = ($rects | ForEach-Object { $_.L } | Measure-Object -Minimum).Minimum; $t = ($rects | ForEach-Object { $_.T } | Measure-Object -Minimum).Minimum
  $r = ($rects | ForEach-Object { $_.R } | Measure-Object -Maximum).Maximum; $b = ($rects | ForEach-Object { $_.B } | Measure-Object -Maximum).Maximum
  @($l, $t, $r, $b)
}
# The process's visible windows (bottom first), each drawn by Windows from its own content
# (PrintWindow, PW_RENDERFULLCONTENT), so whatever else is on the desktop over them isn't in it.
function Ui-ShotPrint([string]$file) {
  Start-Sleep -Milliseconds 500
  $wins = New-Object System.Collections.ArrayList
  $cb = [UiNative+EnumProc]{ param($h, $l) $p = 0; [UiNative]::GetWindowThreadProcessId($h, [ref]$p) | Out-Null; if ($p -eq $global:UiPid -and [UiNative]::IsWindowVisible($h)) { $r = New-Object UiNative+RECT; [UiNative]::GetWindowRect($h, [ref]$r) | Out-Null; if (($r.R - $r.L) -gt 4 -and ($r.B - $r.T) -gt 4) { [void]$wins.Add(@($h, $r)) } }; $true }
  [UiNative]::EnumWindows($cb, [IntPtr]::Zero) | Out-Null
  if ($wins.Count -eq 0) { Ui-Say "no window to shoot for $file"; return }
  $l = ($wins | ForEach-Object { $_[1].L } | Measure-Object -Minimum).Minimum; $t = ($wins | ForEach-Object { $_[1].T } | Measure-Object -Minimum).Minimum
  $r = ($wins | ForEach-Object { $_[1].R } | Measure-Object -Maximum).Maximum; $b = ($wins | ForEach-Object { $_[1].B } | Measure-Object -Maximum).Maximum
  [int]$w = $r - $l; [int]$h = $b - $t
  $image = New-Object System.Drawing.Bitmap $w, $h
  $g = [System.Drawing.Graphics]::FromImage($image)
  $g.Clear([System.Drawing.Color]::Transparent)
  for ($i = $wins.Count - 1; $i -ge 0; $i--) {
    $hw = $wins[$i][0]; $rc = $wins[$i][1]
    [int]$ww = $rc.R - $rc.L; [int]$wh = $rc.B - $rc.T
    $one = New-Object System.Drawing.Bitmap $ww, $wh
    $og = [System.Drawing.Graphics]::FromImage($one)
    $dc = $og.GetHdc(); [UiNative]::PrintWindow($hw, $dc, 2) | Out-Null; $og.ReleaseHdc($dc); $og.Dispose()
    $g.DrawImage($one, [int]($rc.L - $l), [int]($rc.T - $t))
    $one.Dispose()
  }
  $image.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $image.Dispose()
  Ui-Say "shot: $file ($w x $h, $($wins.Count) windows)"
}# The screen where the process's windows are (the window is made topmost by Ui-Topmost first,
# so other programs' windows don't come over it): its popups' content is only seen this way.
function Ui-Shot([string]$file) {
  Start-Sleep -Milliseconds 500
  $r = Ui-Rect; if (-not $r) { Ui-Say "no window to shoot for $file"; return }
  [int]$w = $r[2] - $r[0]; [int]$h = $r[3] - $r[1]
  $image = New-Object System.Drawing.Bitmap $w, $h
  $g = [System.Drawing.Graphics]::FromImage($image)
  $g.CopyFromScreen([int]$r[0], [int]$r[1], 0, 0, $image.Size)
  $image.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $image.Dispose()
  Ui-Say "shot: $file ($w x $h)"
}
function Ui-Topmost {
  Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class UiTop { [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags); }' -ErrorAction SilentlyContinue
  $main = (Get-Process -Id $global:UiPid).MainWindowHandle
  [UiTop]::SetWindowPos($main, [IntPtr](-1), 0, 0, 0, 0, 0x0013) | Out-Null
  Ui-Say "topmost: $main"
}