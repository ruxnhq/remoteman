; The installer of remoteman's Windows host (Inno Setup 6). Built by build.ps1 beside it, which
; puts the files in .\stage first; docs/WINDOWS-PACKAGING.md says how and why.
; One elevation prompt (the installer runs as an administrator), no terminal: the files go to
; Program Files, then setup.ps1 installs the display driver, adds the firewall rule, registers
; the task that starts the host at each logon and starts it. The uninstaller (Settings > Apps)
; undoes all of it.

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

[Setup]
AppId={{6A0F3C2E-5B7D-4E8A-9C1F-2D4B6E8A0C13}
AppName=remoteman host
AppVersion={#AppVersion}
AppVerName=remoteman host {#AppVersion}
AppPublisher=remoteman
DefaultDirName={autopf}\remoteman host
DisableDirPage=yes
DisableProgramGroupPage=yes
DisableReadyPage=no
PrivilegesRequired=admin
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041
OutputDir=out
OutputBaseFilename=remoteman-host-setup-{#AppVersion}
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
UninstallDisplayName=remoteman host
UninstallDisplayIcon={app}\remoteman-host.exe
; setup.ps1 stops the host itself, by its pid.
CloseApplications=no
SetupLogging=yes

[Files]
Source: "stage\remoteman-host.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "stage\setup.ps1"; DestDir: "{app}"; Flags: ignoreversion
Source: "stage\driver\*"; DestDir: "{app}\driver"; Flags: ignoreversion recursesubdirs

[UninstallDelete]
; What setup.ps1 writes beside itself, and the catalog it makes.
Type: files; Name: "{app}\installed.json"
Type: files; Name: "{app}\setup.log"
Type: files; Name: "{app}\driver\pkg\GuesthouseDisplay.cat"
Type: dirifempty; Name: "{app}\driver\pkg"
Type: dirifempty; Name: "{app}\driver"
Type: dirifempty; Name: "{app}"

[Code]
var
  SetupFailed: Boolean;

function RunSetup(Verb: String): Integer;
var
  Code: Integer;
begin
  if not Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
      '-NoProfile -ExecutionPolicy Bypass -File "' + ExpandConstant('{app}\setup.ps1') + '" ' + Verb,
      ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code) then
    Code := -1;
  Result := Code;
end;

// Installed over an earlier install: the running host holds its program file, so it is
// stopped first (its task ended, then the process by its pid, only the one in this folder).
function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  Code: Integer;
begin
  Exec(ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe'),
    '-NoProfile -Command "Stop-ScheduledTask -TaskName ''remoteman host'' -ErrorAction SilentlyContinue; ' +
    'Get-Process -Name remoteman-host -ErrorAction SilentlyContinue | Where-Object Path -eq ''' +
    ExpandConstant('{app}\remoteman-host.exe') +
    ''' | ForEach-Object { Stop-Process -Id $_.Id -Force; $_.WaitForExit(5000) | Out-Null }"',
    '', SW_HIDE, ewWaitUntilTerminated, Code);
  Result := '';
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    WizardForm.StatusLabel.Caption := 'Installing the display driver and starting the host...';
    if RunSetup('install') <> 0 then
    begin
      SetupFailed := True;
      MsgBox('remoteman host could not finish installing. What went wrong is in ' +
        ExpandConstant('{app}\setup.log') + '.', mbError, MB_OK);
    end;
  end;
end;

// A failed setup.ps1 shows in the installer's exit code (1), for silent installs.
function GetCustomSetupExitCode: Integer;
begin
  if SetupFailed then Result := 1 else Result := 0;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    RunSetup('uninstall');
end;
