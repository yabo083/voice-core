# voice-core

A local voice-output runtime for AI agents. Text in, spoken audio out — played on the
machine, with a subtitle dialog on screen. It runs entirely on your own hardware: no
account, no API key, no network egress. The HTTP surface binds loopback by default
(`127.0.0.1:8760`), and the bearer token is the only auth it has.

The problem it solves is narrow on purpose. An agent that wants to *speak* otherwise has
to know where Python is, which virtualenv holds the model, how much VRAM is free, which
port the engine got, and how to draw a subtitle. voice-core owns all of that and gives
the agent one POST:

```bash
curl -s -X POST http://127.0.0.1:8760/api/speak \
  -H "Authorization: Bearer $(cat data/token.txt)" \
  -H 'Content-Type: application/json' \
  -d '{"text": "おかえりなさい、先生。",
       "displayText": "Welcome back, sensei.",
       "voicePackId": "my-voice"}'
```

The audio plays and the subtitle appears. `text` is what gets synthesized; `displayText`
is what the human reads. Translation is the caller's job — the runtime never translates.

## One entry point

`VoiceCore.exe` is the app, and the only executable a user launches. It starts the backend and
the subtitle presenter itself, owns the tray icon, hides to the tray when its window is closed,
and stops its children only when you explicitly quit it. A second launch focuses the window
that is already open.

```
VoiceCore.exe                       the app: tray, settings, provisioning    (Tauri 2)
 |-- bin\presenter\                 subtitle dialog, hotkeys, wheel gesture  (WinUI 3)
 `-- bin\voice-core-runtime.exe     the service on 127.0.0.1:8760            (Rust, axum)
      `-- runtime\worker\irodori\   the engine                               (Python, FastAPI)
```

That tree is supervision: each parent spawns its child and takes it down with it. The contract
between them runs the other way, and only downward:

```
presenter   bin\presenter · bin\voice-core.exe · your agent   knows no ports, models or venvs
   |   commands over HTTP        ^ events over SSE
runtime     voice-core-runtime.exe   (Rust, axum)             knows nothing about presentation
   |   spawns + loopback HTTP
worker      worker/irodori/worker.py (Python, FastAPI)        knows nothing about who called
```

The runtime never calls a frontend back. Presenters subscribe to `GET /api/events`
(Server-Sent Events) and that is the entire presentation contract (`src/lib.rs:1-12`).

Two invariants are load-bearing:

- **Audio never travels inside JSON.** `POST /api/speak` returns an `audioId`; the bytes
  come from `GET /api/audio/{audioId}` as `audio/wav`. The worker writes its WAV straight
  into a runtime-owned spool path, so no base64 exists anywhere and no process holds a
  second copy of the samples (`worker/irodori/worker.py:16-18`).
- **The runtime owns the engine process.** It is assigned to a Win32 job object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so a crashed or killed runtime cannot leave a
  multi-gigabyte GPU process orphaned (`src/supervise.rs:405-451`).

### Why `voice-core.exe` stays

The CLI is an agent's tool, not a second launcher, and it gets no shortcut. Delete it and three
things stop working that the GUI cannot do on an agent's behalf:

- **It resolves the token and the data directory by itself**, using the runtime's own rule, so
  a script needs no configuration and no `--data-dir`: `speak` works from any cwd, on a
  packaged install or a dev checkout.
- **It plays the audio when nothing else will.** `--play auto` (the default) plays only when the
  runtime reports zero event-stream subscribers, so an agent that speaks before the GUI is up,
  or on a machine with no presenter at all, is still audible — and never doubles the audio when
  the presenter *is* subscribed (`src/bin/voice-core.rs:167-171`, `src/obs.rs:172-176`).
- **`doctor` is a one-command diagnosis** — reachability, auth, engine state and voice packs in
  one output. It is the first thing to ask for in a bug report.

## What you need

