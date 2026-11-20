# The whole of Remoteman Host walked through UI Automation against test\fakehost.ps1, with a
# screenshot of each page, the icon's menu and the dialogs:
#   pwsh -File walk.ps1 [-Theme dark|light] [-Out C:\rm\shots-host\dark] [-Exe ...\remoteman-host-app.exe]
# Every word the app tells the host is checked in the fake host's log ("ok" or "FAIL" lines in
# OUT\walk.log). Port 7899 by default, so the host installed on the PC (7791) is never spoken to.
param([string]$Theme = "dark", [string]$Out = "", [string]$Exe = "", [int]$Port = 7899)
$here = $PSScriptRoot
if (-not $Exe) { $Exe = Join-Path $here "..\..\..\target\release\remoteman-host-app.exe" }
if (-not $Out) { $Out = "C:\rm\shots-host\$Theme" }
New-Item -ItemType Directory -Force $Out | Out-Null
Remove-Item "$Out\*" -Include *.png, *.log -ErrorAction SilentlyContinue
$global:UiLog = "$Out\walk.log"; $global:UiPort = $Port
$fakeLog = "$Out\fakehost.log"; $appLog = "$Out\app.log"
. "$here\ui.ps1"

function Check([bool]$ok, [string]$what) { Ui-Say ($(if ($ok) { "ok: " } else { "FAIL: " }) + $what) }
$global:Mark = 0
function Mark { $global:Mark = @(Get-Content $fakeLog -ErrorAction SilentlyContinue).Count }
# The app told the host exactly these words since the mark.
function Told([string]$words, [int]$seconds = 4) {
  for ($i = 0; $i -lt $seconds * 5; $i++) {
    $lines = @(Get-Content $fakeLog -ErrorAction SilentlyContinue) | Select-Object -Skip $global:Mark
    if ($lines | Where-Object { $_ -match "^\S+ $([regex]::Escape($words))$" }) { Check $true "told the host: $words"; Mark; return }
    Start-Sleep -Milliseconds 200
  }
  Check $false "told the host: $words"
}
function Notified([string]$title) {
  $ok = [bool](Select-String -Path $appLog -SimpleMatch "notified: $title" -Quiet)
  Check $ok "notification: $title"
}
function Start-App([string]$extra = "") {
  $p = Start-Process -PassThru $Exe -ArgumentList "--port $Port --theme $Theme --log `"$appLog`" $extra"
  $global:UiPid = $p.Id
  Start-Sleep 3
  $p
}
function Close-Window { $w = Ui-Window; $o = $null; if ($w -and $w.TryGetCurrentPattern([System.Windows.Automation.WindowPattern]::Pattern, [ref]$o)) { $o.Close(); Start-Sleep 1.5 } }
function Menu-Find {
  $c = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Menu)
  for ($i = 0; $i -lt 15; $i++) {
    foreach ($t in Ui-Tops) { $m = $t.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c); if ($m) { return $m } }
    Start-Sleep -Milliseconds 200
  }
  $null
}
function Menu-Shot([string]$file) {
  $m = Menu-Find
  if ($m) { $b = $m.Current.BoundingRectangle; Ui-ShotRect ([int]$b.Left - 12) ([int]$b.Top - 12) ([int]$b.Width + 24) ([int]$b.Height + 24) $file; return }
  Check $false "the menu is open for $file"
}
# Press an item of the icon's menu (not a button of the same name in the window).
function Menu-Invoke([string]$name) {
  $m = Menu-Find
  $c = New-Object System.Windows.Automation.AndCondition(
    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::MenuItem)),
    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, $name)))
  $e = if ($m) { $m.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c) }
  $o = $null
  if ($e -and $e.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$o)) { $o.Invoke(); Ui-Say "menu: $name"; Start-Sleep -Milliseconds 800 }
  else { Check $false "menu item: $name" }
}
function Menu-Items {
  $c = New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::MenuItem)
  foreach ($t in Ui-Tops) { foreach ($i in $t.FindAll([System.Windows.Automation.TreeScope]::Descendants, $c)) { $i.Current.Name } }
}
function Escape { [HostUi]::keybd_event(0x1B, 0, 0, [UIntPtr]::Zero); [HostUi]::keybd_event(0x1B, 0, 2, [UIntPtr]::Zero); Start-Sleep -Milliseconds 500 }

# The fake host, fresh.
$fake = Start-Process -PassThru -WindowStyle Hidden pwsh -ArgumentList "-NoProfile -File `"$here\fakehost.ps1`" -Port $Port -Log `"$fakeLog`""
Start-Sleep 2; Mark
$app = Start-App
Ui-Place 120 120; Ui-Front
Check ([bool](Ui-Wait "Share this PC")) "the window opens at Sharing"
Ui-Shot "$Out\01-sharing.png"
Ui-Invoke "Devices"; Ui-Front; Ui-Shot "$Out\02-devices.png"
Ui-Invoke "About"; Ui-Shot "$Out\03-about.png"

# Sharing off and on from the switch.
Ui-Invoke "Sharing"
Ui-Toggle "Share this PC"; Told "sharing off"
Start-Sleep 1; Ui-Front; Ui-Shot "$Out\04-sharing-off.png"
Ui-Toggle "Share this PC"; Told "sharing on"

# Clipboard and files from their switches.
Ui-Invoke "Devices"
Ui-Toggle "Share the clipboard with devices"; Told "allow clipboard on"
Ui-Toggle "Receive files from devices"; Told "allow files on"
Start-Sleep 1; Check ((Ui-IsOn "Receive files from devices") -eq $true) "the files switch follows the host"

# The icon's menu, and pairing from it.
Tray-Menu
Menu-Shot "$Out\05-tray-ready.png"
Menu-Invoke "Pair a device…"; Told "pair on"
Check ([bool](Ui-Wait "Pairing is open")) "Devices shows pairing open"
Ui-Front; Ui-Shot "$Out\06-pairing-open.png"

# A new device asks: the question, then Codes match.
Fake-Say "fake pending Studio_Mac 482913 c41e77a09b2d5f36" | Out-Null
Check ([bool](Ui-Wait "Codes match" 6)) "the pairing question is put"
Notified "Studio Mac wants to pair"
Ui-Front; Ui-Shot "$Out\07-question.png"
Ui-Invoke "Codes match"; Told "pair accept"
Start-Sleep 1.2; Ui-Shot "$Out\08-paired.png"

# Asked and refused.
Fake-Say "pair on" | Out-Null; Mark
Fake-Say "fake pending Pixel_9 135790 99887766aabbccdd" | Out-Null
Check ([bool](Ui-Wait "Refuse" 6)) "a second device is asked about"
Ui-Invoke "Refuse"; Told "pair decline"

# Asked, then answered elsewhere: the question goes, and nothing is said.
Fake-Say "pair on" | Out-Null; Fake-Say "fake pending Laptop 246802 1122334455667788" | Out-Null; Mark
[void](Ui-Wait "Codes match" 6)
Fake-Say "pair off" | Out-Null; Mark
Check (Ui-Gone "Codes match" 5) "the question goes when it is answered elsewhere"
Start-Sleep 1.5
Check (-not (@(Get-Content $fakeLog) | Select-Object -Skip $global:Mark | Where-Object { $_ -match "pair (accept|decline)$" })) "nothing is said for a question taken away"

# A device connects: notification, the Sharing page with it, the menu.
Fake-Say "fake client iPad_Pro 1234abcd5678ef90" | Out-Null; Mark
Start-Sleep 2; Notified "iPad Pro connected"
Ui-Invoke "Sharing"; Ui-Front; Ui-Shot "$Out\09-connected.png"
Tray-Menu; Menu-Shot "$Out\10-tray-connected.png"
$items = Menu-Items
Check (($items -contains "Send a file to the device…") -and ($items -contains "Disconnect")) "the menu offers Send a file and Disconnect"
Escape

# Send a file: Windows' file dialog, a file picked, the device takes it.
$file = "$Out\a file to send.txt"; Set-Content $file "hello from the walk"
Ui-Invoke "Send a file…"
# (UI Automation shows the dialog under the window it belongs to.)
$dlg = Ui-Wait "Send a file to the device" 6
Check ([bool]$dlg) "the file dialog opens"
if ($dlg) {
  [HostUi]::SetForegroundWindow([IntPtr]$dlg.Current.NativeWindowHandle) | Out-Null; Start-Sleep -Milliseconds 400
  $b = $dlg.Current.BoundingRectangle; Ui-ShotRect ([int]$b.Left) ([int]$b.Top) ([int]$b.Width) ([int]$b.Height) "$Out\11-file-dialog.png"
  $c = New-Object System.Windows.Automation.AndCondition(
    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Edit)),
    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, "File name:")))
  $edit = $dlg.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c)
  $o = $null; if ($edit -and $edit.TryGetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern, [ref]$o)) { $o.SetValue($file) }
  $c = New-Object System.Windows.Automation.AndCondition(
    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::ControlTypeProperty, [System.Windows.Automation.ControlType]::Button)),
    (New-Object System.Windows.Automation.PropertyCondition([System.Windows.Automation.AutomationElement]::NameProperty, "Open")))
  $open = $dlg.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $c)
  $o = $null; if ($open -and $open.TryGetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern, [ref]$o)) { $o.Invoke() }
  Told "send $file" 6
  Start-Sleep 2; Notified "The device has the file"
  Ui-Front; Ui-Shot "$Out\12-sent.png"
}

