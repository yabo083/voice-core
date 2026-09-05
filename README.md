# voice-core

Local text-to-speech for AI agents on Windows: one HTTP call in, audio on the speakers and a subtitle on screen. No account, no API key, no network egress.

## What it is

voice-core owns Python environments, model weights, VRAM management, and subtitle presentation behind one HTTP POST:

```bash
curl -s -X POST http://127.0.0.1:8760/api/speak \
  -H "Authorization: Bearer $(cat data/token.txt)" \
  -H 'Content-Type: application/json' \
  -d '{"text": "おかえりなさい、先生。",
       "displayText": "Welcome back, sensei.",
       "voicePackId": "my-voice"}'
```

`VoiceCore.exe` is the single entry point. It launches the runtime service and WinUI 3 subtitle presenter, supervises child processes via Win32 Job Objects (so a killed parent cannot orphan GPU processes), and manages the tray icon.

```
VoiceCore.exe                       the app: tray, provisioning, settings, training  (Tauri 2)
 |-- bin\presenter\                 subtitle dialog, hotkeys, wheel gesture          (WinUI 3)
 `-- bin\voice-core-runtime.exe     the service on 127.0.0.1:8760                    (Rust, axum)
      `-- runtime\worker\irodori\   the engine worker                                (Python, FastAPI)
           `-- runtime\engine\      Irodori-TTS v4.1-Small, our fork                 (PyTorch, CUDA)
