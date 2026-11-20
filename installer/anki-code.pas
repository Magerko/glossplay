// Core of the optional Anki step: detecting Anki and placing the AnkiConnect add-on.
// Included from the [Code] section of glossplay.iss; the wizard page and the download
// live there. Nothing in here is ever undone by the uninstaller: Anki and its data are
// not mine.
const
  AnkiConnectId = '2055492159';

var
  // Empty means %APPDATA%\Anki2; the test harness points it at a scratch folder.
  AnkiBaseOverride: String;

function AnkiBaseDir: String;
begin
  if AnkiBaseOverride <> '' then
    Result := AnkiBaseOverride
  else
    Result := ExpandConstant('{userappdata}\Anki2');
end;

function UninstallKeyHasAnki(Root: Integer): Boolean;
var
  Sub, Name: String;
  Keys: TArrayOfString;
  I: Integer;
begin
  Result := False;
  Sub := 'Software\Microsoft\Windows\CurrentVersion\Uninstall';
  if RegGetSubkeyNames(Root, Sub, Keys) then
    for I := 0 to GetArrayLength(Keys) - 1 do
      if RegQueryStringValue(Root, Sub + '\' + Keys[I], 'DisplayName', Name) and
         (CompareText(Name, 'Anki') = 0) then
      begin
        Result := True;
        Exit;
      end;
end;

function AnkiAppFound: Boolean;
begin
  Result :=
    FileExists(ExpandConstant('{localappdata}\Programs\Anki\anki.exe')) or
    FileExists(ExpandConstant('{commonpf64}\Anki\anki.exe')) or
    FileExists(ExpandConstant('{commonpf32}\Anki\anki.exe')) or
    UninstallKeyHasAnki(HKCU) or UninstallKeyHasAnki(HKLM) or UninstallKeyHasAnki(HKLM32);
end;

function AddonDir(const BaseDir: String): String;
begin
  Result := BaseDir + '\addons21\' + AnkiConnectId;
end;

// Returns '' on success and a short reason otherwise. Never touches an add-on folder that
// already exists: its config lives in meta.json and has to survive.
function InstallAnkiConnect(const ZipFile, BaseDir: String): String;
var
  Stage, Src, Dest, Meta, Name: String;
  Code: Integer;
  Rec: TFindRec;
  Copied: Boolean;
begin
  Result := '';
  Dest := AddonDir(BaseDir);
  if DirExists(Dest) then Exit;

  Stage := ExpandConstant('{tmp}\ankiconnect-src');
  DelTree(Stage, True, True, True);
  if not ForceDirectories(Stage) then
  begin
    Result := 'cannot create a temporary folder';
    Exit;
  end;
  // tar.exe ships with Windows 10 1803 and later and reads zip archives.
  if (not Exec(ExpandConstant('{sys}\tar.exe'),
        '-xf "' + ZipFile + '" -C "' + Stage + '" "anki-connect-{#AnkiConnectVersion}/plugin"',
        '', SW_HIDE, ewWaitUntilTerminated, Code)) or (Code <> 0) then
  begin
    Result := 'cannot unpack the archive';
    Exit;
  end;
  Src := Stage + '\anki-connect-{#AnkiConnectVersion}\plugin';
  if not (FileExists(Src + '\__init__.py') and FileExists(Src + '\manifest.json')) then
  begin
    Result := 'the archive does not contain the add-on';
    Exit;
  end;
  if not ForceDirectories(Dest) then
  begin
    Result := 'cannot create the add-on folder';
    Exit;
  end;

  // __init__.py goes last: Anki ignores a folder without it, so an interrupted copy never
  // leaves a half-installed add-on that it would try to load.
  Copied := True;
  if FindFirst(Src + '\*', Rec) then
  try
    repeat
      Name := Rec.Name;
      if ((Rec.Attributes and FILE_ATTRIBUTE_DIRECTORY) = 0) and (Name <> '__init__.py') and
         (CompareText(Name, 'meta.json') <> 0) and (Copy(Name, 1, 1) <> '.') then
        if not CopyFile(Src + '\' + Name, Dest + '\' + Name, False) then Copied := False;
    until (not FindNext(Rec)) or (not Copied);
  finally
    FindClose(Rec);
  end;

  // Written as if installed from AnkiWeb (numeric folder name plus this meta.json), so Anki
  // shows it in its add-on list and offers later updates from AnkiWeb.
  // "mod" is when I pinned this version: Anki offers an update only if AnkiWeb has an
  // upload newer than that.
  Meta := '{"name": "Anki-Connect", "mod": {#AnkiConnectPinnedAt}, "human_version": "{#AnkiConnectVersion}"}';
  if Copied then
    Copied := SaveStringToFile(Dest + '\meta.json', Meta, False);
  if Copied then
    Copied := CopyFile(Src + '\__init__.py', Dest + '\__init__.py', False);
  if not Copied then
  begin
    DelTree(Dest, True, True, True);
    Result := 'cannot copy the add-on files';
  end;
end;