| | |
|---|---|
| OS | Windows 10 20H1 (build 19041) or newer, x64 (`app/VoiceCoreTray/VoiceCoreTray.csproj:5,18`) |
| GPU | An NVIDIA GPU. The worker runs the model on CUDA in bf16; there is no CPU path today. Reference machine: RTX 5060 Ti 16 GB |
| Toolchain | Rust (2021 edition), .NET 8 SDK with the Windows App SDK workload, and Node.js for the GUI's Vite frontend |
| Engine | **Not shipped**, but provisioned for you. [Irodori-TTS](https://github.com/Aratako/Irodori-TTS) v4.1-Small plus its virtualenv |
| Weights | **Not shipped**, but provisioned for you. ~4.8 GiB: `Aratako/Irodori-TTS-v4.1-Small` (3.1 GiB), `sbintuitions/modernbert-ja-310m` (1.3 GiB), `Aratako/Semantic-DACVAE-Japanese-32dim` (0.4 GiB) |
| Voices | **Not shipped and not provisionable.** A voice pack is trained or cloned from audio you supply — the training kit is `scripts/training/` |

## Install

Download the single `voice-core-<version>-setup.exe` from the
[releases page](https://github.com/yabo083/voice-core/releases), run it, and let its last page
launch `VoiceCore.exe`. The installer is **not code-signed**, so SmartScreen will warn that the
publisher is unrecognised; the release notes publish the installer's SHA256. Exactly one Start
Menu shortcut is created, and it points at `VoiceCore.exe`.

`VoiceCore.exe` renders in WebView2, which is present by default on Windows 11 and current
Windows 10. Setup checks for it and points you at Microsoft's download if it is missing, rather
than fetching ~100 MB behind your back.

Provisioning happens inside the app, which detects what this machine already has — an engine
tree, a virtualenv, a Hugging Face cache — and downloads only what is missing. It is the same
script either way, so it can also be driven from a shell:

```powershell
.\scripts\bootstrap.ps1 -CheckOnly   # environment report; downloads nothing
.\scripts\bootstrap.ps1              # engine, virtualenv, weights, layout, smoke test
```

The getting-started guide in the development tree walks the same path in prose, with the
measured cost of each stage and what to read when one fails.

A missing engine or model is reported, never crashed on: startup continues, the missing paths
appear in `GET /api/status` under `worker.missing`, and a synthesis attempt fails with
`worker_start_failed` or `model_load_failed` naming the actual path.

## Build

```powershell
cargo build --release                                                  # runtime + client
cd app\VoiceCoreTray; dotnet build -c Release -p:Platform=x64          # presenter
cd manager; npm install; .\node_modules\.bin\tauri build --no-bundle   # VoiceCore.exe
```

`--no-bundle` on purpose: `scripts/package.ps1` plus Inno Setup is this project's bundler, and
Tauri's own MSI/NSIS output would be built and thrown away. `manager/src-tauri` is its own
crate with its own `target/`, deliberately outside the root Cargo workspace.

```powershell
cargo test          # pipeline, auth, shutdown-with-subscriber, percentiles
```

`cargo test` needs no GPU and no models: a fake engine stands in for the worker
(`tests/speak_pipeline.rs`).

## Run

`VoiceCore.exe` is the human entry point, and the only one. It starts the runtime and the
subtitle presenter, closing its window hides it to the tray, and quitting is an explicit action
that stops both children. The runtime is launched with a single `--data-dir` argument and no
engine knowledge.

```powershell
manager\src-tauri\target\release\VoiceCore.exe
```

The presenter is the same executable a developer can still run alone. Without `--presenter` it
keeps the tray icon, context menu and status window it had in 1.1.0; with `--presenter` it is
the subtitle surface and nothing else, and it never starts or stops a runtime. Either way its
lifetime is independent of the backend's: stopping the runtime leaves it running, and it
re-subscribes when a runtime reappears.

```powershell
app\VoiceCoreTray\bin\x64\Release\net8.0-windows10.0.22621.0\VoiceCorePresenter.exe
```

Headless, for an agent:

```powershell
target\release\voice-core-runtime.exe `
  --tts-python <path>\irodori-tts\env\Scripts\python.exe `
  --tts-root   <path>\irodori-tts

target\release\voice-core.exe speak --text "おかえりなさい、先生。" --display "Welcome back, sensei." --voice my-voice
target\release\voice-core.exe events    # subtitles, engine state, progress
target\release\voice-core.exe doctor    # reachability, auth, engine, voice packs
```

`voice-core-runtime.exe --print-layout` prints every resolved path with an `ok`/`MISSING`
marker, and prints the diagnosis even when the engine cannot be resolved at all.

### Portable install

```powershell
.\scripts\package.ps1                                    # binaries only
.\scripts\package.ps1 -IncludeEngine -IncludeModels -Zip  # self-contained, ~15 GB
```

The output is a tree — `VoiceCore.exe` at the root, with `bin/`, `runtime/`, `models/`, `data/`
and `skills/` beside it — containing no absolute paths: every location is derived from the
executable's own position, so the folder survives being zipped, moved, or copied to another
machine. The root holds exactly one executable, and `package.ps1` asserts that before it
reports success. A bundled virtualenv is repointed at its shipped interpreter automatically,
since Windows venvs record an absolute `home`.

Read the deployment notes in the development tree before packaging: `-IncludeEngine` and
`-IncludeModels` copy third-party code and weights, and `scripts/package.ps1:60` defaults to
bundling voice packs from a local directory. See *Licensing* below.

## Settings

One file: **`data/config.json`**. Three sections — the dialog's appearance, the global
hotkeys, and the voice-pack registry. It is a plain file meant to be opened in an editor;
`VoiceCore.exe` rewrites the `voicePacks` section when you add or remove a voice and leaves the
rest of the file, comments included, exactly as it found it.

```jsonc
{
  "dialog": {
    "annotationAbove": false,   // spoken line above or below the line being read
    "reveal": "typewriter"      // typewriter | sweep | fade
  },
  "hotkeys": {
    "toggleDialog": "Ctrl+Alt+D",
    "toggleHold":   "Ctrl+Alt+H"
  },
  "voicePacks": [ /* id, kind, path, character, avatar */ ]
}
```

It is JSONC: `//` and `/* */` comments and one trailing comma are accepted, and a UTF-8 BOM
is tolerated, because this file exists to be edited by hand in Notepad
(`src/jsonc.rs:1-25`). `voicePacks` is re-read by the runtime whenever the file's mtime
changes, so adding a voice needs no restart; `dialog` and `hotkeys` are read once when the
presenter starts (`src/packs.rs:91-107`).

`data/runtime.json` is where the engine's location lives, and provisioning writes it. Relative
paths resolve against the install root, which is what keeps a provisioned tree portable;
absolute paths are honoured as they are, which is what lets an install reuse an engine, a
virtualenv or a model cache that already exists elsewhere on the machine instead of
downloading ~4.8 GiB a second time.

Other things in the data directory: `token.txt` (the bearer token, minted on first run),
`logs/` (`runtime.{out,err}.log`, `tts-worker.{out,err}.log`, `dialog.jsonl`),
`metrics.jsonl` (per-utterance latency), `spool/` (generated audio, cleared on restart).

## For agents

**[`skills/voice-core/SKILL.md`](skills/voice-core/SKILL.md)** is the agent-facing contract
and the only place agent-facing instructions live, so nothing here duplicates it: it ships
inside the portable install, and an agent that finds the tree learns the surface from it
without this repository. It covers the shortest call that produces sound, the cold-start
latency an unprepared caller reads as a hang, the two-text convention and the `rubyPairs`
alignment array, how to list and pick a voice, the error-code table with the action for each
code, a symptom-to-action table for a runtime that is down, undeployed, unregistered or
already holding the port, and how to register a trained pack. Voice training itself stays in
the training kit under `scripts/training/`, which it points at.

The HTTP surface, the subtitle overlay's behaviour (reveal presets, annotation layout, the
Hold state machine, the window model) and the architecture decisions behind them are written
down in this project's `docs/`, which is **development documentation and stays on the
development machine** — it is neither committed nor shipped. `SKILL.md` above is written to
stand alone, and `bin\voice-core.exe --help` plus `--print-layout` answer the rest from an
install.

## What this is not

- **Not an LLM.** It performs no inference beyond text-to-speech and generates no text.
- **Not a dialogue manager.** It holds no conversation state and decides nothing about what
  to say or when.
- **Not a translator.** `text` and `displayText` are both produced by the caller.
- **Not a cloud service.** Loopback only. Nothing is uploaded, and `HF_HUB_OFFLINE=1` is set
  for the worker (`worker/irodori/worker.py:48-49`).
- **Not speech recognition.** TTS only. Adding STT later is an `SttEngine` trait beside
  `TtsEngine` plus a worker — additive, not a redesign.
- **Not a model distributor.** No weights, no voice packs, no game assets. See *Licensing*.

## Platform support

Windows-only today, and honestly so:

- The subtitle presenter is **WinUI 3**. Its overlay is ~500 lines of Win32 geometry —
  `WM_NCHITTEST` caption regions, `WM_ERASEBKGND`, `DWMWA_WINDOW_CORNER_PREFERENCE`,
  `DesktopAcrylicController`, `WS_EX_NOACTIVATE` — plus `RegisterHotKey` for the global
  hotkeys and a `WH_MOUSE_LL` hook for the wheel gesture. None of that ports; it would be a
  rewrite, not a port.
- Process supervision is a **Win32 job object**. The Rust code is `#[cfg(windows)]`-gated,
  so the runtime would compile elsewhere, but the kernel-enforced guarantee that a dead
  runtime cannot orphan a GPU process exists only on Windows (`src/supervise.rs:34,405`).
- `VoiceCore.exe` is Tauri 2, so its own code is portable, but it renders in **WebView2** and
  supervises a WinUI presenter. Nothing about it is cross-platform in this build.

A headless port is plausible — the runtime, the client and the worker have no GUI
dependency — and would need a cgroup or process-group equivalent for the job object. Neither
the presenter nor the GUI would come with it.

## Known gaps

Not implemented, and not pretended to be:

- **Browsing does not interrupt a line that is still speaking.** Wheel-up over the dialog
  walks the backlog once the current line has finished — the history badge steps and the band
  swaps to the replay control — but the gesture does not fire while audio is playing. The
  suspected cause is UI-thread starvation from the typewriter and growth timers: one
  32-character line records 86 resizes and 448 ms of resize time in `data/logs/dialog.jsonl`,
  on the same thread that owns the low-level mouse hook. Judged non-essential; the interrupt
  path itself is implemented and does run from the 历史 control on the dialog itself.
- **Mid-step cancellation.** `DELETE /api/requests/{id}` frees the caller immediately and
  guarantees the utterance is never delivered, but the engine finishes its current step
  before the GPU permit is released.
- **Real streaming audio.** One `audioId` per utterance. The byte endpoint can become
  chunked without changing any client.
- **One backend.** Irodori is best at Japanese and is the only implementation. The seam for
  another is real but the routing is not wired: every pack already declares its `engine` and
  `languages`, and nothing reads them yet (`src/engine.rs`; ADR-0001 in the development tree).

## Licensing

voice-core is licensed under **GPL-3.0-or-later**. The full, unmodified licence text is in
[`LICENSE`](LICENSE). Every redistributed third-party component is listed with its licence in
[`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).

**The thing that is easy to get wrong here.** Almost everything voice-core links against is
MIT, Apache-2.0 or BSD — 168 Rust crates, the .NET packages, the Irodori-TTS engine (MIT),
DACVAE (Apache-2.0), all three model checkpoints (MIT). Those are GPL-3.0-compatible in one
direction only: their code may be combined into a GPL-3.0 work, and in exchange **their
copyright and permission notices must be reproduced in every distribution**. A GPL project
that ships a binary with no notices file is not "more free", it is in breach of the permissive
licences it consumed. That is what `THIRD-PARTY-NOTICES.md` is for, and it must ship inside
every release artefact, not only live in the repository.

**One genuine conflict, in the subtitle presenter only — resolved by an exception.**
`VoiceCoreTray.csproj:22` sets `WindowsAppSDKSelfContained=true`, so the presenter's build output —
which `scripts/package.ps1` copies into the package — contains Microsoft's Windows App SDK
redistributables. Those are governed by the Microsoft Software License Terms, whose §3(c)(ii)
forbids distributing the distributable code under any licence that requires source disclosure
or grants recipients the right to modify, and whose §3(b)(ii) requires imposing protective
terms on downstream recipients. GPL-3.0 §10 forbids exactly that kind of further restriction,
and the Windows App SDK is not a GPL-3.0 §1 "System Library" — it is not part of the normal
packaging of Windows, which is why `WindowsAppSDKSelfContained` exists at all.

GPL-3.0 §7 anticipates this case, and the copyright holder has granted the additional
permission it provides for: see **[`LICENSE-EXCEPTION.md`](LICENSE-EXCEPTION.md)**, which
permits linking with the Windows App SDK and distributing its unmodified redistributables.
`LICENSE` itself stays the verbatim GPL text — an exception is stated separately, never by
editing the licence. Microsoft's own terms still bind anyone who redistributes those
components, so `license.txt` and `NOTICE.txt` travel with the binaries in `bin/presenter/`.
The runtime, the client and the worker have no Windows App SDK dependency and never needed
the exception; the source publication was never affected either way, because no Microsoft
code is in this repository.

**What this repository deliberately does not contain.** `.gitignore` keeps `data/`, `dist/`,
`target/` and the presenter's build output out of version control, and that is load-bearing rather
than tidiness: those directories are where game audio, trained voice packs, speaker
embeddings, character portraits and model weights actually live on a development machine.
Voice packs trained on copyrighted game voice data are for personal use and must never enter a
published artefact — source or binary. If you add a new kind of artefact, check it against
`.gitignore` before committing.

The bundled subtitle typeface, LXGW WenKai, is under the SIL Open Font License 1.1, which
requires its copyright notice and licence to accompany every copy. Both live in
`app/VoiceCoreTray/assets/fonts/OFL.txt` and are copied to the build output alongside the
font.
