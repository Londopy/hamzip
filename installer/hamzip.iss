; hamzip - Inno Setup script
;
; Built by the release workflow with
;   ISCC.exe /DAppVersion=1.0.0 installer\hamzip.iss
; Run `cargo build --release` and `cargo build --release -p hamzip-gui` first
; so ..\target\release\hamzip.exe and hamzip-gui.exe exist.
;
; Output: installer\output\hamzip-<version>-setup-x64.exe
;
; Installs both programs in one folder: hamzip.exe, the command line, and
; hamzip-gui.exe, the window, which the Start menu and desktop shortcuts
; (named "hamzip") open. Putting hamzip on PATH is on by default, because a
; terminal is the only way to use the command line. No startup entry: the
; window has no reason to run all the time.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

; Folder holding the built exes, relative to this script. CI passes
; /DBinDir=..\target\x86_64-pc-windows-msvc\release when it builds with --target.
#ifndef BinDir
  #define BinDir "..\target\release"
#endif

; VersionInfoVersion must be plain numeric (x.x.x[.x]); strip any prerelease
; suffix like "-rc1" so prerelease tags still compile.
#if Pos("-", AppVersion) > 0
  #define NumericVersion Copy(AppVersion, 1, Pos("-", AppVersion) - 1)
#else
  #define NumericVersion AppVersion
#endif

#define AppName      "hamzip"
#define AppPublisher "Londopy"
#define AppURL       "https://github.com/Londopy/hamzip"
#define AppExeName   "hamzip.exe"
#define GuiExeName   "hamzip-gui.exe"

