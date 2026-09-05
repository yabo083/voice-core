# bootstrap.ps1 — make a voice-core install able to speak, reusing whatever is already here.
#
# voice-core ships the app and nothing else: no engine, no model weights, no Python
# environment, no voice packs. That is deliberate (the weights are 4.44 GiB and are not
# ours to redistribute), but it means a fresh install cannot make a sound until this has
# run.
#
# The rule that shapes everything below: NEVER fetch what the machine already has. The
# first release of this script trusted nothing and detected nothing, so a user with a
# complete engine tree, a working venv and a full model cache on disk was told to
# download 4.44 GiB again. Every stage therefore starts by looking, and every check looks
# at CONTENT rather than at a path that happens to exist:
#
#   engine tree   webui/Irodori-TTS/irodori_tts/inference_runtime.py is importable-shaped
#   codec         webui/dacvae/dacvae/__init__.py exists
#   interpreter   it RUNS and reports torch.__version__ and torch.cuda.is_available()
#   model repo    the HF cache holds the repo's snapshot payload, and it is not a stub
#   voice pack    a directory with adapter_config.json + adapter_model.safetensors, or a
#                 file named *.speaker.safetensors
#
# What it will NOT do: crawl the disk. A whole-volume search for a 5 GiB tree is slow and
# invasive, so detection only looks where an answer can be justified — in order:
#
#   1. data/runtime.json, the record of what this install was last pointed at
#   2. -EngineRoot / -HfHome / -VoicePacks. Passing one REPLACES the search for that
#      resource: an explicit answer is not a hint to be overruled by a stale file.
#   3. <engine root>\model\huggingface — the cache the WORKER itself falls back to when
#      runtime.json names no hfHome, so weights there are already the ones this install
#      loads (cache only)
#   4. HF_HOME / HF_HUB_CACHE from the environment (cache only)
#   5. the packaged layout, <root>\runtime\engine and <root>\models\huggingface
#   6. %USERPROFILE%\.cache\huggingface, the HF default (cache only)
#
# When none of them answers, the failing stage says exactly what would satisfy the check
# and which flag to pass. It never guesses. Candidates 3, 4 and 6 are for REUSE only: a
# download always lands in the install's own models\huggingface (or in -HfHome), because
# filling %USERPROFILE%\.cache\huggingface from a fresh install would put 4.44 GiB in the
# user profile and cost the tree its portability.
#
# Reuse means POINTING at what exists — data/runtime.json gets the absolute path — never
# copying gigabytes. A path inside the install stays relative, which is what keeps the
# promise that the tree can be zipped, moved or copied (docs/deployment.md). Only reuse of
# an outside tree writes an absolute path, and the layout stage says out loud that this
# couples the install to that location.
#
# Voice packs are the exception, and the layout stage COPIES them: a pack is ~100 MiB, not
# gigabytes, and an install that owns its packs stays zippable and lets the training corpus
# be deleted. That copy plus the config.json entry is scripts/training/install_pack.py's job,
# called rather than reimplemented (see Register-Packs).
#
# No stage aborts the run. A failure is an event with a remedy; independent later stages
# still execute; the exit code stays 0. Only a usage error exits non-zero (2), because
# that is the caller's bug, not the machine's.
#
#   .\scripts\bootstrap.ps1 -CheckOnly            # what is here, what is missing. Changes nothing.
#   .\scripts\bootstrap.ps1                       # provision whatever is missing
#   .\scripts\bootstrap.ps1 -Only models          # one stage, e.g. after fixing the network
#   .\scripts\bootstrap.ps1 -EngineRoot D:\irodori-tts -HfHome D:\hf   # reuse a tree elsewhere
#   .\scripts\bootstrap.ps1 -Json                 # one JSON event per line on stdout, for the GUI
#
# -Json is the machine-readable contract the Setup screen consumes. Stdout carries JSON
# lines and NOTHING else — every child process (git, uv, pip, python, the runtime, the
# CLI) has both its pipes captured and re-emitted as `log` events, so no tool can leak
# into the stream. One object per line, all keys always present, null where they do not
# apply:
#
#   {"ts":1757000000000,"stage":"models","event":"progress","message":"…",
#    "done":1234567,"total":3071026671,"remedy":null}
#
#   stage    preflight | engine | codec | venv | models | layout | smoke, in that order
#   event    start | progress | log | ok | skip | fail
#   progress done/total in BYTES in the models stage, item counts elsewhere
#   skip     already satisfied and reused; the message names WHAT and WHERE
#   fail     always carries a remedy
#
# Two invariants the reader depends on: a stage that runs emits exactly one terminal event
# (ok | skip | fail), and a stage excluded by -Only emits nothing at all. `remedy` is also
# set on individual `log` lines that report one failing check inside a multi-check stage,
# so failure is `event -eq 'fail'`, never `remedy -ne $null`.

[CmdletBinding()]
param(
  # Emit the event stream above instead of human text. Same information either way: both
  # renderings come from the one Write-Event, so they cannot drift apart.
  [switch]$Json,

  # Detect and report. Mutates nothing, downloads nothing, starts nothing.
  [switch]$CheckOnly,

  # Install root. Defaults to the tree this script sits in, which is what makes a moved or
  # copied install work unchanged: everything below is written relative to it.
  [string]$InstallRoot = '',

  # An engine tree to reuse, i.e. the directory that CONTAINS webui\Irodori-TTS.
  [string]$EngineRoot = '',

  # A Hugging Face home to reuse, i.e. the directory that contains `hub`.
  [string]$HfHome = '',

  # A voice pack, or a directory of them. The layout stage copies each one into
  # data\voicepacks\<id> and registers it in data\config.json, skipping ids already there.
  [string]$VoicePacks = '',

  # One stage, or a comma-separated list, so the GUI can offer per-stage Retry.
  [string]$Only = '',

  # Pinned engine revision. NOT a moving branch: the worker talks to the engine through
  # `irodori_tts.inference_runtime`, and an engine that changes shape silently breaks
  # synthesis with no version to blame. Bump deliberately, then re-run the smoke test.
  [string]$EngineRef = 'main'
)

$ErrorActionPreference = 'Stop'

# ------------------------------------------------------------------ constants --

$StageOrder = @('preflight', 'engine', 'codec', 'venv', 'models', 'layout', 'smoke')

$StageIntro = @{
  preflight = 'checking this machine'
  engine    = 'engine source (Irodori-TTS, MIT)'
  codec     = 'audio codec (DACVAE, Apache-2.0, Meta)'
  venv      = "the engine's Python environment"
  models    = 'model weights'
  layout    = 'telling the runtime where everything is (data/runtime.json)'
  smoke     = 'proving it'
}

# Upstream sources. Both MIT/Apache-2.0 and both redistributable, but we fetch rather than
# vendor so the user gets the real upstream and its licence file.
$EngineGit = 'https://github.com/Aratako/Irodori-TTS.git'
$DacvaeGit = 'https://github.com/facebookresearch/dacvae.git'

# The three repos the Irodori backend loads. `Payload` is deliberately per-repo: the codec
# ships weights.pth, not model.safetensors, and a uniform model.safetensors probe would
# report it missing forever. `Bytes` is the repo's measured on-disk footprint and is what
# the models stage reports as `total`; sizes come from a complete cache on the reference
# machine (snapshot entries there are reparse points into blobs\ and measure 0, so every
# blob is counted exactly once).
$Models = @(
  [pscustomobject]@{
    Repo = 'Aratako/Irodori-TTS-v4.1-Small'
    Folder = 'models--Aratako--Irodori-TTS-v4.1-Small'
    Payload = 'model.safetensors'
    Bytes = 3071026671
    What = 'the TTS checkpoint'
  }
  [pscustomobject]@{
    Repo = 'sbintuitions/modernbert-ja-310m'
    Folder = 'models--sbintuitions--modernbert-ja-310m'
    Payload = 'model.safetensors'
    Bytes = 1269815225
    What = 'the Japanese text encoder'
  }
  [pscustomobject]@{
    Repo = 'Aratako/Semantic-DACVAE-Japanese-32dim'
    Folder = 'models--Aratako--Semantic-DACVAE-Japanese-32dim'
    Payload = 'weights.pth'
    Bytes = 429620105
    What = 'the 48 kHz audio codec'
  }
)

# torch + the CUDA 12.8 wheel set, measured at 4.99 GiB on the reference machine, plus the
# pip/uv download cache and the engine clone. Only charged when the venv is missing.
$VenvNeedBytes = 6442450944
# Spool, logs and the odd model revision. Charged always, so "0 GiB needed" never means
# "run with a full disk".
$HeadroomBytes = 536870912

