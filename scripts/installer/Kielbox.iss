; Kielbox's Windows installer. Application data is deliberately never installed,
; removed, or recursively cleaned by Setup or its uninstaller.
#ifndef PayloadDir
  #error PayloadDir is required
#endif

[Setup]
AppId={#InstallAppId}
AppName={#InstallAppName}
AppVersion={#AppVersion}
AppVerName={#InstallAppName} {#AppVersion}
AppPublisher=Kieler
AppPublisherURL=https://kieler.dev
AppSupportURL=https://github.com/kielerdotdev/kielbox/issues
AppUpdatesURL=https://github.com/kielerdotdev/kielbox/releases
DefaultDirName={localappdata}\Programs\{#InstallAppName}
DefaultGroupName={#InstallAppName}
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
WizardStyle=modern dynamic
WizardImageFile={#BrandDir}\wizard.bmp
WizardSmallImageFile={#BrandDir}\wizard-small.bmp
SetupIconFile={#BrandDir}\kielbox.ico
UninstallDisplayIcon={app}\Kielbox.exe
DisableWelcomePage=no
DisableDirPage=no
DisableProgramGroupPage=yes
DisableReadyPage=no
UsePreviousTasks=yes
CloseApplications=no
RestartApplications=no
SetupMutex={#InstallAppId}.Setup
Compression=lzma2/normal
SolidCompression=yes
OutputDir={#ReleaseDir}
OutputBaseFilename=Kielbox-Setup
VersionInfoVersion={#NumericVersion}
VersionInfoProductName=Kielbox
VersionInfoProductVersion={#NumericVersion}
VersionInfoProductTextVersion={#AppVersion}
VersionInfoDescription=Kielbox Setup
LicenseFile={#PayloadDir}\LICENSE-GPL-3.0
Uninstallable=yes

[Languages]
Name: english; MessagesFile: compiler:Default.isl

[Tasks]
Name: desktopicon; Description: Create a desktop shortcut; GroupDescription: Shortcuts:; Flags: unchecked
Name: autostart; Description: Start Kielbox when I sign in to Windows; GroupDescription: Startup:; Flags: unchecked

[Files]
Source: "{#PayloadDir}\*"; DestDir: "{app}"; Excludes: "kielbox-install.json"; Flags: ignoreversion recursesubdirs createallsubdirs
; The target marker authorizes startup only after every application file was copied.
Source: "{#PayloadDir}\kielbox-install.json"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#InstallAppName}"; Filename: "{app}\Kielbox.exe"; WorkingDir: "{app}"; AppUserModelID: "{#InstallAppId}"
Name: "{autodesktop}\{#InstallAppName}"; Filename: "{app}\Kielbox.exe"; WorkingDir: "{app}"; AppUserModelID: "{#InstallAppId}"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#InstallAppId}"; ValueData: """{app}\Kielbox.exe"""; Tasks: autostart; Flags: uninsdeletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueName: "{#InstallAppId}"; Tasks: not autostart; Flags: deletevalue

[InstallDelete]
Type: files; Name: "{autodesktop}\{#InstallAppName}.lnk"; Tasks: not desktopicon

[Run]
Filename: "{app}\Kielbox.exe"; Description: Launch Kielbox; WorkingDir: "{app}"; Flags: nowait postinstall skipifsilent; Check: AllowManualLaunch

[Code]
const
  SynchronizeAccess = $00100000;
  WaitSignaled = 0;
  WaitTimeout = 258;
  InvalidProcessId = 87;

function OpenProcess(Access: LongWord; InheritHandle: Boolean; ProcessId: LongWord): THandle;
  external 'OpenProcess@kernel32.dll stdcall';
function WaitForSingleObject(Handle: THandle; Milliseconds: LongWord): LongWord;
  external 'WaitForSingleObject@kernel32.dll stdcall';
function CloseHandle(Handle: THandle): Boolean;
  external 'CloseHandle@kernel32.dll stdcall';
function GetCurrentProcessId: LongWord;
  external 'GetCurrentProcessId@kernel32.dll stdcall';
type
  TProcessIds = array[0..4095] of LongWord;
function EnumProcesses(var Ids: TProcessIds; Bytes: LongWord; var Required: LongWord): Boolean;
  external 'K32EnumProcesses@kernel32.dll stdcall';
function QueryFullProcessImageName(Handle: THandle; Flags: LongWord; Name: String; var Characters: LongWord): Boolean;
  external 'QueryFullProcessImageNameW@kernel32.dll stdcall';

function TargetDirectoryIsIdle: Boolean;
var
  Ids: TProcessIds;
  Required, I, Characters: LongWord;
  Process: THandle;
  Name, Target, OwnSource: String;
begin
  Result := False;
  if not EnumProcesses(Ids, SizeOf(Ids), Required) or (Required = 0) or (Required >= SizeOf(Ids)) then Exit;
  Target := AddBackslash(ExpandConstant('{app}'));
  if IsUninstaller then
    OwnSource := ExpandConstant('{uninstallexe}')
  else
    OwnSource := ExpandConstant('{srcexe}');
  for I := 0 to (Required div 4) - 1 do begin
    Process := 0;
    if Ids[I] <> GetCurrentProcessId then
      Process := OpenProcess($1000, False, Ids[I]);
    if Process <> 0 then begin
      try
        Characters := 32768;
        SetLength(Name, Characters);
        if QueryFullProcessImageName(Process, 0, Name, Characters) then begin
          SetLength(Name, Characters);
          // Inno retains its original launcher while its temporary copy works.
          // Exclude only that exact source executable, not other application tools.
          if (CompareText(Name, OwnSource) <> 0) and
            (CompareText(Copy(Name, 1, Length(Target)), Target) = 0) then begin
            Log('Application directory is in use by process ' + IntToStr(Ids[I]) + ': ' + Name);
            Exit;
          end;
        end;
      finally
        CloseHandle(Process);
      end;
    end;
  end;
  Result := True;
end;

function HasSwitch(Name: String): Boolean;
var I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), '/' + Name) = 0 then begin
      Result := True;
      Exit;
    end;
end;

function AllowManualLaunch: Boolean;
begin
  Result := not HasSwitch('NOAUTORUN');
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  ParentText: String;
  ParentId: Integer;
  Parent: THandle;
  WaitResult: LongWord;
  ErrorCode: LongWord;
begin
  Result := '';
  ParentText := ExpandConstant('{param:WAITFORPID|}');
  if ParentText <> '' then begin
    ParentId := StrToIntDef(ParentText, 0);
    if ParentId <= 0 then begin
    Result := 'Invalid Kielbox update process identifier. Reopen Kielbox and retry the update.';
    Exit;
    end;
    Parent := OpenProcess(SynchronizeAccess, False, ParentId);
    if Parent = 0 then begin
    ErrorCode := DLLGetLastError;
    if ErrorCode <> InvalidProcessId then
      Result := 'Setup could not verify that Kielbox has finished closing. No application files were changed. Close Kielbox and retry.';
      if Result <> '' then Exit;
    end else try
    Log('Waiting up to 60 seconds for Kielbox process ' + ParentText + ' to finish; Setup never terminates it.');
    WaitResult := WaitForSingleObject(Parent, 60000);
    if WaitResult <> WaitSignaled then
      Result := 'Kielbox is still closing or its status could not be checked. No application files were changed. Wait for recording and saves to finish, then retry the update.';
    finally
      CloseHandle(Parent);
    end;
    if Result <> '' then Exit;
  end;
  if not TargetDirectoryIsIdle then
    Result := 'Kielbox or one of its tools is still running from this installation folder. Quit Kielbox from the tray and close its tools, then retry. Setup will not stop an active recording or terminate any process.';
end;

function InitializeUninstall: Boolean;
begin
  Result := TargetDirectoryIsIdle;
  if not Result then
    SuppressibleMsgBox('Quit Kielbox from the tray and close its tools before uninstalling. Your recordings and settings will be preserved.', mbError, MB_OK, IDOK);
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  WorkingDirectory, Parameters: String;
  ResultCode, Tail: Integer;
begin
  if (CurStep = ssDone) and HasSwitch('RESTARTAPP') and not HasSwitch('NOAUTORUN') then begin
    WorkingDirectory := ExpandConstant('{param:WORKINGDIRECTORY|}');
    if (WorkingDirectory = '') or not DirExists(WorkingDirectory) or
      not (((Length(WorkingDirectory) >= 3) and (WorkingDirectory[2] = ':') and (WorkingDirectory[3] = '\')) or (Copy(WorkingDirectory, 1, 2) = '\\')) or
      (Pos('"', WorkingDirectory) <> 0) then begin
      Log('Restart was requested without a valid existing data directory; preserving the installation without launching.');
      Exit;
    end;
    Parameters := '--working-directory "' + WorkingDirectory;
    Tail := Length(WorkingDirectory);
    while (Tail > 0) and (WorkingDirectory[Tail] = '\') do begin
      Parameters := Parameters + '\';
      Tail := Tail - 1;
    end;
    Parameters := Parameters + '"';
    if not Exec(ExpandConstant('{app}\Kielbox.exe'), Parameters,
      ExpandConstant('{app}'), SW_SHOWNORMAL, ewNoWait, ResultCode) then
      Log('Installation succeeded, but Kielbox could not restart: ' + IntToStr(ResultCode));
  end;
end;
