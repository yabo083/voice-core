# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The HTTP surface carries its own version, `apiVersion`, which is bumped only on a breaking
change to the public contract and is independent of the release version below
(`src/service.rs:26-27`).

## [1.2.0] - 2026-09-04

A restructuring rather than a patch: voice-core stops being three executables a user has to
choose between and becomes one app. `VoiceCore.exe` is the entry point, it starts the backend
and the subtitle presenter itself, and the installer creates exactly one shortcut. `apiVersion`
stays `1` — no route, request or event changed shape.

### Added

- **`VoiceCore.exe` — one entry point, at the root of the install tree.** A Tauri 2 desktop app
  (vanilla TypeScript + Vite, hand-written CSS, no component library) that owns the tray icon
  and the window, supervises `bin\voice-core-runtime.exe` and the subtitle presenter, hides to
  the tray when its window is closed, and stops both children only on an explicit Quit. A
  second launch focuses the window that is already open instead of starting a second app. It is
  one self-contained executable: the frontend is embedded at compile time and the WebView2
  loader is linked statically, so nothing has to travel beside it.
- **Provisioning that detects before it downloads.** The app inspects the machine first and
  adopts what is already there — an engine tree, its virtualenv, a Hugging Face cache — instead
  of re-fetching it. `data\runtime.json` is the mechanism: absolute paths in it are honoured as
  they are, which is what makes reuse possible, and relative ones resolve against the install
  root, which is what keeps a provisioned tree portable. Re-downloading 4.8 GiB that was
  already on the disk is the complaint this release exists to answer.
- **`scripts/bootstrap.ps1 -Json` — a machine-readable event stream.** One JSON object per line
  on stdout and nothing else on it, every key always present: `ts`, `stage`
  (`preflight` → `engine` → `codec` → `venv` → `models` → `layout` → `smoke`, always in that
  order), `event` (`start` / `progress` / `log` / `ok` / `skip` / `fail`), `message`,
  `done` / `total` (bytes while downloading, item counts otherwise) and `remedy`. A `skip` names
  what was reused and where it was found; a `fail` always carries a remedy; and a failed stage
  no longer aborts the run — later independent stages still execute and the process exits 0, so
  one missing prerequisite can no longer hide the state of everything behind it. Only a usage
  error exits non-zero. Without `-Json`, the output is the human text it always was.
- **A WebView2 prerequisite check in Setup.** `VoiceCore.exe` renders in WebView2, which is
  present by default on Windows 11 and current Windows 10 and absent on older or stripped
  images — where the window would simply come up blank. Setup reads Microsoft's documented
  detection key and offers the download link rather than silently fetching ~100 MB
  (`scripts/installer/voice-core.iss`).
