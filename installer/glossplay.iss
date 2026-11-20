; Glossplay per-user installer. Build with scripts\build-installer.ps1, which passes
; SourceDir (the unpacked release folder), OutDir and AppVersion.
#ifndef SourceDir
  #error SourceDir is not set (/DSourceDir=...)
#endif
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef OutDir
  #define OutDir "."
#endif

#define AppName "Glossplay"
#define AppExe "glossplay.exe"
#define AppUrl "https://github.com/Magerko/glossplay"

; Pinned versions and SHA-256 of the optional Anki downloads (written by scripts\update-anki-pins.ps1).
#include "anki-pins.iss"

[Setup]
; Never change this GUID: it is what makes a new version install over the old one.
AppId={{89171F3D-837A-44EF-9C99-5DCA626462A3}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher=Glossplay
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}
AppUpdatesURL={#AppUrl}/releases
VersionInfoVersion={#AppVersion}
DefaultDirName={localappdata}\Programs\Glossplay
DisableProgramGroupPage=yes
DisableDirPage=no
ShowLanguageDialog=yes
UsePreviousLanguage=no
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
OutputDir={#OutDir}
OutputBaseFilename=glossplay-setup-x64
SetupIconFile=..\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExe}
Compression=lzma2/ultra
SolidCompression=yes
WizardStyle=modern
; The player has no mutex, so I rely on Restart Manager to close a running copy.
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "russian"; MessagesFile: "compiler:Languages\Russian.isl"
Name: "ukrainian"; MessagesFile: "compiler:Languages\Ukrainian.isl"
Name: "chinese"; MessagesFile: "languages\ChineseSimplified.isl"

[CustomMessages]
english.TaskShell=Add Glossplay to the context menu of video and audio files and to "Open with"
english.TaskShellGroup=Windows Explorer:
english.TaskDefault=Offer Glossplay as the default app for video and audio (Windows will ask you to confirm)
english.TaskDefaultGroup=Default apps:
english.DefaultAppHint=Windows will ask you to confirm the choice: mark Glossplay for the file types you want.
english.AskRemoveData=Also remove your settings, dictionary and downloaded components?
russian.TaskShell=Добавить Glossplay в контекстное меню видео и аудио и в «Открыть с помощью»
russian.TaskShellGroup=Проводник:
russian.TaskDefault=Предложить Glossplay как программу по умолчанию для видео и аудио (Windows попросит подтвердить)
russian.TaskDefaultGroup=Программы по умолчанию:
russian.DefaultAppHint=Windows попросит подтвердить выбор — отметьте Glossplay для нужных типов файлов.
russian.AskRemoveData=Удалить также настройки, словарь и скачанные компоненты?
ukrainian.TaskShell=Додати Glossplay до контекстного меню відео й аудіо та до «Відкрити за допомогою»
ukrainian.TaskShellGroup=Провідник:
ukrainian.TaskDefault=Запропонувати Glossplay як програму за замовчуванням для відео й аудіо (Windows попросить підтвердити)
ukrainian.TaskDefaultGroup=Програми за замовчуванням:
ukrainian.DefaultAppHint=Windows попросить підтвердити вибір — позначте Glossplay для потрібних типів файлів.
ukrainian.AskRemoveData=Видалити також налаштування, словник і завантажені компоненти?
chinese.TaskShell=将 Glossplay 添加到视频和音频文件的右键菜单以及“打开方式”
chinese.TaskShellGroup=资源管理器：
chinese.TaskDefault=将 Glossplay 提供为视频和音频的默认应用（Windows 会请您确认）
chinese.TaskDefaultGroup=默认应用：
chinese.DefaultAppHint=Windows 会请您确认选择：请为需要的文件类型选择 Glossplay。
chinese.AskRemoveData=是否同时删除设置、词典和已下载的组件？

#include "anki-messages.iss"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "shell"; Description: "{cm:TaskShell}"; GroupDescription: "{cm:TaskShellGroup}"
Name: "defaultapp"; Description: "{cm:TaskDefault}"; GroupDescription: "{cm:TaskDefaultGroup}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\glossplay.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\libmpv-2.dll"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\ffmpeg\ffmpeg.exe"; DestDir: "{app}\ffmpeg"; Flags: ignoreversion
Source: "{#SourceDir}\LICENSE";DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\THIRD-PARTY*.txt"; DestDir: "{app}"; Flags: ignoreversion
Source: "{#SourceDir}\README*.md"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#AppName}"; Filename: "{app}\{#AppExe}"
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExe}"; Tasks: desktopicon

[Registry]
; Per-extension entries (Capabilities\FileAssociations, OpenWithProgids) are written from
; the [Code] section, which has the extension list.
Root: HKCU; Subkey: "Software\Classes\Glossplay.Media"; ValueType: string; ValueName: ""; ValueData: "Glossplay"; Flags: uninsdeletekey; Tasks: defaultapp
Root: HKCU; Subkey: "Software\Classes\Glossplay.Media"; ValueType: string; ValueName: "FriendlyTypeName"; ValueData: "Glossplay"; Tasks: defaultapp
Root: HKCU; Subkey: "Software\Classes\Glossplay.Media\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"",0"; Tasks: defaultapp
Root: HKCU; Subkey: "Software\Classes\Glossplay.Media\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\{#AppExe}"" ""%1"""; Tasks: defaultapp
Root: HKCU; Subkey: "Software\Glossplay\Capabilities"; ValueType: string; ValueName: "ApplicationName"; ValueData: "Glossplay"; Flags: uninsdeletekey; Tasks: defaultapp
Root: HKCU; Subkey: "Software\Glossplay\Capabilities"; ValueType: string; ValueName: "ApplicationDescription"; ValueData: "Glossplay video player"; Tasks: defaultapp
Root: HKCU; Subkey: "Software\Glossplay"; Flags: uninsdeletekeyifempty; Tasks: defaultapp
Root: HKCU; Subkey: "Software\RegisteredApplications"; ValueType: string; ValueName: "Glossplay"; ValueData: "Software\Glossplay\Capabilities"; Flags: uninsdeletevalue; Tasks: defaultapp

[Run]
; The player writes its own HKCU entries, so the installer and the Settings checkbox
; cannot drift apart.
Filename: "{app}\{#AppExe}"; Parameters: "--shell-register"; Flags: runhidden waituntilterminated; Tasks: shell
Filename: "{app}\{#AppExe}"; Description: "{cm:LaunchProgram,{#AppName}}"; Flags: nowait postinstall skipifsilent

[Code]
const
  // Same list as the player's own shell integration (shell.rs).
  Extensions = 'mkv,mp4,m4v,webm,avi,mov,wmv,flv,ts,m2ts,mts,mpg,mpeg,ogv,3gp,mp3,flac,m4a,aac,ogg,opus,wav,wma';
  ProgId = 'Glossplay.Media';

var
  OpenDefaultApps: Boolean;
  HintAdded: Boolean;

#include "anki-code.pas"

var
  AnkiPage: TWizardPage;
  AnkiText: TNewMemo;
  AnkiStatus: TNewStaticText;
  AnkiCheck, AddonCheck: TNewCheckBox;
  AnkiPresent, AddonPresent: Boolean;
  DownloadPage: TDownloadWizardPage;
  // Paths of verified downloads; empty when the part was not chosen or did not download.
  AnkiMsi, AddonZip: String;
  AnkiNow, AddonNow: Boolean;

function NextExtension(var List: String): String;
var
  P: Integer;
begin
  P := Pos(',', List);
  if P = 0 then
  begin
    Result := List;
    List := '';
  end
  else
  begin
    Result := Copy(List, 1, P - 1);
    Delete(List, 1, P);
  end;
end;

function CommandOf(const SubKey: String): String;
begin
  if not RegQueryStringValue(HKCU, 'Software\Classes\' + SubKey, '', Result) then
    Result := '';
end;

procedure RegisterDefaultAppExtensions;
var
  List, Ext, Cur: String;
begin
  List := Extensions;
  while List <> '' do
  begin
    Ext := NextExtension(List);
    RegWriteStringValue(HKCU, 'Software\Glossplay\Capabilities\FileAssociations', '.' + Ext, ProgId);
    RegWriteStringValue(HKCU, 'Software\Classes\.' + Ext + '\OpenWithProgids', ProgId, '');
    // HKCR is the merged HKCU+HKLM view; I only fill an extension nobody handles, so I
    // never take over from another player.
    if (not RegQueryStringValue(HKCR, '.' + Ext, '', Cur)) or (Cur = '') then
      RegWriteStringValue(HKCU, 'Software\Classes\.' + Ext, '', ProgId);
  end;
end;

procedure UnregisterDefaultAppExtensions;
var
  List, Ext, Cur, Key: String;
begin
  List := Extensions;
  while List <> '' do
  begin
    Ext := NextExtension(List);
    Key := 'Software\Classes\.' + Ext;
    RegDeleteValue(HKCU, Key + '\OpenWithProgids', ProgId);
    RegDeleteKeyIfEmpty(HKCU, Key + '\OpenWithProgids');
    if RegQueryStringValue(HKCU, Key, '', Cur) and (Cur = ProgId) then
      RegDeleteValue(HKCU, Key, '');
    RegDeleteKeyIfEmpty(HKCU, Key);
  end;
end;

// Seeds the interface language only when there is no config yet; an existing one is never touched.
procedure SeedConfig;
var
  Lang, Dir: String;
begin
  Lang := '';
  if ActiveLanguage = 'english' then Lang := 'en'
  else if ActiveLanguage = 'russian' then Lang := 'ru'
  else if ActiveLanguage = 'ukrainian' then Lang := 'uk'
  else if ActiveLanguage = 'chinese' then Lang := 'zh';
  if Lang = '' then Exit;
  Dir := ExpandConstant('{userappdata}\Glossplay\data');
  if FileExists(Dir + '\config.toml') then Exit;
  if ForceDirectories(Dir) then
    SaveStringToFile(Dir + '\config.toml', '[general]' + #13#10 + 'ui_lang = "' + Lang + '"' + #13#10, False);
end;

function OnDownloadProgress(const Url, FileName: String; const Progress, ProgressMax: Int64): Boolean;
begin
  Result := True;
end;

procedure CreateAnkiPage;
var
  Y: Integer;
  Info, Status: String;
begin
  AnkiPresent := AnkiAppFound;
  AddonPresent := DirExists(AddonDir(AnkiBaseDir));

  AnkiPage := CreateCustomPage(wpSelectTasks, CustomMessage('AnkiPageTitle'), CustomMessage('AnkiPageSub'));

  Info := CustomMessage('AnkiAbout') + #13#10#13#10;
  if AnkiPresent then
    Info := Info + CustomMessage('AnkiDownloadAddon')
  else
    Info := Info + FmtMessage(CustomMessage('AnkiDownloadBoth'), ['{#AnkiSizeMB}']);

  AnkiText := TNewMemo.Create(AnkiPage);
  AnkiText.Parent := AnkiPage.Surface;
  AnkiText.Left := 0;
  AnkiText.Top := 0;
  AnkiText.Width := AnkiPage.SurfaceWidth;
  AnkiText.Height := AnkiPage.SurfaceHeight - ScaleY(76);
  AnkiText.ReadOnly := True;
  AnkiText.ScrollBars := ssVertical;
  AnkiText.WordWrap := True;
  AnkiText.Text := Info;

  Y := AnkiText.Top + AnkiText.Height + ScaleY(8);
  Status := '';
  if AnkiPresent then Status := CustomMessage('AnkiAlready')
  else if AddonPresent then Status := CustomMessage('AddonAlready');
  if Status <> '' then
  begin
    AnkiStatus := TNewStaticText.Create(AnkiPage);
    AnkiStatus.Parent := AnkiPage.Surface;
    AnkiStatus.Left := 0;
    AnkiStatus.Top := Y;
    AnkiStatus.Width := AnkiPage.SurfaceWidth;
    AnkiStatus.Caption := Status;
    Y := Y + ScaleY(20);
  end;

  if not AnkiPresent then
  begin
    AnkiCheck := TNewCheckBox.Create(AnkiPage);
    AnkiCheck.Parent := AnkiPage.Surface;
    AnkiCheck.Left := 0;
    AnkiCheck.Top := Y;
    AnkiCheck.Width := AnkiPage.SurfaceWidth;
    AnkiCheck.Caption := FmtMessage(CustomMessage('AnkiCheckInstall'), ['{#AnkiSizeMB}']);
    AnkiCheck.Checked := False;
    Y := Y + ScaleY(22);
  end;
  if not AddonPresent then
  begin
    AddonCheck := TNewCheckBox.Create(AnkiPage);
    AddonCheck.Parent := AnkiPage.Surface;
    AddonCheck.Left := 0;
    AddonCheck.Top := Y;
    AddonCheck.Width := AnkiPage.SurfaceWidth;
    AddonCheck.Caption := CustomMessage('AnkiCheckAddon');
    AddonCheck.Checked := True;
  end;
end;

procedure InitializeWizard;
begin
  CreateAnkiPage;
  DownloadPage := CreateDownloadPage(CustomMessage('AnkiDlTitle'), CustomMessage('AnkiDlDesc'), @OnDownloadProgress);
  DownloadPage.ShowBaseNameInsteadOfUrl := True;
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := (AnkiPage <> nil) and (PageID = AnkiPage.ID) and AnkiPresent and AddonPresent;
end;

function WantAnki: Boolean;
begin
  Result := (AnkiCheck <> nil) and AnkiCheck.Checked and (not AnkiPresent);
end;

function WantAddon: Boolean;
begin
  Result := (AddonCheck <> nil) and AddonCheck.Checked and (not AddonPresent);
end;

// Downloads one file into {tmp} and checks its SHA-256. A failure is reported and returns
// False; Cancelled tells apart a click on Cancel from a real error.
function FetchOne(const Url, FileName, Sha256: String; var Cancelled: Boolean): Boolean;
begin
  Result := False;
  DownloadPage.Clear;
  DownloadPage.Add(Url, FileName, Sha256);
  try
    DownloadPage.Download;
    Result := True;
  except
    if DownloadPage.AbortedByUser then
      Cancelled := True
    else
      MsgBox(FmtMessage(CustomMessage('AnkiDlFailed'), [FileName, GetExceptionMessage]), mbError, MB_OK);
  end;
end;

// Runs when the user presses Install. A failed or cancelled download only drops that part;
// the player itself is always installed.
function DownloadChosenParts: Boolean;
var
  Cancelled: Boolean;
begin
  Result := True;
  Cancelled := False;
  if (not WantAnki) and (not WantAddon) then Exit;

  DownloadPage.Show;
  try
    if WantAnki and (AnkiMsi = '') then
      if FetchOne('{#AnkiUrl}', '{#AnkiFileName}', '{#AnkiSha256}', Cancelled) then
        AnkiMsi := ExpandConstant('{tmp}\{#AnkiFileName}');
    if WantAddon and (AddonZip = '') and (not Cancelled) then
      if FetchOne('{#AnkiConnectUrl}', '{#AnkiConnectFileName}', '{#AnkiConnectSha256}', Cancelled) then
        AddonZip := ExpandConstant('{tmp}\{#AnkiConnectFileName}');
  finally
    DownloadPage.Hide;
  end;

  if Cancelled then
    Result := MsgBox(CustomMessage('AnkiDlCancelAsk'), mbConfirmation, MB_YESNO) = IDYES;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
begin
  Result := True;
  if CurPageID = wpReady then
    Result := DownloadChosenParts;
end;

// The add-on goes first: it is quick, and the MSI below is the long step.
procedure InstallAnkiParts;
var
  Err: String;
  Code: Integer;
begin
  if (AddonZip <> '') and WantAddon then
  begin
    Err := InstallAnkiConnect(AddonZip, AnkiBaseDir);
    if Err = '' then
      AddonNow := True
    else
      SuppressibleMsgBox(FmtMessage(CustomMessage('AddonFailed'), [Err]), mbError, MB_OK, IDOK);
  end;
  if (AnkiMsi <> '') and WantAnki then
  begin
    // MSIINSTALLPERUSER keeps this a per-user install, so no administrator rights are needed.
    // 3010 only says a restart would be tidy, which Anki does not need.
    if Exec(ExpandConstant('{sys}\msiexec.exe'), '/i "' + AnkiMsi + '" /passive /norestart MSIINSTALLPERUSER=1',
         '', SW_SHOWNORMAL, ewWaitUntilTerminated, Code) and ((Code = 0) or (Code = 3010)) then
      AnkiNow := True
    else
      SuppressibleMsgBox(FmtMessage(CustomMessage('AnkiInstallFailed'), [IntToStr(Code)]), mbError, MB_OK, IDOK);
  end;
end;

procedure CurStepChanged(CurStep: TSetupStep);
var
  Code: Integer;
begin
  if CurStep = ssPostInstall then
  begin
    SeedConfig;
    if WizardIsTaskSelected('defaultapp') then
    begin
      RegisterDefaultAppExtensions;
      OpenDefaultApps := not WizardSilent;
    end;
    InstallAnkiParts;
  end
  else if (CurStep = ssDone) and OpenDefaultApps then
  begin
    // Windows lets only the user make the final choice; I just open the right page.
    if not ShellExec('open', 'ms-settings:defaultapps?registeredAppUser=Glossplay', '', '', SW_SHOWNORMAL, ewNoWait, Code) then
      ShellExec('open', 'ms-settings:defaultapps', '', '', SW_SHOWNORMAL, ewNoWait, Code);
  end;
end;

procedure CurPageChanged(CurPageID: Integer);
var
  Old, Delta: Integer;
  Note: String;
begin
  if (CurPageID = wpFinished) and (not HintAdded) then
  begin
    Note := '';
    if OpenDefaultApps then Note := CustomMessage('DefaultAppHint');
    if AnkiNow or AddonNow then
    begin
      if Note <> '' then Note := Note + #13#10#13#10;
      Note := Note + CustomMessage('AnkiFinish');
    end;
    if Note = '' then Exit;
    HintAdded := True;
    Old := WizardForm.FinishedLabel.Height;
    WizardForm.FinishedLabel.Caption := WizardForm.FinishedLabel.Caption + #13#10#13#10 + Note;
    WizardForm.FinishedLabel.AdjustHeight;
    Delta := WizardForm.FinishedLabel.Height - Old;
    WizardForm.RunList.Top := WizardForm.RunList.Top + Delta;
    WizardForm.RunList.Height := WizardForm.RunList.Height - Delta;
  end;
end;

// Integration keys are removed only when they point at the exe I installed; another copy
// of the player may own them.
function ShellKeysAreMine: Boolean;
var
  Want: String;
begin
  Want := '"' + ExpandConstant('{app}\{#AppExe}') + '" "%1"';
  Result :=
    (CompareText(CommandOf('Applications\glossplay.exe\shell\open\command'), Want) = 0) and
    (CompareText(CommandOf('SystemFileAssociations\video\shell\Glossplay\command'), Want) = 0);
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Code: Integer;
begin
  if CurUninstallStep = usUninstall then
  begin
    if ShellKeysAreMine then
      Exec(ExpandConstant('{app}\{#AppExe}'), '--shell-unregister', '', SW_HIDE, ewWaitUntilTerminated, Code);
  end
  else if CurUninstallStep = usPostUninstall then
  begin
    // Only values that name my ProgID are removed; anything else in those keys stays.
    UnregisterDefaultAppExtensions;
    RegDeleteKeyIfEmpty(HKCU, 'Software\Glossplay');
    if (not UninstallSilent) and
       (MsgBox(CustomMessage('AskRemoveData'), mbConfirmation, MB_YESNO or MB_DEFBUTTON2) = IDYES) then
      DelTree(ExpandConstant('{userappdata}\Glossplay'), True, True, True);
  end;
end;
