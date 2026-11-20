# Start the Reactor viewer for a test: a scratch state, its log, a deadline; prints the pid.
param([string]$Exe = "$PSScriptRoot\..\..\..\target\debug\remoteman-app.exe",
      [string]$Log = "C:\rm\shots-viewer\run.log", [string]$State = "C:\rm\state-viewer", [int]$Seconds = 120,
      [string[]]$More = @())
$env:PATH = "C:\ffmpeg\bin;$env:PATH"
Remove-Item $Log, "$Log.view" -ErrorAction SilentlyContinue
$args2 = @("--state", $State, "--log", $Log, "--seconds", "$Seconds", "--browse", "_rmtest._udp.local") + $More
$p = Start-Process -FilePath $Exe -ArgumentList $args2 -PassThru
for ($i = 0; $i -lt 50; $i++) { Start-Sleep -Milliseconds 200; if ((Get-Content $Log -ErrorAction SilentlyContinue) -match "window: made") { break } }
Start-Sleep -Milliseconds 1500
$p.Id