[Setup]
; Unique GUID - do NOT reuse this for a different app.
; The doubled opening brace is Inno's escape for a literal "{".
AppId={{8789A5D3-DF9C-4D72-A670-BF3F15121604}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases

; Per-user install, no UAC prompt. The dialog still lets the user pick an
; all-users install if they want one.
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog

; With lowest privileges {autopf} resolves to %LOCALAPPDATA%\Programs.
DefaultDirName={autopf}\{#AppName}
; The Start menu folder holds the window's shortcut and the uninstall entry.
DefaultGroupName={#AppName}
AllowNoIcons=yes

; hamzip is MIT OR Apache-2.0; the license page shows the MIT text, and the
; MSI shows the same.
LicenseFile=..\LICENSE-MIT

OutputDir=output
OutputBaseFilename=hamzip-{#AppVersion}-setup-x64
; No SetupIconFile: hamzip has no icon yet, so setup shows Inno's own.

; Compression
Compression=lzma2/ultra64
SolidCompression=yes

; Windows 10+ only (64-bit)
MinVersion=10.0
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible

WizardStyle=modern

; The PATH task edits the environment; this tells Windows, so terminals
; opened afterwards see the change without signing out.
ChangesEnvironment=yes

; Restart Manager asks to close a running hamzip before its exe is replaced.
; The command line is never killed by force, since it may be halfway through
; decoding a message; the window is (CloseWindow below).
CloseApplications=yes
CloseApplicationsFilter=*.exe
RestartApplications=no

UninstallDisplayIcon={app}\{#AppExeName}
UninstallDisplayName={#AppName} {#AppVersion}
VersionInfoVersion={#NumericVersion}
VersionInfoDescription={#AppName} {#AppVersion} setup
VersionInfoProductName={#AppName}
VersionInfoCompany={#AppPublisher}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; \
  Description: "{cm:CreateDesktopIcon}"; \
  GroupDescription: "{cm:AdditionalIcons}"
; Checked by default, unlike the GUI apps' opt-in integrations: the command
; line is only usable from a terminal. Puts {app} on the user's PATH (the
; machine's PATH for an all-users install). install.ps1's silent install gets
; it too.
Name: "addtopath"; \
  Description: "Add {#AppName} to the &PATH"; \
  GroupDescription: "Environment:"

[Files]
; Both programs - built before running iscc
Source: "{#BinDir}\{#AppExeName}"; \
  DestDir: "{app}"; \
  Flags: ignoreversion
Source: "{#BinDir}\{#GuiExeName}"; \
  DestDir: "{app}"; \
  Flags: ignoreversion

[Icons]
; The shortcuts open the window. The command line has none: it runs in a
; terminal, so a shortcut to it would just flash a console window.
Name: "{group}\{#AppName}"; Filename: "{app}\{#GuiExeName}"
Name: "{group}\{cm:UninstallProgram,{#AppName}}"; Filename: "{uninstallexe}"
Name: "{autodesktop}\{#AppName}"; \
  Filename: "{app}\{#GuiExeName}"; \
  Tasks: desktopicon

[Run]
; Offer to open the window after install (user can uncheck)
Filename: "{app}\{#GuiExeName}"; \
  Description: "{cm:LaunchProgram,{#StringChange(AppName, '&', '&&')}}"; \
  Flags: nowait postinstall skipifsilent

[UninstallDelete]
; An upgrade from either program renames each exe it replaces to <name>.old,
; which that program deletes the next time it starts; remove any still there.
Type: files; Name: "{app}\hamzip.old"
Type: files; Name: "{app}\hamzip-gui.old"
; The data folder (%LOCALAPPDATA%\hamzip, holding the 137 MB model) stays, as
; it does with the MSI: a station that reinstalls offline still has its model.

[Code]
// Windows can't replace or delete a running exe, and the window is often left
// open. Forcing it closed loses nothing a normal close would keep: it saves
// no state.
procedure CloseWindow();
var
  ResultCode: Integer;
begin
  Exec('taskkill.exe', '/F /IM {#GuiExeName}', '', SW_HIDE,
       ewWaitUntilTerminated, ResultCode);
  Sleep(500);
end;

// PATH handling (from nexium): append {app} to the user's PATH, or the
// machine's for an all-users install, and take it out again on uninstall.

function EnvKey(): string;
begin
  if IsAdminInstallMode then
    Result := 'SYSTEM\CurrentControlSet\Control\Session Manager\Environment'
  else
    Result := 'Environment';
end;

function EnvRoot(): Integer;
begin
  if IsAdminInstallMode then
    Result := HKEY_LOCAL_MACHINE
  else
    Result := HKEY_CURRENT_USER;
end;

procedure EnvAddPath(Dir: string);
var
  Paths: string;
begin
  if not RegQueryStringValue(EnvRoot(), EnvKey(), 'Path', Paths) then
    Paths := '';
  // already there (a reinstall): leave PATH as it is
  if Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';') > 0 then
    exit;
  if (Paths <> '') and (Paths[Length(Paths)] <> ';') then
    Paths := Paths + ';';
  Paths := Paths + Dir;
  RegWriteExpandStringValue(EnvRoot(), EnvKey(), 'Path', Paths);
end;

procedure EnvRemovePath(Dir: string);
var
  Paths: string;
  P: Integer;
begin
  if not RegQueryStringValue(EnvRoot(), EnvKey(), 'Path', Paths) then
    exit;
  P := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Paths) + ';');
  if P = 0 then
    exit;
  // P is the ';' in front of Dir in the padded string, which is Paths[P - 1].
  // When Dir is the first entry that ';' is the padding, so drop "Dir;"
  // instead (Delete at index 0 does nothing).
  if P = 1 then
    Delete(Paths, 1, Length(Dir) + 1)
  else
    Delete(Paths, P - 1, Length(Dir) + 1);
  RegWriteExpandStringValue(EnvRoot(), EnvKey(), 'Path', Paths);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssInstall then
    CloseWindow();
  if (CurStep = ssPostInstall) and WizardIsTaskSelected('addtopath') then
    EnvAddPath(ExpandConstant('{app}'));
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
begin
  if CurUninstallStep = usUninstall then
    CloseWindow();
  if CurUninstallStep = usPostUninstall then
    EnvRemovePath(ExpandConstant('{app}'));
end;