# Disconnect, then Revoke… with its dialog.
Ui-Invoke "Devices"
Ui-Invoke "Disconnect"; Told "drop"
Ui-Invoke "Revoke iPad Pro"
Check ([bool](Ui-Wait "Revoke iPad Pro?" 5)) "the Revoke dialog opens"
Ui-Front; Ui-Shot "$Out\13-revoke.png"
Ui-Invoke "Revoke"; Told "revoke 1234abcd5678ef90"

# Closing the window keeps the icon; the icon opens it again.
Close-Window
Check ((-not (Ui-Window)) -and -not $app.HasExited) "closing the window keeps the app (and its icon)"
Tray-Click
Check ([bool](Ui-Wait "Paired devices" 5)) "clicking the icon opens the window again, at the page it was on"
Close-Window

# A device asks while the window is closed: the window opens with the question.
Fake-Say "pair on" | Out-Null; Fake-Say "fake pending Studio_Mac_2 975310 abcdefabcdef0001" | Out-Null; Mark
Check ([bool](Ui-Wait "Codes match" 8)) "the question opens the closed window"
# Closing the window with the question open doesn't answer it.
Close-Window; Start-Sleep 1.5
Check (-not $app.HasExited) "the app lives on after closing with the question open"
Check (-not (@(Get-Content $fakeLog) | Select-Object -Skip $global:Mark | Where-Object { $_ -match "pair (accept|decline)$" })) "closing the window doesn't answer the question"
Tray-Click
Check ([bool](Ui-Wait "Codes match" 5)) "the question is asked again in the next window"
Ui-Invoke "Codes match"; Told "pair accept"

