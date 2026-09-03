# package.ps1 — assemble a portable voice-core install tree.
#
# The output is the production layout documented in docs/deployment.md:
#
#   <out>/bin/          voice-core-runtime.exe, voice-core.exe, app/VoiceCoreTray.exe
#   <out>/runtime/      python/ (engine venv), python-base/ (its interpreter),
#                       worker/irodori/worker.py, engine/ (engine source tree)
#   <out>/models/       huggingface/hub/... (weights)
#   <out>/data/         token.txt, config.json (voicePacks seeded), voicepacks/, logs/, spool/
#   <out>/skills/       voice-core/SKILL.md — the agent-facing contract
#
# Nothing in the tree contains an absolute path: the runtime derives everything
# from its own executable location, so the folder can be zipped, moved or copied
# to another machine. `runtime.json` is therefore NOT written — it exists only
# for dev checkouts and custom installs that need to override the layout.
#
# Engine and model payloads are several GB each and are opt-in:
#
#   .\scripts\package.ps1                                 # binaries + voice packs
#   .\scripts\package.ps1 -IncludeEngine -IncludeModels    # full, self-contained
#   .\scripts\package.ps1 -IncludeEngine -IncludeModels -Zip
#
# Without the engine the package still starts and serves; it reports the missing
# interpreter through GET /api/status and refuses to synthesize with a named
# error rather than crashing.

[CmdletBinding()]
param(
  # Output directory. Cleared of a previous package before assembly.
  [string]$Out = "dist/voice-core",

  # Copy the engine virtualenv, its base interpreter and the engine source.
  [switch]$IncludeEngine,

  # Copy the HuggingFace model cache.
  [switch]$IncludeModels,

  # Produce <out>.zip as well.
  [switch]$Zip,

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

Step "assembling $outRoot"
if (Test-Path $outRoot) { Remove-Item -Recurse -Force $outRoot }
foreach ($dir in 'bin', 'bin\app', 'runtime\worker\irodori', 'data\logs', 'data\spool') {
  New-Item -ItemType Directory -Force -Path (Join-Path $outRoot $dir) | Out-Null
}

Copy-Item $runtimeExe (Join-Path $outRoot 'bin')
Copy-Item $clientExe (Join-Path $outRoot 'bin')
Copy-Tree $trayDir (Join-Path $outRoot 'bin\app')
Copy-Item (Join-Path $repo 'worker\irodori\worker.py') (Join-Path $outRoot 'runtime\worker\irodori')

# The agent-facing contract travels WITH the install: an agent that finds the tree has to
# be able to learn the surface from it, without the development repo.
Copy-Tree (Join-Path $repo 'skills') (Join-Path $outRoot 'skills')

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
foreach ($doc in 'LICENSE', 'THIRD-PARTY-NOTICES.md', 'CHANGELOG.md') {
  $src = Join-Path $repo $doc
  if (Test-Path $src) { Copy-Item $src (Join-Path $outRoot $doc) }
  else { Warn "$doc is missing from the repo; the package will ship without it" }
}
$sdkLicenceDir = Join-Path $env:USERPROFILE '.nuget\packages\microsoft.windowsappsdk\1.6.250108002'
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

整个目录可以移动或复制到其他电脑，无需重新安装。
配置全在 data\config.json（对话框、快捷键、声线包一个文件），日志在 data\logs\。
AI agent 接入与新声线包制作：skills\voice-core\SKILL.md
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