$ProbeTorch = "import json,sys,torch;print(json.dumps({'python':'.'.join(map(str,sys.version_info[:3])),'torch':torch.__version__,'cudaAvailable':torch.cuda.is_available(),'cudaVersion':torch.version.cuda}))"
$ProbeHub = 'import huggingface_hub;print(huggingface_hub.__version__)'
# snapshot_download, not the CLI: huggingface_hub 1.x deleted the
# `huggingface_hub.commands.huggingface_cli` module the previous version of this script
# invoked (verified against 1.29.0), while snapshot_download has been public API through
# 0.x and 1.x. The cache location comes from HF_HOME/HF_HUB_CACHE in the child's
# environment — the same variables the worker sets — so provisioning proves the very
# wiring the runtime will use.
$FetchRepo = 'import sys;from huggingface_hub import snapshot_download;print(snapshot_download(sys.argv[1]))'

# ------------------------------------------------------------- event stream --

$script:Terminal = @{}
$script:ProgressOpen = $false

function Show-Human($e) {
  # The human rendering of the same event. Kept in one place so -Json and the console can
  # never report different things.
  if ($script:ProgressOpen -and $e['event'] -ne 'progress') {
    Write-Host ("`r" + (' ' * 78) + "`r") -NoNewline
    $script:ProgressOpen = $false
  }
  switch ($e['event']) {
    'start' { Write-Host "==> $($e['stage']): $($e['message'])" -ForegroundColor Cyan }
    'log' {
      Write-Host "    $($e['message'])"
      if ($null -ne $e['remedy']) { Write-Host "           fix: $($e['remedy'])" -ForegroundColor Yellow }
    }
    'progress' {
      $line = "    $($e['message'])"
      if ($null -ne $e['total'] -and [long]$e['total'] -gt 0) {
        $pct = [math]::Floor(100.0 * [long]$e['done'] / [long]$e['total'])
        $line += ("  {0:N2} / {1:N2} GiB  {2}%" -f ([long]$e['done'] / 1GB), ([long]$e['total'] / 1GB), $pct)
      }
      Write-Host ("`r" + $line.PadRight(78)) -NoNewline
      $script:ProgressOpen = $true
    }
    'ok' { Write-Host "    [OK]    $($e['message'])" -ForegroundColor Green }
    'skip' { Write-Host "    [REUSE] $($e['message'])" -ForegroundColor DarkGreen }
    'fail' {
      Write-Host "    [FAIL]  $($e['message'])" -ForegroundColor Red
      if ($null -ne $e['remedy']) { Write-Host "            fix: $($e['remedy'])" -ForegroundColor Red }
    }
  }
}

