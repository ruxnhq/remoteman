# The viewer's installer, all with --dry-run (nothing on the PC is changed):
#   powershell -ExecutionPolicy Bypass -File viewer.ps1 -Exe ...\remoteman-viewer-setup-0.1.0.exe
#              -Zip ...\remoteman-viewer-windows-0.1.0.zip [-Out C:\rm\shots-viewer-setup]
# 1. a silent install: exit 0, and the dry run's install folder holds exactly the zip's folder
#    (every file with its size and SHA-256), beside uninstall.exe and setup.log, and nothing else;
#    no UAC prompt was asked for (the log says the elevated part ran in the starter itself);
# 2. the host's options refused (--no-pair): exit 1 before anything is done;
# 3. the pages, with a picture of each: welcome, done (Open remoteman), the uninstaller's confirm
#    and done, and each run's exit code;
# 4. the uninstaller in that folder, silent: it copies itself out, ends with 0, the folder is gone.
# The checks are "ok" or "FAIL" lines in OUT\viewer.log.
param([Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string]$Zip, [string]$Out = "C:\rm\shots-viewer-setup")
$here = $PSScriptRoot
New-Item -ItemType Directory -Force $Out | Out-Null
Remove-Item "$Out\*" -Include *.png, *.log -ErrorAction SilentlyContinue
$global:UiLog = "$Out\viewer.log"
$global:SetupExeName = 'remoteman-viewer-setup.exe'
. "$here\ui.ps1"
$dry = Join-Path $env:TEMP 'remoteman-viewer-setup-dry-run\remoteman'

function Run([string]$exe, [string]$arguments, [int]$seconds = 180) {
  $p = Start-Process -PassThru $exe -ArgumentList $arguments
  $null = $p.Handle
  if (-not $p.WaitForExit($seconds * 1000)) { Ui-Say "still running after $seconds s: $exe $arguments"; return -1 }
  $p.ExitCode
}

# The zip's folder, to compare with.
$unzipped = Join-Path $Out 'unzipped'
if (Test-Path $unzipped) { Remove-Item -Recurse -Force $unzipped }
Add-Type -AssemblyName System.IO.Compression.FileSystem
[IO.Compression.ZipFile]::ExtractToDirectory($Zip, $unzipped)
$folder = @(Get-ChildItem $unzipped -Directory)
Check ($folder.Count -eq 1) "the zip holds one folder ($($folder.Name -join ', '))"
$folder = $folder[0].FullName

# 1. Silent install, every file checked against the zip.
$log = "$Out\silent-install.log"
$code = Run $Exe "--silent --dry-run --log `"$log`""
Check ($code -eq 0) "silent dry-run install ends with 0 ($code)"
Check ([bool](Select-String -Path $log -SimpleMatch 'the elevated part runs Here')) "no elevation was asked for: the starter ran the steps itself"
$want = Get-ChildItem -Recurse -File $folder | ForEach-Object { $_.FullName.Substring($folder.Length + 1) }
$bad = 0
foreach ($rel in $want) {
  $a = Join-Path $folder $rel; $b = Join-Path $dry $rel
  if (-not (Test-Path $b)) { Ui-Say "missing: $rel"; $bad++; continue }
  if ((Get-Item $a).Length -ne (Get-Item $b).Length -or (Get-FileHash $a).Hash -ne (Get-FileHash $b).Hash) { Ui-Say "differs: $rel"; $bad++ }
}
Check ($bad -eq 0) "all $($want.Count) files of the zip's folder were installed with their sizes and SHA-256 ($bad wrong)"
$extra = Get-ChildItem -Recurse -File $dry | ForEach-Object { $_.FullName.Substring($dry.Length + 1) } | Where-Object { $want -notcontains $_ }
Check ((@($extra) -join ',') -eq 'setup.log,uninstall.exe' -or (@($extra | Sort-Object) -join ',') -eq 'setup.log,uninstall.exe') "beside them only uninstall.exe and setup.log ($(@($extra) -join ', '))"
foreach ($would in 'HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\remoteman', 'remoteman.lnk') {
  Check ([bool](Select-String -Path $log -SimpleMatch $would)) "it would have written $would"
}
Check (-not (Select-String -Path $log -SimpleMatch 'HKLM')) "nothing in HKLM"

# 2. The host's options are refused at once.
$log = "$Out\refused.log"
$code = Run $Exe "--silent --dry-run --no-pair --log `"$log`""
Check ($code -eq 1) "--no-pair is refused with 1 ($code)"
Check (-not (Select-String -Path $log -SimpleMatch 'step:')) "and no step was taken"

# 3. The pages.
$p = Start-Process -PassThru $Exe -ArgumentList "--dry-run --log `"$Out\pages-install.log`""
$null = $p.Handle
$global:UiPid = Ui-Pid
$null = Ui-Wait "Install"
Ui-Place 60 30
Ui-Shot "$Out\1-welcome.png"
Ui-Invoke "Install"
$null = Ui-Wait "Open remoteman" 120
Ui-Shot "$Out\2-done.png"
Ui-Invoke "Close"
Check ($p.WaitForExit(30000) -and $p.ExitCode -eq 0) "the pages' install ends with 0 ($($p.ExitCode))"

$p = Start-Process -PassThru (Join-Path $dry 'uninstall.exe') -ArgumentList "--uninstall --dry-run --log `"$Out\pages-uninstall.log`""
$null = $p.Handle
$global:UiPid = Ui-Pid
$null = Ui-Wait "Remove"
Ui-Place 60 30
Ui-Shot "$Out\3-confirm.png"
Ui-Invoke "Remove"
$null = Ui-Wait "remoteman is removed" 60
Ui-Shot "$Out\4-removed.png"
Ui-Invoke "Close"
Start-Sleep -Seconds 3
Check (-not (Test-Path $dry)) "the pages removed the folder"

# 4. Installed again, then the uninstaller in its folder, silent.
$code = Run $Exe "--silent --dry-run --log `"$Out\reinstall.log`""
$log = "$Out\silent-uninstall.log"
$code = Run (Join-Path $dry 'uninstall.exe') "--uninstall --silent --dry-run --log `"$log`""
Check ($code -eq 0) "the silent uninstaller ends with 0 ($code)"
Start-Sleep -Seconds 5
Check (-not (Test-Path $dry)) "and the folder is gone"
Get-ChildItem $env:TEMP -Directory -Filter 'remoteman-viewer-setup-*' | Remove-Item -Recurse -Force -ErrorAction SilentlyContinue
Remove-Item -Recurse -Force $unzipped -ErrorAction SilentlyContinue
Get-Content $global:UiLog | Where-Object { $_ -match 'ok: |FAIL: ' }