```

The synthesis engine is a maintained fork of [Irodori-TTS](https://github.com/Aratako/Irodori-TTS) (MIT) with inference-latency patches.

## What it does

- **Dual-text synthesis:** `text` is synthesized; `displayText` is displayed on the subtitle overlay; `rubyPairs` aligns them segment by segment.
- **Prosody & expression:** `[pause:N]` (1–10000 ms) inserts exact silence; `emotion` captions condition voice delivery without being spoken.
- **Presenter overlay:** WinUI 3 subtitle dialog with typewriter/sweep/fade reveal, custom colours, and global hotkeys (`Ctrl+Alt+D` toggle, `Ctrl+Alt+H` hold).
- **Voice packs:** Supports LoRA adapters, speaker embeddings, and reference audio described by `voicepack.json`.
- **GPU housekeeping:** Lazy model loading, manual warmup (`POST /api/warm`), explicit release (`POST /api/sleep`), and idle reclamation (900 s default).
- **Events & completion:** `GET /api/events` (SSE) streams subtitles, engine state, and progress; `POST /api/played` reports playback completion.
- **CLI & diagnosis:** `bin\voice-core.exe` for agent CLI control and `bin\voice-core.exe doctor` for one-command environment diagnosis.

## What you need

| | |
|---|---|
| OS | Windows 10 20H1 (build 19041) or newer, x64 (Windows only: presenter uses WinUI 3; process supervision uses Win32 job objects) |
| GPU | NVIDIA GPU with CUDA support in bf16 (peak reserved VRAM: 3194 MiB with CUDA graphs, ~2406 MiB eager; reference: RTX 5060 Ti 16 GB, i5-12600KF) |
| WebView2 | Required for `VoiceCore.exe` (present by default on Windows 11 and current Windows 10) |
| Engine & Models | Provisioned on first run (~4.8 GiB: Irodori-TTS v4.1-Small, modernbert-ja-310m, Semantic-DACVAE-Japanese-32dim) |
| Voices | Not shipped; train or clone your own using `scripts\training\` |
| Disk | ~150 MB binaries (~15 GB for a self-contained portable tree with engine and weights) |

## Install

Download `voice-core-<version>-setup.exe` from [releases](https://github.com/yabo083/voice-core/releases) and run it. The installer creates a Start Menu shortcut to `VoiceCore.exe`. On first launch, the 部署 screen detects existing dependencies and downloads missing components.

You can also run provisioning from PowerShell:

```powershell
.\scripts\bootstrap.ps1              # engine, virtualenv, weights, layout, smoke test
```

### Portable

```powershell
.\scripts\package.ps1                                   # binaries, notices, skills, docs\api.md
.\scripts\package.ps1 -IncludeEngine -IncludeModels     # self-contained, ~15 GB
```

The second form needs to be told where the engine and weights are — `-EngineVenv`, `-EngineRoot`
and `-ModelCache`, or the `VC_ENGINE_VENV` / `VC_ENGINE_ROOT` / `VC_MODEL_CACHE` environment
variables. It refuses rather than guessing. The output tree holds no absolute paths, so it
survives being moved to another machine; a bundled virtualenv is repointed at its shipped
interpreter on first start. See [Licensing](#licensing) before publishing one.

## Quick start

1. Launch `VoiceCore.exe` and let 部署 complete.
2. Put a voice on the machine — register a pack on 音色, or train one on 训练 (no voice pack ships by default).
3. The shortest call that produces sound:
   ```powershell
   bin\voice-core.exe speak --text "おかえりなさい、先生。" --display "Welcome back, sensei." --voice my-voice
   ```
4. Check environment health: `bin\voice-core.exe doctor`.

> **Cold start:** Model loading on first use takes tens of seconds (17–50 s). Once resident, p50 latency is ~636 ms (p95 ~701 ms). Call `POST /api/warm` or `bin\voice-core.exe warm` to pay the load in advance.

### For an agent
- [`skills/voice-core-tts/SKILL.md`](skills/voice-core-tts/SKILL.md) — Agent contract for speaking, CLI flags, `--wait`, ruby alignment, and error recovery.
- [`skills/voice-core-voice-training/SKILL.md`](skills/voice-core-voice-training/SKILL.md) — Agent contract for running the six-step training pipeline in `scripts\training\`.

## HTTP API

[`docs/api.md`](docs/api.md) (also shipped at `docs\api.md`) is the complete HTTP contract. The service binds to `127.0.0.1:8760` by default and requires `Authorization: Bearer <token>` from `data/token.txt` (except unauthenticated `GET /api/health`).

Two performance characteristics to design around:
- **Cold start cost:** First load takes tens of seconds; warm synthesis p50 is 636 ms (CUDA graphs) or ~2.5 s (eager fallback).
- **Single-tenant GPU:** Synthesis is serialized behind a permit; concurrent requests queue and return `resource_busy` (429) on timeout.

## Project layout

### Repository

```
src/                    Rust runtime service (axum), CLI, engine seam, pack registry, supervision
worker/irodori/         Python engine worker: four HTTP routes, one WAV per call
app/VoiceCoreTray/      WinUI 3 subtitle presenter (VoiceCorePresenter.exe)
manager/                Tauri 2 desktop app (VoiceCore.exe)
scripts/                bootstrap.ps1 (provisioning), package.ps1 (bundler), training/
skills/                 Agent skills (voice-core-tts, voice-core-voice-training)
docs/api.md             Published HTTP API contract
tests/speak_pipeline.rs Integration tests against a mock engine (no GPU required)
```

### Packaged install

```
VoiceCore.exe           Desktop app entry point
bin\                    voice-core-runtime.exe, voice-core.exe, presenter\VoiceCorePresenter.exe
runtime\                python\ (venv), worker\irodori\worker.py, engine\
models\huggingface\     Model weights
data\                   token.txt, config.json, voicepacks\, logs\, spool\, metrics.jsonl
skills\                 voice-core-tts\, voice-core-voice-training\
docs\                   api.md
scripts\                bootstrap.ps1, training\
```

## Extending it (二开)

- **Add a TTS engine backend:** The runtime communicates with the worker over four HTTP routes (`GET /health`, `POST /load`, `POST /unload`, `POST /synthesize`). Details are in [`docs/api.md`](docs/api.md) §Engine contract. Attach a custom worker with `--tts-python <python.exe> --tts-script <script.py> --tts-root <root>` or connect to an external server via `--tts-url <url>`.
- **Voice packs (`voicepack.json`):** A pack defines its portrait, subtitle styling (`dialog`), and synthesis/emotion parameters in `voicepack.json`, overriding `data\config.json` defaults.
- **Desktop panel (`manager/`):** Tauri 2 app with frontend types and IPC commands centralized in `manager/src/ipc.ts`.
- **Engine fork:** Cloned at `runtime\engine\webui\Irodori-TTS` (branch `voice-core`). Optimizations (single condition encoding, CUDA graph step replay) are documented in `FORK.md`.

## Build from source

Prerequisites: Rust (2021 edition), .NET 8 SDK with Windows App SDK workload, and Node.js.

```powershell
cargo build --release                                                  # runtime + CLI
cd app\VoiceCoreTray; dotnet build -c Release -p:Platform=x64          # presenter
cd manager; npm install; .\node_modules\.bin\tauri build --no-bundle   # VoiceCore.exe
```

> **Build trap:** `VoiceCore.exe` must be built via `tauri build --no-bundle` (not plain `cargo build`) to embed frontend assets via the `custom-protocol` feature.

```powershell
cargo test                       # pipeline, auth, shutdown, percentiles (no GPU needed)
cd manager; npm run typecheck    # frontend typecheck
```

## Known gaps

- **Browsing during playback:** Wheel-up over the subtitle window walks history only after the current line finishes playing.
- **Mid-step cancellation:** `DELETE /api/requests/{id}` frees the caller immediately, but the engine finishes its active step before releasing the GPU permit.
- **Audio streaming:** Utterances are returned as complete WAV files per `audioId` rather than chunked streams.
- **Single active backend:** Irodori-TTS is currently the only dispatched engine implementation.

## Licensing

- **voice-core:** Licensed under [GPL-3.0-or-later](LICENSE).
- **Third-party notices:** Required copyright and license notices for permissive dependencies are reproduced in [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md).
- **Windows App SDK exception:** The presenter includes an additional permission under GPL-3.0 §7 in [`LICENSE-EXCEPTION.md`](LICENSE-EXCEPTION.md) for linking with Microsoft Windows App SDK redistributables.
- **Engine fork:** Upstream Irodori-TTS MIT license is retained.
- **Fonts & assets:** Bundled LXGW WenKai font is licensed under SIL Open Font License 1.1 (`app/VoiceCoreTray/assets/fonts/OFL.txt`). No copyrighted voice data or weights are committed.
