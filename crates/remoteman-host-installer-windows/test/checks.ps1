# What walk.ps1 doesn't see, all with --dry-run (nothing on the PC is changed):
#   powershell -ExecutionPolicy Bypass -File checks.ps1 -Exe ...\remoteman-host-setup-0.1.0.exe -Payload C:\rm\setup-stage\payload [-Out C:\rm\shots-setup]
# 1. a silent install: exit 0, and every file of the payload arrived in the dry run's install
#    folder with its size and SHA-256, beside uninstall.exe and setup.log, and nothing else;
# 2. a silent install that fails (--fail-at setup): exit 1, the reason in the log;
# 3. the uninstaller in that folder, silent: it copies itself out of the folder, waits, ends with
#    0, and the folder is gone (its own file, held by the waiting copy, moved out);
# 4. the uninstaller again, with its pages: the starter ends at once, the pages remove the folder,
#    and the copy it ran from is deleted after it;
# 5. the pages' Windows App Runtime: every Microsoft.UI* / WindowsAppRuntime / WinUI DLL the pages
#    have loaded is from their own folder, none from the runtime installed on the PC (WindowsApps,
#    SystemApps), and without Microsoft.ui.xaml.dll beside it the window doesn't open at all.
# The checks are "ok" or "FAIL" lines in OUT\checks.log.
param([Parameter(Mandatory)][string]$Exe, [Parameter(Mandatory)][string]$Payload, [string]$Out = "C:\rm\shots-setup")
$here = $PSScriptRoot
New-Item -ItemType Directory -Force $Out | Out-Null
$global:UiLog = "$Out\checks.log"
Remove-Item $global:UiLog -ErrorAction SilentlyContinue
. "$here\ui.ps1"
$dry = Join-Path $env:TEMP 'remoteman-host-setup-dry-run\remoteman host'

function Run([string]$exe, [string]$arguments, [int]$seconds = 120) {
  $p = Start-Process -PassThru $exe -ArgumentList $arguments
  $null = $p.Handle
  if (-not $p.WaitForExit($seconds * 1000)) { Ui-Say "still running after $seconds s: $exe $arguments"; return -1 }
  $p.ExitCode
}

# 1. Silent install, every file checked against the payload.
$log = "$Out\checks-silent-install.log"; Remove-Item $log -ErrorAction SilentlyContinue
$code = Run $Exe "--silent --dry-run --log `"$log`""
Check ($code -eq 0) "silent dry-run install ends with 0 ($code)"
$want = Get-ChildItem -Recurse -File $Payload | ForEach-Object { $_.FullName.Substring($Payload.TrimEnd('\').Length + 1) }
$bad = 0
foreach ($rel in $want) {
  $a = Join-Path $Payload $rel; $b = Join-Path $dry $rel
  if (-not (Test-Path $b)) { Ui-Say "missing: $rel"; $bad++; continue }
  if ((Get-Item $a).Length -ne (Get-Item $b).Length -or (Get-FileHash $a).Hash -ne (Get-FileHash $b).Hash) { Ui-Say "differs: $rel"; $bad++ }
}
Check ($bad -eq 0) "all $($want.Count) payload files arrived with their sizes and SHA-256 ($bad wrong)"
$extra = Get-ChildItem -Recurse -File $dry | ForEach-Object { $_.FullName.Substring($dry.Length + 1) } | Where-Object { $want -notcontains $_ }
Check ((@($extra) -join ',') -eq 'setup.log,uninstall.exe') "beside them only setup.log and uninstall.exe ($(@($extra) -join ', '))"
Check ([bool](Select-String -Path $log -SimpleMatch "each checked" -Quiet)) "the installer checked each file it wrote"
$would = @(Select-String -Path $log -SimpleMatch "  would ").Count
Check ($would -ge 10) "what would be done is logged ($would lines)"
Ui-Say "the would lines:"; Select-String -Path $log -SimpleMatch "  would " | ForEach-Object { Ui-Say ("   " + $_.Line.Substring($_.Line.IndexOf('would'))) }

# 2. Silent install that fails.
$log = "$Out\checks-silent-fails.log"; Remove-Item $log -ErrorAction SilentlyContinue
$code = Run $Exe "--silent --dry-run --fail-at setup --log `"$log`""
Check ($code -eq 1) "silent dry-run install failing at setup ends with 1 ($code)"
Check ([bool](Select-String -Path $log -SimpleMatch "FAILED: a failure made with --fail-at setup" -Quiet)) "the reason is in the log"

# 3. The uninstaller from the install folder, silent.
$code = Run $Exe "--silent --dry-run --log `"$Out\checks-reinstall.log`""
$log = "$Out\checks-silent-uninstall.log"; Remove-Item $log -ErrorAction SilentlyContinue
$code = Run (Join-Path $dry 'uninstall.exe') "--uninstall --silent --dry-run --log `"$log`""
Check ($code -eq 0) "the silent uninstaller ends with 0 ($code)"
Check ([bool](Select-String -Path $log -SimpleMatch "(from the install folder)" -Quiet)) "it knew it ran from the install folder"
$second = Select-String -Path $log -Pattern "elevated part: (.+?) \(elevated" | Select-Object -First 1
Check ($second -and -not $second.Matches[0].Groups[1].Value.StartsWith($dry, [StringComparison]::OrdinalIgnoreCase)) "the rest ran from a copy outside it ($(if ($second) { $second.Matches[0].Groups[1].Value }))"
Check (-not (Test-Path $dry)) "the install folder is gone"
Select-String -Path $log -Pattern "deleted when Windows restarts|Deleted" | ForEach-Object { Ui-Say ("   " + $_.Line) }

