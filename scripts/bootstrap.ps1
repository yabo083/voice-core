# bootstrap.ps1 — provision a voice-core install so it can actually speak.
#
# voice-core ships the app and nothing else: no engine, no model weights, no Python
# environment, no voice packs. That is deliberate (weights are gigabytes and are not ours
# to redistribute), but it means a fresh install cannot make a sound until this script has
# run. It is idempotent and resumable — every stage checks whether it is already satisfied
# and says so — so re-running after a failure costs only the stage that failed.
#
# The Irodori backend is A backend, the one that is best at Japanese. Others, specialised
# for other languages, are expected later (docs/adr/0001-tts-backend-seam.md); nothing here
# may assume it is the only one, which is why every path it writes is namespaced by backend.
#
#   .\scripts\bootstrap.ps1 -CheckOnly     # environment report, downloads nothing
#   .\scripts\bootstrap.ps1                # full provision
#   .\scripts\bootstrap.ps1 -SkipModels    # engine + venv only
#
[CmdletBinding()]
param(
  # Report the environment and exit. Mutates nothing.
  [switch]$CheckOnly,

  # Install root. Defaults to the tree this script sits in, which is what makes a moved or
  # copied install work unchanged: everything below is written relative to it.
  [string]$InstallRoot = "",

  # Pinned engine revision. NOT a moving branch: the worker talks to the engine through
  # `irodori_tts.inference_runtime`, and an engine that changes shape silently breaks
  # synthesis with no version to blame. Bump deliberately, then re-run the smoke test.
  [string]$EngineRef = "main",

  [switch]$SkipEngine,
  [switch]$SkipVenv,
  [switch]$SkipModels,
  [switch]$SkipSmokeTest
)

$ErrorActionPreference = 'Stop'
$script:Failures = 0

if (-not $InstallRoot) { $InstallRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path }
$DataDir    = Join-Path $InstallRoot 'data'
$RuntimeDir = Join-Path $InstallRoot 'runtime'
$EngineRoot = Join-Path $RuntimeDir 'engine'          # <root>/runtime/engine
$EngineRepo = Join-Path $EngineRoot 'webui\Irodori-TTS'
$DacvaeDir  = Join-Path $EngineRoot 'webui\dacvae'
$VenvPython = Join-Path $EngineRepo '.venv\Scripts\python.exe'
$WorkerPy   = Join-Path $RuntimeDir 'worker\irodori\worker.py'
$HfHome     = Join-Path $InstallRoot 'models\huggingface'

# Upstream sources. Both MIT/Apache-2.0 and both redistributable, but we fetch rather than
# vendor so the user gets the real upstream and its licence file.
$EngineGit = 'https://github.com/Aratako/Irodori-TTS.git'
$DacvaeGit = 'https://github.com/facebookresearch/dacvae.git'

# The three model repos the Irodori backend loads, with their approximate sizes. Named in
# the engine's README and its checkpoint config; sizes measured from the local cache.
# PSCustomObject, not hashtable: Measure-Object reads properties, and a hashtable's keys
# are not properties in Windows PowerShell 5.1.
$Models = @(
  [pscustomobject]@{ Repo = 'Aratako/Irodori-TTS-v4.1-Small';         GiB = 3.1; What = 'the TTS checkpoint' }
  [pscustomobject]@{ Repo = 'sbintuitions/modernbert-ja-310m';        GiB = 1.3; What = 'the Japanese text encoder' }
  [pscustomobject]@{ Repo = 'Aratako/Semantic-DACVAE-Japanese-32dim'; GiB = 0.4; What = 'the 48 kHz audio codec' }
)
$ModelsGiB = ($Models | Measure-Object -Property GiB -Sum).Sum

