# Builds remoteman's display driver, RemotemanDisplay.dll and its INF, without Visual Studio's
# driver-kit integration: the compiler and linker from Visual Studio Build Tools (2022 or later,
# "Desktop development with C++"), the headers and libraries from the Windows Driver Kit's NuGet
# packages. Nothing needs an administrator.
#
#   powershell -ExecutionPolicy Bypass -File build.ps1 [-Kit FOLDER] [-Out FOLDER] [-Version 0.1.1.0]
#
# -Kit is a folder holding the three NuGet packages unpacked, each in a folder named
# <id>.<version> (as nuget.exe or `nuget install` leaves them):
#   microsoft.windows.wdk.x64, microsoft.windows.sdk.cpp, microsoft.windows.sdk.cpp.x64
# all of one version (10.0.28000.2526 was used first). The default is $env:REMOTEMAN_WDK, else
# C:\rm\wdk\packages. -Out (default .\out) gets RemotemanDisplay.dll, RemotemanDisplay.inf
# (stamped), vdd_settings.xml and both licences: what remoteman host's installer carries. The
# catalog is made and signed on the machine that installs it (windows/host-installer/setup.ps1).
#
# The settings mirror RemotemanDisplay.vcxproj and the kit's WindowsUserModeDriver10.0 toolset
# (UMDF 2.25, IddCx 1.10, universal, C++17, /EHa). Not done here: the Spectre-mitigated runtime
# (Build Tools doesn't have it unless its component is added) and code analysis.
param([string]$Kit = $(if ($env:REMOTEMAN_WDK) { $env:REMOTEMAN_WDK } else { 'C:\rm\wdk\packages' }),
      [string]$Out = (Join-Path $PSScriptRoot 'out'),
      [string]$Version = '0.1.1.0')
$ErrorActionPreference = 'Stop'
$src = Join-Path $PSScriptRoot 'RemotemanDisplay'

# The compiler: the newest MSVC in the newest Build Tools or Visual Studio with C++.
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$vs = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
if (-not $vs) { throw 'Visual Studio Build Tools with the C++ tools (x64) are not installed' }
$msvc = Get-ChildItem "$vs\VC\Tools\MSVC" -Directory | Sort-Object { [version]$_.Name } | Select-Object -Last 1
$bin = "$($msvc.FullName)\bin\Hostx64\x64"

# The kit: one version of all three packages.
function Package([string]$id) {
  $p = Get-ChildItem $Kit -Directory -Filter "$id.*" | Where-Object { $_.Name -match "^$([regex]::Escape($id))\.\d" } |
    Sort-Object { [version]($_.Name.Substring($id.Length + 1) -replace '-.*$', '') } | Select-Object -Last 1
  if (-not $p) { throw "the NuGet package $id is not in $Kit" }
  $p.FullName
}
$wdk = Package 'microsoft.windows.wdk.x64'
$sdk = Package 'microsoft.windows.sdk.cpp'
$sdk64 = Package 'microsoft.windows.sdk.cpp.x64'
$kv = (Get-ChildItem "$wdk\c\Include" -Directory | Where-Object Name -match '^10\.' | Sort-Object { [version]$_.Name } | Select-Object -Last 1).Name
"compiler: $($msvc.Name); kit: $kv"

$umdf = '2.25'; $iddcx = '1.10'
$include = @(
  "$($msvc.FullName)\include",
  "$sdk\c\Include\$kv\ucrt", "$sdk\c\Include\$kv\um", "$sdk\c\Include\$kv\shared", "$sdk\c\Include\$kv\winrt",
  "$wdk\c\Include\$kv\um", "$wdk\c\Include\$kv\shared", "$wdk\c\Include\wdf\umdf\$umdf", "$wdk\c\Include\$kv\um\iddcx\$iddcx",
  (Join-Path $PSScriptRoot 'Common\Include'))