# 4. The uninstaller with its pages.
$code = Run $Exe "--silent --dry-run --log `"$Out\checks-reinstall.log`""
$log = "$Out\checks-uninstall-pages.log"; Remove-Item $log -ErrorAction SilentlyContinue
$p = Start-Process -PassThru (Join-Path $dry 'uninstall.exe') -ArgumentList "--uninstall --dry-run --theme dark --log `"$log`""
$null = $p.Handle
$global:UiPid = Ui-Pid
Check ($p.WaitForExit(10000) -and $p.ExitCode -eq 0) "the uninstaller's starter ended at once, with 0, while the pages are open"
Ui-Invoke "Remove"
$null = Ui-Wait "remoteman host is removed" 60
Ui-Invoke "Close"
Start-Sleep 8
Check (-not (Test-Path $dry)) "the install folder is gone"
$copy = Select-String -Path $log -Pattern "elevated part: (.+?) \(elevated" | Select-Object -First 1
if ($copy) {
  $copyDir = Split-Path $copy.Matches[0].Groups[1].Value
  Check (-not (Test-Path $copyDir)) "the copy it ran from ($copyDir) was deleted after it ended"
}

# 5. The pages' runtime.
$log = "$Out\checks-runtime.log"; Remove-Item $log -ErrorAction SilentlyContinue
$p = Start-Process -PassThru $Exe -ArgumentList "--dry-run --log `"$log`""
$global:UiPid = Ui-Pid
$null = Ui-Wait "Install"
$own = Split-Path (Get-Process -Id $global:UiPid).Path
$modules = Ui-Modules
# The runtime's DLLs are the ones in the pages' folder; whichever of those names is loaded must
# be loaded from there.
$runtimeNames = Get-ChildItem $own -Filter *.dll | ForEach-Object { $_.Name.ToLowerInvariant() }
$winui = $modules | Where-Object { $runtimeNames -contains $_.ModuleName.ToLowerInvariant() }
Ui-Say "the pages ($global:UiPid) run from $own and have loaded, of the runtime's $($runtimeNames.Count) DLLs:"
$winui | ForEach-Object { Ui-Say ("   " + $_.FileName) }
Ui-Say "and these Microsoft.* DLLs of Windows itself:"
$modules | Where-Object { $_.ModuleName -like 'Microsoft.*' -and $runtimeNames -notcontains $_.ModuleName.ToLowerInvariant() } | ForEach-Object { Ui-Say ("   " + $_.FileName) }
Check ($winui.Count -ge 10) "the pages loaded $($winui.Count) of the runtime's DLLs"
Check (-not ($winui | Where-Object { -not $_.FileName.StartsWith($own, [StringComparison]::OrdinalIgnoreCase) })) "each from the pages' own folder"
Check (-not ($modules | Where-Object { $_.FileName -match '\\(WindowsApps|SystemApps)\\' })) "nothing from WindowsApps or SystemApps (the runtime installed on the PC)"
Get-AppxPackage Microsoft.WindowsAppRuntime* | ForEach-Object { Ui-Say ("   installed on the PC, not used: {0} {1}" -f $_.Name, $_.Version) }
# Without Microsoft.ui.xaml.dll beside it, the window doesn't open (nothing falls back to the
# installed runtime). A copy of the pages' folder, with and without it.
$with = "C:\rm\spike\pages-with"; $without = "C:\rm\spike\pages-without"
foreach ($d in $with, $without) { Remove-Item -Recurse -Force $d -ErrorAction SilentlyContinue; Copy-Item -Recurse $own $d }
Remove-Item "$without\Microsoft.ui.xaml.dll"
Ui-Invoke "Cancel"
$null = $p.WaitForExit(15000)
foreach ($case in @(@{ d = $with; name = 'with' }, @{ d = $without; name = 'without' })) {
  $l = "$Out\checks-runtime-$($case.name).log"; Remove-Item $l -ErrorAction SilentlyContinue
  $q = Start-Process -PassThru "$($case.d)\remoteman-host-setup.exe" -ArgumentList "--ui --dry-run --source `"$Exe`" --log `"$l`""
  $null = $q.Handle
  $global:UiPid = $q.Id
  $opened = [bool](Ui-Wait "Install" 12)
  Ui-Say "pages $($case.name) Microsoft.ui.xaml.dll: window opened: $opened"
  if ($opened) { Ui-Invoke "Cancel"; $null = $q.WaitForExit(10000) } else { if (-not $q.HasExited) { Stop-Process -Id $q.Id -Force }; $null = $q.WaitForExit(5000) }
  Ui-Say ("   exit code {0}; its log: {1}" -f $q.ExitCode, ((Get-Content $l -ErrorAction SilentlyContinue) -join ' | '))
  if ($case.name -eq 'with') { Check $opened "a copy of the pages' folder opens the window" } else { Check (-not $opened) "without Microsoft.ui.xaml.dll the window doesn't open" }
}
Ui-Say "checks done"
