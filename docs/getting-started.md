# Getting started

voice-core ships the app and nothing else — no engine, no model weights, no Python
environment, no voices. That is deliberate: the weights are 4.8 GiB and are not ours to
redistribute. One script provisions the rest.

## What you need first

| | |
|---|---|
| OS | Windows 10 or 11 |
| GPU | An NVIDIA GPU with a current driver. The Irodori backend runs the model on CUDA in bf16; there is no CPU path. Reference machine: RTX 5060 Ti 16 GB |
| Disk | About 11 GiB free: 4.8 GiB of weights, plus the engine's virtualenv (torch + CUDA wheels), plus headroom |
| Tools | [git](https://git-scm.com/download/win) is required. [uv](https://astral.sh/uv) is strongly recommended — it is how the engine's own README installs it, and it provisions its own Python |

Everything else, including the Python interpreter, is installed for you.

## Install

Download the single `voice-core-<version>-setup.exe` from the
[releases page](https://github.com/yabo083/voice-core/releases) and run it.

The installer is **not code-signed**, so Windows SmartScreen will warn that the publisher is
unrecognised. The release notes publish the installer's SHA256; verify it if you want:

```powershell
Get-FileHash .\voice-core-1.0.0-setup.exe -Algorithm SHA256
```

Prefer to build it yourself? See the README's Build section — then run the same bootstrap.

## Provision the backend

```powershell
# See what your machine is missing. Downloads nothing, changes nothing.
.\scripts\bootstrap.ps1 -CheckOnly

# Do it.
.\scripts\bootstrap.ps1
```

The installer offers to run this on its last page. Six stages, each idempotent — re-running
after a failure costs only the stage that failed:

1. **Preflight.** Every check reports PASS/WARN/FAIL with the remedy for a FAIL. `-CheckOnly`
   stops here.
2. **Engine source.** Clones [Irodori-TTS](https://github.com/Aratako/Irodori-TTS) (MIT) at a
   pinned revision, and [DACVAE](https://github.com/facebookresearch/dacvae) (Apache-2.0)
   beside it. Pinned rather than tracking a branch on purpose: the worker talks to the
   engine's Python API, and an engine that changes shape silently breaks synthesis.
3. **Virtualenv.** `uv sync --extra cu128` inside the engine clone, which is upstream's own
   instruction. Without `uv`, falls back to `python -m venv` plus pip against the CUDA wheel
   index. This is the slow stage: several GB of wheels.
4. **Weights**, about 4.8 GiB, all MIT, into the Hugging Face cache under
   `models/huggingface`:
   `Aratako/Irodori-TTS-v4.1-Small` (3.1 GiB, the checkpoint),
   `sbintuitions/modernbert-ja-310m` (1.3 GiB, the Japanese text encoder),
   `Aratako/Semantic-DACVAE-Japanese-32dim` (0.4 GiB, the 48 kHz codec).
   Resumable: a re-run continues rather than restarting.
5. **Layout.** Writes `data/runtime.json` with **relative** paths, so the whole tree can be
   zipped, moved or copied to another machine and still work.
6. **Smoke test.** Starts the runtime, loads the model, prints what it cost, stops.

On the reference machine the first load is the slow one — 34 s with a cold page cache, 13.8 s
warm — and the torch import before it is 3-9 s. Both are printed by the smoke test and
recorded in `data/logs/tts-worker.out.log` as `boot.imports` and `model.load.done`, with the
VRAM the checkpoint occupies (1.76 GiB allocated, 3.18 GiB reserved).

## You still have no voice

A provisioned install can start, but it cannot speak until at least one **voice pack** is
registered. A pack is one of three things, and they need very different amounts of work:

| Kind | Needs | Gives you |
|---|---|---|
| `reference-audio` | one or more clips, no text | timbre |
| `speaker-embedding` | ~80 clips of one speaker, no per-clip text | timbre |
| `lora-adapter` | audio **plus a transcript in the same language as the audio** | timbre and prosody |

**[docs/training-a-voice.md](training-a-voice.md)** is the full pipeline.

## Run it

```powershell
bin\app\VoiceCoreTray.exe          # the tray, which starts the runtime itself
bin\voice-core.exe doctor          # reachability, auth, backend state, packs
bin\voice-core.exe speak --voice <pack-id> --text "<what to say>" --display "<what to show>"
```

The tray reads the audio aloud and puts a subtitle dialog on screen; you write no playback
code. An AI agent should read **[skills/voice-core/SKILL.md](../skills/voice-core/SKILL.md)**
instead of this page — it is the calling contract, including the alignment between the spoken
text and the displayed text.

## Settings

One file: `data/config.json`. It holds the dialog's appearance, the global hotkeys and the
voice-pack registry, it accepts `//` comments and trailing commas, and the tray's
*设置（含声线包）* menu entry opens it. The pack section is re-read whenever the file changes,
so adding a voice needs no restart; the dialog and hotkey sections are read at tray startup.

`data/runtime.json` is a different file with a different owner: bootstrap writes it, the
runtime reads it, and it holds where the engine lives. You should not need to touch it.

## When a stage fails

| Symptom | Where to look |
|---|---|
| `warm` fails, or a speak returns `model_load_failed` | `data/logs/tts-worker.err.log` — the engine's own reason is in there verbatim, including its traceback |
| The runtime will not start | `data/logs/runtime.err.log` |
| The tray says the runtime is not running while it is | The tray and the runtime disagree about the data directory; check the `token.txt` path the tray reports |
| Something is slow and you want to know which part | `data/logs/tts-worker.out.log` has a timed line per stage; `data/metrics.jsonl` has one per synthesis |

## One backend today, not forever

The Irodori backend is the one that is best at Japanese, and it is the only one implemented.
voice-core itself is not Japanese-only: a backend is any process that speaks the loopback
protocol, and every voice pack already declares which `engine` it needs and which `languages`
it can say. See [docs/adr/0001-tts-engine-backend-seam.md](adr/0001-tts-engine-backend-seam.md) for what a
second backend would take.