function Write-Event {
  param(
    [Parameter(Mandatory = $true)][string]$Stage,
    [Parameter(Mandatory = $true)]
    [ValidateSet('start', 'progress', 'log', 'ok', 'skip', 'fail')]
    [string]$Kind,
    [Parameter(Mandatory = $true)][string]$Message,
    $Done = $null,
    $Total = $null,
    $Remedy = $null
  )
  if (@('ok', 'skip', 'fail') -contains $Kind) { $script:Terminal[$Stage] = $Kind }
  $payload = [ordered]@{
    ts      = [DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
    stage   = $Stage
    event   = $Kind
    message = $Message
    done    = $Done
    total   = $Total
    remedy  = $Remedy
  }
  if ($Json) {
    # [Console]::Out, not Write-Output: this must be one line of exactly these bytes, with
    # no host formatting and no width-based wrapping between us and the pipe.
    [Console]::Out.WriteLine(($payload | ConvertTo-Json -Compress -Depth 4))
  }
  else { Show-Human $payload }
}

# ------------------------------------------------------------------- probes --

function Format-Arg([string]$value) {
  if ($value -eq '') { return '""' }
  if ($value -notmatch '[\s"]') { return $value }
  # Windows' own quoting rule: a run of backslashes immediately before the closing quote
  # must be doubled, or a path like `E:\tree\` eats the quote and swallows the next
  # argument. Every path we pass a child comes from Join-Path, so this is not theoretical.
  '"' + ($value -replace '(\\+)$', '$1$1') + '"'
}

function Invoke-Child {
  param(
    [string]$File,
    [string[]]$Arguments = @(),
    [string]$WorkDir = '',
    [int]$TimeoutSec = 300,
    [hashtable]$SetEnv = @{},
    [string[]]$ClearEnv = @(),
    [scriptblock]$OnTick = $null,
    [int]$TickMs = 1000
  )
  $psi = New-Object System.Diagnostics.ProcessStartInfo
  $psi.FileName = $File
  $psi.Arguments = (($Arguments | ForEach-Object { Format-Arg $_ }) -join ' ')
  $psi.UseShellExecute = $false
  $psi.CreateNoWindow = $true
  $psi.RedirectStandardOutput = $true
  $psi.RedirectStandardError = $true
  $psi.StandardOutputEncoding = New-Object System.Text.UTF8Encoding($false)
  $psi.StandardErrorEncoding = New-Object System.Text.UTF8Encoding($false)
  if ($WorkDir) { $psi.WorkingDirectory = $WorkDir }
  # Python defaults to the console codepage when its stdout is a pipe (cp936 here), which
  # turns any non-ASCII path in a traceback into mojibake in the log events.
  $psi.EnvironmentVariables['PYTHONIOENCODING'] = 'utf-8'
  foreach ($k in $SetEnv.Keys) { $psi.EnvironmentVariables[$k] = [string]$SetEnv[$k] }
  foreach ($k in $ClearEnv) { if ($psi.EnvironmentVariables.ContainsKey($k)) { $psi.EnvironmentVariables.Remove($k) } }

  $proc = New-Object System.Diagnostics.Process
  $proc.StartInfo = $psi
  [void]$proc.Start()
  # Both pipes are drained concurrently from the start: torch writes warnings to stderr,
  # and a child whose pipe fills up while nobody reads it blocks forever.
  $stdout = $proc.StandardOutput.ReadToEndAsync()
  $stderr = $proc.StandardError.ReadToEndAsync()
  $deadline = [DateTime]::UtcNow.AddSeconds($TimeoutSec)
  $timedOut = $false
  while (-not $proc.WaitForExit($TickMs)) {
    if ($OnTick) { & $OnTick }
    if ([DateTime]::UtcNow -gt $deadline) {
      $timedOut = $true
      try { $proc.Kill() } catch { }
      break
    }
  }
  [void]$proc.WaitForExit()
  $code = if ($timedOut) { 124 } else { $proc.ExitCode }
  [pscustomobject]@{
    ExitCode = $code
    Out      = $stdout.Result
    Err      = $stderr.Result
    TimedOut = $timedOut
  }
}

function Write-ChildLog([string]$Stage, [string]$What, $result) {
  # Everything a child said, as log events. In -Json mode this is the only path by which
  # child output reaches the caller, which is what keeps stdout pure JSON.
  foreach ($chunk in @($result.Out, $result.Err)) {
    if ([string]::IsNullOrWhiteSpace($chunk)) { continue }
    foreach ($line in ($chunk -split "`r?`n")) {
      if (-not [string]::IsNullOrWhiteSpace($line)) { Write-Event $Stage 'log' "$What | $($line.TrimEnd())" }
    }
  }
}

function Get-RealLength([string]$path) {
  # The HF cache stores each snapshot entry as a reparse point into blobs\, and Windows
  # PowerShell 5.1 reports Length 0 for a reparse point (measured). Opening the file gives
  # the size the download actually produced, and also proves the link is not dangling.
  try {
    $stream = [System.IO.File]::OpenRead($path)
    try { return [long]$stream.Length } finally { $stream.Dispose() }
  }
  catch { return [long]-1 }
}

function Get-DirBytes([string]$dir) {
  if (-not (Test-Path -LiteralPath $dir)) { return [long]0 }
  $m = Get-ChildItem -LiteralPath $dir -Recurse -File -Force -ErrorAction SilentlyContinue |
    Measure-Object -Property Length -Sum
  if ($null -eq $m -or $null -eq $m.Sum) { return [long]0 }
  [long]$m.Sum
}

function Get-FreeBytes([string]$path) {
  try { return [long](New-Object System.IO.DriveInfo([System.IO.Path]::GetPathRoot($path))).AvailableFreeSpace }
  catch { return [long]-1 }
}

function Format-GiB($bytes) { '{0:N2} GiB' -f ([long]$bytes / 1GB) }

function Get-FullPath([string]$path) {
  if ([string]::IsNullOrWhiteSpace($path)) { return '' }
  try { return ([System.IO.Path]::GetFullPath($path)).TrimEnd('\') } catch { return $path.TrimEnd('\') }
}

function Test-Inside([string]$child, [string]$parent) {
  if (-not $child -or -not $parent) { return $false }
  $c = (Get-FullPath $child) + '\'
  $p = (Get-FullPath $parent) + '\'
  $c.StartsWith($p, [System.StringComparison]::OrdinalIgnoreCase)
}

function Get-LayoutPath([string]$full, [string]$root) {
  # Relative inside the install (portable), absolute outside it (reuse). The runtime
  # resolves a relative path against the install root, so this choice IS the portability
  # promise; forward slashes because that is what the shipped runtime.json uses.
  if (Test-Inside $full $root) {
    return ((Get-FullPath $full).Substring((Get-FullPath $root).Length + 1) -replace '\\', '/')
  }
  Get-FullPath $full
}

function Test-Executable([string]$exe) {
  $null -ne (Get-Command $exe -ErrorAction SilentlyContinue)
}

function Test-EngineTree([string]$root) {
  if (-not $root) { return $false }
  Test-Path -LiteralPath (Join-Path $root 'webui\Irodori-TTS\irodori_tts\inference_runtime.py')
}

function Test-CodecTree([string]$root) {
  if (-not $root) { return $false }
  Test-Path -LiteralPath (Join-Path $root 'webui\dacvae\dacvae\__init__.py')
}

function Test-EnginePython([string]$exe) {
  # Spawned, never inferred from the path: a .venv directory proves nothing about whether
  # torch imports or whether this build can see the GPU. 5.2 s cold on the reference
  # machine, dominated by the torch import.
  $state = [pscustomobject]@{ Path = $exe; Ok = $false; Python = $null; Torch = $null; Cuda = $false; CudaVersion = $null; Why = $null }
  if (-not (Test-Path -LiteralPath $exe)) { $state.Why = 'not there'; return $state }
  $r = Invoke-Child -File $exe -Arguments @('-c', $ProbeTorch) -TimeoutSec 120
  if ($r.TimedOut) { $state.Why = 'the interpreter did not answer within 120 s'; return $state }
  if ($r.ExitCode -ne 0 -or [string]::IsNullOrWhiteSpace($r.Out)) {
    $tail = ($r.Err -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -Last 1)
    $state.Why = if ($tail) { $tail.Trim() } else { "exit $($r.ExitCode) with no output" }
    return $state
  }
  try { $info = $r.Out.Trim() | ConvertFrom-Json } catch { $state.Why = 'the probe printed something that is not JSON'; return $state }
  $state.Python = $info.python
  $state.Torch = $info.torch
  $state.Cuda = [bool]$info.cudaAvailable
  $state.CudaVersion = $info.cudaVersion
  $state.Ok = $true
  $state
}

function Find-Python {
  param(
    [Parameter(Mandatory = $true)][string]$Probe,
    [string[]]$ProbeArgs = @()
  )
  # The first interpreter that can run $Probe, or $null. The engine's own venv is tried
  # first because it is the one whose contents this script guarantees; `python` on PATH is
  # the fallback for a stage that only needs the standard library. Paths go through argv,
  # never through the -c source: argv is handed to CPython as wide characters, so a path
  # with non-ASCII in it survives whatever the console codepage is.
  foreach ($cand in @($script:Inv.Python, 'python')) {
    if (-not $cand) { continue }
    if ($cand -eq 'python') { if (-not (Test-Executable 'python')) { continue } }
    elseif (-not (Test-Path -LiteralPath $cand)) { continue }
    $allArgs = @('-c', $Probe) + $ProbeArgs
    $r = Invoke-Child -File $cand -Arguments $allArgs -TimeoutSec 120
    if ($r.ExitCode -eq 0) { return [pscustomobject]@{ Path = $cand; Out = $r.Out.Trim(); Err = $r.Err } }
  }
  $null
}

function Get-ModelState($model, [string]$hub) {
  $dir = Join-Path $hub $model.Folder
  # `Model` is the catalogue entry this state was probed from, so a re-probe after a
  # download reads the expected size from the catalogue rather than from the state's own
  # `Bytes`, which is what is on disk right now.
  $state = [pscustomobject]@{
    Repo = $model.Repo; What = $model.What; Model = $model
    Total = [long]$model.Bytes; Dir = $dir; Present = $false; Path = $null
    Bytes = (Get-DirBytes $dir); Why = $null
  }
  $snapshots = Join-Path $dir 'snapshots'
  if (-not (Test-Path -LiteralPath $snapshots)) {
    $state.Why = "no $($model.Folder)\snapshots in the cache"
    return $state
  }
  foreach ($snap in (Get-ChildItem -LiteralPath $snapshots -Directory -ErrorAction SilentlyContinue)) {
    $payload = Join-Path $snap.FullName $model.Payload
    if (-not (Test-Path -LiteralPath $payload)) { continue }
    # hf links a snapshot entry only after its blob is complete, so existence is the
    # completeness signal; the size guards the one case existence does not cover, a
    # dangling link left behind by a half-copied cache.
    if ((Get-RealLength $payload) -lt 1MB) {
      $state.Why = "$($model.Payload) is there but unreadable or a stub"
      continue
    }
    $state.Present = $true
    $state.Path = $payload
    $state.Why = $null
    return $state
  }
  if (-not $state.Why) { $state.Why = "no snapshot holds $($model.Payload)" }
  $state
}

function Get-PackState([string]$path) {
  $state = [pscustomobject]@{ Id = (Split-Path $path -Leaf); Path = $path; Kind = $null; Ok = $false; Why = $null }
  if (Test-Path -LiteralPath $path -PathType Container) {
    $hasConfig = Test-Path -LiteralPath (Join-Path $path 'adapter_config.json')
    $hasWeights = Test-Path -LiteralPath (Join-Path $path 'adapter_model.safetensors')
    if ($hasConfig -and $hasWeights) { $state.Kind = 'lora-adapter'; $state.Ok = $true; return $state }
    $missing = @()
    if (-not $hasConfig) { $missing += 'adapter_config.json' }
    if (-not $hasWeights) { $missing += 'adapter_model.safetensors' }
    $state.Why = "a directory without $($missing -join ' and ')"
    return $state
  }
  if ($path.ToLowerInvariant().EndsWith('.speaker.safetensors')) {
    $state.Id = (Split-Path $path -Leaf) -replace '\.speaker\.safetensors$', ''
    $state.Kind = 'speaker-embedding'
    $state.Ok = $true
    return $state
  }
  # The engine refuses an embedding without that suffix, by name
  # ("Speaker Inversion embeddings must use the '.speaker.safetensors' suffix"), so a
  # renamed file is not a pack no matter what it contains.
  $state.Why = 'not a .speaker.safetensors file and not a LoRA directory'
  $state
}

function Get-PackStates([string]$path) {
  if (-not $path -or -not (Test-Path -LiteralPath $path)) { return @() }
  $self = Get-PackState $path
  if ($self.Ok) { return @($self) }
  if (-not (Test-Path -LiteralPath $path -PathType Container)) { return @($self) }
  @(Get-ChildItem -LiteralPath $path -Force -ErrorAction SilentlyContinue |
    Where-Object { -not $_.Name.StartsWith('.') } |
    ForEach-Object { Get-PackState $_.FullName })
}

# --------------------------------------------------------------- resolution --

function Read-RuntimeJson([string]$dataDir) {
  $file = Join-Path $dataDir 'runtime.json'
  if (-not (Test-Path -LiteralPath $file)) { return $null }
  try { return (Get-Content -LiteralPath $file -Raw -Encoding UTF8 | ConvertFrom-Json) }
  catch { return $null }
}

function Resolve-Against([string]$value, [string]$root) {
  # Same rule the runtime applies: a relative path in runtime.json resolves against the
  # install root, an absolute one is honoured as written.
  if ([string]::IsNullOrWhiteSpace($value)) { return '' }
  if ([System.IO.Path]::IsPathRooted($value)) { return Get-FullPath $value }
  Get-FullPath (Join-Path $root $value)
}

function Resolve-Inventory {
  $inv = [pscustomobject]@{
    Root = $script:Root; DataDir = $script:DataDir
    EngineRoot = ''; EngineWhy = ''; EngineOk = $false; CodecOk = $false; EngineReused = $false
    Python = ''; PythonWhy = ''; PythonState = $null
    HfHome = ''; Hub = ''; HfWhy = ''; HfReused = $false; HfNote = $null
    Models = @(); MissingBytes = [long]0
    Worker = ''; Packs = @(); PackSource = ''
    Runtime = $script:RuntimeFile
  }
  $rt = $script:RuntimeFile

  # --- engine tree
  $candidates = New-Object System.Collections.ArrayList
  if ($EngineRoot) {
    [void]$candidates.Add(@{ Path = (Get-FullPath $EngineRoot); Why = 'the -EngineRoot you passed' })
  }
  else {
    if ($rt -and $rt.ttsRoot) {
      [void]$candidates.Add(@{ Path = (Resolve-Against $rt.ttsRoot $script:Root); Why = 'ttsRoot in data/runtime.json' })
    }
    [void]$candidates.Add(@{ Path = (Get-FullPath (Join-Path $script:Root 'runtime\engine')); Why = 'the packaged layout' })
  }
  foreach ($c in $candidates) {
    if (Test-EngineTree $c.Path) {
      $inv.EngineRoot = $c.Path
      $inv.EngineWhy = $c.Why
      $inv.EngineOk = $true
      break
    }
  }
  if (-not $inv.EngineOk) {
    # Nothing found: the last candidate is where a clone would go.
    $inv.EngineRoot = $candidates[$candidates.Count - 1].Path
    $inv.EngineWhy = $candidates[$candidates.Count - 1].Why
  }
  $inv.EngineReused = -not (Test-Inside $inv.EngineRoot $script:Root)
  $inv.CodecOk = Test-CodecTree $inv.EngineRoot

  # --- interpreter
  $pys = New-Object System.Collections.ArrayList
  if ($rt -and $rt.ttsPython) {
    [void]$pys.Add(@{ Path = (Resolve-Against $rt.ttsPython $script:Root); Why = 'ttsPython in data/runtime.json' })
  }
  [void]$pys.Add(@{ Path = (Join-Path $inv.EngineRoot 'env\Scripts\python.exe'); Why = "the engine tree's env\" })
  [void]$pys.Add(@{ Path = (Join-Path $inv.EngineRoot 'webui\Irodori-TTS\.venv\Scripts\python.exe'); Why = "uv sync's .venv" })
  [void]$pys.Add(@{ Path = (Join-Path $script:Root 'runtime\python\Scripts\python.exe'); Why = 'the packaged virtualenv' })
  [void]$pys.Add(@{ Path = (Join-Path $script:Root 'runtime\python\python.exe'); Why = 'the packaged embeddable interpreter' })
  foreach ($c in $pys) {
    if (-not (Test-Path -LiteralPath $c.Path)) { continue }
    $state = Test-EnginePython $c.Path
    if ($state.Ok) {
      $inv.Python = $c.Path
      $inv.PythonWhy = $c.Why
      $inv.PythonState = $state
      break
    }
    if (-not $inv.PythonState) { $inv.PythonState = $state; $inv.PythonWhy = $c.Why }
  }
  if (-not $inv.Python) {
    # Where a new venv would go. uv sync puts .venv inside the repo, which is upstream's
    # own layout, so that is the destination even when env\ is the one being reused.
    $inv.Python = Join-Path $inv.EngineRoot 'webui\Irodori-TTS\.venv\Scripts\python.exe'
  }

  # --- model cache. `$hfDest` is where a download would go, which is NOT simply the last
  # candidate: %USERPROFILE%\.cache\huggingface is worth REUSING when it already holds these
  # repos, but making it the destination of a fresh install would put 4.44 GiB in the user
  # profile and write an absolute path to it, so the tree could no longer be zipped and
  # moved. A fresh install always fills its own models\huggingface.
  $hfs = New-Object System.Collections.ArrayList
  $hfDest = $null
  if ($HfHome) {
    [void]$hfs.Add(@{ Home = (Get-FullPath $HfHome); Hub = (Join-Path (Get-FullPath $HfHome) 'hub'); Why = 'the -HfHome you passed' })
    $hfDest = $hfs[0]
  }
  else {
    if ($rt -and $rt.hfHome) {
      $h = Resolve-Against $rt.hfHome $script:Root
      [void]$hfs.Add(@{ Home = $h; Hub = (Join-Path $h 'hub'); Why = 'hfHome in data/runtime.json' })
    }
    # The engine tree's own cache. Not a guess and not a crawl: when runtime.json carries no
    # hfHome, the worker itself falls back to <ttsRoot>\model\huggingface (worker.py's
    # HF_HOME setdefault), so weights there are already the ones this install loads. Missing
    # this candidate is exactly how a machine with a full cache gets told to download
    # 4.44 GiB again.
    if ($inv.EngineRoot) {
      $h = Get-FullPath (Join-Path $inv.EngineRoot 'model\huggingface')
      [void]$hfs.Add(@{ Home = $h; Hub = (Join-Path $h 'hub'); Why = "the engine tree's own cache, which is where the worker looks by default" })
    }
    if ($env:HF_HOME) {
      $h = Get-FullPath $env:HF_HOME
      [void]$hfs.Add(@{ Home = $h; Hub = (Join-Path $h 'hub'); Why = 'HF_HOME in the environment' })
    }
    if ($env:HF_HUB_CACHE) {
      $hub = Get-FullPath $env:HF_HUB_CACHE
      if ((Split-Path $hub -Leaf).ToLowerInvariant() -eq 'hub') {
        [void]$hfs.Add(@{ Home = (Split-Path $hub -Parent); Hub = $hub; Why = 'HF_HUB_CACHE in the environment' })
      }
      else {
        # runtime.json carries an HF home, and the worker derives HF_HUB_CACHE as
        # <hfHome>\hub. A cache whose parent is not its home cannot be expressed there, so
        # say so rather than writing a path the worker will not look in.
        $inv.HfNote = "HF_HUB_CACHE is $hub, which is not named 'hub'. The worker derives the cache as <hfHome>\hub, so this one cannot be expressed in runtime.json; pass -HfHome with a directory whose 'hub' subdirectory is the cache."
      }
    }
    $h = Get-FullPath (Join-Path $script:Root 'models\huggingface')
    $hfDest = @{ Home = $h; Hub = (Join-Path $h 'hub'); Why = "the packaged layout, which is where a fresh install downloads to" }
    [void]$hfs.Add($hfDest)
    if ($env:USERPROFILE) {
      $h = Get-FullPath (Join-Path $env:USERPROFILE '.cache\huggingface')
      [void]$hfs.Add(@{ Home = $h; Hub = (Join-Path $h 'hub'); Why = "Hugging Face's own default" })
    }
  }
  foreach ($c in $hfs) {
    $states = @($Models | ForEach-Object { Get-ModelState $_ $c.Hub })
    if (@($states | Where-Object { $_.Present }).Count -gt 0) {
      $inv.HfHome = $c.Home; $inv.Hub = $c.Hub; $inv.HfWhy = $c.Why; $inv.Models = $states
      break
    }
  }
  if (-not $inv.Hub) {
    # One cache, always: the worker gets exactly one HF_HOME, so weights spread across two
    # caches would leave it unable to load.
    $inv.HfHome = $hfDest.Home; $inv.Hub = $hfDest.Hub; $inv.HfWhy = $hfDest.Why
    $inv.Models = @($Models | ForEach-Object { Get-ModelState $_ $hfDest.Hub })
  }
  $inv.HfReused = -not (Test-Inside $inv.HfHome $script:Root)
  $inv.MissingBytes = [long]0
  foreach ($m in $inv.Models) { if (-not $m.Present) { $inv.MissingBytes += [long]($m.Total - $m.Bytes) } }
  if ($inv.MissingBytes -lt 0) { $inv.MissingBytes = [long]0 }

  # --- worker script. Shipped with the tree, never fetched; the packaged location wins
  # over the dev checkout so an install never runs a stale copy from a source tree.
  foreach ($w in @((Join-Path $script:Root 'runtime\worker\irodori\worker.py'), (Join-Path $script:Root 'worker\irodori\worker.py'))) {
    if (Test-Path -LiteralPath $w) { $inv.Worker = Get-FullPath $w; break }
  }

  # --- voice packs
  if ($VoicePacks) { $inv.PackSource = Get-FullPath $VoicePacks }
  else { $inv.PackSource = Get-FullPath (Join-Path $script:DataDir 'voicepacks') }
  $inv.Packs = @(Get-PackStates $inv.PackSource)
  $inv
}

# --------------------------------------------------------------- the stages --

function Invoke-Stage([string]$Name, [scriptblock]$Body) {
  if (-not $script:Selected.Contains($Name)) { return }
  Write-Event $Name 'start' $StageIntro[$Name]
  try { & $Body }
  catch {
    # A stage that throws must not take the run down: the later stages are independent and
    # the GUI offers per-stage Retry.
    if (-not $script:Terminal.ContainsKey($Name)) {
      Write-Event $Name 'fail' "unexpected error: $($_.Exception.Message)" $null $null 'that is a bug in bootstrap.ps1 or a broken environment; re-run this one stage with -Only to see it again'
    }
  }
  if (-not $script:Terminal.ContainsKey($Name)) {
    Write-Event $Name 'fail' 'the stage produced no result' $null $null "re-run with -Only $Name and report it: every stage owes exactly one ok, skip or fail"
  }
}

function Invoke-Preflight {
  $inv = $script:Inv
  $problems = New-Object System.Collections.ArrayList

  $os = [Environment]::OSVersion.Version
  if ($os.Major -ge 10) { Write-Event 'preflight' 'log' "Windows $($os.Major).$($os.Build)" }
  else {
    $why = "Windows $($os.Major) is too old"
    Write-Event 'preflight' 'log' $why $null $null 'voice-core targets Windows 10/11'
    [void]$problems.Add($why)
  }

  # Charged per volume that will actually receive bytes: reusing a cache on another drive
  # must not demand the space on this one.
  $needs = @{}
  if ($inv.MissingBytes -gt 0) {
    $root = [System.IO.Path]::GetPathRoot($inv.Hub)
    if (-not $needs.ContainsKey($root)) { $needs[$root] = [long]0 }
    $needs[$root] += [long]$inv.MissingBytes
  }
  if (-not ($inv.PythonState -and $inv.PythonState.Ok)) {
    $root = [System.IO.Path]::GetPathRoot($inv.Python)
    if (-not $needs.ContainsKey($root)) { $needs[$root] = [long]0 }
    $needs[$root] += [long]$VenvNeedBytes
  }
  $root = [System.IO.Path]::GetPathRoot($script:Root)
  if (-not $needs.ContainsKey($root)) { $needs[$root] = [long]0 }
  $needs[$root] += [long]$HeadroomBytes
  foreach ($vol in $needs.Keys) {
    $free = Get-FreeBytes $vol
    $need = [long]$needs[$vol]
    if ($free -lt 0) { Write-Event 'preflight' 'log' "disk $vol : cannot read free space; skipping the check"; continue }
    if ($free -ge $need) { Write-Event 'preflight' 'log' "disk $vol $(Format-GiB $free) free, needs $(Format-GiB $need)" }
    else {
      $why = "disk $vol has $(Format-GiB $free) free but needs $(Format-GiB $need)"
      Write-Event 'preflight' 'log' $why $null $null "free up space, or point at another volume with -InstallRoot / -HfHome"
      [void]$problems.Add($why)
    }
  }

  if (Test-Executable 'nvidia-smi') {
    $r = Invoke-Child -File 'nvidia-smi' -Arguments @('--query-gpu=name,memory.total,driver_version', '--format=csv,noheader') -TimeoutSec 30
    $gpu = ($r.Out -split "`r?`n" | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | Select-Object -First 1)
    if ($gpu) { Write-Event 'preflight' 'log' "GPU $($gpu.Trim())" }
    else { Write-Event 'preflight' 'log' 'nvidia-smi ran but reported no GPU' $null $null 'the Irodori backend runs the model on CUDA; there is no CPU path' }
  }
  else {
    $why = 'no nvidia-smi'
    Write-Event 'preflight' 'log' $why $null $null 'install an NVIDIA driver: the Irodori backend runs the model on CUDA and a CPU path is not implemented'
    [void]$problems.Add($why)
  }

  # git and uv are only needed for what is still missing. Demanding them from a user who
  # is reusing a complete tree is the kind of false blocker this rewrite exists to remove.
  $needGit = -not ($inv.EngineOk -and $inv.CodecOk)
  $needVenv = -not ($inv.PythonState -and $inv.PythonState.Ok)
  if (Test-Executable 'git') {
    $r = Invoke-Child -File 'git' -Arguments @('--version') -TimeoutSec 30
    Write-Event 'preflight' 'log' ($r.Out.Trim())
  }
  elseif ($needGit) {
    $why = 'no git, and the engine source is not here yet'
    Write-Event 'preflight' 'log' $why $null $null 'install Git for Windows (https://git-scm.com/download/win), or pass -EngineRoot pointing at a tree you already have'
    [void]$problems.Add($why)
  }
  else { Write-Event 'preflight' 'log' 'no git, but the engine source is already here, so nothing needs cloning' }

  if (Test-Executable 'uv') {
    $r = Invoke-Child -File 'uv' -Arguments @('--version') -TimeoutSec 30
    Write-Event 'preflight' 'log' ($r.Out.Trim())
  }
  elseif ($needVenv) {
    Write-Event 'preflight' 'log' 'no uv; the venv stage will fall back to python -m venv + pip' $null $null 'install uv (https://astral.sh/uv) for the path upstream tests: it resolves the CUDA wheel index itself and brings its own 3.12'
  }
  else { Write-Event 'preflight' 'log' 'no uv, but the interpreter is already usable' }

  foreach ($v in @('HTTPS_PROXY', 'HTTP_PROXY')) {
    $value = [Environment]::GetEnvironmentVariable($v)
    if ($value) { Write-Event 'preflight' 'log' "$v is set: $value" }
  }

  $config = Join-Path $script:DataDir 'config.json'
  if ((Test-Path -LiteralPath $config) -and (Select-String -LiteralPath $config -Pattern '"id"' -Quiet)) {
    Write-Event 'preflight' 'log' 'at least one voice pack is registered in data/config.json'
  }
  else {
    Write-Event 'preflight' 'log' 'no voice pack registered, so the runtime will start but cannot speak' $null $null 'the Setup screen registers a pack for you; from the command line see docs/training-a-voice.md'
  }

  if ($problems.Count -eq 0) { Write-Event 'preflight' 'ok' 'this machine can run voice-core' }
  else {
    Write-Event 'preflight' 'fail' "$($problems.Count) blocking problem(s): $($problems -join '; ')" $null $null 'each line above carries its own fix; the later stages still ran, so re-run this one after fixing them'
  }
}

function Invoke-EngineStage {
  $inv = $script:Inv
  $repo = Join-Path $inv.EngineRoot 'webui\Irodori-TTS'
  if ($inv.EngineOk) {
    $what = if ($inv.EngineReused) {
      "reused the engine tree at $($inv.EngineRoot) (found via $($inv.EngineWhy)); nothing cloned"
    }
    else { "the engine tree is already at $($inv.EngineRoot)" }
    Write-Event 'engine' 'skip' $what
    return
  }
  $marker = 'webui\Irodori-TTS\irodori_tts\inference_runtime.py'
  if ($CheckOnly) {
    Write-Event 'engine' 'fail' "no engine tree: $($inv.EngineRoot) has no $marker" $null $null "run without -CheckOnly to clone it, or pass -EngineRoot <dir> where <dir>\$marker exists"
    return
  }
  if (-not (Test-Executable 'git')) {
    Write-Event 'engine' 'fail' 'git is not installed, so the engine cannot be cloned' $null $null 'install Git for Windows (https://git-scm.com/download/win), or pass -EngineRoot pointing at a tree you already have'
    return
  }
  if (Test-Path -LiteralPath $repo) {
    Write-Event 'engine' 'fail' "$repo exists but holds no $marker" $null $null 'move that directory aside and re-run, or point -EngineRoot at a complete tree; overwriting it here could destroy work'
    return
  }
  [void](New-Item -ItemType Directory -Force -Path (Split-Path $repo -Parent))
  Write-Event 'engine' 'progress' "cloning $EngineGit at $EngineRef" 0 1
  $r = Invoke-Child -File 'git' -Arguments @('clone', '--depth', '1', '--branch', $EngineRef, $EngineGit, $repo) -TimeoutSec 1800
  Write-ChildLog 'engine' 'git' $r
  if (Test-EngineTree $inv.EngineRoot) { Write-Event 'engine' 'ok' "cloned $EngineRef into $repo" 1 1 }
  else { Write-Event 'engine' 'fail' "clone finished with exit $($r.ExitCode) but $marker is not there" $null $null 'read the git lines above; a proxy or a partial clone is the usual reason, and re-running resumes nothing — delete the directory first' }
}

function Invoke-CodecStage {
  $inv = $script:Inv
  $dir = Join-Path $inv.EngineRoot 'webui\dacvae'
  if (Test-CodecTree $inv.EngineRoot) {
    Write-Event 'codec' 'skip' "reused the DACVAE checkout at $dir; nothing cloned"
    return
  }
  if ($CheckOnly) {
    Write-Event 'codec' 'fail' "no DACVAE checkout: $dir has no dacvae\__init__.py" $null $null 'run without -CheckOnly to clone it; the worker puts this directory on sys.path and the engine imports it as `dacvae`'
    return
  }
  if (-not (Test-Executable 'git')) {
    Write-Event 'codec' 'fail' 'git is not installed, so DACVAE cannot be cloned' $null $null 'install Git for Windows (https://git-scm.com/download/win)'
    return
  }
  if (Test-Path -LiteralPath $dir) {
    Write-Event 'codec' 'fail' "$dir exists but holds no dacvae\__init__.py" $null $null 'move that directory aside and re-run'
    return
  }
  [void](New-Item -ItemType Directory -Force -Path (Split-Path $dir -Parent))
  Write-Event 'codec' 'progress' "cloning $DacvaeGit" 0 1
  $r = Invoke-Child -File 'git' -Arguments @('clone', '--depth', '1', $DacvaeGit, $dir) -TimeoutSec 1800
  Write-ChildLog 'codec' 'git' $r
  if (Test-CodecTree $inv.EngineRoot) { Write-Event 'codec' 'ok' "cloned into $dir" 1 1 }
  else { Write-Event 'codec' 'fail' "clone finished with exit $($r.ExitCode) but dacvae\__init__.py is not there" $null $null 'read the git lines above, then delete the directory and re-run' }
}

function Invoke-VenvStage {
  $inv = $script:Inv
  if ($inv.PythonState -and $inv.PythonState.Ok) {
    $s = $inv.PythonState
    if (-not $s.Cuda) {
      # Before the terminal event, so a reader that closes the stage row on ok/skip/fail
      # still sees it. An interpreter with torch but no CUDA is reused-and-broken, not fine.
      Write-Event 'venv' 'log' 'that interpreter cannot see the GPU' $null $null 'a CPU-only torch build cannot run this engine; reinstall torch from https://download.pytorch.org/whl/cu128 inside that environment'
    }
    $cuda = if ($s.Cuda) { "CUDA $($s.CudaVersion) is visible" } else { 'but torch.cuda.is_available() is FALSE' }
    Write-Event 'venv' 'skip' "reused $($s.Path) (found via $($inv.PythonWhy)): Python $($s.Python), torch $($s.Torch), $cuda"
    return
  }
  $why = if ($inv.PythonState) { "$($inv.PythonState.Path): $($inv.PythonState.Why)" } else { 'no interpreter to probe' }
  if ($CheckOnly) {
    Write-Event 'venv' 'fail' "no interpreter reports torch: $why" $null $null "run without -CheckOnly to build one with uv (about $(Format-GiB $VenvNeedBytes) of disk: a 5.0 GiB virtualenv plus the wheel cache), or pass -EngineRoot pointing at a tree whose env\Scripts\python.exe already has torch"
    return
  }
  if (-not $inv.EngineOk) {
    Write-Event 'venv' 'fail' 'the engine source is not here, and its pyproject.toml is what pins the dependency set' $null $null 'fix the engine stage first, then re-run with -Only venv'
    return
  }
  $repo = Join-Path $inv.EngineRoot 'webui\Irodori-TTS'
  $target = Join-Path $repo '.venv\Scripts\python.exe'
  if (Test-Executable 'uv') {
    # Upstream's own instruction (README: `uv sync --extra cu128`): it creates .venv inside
    # the repo, pins the interpreter and picks the CUDA 12.8 wheel set.
    Write-Event 'venv' 'progress' 'uv sync --extra cu128 (several GB of wheels)' 0 1
    $r = Invoke-Child -File 'uv' -Arguments @('sync', '--extra', 'cu128') -WorkDir $repo -TimeoutSec 5400
    Write-ChildLog 'venv' 'uv' $r
  }
  else {
    Write-Event 'venv' 'progress' 'python -m venv + pip (no uv)' 0 1
    $r = Invoke-Child -File 'python' -Arguments @('-m', 'venv', (Join-Path $repo '.venv')) -TimeoutSec 900
    Write-ChildLog 'venv' 'venv' $r
    if (Test-Path -LiteralPath $target) {
      # The CUDA wheels are not on PyPI, hence the extra index.
      $r = Invoke-Child -File $target -Arguments @('-m', 'pip', 'install', '-e', $repo, '--extra-index-url', 'https://download.pytorch.org/whl/cu128') -TimeoutSec 5400
      Write-ChildLog 'venv' 'pip' $r
    }
  }
  $state = Test-EnginePython $target
  if ($state.Ok) {
    $script:Inv.Python = $target
    $script:Inv.PythonState = $state
    $script:Inv.PythonWhy = 'built by this run'
    $cuda = if ($state.Cuda) { "CUDA $($state.CudaVersion)" } else { 'NO CUDA' }
    Write-Event 'venv' 'ok' "$($target): Python $($state.Python), torch $($state.Torch), $cuda" 1 1
  }
  else {
    Write-Event 'venv' 'fail' "the environment was built but $target does not report torch: $($state.Why)" $null $null "run 'uv sync --extra cu128' inside $repo by hand and read its output; the wheel resolution is the part that fails"
  }
}

function Invoke-ModelsStage {
  $inv = $script:Inv
  $present = @($inv.Models | Where-Object { $_.Present })
  $missing = @($inv.Models | Where-Object { -not $_.Present })
  foreach ($m in $present) {
    Write-Event 'models' 'log' "$($m.Repo) — $($m.What) — already in $($inv.Hub), $(Format-GiB $m.Bytes) not downloaded again"
  }
  if ($missing.Count -eq 0) {
    $saved = [long]0
    foreach ($m in $present) { $saved += [long]$m.Bytes }
    Write-Event 'models' 'skip' "all $($present.Count) repos are in $($inv.Hub) (found via $($inv.HfWhy)); $(Format-GiB $saved) not downloaded" $saved $saved
    return
  }
  if ($inv.HfNote) { Write-Event 'models' 'log' $inv.HfNote }
  $need = [long]0
  foreach ($m in $missing) { $need += [long]$m.Total }
  if ($CheckOnly) {
    $names = ($missing | ForEach-Object { "$($_.Repo) ($($_.Why))" }) -join '; '
    Write-Event 'models' 'fail' "$($missing.Count) of $($inv.Models.Count) repos are missing from $($inv.Hub): $names" $need $need "run without -CheckOnly to fetch $(Format-GiB $need), or pass -HfHome <dir> where <dir>\hub\models--Aratako--Irodori-TTS-v4.1-Small\snapshots\*\model.safetensors already exists"
    return
  }

  # Any python with huggingface_hub will do, and the engine's own venv is the one that is
  # guaranteed to have it — which is why venv runs before models.
  $found = Find-Python $ProbeHub
  if (-not $found) {
    Write-Event 'models' 'fail' 'no interpreter here can import huggingface_hub, so nothing can be downloaded' $null $null 'run the venv stage first (-Only venv): the engine venv brings huggingface_hub with it'
    return
  }
  $py = $found.Path
  Write-Event 'models' 'log' "downloading with $py, huggingface_hub $($found.Out)"

  [void](New-Item -ItemType Directory -Force -Path $inv.Hub)
  $childEnv = @{ HF_HOME = $inv.HfHome; HF_HUB_CACHE = $inv.Hub; HF_HUB_DISABLE_PROGRESS_BARS = '1' }
  $failed = 0
  $fetched = [long]0
  foreach ($m in $missing) {
    Write-Event 'models' 'log' "$($m.Repo) — $($m.What) — $($m.Why), fetching about $(Format-GiB $m.Total)"
    $dir = $m.Dir
    $total = [long]$m.Total
    # Byte progress by polling the repo's cache directory once a second, rather than by
    # parsing huggingface_hub's progress bars. Deliberate: this cache is already on Xet
    # (there is an xet\ directory beside hub\), Xet's chunk-level reporting is not a stable
    # API, and a reporting change must never be able to break the download itself. What we
    # report is therefore bytes LANDED ON DISK for this repo — with Xet that can lag the
    # wire by the chunk cache — at one-second granularity, against the repo's measured
    # size. HF_HUB_DISABLE_PROGRESS_BARS keeps its bars out of the captured log.
    $tick = {
      $done = Get-DirBytes $dir
      $cap = if ($done -gt $total) { $done } else { $total }
      Write-Event 'models' 'progress' "$($m.Repo)" $done $cap
    }.GetNewClosure()
    $r = Invoke-Child -File $py -Arguments @('-c', $FetchRepo, $m.Repo) -TimeoutSec 7200 -SetEnv $childEnv -ClearEnv @('HF_HUB_OFFLINE', 'TRANSFORMERS_OFFLINE') -OnTick $tick
    Write-ChildLog 'models' 'hf' $r
    # Verified by looking, not by trusting the exit code: a 0 from a resumed transfer that
    # wrote nothing usable would otherwise be reported as success.
    $after = Get-ModelState $m.Model $inv.Hub
    if ($after.Present) {
      $fetched += [long]$after.Bytes
      Write-Event 'models' 'log' "$($m.Repo) is complete: $($after.Path)"
    }
    else {
      $failed++
      $reason = if ($r.TimedOut) { 'timed out after 2 h' } else { "exit $($r.ExitCode)" }
      Write-Event 'models' 'log' "$($m.Repo) is still incomplete ($reason): $($after.Why)" $null $null 'check the network or the proxy and re-run with -Only models; completed files are kept, so it resumes'
    }
  }
  if ($failed -eq 0) { Write-Event 'models' 'ok' "$($missing.Count) repo(s) fetched into $($inv.Hub), $(Format-GiB $fetched)" $fetched $fetched }
  else { Write-Event 'models' 'fail' "$failed of $($missing.Count) repo(s) did not complete" $fetched $need 'the per-repo lines above say which; re-run with -Only models, the cache is resumable' }
}

# Registration is COPY, not point, and it goes through scripts/training/install_pack.py
# rather than through a second implementation of the same edit. Two reasons for each half:
#
# copy   the reason the engine tree is pointed at does not apply to a pack. A LoRA adapter is
#        100.8 MiB here (measured, trainer_state.pt excluded), against 4.44 GiB of weights and
#        a 5.0 GiB venv, and install_pack.py writes `voicepacks/<id>` — relative to the data
#        dir — so a registered install stays zippable and the training corpus can be deleted.
# reuse  config.json is JSONC written for a human: comments, trailing commas, CRLF, maybe a
#        BOM. install_pack.py splices the voicePacks array surgically and re-parses the result
#        before writing. A PowerShell reimplementation would be a second dialect to keep
#        correct, and the first bug in it would silently delete somebody's comments.
#
# Idempotence is ours, not its: install_pack.py appends unconditionally, so a pack whose id is
# already in voicePacks is skipped here before anything is copied or spliced. `--force` is
# deliberately NOT passed, so a directory somebody put in data\voicepacks by hand is reported
# rather than overwritten.
function Register-Packs {
  $inv = $script:Inv
  $usable = @($inv.Packs | Where-Object { $_.Ok })
  if ($usable.Count -eq 0) { return '' }
  Write-Event 'layout' 'log' "$($usable.Count) registerable pack(s) under $($inv.PackSource)"

  $installPack = Join-Path $script:Root 'scripts\training\install_pack.py'
  $config = Join-Path $script:DataDir 'config.json'
  if (-not (Test-Path -LiteralPath $installPack)) {
    Write-Event 'layout' 'log' "$($usable.Count) pack(s) found, none registered: $installPack is not in this tree" $null $null 'reinstall, or run from a checkout that has scripts/training/install_pack.py'
    return "; $($usable.Count) pack(s) NOT registered"
  }
  if (-not (Test-Path -LiteralPath $config)) {
    Write-Event 'layout' 'log' "$($usable.Count) pack(s) found, none registered: there is no $config to register them in" $null $null 'start the runtime once so it creates the data directory, then re-run with -Only layout'
    return "; $($usable.Count) pack(s) NOT registered"
  }

  # One spawn does both jobs: it proves an interpreter can drive install_pack.py, and it
  # returns the ids already in voicePacks, read through install_pack's own JSONC pass because
  # PowerShell's ConvertFrom-Json cannot parse a file with comments in it.
  $probe = "import sys,json;sys.path.insert(0,sys.argv[1]);import install_pack;print(json.dumps([p.get('id') for p in json.loads(install_pack.to_json(open(sys.argv[2],encoding='utf-8-sig').read())).get('voicePacks',[])]))"
  $found = Find-Python $probe @((Split-Path $installPack -Parent), $config)
  if (-not $found) {
    Write-Event 'layout' 'log' "$($usable.Count) pack(s) found, none registered: no interpreter here can read data/config.json" $null $null 'run the venv stage first (-Only venv), or put a python 3.9+ on PATH: registration is a Python script'
    return "; $($usable.Count) pack(s) NOT registered"
  }
  $known = @()
  try { $known = @($found.Out | ConvertFrom-Json) } catch { $known = @() }

  $packsDir = Join-Path $script:DataDir 'voicepacks'
  $registered = 0
  $already = 0
  $blocked = 0
  $would = 0
  foreach ($p in $usable) {
    if ($known -contains $p.Id) {
      $already++
      Write-Event 'layout' 'log' "voice pack $($p.Id) is already registered; left alone"
      continue
    }
    if ($p.Id -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]*$') {
      # install_pack.py enforces this too, but its id becomes a directory name and an API
      # identifier, so saying it here costs no spawn.
      $blocked++
      Write-Event 'layout' 'log' "cannot register $($p.Path): the id $($p.Id) is not a plain name" $null $null 'rename it to letters, digits, dot, dash or underscore, then re-run with -Only layout'
      continue
    }
    if (Test-Inside $p.Path $packsDir) {
      # install_pack.py copies source to data\voicepacks\<id>, and here the source IS that
      # target. Copying a directory onto itself is how a pack gets destroyed, so this reports
      # the one command that registers it in place instead of risking it.
      $blocked++
      Write-Event 'layout' 'log' "voice pack $($p.Id) is already in data\voicepacks but nothing registers it" $null $null "register it without copying: $($found.Path) $installPack --pack $($p.Path) --id $($p.Id) --data-dir $($script:DataDir)"
      continue
    }
    if ($CheckOnly) {
      $would++
      Write-Event 'layout' 'log' "would register voice pack $($p.Id) ($($p.Kind)) from $($p.Path)"
      continue
    }
    $r = Invoke-Child -File $found.Path -Arguments @($installPack, '--pack', $p.Path, '--id', $p.Id, '--data-dir', $script:DataDir) -TimeoutSec 900
    Write-ChildLog 'layout' 'install_pack' $r
    if ($r.ExitCode -eq 0) {
      $registered++
      # The runtime re-reads voicePacks on mtime change, so the pack is speakable now without
      # restarting anything.
      Write-Event 'layout' 'progress' "registered voice pack $($p.Id) ($($p.Kind))" $registered $usable.Count
    }
    else {
      $blocked++
      $reason = if ($r.TimedOut) { 'timed out' } else { "exit $($r.ExitCode)" }
      Write-Event 'layout' 'log' "voice pack $($p.Id) was not registered ($reason)" $null $null 'the install_pack lines above carry its reason verbatim; data/config.json is only written after the edited copy re-parses, so it is intact'
    }
  }

  $parts = @()
  if ($registered -gt 0) { $parts += "$registered voice pack(s) registered" }
  if ($would -gt 0) { $parts += "$would voice pack(s) would be registered" }
  if ($already -gt 0) { $parts += "$already already registered" }
  if ($blocked -gt 0) { $parts += "$blocked NOT registered" }
  if ($parts.Count -eq 0) { return '' }
  '; ' + ($parts -join ', ')
}

function Invoke-LayoutStage {
  $inv = $script:Inv
  $file = Join-Path $script:DataDir 'runtime.json'

  foreach ($p in $inv.Packs) {
    if ($p.Ok) { Write-Event 'layout' 'log' "voice pack: $($p.Id) ($($p.Kind)) at $($p.Path)" }
    else { Write-Event 'layout' 'log' "not a voice pack: $($p.Path) — $($p.Why)" $null $null 'a pack is a directory with adapter_config.json + adapter_model.safetensors, or a file named *.speaker.safetensors; the engine rejects a renamed embedding by name' }
  }
  $packNote = Register-Packs

  $layout = [ordered]@{
    ttsPython = (Get-LayoutPath $inv.Python $script:Root)
    ttsScript = ''
    ttsRoot   = (Get-LayoutPath $inv.EngineRoot $script:Root)
    hfHome    = (Get-LayoutPath $inv.HfHome $script:Root)
  }
  if ($inv.Worker) { $layout['ttsScript'] = (Get-LayoutPath $inv.Worker $script:Root) }
  else { $layout['ttsScript'] = 'runtime/worker/irodori/worker.py' }
  # Preserved, not rewritten: the idle timeout is the user's tuning knob and this file is
  # the only place it lives.
  if ($inv.Runtime -and $null -ne $inv.Runtime.idleStopSecs) { $layout['idleStopSecs'] = [int]$inv.Runtime.idleStopSecs }
  else { $layout['idleStopSecs'] = 900 }

  $outside = @()
  foreach ($k in @('ttsPython', 'ttsRoot', 'hfHome')) {
    if ([System.IO.Path]::IsPathRooted($layout[$k])) { $outside += "$k=$($layout[$k])" }
  }
  foreach ($k in $layout.Keys) { Write-Event 'layout' 'log' "$k = $($layout[$k])" }

  # A key pointing at something that is not there yet is still written — that is where the
  # stage that failed would have put it — but the runtime honours this file verbatim and
  # would then report worker_start_failed naming the path, so say it now.
  $absent = @()
  foreach ($k in @('ttsPython', 'ttsScript', 'ttsRoot', 'hfHome')) {
    if (-not (Test-Path -LiteralPath (Resolve-Against $layout[$k] $script:Root))) { $absent += $k }
  }
  if ($absent.Count -gt 0) {
    Write-Event 'layout' 'log' "not there yet: $($absent -join ', ')" $null $null 'the stage that would create it failed above; the runtime will still start and report the missing path rather than pretend it can speak'
  }

  if (-not $inv.Worker) {
    Write-Event 'layout' 'fail' "the worker script is not in this tree; expected $($script:Root)\runtime\worker\irodori\worker.py" $null $null 'worker.py ships with the installer — reinstall, or run from a source checkout where worker/irodori/worker.py exists'
    return
  }
  # The coupling note goes BEFORE the stage's terminal event: a reader that finalises a stage
  # row when ok/skip/fail arrives would otherwise drop the one line the user most needs.
  if ($outside.Count -gt 0) {
    Write-Event 'layout' 'log' "$($outside.Count) of these paths point outside the install: $($outside -join ', ')" $null $null 'that reuse couples this install to those locations: moving, renaming or deleting any of them breaks this install until bootstrap is re-run. Provision inside the install instead if you want a tree you can zip and copy'
  }
  if ($CheckOnly) {
    $verb = if (Test-Path -LiteralPath $file) { 'would rewrite' } else { 'would write' }
    Write-Event 'layout' 'skip' "$verb $file with the paths above$packNote; -CheckOnly changed nothing"
    return
  }

  [void](New-Item -ItemType Directory -Force -Path $script:DataDir)
  # NOT Set-Content -Encoding UTF8: that writes a BOM on Windows PowerShell 5.1, and the
  # runtime parses this file with a strict JSON reader that also rejects unknown keys —
  # which is why nothing but these five ever goes in it.
  [System.IO.File]::WriteAllText($file, ($layout | ConvertTo-Json), (New-Object System.Text.UTF8Encoding($false)))
  try { [void](Get-Content -LiteralPath $file -Raw | ConvertFrom-Json) }
  catch {
    Write-Event 'layout' 'fail' "wrote $file but it does not parse back: $($_.Exception.Message)" $null $null 'delete data/runtime.json and re-run with -Only layout'
    return
  }
  if ($outside.Count -gt 0) { Write-Event 'layout' 'ok' "wrote $file$packNote; $($outside.Count) path(s) are absolute because they point outside the install" }
  else { Write-Event 'layout' 'ok' "wrote $file with relative paths only, so the whole tree stays portable$packNote" }
}

function Invoke-SmokeStage {
  $inv = $script:Inv
  $runtimeExe = Join-Path $script:Root 'bin\voice-core-runtime.exe'
  $cliExe = Join-Path $script:Root 'bin\voice-core.exe'
  if (-not (Test-Path -LiteralPath $runtimeExe)) { $runtimeExe = Join-Path $script:Root 'target\release\voice-core-runtime.exe' }
  if (-not (Test-Path -LiteralPath $cliExe)) { $cliExe = Join-Path $script:Root 'target\release\voice-core.exe' }
  if (-not (Test-Path -LiteralPath $runtimeExe) -or -not (Test-Path -LiteralPath $cliExe)) {
    Write-Event 'smoke' 'fail' 'the runtime or the CLI is not in this tree' $null $null 'install the packaged tree, or build with cargo build --release'
    return
  }
  if ($CheckOnly) {
    Write-Event 'smoke' 'skip' "would load the model with $cliExe warm; -CheckOnly started nothing"
    return
  }

  # A runtime may already be up — the GUI starts one. Starting a second would fail on the
  # port by design, so use the live one and leave it exactly as we found it.
  $status = Invoke-Child -File $cliExe -Arguments @('--data-dir', $script:DataDir, 'status') -TimeoutSec 30
  $mine = $null
  if ($status.ExitCode -eq 0) { Write-Event 'smoke' 'log' 'a runtime is already listening; using it' }
  else {
    Write-Event 'smoke' 'log' "starting $runtimeExe"
    $mine = Start-Process -FilePath $runtimeExe -ArgumentList @('--data-dir', $script:DataDir) -PassThru -WindowStyle Hidden
    Start-Sleep -Seconds 3
  }
  try {
    # 34.1 s cold and 13.4 s warm on the reference machine, so the timeout is generous on
    # purpose: giving up here only means the first real utterance pays the same load again.
    Write-Event 'smoke' 'progress' 'loading the model (34 s cold, 14 s warm on the reference machine)' 0 1
    $warm = Invoke-Child -File $cliExe -Arguments @('--data-dir', $script:DataDir, 'warm') -TimeoutSec 660
    Write-ChildLog 'smoke' 'warm' $warm
    $log = Join-Path $script:DataDir 'logs\tts-worker.out.log'
    if (Test-Path -LiteralPath $log) {
      foreach ($hit in @(Select-String -LiteralPath $log -Pattern 'stage=(boot\.imports|model\.load\.done)' | Select-Object -Last 4)) {
        Write-Event 'smoke' 'log' "cost | $($hit.Line.Trim())"
      }
    }
    if ($warm.ExitCode -eq 0) { Write-Event 'smoke' 'ok' 'the backend loaded its model' 1 1 }
    else { Write-Event 'smoke' 'fail' "warm failed with exit $($warm.ExitCode)" $null $null 'data\logs\tts-worker.err.log has the engine reason verbatim, including its traceback' }
  }
  finally {
    if ($mine -and -not $mine.HasExited) {
      [void](Invoke-Child -File $cliExe -Arguments @('--data-dir', $script:DataDir, 'stop') -TimeoutSec 60)
    }
  }
}

# --------------------------------------------------------------- the driver --

if (-not $InstallRoot) { $InstallRoot = (Join-Path $PSScriptRoot '..') }
if (-not (Test-Path -LiteralPath $InstallRoot)) {
  [Console]::Error.WriteLine("bootstrap: -InstallRoot $InstallRoot does not exist")
  exit 2
}
$script:Root = Get-FullPath $InstallRoot
$script:DataDir = Join-Path $script:Root 'data'

$script:Selected = New-Object 'System.Collections.Generic.HashSet[string]' ([System.StringComparer]::OrdinalIgnoreCase)
if ([string]::IsNullOrWhiteSpace($Only)) {
  foreach ($s in $StageOrder) { [void]$script:Selected.Add($s) }
}
else {
  foreach ($s in ($Only -split ',')) {
    $name = $s.Trim().ToLowerInvariant()
    if (-not $name) { continue }
    if ($StageOrder -notcontains $name) {
      # Usage error, not a stage failure: nothing on stdout, non-zero exit, because the
      # caller's argv is wrong and that must not look like a broken machine.
      [Console]::Error.WriteLine("bootstrap: -Only '$name' is not a stage. Pick from: $($StageOrder -join ', ')")
      exit 2
    }
    [void]$script:Selected.Add($name)
  }
  if ($script:Selected.Count -eq 0) {
    [Console]::Error.WriteLine('bootstrap: -Only was empty')
    exit 2
  }
}

if ($Json) {
  # Messages carry pack names and paths that are not ASCII. Set before anything is written:
  # assigning OutputEncoding replaces the cached Console.Out writer.
  [Console]::OutputEncoding = New-Object System.Text.UTF8Encoding($false)
}

$script:RuntimeFile = Read-RuntimeJson $script:DataDir
$script:Inv = Resolve-Inventory

Invoke-Stage 'preflight' { Invoke-Preflight }
Invoke-Stage 'engine' { Invoke-EngineStage }
Invoke-Stage 'codec' { Invoke-CodecStage }
Invoke-Stage 'venv' { Invoke-VenvStage }
Invoke-Stage 'models' { Invoke-ModelsStage }
Invoke-Stage 'layout' { Invoke-LayoutStage }
Invoke-Stage 'smoke' { Invoke-SmokeStage }

if (-not $Json) {
  # Human-only closing block. There is deliberately no run-finished EVENT: the schema has
  # no stage to put it under, and the GUI already knows the run ended when the process does.
  Write-Host ''
  $failed = @($StageOrder | Where-Object { $script:Terminal[$_] -eq 'fail' })
  $ran = @($StageOrder | Where-Object { $script:Terminal.ContainsKey($_) })
  if ($failed.Count -gt 0) {
    Write-Host "$($failed.Count) stage(s) failed: $($failed -join ', ')" -ForegroundColor Yellow
    Write-Host 'Each one printed its own fix. Retry just that stage with -Only <stage>.' -ForegroundColor Yellow
  }
  elseif ($ran.Count -lt $StageOrder.Count) {
    # Do not claim the install is ready on the strength of the one stage -Only asked for.
    Write-Host "$($ran -join ', ') finished. The other stages were not run (-Only)." -ForegroundColor Green
  }
  else { Write-Host 'Everything the app needs is in place.' -ForegroundColor Green }
  Write-Host ''
  Write-Host "  Launch:        $(Join-Path $script:Root 'VoiceCore.exe')"
  Write-Host "  Settings:      $(Join-Path $script:DataDir 'config.json')  (one file; comments allowed)"
  Write-Host "  Diagnose:      $(Join-Path $script:Root 'bin\voice-core.exe') doctor"
  Write-Host '  A voice pack:  docs\training-a-voice.md'
  Write-Host '  For agents:    skills\voice-core-tts\SKILL.md (说话)'
  Write-Host '                 skills\voice-core-voice-training\SKILL.md (训练音色)'
}

# Always 0: a failed stage is a reported event, not a crashed script. Only the usage
# errors above exit non-zero.
exit 0
