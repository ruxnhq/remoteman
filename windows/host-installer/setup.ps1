# What the host's installer does once its files are in place, and what its uninstaller undoes.
# Run by the installer (remoteman-host-setup.exe, or remoteman-host.iss), elevated, hidden; never
# by hand except to repair:
#   powershell -ExecutionPolicy Bypass -File setup.ps1 install|uninstall|steam
#              [-Trust FILE] [-Encoder auto|nvenc|mf|d3d12] [-NoPair]      (install only)
# -Trust FILE:  the host's trust list (one line per viewer: the SHA-256 of its certificate in hex,
#               a space, its name), put in place of the host's own (STATE\host\trusted) before
#               the service starts.
# -Encoder E:   the service's host is started with --encoder E.
# -NoPair:      the service's host is started with --no-pair (no --ask either): it never opens
#               pairing by itself; only the owner's Pair a device (ctl pair on) lets a new one in.
# Each install writes the service's command line from its own options: one without them goes back
# to the defaults (no --encoder; --ask when Remoteman Host isn't installed), and leaves the trust
# list as it is.
# install:   the display driver (remoteman's own user-mode display driver, windows/display-driver,
#            signed here with a certificate made on this machine, trusted only on it, its private
#            key deleted at once), the inbound firewall rule for the host's port, the service
#            "remoteman host" (automatic, LocalSystem) that keeps the host running in the
#            console's session from boot on (crates/remoteman-host-windows/src/service.rs), the
#            policy that lets services send Ctrl+Alt+Del, and the service started now. An earlier
#            install's logon task is removed.
# steam:     (a step of its own, run after install when the owner chose it) Steam's streaming
#            sound devices, "Steam Streaming Speakers" and "Steam Streaming Microphone", which the
#            host uses, installed from the owner's own Steam folder when Steam is there and they
#            are not: see Install-Steam. It never fails the install; uninstall leaves them.
# uninstall: all of that taken away again (the policy back as it was), and the host's state
#            (its identity and the paired devices) deleted.
# What install made is written to installed.json beside this script, so that uninstall takes
# away exactly that (a display device that was here before is left, with the driver it had).
# Exit code 0, or 1 with the reason as the last line of setup.log.
param(
  [Parameter(Mandatory)][ValidateSet('install', 'uninstall', 'steam')][string]$Verb,
  [string]$Trust = '',
  # The names remoteman-host.exe's --encoder takes (crates/remoteman-host-windows/src/main.rs).
  [ValidateSet('auto', 'nvenc', 'mf', 'd3d12')][string]$Encoder,
  [switch]$NoPair
)
$ErrorActionPreference = 'Stop'
$app = $PSScriptRoot
$exe = Join-Path $app 'remoteman-host.exe'
$state = Join-Path $env:ProgramData 'remoteman\host'
$record = Join-Path $app 'installed.json'
$log = Join-Path $app 'setup.log'
$port = 7791
$name = 'remoteman host'                   # the task's and the firewall rule's name
$hardwareId = 'Root\RemotemanDisplay'
$subject = 'CN=remoteman display driver, this machine only'
$ours = '*remoteman display driver, this machine only*'
$settingsFolder = Join-Path $env:ProgramData 'remoteman\display'  # where the driver reads its settings