- **A visual identity: the mark, the palette and the type.** The product mark is CJK corner
  brackets `「」` closing inward around a cleaved obsidian shard - the typographic sign that
  someone is speaking, around a stone whose facets are its only light, and nothing radiating
  outward because nothing leaves the machine. The surfaces went hueless graphite with one
  violet accent (Obsidian's palette, and its callout hues for state), the geometry went WinUI 3
  (controls 4px, cards 8px, flat cards with a top inset highlight instead of a shadow), and the
  window's native caption is painted `#161616` through DWM so the title bar is part of the app
  rather than a system bar on top of it (`manager/src-tauri/src/caption.rs`).
  `manager/src/fonts/` ships Noto Sans SC and Sarasa Mono SC as subset woff2: one CJK/Latin
  design instead of Segoe bolted to Microsoft YaHei, and a mono whose CJK advances are exactly
  twice its Latin ones, so a log line that mixes Chinese with an ASCII path still lines up.
- **Measured memory, in the panel.** `resource_usage` walks the process tree this app owns and
  reads each working set, and queries nvidia-smi for the card. The 音色引擎 row therefore
  reports what the engine actually costs (~4.3 GiB resident with a model loaded) rather than a
  state word. Per-process VRAM is reported only when the driver supplies it: a GeForce in WDDM
  mode refuses to break it down, and the panel says so instead of showing a false zero
  (`manager/src-tauri/src/usage.rs`).
- **A voice pack format, so a pack can describe itself (`docs/voicepack-spec.md`).** A pack may
  carry `voicepack.json` — inside a directory pack, or as `<stem>.voicepack.json` beside a
  single-file one — naming its display name, engine, kind, languages, speaker and portrait, plus
  optional `dialog` and `synthesis` sections. `schema` is the only required key, unknown keys are
  ignored, a newer schema degrades to the core fields with one warning, and a pack with no
  manifest behaves exactly as it did in 1.1.0. The portrait now lives *in* the pack, so a voice
  that is copied to another machine keeps its name and its face; `data\avatars\` is retired.
  `GET /api/voices` returns the merged result plus a `manifest` field naming the file it came
  from. There are exactly two places configuration lives — `data\config.json` and a pack's own
  manifest — and **the manifest wins**: a registry entry is generated (seeded by Setup, written
  by the panel when a pack is registered), so it must not outrank the pack's own description of
  itself. What `config.json` is authoritative about is which packs exist and where, which is why
  an entry needs nothing but `id` and `path`. Registering a pack in the panel therefore writes
  the pack's manifest and leaves a pointer in `config.json`; on read-only media it falls back to
  writing the entry and says so.
- **A prompt for somebody else's agent.** 状态 → 使用方式 carries a copy-paste English prompt
  that points at the `SKILL.md` shipped in the install tree and states the two things an agent
  gets wrong unaided: use the CLI, and a 20-60 s cold start is not a timeout.

### Changed

- **`data\runtime.json` is JSONC, like `config.json` beside it.** Both files are ones an error
  message tells a human to go and edit, so a `//` note explaining an overridden path no longer
  fails startup with `key must be a string at line 2`
  (`src/bin/voice-core-runtime.rs:load_runtime_file`).
- **An install owns its engine.** The interpreter, the engine source and the Hugging Face cache
  now sit where the runtime already looked for them by itself — `runtime\python`,
  `runtime\engine`, `models\huggingface` — so `runtime.json` on this machine is down to
  `idleStopSecs` and no longer names a path inside a development checkout. Nothing in the
  resolution order changed; the files moved to where the defaults point.
- **`package.ps1` will not guess where an engine lives.** `-IncludeEngine` and `-IncludeModels`
  now require `-EngineVenv` / `-EngineRoot` / `-ModelCache` (or `VC_ENGINE_VENV`,
  `VC_ENGINE_ROOT`, `VC_MODEL_CACHE`) and fail with the paths to pass, instead of defaulting to
  a sibling checkout that only ever existed on one machine. On a machine with voice-core
  installed, those three are `<install>\runtime\python`, `<install>\runtime\engine` and
  `<install>\models\huggingface`.
- **The subtitle presenter stopped being a launcher, and was renamed for it.**
  `bin\app\VoiceCoreTray.exe` became `bin\presenter\VoiceCorePresenter.exe`, and the new
  `--presenter` flag makes it the subtitle surface and nothing else. The tray icon is detached
  from the visual tree before H.NotifyIcon's `Loaded` handler can register it with the shell,
  which takes the context menu with it because the menu was that icon's `ContextFlyout`; the
  status window's only `AppWindow.Show()` call site returns early; `SubtitleOptions.NoRuntime`
  is computed as `--no-runtime || --presenter`, so a presenter cannot start a runtime at all;
  and both `RuntimeClient.StopAsync()` call sites are guarded, because stopping the backend
  belongs to `VoiceCore.exe` now and a presenter that stopped it would take the service down
  for every other subscriber. The dialog, the two global hotkeys, the wheel gesture and the SSE
  subscription are untouched — they are the job. Single instance is a separate mutex,
  `voice-core-presenter`, so a GUI-spawned presenter and a developer's standalone tray cannot
  silently kill each other.
- **Exactly one Start Menu shortcut, to `VoiceCore.exe`.** The presenter and the CLI get none:
  the presenter is a child process nobody should launch by hand, and the CLI is an agent's tool
  rather than a launcher. Setup's final page now runs the app instead of a PowerShell window,
  so a first-time user lands in the app. The per-user default, the `data\` preservation on
  uninstall and the unsigned-binary notice are unchanged.
- **`scripts/package.ps1` assembles a single-entry-point tree, and asserts it.**
  `VoiceCore.exe` at the root, `bin\voice-core-runtime.exe`, `bin\voice-core.exe` and
  `bin\presenter\` beneath it; the run fails if the root ever holds a second executable.
  `-SkipGui` assembles a tree without the GUI while it is being built and says loudly that the
  result has no entry point; `-GuiExe` points at a build explicitly.
- **The voice-pack registry is edited by the app.** Adding or removing a voice rewrites the
  `voicePacks` section of `data\config.json` and leaves the rest of that file — including its
  comments — exactly as it was found. The runtime still re-reads the section on mtime change, so
  installing a voice still restarts nothing.
- **The Windows App SDK notices moved** from `bin\app\` to `bin\presenter\`, beside the DLLs
  they cover (`LICENSE-EXCEPTION.md`, `THIRD-PARTY-NOTICES.md`).
- **Setup's running-app detection names all three single-instance mutexes** — the GUI's
  `io.github.yabo083.voicecore-sim`, the presenter's `voice-core-presenter`, and 1.1.0's
  `voice-core-winui-tray` — so an upgrade over either generation closes what is running first.
- **Setup became 部署, and stopped being a permanent tab.** Once the engine is installed the
  rail item retires; the page stays reachable from 状态 → 环境 → 检查环境 as a transient page
  with a back arrow. A tab whose job is finished trains people to ignore the rail.
- **The screens stopped explaining themselves.** Every hint paragraph, every "why this matters"
  note and the three-sentence blocks under the stage list are gone. What survives is either
  data or a tooltip on the control it belongs to; the seven stage names are results now
  (`环境检查 / 引擎源码 / 音频编解码器 / Python 环境 / 模型权重 / 写入配置 / 试跑一句`) and the
  engine's vocabulary lives in the log, where somebody who needs it is already looking. The
  environment inventory and the "point at what you already have" pickers merged into one card
  whose every row carries its own outcome on the right: a chip when the runtime handles it, a
  button when the user must.
- **The primary actions moved out of the panels and into a command bar** pinned below the scroll
  region, so the action of a seven-stage wizard cannot scroll away mid-download. Disabled
  buttons carry their reason as a tooltip and use `aria-disabled`, not `disabled`: Chromium
  drops pointer events on a disabled control, so a truly disabled button can never show the
  hint that explains itself.
- **`/api/status` is polled every second, and nothing is extrapolated between polls.** The
  earlier panel polled every five seconds and advanced its own clocks in between, which made
  uptime and idle time jump - and occasionally count backwards, because two clocks measured in
  different places do not agree. A loopback GET costs about a millisecond; guessing cost
  correctness.
- **声线 is now 音色** across the interface and the agent-facing docs. Wire names
  (`voicePackId`, `voicePacks`, `voice_pack_not_found`) are unchanged: they are the contract
  other agents already read.

### Fixed

- **A pack's manifest is now part of the hot-reload check.** The registry reloaded on
  `config.json`'s mtime alone, so editing a pack's `voicepack.json` — the file the format tells
  you to edit — changed nothing until the config file was touched or the runtime restarted,
  while the docs promised no restart was needed. `reload_if_changed` now also stats every
  manifest behind the loaded view, and treats a manifest that has *appeared* since the last
  read as a change too (`src/packs.rs`, four unit tests).

### Removed

- **The 初始化向导 and 环境诊断 Start Menu shortcuts, and the "run bootstrap" checkbox on
  Setup's final page.** Provisioning moved into the app, and two provisioners racing over one
  engine tree is worse than none. `scripts\bootstrap.ps1` still ships inside the install and
  still runs by hand, with or without `-CheckOnly`.
- **The 托盘控制台 shortcut.** The optional desktop shortcut now points at `VoiceCore.exe`.

### Deprecated

- **The presenter's own tray icon, context menu and status window.** They are still there when
  it runs without `--presenter`, deliberately: the presenter has to remain exercisable on its
  own for one release, and this restructuring has to stay revertible. They are scheduled for
  deletion along with the `H.NotifyIcon.WinUI` dependency and every tray menu handler in
  `app/VoiceCoreTray/MainWindow.xaml` and `MainWindow.xaml.cs`.

## [1.1.0] - 2026-09-04

Feature-sized rather than a patch: a first-run experience that did not exist, a training kit
that did not exist, and a measured change to when the engine's cost is paid. `apiVersion`
stays `1` — no contract changed shape.

### Added

- **A single-file installer.** `voice-core-<version>-setup.exe`, built by
  `scripts/package.ps1 -Installer` from the same portable tree package.ps1 assembles
  (`scripts/installer/voice-core.iss`). Per-user by default so it needs no administrator;
  the uninstaller leaves `data/` alone because that is where the token, the settings and the
  voice packs live. Unsigned, so SmartScreen warns; the release publishes its SHA256.
- **`scripts/bootstrap.ps1`** — six idempotent stages that turn a fresh install into one
  that can speak: preflight (disk, GPU, driver, git, uv) with a remedy printed for every
  failure, the Irodori engine and DACVAE cloned at a pinned revision, the engine virtualenv
  via upstream's own `uv sync --extra cu128`, ~4.8 GiB of weights into a resumable Hugging
  Face cache, `data/runtime.json` written with **relative** paths so the tree stays portable,
  and a smoke test that loads the model and prints what each stage cost. `-CheckOnly` reports
  the environment and changes nothing.
- **`docs/getting-started.md`** and **`docs/training-a-voice.md`** — the install path and the
  voice-pack pipeline in prose, including the honest table of what each pack kind actually
  requires (reference audio needs no text; a LoRA needs audio *plus* a transcript in the same
  language as the audio).
- **`scripts/training/`** — the training pipeline as first-party scripts, parameterised for
  any speaker and any dataset. No dataset, no audio and no character name ships with them.
- **`docs/adr/0001-tts-engine-backend-seam.md`** — the decision that a TTS backend is any process
  that speaks the loopback protocol, that `pack.engine` is the routing key and
  `pack.languages` the capability advertisement, and what a second backend would actually
  cost. Records the part that would otherwise be discovered late: the GPU is arbitrated by a
  single semaphore and idle reclaim is per-worker, so two resident backends need a VRAM
  budget and a scheduling policy that do not exist.

### Changed

- **The engine's Python imports moved off the readiness path.** `torch` and the engine are
  ~3 s of imports (9 s with a cold page cache) and are not needed to answer `/health`, so
  they now load on a background thread while uvicorn binds. Measured in
  `data/logs/tts-worker.out.log`: `boot.listening` moved from t=3017 ms to t=604 ms and the
  supervisor's `worker.ready` from 3305 ms to 1218 ms. Nothing got faster in total — the
  import still finishes before the model load starts — the wait moved off the path where a
  caller is blocked.
- **`PYTORCH_CUDA_ALLOC_CONF=expandable_segments:True`** is now the worker's default
  (overridable). **Measured result on the reference machine: no change** — 1755.6 MiB
  allocated against 3178.0 MiB reserved, before and after. The overhang is the engine's own
  transient peak, not fragmentation. Kept because it costs nothing and is the right default
  for a process that loads and unloads repeatedly; `boot.device` now reports whether it is in
  effect, so the next person can re-measure instead of re-guessing.

### Fixed

- **Idle reclaim could kill a model load in flight.** A load takes 14-34 s and touches
  nothing else, so the reaper read a loading worker as an unused one. With
  `--idle-stop-secs 20` the process was killed mid-load and `/api/warm` returned
  `model_load_failed: engine unreachable` after 79 s. A load now counts as activity
  (`Worker::idle_for` returns zero while one is in flight), which is why warming works at any
  idle setting rather than only at the 900 s default that hid the bug.
- **Tier-1 idle reclaim left no trace on disk.** Releasing VRAM reported only to the
  in-memory event bus, so with no frontend subscribed there was nothing to read afterwards,
  while tier 2 (stopping the process) did log. Both tiers now write to
  `tts-worker.out.log`, and the worker's own `model.unload.done` line moved from stderr to
  stdout — an unload is a lifecycle event, not an error, and the cold path has to be readable
  end to end in one file.

## [1.0.0] - 2026-09-04

First public release. The runtime, the client, the Python worker and the WinUI 3 tray are
all at this version; `apiVersion` is `1`.

### Added

- **Two binaries with one job each.** `voice-core-runtime` is the daemon and the sole owner
  of a request's lifecycle; `voice-core` is the client and does nothing the HTTP API cannot
  (`Cargo.toml:10-16`).
- **One documented HTTP contract**, loopback only on `127.0.0.1:8760`, bearer-token
  authenticated from `data/token.txt`. `GET /api/health` is the only unauthenticated route.
  Eleven routes, one error shape, no SDK to keep in sync (`docs/api.md`).
- **Audio never travels inside JSON.** `POST /api/speak` returns an `audioId`; the bytes come
  from `GET /api/audio/{audioId}` as `audio/wav`. The runtime reserves a spool path and the
  worker writes its WAV directly there, so no base64 exists in the system and neither process
  holds a second copy of the samples.
- **Kernel-enforced engine ownership.** The worker process is assigned to a Win32 job object
  with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so a dead runtime cannot orphan a
  multi-gigabyte GPU process (`src/supervise.rs:405-451`).
- **Server-Sent event stream** at `GET /api/events`, the only presentation contract: the
  runtime never calls a frontend back. A new subscriber receives the recent tail of up to 64
  envelopes first, so a presenter that restarts mid-utterance can render current state
  without a catch-up call.
- **Idle stop.** After `--idle-stop-secs` (default 900) with no work, the engine process is
  stopped and the GPU is handed back; the runtime keeps serving.
- **Audio spool with bounded lifetime.** Entries expire at `spool_ttl` (default 3600 s), are
  evicted oldest-first past `spool_max_bytes` (default 2048 MiB), and every `*.wav` is deleted
  on runtime start. `HEAD /api/audio/{id}` lets a history view ask whether an older utterance
  is still replayable instead of assuming it (`src/spool.rs`, `docs/api.md:36-39`).
- **Voice pack registry, hot-reloaded.** Packs are data plus metadata — a kind and a path —
  read from the `voicePacks` section of `data/config.json` and re-read whenever that file's
  mtime changes, so installing a voice needs no restart of anything. Three kinds:
  `lora-adapter`, `speaker-embedding`, `reference-audio` (`src/packs.rs`).
- **Dual-text protocol with caller-supplied alignment.** `text` is spoken, `displayText` is
  shown, and `rubyPairs` carries the segment-by-segment mapping between them. The alignment is
  not derivable downstream — Chinese and Japanese do not line up positionally, and a
  translation freely merges, splits and reorders clauses — so the party that produced both
  strings sends it and the presenter renders it verbatim (`docs/api.md:64-80`).
- **Galgame-style subtitle overlay** in the tray: the window *is* the box, sized to its
  content by `AppWindow.MoveAndResize` so the plate, its DWM-rounded corners and its DWM
  shadow are all system-drawn. Bottom-center anchoring with work-area clamping, DPI-change
  re-fit, never-focused (`WS_EX_NOACTIVATE`), drag by the top band only
  (`docs/dialog-presenter.md:222-249`).
- **Hold lifecycle with a countdown, not a permanent caption.** The default is a dwell
  countdown (`displaySeconds`, else 6 s) animated as one storyboard on the plate's bottom
  edge and frozen while the cursor is over the box; 常驻 is the opt-in, reachable from the
  tray menu and from `Ctrl+Alt+H`, which reports which mode it switched to
  (`docs/dialog-presenter.md:66-95`).
- **In-place backlog.** Wheel-up over the box walks back through the last 50 utterances in
  the box itself, wheel-down walks forward, and passing the newest entry returns to the live
  line. A body click replays that line's audio, or reports 语音已释放 when the spool has
  already dropped it. The speaker travels with the line, so the avatar can never show
  whoever spoke last while the box displays someone else's words.
- **Two global hotkeys**, `Ctrl+Alt+D` (show/hide) and `Ctrl+Alt+H` (hold/countdown), bound
  in `config.json`. A registration failure — the combination already owned by another app —
  is reported in the tray tooltip and status note, never swallowed.
- **Metrics and observability without a logging dependency.** `GET /api/metrics` serves
  counters and speak-latency percentiles; `data/metrics.jsonl` records one line per
  synthesis (queue, synth and total milliseconds, cold-start flag, audio bytes) and
  `data/logs/dialog.jsonl` one line per rendered utterance (reveal, measure and resize
  timings, frame gaps, queue depth). Runtime and worker stdout/stderr are tee'd to
  `data/logs/{runtime,tts-worker}.{out,err}.log`.
- **Portable install with no absolute paths.** `scripts/package.ps1` assembles a
  `bin/ runtime/ models/ data/ skills/` tree in which every location is derived from the
  executable's own position, so the folder survives being zipped, moved, or copied to
  another machine. A bundled Windows virtualenv is repointed at its shipped interpreter
  automatically, since `pyvenv.cfg` records an absolute `home`
  (`docs/deployment.md:94-101`).
- **Preflight that reports instead of crashing.** Missing resources do not abort startup: they
  are logged, published as a `progress` event with phase `preflight`, and listed in
  `GET /api/status` under `worker.missing`, so a frontend can show a broken install
  immediately rather than discovering it as a failed utterance.
  `voice-core-runtime.exe --print-layout` prints every resolved path with an `ok`/`MISSING`
  marker, and prints the diagnosis even when the engine cannot be resolved at all.
- **The agent-facing contract ships with the install.** `skills/voice-core/SKILL.md` is copied
  into the package (`scripts/package.ps1:133`), so an agent that finds the tree can learn the
  surface without the development repository.
- **End-to-end tests that need no GPU.** A fake engine stands in for the worker, covering the
  speak pipeline, authentication, shutdown with a live event subscriber, and latency
  percentiles (`tests/speak_pipeline.rs`).
- **Subtitle layout self-test.** `VoiceCoreTray.exe --subtitle-selftest <path>` renders nine
  cases and asserts, per case, that the client area the OS gave the window is the size XAML
  laid out (`fits_client`), that the box is on screen (`onscreen`), and how many cells
  actually carry an annotation (`rubies`) (`docs/dialog-presenter.md:292-295`).

### Fixed

- **One settings file, parsed as JSONC.** `data/config.json` now holds every preference the
  app has — `dialog.*`, `hotkeys.*` and the `voicePacks` registry the runtime reads —
  superseding the separate `dialog.json`, `hotkeys.json` and `voicepacks.json`, whose values
  are migrated once and the files removed. Because a human edits it in Notepad, the reader
  strips a UTF-8 BOM, treats `//` and `/* */` comments as whitespace and forgives one
  trailing comma, and it turns comments into spaces rather than deleting them so a parse
  error still points at the line and column the user is looking at. The tray is the only
  writer and preserves the packs section verbatim as a raw `JsonElement`, so a field it does
  not know cannot be silently dropped (`src/jsonc.rs`, `docs/dialog-presenter.md:197-213`).
- **Voice pack reloads no longer pin a stale or empty registry.** The file's mtime is sampled
  *before* the read and cached only after a successful parse. Both halves matter: an editor
  that saves in place truncates and rewrites, so a read can land on a prefix, and Windows
  file times only advance on the ~15.6 ms timer tick — caching the mtime of a failed parse
  would match the change guard forever and hold an empty registry against a file that
  visibly lists packs. A broken file keeps the last good list, complains once, and is retried
  on the next access (`src/packs.rs:91-120`).
- **Three reveal presets that share one layout.** `dialog.reveal` selects `typewriter`
  (per-character, paced to 85 % of the WAV duration parsed from the RIFF header, or
  45 ms/char clamped to [0.35 s, 4.5 s] without audio), `sweep` (a soft band of light
  crossing the line at one speed regardless of segment count, 420 ms travel / 200 ms
  feather) or `fade` (segments fading up a clause-sized pause apart, 100 ms stagger / 220 ms
  per segment). All three build the layout from the full line before anything is shown, so
  wrap points and annotation slices are final in every preset and only the animation differs
  (`docs/dialog-presenter.md:104-121`).
- **Punctuation-only annotations are dropped instead of rendered.** Callers are asked to send
  punctuation pairs — the `rubyPairs` concatenation rule requires them — so the renderer, not
  the caller, does the trimming: every annotation loses its leading and trailing punctuation,
  and a fragment that was *only* punctuation annotates nothing and renders a bare cell. A
  lone 「、」 under a 「，」 carries no information and at 11 dip reads as dirt on the plate.
  The self-test's `rubies` count asserts it (`docs/dialog-presenter.md:138-141`,
  `docs/api.md:69-71`).