function Step($m)  { Write-Host "==> $m" -ForegroundColor Cyan }
function Ok($m)    { Write-Host "    [OK]   $m" -ForegroundColor Green }
function Warn($m)  { Write-Host "    [WARN] $m" -ForegroundColor Yellow }
function Bad($m, $remedy) {
  Write-Host "    [FAIL] $m" -ForegroundColor Red
  Write-Host "           fix: $remedy" -ForegroundColor Red
  $script:Failures++
}
function Have($exe) { $null -ne (Get-Command $exe -ErrorAction SilentlyContinue) }

# -- stage 1: preflight --------------------------------------------------------
#
# Every FAIL here names its own remedy. A user who cannot get past this stage must never
# have to guess what to install.

Step "preflight"

$os = [Environment]::OSVersion.Version
if ($os.Major -ge 10) { Ok "Windows $($os.Major).$($os.Build)" }
else { Bad "Windows $($os.Major) is too old" "voice-core targets Windows 10/11 (WinUI 3)" }

$volume = (Get-Item $InstallRoot).PSDrive.Name
$freeGiB = [math]::Round((Get-PSDrive $volume).Free / 1GB, 1)
# Weights, plus the engine venv (torch + CUDA wheels are several GB), plus headroom for the
# spool and the pip/uv download cache.
$needGiB = [math]::Round($ModelsGiB + 6, 1)
if ($freeGiB -ge $needGiB) { Ok "disk ${volume}: ${freeGiB} GiB free (need about ${needGiB})" }
else { Bad "disk ${volume}: ${freeGiB} GiB free, need about ${needGiB}" "free up space or pass -InstallRoot on another volume" }

if (Have 'nvidia-smi') {
  $gpu = (& nvidia-smi --query-gpu=name,memory.total,driver_version --format=csv,noheader 2>$null | Select-Object -First 1)
  if ($gpu) { Ok "GPU: $gpu" } else { Warn "nvidia-smi ran but reported no GPU" }
}
else {
  Bad "no nvidia-smi" "the Irodori backend runs the model on CUDA; install an NVIDIA driver. A CPU path is not implemented"
}

# uv is how upstream installs the engine (`uv sync --extra cu128`), and it resolves the
# CUDA wheel index correctly on its own. Everything works without it, just slower to set up.
if (Have 'uv') { Ok "uv $((& uv --version) -replace 'uv ', '')" }
else { Warn "uv is absent; falling back to python -m venv + pip. Install from https://astral.sh/uv for the path upstream tests" }

if (Have 'git') { Ok "git $((& git --version) -replace 'git version ', '')" }
else { Bad "no git" "install Git for Windows (https://git-scm.com/download/win) — the engine is fetched by clone" }

# Only needed as a fallback: `uv sync` brings its own interpreter.
if (Have 'python') {
  $pyv = (& python -c "import sys;print('.'.join(map(str,sys.version_info[:3])))" 2>$null)
  if ($pyv) { Ok "python $pyv" } else { Warn "python is on PATH but did not answer --version" }
}
else { Warn "no python on PATH (fine when uv is present: it provisions its own 3.12)" }

foreach ($v in 'HTTPS_PROXY', 'HTTP_PROXY') {
  if ([Environment]::GetEnvironmentVariable($v)) { Ok "$v is set: $([Environment]::GetEnvironmentVariable($v))" }
}

$state = @(
  @{ What = 'engine source';  Path = $EngineRepo;  Hint = 'stage 2' }
  @{ What = 'DACVAE codec';   Path = $DacvaeDir;   Hint = 'stage 2' }
  @{ What = 'engine venv';    Path = $VenvPython;  Hint = 'stage 3' }
  @{ What = 'model cache';    Path = $HfHome;      Hint = 'stage 4' }
  @{ What = 'worker script';  Path = $WorkerPy;    Hint = 'shipped with the installer' }
)
Step "current state"
foreach ($s in $state) {
  if (Test-Path $s.Path) { Ok "$($s.What): present" } else { Warn "$($s.What): missing ($($s.Hint))" }
}