# The installer writes to the same file between this script's lines, a line at a time, so each line
# is written sharing the file and tried again if it's busy. Add-Content shares it only for reading
# and doesn't try again, and stopped installs with a sharing violation. A line that still can't be
# written is lost, and the install goes on: the log is never a reason to fail.
function Say([string]$line) {
    $bytes = [System.Text.Encoding]::UTF8.GetBytes("$(Get-Date -Format s) $line`r`n")
    for ($try = 0; $try -lt 20; $try++) {
        try {
            $file = [System.IO.File]::Open($log, [System.IO.FileMode]::Append, [System.IO.FileAccess]::Write, [System.IO.FileShare]::ReadWrite)
            try { $file.Write($bytes, 0, $bytes.Length) } finally { $file.Dispose() }
            return
        } catch {
            Start-Sleep -Milliseconds 25
        }
    }
}

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
    return Make(id, inf, "4d36e968-e325-11ce-bfc1-08002be10318", "Display");
  }
  // A root-enumerated device with this hardware id, of this setup class, named ROOT/name/nnnn.
  public static string Make(string id, string inf, string cls, string name) {
    Guid g = new Guid(cls);
    IntPtr s = SetupDiCreateDeviceInfoList(ref g, IntPtr.Zero);
    SP d = new SP(); d.cbSize = Marshal.SizeOf(d);
    if (!SetupDiCreateDeviceInfoW(s, name, ref g, null, IntPtr.Zero, 1, ref d)) return "create failed " + Marshal.GetLastWin32Error();
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
  $r = [ordered]@{ driverInfs = @(); deviceMade = $false; settingsMade = $false; sasSet = $false; sasBefore = $null; vcamMade = $false; steamInfs = @(); steamDevices = @() }
  if (Test-Path $record) {
    $old = Get-Content $record -Raw | ConvertFrom-Json
    if ($old.PSObject.Properties.Name -contains 'vcamMade') { $r.vcamMade = [bool]$old.vcamMade }
    # What the steam step installed (Steam's sound devices): only recorded, uninstall leaves them.
    if ($old.PSObject.Properties.Name -contains 'steamInfs') { $r.steamInfs = @($old.steamInfs | Where-Object { $_ }); $r.steamDevices = @($old.steamDevices | Where-Object { $_ }) }
    $r.driverInfs = @($old.driverInfs | Where-Object { $_ })
    $r.deviceMade = [bool]$old.deviceMade
    $r.settingsMade = [bool]$old.settingsMade
    # What SoftwareSASGeneration was before the first install set it (null: not set).
    if ($old.PSObject.Properties.Name -contains 'sasBefore') { $r.sasBefore = $old.sasBefore; $r.sasSet = [bool]$old.sasSet }
  }
  $r
}
function Save-Record($r) { $r | ConvertTo-Json | Set-Content $record }

# The camera: a device's camera shown to the PC's programs as a camera (Windows 11's virtual
# camera). The host makes the camera when a device turns its camera on; Windows' Frame Server
# then loads vcam\remoteman_vcam_windows.dll, a COM in-process server, by this class id (the
# same as CLSID_VCAM in crates/remoteman-vcam-windows/src/lib.rs). Registered under HKLM, where
# the Frame Server (a service) looks; it is not per user. The host offers "camera" only when
# this key is there.
$vcamClsid = '{B3F1C4A2-6D5E-4F87-9A21-7C0E3D5B8A64}'
$vcamKey = "HKLM:\SOFTWARE\Classes\CLSID\$vcamClsid"
$vcamFolder = Join-Path $env:ProgramData 'remoteman\vcam'  # the ring file, where a section can't be

function Register-Camera($made) {
  $dll = Join-Path $app 'vcam\remoteman_vcam_windows.dll'
  if (-not (Test-Path $dll)) { Say "no camera driver in this install ($dll): the device's camera won't be shown"; return }
  New-Item -Path "$vcamKey\InprocServer32" -Force | Out-Null
  Set-ItemProperty -Path $vcamKey -Name '(default)' -Value 'remoteman camera'
  Set-ItemProperty -Path "$vcamKey\InprocServer32" -Name '(default)' -Value $dll
  Set-ItemProperty -Path "$vcamKey\InprocServer32" -Name 'ThreadingModel' -Value 'Both'
  $made.vcamMade = $true
  Save-Record $made
  Say "camera registered: $vcamClsid -> $dll"
}

function Unregister-Camera($made) {
  # Whatever is there under our class id goes (the key is ours alone), made by this install or an
  # earlier one.
  if (Test-Path $vcamKey) { Remove-Item -Path $vcamKey -Recurse -Force; Say "camera unregistered" }
  Remove-Item -Recurse -Force $vcamFolder -ErrorAction SilentlyContinue
}

# -Trust: the file given becomes the host's trust list, STATE\host\trusted, as the host reads it
# (crates/remoteman-transport/src/identity.rs, Trust): one line per viewer, the SHA-256 of its
# certificate as 64 hex digits, a space, its name. Every line is checked; the fingerprints are
# written in lower case, as the host compares them, in UTF-8 without a byte-order mark.
function Write-Trust([string]$file) {
  if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "the trust list $file isn't there" }
  $lines = @()
  foreach ($line in [IO.File]::ReadAllLines($file)) {
    $line = $line.Trim()
    if (-not $line) { continue }
    if ($line -notmatch '^([0-9A-Fa-f]{64})(\s+(.*))?$') { throw "the trust list $file has a line that isn't a fingerprint and a name: $line" }
    $lines += "$($Matches[1].ToLowerInvariant()) $($Matches[3])".TrimEnd()
  }
  if ($lines.Count -eq 0) { throw "the trust list $file has no fingerprints" }
  $folder = Join-Path $state 'host'
  New-Item -ItemType Directory -Force $folder | Out-Null
  [IO.File]::WriteAllText((Join-Path $folder 'trusted'), (($lines -join "`n") + "`n"), (New-Object Text.UTF8Encoding $false))
  Say "trust list from ${file}: $($lines.Count) device(s), $(($lines | ForEach-Object { $_.Substring(0, 12) }) -join ', ')"
}

