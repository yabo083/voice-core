# voice-core

A local voice-output runtime for AI agents. Text in, spoken audio out — played on the
machine, with a subtitle dialog on screen. It runs entirely on your own hardware: no
account, no API key, no network egress. The HTTP surface is bound to loopback only
(`127.0.0.1:8760`, `docs/api.md:7`).

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

## Three processes

```
presenter   voice-core.exe · VoiceCoreTray.exe · your agent   knows no ports, models or venvs
   |   commands over HTTP        ^ events over SSE
runtime     voice-core-runtime.exe   (Rust, axum)             knows nothing about presentation
   |   spawns + loopback HTTP
worker      worker/irodori/worker.py (Python, FastAPI)        knows nothing about who called
```

Dependencies point downward only; the runtime never calls a frontend back. Presenters
subscribe to `GET /api/events` (Server-Sent Events) and that is the entire presentation
contract (`docs/api.md:11-18`, `src/lib.rs:1-12`).

Two invariants are load-bearing:

- **Audio never travels inside JSON.** `POST /api/speak` returns an `audioId`; the bytes
  come from `GET /api/audio/{audioId}` as `audio/wav`. The worker writes its WAV straight
  into a runtime-owned spool path, so no base64 exists anywhere and no process holds a
  second copy of the samples (`docs/api.md:13-15`, `worker/irodori/worker.py:16-18`).
- **The runtime owns the engine process.** It is assigned to a Win32 job object with
  `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`, so a crashed or killed runtime cannot leave a
  multi-gigabyte GPU process orphaned (`src/supervise.rs:405-451`).

Two binaries, deliberately: `voice-core-runtime` runs, `voice-core` controls
(`Cargo.toml:10-16`).

## What you need