$libs = @(
  "$($msvc.FullName)\lib\onecore\x64", "$($msvc.FullName)\lib\x64",
  "$sdk64\c\ucrt\x64", "$sdk64\c\um\x64",
  "$wdk\c\Lib\wdf\umdf\x64\$umdf", "$wdk\c\Lib\$kv\um\x64\iddcx\$iddcx", "$wdk\c\Lib\$kv\um\x64")
foreach ($d in $include + $libs) { if (-not (Test-Path $d)) { throw "missing: $d" } }
$env:INCLUDE = $include -join ';'
$env:LIB = $libs -join ';'

$defines = @('_WIN64', '_AMD64_', 'AMD64', 'WIN32_LEAN_AND_MEAN=1', 'NDEBUG',
  '_WIN32_WINNT=0x0A00', 'WINVER=0x0A00', 'WINNT=1', 'NTDDI_VERSION=0xA000012',
  "UMDF_VERSION_MAJOR=2", "UMDF_VERSION_MINOR=$($umdf.Split('.')[1])", "UMDF_MINIMUM_VERSION_REQUIRED=$($umdf.Split('.')[1])",
  'UMDF_USING_NTSTATUS', '_UNICODE', 'UNICODE',
  '_ATL_NO_WIN_SUPPORT', 'UMDF_DRIVER', 'IDDCX_VERSION_MAJOR=1', "IDDCX_VERSION_MINOR=$($iddcx.Split('.')[1])", 'IDDCX_MINIMUM_VERSION_REQUIRED=3')

# The sources, as the project lists them.
$project = [xml](Get-Content (Join-Path $src 'RemotemanDisplay.vcxproj'))
$sources = $project.Project.ItemGroup.ClCompile | Where-Object { $_ -and $_.Include } | ForEach-Object { Join-Path $src $_.Include }
$obj = Join-Path $PSScriptRoot 'obj'
Remove-Item -Recurse -Force $obj, $Out -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $obj, $Out | Out-Null

$cl = @('/nologo', '/c', '/O2', '/MT', '/EHa', '/std:c++17', '/permissive', '/W3', '/GS', '/guard:cf', '/Gy', '/Zi', '/utf-8',
  "/Fd$obj\RemotemanDisplay.pdb", "/Fo$obj\") + ($defines | ForEach-Object { "/D$_" })
& "$bin\cl.exe" @cl @sources
if ($LASTEXITCODE -ne 0) { throw "compiling failed ($LASTEXITCODE)" }

$dll = Join-Path $Out 'RemotemanDisplay.dll'
$link = @('/nologo', '/DLL', "/OUT:$dll", '/SUBSYSTEM:WINDOWS', '/DEBUG', "/PDB:$obj\RemotemanDisplay.pdb", '/OPT:REF', '/OPT:ICF',
  '/GUARD:CF', '/DYNAMICBASE', '/NXCOMPAT', '/NODEFAULTLIB:kernel32.lib', '/NODEFAULTLIB:user32.lib')
& "$bin\link.exe" @link (Get-ChildItem "$obj\*.obj").FullName WdfDriverStubUm.lib iddcxstub.lib ntdll.lib OneCoreUAP.lib avrt.lib
if ($LASTEXITCODE -ne 0) { throw "linking failed ($LASTEXITCODE)" }

# The INF, stamped as the kit's toolset does: the architecture, the UMDF version and DriverVer.
$inf = Join-Path $Out 'RemotemanDisplay.inf'
Copy-Item (Join-Path $src 'RemotemanDisplay.inf') $inf
$stampinf = Get-ChildItem "$wdk\c\bin\$kv\x64\stampinf.exe" | Select-Object -First 1
& $stampinf.FullName -f $inf -a amd64 -d * -v $Version -u "$umdf.0"
if ($LASTEXITCODE -ne 0) { throw "stampinf failed ($LASTEXITCODE)" }
Copy-Item (Join-Path $PSScriptRoot 'vdd_settings.xml'), (Join-Path $PSScriptRoot 'LICENSE'), (Join-Path $PSScriptRoot 'LICENSE-MS-PL') $Out
"made $dll ($([math]::Round((Get-Item $dll).Length / 1KB)) KB) and its INF in $Out"
