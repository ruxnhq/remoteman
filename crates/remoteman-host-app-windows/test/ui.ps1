# Driving Remoteman Host (remoteman-host-app.exe) in tests with nobody at the keyboard, through UI
# Automation (what screen readers use), against test\fakehost.ps1. Dot-source it, then:
#   $global:UiPid = (Start-Process -PassThru remoteman-host-app.exe -ArgumentList '--port 7899').Id
#   Ui-Wait NAME [SECONDS]      wait until a control of that name is on screen (the app's own)
#   Ui-Invoke NAME              press it (a button, a menu item, a navigation item)
#   Ui-Toggle NAME              flip a switch
#   Ui-IsOn NAME                a switch's state
#   Ui-Front / Ui-Place X Y     bring the window forward / put it there
#   Ui-Shot FILE                the window as a PNG (what is on screen there)
#   Ui-ShotAround NAME FILE     the area around a control (the tray menu) as a PNG
#   Tray-Menu                   open the icon's menu as a right click on it would
#   Fake-Say WORDS              tell the fake host something (as a device would make happen)
# Everything is scoped to the app's process, since other WinUI windows may be on the desktop.
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms, System.Drawing
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices; using System.Text; using System.Collections.Generic;
public class HostUi {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, uint msg, IntPtr w, IntPtr l);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr h, StringBuilder s, int n);
  [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h, out uint pid);
  public delegate bool EnumProc(IntPtr h, IntPtr l);
  [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f, IntPtr l);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int size);
  public static List<IntPtr> WindowsOf(uint pid, string title) {
    var found = new List<IntPtr>();
    EnumWindows((h, l) => { uint p; GetWindowThreadProcessId(h, out p); var sb = new StringBuilder(256); GetWindowText(h, sb, 256);
      if (p == pid && (title == null || sb.ToString() == title)) found.Add(h); return true; }, IntPtr.Zero);
    return found;
  }
  public static string Foreground() { var sb = new StringBuilder(256); GetWindowText(GetForegroundWindow(), sb, 256); return sb.ToString(); }
}
'@
[HostUi]::SetProcessDPIAware() | Out-Null
$global:UiRoot = [System.Windows.Automation.AutomationElement]::RootElement
if (-not $global:UiLog) { $global:UiLog = "C:\rm\shots-host\ui.log" }
if (-not $global:UiPort) { $global:UiPort = 7899 }
function Ui-Say([string]$line) { Add-Content $global:UiLog ("{0} {1}" -f (Get-Date -Format "HH:mm:ss.fff"), $line) }
function Ui-Window {
  $c = New-Object System.Windows.Automation.AndCondition(
    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ProcessIdProperty, $global:UiPid)),
    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ClassNameProperty, 'WinUIDesktopWin32WindowClass')))
  $UiRoot.FindFirst([System.Windows.Automation.TreeScope]::Children, $c)
}
function Ui-Tops {
  # The app's top-level windows: the main one, and popups (the tray menu's host).
  $c = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ProcessIdProperty, $global:UiPid)
  $UiRoot.FindAll([System.Windows.Automation.TreeScope]::Children, $c)
}
function Ui-Find([string]$name) {
  $c = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $name)
  $text = [System.Windows.Automation.ControlType]::Text
  foreach ($top in @(Ui-Tops)) {
    foreach ($e in $top.FindAll([System.Windows.Automation.TreeScope]::Descendants, $c)) { if ($e.Current.ControlType -ne $text) { return $e } }
  }
  # Words only (a text block), when nothing else has that name.
  foreach ($top in @(Ui-Tops)) { $e = $top.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c); if ($e) { return $e } }
  $null
}
function Ui-Wait([string]$name, [int]$seconds = 10) {
  for ($i = 0; $i -lt $seconds * 5; $i++) { $e = Ui-Find $name; if ($e) { return $e }; Start-Sleep -Milliseconds 200 }
  Ui-Say "not found: $name"; $null
}
function Ui-Gone([string]$name, [int]$seconds = 10) {
  for ($i = 0; $i -lt $seconds * 5; $i++) { if (-not (Ui-Find $name)) { return $true }; Start-Sleep -Milliseconds 200 }
  Ui-Say "still there: $name"; $false
}
function Ui-Invoke([string]$name) {
  $e = Ui-Wait $name 5
  if (-not $e) { return }
  foreach ($p in @([System.Windows.Automation.InvokePattern]::Pattern, [System.Windows.Automation.SelectionItemPattern]::Pattern, [System.Windows.Automation.TogglePattern]::Pattern, [System.Windows.Automation.ExpandCollapsePattern]::Pattern)) {
    $o = $null
    if ($e.TryGetCurrentPattern($p, [ref]$o)) {
      switch ($p.ProgrammaticName) {
        'InvokePatternIdentifiers.Pattern' { $o.Invoke() }
        'SelectionItemPatternIdentifiers.Pattern' { $o.Select() }
        'TogglePatternIdentifiers.Pattern' { $o.Toggle() }
        'ExpandCollapsePatternIdentifiers.Pattern' { $o.Expand() }
      }
      Ui-Say "pressed: $name"; Start-Sleep -Milliseconds 700; return
    }
  }
  Ui-Say "can't press: $name"
}
function Ui-Toggle([string]$name) {
  $e = Ui-Wait $name 5
  $o = $null
  if ($e -and $e.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$o)) { $o.Toggle(); Ui-Say "toggled: $name"; Start-Sleep -Milliseconds 700 }
  else { Ui-Say "can't toggle: $name" }
}
function Ui-IsOn([string]$name) {
  $e = Ui-Wait $name 5
  $o = $null
  if ($e -and $e.TryGetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern, [ref]$o)) { return $o.Current.ToggleState -eq 'On' }
  $null
}
function Ui-Hwnd { $w = Ui-Window; if ($w) { [IntPtr]$w.Current.NativeWindowHandle } else { [IntPtr]::Zero } }
function Ui-Front {
  $h = Ui-Hwnd
  # An Alt tap lets this process hand the foreground over.
  [HostUi]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero); [HostUi]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
  [HostUi]::ShowWindow($h, 9) | Out-Null
  [HostUi]::SetForegroundWindow($h) | Out-Null
  Start-Sleep -Milliseconds 500
}
function Ui-Place([int]$x, [int]$y) { [HostUi]::SetWindowPos((Ui-Hwnd), [IntPtr]::Zero, $x, $y, 0, 0, 0x0001 -bor 0x0004) | Out-Null; Start-Sleep -Milliseconds 300 }
function Ui-Rect { $r = New-Object HostUi+RECT; [HostUi]::DwmGetWindowAttribute((Ui-Hwnd), 9, [ref]$r, 16) | Out-Null; $r }
function Ui-ShotRect([int]$left, [int]$top, [int]$width, [int]$height, [string]$file) {
  $image = New-Object System.Drawing.Bitmap $width, $height
  $g = [System.Drawing.Graphics]::FromImage($image)
  $g.CopyFromScreen($left, $top, 0, 0, $image.Size)
  $image.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $image.Dispose()
  Ui-Say "shot: $file"
}
function Ui-Shot([string]$file) { $r = Ui-Rect; Ui-ShotRect $r.Left $r.Top ($r.Right - $r.Left) ($r.Bottom - $r.Top) $file }
function Ui-ShotAround([string]$name, [string]$file, [int]$margin = 24) {
  $e = Ui-Wait $name 5
  if (-not $e) { return }
  $b = $e.Current.BoundingRectangle
  Ui-ShotRect ([int]$b.Left - $margin) ([int]$b.Top - $margin) ([int]$b.Width + 2 * $margin) ([int]$b.Height + 2 * $margin) $file
}
function Tray-Menu([int]$x = 1700, [int]$y = 1000) {
  # Reactor's notification icon answers on a hidden window of the app's called
  # "windows-notifyicon" (version 4: the event in lParam's low word, the anchor in wParam).
  $h = [HostUi]::WindowsOf([uint32]$global:UiPid, "windows-notifyicon") | Select-Object -First 1
  if (-not $h) { Ui-Say "no notification icon window"; return }
  # The icon's menu always ends with Quit (the window's own system menu has no such item).
  $c = New-Object System.Windows.Automation.AndCondition(
    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::MenuItem)),
    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, "Quit remoteman")))
  # Asked again if it doesn't come (the first one an app shows takes a moment to make).
  for ($try = 0; $try -lt 3; $try++) {
    [HostUi]::PostMessage($h, 0x401, [IntPtr](($y -shl 16) -bor $x), [IntPtr]((1 -shl 16) -bor 0x7B)) | Out-Null
    Ui-Say "tray: context menu asked for"
    for ($i = 0; $i -lt 10; $i++) {
      Start-Sleep -Milliseconds 300
      foreach ($t in Ui-Tops) { if ($t.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c)) { Start-Sleep -Milliseconds 300; return } }
    }
  }
  Ui-Say "tray: no menu came"
}
function Tray-Click {
  $h = [HostUi]::WindowsOf([uint32]$global:UiPid, "windows-notifyicon") | Select-Object -First 1
  [HostUi]::PostMessage($h, 0x401, [IntPtr]::Zero, [IntPtr]((1 -shl 16) -bor 0x400)) | Out-Null
  Ui-Say "tray: clicked"; Start-Sleep -Milliseconds 900
}
function Fake-Say([string]$words) {
  $pipe = New-Object System.IO.Pipes.NamedPipeClientStream(".", "remoteman-host-$global:UiPort", [System.IO.Pipes.PipeDirection]::InOut)
  $pipe.Connect(3000)
  $b = [System.Text.Encoding]::UTF8.GetBytes("$words`n"); $pipe.Write($b, 0, $b.Length); $pipe.Flush()
  $r = New-Object System.IO.StreamReader($pipe); $said = $r.ReadToEnd().Trim(); $pipe.Dispose()
  Ui-Say "fake host told: $words -> $said"; $said
}