| | |
|---|---|
| OS | Windows 10 20H1 (build 19041) or newer, x64 (`app/VoiceCoreTray/VoiceCoreTray.csproj:5,13`) |
| GPU | An NVIDIA GPU. The worker runs the model on CUDA in bf16; there is no CPU path today. Reference machine: RTX 5060 Ti 16 GB |
| Toolchain | Rust (2021 edition) and .NET 8 SDK with the Windows App SDK workload |
| Engine | **Not shipped.** [Irodori-TTS](https://github.com/Aratako/Irodori-TTS) v4.1-Small, plus a Python virtualenv for it |
| Weights | **Not shipped.** ~4.7 GB: `Aratako/Irodori-TTS-v4.1-Small` (3.1 GB), `sbintuitions/modernbert-ja-310m` (1.3 GB), `Aratako/Semantic-DACVAE-Japanese-32dim` (0.4 GB) |
| Voices | **Not shipped.** A voice pack is a LoRA adapter directory or a speaker-embedding file that you train or clone yourself; `skills/voice-core/SKILL.md` §6 documents the process end to end |

Nothing is downloaded for you. First-run model provisioning is not implemented (see
*Known gaps*), so the weights must already be on disk. A missing engine or model is
reported, never crashed on: startup continues, the missing paths appear in
`GET /api/status` under `worker.missing`, and a synthesis attempt fails with
`worker_start_failed` or `model_load_failed` naming the actual path
(`docs/deployment.md:103-114`).

## Build

```powershell
cargo build --release                                    # runtime + client
cd app\VoiceCoreTray; dotnet build -c Release -p:Platform=x64   # tray
```

```powershell
cargo test          # pipeline, auth, shutdown-with-subscriber, percentiles
```

`cargo test` needs no GPU and no models: a fake engine stands in for the worker
(`tests/speak_pipeline.rs`).

## Run

The tray is the human entry point. It launches the runtime with a single `--data-dir`
argument and no engine knowledge, and the two have independent lifetimes — stopping the
runtime leaves the tray running, and the tray reconnects when a runtime reappears.

```powershell
app\VoiceCoreTray\bin\x64\Release\net8.0-windows10.0.22621.0\VoiceCoreTray.exe
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
marker, and prints the diagnosis even when the engine cannot be resolved at all
(`docs/deployment.md:62-64`).

### Portable install

```powershell
.\scripts\package.ps1                                    # binaries only
.\scripts\package.ps1 -IncludeEngine -IncludeModels -Zip  # self-contained, ~15 GB
```

The output is a tree — `bin/`, `runtime/`, `models/`, `data/`, `skills/` — containing no
absolute paths: every location is derived from the executable's own position, so the folder
survives being zipped, moved, or copied to another machine. A bundled virtualenv is
repointed at its shipped interpreter automatically, since Windows venvs record an absolute
`home` (`docs/deployment.md:94-101`).

Read [`docs/deployment.md`](docs/deployment.md) before packaging: `-IncludeEngine` and
`-IncludeModels` copy third-party code and weights, and `scripts/package.ps1:60` defaults to
bundling voice packs from a local directory. See *Licensing* below.

## Settings

One file: **`data/config.json`**. Three sections — the dialog's appearance, the global
hotkeys, and the voice-pack registry. The tray's *设置（含声线包）* menu entry opens it.

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
changes, so adding a voice needs no restart; `dialog` and `hotkeys` are read once at tray
startup (`src/packs.rs:91-107`, `docs/dialog-presenter.md:208-210`).

`data/runtime.json` is a separate, optional file that overrides where the engine lives. It
is only needed for dev checkouts and installs that keep the engine elsewhere; a packaged
install has none (`docs/deployment.md:66-86`).

Other things in the data directory: `token.txt` (the bearer token, minted on first run),
`logs/` (`runtime.{out,err}.log`, `tts-worker.{out,err}.log`, `dialog.jsonl`),
`metrics.jsonl` (per-utterance latency), `spool/` (generated audio, cleared on restart).

## For agents

**[`skills/voice-core/SKILL.md`](skills/voice-core/SKILL.md)** is the agent-facing contract,
and it ships inside the portable install so an agent that finds the tree can learn the
surface without this repository. It covers the two-text convention, the `rubyPairs`
alignment array, the error-code table and what to do about each code, and how to train and
register a new voice.

The full HTTP surface is documented once, in [`docs/api.md`](docs/api.md). The subtitle
overlay's behaviour — reveal presets, annotation layout, the Hold state machine, the window
model — is in [`docs/dialog-presenter.md`](docs/dialog-presenter.md).

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

- The tray is **WinUI 3**. Its subtitle overlay is ~500 lines of Win32 geometry —
  `WM_NCHITTEST` caption regions, `WM_ERASEBKGND`, `DWMWA_WINDOW_CORNER_PREFERENCE`,
  `DesktopAcrylicController`, `WS_EX_NOACTIVATE` — plus `RegisterHotKey` for the global
  hotkeys and a `WH_MOUSE_LL` hook for the wheel gesture. None of that ports; it would be a
  rewrite, not a port (`docs/dialog-presenter.md:222-295`).
- Process supervision is a **Win32 job object**. The Rust code is `#[cfg(windows)]`-gated,
  so the runtime would compile elsewhere, but the kernel-enforced guarantee that a dead
  runtime cannot orphan a GPU process exists only on Windows (`src/supervise.rs:34,405`).

A headless port is plausible — the runtime, the client and the worker have no GUI
dependency — and would need a cgroup or process-group equivalent for the job object. The
tray would not come with it.

## Known gaps

Not implemented, and not pretended to be:

- **First-run model provisioning.** No download, no disk-space preflight. The weights must
  already be on disk.
- **Mid-step cancellation.** `DELETE /api/requests/{id}` frees the caller immediately and
  guarantees the utterance is never delivered, but the engine finishes its current step
  before the GPU permit is released (`docs/api.md:183-190`).
- **Real streaming audio.** One `audioId` per utterance. The byte endpoint can become
  chunked without changing any client.

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

**One genuine conflict, in the tray only — resolved by an exception.**
`VoiceCoreTray.csproj:17` sets `WindowsAppSDKSelfContained=true`, so the tray build output —
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
components, so `license.txt` and `NOTICE.txt` travel with the binaries in `bin/app/`.
The runtime, the client and the worker have no Windows App SDK dependency and never needed
the exception; the source publication was never affected either way, because no Microsoft
code is in this repository.

**What this repository deliberately does not contain.** `.gitignore` keeps `data/`, `dist/`,
`target/` and the tray's build output out of version control, and that is load-bearing rather
than tidiness: those directories are where game audio, trained voice packs, speaker
embeddings, character portraits and model weights actually live on a development machine.
Voice packs trained on copyrighted game voice data are for personal use and must never enter a
published artefact — source or binary. If you add a new kind of artefact, check it against
`.gitignore` before committing.

The bundled subtitle typeface, LXGW WenKai, is under the SIL Open Font License 1.1, which
requires its copyright notice and licence to accompany every copy. Both live in
`app/VoiceCoreTray/assets/fonts/OFL.txt` and are copied to the build output alongside the
font.
