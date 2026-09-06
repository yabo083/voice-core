# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The HTTP surface carries its own version, `apiVersion`, which is bumped only on a breaking
change to the public contract and is independent of the release version below
(`src/service.rs:26-27`).

## [1.5.0] - 2026-09-06

Training schedules itself from the corpus instead of from constants, and stops when it stops
improving. A fixed step count cannot be right for two corpus sizes at once: at batch 16, 100 steps
is 9 epochs of 163 rows and 1.6 epochs of 1000.

### Added

- **Validation and save cadence derived from the dataset.** `run_training.py` computes steps per
  epoch from the manifest row count and the effective batch (rounding up), then validates and saves
  every 5-8 epochs - the widest interval that still leaves more validations than both
  `early_stopping_patience` and `checkpoint_best_n` require. Measured: 69 rows -> every 40 steps,
  163 -> 88, 400 -> 200, 800 -> 300 (stepped down to 6 epochs). If even 5 epochs is too wide the
  cadence is clamped to reach the floor and says so, because a cadence that silently disables both
  mechanisms is worse than a coarse one. Passing `--valid-every` or `--save-every` turns the whole
  derivation off.
- **Early stopping, patience 5.** Five consecutive validations without a new minimum end the run.
  Counted on validation lines rather than checkpoint saves, because a validation that does not
  improve writes no checkpoint and is exactly the event being counted. The stop is armed at the
  validation and fired at the next training step, so the leaderboard and periodic saves at that
  boundary complete first rather than being interrupted mid-write. A stopped run reports the step it
  stopped at and why, and is a success, not a failure - `max_steps` is a budget ceiling now, not an
  expected duration.

### Changed

- `checkpoint_best_n` 5 -> 3. Every surviving candidate costs a full sample-generation pass in
  stage 4 and a scoring pass in stage 5.
- `stream_upstream` takes an optional `should_stop`, which is how early stopping reaches a trainer
  that has no notion of it. A terminated child still exits nonzero, so the caller checks whether it
  was the one who asked before reporting a failure.

## [1.4.2] - 2026-09-06

### Fixed

- **`checkpoint_best_val_loss_*` now means what it says: exactly one of them, the lowest.**
  Upstream keeps a top-N leaderboard and names every member `best`, which is true of the set and
  false of each member - while `checkpoint_best_n` exceeds the number of validations its eviction
  gate never engages, so a run whose loss went 0.804 -> 0.843 -> 0.839 -> 0.844 left four
  directories all called best. 1.4.1 fixed the *reporting* and told readers the name did not count;
  that put the burden on whoever reads the directory. The leaderboard is worth keeping - validation
  loss does not decide which checkpoint ships, the similarity score does - so the other members are
  renamed to `checkpoint_val_loss_<step>_<loss>` after the trainer exits. Same stem, so the scorer
  reads step and loss out of either name, and both stay on the validated side of its tie-break.
  Periodic checkpoints and `checkpoint_final` are untouched.

## [1.4.1] - 2026-09-06

Voice training, audited and repaired. Every item here was found by measuring the pipeline against
a real 163-clip run rather than by reading it, and two of them were selecting the wrong checkpoint.

### Fixed

- **The pipeline reported the LAST checkpoint as the best one.** Upstream writes a
  `checkpoint_best_val_loss_<step>_<loss>` directory at every validation, improvement or not, so a
  run whose validation loss went 0.804 → 0.843 → 0.839 → 0.844 ended with four directories all
  named `best`, and the wrapper's summary named the last one. The run now reports the minimum, and
  a saved checkpoint that did not improve is labelled as saved rather than best. `checkpoint_best_n`
  drops from 5 to 3 so upstream's pruning gate can actually fire — above the number of validations
  it never did.
- **Validation was a different problem every time it ran.** The trainer re-drew the flow-matching
  timesteps, the noise and the reference-audio concatenation at every validation, so successive
  losses were not comparable: four validations spanned 0.0395 while the same loss functional's
  point-to-point noise is 0.0358. Selecting the minimum of those four was selecting noise.
  Validation now runs from a fixed seed and its loader runs in-process, and two independent runs
  agree to 1.75e-4 — 205x below the noise floor. Also returns ~1.4 GB of worker RAM.
