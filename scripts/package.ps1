# package.ps1 — assemble a portable voice-core install tree and optionally produce an installer.
#
# The output is the production layout documented in docs/deployment.md:
#
#   <out>/bin/          voice-core-runtime.exe, voice-core.exe, app/VoiceCoreTray.exe
#   <out>/runtime/      python/ (engine venv), python-base/ (its interpreter),
#                       worker/irodori/worker.py, engine/ (engine source tree)
#   <out>/models/       huggingface/hub/... (weights)
#   <out>/data/         token.txt, config.json (voicePacks seeded), voicepacks/, logs/, spool/
#   <out>/skills/       voice-core/SKILL.md — the agent-facing contract
#   <out>/docs/         complete markdown documentation and guides
#   <out>/scripts/      bootstrap.ps1, training/ — provisioning and training kits
#
# Nothing in the tree contains an absolute path: the runtime derives everything
# from its own executable location, so the folder can be zipped, moved or copied
# to another machine. `runtime.json` is therefore NOT written — it exists only
# for dev checkouts and custom installs that need to override the layout.
#
# Engine and model payloads are several GB each and are opt-in for portable trees:
#
#   .\scripts\package.ps1                                 # binaries + notices + docs + skills
#   .\scripts\package.ps1 -IncludeEngine -IncludeModels    # full portable, self-contained
#   .\scripts\package.ps1 -Zip                            # portable zip archive
#   .\scripts\package.ps1 -Installer                      # build voice-core-<version>-setup.exe
#   .\scripts\package.ps1 -Installer -SkipBuild           # package existing binaries into installer
#
# Without the engine the package still starts and serves; it reports the missing
# interpreter through GET /api/status and refuses to synthesize with a named
# error rather than crashing.

