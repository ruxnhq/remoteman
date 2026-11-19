# Driving the WinUI 3 viewer in tests with nobody at the keyboard, through UI Automation (what
# screen readers use), in the guest's desktop session (tools/desk.py). Dot-source it, then:
#   Ui-Wait NAME [SECONDS]     wait until a control of that name is on screen
#   Ui-Invoke NAME             press it (a button, a menu item, a toggle)
#   Ui-Text NAME TEXT          type into the text field of that name
#   Ui-Shot FILE               the whole desktop as a PNG
Add-Type -AssemblyName UIAutomationClient, UIAutomationTypes, System.Windows.Forms, System.Drawing
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public class UiDpi { [DllImport("user32.dll")] public static extern bool SetProcessDPIAware(); }'
[UiDpi]::SetProcessDPIAware() | Out-Null
$global:UiRoot = [System.Windows.Automation.AutomationElement]::RootElement
function Ui-Window {
  $c = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ClassNameProperty, 'WinUIDesktopWin32WindowClass')
  $UiRoot.FindFirst([System.Windows.Automation.TreeScope]::Children, $c)
}
function Ui-Find([string]$name) {
  $c = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $name)
  # Menus and dialogs are popups: search the whole desktop, the window first.
  # Not the words inside a button: the button.
  $text = [System.Windows.Automation.ControlType]::Text
  $w = Ui-Window
  foreach ($scope in @($w, $UiRoot)) {
    if (-not $scope) { continue }
    foreach ($e in $scope.FindAll([System.Windows.Automation.TreeScope]::Descendants, $c)) { if ($e.Current.ControlType -ne $text) { return $e } }
  }
  $null
}
function Ui-Wait([string]$name, [int]$seconds = 10) {
  for ($i = 0; $i -lt $seconds * 5; $i++) { $e = Ui-Find $name; if ($e) { return $e }; Start-Sleep -Milliseconds 200 }
  Add-Content C:\rm\ui.log "not found: $name"; $null
}
function Ui-Invoke([string]$name) {
  $e = Ui-Wait $name 5
  if (-not $e) { return }
  foreach ($p in @([System.Windows.Automation.InvokePattern]::Pattern, [System.Windows.Automation.TogglePattern]::Pattern, [System.Windows.Automation.ExpandCollapsePattern]::Pattern, [System.Windows.Automation.SelectionItemPattern]::Pattern)) {
    $o = $null
    if ($e.TryGetCurrentPattern($p, [ref]$o)) {
      switch ($p.ProgrammaticName) {
        'InvokePatternIdentifiers.Pattern' { $o.Invoke() }
        'TogglePatternIdentifiers.Pattern' { $o.Toggle() }
        'ExpandCollapsePatternIdentifiers.Pattern' { $o.Expand() }
        'SelectionItemPatternIdentifiers.Pattern' { $o.Select() }
      }
      Add-Content C:\rm\ui.log "pressed: $name"; Start-Sleep -Milliseconds 600; return
    }
  }
  Add-Content C:\rm\ui.log "can't press: $name"
}
function Ui-Text([string]$name, [string]$text) {
  $e = Ui-Wait $name 5
  if (-not $e) { return }
  $o = $null
  if ($e.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$o)) { $o.SetValue($text); Add-Content C:\rm\ui.log "typed into $name" }
}
function Ui-Shot([string]$file) {
  $b = [System.Windows.Forms.SystemInformation]::VirtualScreen
  $image = New-Object System.Drawing.Bitmap $b.Width, $b.Height
  $g = [System.Drawing.Graphics]::FromImage($image)
  $g.CopyFromScreen($b.Left, $b.Top, 0, 0, $image.Size)
  $image.Save($file, [System.Drawing.Imaging.ImageFormat]::Png)
  $g.Dispose(); $image.Dispose()
  Add-Content C:\rm\ui.log "shot: $file"
}
