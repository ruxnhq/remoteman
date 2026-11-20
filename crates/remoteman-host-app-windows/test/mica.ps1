# Does the window keep its Mica when another window has the keyboard? Screenshots of the window in
# front and then with a small "focus thief" window active beside it, and the backdrop's pixels at
# a few empty places compared (WinUI's own Mica would turn flat: 32,32,32 dark, 243,243,243 light):
#   pwsh -File mica.ps1 [-Theme dark|light] [-Out C:\rm\shots-host\mica]
param([string]$Theme = "dark", [string]$Out = "C:\rm\shots-host\mica", [string]$Exe = "", [int]$Port = 7899)
$here = $PSScriptRoot
if (-not $Exe) { $Exe = Join-Path $here "..\..\..\target\release\remoteman-host-app.exe" }
New-Item -ItemType Directory -Force $Out | Out-Null
$global:UiLog = "$Out\mica-$Theme.log"; $global:UiPort = $Port
Remove-Item $global:UiLog -ErrorAction SilentlyContinue
. "$here\ui.ps1"

$fake = Start-Process -PassThru -WindowStyle Hidden pwsh -ArgumentList "-NoProfile -File `"$here\fakehost.ps1`" -Port $Port -Log `"$Out\fakehost-$Theme.log`""
Start-Sleep 2
$app = Start-Process -PassThru $Exe -ArgumentList "--port $Port --theme $Theme --page about"
$global:UiPid = $app.Id
Start-Sleep 3
Ui-Place 120 120; Ui-Front
$front = "$Out\$Theme-front.png"; Ui-Shot $front
Ui-Say ("foreground: " + [HostUi]::Foreground())

# A window of another program takes the keyboard, beside this one (not over it).
$r = Ui-Rect
$thief = Join-Path $env:TEMP "remoteman-focus-thief.ps1"
Set-Content $thief @'
param([int]$X, [int]$Y)
Add-Type -AssemblyName System.Windows.Forms
$f = New-Object Windows.Forms.Form
$f.Text = "focus thief"; $f.StartPosition = "Manual"; $f.Location = New-Object Drawing.Point($X, $Y); $f.Size = New-Object Drawing.Size(420, 260)
$k = New-Object Windows.Forms.Timer; $k.Interval = 12000; $k.add_Tick({ $f.Close() })
$f.add_Shown({ $f.Activate(); $k.Start() })
# Started hidden (no console over the window being looked at): the first show takes the hidden
# start's SW_HIDE, the second is seen.
$f.Show(); $f.Hide()
[void]$f.ShowDialog()
'@
$t = Start-Process -PassThru -WindowStyle Hidden pwsh -ArgumentList "-NoProfile -File `"$thief`" -X $($r.Right + 40) -Y $($r.Top)"
Start-Sleep 4
Ui-Say ("foreground: " + [HostUi]::Foreground())
$behind = "$Out\$Theme-behind.png"; Ui-Shot $behind

# The backdrop at empty places: the navigation pane below its items, the title bar, the page.
Add-Type -AssemblyName System.Drawing
$a = [System.Drawing.Bitmap]::FromFile($front); $b = [System.Drawing.Bitmap]::FromFile($behind)
$w = $a.Width; $h = $a.Height
$same = $true
foreach ($p in @(@(0.10, 0.60), @(0.15, 0.85), @(0.45, 0.02), @(0.60, 0.80), @(0.90, 0.95))) {
  $x = [int]($w * $p[0]); $y = [int]($h * $p[1])
  $ca = $a.GetPixel($x, $y); $cb = $b.GetPixel($x, $y)
  $line = "at {0},{1}: in front {2},{3},{4}; behind {5},{6},{7}" -f $x, $y, $ca.R, $ca.G, $ca.B, $cb.R, $cb.G, $cb.B
  Ui-Say $line; $line
  if ([Math]::Abs($ca.R - $cb.R) + [Math]::Abs($ca.G - $cb.G) + [Math]::Abs($ca.B - $cb.B) -gt 6) { $same = $false }
}
$a.Dispose(); $b.Dispose()
Ui-Say ($(if ($same) { "ok: the backdrop is the same in front and behind" } else { "FAIL: the backdrop changed when the window lost the keyboard" }))
if (-not $t.HasExited) { Stop-Process -Id $t.Id -Force }
Stop-Process -Id $app.Id -Force; Stop-Process -Id $fake.Id -Force
Get-Content $global:UiLog | Select-String "foreground|ok:|FAIL:"
