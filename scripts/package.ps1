# package.ps1 — assemble a portable voice-core install tree and optionally produce an installer.
#
# The output is the production layout (docs/deployment.md, which stays in the dev tree):
#
#   <out>/VoiceCore.exe the app, and the ONLY executable a user ever launches. It starts the
#                       runtime and the subtitle presenter itself and owns the tray icon.
#   <out>/bin/          voice-core-runtime.exe (the service), voice-core.exe (the agent CLI),
#                       presenter/ (VoiceCorePresenter.exe + its Windows App SDK payload —
#                       spawned by VoiceCore.exe with --presenter, never by a human)
#   <out>/runtime/      python/ (engine venv), python-base/ (its interpreter),
#                       worker/irodori/worker.py, engine/ (engine source tree)
#   <out>/models/       huggingface/hub/... (weights)
#   <out>/data/         token.txt, config.json (voicePacks seeded), voicepacks/, logs/, spool/
#   <out>/skills/       voice-core-tts/SKILL.md (speaking) and
#                       voice-core-voice-training/SKILL.md (training a pack) — the two
#                       agent-facing contracts. The installer also places both under
#                       %USERPROFILE%\.agents\skills\<name>\, where an agent finds them
#                       without being handed a path.
#   <out>/docs/         api.md — the HTTP contract, for somebody building on the runtime
#   <out>/scripts/      bootstrap.ps1, training/ — provisioning and training kits
#
# The root holds exactly ONE executable, and the report at the bottom asserts it. That is the
# product decision, not an accident of copying: a user who opens the install folder must not
# have to work out which of three exes is the app, and the other two are private to
# VoiceCore.exe. (The installer adds its own unins000.exe there at install time.)
#
# Nothing in the tree contains an absolute path: the runtime derives everything
# from its own executable location, so the folder can be zipped, moved or copied
# to another machine. `runtime.json` is therefore NOT written — it exists only
# for dev checkouts and custom installs that need to override the layout.
#
# Engine and model payloads are several GB each and are opt-in for portable trees:
#
#   .\scripts\package.ps1                                 # binaries + notices + skills
#   .\scripts\package.ps1 -IncludeEngine -IncludeModels    # full portable, self-contained
#   .\scripts\package.ps1 -Installer                      # build voice-core-<version>-setup.exe
#   .\scripts\package.ps1 -Installer -SkipBuild           # package existing binaries into installer
#   .\scripts\package.ps1 -SkipGui                        # NO entry point; developer builds only
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

  # Compile the Inno Setup installer (producing voice-core-<version>-setup.exe + SHA256).
  [switch]$Installer,

  # Optional explicit path to ISCC.exe (Inno Setup Command-Line Compiler).
  [string]$IsccPath = "",

  # Engine virtualenv to bundle. Required with -IncludeEngine; no default, because the
  # only honest one would be a path on one machine. Set VC_ENGINE_VENV to avoid retyping.
  [string]$EngineVenv = "",

  # Engine source root: the directory that contains webui\Irodori-TTS. Same rule as above,
  # with VC_ENGINE_ROOT. What ships in there is OUR FORK of the engine rather than pristine
  # upstream - github.com/yabo083/Irodori-TTS on branch `voice-core`, taken from upstream
  # Aratako/Irodori-TTS at 8224daf, still MIT and with upstream's LICENSE byte-identical.
  # The branch carries the inference latency patches this release's numbers were measured
  # with; FORK.md at its root lists them and docs/adr/0002-engine-fork.md records why we own
  # it. A tree sitting on upstream `main` packages a working engine, just a slower one.
  [string]$EngineRoot = "",

  # HuggingFace cache to bundle with -IncludeModels. Same rule as the two above, with
  # VC_MODEL_CACHE.
  [string]$ModelCache = "",

  # Voice packs to bundle. OPT-IN and never defaulted: the packs on this machine are
  # LoRA adapters trained on Blue Archive voice audio, and v1 ADR-0007 decision 3 says
  # those are personal-use only and must not enter a distribution artefact. A package
  # built without this switch reports zero voices, which is the correct public default -
  # the user installs their own packs and registers them in data\config.json.
  [string]$VoicePacks = "",

  # Skip cargo/dotnet/tauri builds and use whatever is already built.
  [switch]$SkipBuild,

  # Assemble a tree with NO VoiceCore.exe at its root. This exists for one reason: the GUI is
  # still being written and packaging the rest must not be blocked on it. The result has no
  # entry point, the installer will create no Start Menu shortcut, and it must not be released.
  [switch]$SkipGui,

  # Explicit path to the built GUI executable, bypassing the search below.
  [string]$GuiExe = ""
)

