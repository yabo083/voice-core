; ==============================================================================
; Inno Setup Script for voice-core
;
; Goal: Produce exactly ONE self-contained installer:
;       voice-core-<version>-setup.exe
;
; Requirements & Architecture:
; 1. Per-user install by default (PrivilegesRequired=lowest) with dialog override
;    allowed so administrators can choose per-machine if desired.
;    - Per-user installs into {autopf} (which resolves to %LOCALAPPDATA%\Programs
;      in lowest mode). The application directly writes to its own data\ dir without
;      requiring UAC elevation or splitting state into %APPDATA%.
;    - Per-machine installs into Program Files, where data\ is read-only for normal
;      users, causing voice-core-runtime to automatically fall back to
;      %APPDATA%\voice-core as documented in docs/deployment.md.
; 2. Single source of truth:
;    - AppVersion is passed via command-line (/DAppVersion=...) by package.ps1
;      which reads Cargo.toml, or read directly from the built voice-core-runtime.exe.
;    - SourceTree is passed via command-line (/DSourceTree=...) pointing to the
;      portable dist tree assembled by package.ps1.
; 3. Security & Integrity:
;    - The resulting executable is unsigned. SmartScreen and release notes verification
;      are documented on the InfoBefore page (before-install.txt).
;    - PowerShell execution uses absolute system paths ({sys}\WindowsPowerShell\v1.0\powershell.exe)
;      to prevent path hijacking.
; 4. Data Preservation:
;    - data\ directory (tokens, config, voicepacks, logs, spool) is NEVER removed
;      on uninstall (uninsneveruninstall).
; 5. Engine Provisioning:
;    - Engine models (~4.7 GB) and venvs are NOT shipped. On the finished page,
;      Setup offers to run scripts\bootstrap.ps1 to provision the engine.
; ==============================================================================

#if Ver < EncodeVer(6, 3, 0)
  #error This script requires Inno Setup 6.3.0 or later (for x64compatible architecture and UTF-8 handling without BOM).
#endif

#ifndef SourceTree
  ; Default to dist/voice-core relative to the script location when invoked standalone
  #define SourceTree "..\..\dist\voice-core"
#endif

#ifndef OutputDir
  #define OutputDir "..\..\dist"
#endif

#ifndef AppVersion
  #if FileExists(SourceTree + "\bin\voice-core-runtime.exe")
    #define AppVersion GetFileVersion(SourceTree + "\bin\voice-core-runtime.exe")
  #else
    #error AppVersion not defined and voice-core-runtime.exe not found. Pass /DAppVersion=x.y.z to ISCC.
  #endif
#endif

#define AppName "voice-core"
#define AppPublisher "voice-core contributors"
#define AppURL "https://github.com/yabo083/voice-core"
#define AppExeName "bin\app\VoiceCoreTray.exe"
#define TrayMutex "voice-core-winui-tray"