# A second start opens the first one's window, at its page, and ends.
Close-Window
$second = Start-Process -PassThru $Exe -ArgumentList "--port $Port --page about --log `"$appLog`""
Start-Sleep 3
Check ($second.HasExited -and [bool](Ui-Wait "Microphone of a device" 5)) "a second start opens the first's window (About) and ends"

# Quit from the menu: the app ends, the host is left as it is.
Tray-Menu; Menu-Invoke "Quit remoteman"; Start-Sleep 2
Check $app.HasExited "Quit ends the app"

# At sign-in: the icon only, until a device asks.
$app = Start-App "--background"
Check ((-not (Ui-Window)) -and -not $app.HasExited) "--background starts with the icon only"
Fake-Say "pair on" | Out-Null; Fake-Say "fake pending Background_Mac 112358 0f0f0f0f0f0f0f0f" | Out-Null; Mark
Check ([bool](Ui-Wait "Codes match" 8)) "a device asking opens the window from the background"
Ui-Place 120 120; Ui-Front; Ui-Shot "$Out\14-background-question.png"
Ui-Invoke "Codes match"; Told "pair accept"

# Paused: the menu.
Ui-Invoke "Sharing"; Ui-Toggle "Share this PC"; Told "sharing off"
Tray-Menu; Menu-Shot "$Out\15-tray-paused.png"; Escape
Ui-Toggle "Share this PC"; Told "sharing on"

# The host goes away: the window and the menu say so.
Fake-Say "fake stop" | Out-Null
Start-Sleep 3; Ui-Front; Ui-Shot "$Out\16-no-host.png"
Tray-Menu; Menu-Shot "$Out\17-tray-no-host.png"; Escape
Tray-Menu; Menu-Invoke "Quit remoteman"; Start-Sleep 1.5
if (-not $app.HasExited) { Stop-Process -Id $app.Id -Force }
if (-not $fake.HasExited) { Stop-Process -Id $fake.Id -Force }
$fails = @(Select-String -Path $global:UiLog -SimpleMatch "FAIL:").Count
$oks = @(Select-String -Path $global:UiLog -SimpleMatch "ok:").Count
"walk ($Theme): $oks ok, $fails failed; log $global:UiLog"