$packs = Join-Path $DataDir 'config.json'
if ((Test-Path $packs) -and (Select-String -Path $packs -Pattern '"id"' -Quiet)) { Ok "at least one voice pack is registered" }
else { Warn "no voice pack registered — see docs/training-a-voice.md; without one the runtime starts but cannot speak" }

if ($script:Failures -gt 0) {
  Write-Host ""
  Write-Host "$($script:Failures) blocking problem(s) above. Nothing was changed." -ForegroundColor Red
  exit 1
}
if ($CheckOnly) {
  Write-Host ""
  Write-Host "Environment looks usable. Re-run without -CheckOnly to provision." -ForegroundColor Green
  exit 0
}

# -- stage 2: the engine ------------------------------------------------------

if (-not $SkipEngine) {
  Step "engine source (Irodori-TTS, MIT)"
  New-Item -ItemType Directory -Force -Path (Split-Path $EngineRepo) | Out-Null
  if (Test-Path (Join-Path $EngineRepo '.git')) {
    & git -C $EngineRepo fetch --depth 1 origin $EngineRef
    & git -C $EngineRepo checkout --detach FETCH_HEAD
    Ok "updated to $EngineRef"
  }
  elseif (Test-Path $EngineRepo) {
    Warn "$EngineRepo exists but is not a git clone; leaving it alone"
  }
  else {
    & git clone --depth 1 --branch $EngineRef $EngineGit $EngineRepo
    Ok "cloned $EngineRef"
  }

  Step "audio codec (DACVAE, Apache-2.0, Meta)"
  # The worker puts this on sys.path, and the engine imports it as `dacvae`. Cloned even
  # when `uv sync` would install it as a dependency: both resolution paths then work.
  if (Test-Path (Join-Path $DacvaeDir '.git')) { Ok "already present" }
  elseif (Test-Path $DacvaeDir) { Warn "$DacvaeDir exists but is not a git clone; leaving it alone" }
  else {
    & git clone --depth 1 $DacvaeGit $DacvaeDir
    Ok "cloned"
  }
}

# -- stage 3: the engine's Python environment ---------------------------------

if (-not $SkipVenv) {
  Step "engine virtualenv"
  if (Test-Path $VenvPython) {
    Ok "already present ($VenvPython)"
  }
  elseif (Have 'uv') {
    # Upstream's own instruction (README: `uv sync --extra cu128`). It creates .venv inside
    # the repo, pins the interpreter, and picks the CUDA 12.8 wheel set.
    Push-Location $EngineRepo
    try { & uv sync --extra cu128 } finally { Pop-Location }
    if (Test-Path $VenvPython) { Ok "created with uv sync --extra cu128" }
    else { Bad "uv sync finished but $VenvPython is missing" "run 'uv sync --extra cu128' inside $EngineRepo and read its output" }
  }
  else {
    # Fallback without uv. The CUDA wheels do not live on PyPI, hence the extra index.
    & python -m venv (Join-Path $EngineRepo '.venv')
    & $VenvPython -m pip install --upgrade pip
    & $VenvPython -m pip install -e $EngineRepo --extra-index-url https://download.pytorch.org/whl/cu128
    if (Test-Path $VenvPython) { Ok "created with python -m venv + pip" }
    else { Bad "venv creation failed" "install uv (https://astral.sh/uv) and re-run" }
  }
}

# -- stage 4: model weights ---------------------------------------------------

if (-not $SkipModels) {
  Step "model weights (about $ModelsGiB GiB, all MIT)"
  New-Item -ItemType Directory -Force -Path $HfHome | Out-Null
  $env:HF_HOME = $HfHome
  $env:HF_HUB_CACHE = Join-Path $HfHome 'hub'
  # Downloads only; the worker sets HF_HUB_OFFLINE=1 at run time so a synthesis never
  # reaches for the network.
  Remove-Item Env:\HF_HUB_OFFLINE -ErrorAction SilentlyContinue

  foreach ($m in $Models) {
    Step "  $($m.Repo) — $($m.What), about $($m.GiB) GiB"
    # hf's cache is resumable and content-addressed: a complete repo is a no-op, a partial
    # one continues. That is the whole reason this stage needs no bookkeeping of its own.
    & $VenvPython -m huggingface_hub.commands.huggingface_cli download $m.Repo --quiet
    if ($LASTEXITCODE -ne 0) {
      Bad "download failed: $($m.Repo)" "check the network or the proxy, then re-run; completed files are kept"
    }
    else { Ok "$($m.Repo) is in the cache" }
  }
  if ($script:Failures -gt 0) { exit 1 }
}