function Install {
  Say "install from $app"
  $made = Load-Record

  # The host's state: its identity key and the paired devices. Administrators and SYSTEM only.
  New-Item -ItemType Directory -Force $state | Out-Null
  & icacls.exe $state /inheritance:r /grant:r '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F' | Out-Null

  # The driver's settings, unless an earlier install left some (the owner may have changed them).
  # The driver runs as LOCAL SERVICE, which may change them (its toggle commands write them back);
  # other users may only read them.
  if (-not (Test-Path (Join-Path $settingsFolder 'vdd_settings.xml'))) {
    New-Item -ItemType Directory -Force $settingsFolder | Out-Null
    & icacls.exe $settingsFolder /inheritance:r /grant:r '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F' '*S-1-5-19:(OI)(CI)M' '*S-1-5-32-545:(OI)(CI)RX' | Out-Null
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
  $catalog = Join-Path $app 'driver\RemotemanDisplay.cat'
  Remove-Item (Join-Path $package 'RemotemanDisplay.cat'), $catalog -ErrorAction SilentlyContinue
  New-FileCatalog -Path $package -CatalogFilePath $catalog -CatalogVersion 2.0 | Out-Null
  Move-Item $catalog (Join-Path $package 'RemotemanDisplay.cat') -Force
  $certificate = New-SelfSignedCertificate -Type CodeSigningCert -Subject $subject -CertStoreLocation Cert:\LocalMachine\My `
    -KeyExportPolicy NonExportable -NotAfter (Get-Date).AddYears(10)
  $public = New-Object Security.Cryptography.X509Certificates.X509Certificate2 (, $certificate.Export('Cert'))
  foreach ($storeName in 'Root', 'TrustedPublisher') {
    $store = New-Object Security.Cryptography.X509Certificates.X509Store $storeName, 'LocalMachine'
    $store.Open('ReadWrite'); $store.Add($public); $store.Close()
  }
  $signed = Set-AuthenticodeSignature -FilePath (Join-Path $package 'RemotemanDisplay.cat') -Certificate $certificate -HashAlgorithm SHA256
  Remove-Item "Cert:\LocalMachine\My\$($certificate.Thumbprint)" -DeleteKey
  if ($signed.Status -ne 'Valid') { throw "signing the driver failed: $($signed.StatusMessage)" }
  Say "driver signed by $($certificate.Thumbprint)"

  # Only a package that is new is ours: pnputil names an existing one if it is the same.
  $before = @(Driver-Packages)
  $inf = Join-Path $package 'RemotemanDisplay.inf'
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

  # Earlier installs started the host at each logon from a scheduled task: gone, or two hosts
  # would fight over the port.
  if (Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue) {
    Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $name -Confirm:$false
    Say "the logon task of an earlier install removed"
  }

  # Ctrl+Alt+Del from a device: Windows takes it from a service only where this policy allows
  # services (1, or 3 with Ease of Access programs). What it was is kept for the uninstaller.
  $policy = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System'
  $sas = (Get-ItemProperty $policy -Name SoftwareSASGeneration -ErrorAction SilentlyContinue).SoftwareSASGeneration
  if ($sas -ne 1 -and $sas -ne 3) {
    if (-not $made.sasSet) { $made.sasBefore = $sas; $made.sasSet = $true }
    Set-ItemProperty $policy -Name SoftwareSASGeneration -Value $(if ($sas -eq 2) { 3 } else { 1 }) -Type DWord
    Save-Record $made
    Say "SoftwareSASGeneration set (was $(if ($null -eq $sas) { 'not set' } else { $sas }))"
  }

  Register-Camera $made

  # The service: from boot, as SYSTEM, it keeps the host running in the console's session
  # (the sign-in screen's, the lock screen's, whoever is signed in), with these options.
  # New devices are asked about by the owner's app (Remoteman Host) when it is installed beside
  # the host; without it, by the host itself in a window of Windows' own (--ask). With -NoPair,
  # only when the owner opens pairing in the app; otherwise only the trust list's devices get in.
  $hostArgs = "--state `"$state`" --log `"$state\host.log`""
  if ($Encoder) { $hostArgs += " --encoder $Encoder" }
  if ($NoPair) { $hostArgs += ' --no-pair' }
  elseif (-not (Test-Path (Join-Path $app 'app\remoteman-host-app.exe'))) { $hostArgs += ' --ask' }
  $binary = "`"$exe`" service $hostArgs"
  $existing = Get-Service -Name $name -ErrorAction SilentlyContinue
  # The host down before its trust list is written: a running one writes it back from memory.
  if ($existing -and $existing.Status -ne 'Stopped') { Stop-Service -Name $name -Force -ErrorAction SilentlyContinue }
  if ($Trust) { Write-Trust $Trust }
  # The host's identity, made now if there is none (an earlier install's is kept, so its
  # fingerprint stays the same), and said here, before the service starts.
  $fingerprint = & $exe fingerprint --state $state
  if ($LASTEXITCODE -ne 0) { throw "the host's identity couldn't be made in $state (remoteman-host fingerprint ended with $LASTEXITCODE)" }
  Say "host fingerprint: $($fingerprint -join '')"
  if ($existing) {
    # Installed over an earlier install: the command line straight into the service's key
    # (sc.exe can't be handed one with quotes in it from here).
    Set-ItemProperty "HKLM:\SYSTEM\CurrentControlSet\Services\$name" -Name ImagePath -Value $binary -Type ExpandString
    Set-Service -Name $name -StartupType Automatic
  } else {
    New-Service -Name $name -BinaryPathName $binary -DisplayName $name -StartupType Automatic `
      -Description 'remoteman: lets your paired devices see and control this computer, from the sign-in screen on.' | Out-Null
  }
  # Started again if it ever ends: after 5 s, twice, then after a minute.
  $out = & sc.exe failure $name reset= 86400 actions= restart/5000/restart/5000/restart/60000
  if ($LASTEXITCODE -ne 0) { Say "the service's restart rule wasn't set: $out" }
  Start-Service -Name $name
  Say "service registered and started"

  # How it went, where an unelevated reader can see it (the host's own files are
  # Administrators' only): the host's answer, or the service's last words if there is none.
  Start-Sleep -Seconds 6
  $status = & $exe ctl status 2>&1
  Say "host: $($status -join ' ')"
  if ($LASTEXITCODE -ne 0) {
    Get-Content (Join-Path $state 'service.log') -Tail 15 -ErrorAction SilentlyContinue | ForEach-Object { Say "service.log: $_" }
  }
}