- **The resize seam is closed.** A growing plate is a stream of `MoveAndResize` calls, each
  exposing a strip of client area XAML has not arranged into yet, and the plate is acrylic so
  nothing else paints it. Three fixes, all required: `WM_ERASEBKGND` fills the client rect
  with the acrylic fallback and returns 1, stopping the OS erasing that strip with the window
  class brush — a white edge along the growing side for one frame per resize; `DialogBox`
  stretches in both directions so a `Top`-aligned plate cannot leave the strip unpainted once
  arrange happens; and `RootGrid.UpdateLayout()` runs right after every box application,
  growth frames included, so the arrange does not land a frame late
  (`docs/dialog-presenter.md:251-262`).
- **Engine failures reach the caller with their real reason.** Without a handler the worker's
  Starlette layer answered 500 with the literal body `Internal Server Error` and the reason —
  a missing checkpoint, a pack with no reference audio — never left the process. The worker
  now returns its reason in the reply's single `error` field, tagged with the stage that
  failed (`model load failed: ` / `synthesis failed: `), and the runtime strips those prefixes
  into distinct error variants so a model that never loaded and an utterance the engine
  refused get different codes and different recovery advice. An untagged reason is treated as
  a synthesis failure, which is what an externally attached worker produces. The traceback
  still goes to stderr, which the runtime tees to `tts-worker.err.log`
  (`worker/irodori/worker.py:142-176`, `src/engine.rs:196-208`).