- **The scoring stage recommended a checkpoint by alphabetical tie-break.** Three candidates that
  were byte-identical weights scored identically, and a stable sort left the PERIODIC checkpoint
  first, purely because `0` sorts before `b`. Ties now prefer a checkpoint a validation selected,
  then the lower validation loss, then the earlier step.
- **Overriding `--max-steps` silently deleted the learning-rate decay.** `warmup_steps` and
  `stable_steps` are absolute, so the documented `-- --max-steps 1000` left 1600 steps of
  warmup-plus-stable inside a 1000-step run and the cosine decay never happened. The schedule now
  scales with the template's own 5%/75% shape, and says what it derived.
- **`ref_max_seconds` 120 → 30.** Upstream's own documentation says ~30 s of reference captures
  most of the measured speaker-similarity gain; at 120 s on a 16-minute corpus the draw exhausted
  the speaker's clips and "the reference" became a large random slice of the whole dataset.

### Added

- **The dataset stage measures corpus quality and says what it found.** Clipping as a flat-top
  count rather than a grazed peak, integrated loudness (ITU-R BS.1770-4), a noise-floor SNR, lead
  and trailing silence, and real signal bandwidth. Every threshold carries its source. The training
  stage now reads that report and prints it before spending an hour, which is where a corpus with
  77 clipped clips out of 163 becomes visible.
- **Opt-in quality filters, all off by default:** `--drop-clipped`, `--min-snr`, `--min-bandwidth`.
  Removing clips from somebody's voice corpus is their decision; the report tells them what each
  flag would cost before they pass it.

### Changed

- **The engine worker declines Windows' power throttle; the trainer no longer pretends to.** The
  declination was measured at every call site: 3.02x in the synthesis worker, 1.88x generating
  samples, 1.42x encoding latents, and **1.01x on a training step**. It is gone from the training
  path rather than left inert — the trainer runs in its own process and never inherited it anyway.

## [1.4.0] - 2026-09-06

**A warm utterance now takes about 0.6 s instead of about 4.8 s — 7.5x faster, with
bitwise-identical audio** (total p50 4794 -> 636 ms, p95 -> 701 ms; `sample_rf` 4412 -> 477 ms).
A first utterance still pays the model load, 17.4 s end to end, so the "allow well over a
minute" guidance for a cold caller stands. Two of the three causes were not in our code at all,
and the third was a duplicate call we had been paying for since v1.

The panel also stopped explaining itself: configuration is a form, training is something an
agent does while the panel watches, and the copy was rewritten end to end from a review sheet.

### Changed

- **Windows was power-throttling the engine, and declining it is 3x.** With no stated policy,
  Windows' EcoQoS heuristic treats a windowless child of a console process as background work
  and parks it on an E-core at reduced clock. The synthesis loop is a single-threaded dispatch
  loop, so it paid for that placement in full: **4794 ms -> 1589 ms** per utterance from one
  `SetProcessInformation(ProcessPowerThrottling, EXECUTION_SPEED, 0)` call, and the spread
  across identical work went from **33% to 3.3%** — which is also why every latency number this
  project published before today wobbled. The worker declares the policy itself, so a worker
  started by hand behaves like one started by the runtime; the subtitle presenter and the
  training scripts do the same. `VC_ENGINE_ECOQOS=1` keeps Windows' heuristic for anyone who
  would rather have the battery. The state is read back from the OS and reported in the
  worker's `boot.device` line, because a failed syscall that logs like a success is worse than
  no log at all.
- **CUDA graphs on the sampler: another 2.4x, on by default.** The Euler loop issued about 7200
  ATen dispatches per step and the card sat idle waiting for Python — measured, not assumed:
  quadrupling the DiT work cost 1.3% more wall time. Capturing the step takes a warm utterance
  from **1511 ms to 636 ms** (`sample_rf` 1360 -> 477 ms) and the **per-step cost from 40.1 ms
  to 8.6 ms**, for **+292 MiB allocated / +788 MiB peak reserved** (peak now 3194 MiB:
  comfortable on 8 GiB, workable on 6). Capture is paid per utterance, not per process, because
  the latent length comes from the duration predictor and almost never recurs — **+116 ms**, so
  break-even is 3.7 steps and every real utterance wins. Output is bitwise identical, and
  `VC_ENGINE_CUDA_GRAPHS=0` returns to eager with output identical to the code before the
  change: the same utterance measured 747 ms against 2542 ms with the **same sha256**.
  **A capture that fails does not fail the utterance**: the reason is logged once, the process
  samples eagerly for the rest of its life at the previous release's speed and VRAM, and the log
  says so. Verified by forcing capture to raise — audio still bitwise identical.
  Two things had to be fixed in the engine before capture was possible at all, both recorded in
  the fork: a `torch.tensor(10000.0, device=cuda)` rebuilt on every model evaluation (a
  host-to-device copy 32 times per utterance, which capture rejects outright) and an accumulator
  that silently promotes to fp32 on the first step, so that step stays eager.