# Steam's streaming sound devices, from the owner's own Steam. The host plays a device's
# microphone into "Steam Streaming Microphone" and makes "Steam Streaming Speakers" the default
# output while a device listens; both are virtual audio devices of Valve's. remoteman carries
# none of Valve's files: Steam's bin\drivers.exe (signed by Valve) is a self-extracting program
# with an ordinary ZIP appended, whose Windows10\x64 folder has the .inf, .sys and .cat of each
# (the .cat signed by Microsoft's hardware publisher, the .sys by Valve). Each INF's model is
# ROOT\SteamStreamingSpeakers / ROOT\SteamStreamingMicrophone, class MEDIA, a root device whose
# INF has an AddDevice line: `pnputil /add-driver X.inf /install` adds the package and makes the
# device (made by hand as well, each device came twice: tried on a VM, docs/PLAN.md). Only if
# that leaves no device is it made here as the display device is. drivers.exe is never run.
$steamDevices = @(
  @{ Name = 'SteamStreamingSpeakers';   HardwareId = 'ROOT\SteamStreamingSpeakers' },
  @{ Name = 'SteamStreamingMicrophone'; HardwareId = 'ROOT\SteamStreamingMicrophone' }
)
$mediaClass = '{4d36e96c-e325-11ce-bfc1-08002be10318}'

# Steam's folder: HKLM\SOFTWARE\WOW6432Node\Valve\Steam InstallPath (the same the installer reads).
function Steam-Folder {
  $p = (Get-ItemProperty 'HKLM:\SOFTWARE\WOW6432Node\Valve\Steam' -Name InstallPath -ErrorAction SilentlyContinue).InstallPath
  if ($p -and (Test-Path -LiteralPath $p -PathType Container)) { $p }
}

function Steam-Device([string]$id) {
  # The hardware ids come with the device list (Win32_PnPEntity's HardwareID): asking each device
  # for its property instead took minutes on a PC with 300 devices (2026-11-20).
  Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { $_.HardwareID -contains $id } | Select-Object -First 1
}