[Setup]
; Deterministic application identity derived for voice-core
AppId={{29FD851D-5FAD-563F-ADB6-2AC7B34E76D1}
AppName={#AppName}
AppVersion={#AppVersion}
AppVerName={#AppName} {#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}
AppUpdatesURL={#AppURL}

; Per-user by default (no UAC prompt), with dialog allowed for admin users
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=commandline dialog

; Target modern 64-bit Windows (x64 and ARM64 via Windows 11 emulation)
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.19041

; Destination: {autopf} resolves to {localappdata}\Programs in per-user mode,
; and to {commonpf} (Program Files) in per-machine mode.
DefaultDirName={autopf}\{#AppName}
DefaultGroupName={#AppName}
DisableProgramGroupPage=yes

; Process detection & graceful shutdown during upgrade/reinstall
AppMutex={#TrayMutex}
CloseApplications=yes
RestartApplications=no

; Output settings
OutputDir={#OutputDir}
OutputBaseFilename={#AppName}-{#AppVersion}-setup
SetupIconFile=..\..\app\VoiceCoreTray\assets\icon.ico
UninstallDisplayIcon={app}\bin\app\assets\icon.ico

; High-ratio solid compression
Compression=lzma2/max
SolidCompression=yes

; Wizard appearance & pre-install notes
WizardStyle=modern
DisableWelcomePage=no
InfoBeforeFile=before-install.txt

; Version info embedded in the setup executable resource
VersionInfoVersion={#AppVersion}
VersionInfoCompany={#AppPublisher}
VersionInfoDescription=voice-core installer
VersionInfoProductName={#AppName}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Messages]
; Clarify the uninstall behavior: data, downloaded engines, and voicepacks stay intact
ConfirmUninstall=确定要从计算机中卸载 %1 吗？%n%n注意：您的个人设置、声线包（data\）与下载的模型权重（models\、runtime\）将完整保留。
UninstalledMost=%1 卸载完成。%n%n由于保留了您的声线包、配置文件以及已下载的 AI 模型权重，安装目录仍有文件留存。若需彻底删除，请手动删除安装文件夹。
PrivilegesRequiredOverrideText2=%1 推荐以「仅为我安装」模式进行（无需管理员权限，声线与配置可直接写入）。若选择「为所有用户安装」，引擎初始化下载需管理员权限。

[CustomMessages]
RunBootstrapDescription=运行 voice-core 初始化向导（下载 Irodori-TTS 引擎与模型权重，约 4.7 GB）
LaunchTrayDescription=启动 voice-core 托盘程序 (VoiceCoreTray.exe)
DesktopShortcutDescription=创建桌面快捷方式 (&Create a desktop shortcut)

[Tasks]
Name: "desktopicon"; Description: "{cm:DesktopShortcutDescription}"; Flags: unchecked

[Dirs]
; Ensure data subdirectories exist even before first run, and NEVER delete them on uninstall
Name: "{app}\data"; Flags: uninsneveruninstall
Name: "{app}\data\logs"; Flags: uninsneveruninstall
Name: "{app}\data\spool"; Flags: uninsneveruninstall
Name: "{app}\data\voicepacks"; Flags: uninsneveruninstall
Name: "{app}\runtime"; Flags: uninsneveruninstall
Name: "{app}\models"; Flags: uninsneveruninstall

[Files]
; Copy the complete portable distribution tree assembled by package.ps1
; Exclude runtime state, cache, logs, models and temporary engine venvs.
; These are either user-created or provisioned by bootstrap.ps1.
Source: "{#SourceTree}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs; \
  Excludes: "\data\*,\runtime\python\*,\runtime\python-base\*,\runtime\engine\*,\models\*"

[Icons]
; Start menu shortcuts
Name: "{group}\{#AppName} 托盘控制台"; Filename: "{app}\bin\app\VoiceCoreTray.exe"; WorkingDir: "{app}\bin\app"; IconFilename: "{app}\bin\app\assets\icon.ico"; IconIndex: 0
Name: "{group}\{#AppName} 初始化向导 (Bootstrap)"; Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; \
  Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\bootstrap.ps1"""; WorkingDir: "{app}"; \
  IconFilename: "{app}\bin\app\assets\icon.ico"; IconIndex: 0; Check: BootstrapPresent
Name: "{group}\{#AppName} 环境诊断 (Check Only)"; Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; \
  Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\bootstrap.ps1"" -CheckOnly"; WorkingDir: "{app}"; \
  IconFilename: "{app}\bin\app\assets\icon.ico"; IconIndex: 0; Check: BootstrapPresent
Name: "{group}\卸载 {#AppName}"; Filename: "{uninstallexe}"

; Optional desktop shortcut for the tray
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\bin\app\VoiceCoreTray.exe"; WorkingDir: "{app}\bin\app"; \
  IconFilename: "{app}\bin\app\assets\icon.ico"; IconIndex: 0; Tasks: desktopicon

[Run]
; First run option: Run the bootstrap provisioning wizard.
; runascurrentuser ensures that if Setup was elevated (per-machine install), bootstrap has admin rights to write into Program Files.
; Check ensures the checkbox only appears if bootstrap.ps1 actually exists in the installed tree.
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; \
  Parameters: "-NoProfile -ExecutionPolicy Bypass -File ""{app}\scripts\bootstrap.ps1"""; \
  WorkingDir: "{app}"; \
  Description: "{cm:RunBootstrapDescription}"; \
  Flags: postinstall nowait runascurrentuser; Check: BootstrapPresent

; Second run option: Launch the tray directly
Filename: "{app}\bin\app\VoiceCoreTray.exe"; \
  WorkingDir: "{app}\bin\app"; \
  Description: "{cm:LaunchTrayDescription}"; \
  Flags: postinstall nowait unchecked

[Code]
function BootstrapPresent: Boolean;
begin
  Result := FileExists(ExpandConstant('{app}\scripts\bootstrap.ps1'));
end;
[UninstallRun]
; Ask the running voice-core runtime service to stop gracefully before deleting binaries
Filename: "{app}\bin\voice-core.exe"; Parameters: "stop"; WorkingDir: "{app}"; \
  Flags: runhidden skipifdoesntexist; RunOnceId: "VoiceCoreStopRuntime"
