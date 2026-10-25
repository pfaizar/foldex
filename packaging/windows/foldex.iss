; Build only from the clean staging folder produced by package_installer.py.
; Keep AppId and SetupMutex stable across every version.
#ifndef AppVersion
  #error AppVersion is required
#endif
#ifndef SourceDir
  #error SourceDir is required
#endif
#ifndef ReleaseDir
  #error ReleaseDir is required
#endif

[Setup]
AppId=Foldex.Desktop
AppName=Foldex
AppVersion={#AppVersion}
AppPublisher=Foldex contributors
AppPublisherURL=https://github.com/pfaizar/foldex
AppSupportURL=https://github.com/pfaizar/foldex/issues
AppUpdatesURL=https://github.com/pfaizar/foldex/releases
DefaultDirName={localappdata}\Programs\Foldex
DefaultGroupName=Foldex
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
WizardStyle=modern
DisableDirPage=no
DisableProgramGroupPage=yes
AllowNoIcons=yes
AllowRootDirectory=no
AllowUNCPath=no
UsePreviousAppDir=yes
UsePreviousTasks=yes
SetupMutex=Global\Foldex.Setup
CloseApplications=no
RestartApplications=no
UninstallDisplayIcon={app}\Foldex.exe
UninstallDisplayName=Foldex
UninstallRestartComputer=no
ChangesEnvironment=yes
SetupLogging=yes
Compression=lzma2
SolidCompression=yes
OutputDir={#ReleaseDir}
OutputBaseFilename=Foldex-{#AppVersion}-windows-x64-setup
SetupIconFile=..\..\assets\brand\foldex.ico
LicenseFile=..\..\LICENSE

[Tasks]
Name: desktopicon; Description: "Create a &desktop shortcut"; Flags: unchecked
Name: addtopath; Description: "Add Foldex to my &PATH (new terminals)"; Flags: unchecked

[Files]
Source: "{#SourceDir}\Foldex.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\theme.toml"; DestDir: "{app}"; DestName: "default-theme.toml"; Flags: ignoreversion
Source: "{#SourceDir}\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\BUILD-INFO.json"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\THIRD-PARTY-NOTICES.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\licenses\*"; DestDir: "{app}\licenses"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "{#SourceDir}\foldex-installed.ini"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Foldex"; Filename: "{app}\Foldex.exe"
Name: "{autodesktop}\Foldex"; Filename: "{app}\Foldex.exe"; Tasks: desktopicon

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\Foldex.exe"; ValueType: string; ValueData: "{app}\Foldex.exe"; Flags: uninsdeletekey
; Never use uninsdeletevalue on Environment\Path: it belongs to the user.
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{code:UserPathWithApp}"; Flags: preservestringtype; Check: AddPathNeeded
Root: HKCU; Subkey: "Software\Foldex\Installer"; ValueType: string; ValueName: "OwnedPath"; ValueData: "{app}"; Flags: uninsdeletevalue uninsdeletekeyifempty; Check: AddPathNeeded

[Run]
Filename: "{app}\Foldex.exe"; Description: "Launch Foldex"; Flags: nowait postinstall skipifsilent
Filename: "{app}\Foldex.exe"; Flags: nowait; Check: RestartAfterUpdate

[Code]
const
  UninstallKey = 'Software\Microsoft\Windows\CurrentVersion\Uninstall\Foldex.Desktop_is1';
var
  ExistingDir: String;
  AddPath: Boolean;
  Transaction: THandle;

function OpenProcess(Access: LongWord; Inherit: BOOL; ProcessId: LongWord): THandle;
  external 'OpenProcess@kernel32.dll stdcall';
function WaitForSingleObject(Handle: THandle; Milliseconds: LongWord): LongWord;
  external 'WaitForSingleObject@kernel32.dll stdcall';
function CloseHandle(Handle: THandle): BOOL;
  external 'CloseHandle@kernel32.dll stdcall';
function CreateMutexW(Attributes: LongWord; InitialOwner: BOOL; Name: String): THandle;
  external 'CreateMutexW@kernel32.dll stdcall';

function AcquireTransaction: Boolean;
var Status: LongWord;
begin
  Result := True;
  if Transaction <> 0 then Exit;
  Transaction := CreateMutexW(0, False, 'Global\Foldex.InstallTransaction');
  if Transaction = 0 then begin Result := False; Exit; end;
  Status := WaitForSingleObject(Transaction, 0);
  Result := (Status = 0) or (Status = $80);
  if not Result then begin CloseHandle(Transaction); Transaction := 0; end;
end;

function CanonicalDir(Value: String): String;
var I, Code: Integer;
begin
  Result := RemoveBackslashUnlessRoot(Value);
  // Match Rust exactly, regardless of the user's Windows locale.
  for I := 1 to Length(Result) do begin
    Code := Ord(Result[I]);
    if (Code >= 65) and (Code <= 90) then Result[I] := Chr(Code + 32);
  end;
  if Copy(Result, 1, 4) = '\\?\' then Delete(Result, 1, 4);
end;

function RunningMutex(Param: String): String;
begin
  Result := 'Global\Foldex.Running.' + GetSHA256OfUnicodeString(CanonicalDir(ExpandConstant('{app}')));
end;

function IsUpdate: Boolean;
begin
  Result := ExpandConstant('{param:UPDATE|0}') = '1';
end;

function RestartAfterUpdate: Boolean;
begin
  Result := IsUpdate and (ExpandConstant('{param:RESTARTAPP|0}') = '1');
end;

function InitializeSetup: Boolean;
var
  Pid: LongWord;
  Process: THandle;
begin
  Result := False;
  RegQueryStringValue(HKCU, UninstallKey, 'InstallLocation', ExistingDir);
  if IsUpdate then begin
    if ExistingDir = '' then begin
      MsgBox('Foldex is no longer installed. Run the installer normally to install it again.', mbError, MB_OK);
      Exit;
    end;
    Pid := StrToIntDef(ExpandConstant('{param:WAITPID|0}'), 0);
    if Pid = 0 then Exit;
    Process := OpenProcess($00100000, False, Pid);
    if Process <> 0 then begin
      try
        if WaitForSingleObject(Process, 30000) <> 0 then begin
          MsgBox('Foldex did not exit in time. No files were changed. Run the installer again.', mbError, MB_OK);
          Exit;
        end;
      finally
        CloseHandle(Process);
      end;
    end else if DLLGetLastError <> 87 then begin
      MsgBox('Could not wait for Foldex to exit. No files were changed.', mbError, MB_OK);
      Exit;
    end;
  end;
  Result := True;
end;

procedure InitializeWizard;
begin
  if ExistingDir <> '' then begin
    WizardForm.DirEdit.Text := ExistingDir;
    WizardForm.DirEdit.Enabled := False;
    WizardForm.DirBrowseButton.Enabled := False;
  end;
end;

function ShouldSkipPage(PageId: Integer): Boolean;
begin
  Result := (PageId = wpSelectDir) and (ExistingDir <> '');
end;

function CompareOrdinal(A, B: String): Integer;
var I, Count: Integer;
begin
  Result := 0;
  Count := Length(A);
  if Length(B) < Count then Count := Length(B);
  for I := 1 to Count do
    if Ord(A[I]) <> Ord(B[I]) then begin
      if Ord(A[I]) > Ord(B[I]) then Result := 1 else Result := -1;
      Exit;
    end;
  if Length(A) > Length(B) then Result := 1;
  if Length(A) < Length(B) then Result := -1;
end;

function Numeric(Value: String): Boolean;
var I: Integer;
begin
  Result := Length(Value) > 0;
  for I := 1 to Length(Value) do
    if (Value[I] < '0') or (Value[I] > '9') then Result := False;
end;

function CompareNumber(A, B: String): Integer;
begin
  if Length(A) > Length(B) then Result := 1
  else if Length(A) < Length(B) then Result := -1
  else Result := CompareOrdinal(A, B);
end;

procedure SplitVersion(Value: String; var Core, Preview: TArrayOfString);
var Dash: Integer; Suffix: String;
begin
  Dash := Pos('-', Value);
  Suffix := '';
  if Dash > 0 then begin
    Suffix := Copy(Value, Dash + 1, Length(Value));
    Value := Copy(Value, 1, Dash - 1);
  end;
  Core := StringSplit(Value, ['.'], stAll);
  if Suffix = '' then SetArrayLength(Preview, 0)
  else Preview := StringSplit(Suffix, ['.'], stAll);
end;

function CompareVersion(A, B: String): Integer;
var AC, AP, BC, BP: TArrayOfString; I, Count: Integer;
begin
  SplitVersion(A, AC, AP);
  SplitVersion(B, BC, BP);
  // Fail closed for unknown installed version formats.
  Result := -1;
  if (GetArrayLength(AC) <> 3) or (GetArrayLength(BC) <> 3) then Exit;
  for I := 0 to 2 do begin
    if not Numeric(AC[I]) or not Numeric(BC[I]) then Exit;
    Result := CompareNumber(AC[I], BC[I]);
    if Result <> 0 then Exit;
  end;
  if (GetArrayLength(AP) = 0) and (GetArrayLength(BP) > 0) then begin Result := 1; Exit; end;
  if (GetArrayLength(BP) = 0) and (GetArrayLength(AP) > 0) then begin Result := -1; Exit; end;
  Count := GetArrayLength(AP);
  if GetArrayLength(BP) < Count then Count := GetArrayLength(BP);
  for I := 0 to Count - 1 do begin
    if Numeric(AP[I]) and Numeric(BP[I]) then Result := CompareNumber(AP[I], BP[I])
    else if Numeric(AP[I]) then Result := -1
    else if Numeric(BP[I]) then Result := 1
    else Result := CompareOrdinal(AP[I], BP[I]);
    if Result <> 0 then Exit;
  end;
  Result := GetArrayLength(AP) - GetArrayLength(BP);
end;

function PathContains(Value, Directory: String): Boolean;
var Parts: TArrayOfString; I: Integer; Part: String;
begin
  Result := False;
  Parts := StringSplit(Value, [';'], stAll);
  for I := 0 to GetArrayLength(Parts) - 1 do begin
    Part := Trim(Parts[I]);
    if (Copy(Part, 1, 1) = '"') and (Copy(Part, Length(Part), 1) = '"') then
      Part := Copy(Part, 2, Length(Part) - 2);
    if CanonicalDir(Part) = CanonicalDir(Directory) then begin Result := True; Exit; end;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var InstalledVersion, UserPath: String;
begin
  Result := '';
  if not AcquireTransaction then begin
    Result := 'Another Foldex installation is changing files. Close it and try again.';
    Exit;
  end;
  // Re-read at the point of mutation, including when another setup just exited.
  RegQueryStringValue(HKCU, UninstallKey, 'InstallLocation', ExistingDir);
  if (ExistingDir <> '') and (CanonicalDir(WizardDirValue) <> CanonicalDir(ExistingDir)) then
    Result := 'Foldex is already installed elsewhere. Uninstall that copy before changing its installation directory.'
  else if (ExistingDir <> '') and not RegQueryStringValue(HKCU, UninstallKey, 'DisplayVersion', InstalledVersion) then
    Result := 'The installed Foldex version could not be read. Repair its registration or uninstall it before continuing.'
  else if (ExistingDir <> '') and
      (CompareVersion('{#AppVersion}', InstalledVersion) < 0) then
    Result := 'A newer Foldex version is already installed. Downgrades are blocked to protect your settings.'
  else if CheckForMutexes(RunningMutex('')) then
    Result := 'Exit all Foldex windows and its system tray instance, then try again. Setup will not interrupt file operations.'
  else if (ExistingDir = '') and FileExists(ExpandConstant('{app}\Foldex.exe')) then
    Result := 'This folder contains a portable copy of Foldex. Choose a new installation folder to preserve its settings.'
  else if Pos(';', WizardDirValue) > 0 then
    Result := 'Choose an installation path without a semicolon.';
  RegQueryStringValue(HKCU, 'Environment', 'Path', UserPath);
  AddPath := WizardIsTaskSelected('addtopath') and not PathContains(UserPath, WizardDirValue);
  if AddPath and (Length(UserPath) + Length(WizardDirValue) + 1 > 32760) then
    Result := 'Your user PATH is too long. Clear the Add to PATH option and try again.';
end;

function AddPathNeeded: Boolean;
begin
  Result := AddPath;
end;

function UserPathWithApp(Param: String): String;
begin
  RegQueryStringValue(HKCU, 'Environment', 'Path', Result);
  // Preserve an existing trailing empty segment, too, so uninstall can restore
  // the original PATH exactly after removing only the entry we append.
  if Result <> '' then Result := Result + ';';
  Result := Result + ExpandConstant('{app}');
end;

function InitializeUninstall: Boolean;
begin
  Result := not CheckForMutexes('Global\Foldex.Setup') and AcquireTransaction;
  if not Result then
    MsgBox('Another Foldex installer is running. Close it before uninstalling.', mbError, MB_OK)
  else CreateMutex('Global\Foldex.Setup');
end;

procedure CurUninstallStepChanged(Step: TUninstallStep);
var Code: Integer;
begin
  if Step = usAppMutexCheck then begin
    // The install directory is now final. A dynamic [Setup] AppMutex would be
    // expanded before directory selection and log the wrong path on first install.
    while CheckForMutexes(RunningMutex('')) do
      if SuppressibleMsgBox('Exit all Foldex windows and its system tray instance before uninstalling.',
          mbError, MB_OKCANCEL, IDCANCEL) <> IDOK then Abort;
  end;
  if Step = usUninstall then begin
    // Runs after the running-app check and confirmation, before removing files.
    if not Exec(ExpandConstant('{app}\Foldex.exe'), '--uninstall-integration',
        ExpandConstant('{app}'), SW_HIDE, ewWaitUntilTerminated, Code) or (Code <> 0) then
      RaiseException('Windows integration could not be restored. Foldex was not removed. Try uninstalling again.');
  end;
end;