function Steam-InStore([string]$inf) { ((& pnputil.exe /enum-drivers) -join "`n") -match "(?im)^\s*Original Name:\s*$([regex]::Escape($inf))\s*$" }

# The wanted entries' files out of the ZIP appended to an exe. .NET's ZipArchive counts offsets
# from the start of the file, so the stub in front is cut off first (its length is what the end
# record's directory position falls short by).
function Extract-Appended-Zip([string]$path, [string[]]$wanted, [string]$into) {
  Add-Type -AssemblyName System.IO.Compression
  $bytes = [IO.File]::ReadAllBytes($path)
  $eocd = -1
  for ($i = $bytes.Length - 22; $i -ge [Math]::Max(0, $bytes.Length - 22 - 65535); $i--) {
    if ($bytes[$i] -eq 0x50 -and $bytes[$i+1] -eq 0x4B -and $bytes[$i+2] -eq 5 -and $bytes[$i+3] -eq 6) { $eocd = $i; break }
  }
  if ($eocd -lt 0) { throw "$path has no ZIP in it" }
  $start = $eocd - [BitConverter]::ToUInt32($bytes, $eocd + 12) - [BitConverter]::ToUInt32($bytes, $eocd + 16)
  if ($start -lt 0) { throw "$path's ZIP offsets do not line up" }
  $ms = New-Object IO.MemoryStream
  $ms.Write($bytes, $start, $bytes.Length - $start); $ms.Position = 0
  $zip = New-Object IO.Compression.ZipArchive($ms, [IO.Compression.ZipArchiveMode]::Read)
  $got = @()
  foreach ($e in $zip.Entries) {
    if ($wanted -contains $e.FullName) {
      $in = $e.Open(); $f = [IO.File]::Create((Join-Path $into $e.Name))
      try { $in.CopyTo($f) } finally { $f.Dispose(); $in.Dispose() }
      $got += $e.Name
    }
  }
  $zip.Dispose()
  $got
}

