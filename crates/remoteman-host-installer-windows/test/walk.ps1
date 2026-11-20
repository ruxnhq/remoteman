# Every page of the installer, walked through UI Automation with --dry-run (nothing on the PC is
# changed), with a screenshot of each, in light and dark:
#   powershell -ExecutionPolicy Bypass -File walk.ps1 -Exe ...\remoteman-host-setup-0.1.0.exe [-Out C:\rm\shots-setup]
# Installing (welcome, the steps, the close refused while they run, done, Open Remoteman Host),
# installing that fails (--fail-at setup, and --fail-at files), uninstalling (confirm, the steps,
# done) and uninstalling that fails, and each run's exit code. The checks are "ok" or "FAIL"
# lines in OUT\walk.log; each run's own log is OUT\THEME-NAME.log.
param([Parameter(Mandatory)][string]$Exe, [string]$Out = "C:\rm\shots-setup", [string[]]$Themes = @("light", "dark"))
$here = $PSScriptRoot
New-Item -ItemType Directory -Force $Out | Out-Null
Remove-Item "$Out\*" -Include *.png, *.log -ErrorAction SilentlyContinue
$global:UiLog = "$Out\walk.log"
. "$here\ui.ps1"

function Start-Setup([string]$theme, [string]$name, [string]$extra = "") {
  $log = "$Out\$theme-$name.log"
  $p = Start-Process -PassThru $Exe -ArgumentList "--dry-run --theme $theme --log `"$log`" $extra"
  $null = $p.Handle  # so that its exit code is kept
  $global:UiPid = Ui-Pid
  Ui-Say "${theme} ${name}: starter $($p.Id), pages $global:UiPid"
  $p
}
function Ended($p, [int]$code, [string]$what) {
  $done = $p.WaitForExit(30000)
  Check ($done -and $p.ExitCode -eq $code) "$what ends with $code (it ended with $($p.ExitCode))"
}

foreach ($theme in $Themes) {
  # Installing.
  $p = Start-Setup $theme "install"
  $null = Ui-Wait "Install"
  Ui-Place 60 30
  Ui-Shot "$Out\$theme-1-welcome.png"
  Ui-Invoke "Install"
  Start-Sleep -Milliseconds 1300
  Ui-Shot "$Out\$theme-2-progress.png"
  Ui-Close
  Check (Ui-Has "It can't be stopped half way: the window closes when it is done.") "$theme install: the close is refused while the steps run"
  Check ([bool](Ui-Window)) "$theme install: the window is still open"
  Ui-Shot "$Out\$theme-3-close-refused.png"
  $null = Ui-Wait "Open Remoteman Host" 60
  Ui-Shot "$Out\$theme-4-done.png"
  Check (Ui-Has "remoteman host is installed") "$theme install: done"
  # Open Remoteman Host opens its window; Close starts it in the background, as at sign-in. Either
  # is done by the starter, as the user, after the pages have closed.
  if ($theme -eq "dark") {
    Ui-Invoke "Open Remoteman Host"
    Ended $p 0 "$theme install"
    Check ([bool](Select-String -Path "$Out\$theme-install.log" -Pattern "would start .*remoteman-host-app\.exe $" -Quiet)) "$theme install: the starter would open Remoteman Host's window, as the user"
  } else {
    Ui-Invoke "Close"
    Ended $p 0 "$theme install"
    Check ([bool](Select-String -Path "$Out\$theme-install.log" -Pattern "would start .*remoteman-host-app\.exe --background" -Quiet)) "$theme install: the starter would start Remoteman Host in the background, as the user"
  }

  # Installing, failing at setup.ps1.
  $p = Start-Setup $theme "install-fails" "--fail-at setup"
  Ui-Invoke "Install"
  $null = Ui-Wait "Open the log" 60
  Ui-Place 60 30
  Ui-Shot "$Out\$theme-5-failed.png"
  Check (Ui-Has "remoteman host couldn't be installed") "$theme install fails: the failed page"
  Ui-Invoke "Close"
  Ended $p 1 "$theme install that fails"

  # Uninstalling.
  $p = Start-Setup $theme "uninstall" "--uninstall"
  $null = Ui-Wait "Remove"
  Ui-Place 60 30
  Ui-Shot "$Out\$theme-6-uninstall-confirm.png"
  Ui-Invoke "Remove"
  Start-Sleep -Milliseconds 1300
  Ui-Shot "$Out\$theme-7-uninstall-progress.png"
  $null = Ui-Wait "remoteman host is removed" 60
  Ui-Shot "$Out\$theme-8-uninstall-done.png"
  Ui-Invoke "Close"
  Ended $p 0 "$theme uninstall"

  # Uninstalling, failing at setup.ps1.
  $p = Start-Setup $theme "uninstall-fails" "--uninstall --fail-at setup"
  Ui-Invoke "Remove"
  $null = Ui-Wait "Open the log" 60
  Ui-Place 60 30
  Ui-Shot "$Out\$theme-9-uninstall-failed.png"
  Ui-Invoke "Close"
  Ended $p 1 "$theme uninstall that fails"

  # Cancelled on the first page.
  $p = Start-Setup $theme "cancel"
  Ui-Invoke "Cancel"
  Ended $p 2 "$theme install cancelled"
}

# A failure at another step: copying the files.
$p = Start-Setup "dark" "install-fails-files" "--fail-at files"
Ui-Invoke "Install"
$null = Ui-Wait "Open the log" 60
Ui-Place 60 30
Ui-Shot "$Out\dark-10-failed-files.png"
Ui-Invoke "Close"
Ended $p 1 "an install that fails at the files"
# With no --theme, the pages follow Windows' own setting (Settings > Personalization > Colors,
# apps' mode): its background is dark or light as AppsUseLightTheme says.
$light = (Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize').AppsUseLightTheme -eq 1
$log = "$Out\system-theme.log"
$p = Start-Process -PassThru $Exe -ArgumentList "--dry-run --log `"$log`""
$null = $p.Handle
$global:UiPid = Ui-Pid
$null = Ui-Wait "Install"
Ui-Place 60 30
Ui-Shot "$Out\system-theme-welcome.png"
$image = [System.Drawing.Bitmap]::FromFile("$Out\system-theme-welcome.png")
$pixel = $image.GetPixel(12, [int]($image.Height * 0.6)); $image.Dispose()
$bright = ($pixel.R + $pixel.G + $pixel.B) / 3
Check (($bright -gt 128) -eq $light) "with no --theme the pages are $(if ($bright -gt 128) { 'light' } else { 'dark' }) (brightness $([int]$bright)), as Windows is set ($(if ($light) { 'light' } else { 'dark' }))"
Ui-Invoke "Cancel"
Ended $p 2 "the run with Windows' theme"
Ui-Say "walk done"
