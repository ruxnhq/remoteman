# Builds the WinUI 3 viewer. Run inside Windows (the VM) with the .NET 10 SDK in C:\dotnet (or on
# the path). The engine, remoteman_engine.dll, and FFmpeg's DLLs are taken from .\native\
# (scripts/win-viewer-winui.sh puts them there).
#   powershell -File build.ps1 [-Out C:\rm\app]
#       one folder that runs with nothing installed (.NET and the Windows App SDK inside it),
#       for development and the VM
#   powershell -File build.ps1 -Packaged [-Out C:\rm\msix] [-TestCert]
#       the MSIX for the Microsoft Store: remoteman_VERSION_x64.msix in -Out, unsigned (the
#       Store signs what is uploaded), .NET trimmed, the Windows App SDK as the Store's
#       framework package. -TestCert also signs it with a test certificate of this machine's
#       (made once, subject = the manifest's Publisher), for installing it here; the
#       certificate's public half is left beside it as remoteman-test.cer.
param([string]$Out = "", [switch]$Packaged, [switch]$TestCert)
$ErrorActionPreference = "Stop"
$dotnet = if (Test-Path C:\dotnet\dotnet.exe) { "C:\dotnet\dotnet.exe" } else { "dotnet" }
if (-not $env:DOTNET_CLI_HOME) { $env:DOTNET_CLI_HOME = "$env:LOCALAPPDATA\remoteman-build" }
$env:DOTNET_CLI_TELEMETRY_OPTOUT = 1
$env:DOTNET_NOLOGO = 1
Set-Location $PSScriptRoot
if ($Packaged) {
  if (-not $Out) { $Out = "C:\rm\msix" }
  Remove-Item -Recurse -Force bin\AppPackages -ErrorAction SilentlyContinue
  & $dotnet publish Remoteman.csproj -c Release -r win-x64 -p:Platform=x64 -p:Packaged=true `
      -p:GenerateAppxPackageOnBuild=true -nologo -clp:"ErrorsOnly;Summary"
  if ($LASTEXITCODE -ne 0) { throw "the MSIX build failed ($LASTEXITCODE)" }
  $msix = Get-ChildItem bin\AppPackages -Recurse -Filter *.msix | Select-Object -First 1
  if (-not $msix) { throw "no .msix was made" }
  New-Item -ItemType Directory -Force $Out | Out-Null
  $target = Join-Path $Out $msix.Name
  Copy-Item $msix.FullName $target -Force
  if ($TestCert) {
    $kits = Get-ChildItem "${env:ProgramFiles(x86)}\Windows Kits\10\bin\*\x64\signtool.exe" | Sort-Object FullName | Select-Object -Last 1
    $publisher = ([xml](Get-Content Package.appxmanifest)).Package.Identity.Publisher
    $cert = Get-ChildItem Cert:\CurrentUser\My | Where-Object { $_.Subject -eq $publisher -and $_.FriendlyName -eq "remoteman MSIX test" } | Select-Object -First 1
    if (-not $cert) {
      $cert = New-SelfSignedCertificate -Type Custom -Subject $publisher -KeyUsage DigitalSignature -FriendlyName "remoteman MSIX test" `
        -CertStoreLocation Cert:\CurrentUser\My -TextExtension @("2.5.29.37={text}1.3.6.1.5.5.7.3.3", "2.5.29.19={text}")
    }
    Export-Certificate -Cert $cert -FilePath (Join-Path $Out "remoteman-test.cer") | Out-Null
    & $kits.FullName sign /fd SHA256 /sha1 $cert.Thumbprint $target
    if ($LASTEXITCODE -ne 0) { throw "signing with the test certificate failed" }
  }
  "made $target ($([math]::Round((Get-Item $target).Length / 1MB, 1)) MB)"
  return
}
if (-not $Out) { $Out = "C:\rm\app" }
& $dotnet publish Remoteman.csproj -c Release -r win-x64 -p:Platform=x64 -o $Out -nologo -clp:"ErrorsOnly;Summary"
if ($LASTEXITCODE -ne 0) { throw "the build failed ($LASTEXITCODE)" }
"built into $Out"