function Install-Steam {
  $made = Load-Record
  $steam = Steam-Folder
  if (-not $steam) { Say "steam: Steam isn't installed here; nothing to do"; return }
  $drivers = Join-Path $steam 'bin\drivers.exe'
  if (-not (Test-Path -LiteralPath $drivers)) { Say "steam: no bin\drivers.exe in $steam; nothing to do"; return }
  $todo = @($steamDevices | Where-Object { -not ((Steam-InStore "$($_.Name).inf") -and (Steam-Device $_.HardwareId)) })
  if ($todo.Count -eq 0) { Say "steam: Steam's sound devices are installed already"; return }
  $had = @{}; foreach ($d in $steamDevices) { if (Steam-Device $d.HardwareId) { $had[$d.HardwareId] = $true } }
  Say "steam: installing $(($todo | ForEach-Object { $_.Name }) -join ', ') from $drivers"
  # Administrators and SYSTEM only: what is installed is what was extracted.
  $work = Join-Path $env:ProgramData ('remoteman\steam-drivers-' + [guid]::NewGuid().ToString('N'))
  New-Item -ItemType Directory -Force $work | Out-Null
  & icacls.exe $work /inheritance:r /grant:r '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-18:(OI)(CI)F' | Out-Null
  try {
    $files = foreach ($d in $todo) { foreach ($ext in 'inf', 'sys', 'cat') { "drivers/Windows10/x64/$($d.Name).$ext" } }
    $got = @(Extract-Appended-Zip $drivers $files $work)
    Say "steam: extracted $($got -join ', ')"
    Add-Type $source
    foreach ($d in $todo) {
      $inf = Join-Path $work "$($d.Name).inf"
      try {
        foreach ($ext in 'inf', 'sys', 'cat') { if (-not (Test-Path (Join-Path $work "$($d.Name).$ext"))) { throw "$($d.Name).$ext isn't in drivers.exe" } }
        # Only what Windows says is signed goes in.
        foreach ($f in "$($d.Name).cat", "$($d.Name).sys") {
          $sig = Get-AuthenticodeSignature (Join-Path $work $f)
          if ($sig.Status -ne 'Valid') { throw "$f's signature isn't valid ($($sig.Status))" }
          Say "steam: $f signed by $($sig.SignerCertificate.Subject.Split(',')[0])"
        }
        $before = @(Driver-Packages)
        $added = (& pnputil.exe /add-driver $inf /install) -join "`n"
        $new = @(Driver-Packages | Where-Object { $before -notcontains $_ })
        if ($new.Count -eq 0 -and $added -notmatch 'Published Name') { throw "pnputil did not take the driver: $added" }
        Say "steam: $($d.Name) driver package: $((($added -split "`n") | Select-String 'Published Name') -join '') new: $($new -join ', ')"
        if ($new.Count -gt 0) { $made.steamInfs = @($made.steamInfs) + $new; Save-Record $made }
        $device = Steam-Device $d.HardwareId
        if ($device) {
          $result = "ok (made by pnputil: $($device.InstanceId), $($device.Status))"
        } else {
          Say "steam: pnputil made no $($d.HardwareId) device; making it"
          $result = [RmDev]::Make($d.HardwareId, $inf, $mediaClass, $d.Name)
        }
        # Ours only if it wasn't there before this step (the list at the top said so).
        if (-not $had.ContainsKey($d.HardwareId)) { $made.steamDevices = @($made.steamDevices) + $d.HardwareId; Save-Record $made }
        Say "steam: $($d.Name) device: $result"
        if ($result -notlike 'ok*') { throw $result }
      } catch {
        Say "steam: $($d.Name) failed: $($_.Exception.Message)"
        $script:steamFailed = $true
      }
    }
  } finally {
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
  }
}

function Uninstall {
  Say "uninstall"
  $made = if (Test-Path $record) { Get-Content $record -Raw | ConvertFrom-Json } else { $null }
  if (Get-Service -Name $name -ErrorAction SilentlyContinue) {
    Stop-Service -Name $name -Force -ErrorAction SilentlyContinue
    $out = & sc.exe delete $name
    Say "service removed: $out"
  }
  if (Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue) {
    Stop-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
    Unregister-ScheduledTask -TaskName $name -Confirm:$false
  }
  # The Ctrl+Alt+Del policy as it was before the first install.
  if ($made -and $made.PSObject.Properties.Name -contains 'sasSet' -and $made.sasSet) {
    $policy = 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System'
    if ($null -eq $made.sasBefore) { Remove-ItemProperty $policy -Name SoftwareSASGeneration -ErrorAction SilentlyContinue }
    else { Set-ItemProperty $policy -Name SoftwareSASGeneration -Value ([int]$made.sasBefore) -Type DWord }
    Say "SoftwareSASGeneration put back"
  }
  # The host, stopped by its own pid, wherever it was started from this folder.
  Get-Process -Name remoteman-host -ErrorAction SilentlyContinue | Where-Object Path -eq $exe |
    ForEach-Object { Stop-Process -Id $_.Id -Force }
  Get-NetFirewallRule -DisplayName $name -ErrorAction SilentlyContinue | Remove-NetFirewallRule
  Unregister-Camera $made

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
  # Steam's sound devices, if the install put them there, stay: Steam uses them too (the record
  # of them goes with installed.json).
  if ($made -and $made.PSObject.Properties.Name -contains 'steamDevices' -and @($made.steamDevices).Count -gt 0) {
    Say "Steam's sound devices (installed by this install) are left in place: $(@($made.steamDevices) -join ', ')"
  }
  if ($made -and $made.settingsMade) { Remove-Item -Recurse -Force $settingsFolder -ErrorAction SilentlyContinue }
  Remove-Item -Recurse -Force $state -ErrorAction SilentlyContinue
  $parent = Split-Path $state
  if ((Test-Path $parent) -and -not (Get-ChildItem $parent)) { Remove-Item $parent }
  Remove-Item $record -ErrorAction SilentlyContinue
  Say "uninstalled"
}

try {
  if ($Verb -eq 'steam') {
    # Never fails the install: what went wrong is in the log, and the exit code says so.
    $script:steamFailed = $false
    try { Install-Steam } catch { Say "steam: $($_.Exception.Message)"; $script:steamFailed = $true }
    # The host looks for a microphone to play a device's into when it starts, and it started
    # before this step: started again, it finds Steam's.
    if ((Get-Service -Name $name -ErrorAction SilentlyContinue).Status -eq 'Running') {
      Restart-Service -Name $name -Force -ErrorAction SilentlyContinue
      Say "steam: the host started again, to find Steam's microphone"
    }
    if ($script:steamFailed) { Say "FAILED: Steam's sound devices weren't installed (see the lines above)"; exit 1 }
    exit 0
  }
  if ($Verb -eq 'install') { Install } else { Uninstall }
  exit 0
} catch {
  Say "FAILED: $($_.Exception.Message)"
  exit 1
}