[CmdletBinding()]
param(
  # Output directory for the portable tree. Cleared of a previous package before assembly.
  [string]$Out = "dist/voice-core",

  # Copy the engine virtualenv, its base interpreter and the engine source.
  [switch]$IncludeEngine,

  # Copy the HuggingFace model cache.
  [switch]$IncludeModels,

  # Produce <out>.zip as well.
  [switch]$Zip,

  # Compile the Inno Setup installer (producing voice-core-<version>-setup.exe + SHA256).
  [switch]$Installer,

  # Optional explicit path to ISCC.exe (Inno Setup Command-Line Compiler).
  [string]$IsccPath = "",

  # Engine virtualenv to bundle. Defaults to the sibling v1 checkout.
  [string]$EngineVenv = "",

  # Engine source root: the directory that contains webui\Irodori-TTS.
  [string]$EngineRoot = "",

  # Voice packs to bundle. OPT-IN and never defaulted: the packs on this machine are
  # LoRA adapters trained on Blue Archive voice audio, and v1 ADR-0007 decision 3 says
  # those are personal-use only and must not enter a distribution artefact. A package
  # built without this switch reports zero voices, which is the correct public default -
  # the user installs their own packs and registers them in data\config.json.
  [string]$VoicePacks = "",

  # Skip cargo/dotnet builds and use whatever is already built.
  [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$sibling = (Resolve-Path (Join-Path $repo '..')).Path

if (-not $EngineVenv) { $EngineVenv = Join-Path $sibling 'voice-core\tts\irodori-tts\env' }
if (-not $EngineRoot) { $EngineRoot = Join-Path $sibling 'voice-core\tts\irodori-tts' }
# NOT defaulted (see the parameter's comment): bundling a pack has to be an explicit act.

$outRoot = if ([System.IO.Path]::IsPathRooted($Out)) { $Out } else { Join-Path $repo $Out }

# Version extracted from Cargo.toml as single source of truth
$cargoToml = Join-Path $repo 'Cargo.toml'
if (Test-Path $cargoToml) {
  $cargoText = Get-Content $cargoToml -Raw
  if ($cargoText -match '(?m)^\s*version\s*=\s*"([^"]+)"') {
    $version = $Matches[1]
  } else {
    throw "Failed to extract version string from $cargoToml"
  }
} else {
  throw "Cargo.toml not found at $cargoToml"
}

function Step($message) { Write-Host "==> $message" -ForegroundColor Cyan }
function Warn($message) { Write-Host "    ! $message" -ForegroundColor Yellow }

function Require-Path($path, $what) {
  if (-not (Test-Path $path)) { throw "$what not found: $path" }
}

function Copy-Tree($from, $to) {
  New-Item -ItemType Directory -Force -Path $to | Out-Null
  # robocopy is the only reliable way to move multi-gigabyte trees on Windows;
  # exit codes 0-7 are success, 8+ are real failures.
  $null = robocopy $from $to /E /NFL /NDL /NJH /NJS /NP /R:1 /W:1
  if ($LASTEXITCODE -ge 8) { throw "robocopy failed ($LASTEXITCODE): $from -> $to" }
  $global:LASTEXITCODE = 0
}

function Measure-Tree($path) {
  if (-not (Test-Path $path)) { return 0 }
  $sum = (Get-ChildItem -Recurse -File -Force $path | Measure-Object -Sum Length).Sum
  if ($null -eq $sum) { return 0 }
  return $sum
}

function Format-Size($bytes) {
  if ($bytes -ge 1GB) { return "{0:N1} GB" -f ($bytes / 1GB) }
  if ($bytes -ge 1MB) { return "{0:N0} MB" -f ($bytes / 1MB) }
  return "{0:N0} KB" -f ($bytes / 1KB)
}

function Find-Iscc($explicitPath) {
  if ($explicitPath -and (Test-Path $explicitPath)) { return (Resolve-Path $explicitPath).Path }
  if ($env:ISCC_PATH -and (Test-Path $env:ISCC_PATH)) { return (Resolve-Path $env:ISCC_PATH).Path }

  $cmd = Get-Command iscc.exe -ErrorAction SilentlyContinue
  if ($cmd -and (Test-Path $cmd.Source)) { return $cmd.Source }

  $candidates = @(
    (Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 6\ISCC.exe'),
    (Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'),
    (Join-Path $env:ProgramFiles 'Inno Setup 6\ISCC.exe'),
    'C:\Inno Setup 6\ISCC.exe'
  )
  foreach ($cand in $candidates) {
    if ($cand -and (Test-Path $cand)) { return $cand }
  }
  return $null
}

# -- build ---------------------------------------------------------------------

if (-not $SkipBuild) {
  Step 'cargo build --release'
  Push-Location $repo
  try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
  } finally { Pop-Location }

  Step 'dotnet build (tray, Release x64)'
  Push-Location (Join-Path $repo 'app\VoiceCoreTray')
  try {
    dotnet build -c Release -p:Platform=x64
    if ($LASTEXITCODE -ne 0) { throw 'dotnet build failed' }
  } finally { Pop-Location }
}

$runtimeExe = Join-Path $repo 'target\release\voice-core-runtime.exe'
$clientExe = Join-Path $repo 'target\release\voice-core.exe'
$trayDir = Join-Path $repo 'app\VoiceCoreTray\bin\x64\Release\net8.0-windows10.0.22621.0'
Require-Path $runtimeExe 'voice-core-runtime.exe'
Require-Path $clientExe 'voice-core.exe'
Require-Path $trayDir 'tray build output'

# -- assemble ------------------------------------------------------------------

Step "assembling $outRoot (v$version)"
if (Test-Path $outRoot) { Remove-Item -Recurse -Force $outRoot }
foreach ($dir in 'bin', 'bin\app', 'runtime\worker\irodori', 'data\logs', 'data\spool', 'data\voicepacks') {
  New-Item -ItemType Directory -Force -Path (Join-Path $outRoot $dir) | Out-Null
}

Copy-Item $runtimeExe (Join-Path $outRoot 'bin')
Copy-Item $clientExe (Join-Path $outRoot 'bin')
Copy-Tree $trayDir (Join-Path $outRoot 'bin\app')
Copy-Item (Join-Path $repo 'worker\irodori\worker.py') (Join-Path $outRoot 'runtime\worker\irodori')

# The agent-facing contract travels WITH the install: an agent that finds the tree has to
# be able to learn the surface from it, without the development repo.
Copy-Tree (Join-Path $repo 'skills') (Join-Path $outRoot 'skills')

# Complete documentation travels with the tree so users and developers have local reference.
$docsSrc = Join-Path $repo 'docs'
if (Test-Path $docsSrc) {
  Step 'copying docs'
  Copy-Tree $docsSrc (Join-Path $outRoot 'docs')
}

# Provisioning and training scripts: bootstrap wizard and voice training kit.
$bootstrapScript = Join-Path $repo 'scripts\bootstrap.ps1'
if (Test-Path $bootstrapScript) {
  New-Item -ItemType Directory -Force -Path (Join-Path $outRoot 'scripts') | Out-Null
  Copy-Item $bootstrapScript (Join-Path $outRoot 'scripts\bootstrap.ps1')
} else {
  Warn "scripts\bootstrap.ps1 not found in repo; package will omit bootstrap wizard"
}

$trainingDir = Join-Path $repo 'scripts\training'
if (Test-Path $trainingDir) {
  Copy-Tree $trainingDir (Join-Path $outRoot 'scripts\training')
  # Stale bytecode from whatever interpreter last ran these locally has no business in a
  # release artefact: it is not portable, it is not the source, and a mismatched .pyc is a
  # confusing failure to debug.
  Get-ChildItem (Join-Path $outRoot 'scripts\training') -Recurse -Directory -Filter '__pycache__' |
    ForEach-Object { Remove-Item -Recurse -Force $_.FullName }
} else {
  Warn "scripts\training not found in repo; package will omit training kit"
}

Step 'voice packs'
if (-not $VoicePacks) {
  Write-Host "    none bundled (pass -VoicePacks <dir> to include your own)"
}
elseif (Test-Path $VoicePacks) {
  Copy-Tree $VoicePacks (Join-Path $outRoot 'data\voicepacks')
  # A pack is a directory (LoRA adapter) or a single file (speaker embedding), so
  # enumerate both. Registry paths stay relative to the data dir, which is what
  # keeps the tree portable.
  $packs = Get-ChildItem (Join-Path $outRoot 'data\voicepacks') | ForEach-Object {
    $kind = if ($_.PSIsContainer) { 'lora-adapter' } else { 'speaker-embedding' }
    [ordered]@{
      id        = $_.BaseName
      name      = $_.BaseName
      languages = @('ja')
      kind      = $kind
      path      = "voicepacks/$($_.Name)"
      engine    = 'irodori-tts-v4.1-small'
    }
  }
  if ($packs) {
    # The registry is a section of the app's one settings file. The tray writes that file
    # (with comments) on first run, but a packaged install must have voices BEFORE the tray
    # has ever started, so the packs are seeded here and the tray fills in the rest.
    #
    # NOT Set-Content -Encoding UTF8: on Windows PowerShell 5.1 that means "with BOM",
    # and while the runtime's JSONC reader now strips one, a settings file a human opens
    # should not start with three invisible bytes.
    [System.IO.File]::WriteAllText(
      (Join-Path $outRoot 'data\config.json'),
      ([ordered]@{ voicePacks = @($packs) } | ConvertTo-Json -Depth 5),
      (New-Object System.Text.UTF8Encoding($false)))
    Write-Host "    $($packs.Count) pack(s): $(($packs | ForEach-Object { $_.id }) -join ', ')"
  }
}
else {
  Warn "no voice packs at $VoicePacks; the install will report zero voices"
}

# Notices travel with the binaries, not just with the source: every permissive licence in
# THIRD-PARTY-NOTICES.md requires its notice to accompany a binary distribution, and the
# Windows App SDK's terms forbid removing supplier notices from the redistributables the
# tray ships. A package with no licence file next to the DLLs is not distributable.
Step 'licences and notices'
foreach ($doc in 'LICENSE', 'LICENSE-EXCEPTION.md', 'THIRD-PARTY-NOTICES.md', 'CHANGELOG.md') {
  $src = Join-Path $repo $doc
  if (Test-Path $src) { Copy-Item $src (Join-Path $outRoot $doc) }
  else { Warn "$doc is missing from the repo; the package will ship without it" }
}

# Read Windows App SDK package version dynamically from csproj to keep notices in sync
$csproj = Join-Path $repo 'app\VoiceCoreTray\VoiceCoreTray.csproj'
$sdkVer = '1.6.250108002'
if (Test-Path $csproj) {
  $csprojRaw = Get-Content $csproj -Raw
  if ($csprojRaw -match 'PackageReference\s+Include="Microsoft\.WindowsAppSDK"\s+Version="([^"]+)"') {
    $sdkVer = $Matches[1]
  }
}
$sdkLicenceDir = Join-Path $env:USERPROFILE ".nuget\packages\microsoft.windowsappsdk\$sdkVer"
foreach ($doc in 'license.txt', 'NOTICE.txt') {
  $src = Join-Path $sdkLicenceDir $doc
  if (Test-Path $src) { Copy-Item $src (Join-Path $outRoot "bin\app\WindowsAppSDK-$doc") }
  else { Warn "Windows App SDK $doc not found at $sdkLicenceDir; add it before distributing" }
}

if ($IncludeEngine) {
  Require-Path $EngineVenv 'engine virtualenv'
  Require-Path (Join-Path $EngineRoot 'webui') 'engine source (webui)'

  Step "engine virtualenv ($(Format-Size (Measure-Tree $EngineVenv)))"
  Copy-Tree $EngineVenv (Join-Path $outRoot 'runtime\python')

  # A Windows venv records its base interpreter as an absolute path, so the base
  # has to travel with it. The runtime repoints pyvenv.cfg at this copy on the
  # first start after a move.
  $cfg = Join-Path $EngineVenv 'pyvenv.cfg'
  Require-Path $cfg 'pyvenv.cfg'
  $baseHome = (Get-Content $cfg | Where-Object { $_ -match '^\s*home\s*=' } |
    Select-Object -First 1) -replace '^\s*home\s*=\s*', ''
  if ($baseHome -and (Test-Path $baseHome)) {
    Step "base interpreter ($(Format-Size (Measure-Tree $baseHome)))"
    Copy-Tree $baseHome (Join-Path $outRoot 'runtime\python-base')
    $packagedBase = Join-Path $outRoot 'runtime\python-base'
    (Get-Content (Join-Path $outRoot 'runtime\python\pyvenv.cfg')) |
      ForEach-Object { if ($_ -match '^\s*home\s*=') { "home = $packagedBase" } else { $_ } } |
      Set-Content -Encoding UTF8 (Join-Path $outRoot 'runtime\python\pyvenv.cfg')
  }
  else {
    Warn "pyvenv.cfg home is missing ($baseHome); the bundled venv will not run until repaired"
  }

  Step "engine source ($(Format-Size (Measure-Tree (Join-Path $EngineRoot 'webui'))))"
  Copy-Tree (Join-Path $EngineRoot 'webui') (Join-Path $outRoot 'runtime\engine\webui')
}

if ($IncludeModels) {
  $cache = Join-Path $EngineRoot 'model\huggingface'
  Require-Path $cache 'model cache'
  Step "model cache ($(Format-Size (Measure-Tree $cache)))"
  Copy-Tree $cache (Join-Path $outRoot 'models\huggingface')
}

Set-Content -Encoding UTF8 -Path (Join-Path $outRoot 'README.txt') -Value @'
voice-core — 本地语音合成服务（便携版）

启动：双击 bin\app\VoiceCoreTray.exe（托盘图标 → 右键菜单）
命令行：bin\voice-core.exe speak --text "日文台词" --display "中文字幕" --voice <声线id>
诊断：bin\voice-core.exe doctor
      bin\voice-core-runtime.exe --print-layout

环境初始化向导：scripts\bootstrap.ps1（自动下载配置 Irodori-TTS 引擎与模型权重）
制作训练专属声线：docs\training-a-voice.md，scripts\training\

整个目录可以移动或复制到其他电脑，无需重新安装。
配置全在 data\config.json（对话框、快捷键、声线包一个文件），日志在 data\logs\。
AI agent 接入与接口规范：skills\voice-core\SKILL.md
完整开发与部署文档：docs\
'@

# -- report --------------------------------------------------------------------

Step 'layout check'
& (Join-Path $outRoot 'bin\voice-core-runtime.exe') --print-layout
if ($LASTEXITCODE -ne 0) { Warn "layout check exited $LASTEXITCODE" }
$global:LASTEXITCODE = 0

Step "package ready: $outRoot ($(Format-Size (Measure-Tree $outRoot)))"

if ($Zip) {
  $zipPath = "$outRoot.zip"
  Step "compressing -> $zipPath"
  if (Test-Path $zipPath) { Remove-Item -Force $zipPath }
  Compress-Archive -Path (Join-Path $outRoot '*') -DestinationPath $zipPath -CompressionLevel Optimal
  Step "zip ready: $zipPath ($(Format-Size (Get-Item $zipPath).Length))"
}

if ($Installer) {
  $iscc = Find-Iscc $IsccPath
  if (-not $iscc) {
    throw @"
Inno Setup Compiler (ISCC.exe) not found.
To compile the single-file installer (voice-core-$version-setup.exe), Inno Setup 6.3+ is required.
Remedy:
  1. Install Inno Setup via winget:
       winget install JRSoftware.InnoSetup
  2. Or download installer from:
       https://jrsoftware.org/isdl.php
  3. Or pass the explicit path to ISCC:
       .\scripts\package.ps1 -Installer -IsccPath "C:\Path\To\Inno Setup 6\ISCC.exe"
"@
  }

  $issFile = Join-Path $repo 'scripts\installer\voice-core.iss'
  Require-Path $issFile 'Inno Setup script'

  $setupOutDir = Split-Path $outRoot -Parent
  if (-not (Test-Path $setupOutDir)) {
    New-Item -ItemType Directory -Force -Path $setupOutDir | Out-Null
  }

  Step "compiling Inno Setup installer with $iscc"
  $sourceTreeParam = $outRoot.TrimEnd('\')
  $outputDirParam = $setupOutDir.TrimEnd('\')

  Push-Location (Join-Path $repo 'scripts\installer')
  try {
    & $iscc "/DSourceTree=$sourceTreeParam" "/DOutputDir=$outputDirParam" "/DAppVersion=$version" "/O$outputDirParam" $issFile
    if ($LASTEXITCODE -ne 0) { throw "ISCC compilation failed (exit code $LASTEXITCODE)" }
  } finally {
    Pop-Location
  }

  $expectedSetupExe = Join-Path $setupOutDir "voice-core-$version-setup.exe"
  if (Test-Path $expectedSetupExe) {
    # .NET rather than Get-FileHash: the cmdlet lives in Microsoft.PowerShell.Utility, and
    # this script has been observed running in a host where that module was not available.
    # The hash is the whole point of an unsigned release, so it must not depend on a module.
    $sha = [System.Security.Cryptography.SHA256]::Create()
    try {
      $stream = [System.IO.File]::OpenRead($expectedSetupExe)
      try { $hash = ([BitConverter]::ToString($sha.ComputeHash($stream))) -replace '-', '' }
      finally { $stream.Dispose() }
    } finally { $sha.Dispose() }
    $size = Format-Size (Get-Item $expectedSetupExe).Length
    Step "installer ready: $expectedSetupExe ($size)"
    Write-Host "    SHA256: $hash" -ForegroundColor Green
    Write-Host "    NOTE: The setup executable is UNSIGNED and will trigger Windows Defender SmartScreen." -ForegroundColor Yellow
    Write-Host "          Publish the SHA256 in GitHub Release notes for user integrity verification." -ForegroundColor Yellow
  } else {
    Warn "ISCC succeeded but expected setup executable was not found at $expectedSetupExe"
  }
}