- **The engine is now a maintained fork**
  ([yabo083/Irodori-TTS, branch `voice-core`](https://github.com/yabo083/Irodori-TTS/tree/voice-core),
  from upstream `8224daf`; decision in `docs/adr/0002-engine-fork.md`). Upstream Irodori-TTS is
  MIT and excellent; these patches belong in its sampler, not in a wrapper around it, and a 2.4x
  should not wait on a merge. `origin` stays pristine upstream and our work is a branch, so
  `git diff origin/main` is exactly the patch series and `git rebase origin/main` is how it
  follows upstream. `FORK.md` names the upstream commit, each patch with its measurement, and
  the one command that returns the tree to pristine. LICENSE is byte-identical and no upstream
  file gained a copyright line.
- **`encode_conditions` ran twice per utterance.** Same arguments, once from the runtime and
  once from the sampler: **78 ms** back, bitwise identical.

### Added

- **设置 is a visual configuration manager.** Forms, switches, colour wells, segmented choices
  and steppers over `data/config.json`, auto-saving with a debounce, validated twice (in the UI
  and again in the writer, because a hand-edited file never passes through the UI), and written
  through the same span splice that keeps every comment, key order and BOM in that file intact.
  Version history is a bounded record of the **changes** — which key, before, after — capped at
  50 in `data/settings.history.jsonl`, with a restore that re-splices the inverse edit and gets
  the bytes back exactly. It is not a pile of timestamped file copies.
- **音色 opens a page per pack.** Identity, subtitle style with a live preview, synthesis
  parameters, expression defaults and a 试听 — all writing the pack's own `voicepack.json`,
  which is the file that wins. Keys this build has never heard of survive a save, and every
  field says whether the value came from the pack, from `config.json`, or from a derivation.
  The list rows now show the portrait itself rather than the path to it, served as a `data:` URL
  from the backend so the webview keeps no filesystem access at all.
- **`docs/api.md` is the published HTTP reference** for anyone building on the runtime: every
  route, the error envelope, the event stream, and the two costs to design around (a cold start
  measured in tens of seconds, and a single-tenant GPU). It is the one documentation file that
  ships in the install and lives in the repository; the rest of `docs/` stays on the machine it
  is written on. Installs therefore grow a `docs\` folder holding exactly this file — a fresh
  install gets it from the installer, and an updated one gets it when the tree is replaced.
- **One agent skill became two, and the installer puts them where an agent already looks.**
  `skills/voice-core-tts/` is for speaking, `skills/voice-core-voice-training/` for making a
  pack; `skills/voice-core/` is gone, with no stub. A daily `speak` call no longer injects the
  whole training pipeline into someone's context, and the speaking skill stopped keeping a
  second copy of the HTTP surface — route table, request fields, event kinds and examples are
  `docs/api.md`'s job, and two copies of one contract only drift. The installer also writes
  both to `%USERPROFILE%\.agents\skills\<name>\SKILL.md`, so an agent finds them by name
  instead of being handed a path; ours are overwritten on upgrade and removed on uninstall.
- **状态 gained 使用说明: two sentences to copy, one per skill.** That is the whole card. The
  panel used to hand an agent a wall of prose and PowerShell — the 训练提示词 card on 训练,
  now deleted along with the builder behind it. The skill file is the artefact; the panel's job
  is to say which one and where it is.

### Removed

- **The 使用方式 card on 状态.** It existed to hand an agent a copy-paste call, which is the
  shipped skills' job — `skills/voice-core-tts/` and `skills/voice-core-voice-training/`.
  数据目录 and 安装目录 moved into 环境, where "what is installed and where" already lives.
- **`data\backups\` is no longer written or read.** The version history above replaced it. Any
  copies already in there are left exactly where they are — they are a user's recoverable copies
  of their own configuration and deleting them silently is not a refactor's business — but
  nothing adds to them from now on, and the directory can be removed by hand.

### Fixed

- **Simultaneous setting writes lost changes, and the history recorded ones that never
  landed.** Each write read `config.json`, spliced its own value in and replaced the file, so
  five writes issued together raced: measured on the previous build, one was rejected outright
  (`os error 5`, the replace hitting another writer's temp file), only one of four colours
  reached the file, and the record kept an entry for a key that never got there — a history
  offering to restore something that had never been written. Both write paths now serialise on
  one lock across read → splice → replace → record: five for five, every key in `written`, five
  entries that all describe a change the file actually took.
- **Clicking the sidebar never announced the screen change.** The rail called the shell's
  `show()` directly instead of going through `navigate()`, so `app:navigate` was dispatched by
  in-page jumps only and the two things listening for it were dead on the path users actually
  take: 设置 kept showing values from boot after `config.json` changed underneath it, and 音色
  re-entered on an open pack page instead of the list, so the rail disagreed with what was
  under it.
- **An error message named a document the caller cannot have.** A rejected `dialog` sent a
  third-party caller to `docs/voicepack-spec.md`, which is an internal note that ships nowhere;
  it now names `"Appearance: dialog"` in `docs/api.md`, which travels with the install.
- **The Start Menu kept three shortcuts from 1.1.0, one of them broken.** Dropping an entry from
  the installer's `[Icons]` does not take it off a machine that already has it, so 初始化向导,
  环境诊断 and 托盘控制台 survived every upgrade — and 托盘控制台 pointed at
  `bin\app\VoiceCoreTray.exe`, a path that stopped existing when the presenter moved and was
  renamed, so clicking it produced a Windows error. The installer now deletes all three by name.
  A fresh install of 1.4.0 over 1.3.0 leaves exactly two: the app and its uninstaller.

## [1.3.0] - 2026-09-05

Three things a caller and an owner both asked for: train a voice inside the app, see which file
decided each setting, and stop guessing when a line has finished playing. `apiVersion` stays `1`
for every existing route; one route and two event kinds are added.

### Added

- **Training, in the app (训练).** The six-step pipeline that used to be a PowerShell session —
  dataset → latents → LoRA → samples → similarity → install — runs as one supervised job with
  real progress: a stage rail, live step/loss/s-per-step/ETA, a capped log console, and a
  checkpoint table that ranks candidates by validation loss and speaker-similarity lower bound
  so the pick is the measured one rather than the last one. Installing is a separate, explicit
  action, because training produces several candidates and choosing is a decision.
  Four knobs are exposed (batch size, step budget, learning rate, checkpoint interval); the
  rest of the YAML stays frozen, and the screen says why (`num_workers: 2` and persistent
  workers are what keep a Windows dataloader from starving the GPU, and the `model` block has
  to match the base checkpoint). Before the first GPU step the job asks the runtime to release
  the card, so training and speaking never fight over VRAM. Starting a run that would delete
  checkpoints no pack was installed from is **refused** and names what is at risk.
- **The progress protocol is now one protocol.** `scripts/training/**` grew a `--json` mode that
  emits the same seven keys `scripts/bootstrap.ps1 -Json` emits, and the line-streaming child
  runner behind provisioning was factored out (`manager/src-tauri/src/jsonstream.rs`) so both
  features share one implementation of spawning, reading, job-object ownership and what a
  non-zero exit means. tqdm is parsed in Python, next to the process that writes it — never in
  Rust — and stderr is merged into stdout, which is what keeps a full pipe from hanging a
  50-minute trainer.
- **Configuration, visible (配置).** `data/config.json` and `data/runtime.json` shown verbatim
  with JSONC comments intact, plus a pack's own `voicepack.json`, plus an effective-value table
  that says for every field whether the answer came from the pack, from `config.json`, or from
  a derivation. The provenance is the runtime's own verdict: `src/packs.rs::hydrate` records
  who won each field *as it merges*, and `GET /api/voices` carries that map. The screen renders
  it and computes nothing — comparing the two files in the UI would be a second implementation
  of the one rule this screen exists to make visible. Read-only by design; `config_edit`'s
  span splice stays the only writer.
- **`speak --wait`: the caller learns when the audio actually finished.** `POST /api/played`
  lets whoever played the audio report in, `playbackStarted`/`playbackFinished` join the event
  stream, and both players report — the subtitle presenter and the CLI's own local playback —
  so a caller never has to know which one spoke. `--wait-timeout-ms` bounds it and a timeout
  exits non-zero naming what was not observed. This replaces `time.sleep(7.5)`, which is what
  narrating three paragraphs in order used to require.
- **`[pause:N]` in the text.** A documented pause primitive, 1–10000 ms: the utterance is split
  at the marker, each segment is synthesised, and the PCM is joined with exactly N ms of
  silence — one `audioId`, one `speech` event, `durationMs` counting the silence. Markers at
  the very start or end are dropped rather than producing dead air; adjacent markers sum; a
  malformed marker is an `invalid_request` naming the offending text instead of being read out
  loud.
- **`--ruby-pairs -`** reads the alignment array from stdin, which removes Windows command-line
  quoting from the problem. `@file.json` still works.
- **Optional `language` on speak, validated.** When it does not match the resolved pack's
  declared languages the call fails with `voice_language_unsupported`, naming the pack, what it
  declares and what was asked — instead of feeding Chinese text to a Japanese-only model and
  producing noise. Engine routing on `language` is deliberately not implemented; this is the
  field, the check and the error code, so callers can be written against them now.
- **Per-process VRAM that is actually per-process.** `nvidia-smi --query-compute-apps` returns
  `[N/A]` for every row on a GeForce in WDDM mode, so the panel could not attribute video
  memory. It now reads the same PDH counter Task Manager uses
  (`\GPU Process Memory(pid_*)\Dedicated Usage`) summed over the engine process tree: 2463 MiB
  from the panel against 2464 MiB from the OS's own reader for the same PID.

### Changed

- **1.1 GiB of video memory came back.** With the model loaded, reserved VRAM was 3178 MiB
  against 1755 MiB allocated; per-synthesis peak reserved was 3468 MiB. A single
  `empty_cache()` once the load finishes — releasing the fp32→bf16 transient, not per-utterance
  churn — brings those to **2078 MiB** and **2408 MiB**. `expandable_segments:True` was doing
  nothing here: PyTorch on Windows warns it is unsupported, and the two other allocator knobs
  measured identical to three digits, because the overhang was never fragmentation.
- **Cold model load 23.0 s → 19.2 s.** The load is now instrumented sub-stage by sub-stage
  (checkpoint read, module build, state dict, device move, cast, tokenisers, codec) instead of
  being one opaque number, and building the DiT no longer initialises weights that a
  `strict=True` load overwrites a moment later. Audio is bitwise identical across both changes,
  and what remains is transformers' own lazy-import machinery, not initialisation.
- **`scripts/package.ps1`** ships the training kit as before; `scripts/bench/**` is a
  development harness and is deliberately not packaged.

### Fixed

- **A panel screen no longer shows a stale answer after the service starts.** Both new screens
  refetch on the transition — the effective-value table and 后端占用 were computed once at mount,
  so starting the runtime afterwards left the panel confidently claiming the runtime was down
  and the card was empty.
- **The latents step encoded nothing, on any corpus.** It shelled out to upstream's
  `prepare_manifest.py`, which loads audio through HuggingFace `datasets`; `datasets` 4.x
  decodes audio only through `torchcodec`, and `torchcodec` cannot load its natives without
  FFmpeg shared libraries this install does not carry — so every row died with
  `dataset_iter_error` and training could never start. The step now loads the engine's own
  `DACVAECodec` in-process and encodes the dataset itself, with upstream's preprocessing in
  upstream's order (`encode_waveform` is what resamples to 48 kHz, downmixes and normalises).
  The latents it writes are **bitwise identical** to the ones upstream produced for the same
  clips — shape, dtype and every element, verified across six clips against latents encoded
  before this change. `--dataset`/`--split`/`--config`/`--data-files` retired with the
  subprocess and are now rejected by name.

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
