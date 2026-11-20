# A line a second of the vertical blanks in a full-screen measurement, with who was in front.
param([string]$Name)
$l = Get-Content "C:\rm\shots-viewer\vrr\full-$Name.log"
"===== $Name"
$l | Select-String "presenting: \d+|interface's layer|swap chain: 3840|variable refresh: on" | ForEach-Object { "  " + ($_.Line -replace '^\S+ ','') }
$v = $l | Select-String "vblank:" | ForEach-Object { if ($_.Line -match 'vblank: (\d+) in 1 s, ([\d.]+) ms apart.*in front: (.*)$') { "$($matches[1])/$($matches[2])" + $(if ($matches[3] -match '^remoteman-engine-full') { 'F' } elseif ($matches[3] -match 'this process') { 'w' } else { 'x' }) } }
"  vblanks/s: " + ($v -join ' ')
