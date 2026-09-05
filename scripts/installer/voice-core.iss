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
;      %APPDATA%\voice-core (see docs/deployment.md in the development tree).
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
; 5. One entry point:
;    - Exactly ONE application shortcut, to VoiceCore.exe at the tree root. The subtitle
;      presenter and the CLI get none: the presenter is a child process VoiceCore.exe spawns,
;      and the CLI is an agent's tool rather than a launcher.
; 6. Engine Provisioning:
;    - Engine models (~4.8 GB) and venvs are NOT shipped, and Setup no longer runs
;      scripts\bootstrap.ps1 itself. The finished page launches VoiceCore.exe, whose Setup
;      screen detects what this machine already has and provisions only what is missing.
;      Two provisioners racing over one engine tree is worse than none.
; 7. Prerequisite:
;    - VoiceCore.exe is a WebView2 host. A machine without the Evergreen Runtime is told so,
;      with the download link, instead of being handed a blank window.
; 8. Agent skills:
;    - The two SKILL.md files ship inside the tree (skills\) AND at
;      %USERPROFILE%\.agents\skills\<name>\SKILL.md, which is where an agent with memory looks
;      for a skill without being handed a path. Ours overwrite on upgrade and are removed on
;      uninstall; the [Files] section states why.
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
; The one executable a user launches. bin\ holds the processes it owns and starts itself.
#define AppExeName "VoiceCore.exe"
; Single-instance names, oldest last. The GUI's is tauri-plugin-single-instance's
; "<identifier>-sim" form; the presenter's is its own role; the third is 1.1.0's tray, still
; listed so an upgrade over that install closes what is running there.
#define GuiMutex "io.github.yabo083.voicecore-sim"
#define PresenterMutex "voice-core-presenter"
#define LegacyTrayMutex "voice-core-winui-tray"

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
AppMutex={#GuiMutex},{#PresenterMutex},{#LegacyTrayMutex}
CloseApplications=yes
RestartApplications=no

; Output settings
OutputDir={#OutputDir}
OutputBaseFilename={#AppName}-{#AppVersion}-setup
SetupIconFile=..\..\app\VoiceCoreTray\assets\icon.ico
UninstallDisplayIcon={app}\{#AppExeName}

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
LaunchAppDescription=启动 voice-core（{#AppExeName}，首次使用请在应用内完成引擎与模型的安装）
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

; The two agent-facing skills, a SECOND copy of files already inside {app}\skills\, placed where
; an agent finds them without being told a path: %USERPROFILE%\.agents\skills\<name>\SKILL.md.
;
; Upgrade overwrites (ignoreversion): this file documents THIS build's CLI flags, error codes and
; paths, so a hand-edited copy left in place is a document that lies about the binary beside it.
; It is our versioned interface description rather than user data, and a user who wants a variant
; of it can keep that under a name of their own in the same directory, where nothing overwrites it.
; Uninstall deletes it (no uninsneveruninstall) for the same reason: since every upgrade replaces
; the file, we never promised to preserve edits to it, and a skill outliving the product tells an
; agent to run a binary that is gone. [UninstallDelete] then clears the two directories if they
; are empty, which covers the case where the user had already created one by hand: a directory
; Setup itself created is removed by the uninstaller automatically, one that was already there is
; not. Either way `.agents\skills\` survives as long as any other skill is in it.
;
; {%USERPROFILE} is the environment variable, not a shell folder: Inno 6 has no {userprofile}
; constant and ISCC rejects it outright. It is the profile of the user running Setup, which is the
; right answer in the default per-user mode; an admin who overrides to per-machine gets the global
; copy in the admin's own profile - no Inno constant can do better - and {app}\skills\ still
; serves every user on the machine.
Source: "{#SourceTree}\skills\voice-core-tts\SKILL.md"; \
  DestDir: "{%USERPROFILE}\.agents\skills\voice-core-tts"; Flags: ignoreversion
Source: "{#SourceTree}\skills\voice-core-voice-training\SKILL.md"; \
  DestDir: "{%USERPROFILE}\.agents\skills\voice-core-voice-training"; Flags: ignoreversion

[InstallDelete]
; Shortcuts earlier versions created, deleted on every install.
;
; Dropping an entry from [Icons] does NOT remove the file from a machine that already has it:
; the previous uninstall log is replaced, so 1.1.0's three shortcuts survived every upgrade and
; were still in the Start Menu after installing 1.4.0 over 1.3.0 - measured on this machine.
; One of them was worse than clutter: 托盘控制台 points at bin\app\VoiceCoreTray.exe, a path
; that no longer exists (the presenter lives in bin\presenter\ and is renamed), so clicking it
; produced a Windows error. The other two open bootstrap.ps1 in a PowerShell window, which is
; the provisioning path the 部署 screen replaced.
;
; Named literally rather than by wildcard: a wildcard over {group} would also delete a shortcut
; a user made themselves.
Type: files; Name: "{group}\{#AppName} 初始化向导 (Bootstrap).lnk"
Type: files; Name: "{group}\{#AppName} 环境诊断 (Check Only).lnk"
Type: files; Name: "{group}\{#AppName} 托盘控制台.lnk"
Type: files; Name: "{autodesktop}\{#AppName} 托盘控制台.lnk"

[Icons]
; EXACTLY ONE application shortcut. bin\presenter\ is spawned by {#AppExeName} and bin\voice-core.exe
; is an agent's tool; a shortcut to either would invite a user to start the stack twice. The three
; shortcuts 1.1.0 shipped are removed by [InstallDelete] above rather than merely absent here -
; absent from [Icons] only means "not created", never "taken off the machine".
Name: "{group}\{#AppName}"; Filename: "{app}\{#AppExeName}"; WorkingDir: "{app}"; Check: GuiPresent
Name: "{group}\卸载 {#AppName}"; Filename: "{uninstallexe}"

; Optional desktop shortcut, same single target
Name: "{autodesktop}\{#AppName}"; Filename: "{app}\{#AppExeName}"; WorkingDir: "{app}"; \
  Tasks: desktopicon; Check: GuiPresent

[Run]
; The finished page lands the user in the app, not in a PowerShell window. VoiceCore.exe's Setup
; screen is where provisioning lives now, and unlike bootstrap.ps1 launched blind it can find an
; engine, a virtualenv or a model cache that is already on this machine and reuse it instead of
; downloading ~4.8 GB again. Checked by default: a fresh install is unusable until it has run.
Filename: "{app}\{#AppExeName}"; WorkingDir: "{app}"; \
  Description: "{cm:LaunchAppDescription}"; \
  Flags: postinstall nowait skipifsilent; Check: GuiPresent

[UninstallRun]
; Ask the running voice-core runtime service to stop gracefully before deleting binaries.
; CloseApplications + AppMutex has already dealt with the GUI and the presenter; this covers a
; runtime that was started by the CLI and has no parent to stop it.
Filename: "{app}\bin\voice-core.exe"; Parameters: "stop"; WorkingDir: "{app}"; \
  Flags: runhidden skipifdoesntexist; RunOnceId: "VoiceCoreStopRuntime"

[UninstallDelete]
; The two global SKILL.md files are removed by the uninstaller itself - they are [Files] entries
; without uninsneveruninstall. These two lines only clear the directories they lived in, and only
; when nothing else is inside them.
Type: dirifempty; Name: "{%USERPROFILE}\.agents\skills\voice-core-tts"
Type: dirifempty; Name: "{%USERPROFILE}\.agents\skills\voice-core-voice-training"

[Code]
const
  // Microsoft's documented detection key for the WebView2 Evergreen Runtime.
  WebView2Client = '{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}';
  WebView2Download = 'https://go.microsoft.com/fwlink/p/?LinkId=2124703';

function GuiPresent: Boolean;
begin
  Result := FileExists(ExpandConstant('{app}\{#AppExeName}'));
end;

function WebView2Present: Boolean;
var
  Pv: String;
begin
  // EdgeUpdate is a 32-bit product, so a per-machine runtime records itself in the 32-bit view;
  // HKLM64 and a per-user HKCU install are both legitimate too. Microsoft documents an absent
  // key OR a pv of 0.0.0.0 as "not installed".
  Pv := '';
  if not RegQueryStringValue(HKLM32, 'SOFTWARE\Microsoft\EdgeUpdate\Clients\' + WebView2Client, 'pv', Pv) then
    if not RegQueryStringValue(HKLM64, 'SOFTWARE\Microsoft\EdgeUpdate\Clients\' + WebView2Client, 'pv', Pv) then
      if not RegQueryStringValue(HKCU, 'SOFTWARE\Microsoft\EdgeUpdate\Clients\' + WebView2Client, 'pv', Pv) then
        Pv := '';
  Result := (Pv <> '') and (Pv <> '0.0.0.0');
end;

function InitializeSetup: Boolean;
var
  ResultCode: Integer;
begin
  Result := True;
  if WebView2Present or WizardSilent then
    exit;
  // Reported, never installed behind the user's back: the Evergreen Bootstrapper is a Microsoft
  // download this installer does not redistribute, and a silent ~100 MB fetch is not something
  // an unsigned setup should do on someone's machine.
  if MsgBox('未检测到 Microsoft Edge WebView2 Runtime。' + #13#10#13#10 +
            'voice-core 的主界面（{#AppExeName}）依赖它渲染，缺失时窗口会是空白的。' + #13#10 +
            'Windows 11 与较新的 Windows 10 自带该组件，此机器上没有。' + #13#10#13#10 +
            '「是」= 继续安装（稍后请自行安装 WebView2）；「否」= 取消并打开下载页。',
            mbConfirmation, MB_YESNO) = IDNO then
  begin
    ShellExec('open', WebView2Download, '', '', SW_SHOWNORMAL, ewNoWait, ResultCode);
    Result := False;
  end;
end;

