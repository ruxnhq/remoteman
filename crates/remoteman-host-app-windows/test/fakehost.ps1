# A stand-in for the running host's control pipe, to test Remoteman Host (remoteman-host-app.exe)
# without a host, or without touching the one installed:
#   fakehost.ps1 [-Port 7899] [-Log FILE]
# It answers `status` with JSON shaped like crates/remoteman-host-windows/src/control.rs writes,
# keeps the state the app's words change (sharing, pairing, the switches, devices, a sent file),
# and writes every line it is told into LOG, so a test can show what the app said. Start the app
# with the same --port. Words only a test says, to play a device:
#   fake pending NAME CODE FINGERPRINT   a new device waits for the owner's word (_ in NAME: a space)
#   fake client NAME FINGERPRINT         a paired device connects
#   fake gone                            the connected device leaves
#   fake stop                            the fake host ends
# One line in, one answer out, as the real pipe (control.rs `serve`).
param([int]$Port = 7899, [string]$Log = "C:\rm\shots-host\fakehost.log")

$state = [ordered]@{
  address = "0.0.0.0:$Port"; fingerprint = "c41e77a09b2d5f36aa18e2b7"; pairing = $false; pending = $null
  devices = [System.Collections.ArrayList]@(@{ fingerprint = "2b90e4c715af83d6"; name = "iPhone"; connected = $false })
  daemon = $false; paused = $false; name = "DEN-PC"; clipboard = $false; files = $false; lock_on_leave = $false; speakers_follow = $true; microphone = $true
  client = $null; sent = [ordered]@{ answered = 0; ok = $false; said = "" }
}

function Say([string]$line) { Add-Content -Path $Log -Value ("{0} {1}" -f (Get-Date -Format "HH:mm:ss.fff"), $line) }

function Answer([string]$line) {
  if ($line -match '^send (.+)$') {
    if (-not $state.files) { return "files are switched off on this computer" }
    if (-not $state.client) { return "no device is connected" }
    # The device takes it at once.
    $state.sent.answered++; $state.sent.ok = $true; $state.sent.said = "Downloads\" + (Split-Path -Leaf $Matches[1].Trim('"'))
    return "ok"
  }
  $w = $line.Trim() -split '\s+'
  switch -regex ($line.Trim()) {
    '^(status)?$' { return ($state | ConvertTo-Json -Compress -Depth 6) }
    '^pair (on|off)$' {
      $state.pairing = ($w[1] -eq "on"); if ($w[1] -eq "off") { $state.pending = $null }; return "ok"
    }
    '^pair (accept|decline)$' {
      if (-not $state.pending) { return "no device is waiting" }
      if ($w[1] -eq "accept") { [void]$state.devices.Add(@{ fingerprint = $state.pending.fingerprint; name = $state.pending.name; connected = $false }) }
      $state.pending = $null; $state.pairing = $false; return "ok"
    }
    '^sharing (on|off)$' {
      $state.paused = ($w[1] -eq "off")
      if ($state.paused) { $state.pairing = $false; $state.pending = $null; $state.client = $null; foreach ($d in $state.devices) { $d.connected = $false } }
      return "ok"
    }
    '^drop$' {
      if (-not $state.client) { return "no device is connected" }
      $state.client = $null; foreach ($d in $state.devices) { $d.connected = $false }; return "ok"
    }
    '^revoke \S+$' {
      $d = $state.devices | Where-Object { $_.fingerprint -eq $w[1] } | Select-Object -First 1
      if (-not $d) { return "no such device" }
      if ($d.connected) { $state.client = $null }
      $state.devices.Remove($d); return "ok"
    }
    '^allow (clipboard|files) (on|off)$' { $state[$w[1]] = ($w[2] -eq "on"); return "ok" }
    '^set (lock|speakers) (on|off)$' { $state[$(if ($w[1] -eq 'lock') { 'lock_on_leave' } else { 'speakers_follow' })] = ($w[2] -eq "on"); return "ok" }
    '^fake pending' { $state.pending = [ordered]@{ name = ($w[2] -replace '_', ' '); fingerprint = $w[4]; code = $w[3]; seconds = 60 }; return "ok" }
    '^fake client' {
      $d = $state.devices | Where-Object { $_.fingerprint -eq $w[3] } | Select-Object -First 1
      if (-not $d) { $d = @{ fingerprint = $w[3]; name = ($w[2] -replace '_', ' '); connected = $false }; [void]$state.devices.Add($d) }
      $d.connected = $true
      $state.client = [ordered]@{ name = ($w[2] -replace '_', ' '); since = [DateTimeOffset]::UtcNow.ToUnixTimeSeconds() - 14 * 60; width = 2752; height = 2064; refresh = 120; placement = "extra"; picture = "HEVC 10-bit, 28 Mb/s"; sound = $true }
      return "ok"
    }
    '^fake gone$' { $state.client = $null; foreach ($d in $state.devices) { $d.connected = $false }; return "ok" }
  }
  return "status | pair on|off|accept|decline | allow clipboard|files on|off | set lock|speakers on|off | drop | revoke FINGERPRINT | send PATH | sharing on|off"
}

New-Item -ItemType Directory -Force (Split-Path $Log) | Out-Null
Say "fake host on \\.\pipe\remoteman-host-$Port"
while ($true) {
  $pipe = New-Object System.IO.Pipes.NamedPipeServerStream("remoteman-host-$Port", [System.IO.Pipes.PipeDirection]::InOut, 10, [System.IO.Pipes.PipeTransmissionMode]::Byte)
  $pipe.WaitForConnection()
  try {
    $bytes = New-Object System.Collections.Generic.List[byte]
    while ($bytes.Count -lt 4096) { $b = $pipe.ReadByte(); if ($b -lt 0 -or $b -eq 10) { break }; $bytes.Add([byte]$b) }
    $line = [System.Text.Encoding]::UTF8.GetString($bytes.ToArray()).Trim()
    # `status` comes once a second: not written down, so the log is the owner's words.
    if ($line -ne "status") { Say $line }
    $said = Answer $line
    $out = [System.Text.Encoding]::UTF8.GetBytes("$said`n")
    $pipe.Write($out, 0, $out.Length); $pipe.Flush(); $pipe.WaitForPipeDrain()
  } catch { Say "error: $_" }
  finally { $pipe.Dispose() }
  if ($line -eq "fake stop") { break }
}
