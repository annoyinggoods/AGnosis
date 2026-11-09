; AGnosis — the Windows installer (Inno Setup 6).
;
; Build with packaging/windows/build-installer.ps1 (release build + ISCC).
; Per-user install, no elevation: the default folder is
; %LOCALAPPDATA%\Programs\AGnosis and the wizard's directory page lets the
; user pick another one. The wizard shows the LICENSE and the README (the
; "help on first run" page), and the finish page offers the three setup
; steps the app itself owns: `agnosis install` (login autostart),
; `agnosis start` (daemon now — the global hotkey comes up with it), and
; `agnosis help` (the README window). The uninstaller stops the daemon and
; reverses the autostart before removing the files.

#define MyAppName "AGnosis"
#ifdef AppVersion
#define MyAppVersion AppVersion
#else
#define MyAppVersion "0.9.0"
#endif

[Setup]
AppId={{0F22A6FB-B2D8-4F58-8211-EFF1B0E46E9A}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppVerName={#MyAppName} {#MyAppVersion}
AppPublisher=Annoying Goods
AppPublisherURL=https://annoyinggoods.com
DefaultDirName={localappdata}\Programs\{#MyAppName}
UsePreviousAppDir=yes
DisableDirPage=no
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
OutputDir=dist
OutputBaseFilename=agnosis-setup
SetupIconFile=agnosis.ico
UninstallDisplayName={#MyAppName}
UninstallDisplayIcon={app}\agnosis.exe
LicenseFile=..\..\LICENSE.txt
InfoBeforeFile=..\..\README.md
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
Compression=lzma2
SolidCompression=yes
ArchitecturesInstallIn64BitMode=x64compatible

[Tasks]
Name: "desktopicon"; Description: "Create a &desktop shortcut"; GroupDescription: "Additional icons:"; Flags: checkedonce

[Files]
Source: "..\..\target\release\agnosis.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\..\LICENSE.txt"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\agnosis.exe"; Comment: "Hide your browser behind a fullscreen ASCII duck parade"
Name: "{autodesktop}\{#MyAppName}"; Filename: "{app}\agnosis.exe"; Tasks: desktopicon; Comment: "Hide your browser behind a fullscreen ASCII duck parade"

; Graceful stop first, then a belt-and-braces kill (the daemon holds the exe
; image), then the autostart removal, then Inno deletes the files.
; [UninstallRun] entries run in REVERSE definition order, so "stop" is
; defined last: it runs first.
[UninstallRun]
Filename: "{app}\agnosis.exe"; Parameters: "uninstall"; RunOnceId: "UninstallAutostart"; Flags: runhidden
Filename: "{cmd}"; Parameters: "/C taskkill /f /im agnosis.exe >nul 2>&1"; RunOnceId: "KillDaemon"; Flags: runhidden
Filename: "{app}\agnosis.exe"; Parameters: "stop"; RunOnceId: "StopDaemon"; Flags: runhidden

[Run]
Filename: "{app}\agnosis.exe"; Parameters: "install"; Description: "&Register the login autostart (the daemon starts at login)"; Flags: postinstall runasoriginaluser runhidden
Filename: "{app}\agnosis.exe"; Parameters: "start"; Description: "&Start the daemon now (the global hotkey works immediately)"; Flags: postinstall runasoriginaluser runhidden
Filename: "{app}\agnosis.exe"; Parameters: "help"; Description: "&Show the help (the README)"; Flags: postinstall runasoriginaluser
