# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The HTTP surface carries its own version, `apiVersion`, which is bumped only on a breaking
change to the public contract and is independent of the release version below
(`src/service.rs:26-27`).

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
