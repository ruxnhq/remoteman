# Driving the installer's pages in tests with nobody at the keyboard, through UI Automation
# (what screen readers use), always with --dry-run. Dot-source it, then:
#   $global:SetupPid = (Start-Setup '--dry-run --theme dark').Id   the starter; the pages are its grandchild
#   Ui-Wait NAME [SECONDS]      wait until a control of that name is on screen (the pages' own)
#   Ui-Invoke NAME              press it
#   Ui-Shot FILE                the window as a PNG (what is on screen there)
#   Ui-Close                    ask the window to close, as its X button does
#   Ui-Modules                  the DLLs the pages' process has loaded, with their paths
# Everything is scoped to the pages' process (the one started with --ui).
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Drawing
Add-Type -TypeDefinition @'
using System; using System.Runtime.InteropServices;
public class SetupUi {
  [DllImport("user32.dll")] public static extern bool SetProcessDPIAware();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int c);
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
  [DllImport("dwmapi.dll")] public static extern int DwmGetWindowAttribute(IntPtr h, int a, out RECT r, int size);
}
'@
[SetupUi]::SetProcessDPIAware() | Out-Null
$global:UiRoot = [System.Windows.Automation.AutomationElement]::RootElement
if (-not $global:UiLog) { $global:UiLog = "C:\rm\shots-setup\walk.log" }
function Ui-Say([string]$line) { Add-Content $global:UiLog ("{0} {1}" -f (Get-Date -Format "HH:mm:ss.fff"), $line) }
function Check([bool]$ok, [string]$what) { Ui-Say ($(if ($ok) { "ok: " } else { "FAIL: " }) + $what) }

# The pages' process: the setup exe started with --ui, a descendant of the starter (the viewer's
# installer's when $global:SetupExeName says remoteman-viewer-setup.exe).
function Ui-Pid([int]$seconds = 20) {
  $exeName = if ($global:SetupExeName) { $global:SetupExeName } else { 'remoteman-host-setup.exe' }
  for ($i = 0; $i -lt $seconds * 5; $i++) {
    $p = Get-CimInstance Win32_Process -Filter "Name='$exeName'" | Where-Object { $_.CommandLine -match ' --ui ' } | Select-Object -First 1
    if ($p) { return [int]$p.ProcessId }
    Start-Sleep -Milliseconds 200
  }
  Ui-Say "no pages process"; 0
}
function Ui-Window {
  $c = New-Object System.Windows.Automation.AndCondition(
    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ProcessIdProperty, $global:UiPid)),
    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ClassNameProperty, 'WinUIDesktopWin32WindowClass')))
  $UiRoot.FindFirst([System.Windows.Automation.TreeScope]::Children, $c)
}
function Ui-Find([string]$name) {
  $w = Ui-Window
  if (-not $w) { return $null }
  $c = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $name)
  $text = [System.Windows.Automation.ControlType]::Text
  foreach ($e in $w.FindAll([System.Windows.Automation.TreeScope]::Descendants, $c)) { if ($e.Current.ControlType -ne $text) { return $e } }
  $w.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c)
}
function Ui-Wait([string]$name, [int]$seconds = 15) {
  for ($i = 0; $i -lt $seconds * 5; $i++) { $e = Ui-Find $name; if ($e) { return $e }; Start-Sleep -Milliseconds 200 }
  Ui-Say "not found: $name"; $null
}
function Ui-Has([string]$name) { [bool](Ui-Find $name) }
function Ui-Invoke([string]$name) {
  $e = Ui-Wait $name 10
  $o = $null
  if ($e -and $e.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$o)) { $o.Invoke(); Ui-Say "pressed: $name"; Start-Sleep -Milliseconds 300; return }
  Ui-Say "can't press: $name"
}
function Ui-Enabled([string]$name) { $e = Ui-Find $name; $e -and $e.Current.IsEnabled }
function Ui-Hwnd { $w = Ui-Window; if ($w) { [IntPtr]$w.Current.NativeWindowHandle } else { [IntPtr]::Zero } }
function Ui-Front {
  $h = Ui-Hwnd
  [SetupUi]::keybd_event(0x12, 0, 0, [UIntPtr]::Zero); [SetupUi]::keybd_event(0x12, 0, 2, [UIntPtr]::Zero)
  [SetupUi]::ShowWindow($h, 9) | Out-Null
  [SetupUi]::SetForegroundWindow($h) | Out-Null
  Start-Sleep -Milliseconds 400
}
function Ui-Place([int]$x, [int]$y) { [SetupUi]::SetWindowPos((Ui-Hwnd), [IntPtr]::Zero, $x, $y, 0, 0, 0x0001 -bor 0x0004) | Out-Null; Start-Sleep -Milliseconds 300 }
function Ui-Rect { $r = New-Object SetupUi+RECT; [SetupUi]::DwmGetWindowAttribute((Ui-Hwnd), 9, [ref]$r, 16) | Out-Null; $r }
function Ui-Shot([string]$file) {
  Ui-Front
  $r = Ui-Rect
  $w = $r.Right - $r.Left; $h = $r.Bottom - $r.Top
  $image = New-Object System.Drawing.Bitmap $w, $h
  $g = [System.Drawing.Graphics]::FromImage($image)
  $g.CopyFromScreen($r.Left, $r.Top, 0, 0, $image.Size)
  $image.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $image.Dispose()
  Ui-Say "shot: $file"
}
function Ui-Close {
  $w = Ui-Window; $o = $null
  if ($w -and $w.TryGetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern, [ref]$o)) { $o.Close(); Ui-Say "asked the window to close"; Start-Sleep -Milliseconds 800 }
}
function Ui-Modules {
  (Get-Process -Id $global:UiPid).Modules | Select-Object ModuleName, FileName
}
