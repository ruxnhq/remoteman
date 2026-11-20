# A short burst of real input into the viewer's window (it is brought to the front first; the
# pointer is put back where it was): move, click, wheel, and keys the C# view had to keep from
# XAML (a letter, Tab, the arrows, Alt, F10, Space).
param([int]$ProcId, [int]$X = 0, [int]$Y = 0)
. "$PSScriptRoot\inp.ps1"
$p = Get-Process -Id $ProcId
$h = $p.MainWindowHandle
$was = New-Object Inp+POINT; [Inp]::GetCursorPos([ref]$was) | Out-Null
$front = [Inp]::GetForegroundWindow()
# A tap of Shift lets this process hand the foreground over.
[Inp]::Key(0x10, $false); [Inp]::Key(0x10, $true)
[Inp]::SetForegroundWindow($h) | Out-Null
Start-Sleep -Milliseconds 300
# Nothing is sent unless the viewer's window really is in front (else it would go to whatever is).
if ([Inp]::GetForegroundWindow() -ne $h) { "the viewer isn't in front: nothing sent"; return }
[Inp]::SetCursorPos($X, $Y) | Out-Null; Start-Sleep -Milliseconds 100
[Inp]::SetCursorPos($X + 40, $Y + 30) | Out-Null; Start-Sleep -Milliseconds 100
[Inp]::Mouse(0x0002, 0); Start-Sleep -Milliseconds 60; [Inp]::Mouse(0x0004, 0); Start-Sleep -Milliseconds 100   # left down, up
[Inp]::Mouse(0x0008, 0); Start-Sleep -Milliseconds 60; [Inp]::Mouse(0x0010, 0); Start-Sleep -Milliseconds 100   # right down, up
[Inp]::Mouse(0x0800, [uint32]4294967176); Start-Sleep -Milliseconds 100   # wheel one notch towards the user (-120)
foreach ($vk in 0x41, 0x09, 0x25, 0x27, 0x12, 0x79, 0x20) { [Inp]::Key([uint16]$vk, $false); Start-Sleep -Milliseconds 40; [Inp]::Key([uint16]$vk, $true); Start-Sleep -Milliseconds 60 }
Start-Sleep -Milliseconds 300
[Inp]::SetCursorPos($was.X, $was.Y) | Out-Null
"done; foreground was $front"
