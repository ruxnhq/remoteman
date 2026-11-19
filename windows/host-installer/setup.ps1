# What the host's installer does once its files are in place, and what its uninstaller undoes.
# Run by the installer (remoteman-host.iss), elevated, hidden; never by hand except to repair:
#   powershell -ExecutionPolicy Bypass -File setup.ps1 install|uninstall
# install:   the display driver (Guesthouse's user-mode display driver, signed here with a
#            certificate made on this machine, trusted only on it, its private key deleted at
#            once), the inbound firewall rule for the host's port, a task that starts the host
#            at each logon (elevated, in the user's own desktop session, where it must run), and
#            the host started now.
# uninstall: all of that taken away again, and the host's state (its identity and the paired
#            devices) deleted.
# What install made is written to installed.json beside this script, so that uninstall takes
# away exactly that (a display device that was here before is left, with the driver it had).
# Exit code 0, or 1 with the reason as the last line of setup.log.
param([Parameter(Mandatory)][ValidateSet('install', 'uninstall')][string]$Verb)
$ErrorActionPreference = 'Stop'
$app = $PSScriptRoot
$exe = Join-Path $app 'remoteman-host.exe'
$state = Join-Path $env:ProgramData 'remoteman\host'
$record = Join-Path $app 'installed.json'
$log = Join-Path $app 'setup.log'
$port = 7791
$name = 'remoteman host'                   # the task's and the firewall rule's name
$hardwareId = 'Root\GuesthouseDisplay'
$subject = 'CN=remoteman display driver, this machine only'
$ours = '*remoteman display driver, this machine only*'
$settingsFolder = 'C:\VirtualDisplayDriver'  # where the driver reads its settings

function Say([string]$line) { Add-Content -Path $log -Value "$(Get-Date -Format s) $line" }

$source = @'
using System; using System.Runtime.InteropServices;
public class RmDev {
  [StructLayout(LayoutKind.Sequential)] public struct SP { public int cbSize; public Guid ClassGuid; public int DevInst; public IntPtr Reserved; }
  [DllImport("setupapi.dll", SetLastError=true)] public static extern IntPtr SetupDiCreateDeviceInfoList(ref Guid g, IntPtr h);
  [DllImport("setupapi.dll", SetLastError=true, CharSet=CharSet.Unicode)] public static extern bool SetupDiCreateDeviceInfoW(IntPtr s, string name, ref Guid g, string desc, IntPtr h, int flags, ref SP d);
  [DllImport("setupapi.dll", SetLastError=true, CharSet=CharSet.Unicode)] public static extern bool SetupDiSetDeviceRegistryPropertyW(IntPtr s, ref SP d, int prop, byte[] b, int n);
  [DllImport("setupapi.dll", SetLastError=true)] public static extern bool SetupDiCallClassInstaller(int f, IntPtr s, ref SP d);
  [DllImport("newdev.dll", SetLastError=true, CharSet=CharSet.Unicode)] public static extern bool UpdateDriverForPlugAndPlayDevicesW(IntPtr h, string id, string inf, int flags, out bool reboot);
  public static string Make(string id, string inf) {
    Guid g = new Guid("4d36e968-e325-11ce-bfc1-08002be10318");
    IntPtr s = SetupDiCreateDeviceInfoList(ref g, IntPtr.Zero);
    SP d = new SP(); d.cbSize = Marshal.SizeOf(d);
    if (!SetupDiCreateDeviceInfoW(s, "Display", ref g, null, IntPtr.Zero, 1, ref d)) return "create failed " + Marshal.GetLastWin32Error();
    byte[] b = System.Text.Encoding.Unicode.GetBytes(id + "\0\0");
    if (!SetupDiSetDeviceRegistryPropertyW(s, ref d, 1, b, b.Length)) return "hardware id failed " + Marshal.GetLastWin32Error();
    if (!SetupDiCallClassInstaller(0x19, s, ref d)) return "register failed " + Marshal.GetLastWin32Error();
    return Update(id, inf);
  }
  public static string Update(string id, string inf) {
    bool r; bool ok = UpdateDriverForPlugAndPlayDevicesW(IntPtr.Zero, id, inf, 1, out r);
    return (ok ? "ok" : "failed " + Marshal.GetLastWin32Error()) + (r ? " (a restart is needed)" : "");
  }
}
'@