$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path

# An installed product is the usual source on a machine that has one: it owns
# runtime\python and runtime\engine, which is exactly this pair. Hence env vars rather
# than a guessed path - see docs/getting-started.md. VC_ENGINE_ROOT must point at a tree
# whose webui\Irodori-TTS is our fork on branch `voice-core` (github.com/yabo083/Irodori-TTS);
# `git -C <root>\webui\Irodori-TTS branch --show-current` is how to check before a release
# build, because upstream `main` packages an engine ~2.4x slower per utterance.
if (-not $EngineVenv) { $EngineVenv = $env:VC_ENGINE_VENV }
if (-not $EngineRoot) { $EngineRoot = $env:VC_ENGINE_ROOT }
if ($IncludeEngine -and (-not $EngineVenv -or -not $EngineRoot)) {
  throw "-IncludeEngine needs both the venv and the engine source: pass -EngineVenv <dir> -EngineRoot <dir>, or set VC_ENGINE_VENV / VC_ENGINE_ROOT. On a machine with voice-core installed these are <install>\runtime\python and <install>\runtime\engine."
}
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

$managerDir = Join-Path $repo 'manager'
$managerTauri = Join-Path $managerDir 'src-tauri'

if (-not $SkipBuild) {
  Step 'cargo build --release'
  Push-Location $repo
  try {
    cargo build --release
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
  } finally { Pop-Location }

  Step 'dotnet build (presenter, Release x64)'
  Push-Location (Join-Path $repo 'app\VoiceCoreTray')
  try {
    dotnet build -c Release -p:Platform=x64
    if ($LASTEXITCODE -ne 0) { throw 'dotnet build failed' }
  } finally { Pop-Location }

  if (-not $SkipGui -and (Test-Path $managerTauri)) {
    # node_modules\.bin\tauri is the one entry point npm, pnpm, yarn and bun all write, so the
    # build never has to guess which of them owns manager\node_modules. --no-bundle stops Tauri
    # producing an MSI/NSIS installer that would be thrown away: the Inno script below is this
    # project's bundler. src-tauri is its own crate with its own target\, deliberately outside
    # the root Cargo workspace.
    $tauriCli = Join-Path $managerDir 'node_modules\.bin\tauri.cmd'
    Require-Path $tauriCli 'tauri CLI in manager\node_modules\.bin (install the JS deps first)'
    Step 'tauri build --no-bundle (manager -> VoiceCore.exe)'
    Push-Location $managerDir
    try {
      & $tauriCli build --no-bundle
      if ($LASTEXITCODE -ne 0) { throw 'tauri build failed' }
    } finally { Pop-Location }
  }
}

$runtimeExe = Join-Path $repo 'target\release\voice-core-runtime.exe'
$clientExe = Join-Path $repo 'target\release\voice-core.exe'
$presenterDir = Join-Path $repo 'app\VoiceCoreTray\bin\x64\Release\net8.0-windows10.0.22621.0'
Require-Path $runtimeExe 'voice-core-runtime.exe'
Require-Path $clientExe 'voice-core.exe'
Require-Path $presenterDir 'presenter build output'