# -- stage 5: tell the runtime where everything is ----------------------------
#
# `data/runtime.json`. Engine paths belong to the runtime, not to a frontend, and RELATIVE
# paths in this file resolve against the install root — which is what keeps the promise that
# the whole tree can be zipped, moved or copied to another machine (docs/deployment.md).

Step "runtime layout (data/runtime.json)"
New-Item -ItemType Directory -Force -Path $DataDir | Out-Null
$layout = [ordered]@{
  ttsPython = 'runtime/engine/webui/Irodori-TTS/.venv/Scripts/python.exe'
  ttsScript = 'runtime/worker/irodori/worker.py'
  ttsRoot   = 'runtime/engine'
  hfHome    = 'models/huggingface'
}
# NOT Set-Content -Encoding UTF8: that writes a BOM on Windows PowerShell 5.1.
[System.IO.File]::WriteAllText(
  (Join-Path $DataDir 'runtime.json'),
  ($layout | ConvertTo-Json),
  (New-Object System.Text.UTF8Encoding($false)))
Ok "written with relative paths, so the install stays portable"

# -- stage 6: prove it ---------------------------------------------------------

if (-not $SkipSmokeTest) {
  Step "smoke test"
  $runtimeExe = Join-Path $InstallRoot 'bin\voice-core-runtime.exe'
  $clientExe  = Join-Path $InstallRoot 'bin\voice-core.exe'
  if (-not (Test-Path $runtimeExe)) { $runtimeExe = Join-Path $InstallRoot 'target\release\voice-core-runtime.exe' }
  if (-not (Test-Path $clientExe))  { $clientExe  = Join-Path $InstallRoot 'target\release\voice-core.exe' }

  if (-not (Test-Path $runtimeExe)) {
    Warn "runtime binary not found; skipping. Build it (cargo build --release) or install the packaged tree"
  }
  else {
    $proc = Start-Process -FilePath $runtimeExe -ArgumentList @('--data-dir', $DataDir) -PassThru -WindowStyle Hidden
    try {
      Start-Sleep -Seconds 3
      Step "  loading the model (first time is the slow one: the checkpoint is 3.1 GiB)"
      & $clientExe --data-dir $DataDir warm
      if ($LASTEXITCODE -eq 0) { Ok "the backend loaded its model" }
      else { Warn "warm failed; read data\logs\tts-worker.err.log — the reason is in there verbatim" }

      $stages = Join-Path $DataDir 'logs\tts-worker.out.log'
      if (Test-Path $stages) {
        Step "  what it cost (from tts-worker.out.log)"
        Select-String -Path $stages -Pattern 'stage=(boot\.|model\.load\.done)' |
          Select-Object -Last 6 | ForEach-Object { Write-Host "      $($_.Line)" }
      }
    }
    finally {
      if ($proc -and -not $proc.HasExited) { & $clientExe --data-dir $DataDir stop 2>$null | Out-Null }
    }
  }
}

Write-Host ""
Step "done"
Write-Host "  Settings:      $(Join-Path $DataDir 'config.json')  (one file; comments allowed)"
Write-Host "  Start the app: $(Join-Path $InstallRoot 'bin\app\VoiceCoreTray.exe')"
Write-Host "  A voice pack:  docs\training-a-voice.md"
Write-Host "  For agents:    skills\voice-core\SKILL.md"