function Our-Devices {
  Get-PnpDevice -Class Display -ErrorAction SilentlyContinue | Where-Object {
    (Get-PnpDeviceProperty -InstanceId $_.InstanceId -KeyName DEVPKEY_Device_HardwareIds -ErrorAction SilentlyContinue).Data -contains $hardwareId
  }
}

function Remove-Certificates {
  foreach ($store in 'Root', 'TrustedPublisher') {
    Get-ChildItem "Cert:\LocalMachine\$store" | Where-Object Subject -like $ours | Remove-Item
  }
  Get-ChildItem Cert:\LocalMachine\My | Where-Object Subject -like $ours |
    ForEach-Object { Remove-Item "Cert:\LocalMachine\My\$($_.Thumbprint)" -DeleteKey }
}

function Driver-Packages { (& pnputil.exe /enum-drivers) -join "`n" | Select-String -Pattern 'oem\d+\.inf' -AllMatches | ForEach-Object { $_.Matches.Value } }

# What install made, kept across installs over an earlier one: written after each step, so that a
# failure part way still leaves uninstall knowing what to take away.
function Load-Record {
  $r = [ordered]@{ driverInfs = @(); deviceMade = $false; settingsMade = $false }
  if (Test-Path $record) {
    $old = Get-Content $record -Raw | ConvertFrom-Json
    $r.driverInfs = @($old.driverInfs | Where-Object { $_ })
    $r.deviceMade = [bool]$old.deviceMade
    $r.settingsMade = [bool]$old.settingsMade
  }
  $r
}
function Save-Record($r) { $r | ConvertTo-Json | Set-Content $record }

function Install {
  Say "install from $app"
  $made = Load-Record

  # The host's state: its identity key and the paired devices. Administrators and SYSTEM only.
  New-Item -ItemType Directory -Force $state | Out-Null
  & icacls.exe $state /inheritance:r /grant:r '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F' | Out-Null

  # The driver's settings, unless a driver of Guesthouse's already has some here.
  if (-not (Test-Path (Join-Path $settingsFolder 'vdd_settings.xml'))) {
    New-Item -ItemType Directory -Force $settingsFolder | Out-Null
    Copy-Item (Join-Path $app 'driver\vdd_settings.xml') $settingsFolder -Force
    $made.settingsMade = $true
    Save-Record $made
  }

  # Installed over an earlier install: its driver package goes first. pnputil keeps a package
  # it already has (same INF and files) whatever its catalog says, so the old one would stay,
  # signed by the certificate removed just below, and Windows would no longer trust it.
  foreach ($inf in @($made.driverInfs | Where-Object { $_ })) {
    $out = & pnputil.exe /delete-driver $inf /force
    Say "earlier driver package ${inf}: $($out | Select-Object -Last 1)"
  }
  $made.driverInfs = @()
  Save-Record $made

  # Sign the driver here: a catalog over its two files, signed by a certificate made now and
  # trusted only on this machine, whose private key is deleted before the driver goes in.
  Remove-Certificates
  $package = Join-Path $app 'driver\pkg'
  $catalog = Join-Path $app 'driver\GuesthouseDisplay.cat'
  Remove-Item (Join-Path $package 'GuesthouseDisplay.cat'), $catalog -ErrorAction SilentlyContinue
  New-FileCatalog -Path $package -CatalogFilePath $catalog -CatalogVersion 2.0 | Out-Null
  Move-Item $catalog (Join-Path $package 'GuesthouseDisplay.cat') -Force
  $certificate = New-SelfSignedCertificate -Type CodeSigningCert -Subject $subject -CertStoreLocation Cert:\LocalMachine\My `
    -KeyExportPolicy NonExportable -NotAfter (Get-Date).AddYears(10)
  $public = New-Object Security.Cryptography.X509Certificates.X509Certificate2 (, $certificate.Export('Cert'))
  foreach ($storeName in 'Root', 'TrustedPublisher') {
    $store = New-Object Security.Cryptography.X509Certificates.X509Store $storeName, 'LocalMachine'
    $store.Open('ReadWrite'); $store.Add($public); $store.Close()
  }
  $signed = Set-AuthenticodeSignature -FilePath (Join-Path $package 'GuesthouseDisplay.cat') -Certificate $certificate -HashAlgorithm SHA256
  Remove-Item "Cert:\LocalMachine\My\$($certificate.Thumbprint)" -DeleteKey
  if ($signed.Status -ne 'Valid') { throw "signing the driver failed: $($signed.StatusMessage)" }
  Say "driver signed by $($certificate.Thumbprint)"

  # Only a package that is new is ours: pnputil names an existing one if it is the same.
  $before = @(Driver-Packages)
  $inf = Join-Path $package 'GuesthouseDisplay.inf'
  $added = (& pnputil.exe /add-driver $inf) -join "`n"
  $new = @(Driver-Packages | Where-Object { $before -notcontains $_ })
  if ($new.Count -eq 0 -and $added -notmatch 'Published Name') { throw "pnputil did not take the driver: $added" }
  $made.driverInfs = @($new)
  Save-Record $made
  Say "driver packages added: $($new -join ', ') (pnputil: $(($added -split "`n" | Select-String 'Published Name') -join ''))"
  Add-Type $source
  if (Our-Devices) {
    $result = [RmDev]::Update($hardwareId, $inf)
  } else {
    $result = [RmDev]::Make($hardwareId, $inf)
    $made.deviceMade = $true
    Save-Record $made
  }
  Say "display device: $result"
  if ($result -notlike 'ok*') { throw "the display device: $result" }

  # Other computers reach the host on its port; nobody has to answer Windows' question.
  Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue | Remove-NetFirewallRule
  New-NetFirewallRule -DisplayName $name -Description 'remoteman: devices connecting to this computer' -Direction Inbound `
    -Action Allow -Protocol UDP -LocalPort $port -Program $exe -Profile Any | Out-Null
  Say "firewall rule for UDP $port"

  # Started at each logon, elevated (the driver's device lets in Administrators only), in the
  # session of whoever logs on: input, the cursor and mode changes work nowhere else.
  $action = New-ScheduledTaskAction -Execute $exe -Argument "--state `"$state`" --log `"$state\host.log`" --ask" -WorkingDirectory $app
  $trigger = New-ScheduledTaskTrigger -AtLogOn
  $principal = New-ScheduledTaskPrincipal -GroupId 'S-1-5-32-545' -RunLevel Highest
  $settings = New-ScheduledTaskSettingsSet -ExecutionTimeLimit ([TimeSpan]::Zero) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
    -MultipleInstances IgnoreNew -RestartCount 3 -RestartInterval (New-TimeSpan -Minutes 1) -StartWhenAvailable
  Register-ScheduledTask -TaskName $name -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
  Start-ScheduledTask -TaskName $name
  Say "task registered and started"
}