# The GUI is the tree's entry point, so a missing one is an error with a remedy rather than a
# quietly incomplete package. Only -SkipGui downgrades it, and it says so loudly.
$guiExeResolved = ''
if ($GuiExe) {
  Require-Path $GuiExe 'GUI executable (-GuiExe)'
  $guiExeResolved = (Resolve-Path $GuiExe).Path
}
elseif (-not $SkipGui) {
  foreach ($cand in @(
      (Join-Path $managerTauri 'target\release\VoiceCore.exe'),
      (Join-Path $managerTauri 'target\x86_64-pc-windows-msvc\release\VoiceCore.exe'))) {
    if (Test-Path $cand) { $guiExeResolved = (Resolve-Path $cand).Path; break }
  }
  if (-not $guiExeResolved) {
    throw @"
VoiceCore.exe (the GUI, and the tree's only entry point) not found. Searched:
  $managerTauri\target\release\VoiceCore.exe
  $managerTauri\target\x86_64-pc-windows-msvc\release\VoiceCore.exe
Remedy:
  1. Build it:
       cd manager; .\node_modules\.bin\tauri build --no-bundle
  2. Or point at an existing build:
       .\scripts\package.ps1 -GuiExe "<path>\VoiceCore.exe"
  3. Or assemble a tree with no entry point (developer builds only):
       .\scripts\package.ps1 -SkipGui
"@
  }
}

# A GUI built by plain `cargo build --release` looks fine and is useless: without the
# `custom-protocol` feature that `tauri build` passes, `generate_context!` embeds no assets and
# the window loads `devUrl` (http://localhost:1420) instead of the bundle, so the app comes up
# blank on a machine with no dev server. It costs two people half an hour each to diagnose from
# the symptom, and the two artefacts differ by half their size, so refuse the wrong one here.
if ($guiExeResolved) {
  $guiBytes = (Get-Item $guiExeResolved).Length
  if ($guiBytes -lt 15MB) {
    throw @"
$guiExeResolved is $([math]::Round($guiBytes / 1MB, 1)) MB, which is too small to contain the
embedded frontend (a correct build is ~21 MB). That happens when it was built with
``cargo build --release`` instead of the tauri CLI, which also runs the Vite build and passes the
custom-protocol feature. The window would come up blank.
Remedy:
  cd manager; .\node_modules\.bin\tauri build --no-bundle
"@
  }
}

if ($SkipGui) {
  Warn "-SkipGui: this tree gets NO VoiceCore.exe at its root."
  Warn "  Nothing in it starts the runtime or the subtitle presenter, the installer will create"
  Warn "  no Start Menu shortcut, and its final page will have nothing to run. The runtime and"
  Warn "  the CLI still work by hand. Developer packaging only - do NOT publish this artefact."
}

# -- assemble ------------------------------------------------------------------

Step "assembling $outRoot (v$version)"
if (Test-Path $outRoot) { Remove-Item -Recurse -Force $outRoot }
foreach ($dir in 'bin', 'bin\presenter', 'runtime\worker\irodori', 'data\logs', 'data\spool', 'data\voicepacks') {
  New-Item -ItemType Directory -Force -Path (Join-Path $outRoot $dir) | Out-Null
}

# Copy-Item with a file destination, so a -GuiExe build under another name still lands as
# VoiceCore.exe - the name the installer's shortcut and its final page both point at.
if ($guiExeResolved) { Copy-Item $guiExeResolved (Join-Path $outRoot 'VoiceCore.exe') }
Copy-Item $runtimeExe (Join-Path $outRoot 'bin')
Copy-Item $clientExe (Join-Path $outRoot 'bin')
Copy-Tree $presenterDir (Join-Path $outRoot 'bin\presenter')
Copy-Item (Join-Path $repo 'worker\irodori\worker.py') (Join-Path $outRoot 'runtime\worker\irodori')

# The agent-facing contracts travel WITH the install: an agent that finds the tree has to
# be able to learn the surface from it, without the development repo. Two skills, because a
# daily "say this line" call has no business dragging the whole training pipeline into
# somebody's context: voice-core-tts speaks, voice-core-voice-training makes a new pack.
Copy-Tree (Join-Path $repo 'skills') (Join-Path $outRoot 'skills')

# Asserted rather than assumed: these two paths are what the installer's [Files] section
# copies into %USERPROFILE%\.agents\skills\, and what the 状态 screen's 使用说明 card tells an
# agent to read. A tree missing one of them is a broken product, not a lighter package.
foreach ($skill in 'voice-core-tts', 'voice-core-voice-training') {
  $shipped = Join-Path $outRoot "skills\$skill\SKILL.md"
  if (-not (Test-Path $shipped)) { throw "skills\$skill\SKILL.md missing from the package tree" }
}

# ONE doc, and only that one. The rest of the markdown under docs/ is our development
# documentation: it stays on the machine it is written on, out of the repo (.gitignore keeps
# it there) and out of the artefact. api.md is the exception because it is a published
# interface rather than a note to ourselves - the HTTP contract somebody building on the
# runtime reads - so it ships, and it is the ONLY thing docs/ contributes to the tree. The
# other files an install is self-explanatory through are `skills\voice-core-tts\SKILL.md` and
# `skills\voice-core-voice-training\SKILL.md` (for agents) and README.txt (for a human).
$apiDoc = Join-Path $repo 'docs\api.md'
if (Test-Path $apiDoc) {
  New-Item -ItemType Directory -Force -Path (Join-Path $outRoot 'docs') | Out-Null
  Copy-Item $apiDoc (Join-Path $outRoot 'docs\api.md')
} else {
  Warn "docs\api.md not found in repo; the package will ship without the HTTP contract"
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
  # enumerate both. Each entry is a pointer and nothing more: the pack's own
  # `voicepack.json` carries the name, the engine, the speaker and the portrait, and it
  # wins over anything seeded here (docs/voicepack-spec.md). Seeding a name would either
  # repeat the manifest or contradict it.
  #
  # Paths stay relative to the data dir, which is what keeps the tree portable.
  $packs = Get-ChildItem (Join-Path $outRoot 'data\voicepacks') | ForEach-Object {
    [ordered]@{
      id   = $_.BaseName
      path = "voicepacks/$($_.Name)"
    }
  }
  $described = Get-ChildItem (Join-Path $outRoot 'data\voicepacks') -Recurse -File -Filter '*voicepack.json' |
    Measure-Object | Select-Object -ExpandProperty Count
  Write-Host "    $($packs.Count) pack(s), $described with a manifest"
  if ($described -lt $packs.Count) {
    Warn "$($packs.Count - $described) bundled pack(s) carry no voicepack.json; they will show their id as the name"
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
# presenter ships. A package with no licence file next to the DLLs is not distributable.
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
  if (Test-Path $src) { Copy-Item $src (Join-Path $outRoot "bin\presenter\WindowsAppSDK-$doc") }
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

  # The engine's `.git` TRAVELS ON PURPOSE. Copy-Tree is robocopy /E with no exclusions, so
  # webui\Irodori-TTS\.git lands in the package - 597 KB of a 3.19 MB engine tree, measured,
  # against a ~148 MB artefact. Keep it: what we ship there is our fork (see the -EngineRoot
  # comment above), and FORK.md promises an installed machine two things that only exist if
  # the repository is present. `git -C runtime\engine\webui\Irodori-TTS log --oneline` answers
  # "which engine do I actually have" definitively, and `git checkout origin/main -- .` gets a
  # user back to pristine upstream with no network fetch - which is how you find out whether
  # the fork caused a bug. A documented recovery path that only works on a developer's box is
  # not a recovery path. So this is not an oversight to clean up later: deleting it to save
  # 597 KB silently deletes the recovery path FORK.md documents. Note the licence does not
  # depend on any of this - upstream's LICENSE and our FORK.md are plain files in the tree, so
  # a package with .git stripped would still be compliant. It would just be less recoverable.
  Step "engine source ($(Format-Size (Measure-Tree (Join-Path $EngineRoot 'webui'))))"
  Copy-Tree (Join-Path $EngineRoot 'webui') (Join-Path $outRoot 'runtime\engine\webui')
}

if ($IncludeModels) {
  # The cache is not part of the engine tree in an installed layout - the runtime looks
  # for it at <root>\models\huggingface - so it is named separately, with the in-tree
  # location kept as a fallback for an engine checkout that still holds its own.
  $cache = $ModelCache
  if (-not $cache) { $cache = $env:VC_MODEL_CACHE }
  if (-not $cache) {
    $inTree = Join-Path $EngineRoot 'model\huggingface'
    if (Test-Path $inTree) { $cache = $inTree }
  }
  if (-not $cache) {
    throw "-IncludeModels needs the HuggingFace cache: pass -ModelCache <dir> or set VC_MODEL_CACHE. On a machine with voice-core installed this is <install>\models\huggingface."
  }
  Require-Path $cache 'model cache'
  Step "model cache ($(Format-Size (Measure-Tree $cache)))"
  Copy-Tree $cache (Join-Path $outRoot 'models\huggingface')
}

Set-Content -Encoding UTF8 -Path (Join-Path $outRoot 'README.txt') -Value @'
voice-core — 本地语音合成服务（便携版）

启动：双击 VoiceCore.exe（唯一入口）
      它自己拉起后台服务和字幕对话框；关闭窗口只是收进托盘，显式退出才会一并停掉两者。
      首次使用请在应用内完成引擎与模型的检测和安装（已有的引擎/模型会被复用）。

给 AI agent 用的命令行（不是启动器，自己解析 token 和数据目录）：
      bin\voice-core.exe speak --text "日文台词" --display "中文字幕" --voice <声线id>
      bin\voice-core.exe events    # 订阅字幕与引擎状态
      bin\voice-core.exe doctor    # 一条命令诊断：可达性、鉴权、引擎、声线包
诊断：bin\voice-core-runtime.exe --print-layout

bin\presenter\ 是 VoiceCore.exe 拉起的字幕进程，不要直接双击。
训练专属声线：scripts\training\（install_pack.py --help 说明每个参数）

整个目录可以移动或复制到其他电脑，无需重新安装。
配置有两处：全局在 data\config.json（对话框、快捷键、装了哪些音色包），
每个音色包自己带 voicepack.json（名字、角色、头像、字幕样式）；同字段包内的优先。
日志在 data\logs\。
让 AI agent 出声：skills\voice-core-tts\SKILL.md
训练新的音色包：skills\voice-core-voice-training\SKILL.md
二次开发的 HTTP 接口：docs\api.md
'@

# -- report --------------------------------------------------------------------

Step 'layout check'
& (Join-Path $outRoot 'bin\voice-core-runtime.exe') --print-layout
if ($LASTEXITCODE -ne 0) { Warn "layout check exited $LASTEXITCODE" }
$global:LASTEXITCODE = 0

# One entry point is a property of the tree, so it is asserted rather than assumed: every
# extra root executable is another thing a user could double-click instead of the app.
$rootExes = @(Get-ChildItem -File -Path $outRoot -Filter '*.exe' | ForEach-Object { $_.Name })
$found = (($rootExes | Sort-Object) -join ', ')
$want = if ($SkipGui) { '' } else { 'VoiceCore.exe' }
if ($found -ne $want) {
  throw "package root must contain exactly [$want], found [$found]"
}

Step "package ready: $outRoot ($(Format-Size (Measure-Tree $outRoot)))"

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
