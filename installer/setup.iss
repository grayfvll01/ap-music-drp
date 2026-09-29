; Installer for Apple Music Discord Presence (Inno Setup 6).
; Built by CI:  iscc /DAppVersion=1.2.3 installer\setup.iss
; Per-user install (no admin prompt): %LOCALAPPDATA%\Programs, Start menu,
; optional desktop icon and "Start with Windows". Also used for updates:
; the app downloads the new setup and runs it with /SILENT.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define AppName "Apple Music Discord Presence"
#define AppExe "AppleMusicDiscordPresence.exe"
#define AppId "AppleMusicDiscordPresence"
#define Repo "https://github.com/grayfvll01/apple-music-discord-presence"

[Setup]
AppId={{8C3B6E2A-5D1F-4B7E-9A64-2F0D7C1E5B93}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=grayfvll01
AppPublisherURL={#Repo}
AppSupportURL={#Repo}/issues
AppUpdatesURL={#Repo}/releases
PrivilegesRequired=lowest
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
DisableDirPage=yes
DisableReadyPage=yes
UsePreviousAppDir=yes
OutputDir=..\target\installer
OutputBaseFilename=AppleMusicDiscordPresence-Setup
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
WizardStyle=modern
Compression=lzma2/ultra64
SolidCompression=yes
; Close a running copy (it clears its Discord status on the way out).
CloseApplications=force
; Leave a log in %TEMP% ("Setup Log <date>.txt") for troubleshooting updates.
SetupLogging=yes
RestartApplications=no
VersionInfoVersion={#AppVersion}
VersionInfoProductName={#AppName}
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0

[Tasks]
Name: "desktopicon"; Description: "Add a desktop icon"; Check: not IsUpgrade
Name: "startup"; Description: "Start with Windows"; Check: not IsUpgrade

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"; Comment: "Show your Apple Music songs on Discord"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; Same entry the app's own "Start with Windows" switch uses, so they agree.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; \
  ValueName: "{#AppId}"; ValueData: """{app}\{#AppExe}"""; Tasks: startup

[Run]
; Also runs after silent updates, so the new version comes straight back.
Filename: "{app}\{#AppExe}"; Description: "Start {#AppName} now"; Flags: nowait postinstall

[Code]
function IsUpgrade: Boolean;
begin
  Result := RegValueExists(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Uninstall\{8C3B6E2A-5D1F-4B7E-9A64-2F0D7C1E5B93}_is1', 'UninstallString');
end;

procedure StopApp;
var
  Code: Integer;
begin
  { Ask it to quit (it clears its Discord status), then make sure. }
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/IM {#AppExe}', '', SW_HIDE, ewWaitUntilTerminated, Code);
  Sleep(1500);
  Exec(ExpandConstant('{sys}\taskkill.exe'), '/F /IM {#AppExe}', '', SW_HIDE, ewWaitUntilTerminated, Code);
end;

function InitializeUninstall: Boolean;
begin
  StopApp;
  Result := True;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usPostUninstall then
  begin
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Run', '{#AppId}');
    RegDeleteValue(HKCU, 'Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run', '{#AppId}');
  end;
end;
