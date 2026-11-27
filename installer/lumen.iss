; Lumen — Modern High-Speed File Workbench for Windows 11
; Inno Setup 6 Installer Script
;
; Build via:
;   powershell -ExecutionPolicy Bypass -File installer\build.ps1
; Or directly:
;   ISCC.exe /DMyAppVersion=0.2.0 installer\lumen.iss

#ifndef MyAppVersion
  #define MyAppVersion "0.2.0"
#endif

#define MyAppName "Lumen"
#define MyAppPublisher "Lumen"
#define MyAppURL "https://github.com/lxfight/LumenExplorer"
#define MyAppExeName "lumen.exe"
#define MyAppMutex "LumenExplorer.SingleInstance.v1"

[Setup]
; Fixed GUID ensures smooth in-place upgrades without duplicate Apps & Features entries.
AppId={{9B4E2F6C-7A1D-4C8E-B3F2-5D8A1E6C4B90}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher={#MyAppPublisher}
AppPublisherURL={#MyAppURL}
AppSupportURL={#MyAppURL}/issues
AppUpdatesURL={#MyAppURL}/releases
VersionInfoVersion={#MyAppVersion}.0
VersionInfoDescription=Lumen — Modern File Workbench Installer
VersionInfoProductName={#MyAppName}

; Per-user install by default (VS Code UX, no UAC required), with optional per-machine install.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
DefaultDirName={autopf}\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
LicenseFile=..\LICENSE

; Output & compression
OutputDir=..\dist
OutputBaseFilename=LumenSetup-{#MyAppVersion}-x64
Compression=lzma2/ultra64
SolidCompression=yes
LZMAUseSeparateProcess=yes

; Modern Windows 11 look & HiDPI wizard art
WizardStyle=modern
SetupIconFile=..\assets\lumen.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
UninstallDisplayName={#MyAppName}
WizardImageFile=assets\wizard-side.bmp
WizardSmallImageFile=assets\wizard-small.bmp
WizardImageStretch=yes

; Platform constraints (64-bit Windows 10 2004+ / Windows 11)
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041

; Clean lifecycle management during upgrades & uninstalls
AppMutex={#MyAppMutex}
CloseApplications=yes
RestartApplications=no
ChangesEnvironment=yes
ShowLanguageDialog=auto

[Languages]
; Layer ChineseSimplified.isl on top of Default.isl so every Inno Setup 6.x built-in message stays covered.
Name: "chinesesimplified"; MessagesFile: "compiler:Default.isl,languages\ChineseSimplified.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[CustomMessages]
chinesesimplified.OpenWithLumen=使用 Lumen 打开
english.OpenWithLumen=Open with Lumen

chinesesimplified.TaskGroupShortcuts=快捷方式：
english.TaskGroupShortcuts=Shortcuts:

chinesesimplified.TaskGroupIntegration=桌面集成（不修改默认 Windows 资源管理器，卸载时完全清理）：
english.TaskGroupIntegration=Desktop integration (never alters default File Explorer; cleanly removed on uninstall):

chinesesimplified.TaskDesktopIcon=创建桌面快捷方式
english.TaskDesktopIcon=Create a desktop shortcut

chinesesimplified.TaskContextMenu=添加文件夹与空白处右键菜单「使用 Lumen 打开」
english.TaskContextMenu=Add "Open with Lumen" to folder and directory background context menus

chinesesimplified.TaskAddToPath=添加到 PATH 环境变量（可在终端直接运行 lumen .）
english.TaskAddToPath=Add Lumen to PATH (launch from terminal with "lumen .")

chinesesimplified.TaskAutoStart=开机静默启动到系统托盘（--background）
english.TaskAutoStart=Start minimized to system tray on sign-in (--background)

chinesesimplified.LaunchLumen=立即启动 Lumen
english.LaunchLumen=Launch Lumen

chinesesimplified.RemoveUserDataPrompt=是否同时删除 Lumen 的用户配置文件（%APPDATA%\Lumen）？%n%n如果您打算稍后重新安装，建议点击「否」保留标签页与偏好设置。
english.RemoveUserDataPrompt=Do you also want to remove Lumen user settings (%APPDATA%\Lumen)?%n%nChoose No if you plan to reinstall later and want to keep your tabs and preferences.

[Tasks]
Name: "desktopicon"; Description: "{cm:TaskDesktopIcon}"; GroupDescription: "{cm:TaskGroupShortcuts}"
Name: "contextmenu"; Description: "{cm:TaskContextMenu}"; GroupDescription: "{cm:TaskGroupIntegration}"
Name: "addtopath"; Description: "{cm:TaskAddToPath}"; GroupDescription: "{cm:TaskGroupIntegration}"
Name: "autostart"; Description: "{cm:TaskAutoStart}"; GroupDescription: "{cm:TaskGroupIntegration}"; Flags: unchecked

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; AppUserModelID: "Lumen.Explorer"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"; Tasks: desktopicon; AppUserModelID: "Lumen.Explorer"

[Registry]
; Register App Paths so Win+R -> "lumen" works immediately (cleanly removed on uninstall).
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\{#MyAppExeName}"; ValueType: string; ValueName: ""; ValueData: "{app}\{#MyAppExeName}"; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\App Paths\{#MyAppExeName}"; ValueType: string; ValueName: "Path"; ValueData: "{app}"

; Optional "Open with Lumen" context menus (never overrides default Folder\shell\open).
Root: HKA; Subkey: "Software\Classes\Directory\shell\Lumen"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWithLumen}"; Tasks: contextmenu; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Directory\shell\Lumen"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\{#MyAppExeName}"",0"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\shell\Lumen\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" ""%1"""; Tasks: contextmenu

Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Lumen"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWithLumen}"; Tasks: contextmenu; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Lumen"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\{#MyAppExeName}"",0"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Directory\Background\shell\Lumen\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" ""%V"""; Tasks: contextmenu

Root: HKA; Subkey: "Software\Classes\Drive\shell\Lumen"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWithLumen}"; Tasks: contextmenu; Flags: uninsdeletekey
Root: HKA; Subkey: "Software\Classes\Drive\shell\Lumen"; ValueType: string; ValueName: "Icon"; ValueData: """{app}\{#MyAppExeName}"",0"; Tasks: contextmenu
Root: HKA; Subkey: "Software\Classes\Drive\shell\Lumen\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#MyAppExeName}"" ""%1"""; Tasks: contextmenu

; Optional start-minimized-to-tray auto-start (cleanly removed on uninstall).
Root: HKA; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Lumen"; ValueData: """{app}\{#MyAppExeName}"" --background"; Tasks: autostart; Flags: uninsdeletevalue

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "{cm:LaunchLumen}"; Flags: nowait postinstall skipifsilent
Filename: "{app}\{#MyAppExeName}"; Flags: nowait postinstall skipifnotsilent; Check: IsAutoRelaunch

[Code]
const
  LumenMutex = '{#MyAppMutex}';

function IsAutoRelaunch(): Boolean;
var
  I: Integer;
begin
  Result := False;
  for I := 1 to ParamCount do
  begin
    if CompareText(ParamStr(I), '/RELAUNCH') = 0 then
    begin
      Result := True;
      Exit;
    end;
  end;
end;

procedure GracefulQuitLumen(const ExePath: string);
var
  ResultCode: Integer;
  Attempts: Integer;
begin
  if not CheckForMutexes(LumenMutex) then
    Exit;

  if FileExists(ExePath) then
  begin
    Exec(ExePath, '--quit', '', SW_HIDE, ewWaitUntilTerminated, ResultCode);
    Attempts := 0;
    while CheckForMutexes(LumenMutex) and (Attempts < 40) do
    begin
      Sleep(100);
      Attempts := Attempts + 1;
    end;
  end;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
begin
  GracefulQuitLumen(ExpandConstant('{app}\{#MyAppExeName}'));
  Result := '';
end;

function InitializeUninstall(): Boolean;
begin
  GracefulQuitLumen(ExpandConstant('{app}\{#MyAppExeName}'));
  Result := True;
end;

function NormalizeDir(const S: string): string;
begin
  Result := Trim(S);
  while (Length(Result) > 1) and (Result[Length(Result)] = '\') do
    Delete(Result, Length(Result), 1);
  Result := Lowercase(Result);
end;

procedure GetEnvPathKey(out RootKey: Integer; out SubKey: string);
begin
  if IsAdminInstallMode then
  begin
    RootKey := HKEY_LOCAL_MACHINE;
    SubKey := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment';
  end
  else
  begin
    RootKey := HKEY_CURRENT_USER;
    SubKey := 'Environment';
  end;
end;

procedure UpdatePathEnv(const Dir: string; const AddEntry: Boolean);
var
  RootKey: Integer;
  SubKey, OrigPath, Remaining, Entry, NewPath, TargetNorm: string;
  SepPos: Integer;
  Found: Boolean;
begin
  GetEnvPathKey(RootKey, SubKey);
  if not RegQueryStringValue(RootKey, SubKey, 'Path', OrigPath) then
    OrigPath := '';

  TargetNorm := NormalizeDir(Dir);
  Remaining := OrigPath;
  NewPath := '';
  Found := False;

  while Remaining <> '' do
  begin
    SepPos := Pos(';', Remaining);
    if SepPos > 0 then
    begin
      Entry := Copy(Remaining, 1, SepPos - 1);
      Delete(Remaining, 1, SepPos);
    end
    else
    begin
      Entry := Remaining;
      Remaining := '';
    end;

    if Trim(Entry) = '' then
      Continue;

    if NormalizeDir(Entry) = TargetNorm then
    begin
      Found := True;
      if not AddEntry then
        Continue;
    end;

    if NewPath <> '' then
      NewPath := NewPath + ';';
    NewPath := NewPath + Entry;
  end;

  if AddEntry and (not Found) then
  begin
    if NewPath <> '' then
      NewPath := NewPath + ';';
    NewPath := NewPath + Dir;
  end;

  if NewPath <> OrigPath then
    RegWriteExpandStringValue(RootKey, SubKey, 'Path', NewPath);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then
  begin
    if WizardIsTaskSelected('addtopath') then
      UpdatePathEnv(ExpandConstant('{app}'), True)
    else
      UpdatePathEnv(ExpandConstant('{app}'), False);
  end;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  DataDir: string;
begin
  if CurUninstallStep = usPostUninstall then
  begin
    UpdatePathEnv(ExpandConstant('{app}'), False);
    DataDir := ExpandConstant('{userappdata}\Lumen');
    if (not UninstallSilent) and DirExists(DataDir) then
    begin
      if MsgBox(CustomMessage('RemoveUserDataPrompt'), mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES then
        DelTree(DataDir, True, True, True);
    end;
  end;
end;