- **A worker that dies during import fails in milliseconds, not after the ready timeout.**
  The readiness loop now polls the child process alongside `/health`: a broken virtualenv or
  an `ImportError` used to cost the whole 90-second ready timeout while the caller held the
  single GPU permit and nothing appeared on the event stream. On exit or timeout the error
  carries the actual elapsed milliseconds and the tail of `tts-worker.err.log`, and says
  which of the two happened — "exited before answering /health" versus still loading — and
  the reason reaches every frontend as a `workerStopped` event. Separately, a worker that
  exited on its own between requests is now detected when the next request arrives and
  replaced with a fresh one instead of being handed out as a live base URL
  (`src/supervise.rs:147-157,243-288`).

### Security

- The HTTP surface binds loopback only and every route but `GET /api/health` requires the
  bearer token in `data/token.txt`, minted on first run. `health` succeeding while `status`
  returns 401 means the token does not match — the tray and the runtime resolved different
  data directories — not that the service is down.
- The worker runs with `HF_HUB_OFFLINE=1` and `TRANSFORMERS_OFFLINE=1`, so a synthesis
  attempt cannot reach out to Hugging Face (`worker/irodori/worker.py:48-49`).

### Notes

This is the first release published under **GPL-3.0-or-later**; earlier internal numbering
belonged to an unpublished predecessor line that is not part of this repository. Third-party
notices are in `THIRD-PARTY-NOTICES.md`, and one unresolved licensing conflict affecting
binary releases of the tray is documented there.

No model weights, voice packs, or game-derived audio are part of any release artefact.
