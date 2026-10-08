; Voice Changer installer (Inno Setup 6). Built by the release workflow:
;   iscc /DAppVersion=0.1.0 installer\voicechanger.iss   -> target\installer\VoiceChanger-Setup-0.1.0.exe
; Per-user install: no administrator prompt, installs to %LOCALAPPDATA%\Programs\Voice Changer.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#define AppName "Voice Changer"
#define AppExe "voicechanger.exe"
#define RepoUrl "https://github.com/FlameDevil1/VoiceChangerProject"
; Must match src/autostart.rs and src/single_instance.rs.
#define RunKey "Software\Microsoft\Windows\CurrentVersion\Run"
#define ApprovedKey "Software\Microsoft\Windows\CurrentVersion\Explorer\StartupApproved\Run"

[Setup]
AppId={{E56CA3C2-52E7-4F3D-953D-4F6835C647EE}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=FlameDevil1
AppPublisherURL={#RepoUrl}
AppSupportURL={#RepoUrl}/issues
AppUpdatesURL={#RepoUrl}/releases
DefaultDirName={autopf}\{#AppName}
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
; The app is closed by [Code] below (it lives in the tray, so the usual "close it" prompt confuses).
CloseApplications=no
SetupIconFile=..\assets\voicechanger.ico
UninstallDisplayIcon={app}\{#AppExe}
UninstallDisplayName={#AppName}
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
OutputDir=..\target\installer
OutputBaseFilename=VoiceChanger-Setup-{#AppVersion}

[Messages]
FinishedLabel=Voice Changer is installed.%n%nOther apps hear the changed voice through the free VB-CABLE virtual microphone. If it isn't installed yet, Voice Changer shows you how to set it up.

[Tasks]
Name: "startup"; Description: "Start Voice Changer when I sign in to Windows (hidden in the tray)"; Flags: unchecked
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "..\target\release\{#AppExe}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; DestName: "LICENSE.txt"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; The same entry the app's "Start with Windows" checkbox writes.
Root: HKCU; Subkey: "{#RunKey}"; ValueType: string; ValueName: "{#AppName}"; ValueData: """{app}\{#AppExe}"" --startup"; Tasks: startup

[Run]
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[Code]
const
  AppMutex = 'Local\VoiceChanger.SingleInstance';
  QuitEvent = 'Local\VoiceChanger.Quit';
  EVENT_MODIFY_STATE = $0002;

function OpenEvent(Access: DWORD; Inherit: BOOL; Name: string): THandle;
  external 'OpenEventW@kernel32.dll stdcall';
function SetEvent(Handle: THandle): BOOL;
  external 'SetEvent@kernel32.dll stdcall';
function CloseHandle(Handle: THandle): BOOL;
  external 'CloseHandle@kernel32.dll stdcall';

{ Ask a running Voice Changer to quit (it saves its settings first) and wait for it. }
function CloseRunningApp(Silent: Boolean): Boolean;
var
  Ev: THandle;
  Waited: Integer;
begin
  Result := True;
  while CheckForMutexes(AppMutex) do
  begin
    if not Silent then
      if MsgBox('Voice Changer is running. Close it and continue?', mbConfirmation, MB_OKCANCEL) <> IDOK then
      begin
        Result := False;
        Exit;
      end;
    Ev := OpenEvent(EVENT_MODIFY_STATE, False, QuitEvent);
    if Ev <> 0 then
    begin
      SetEvent(Ev);
      CloseHandle(Ev);
    end;
    Waited := 0;
    while CheckForMutexes(AppMutex) and (Waited < 5000) do
    begin
      Sleep(100);
      Waited := Waited + 100;
    end;
    if CheckForMutexes(AppMutex) then
    begin
      if Silent then
      begin
        Result := False;
        Exit;
      end;
      MsgBox('Voice Changer is still running. Quit it from its tray icon (right-click, Quit), then try again.', mbError, MB_OK);
    end;
  end;
end;

function InitializeSetup(): Boolean;
begin
  Result := CloseRunningApp(WizardSilent);
end;

function InitializeUninstall(): Boolean;
begin
  Result := CloseRunningApp(UninstallSilent);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Settings: string;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    { The app may have created the startup entry itself; remove it either way. }
    RegDeleteValue(HKCU, '{#RunKey}', '{#AppName}');
    RegDeleteValue(HKCU, '{#ApprovedKey}', '{#AppName}');
    Settings := ExpandConstant('{userappdata}\VoiceChanger');
    if DirExists(Settings) and not UninstallSilent then
      if MsgBox('Also delete your Voice Changer settings and saved voices?' + #13#10 +
                '(Recordings in your Music folder are kept.)', mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
        DelTree(Settings, True, True, True);
  end;
end;
