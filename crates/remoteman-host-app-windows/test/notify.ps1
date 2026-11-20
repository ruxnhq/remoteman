# A Windows notification from the app, seen: a device connects to the fake host, and the corner of
# the primary screen where Windows shows notifications is captured.
#   pwsh -File notify.ps1 [-Out C:\rm\shots-host\notify]
param([string]$Out = "C:\rm\shots-host\notify", [string]$Exe = "", [int]$Port = 7899)
$here = $PSScriptRoot
if (-not $Exe) { $Exe = Join-Path $here "..\..\..\target\release\remoteman-host-app.exe" }
New-Item -ItemType Directory -Force $Out | Out-Null
$global:UiLog = "$Out\notify.log"; $global:UiPort = $Port
. "$here\ui.ps1"
$fake = Start-Process -PassThru -WindowStyle Hidden pwsh -ArgumentList "-NoProfile -File `"$here\fakehost.ps1`" -Port $Port -Log `"$Out\fakehost.log`""
Start-Sleep 2
$app = Start-Process -PassThru $Exe -ArgumentList "--port $Port --background --log `"$Out\app.log`""
$global:UiPid = $app.Id
Start-Sleep 3
Fake-Say "fake client iPad_Pro 1234abcd5678ef90" | Out-Null
Start-Sleep 2.5
$s = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
Ui-ShotRect ($s.Right - 900) ($s.Bottom - 600) 900 600 "$Out\connected.png"
Start-Sleep 6
Fake-Say "pair on" | Out-Null; Fake-Say "fake pending Studio_Mac 482913 c41e77a09b2d5f36" | Out-Null
Start-Sleep 2.5
Ui-ShotRect ($s.Right - 900) ($s.Bottom - 600) 900 600 "$Out\wants-to-pair.png"
Stop-Process -Id $app.Id -Force; Stop-Process -Id $fake.Id -Force
Get-Content "$Out\app.log" | Select-String "notified"