function Uninstall {
  Say "uninstall"
  $made = if (Test-Path $record) { Get-Content $record -Raw | ConvertFrom-Json } else { $null }
  if (Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue) {
    Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $name -Confirm:$false
  }
  # The host, stopped by its own pid, wherever it was started from this folder.
  Get-Process -Name remoteman-host -ErrorAction SilentlyContinue | Where-Object Path -eq $exe |
    ForEach-Object { Stop-Process -Id $_.Id -Force }
  Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue | Remove-NetFirewallRule

  if ($made -and $made.deviceMade) {
    foreach ($d in Our-Devices) { $out = & pnputil.exe /remove-device $d.InstanceId; Say "removed device $($d.InstanceId): $($out | Select-Object -Last 1)" }
  }
  foreach ($inf in @($made.driverInfs | Where-Object { $_ })) {
    # /uninstall also takes it off a device that was here before us, which then goes back to
    # the driver it had.
    $out = & pnputil.exe /delete-driver $inf /uninstall /force
    Say "driver package ${inf}: $($out | Select-Object -Last 1)"
  }
  Remove-Certificates
  if ($made -and $made.settingsMade) { Remove-Item -Recurse -Force $settingsFolder -ErrorAction SilentlyContinue }
  Remove-Item -Recurse -Force $state -ErrorAction SilentlyContinue
  $parent = Split-Path $state
  if ((Test-Path $parent) -and -not (Get-ChildItem $parent)) { Remove-Item $parent }
  Remove-Item $record -ErrorAction SilentlyContinue
  Say "uninstalled"
}

try {
  if ($Verb -eq 'install') { Install } else { Uninstall }
  exit 0
} catch {
  Say "FAILED: $($_.Exception.Message)"
  exit 1
}
