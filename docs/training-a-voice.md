# Training a voice — Irodori (Japanese) backend

You have audio of a voice you like. This is how it becomes a voice pack voice-core can
speak with.

Everything on this page is specific to the **Irodori-TTS v4.1-Small backend**, the one
voice-core ships against today. Its text encoder is a Japanese ModernBERT, so it reads
Japanese; a backend for another language would arrive with its own weights, its own
training story and its own page. What stays constant is the *pack contract* — a kind, a
path, an `engine` and a `languages` list in `data/config.json` — which is why the last
step of this page (`install_pack.py`) is the one script here that is not
Irodori-specific.

Scripts live in `scripts/training/`:

| | |
|---|---|
| `irodori/prepare_dataset.py` | folder of clips + transcripts → dataset file + QA report |
| `irodori/encode_latents.py` | dataset file → DACVAE latents + training manifest (wraps upstream) |
| `irodori/run_training.py` | runs upstream `train.py` with the right environment (wraps upstream) |
| `irodori/lora.yaml`, `irodori/speaker-embedding.yaml` | annotated config templates |
| `irodori/generate_samples.py` | fixed-seed samples from every checkpoint |
| `irodori/evaluate_similarity.py` | speaker similarity against your corpus, with a ceiling |
| `install_pack.py` | copy the pack in and register it |

---

## 1. Pick a pack kind. This is the decision that matters.

| kind | audio you need | text you need | learns | cost |
|---|---|---|---|---|
| `reference-audio` | **1 clip**, ≥1 s | **none** | timbre only | seconds, no training |
| `speaker-embedding` | **~80 clips** | **none per clip** (see below) | timbre, as a reusable file | ~1000 steps, minutes |
| `lora-adapter` | **~60–70 clips ≈ 15 min** | **a transcript per clip, in the same language as the audio** | timbre *and* delivery — prosody, pacing, style | 2000 steps, 50–90 min |

Two clarifications, both measured rather than assumed:

**Timbre is not the hard part.** In the reference run, speaker-embedding, one-clip cloning
and eight-clip cloning were indistinguishable on timbre similarity — all 0.77–0.82. If all
you want is "sounds like her", the free option is already as good as the trained one. What
a LoRA buys is *delivery*: it scored 0.815–0.818 with a spread of ±0.002 against the
embedding's ±0.028, and the difference you actually hear is that it stops drifting.

**"No text" is not the same as "no `text` field".** The manifest schema always requires a
`text` key — the loader rejects a row without one
(`irodori_tts/dataset.py:296`) — but a speaker-embedding run learns 16 speaker vectors and
nothing about words, so the text does not have to be a real transcript. Pass
`--placeholder-text "これはサンプル音声です。"` and every row gets that one sentence. The
reference 80-clip embedding was trained exactly this way and came out usable. Do **not**
do this for a LoRA: an adapter trained on placeholder text has learned nothing about how
this voice says anything.

### Why the transcript language must match the audio

The text encoder is `sbintuitions/modernbert-ja-310m`. A LoRA learns a mapping from *that
encoder's Japanese embeddings* to this speaker's sound. Feed it Chinese transcripts over
Japanese audio and it learns a mapping from a text domain you will never generate from —
at synthesis time you will hand it Japanese, and the mapping does not hold there. This was
measured, on an 80-clip dataset with Chinese subtitles: it is recorded in the project's
training playbook as the reason that dataset was rebuilt with Japanese transcripts. The
audio being Japanese is not enough. The *text* has to be the language the encoder reads.

---

## 2. The audio format question, answered

**No format is rejected.** The codec resamples any sample rate to its own and
mean-downmixes any channel count to mono before encoding
(`irodori_tts/codec.py::encode_waveform`), and upstream's encoder hands it your file's
native rate untouched. There is no `--min-sample-rate` in play by default, no channel
requirement, no bit-depth requirement.

**One format loses nothing:**

```
48000 Hz, mono, 16-bit PCM WAV (or better: 24-bit, or float)
```

48 kHz because that is the codec's native rate — upstream documents
Semantic-DACVAE-Japanese-32dim as a 48 kHz codec, and every WAV the engine produces is
48 kHz mono 16-bit PCM (checked by reading the RIFF headers). 44.1 kHz works: the
reference dataset was 44.1 kHz Vorbis and trained fine. But the band above 22 kHz is
simply absent from a 44.1 kHz file and upsampling cannot invent it, so if you are
*choosing* an export setting, choose 48 kHz.

**Duration is the one real bound.** The trainer truncates each target latent at
`max_latent_steps: 750` frames, and the codec runs at **25 latent frames per second**
(48000 Hz / hop 1920 — verified against a real 69-clip manifest, where `num_frames`
equalled `ceil(duration × 25)` for every row). So:

- **longer than 30.0 s** → the audio is cut but the transcript is not, and the model is
  taught that this much text takes that much less time. `prepare_dataset.py` skips these
  by default.
- **shorter than 1.0 s** → below the trainer's own reference floor (`ref_min_seconds`).
  Skipped by default; `--min-seconds 0` keeps them.
- **text longer than 256 tokens** → silently truncated by the tokenizer
  (`irodori_tts/tokenizer.py:111-116`). `prepare_dataset.py` flags long lines using a
  character-count proxy, because tokens are not characters.

Formats `prepare_dataset.py` can measure: WAV, FLAC, OGG/Vorbis, Opus, MP3, AIFF
(libsndfile's repertoire). Convert m4a/aac/wma to WAV first — it refuses to guess at a
file it cannot inspect.

---

## 3. Before you start

Provision the engine and the weights once:

```powershell
.\scripts\bootstrap.ps1
```

That clones upstream to `runtime\engine\webui\Irodori-TTS`, creates its virtualenv with
`uv sync --extra cu128` (so the interpreter is
`runtime\engine\webui\Irodori-TTS\.venv\Scripts\python.exe`), and downloads ~4.7 GB of
weights into `models\huggingface`. The scripts here find all of that themselves; every one
of them accepts `--engine-root`, `--hf-home` and `--python` if your tree is somewhere
else, and prints what it resolved before doing anything.

Set a variable for the interpreter, because every command below uses it:

```powershell
$py = ".\runtime\engine\webui\Irodori-TTS\.venv\Scripts\python.exe"
```

The similarity harness needs two packages that are deliberately **not** engine
dependencies:

```powershell
uv pip install --python $py resemblyzer webrtcvad-wheels
# a venv built by `python -m venv` instead of uv has pip:  & $py -m pip install resemblyzer webrtcvad-wheels
```

Re-running `uv sync` prunes anything not in the lockfile, so if you re-provision, install
these again.

Keep your training scratch **outside** the install tree — the audio, the latents and the
checkpoints are yours and none of it belongs in a release archive. Something like
`E:\voice-training\<voice>\`. The latents and the manifest that names them must move
together: a relative `latent_path` resolves against the manifest's own directory.

---

## 4. The pipeline

### Step 1 — dataset file and QA

```powershell
& $py .\scripts\training\irodori\prepare_dataset.py `
    --audio-dir E:\voice-training\my-voice\wav `
    --transcripts E:\voice-training\my-voice\transcripts.csv `
    --speaker-id my-voice `
    --out-dataset E:\voice-training\my-voice\my-voice.dataset.jsonl
```

Transcripts can be a directory of `<clip>.txt` sidecars (or sidecars sitting beside the
audio, in which case drop the flag), or a `.csv` / `.tsv` / `.jsonl` / `.json` mapping clip
name to text. A bare `.txt` file of lines is rejected on purpose: pairing lines with clips
by position mislabels the entire dataset the moment one clip is missing.

`--speaker-id` matters for a LoRA. With it, the trainer draws a *different* clip of the
same voice as the reference for each training sample; without it, each clip becomes its own
reference and the speaker condition is masked off (`irodori_tts/dataset.py:314-349`).

Read the QA report it writes beside the dataset file. It names every clip it dropped and
why, and flags clipping, sub-48 kHz rates and over-long lines. Also worth knowing: upstream
normalises the text before training (NFKC, `？！`→`?!`, `...`→`…`, and a `「」` pair wrapping
a whole line is stripped). Do not pre-strip those, and do not rely on them surviving.

For a speaker-embedding run instead:

```powershell
& $py .\scripts\training\irodori\prepare_dataset.py `
    --audio-dir E:\voice-training\my-voice\wav `
    --placeholder-text "これはサンプル音声です。" `
    --out-dataset E:\voice-training\my-voice\se.dataset.jsonl
```

### Step 2 — encode latents

```powershell
& $py .\scripts\training\irodori\encode_latents.py `
    --dataset-file E:\voice-training\my-voice\my-voice.dataset.jsonl `
    --latent-dir   E:\voice-training\my-voice\latents `
    --out-manifest E:\voice-training\my-voice\train_manifest.jsonl
```

Add `--check` first: it validates every path, resolves the checkpoint and the codec, and
prints the exact upstream command without running it.

This runs upstream's own `prepare_manifest.py` rather than reimplementing it. The trainer
reads latents, not audio, so this pass is where your audio is consumed. Each output row is

```json
{"text": "…", "latent_path": "latents/00000000_00000000.pt", "num_frames": 84, "speaker_id": "my-voice:my-voice"}
```

`num_frames` is the latent's own length in frames — 25 per second of audio — and is what
the loader uses for length bucketing and for picking reference clips. `latent_path` is
relative to the manifest.

If the row count that comes out is lower than the row count that went in, the wrapper says
so loudly. Upstream skips unusable rows by design (`empty_text`, `audio_decode`,
`low_sample_rate`, `trimmed_empty`, `encode_error` — each means something different), and a
run that quietly encoded 3 of your 60 clips otherwise looks like a success.

### Step 3 — train

```powershell
& $py .\scripts\training\irodori\run_training.py `
    --config lora `
    --manifest   E:\voice-training\my-voice\train_manifest.jsonl `
    --output-dir E:\voice-training\my-voice\lora `
    --log        E:\voice-training\my-voice\train.log
```

`--config lora` picks `scripts\training\irodori\lora.yaml`; `--config speaker-embedding`
picks the other template. Read whichever one you use — every value that a person would
sensibly change carries a comment saying why it is what it is. Both templates are
value-for-value the configs the reference runs used (the LoRA one identically; the
speaker-embedding one differs from upstream's template in three places, each commented).

`--check` prints every resolved path, a summary of what the config will do, and the command
line, and launches nothing. Use it once before committing an hour.

Two things this wrapper does that you would otherwise have to remember: it resolves
`--init-checkpoint` (both LoRA and Speaker Inversion refuse to start without it, and it is
a snapshot path inside the HuggingFace cache), and it sets `HF_HOME`, `HF_HUB_CACHE`,
`HF_HUB_OFFLINE=1` and `PYTHONUNBUFFERED=1`. Offline matters: pristine upstream asks the
hub for the text encoder with `local_files_only=False`, and the environment variable is
what keeps that read local. Verified on transformers 5.16.1 / huggingface_hub 1.29.0 —
with the caveat that the revision the config pins must be the one in your cache. If you
edit `text_encoder_revision` to something you have not downloaded, offline will fail no
matter what.

With `--log`, follow the run from another window:

```powershell
Get-Content -Wait -Tail 20 E:\voice-training\my-voice\train.log
```

Without it the output stays on the console, where tqdm's progress bar is live.

### Step 4 — generate comparison samples

```powershell
& $py .\scripts\training\irodori\generate_samples.py `
    --lora    E:\voice-training\my-voice\lora `
    --no-ref `
    --out-dir E:\voice-training\my-voice\gen
```

Pointed at the run directory, this sweeps **every** checkpoint under it and generates the
same texts with the same fixed seed for each one. That is the whole point: hold the text
and the seed still, and the difference between two WAVs is the difference between two
checkpoints.

`--no-ref` adds an unconditioned run. Keep it — it is the cross-speaker *floor*, and
without it you have no idea whether 0.7 is a good number.

Three neutral Japanese lines are used by default (greeting, short declarative, longer
polite, so a checkpoint that only works at one sentence length is visible). `--texts-file`
takes one line per text.

### Step 5 — score them

```powershell
& $py .\scripts\training\irodori\evaluate_similarity.py `
    --label my-voice-sweep `
    --ref-dir E:\voice-training\my-voice\wav `
    --tests   "E:\voice-training\my-voice\gen\*.wav" `
    --out-dir E:\voice-training\my-voice\results
```

The method, unchanged from the run that produced this project's numbers:

- a speaker-verification model with **no relationship to the generator** (Resemblyzer's
  GE2E encoder, 256-d d-vectors at 16 kHz). Scoring a TTS model with its own encoder
  measures agreement, not similarity;
- cosine similarity of every generated clip against every reference clip;
- **leave-one-out over your reference corpus as the ceiling** — each real clip against all
  the other real clips. That is what "the same human twice" scores, and a generated clip is
  not supposed to beat it;
- judgement by the **lower bound** of the distribution, not the mean.

Output is one row per checkpoint, sorted by lower bound, with a flag on any group whose
worst clip falls below the corpus's own LOO p10.

### Step 6 — install the pack

```powershell
& $py .\scripts\training\install_pack.py `
    --pack E:\voice-training\my-voice\lora\checkpoint_best_val_loss_0001000_0.885155 `
    --id my-voice --name "My Voice (LoRA)" --character "My Voice" `
    --avatar avatars\my-voice.png
```

This copies the pack to `data\voicepacks\my-voice\` and adds one entry to `voicePacks` in
`data\config.json`. `--dry-run` shows exactly what it would copy and the exact JSON it
would insert.

That config file is JSONC written for a human to read, and the tray is its other writer, so
the registration is a surgical splice: comments, key order, trailing commas, line endings
and any byte-order mark all survive, and an existing entry with the same `id` is replaced
rather than duplicated. It refuses to write anything it cannot re-parse.

The runtime re-reads `voicePacks` whenever the file's mtime changes
(`src/packs.rs::reload_if_changed`, called on every voices listing and every pack lookup),
so **no restart is needed**. Confirm:

```powershell
.\bin\voice-core.exe voices
.\bin\voice-core.exe speak --text "こんにちは。" --voice my-voice
```

Notes on what gets copied:

- **LoRA**: the adapter's files, minus `trainer_state.pt` — that is ~202 MiB of optimizer
  state that inference never reads, and it is only useful if you intend to `--resume` that
  run. `--keep-trainer-state` keeps it. What remains is `adapter_config.json` plus a
  ~100 MiB `adapter_model.safetensors`, which is what the engine actually needs.
- **Speaker embedding**: the file, **under its original name**. The
  `.speaker.safetensors` suffix is load-bearing — the engine refuses a file without it by
  name (`Speaker Inversion embeddings must use the '.speaker.safetensors' suffix`), and
  renaming a pack to a bare id is a mistake that has actually been made here. Both
  `install_pack.py` and `generate_samples.py` refuse such a file up front rather than
  letting the engine fail later.
- **Reference audio**: the clip. Note the current limit: the registry's `path` is a single
  string, so a registered `reference-audio` pack is exactly **one** clip, even though the
  engine can concatenate several and the worker's wire accepts a list. Multi-clip
  reference packs need a registry change, not a workaround.

---

## 5. Choosing the checkpoint

**The last step is not the answer.** In the reference LoRA run, the best validation loss
was at step **1000**; step 2000 had overfit, the similarity numbers dropped, and the
val-loss curve agreed. That is why `lora.yaml` sets `valid_ratio: 0.05`,
`valid_every: 500` and `checkpoint_best_n: 5`: the trainer keeps the five best-by-val-loss
adapters as `checkpoint_best_val_loss_<step>_<loss>\` directories, and the loss is in the
name. Do not set `valid_ratio: 0` — no validation split means no best-checkpoint
selection, which removes the entire selection mechanism.

Then confirm the choice by ear and by similarity. Val loss picks the candidate; the
similarity sweep from steps 4–5 tells you whether the candidate is stable.

Speaker Inversion runs have no validation split (upstream's template does not use one), so
there `checkpoint_final` is merely the last step. Compare the waypoints — `save_every: 100`
over 1000 steps gives ten of them, and each is 49,264 bytes, so keeping them all costs
nothing.

## 6. The acceptance criterion

Compare your **distribution's lower bound** against your corpus's LOO ceiling.

The reference corpus (80 clips) scored a LOO mean of **0.771** with **p10 0.703**. Against
that ceiling:

- the best LoRA checkpoint sat at **0.815–0.818**, spread **±0.002** — above the natural
  ceiling and, more importantly, tight;
- a batch whose **minimum fell to 0.651** had dropped through the natural p10, and that is
  exactly the audible failure: "it goes off-key every few lines". The batch's *mean* looked
  fine.

So the rule is: **the worst clip in the batch must not fall below your corpus's LOO p10.**
`evaluate_similarity.py` computes both numbers and flags the groups that fail. Your corpus
has its own ceiling — compare against yours, not against 0.771.

## 7. Training performance: the discipline, and the one fix that matters

The reference run started at a **20% GPU duty cycle** (30 samples, 2–4 s apart, with the
CPU at only 30%) and ended at **99–100%**, steady at 1.9–2.3 s/step for batch 16 — about
6.4 samples/s, 2000 steps in 50–90 minutes on an RTX 5060 Ti 16 GB.

The fix was **`dataloader_persistent_workers: true`**, not more workers. A few hundred
clips at batch 16 is ~4 batches per epoch, so the epoch boundary comes around constantly,
and on Windows every boundary re-spawns each worker — 1–3 s apiece, because each one
re-imports torch. The GPU was starving between epochs.

More workers is not free, and was not the fix. Each Windows DataLoader worker is **~700 MB
resident** and persistent workers hold it for the entire run. `num_workers: 8` across a
train and a valid loader meant 16 worker processes and **~11.4 GB**, taking a 32 GB machine
to 29.7 GB. For a few-hundred-clip dataset, **`num_workers: 0–2` is enough**, which is what
the templates use. `num_workers: 0` is legal — the trainer then ignores the two
dataloader flags and says so.

The discipline that produced those numbers, and that any change to them has to follow:

1. **Diagnose before touching anything.** A throughput baseline, a bottleneck hypothesis,
   the evidence, the proposed change, the expected gain, and the way back.
2. **Understand before tuning.** An idle GPU does not mean "add workers". Work out how much
   of a step is actually GPU compute first.
3. **One variable at a time**, re-measured the same way. No gain → revert, and say so.
4. **State the RAM/VRAM delta before the change.** Do not push the machine to the edge of
   OOM.
5. **Never touch a running job.** Config changes take effect next run.
6. **Report as numbers**: baseline → change → new numbers → kept or reverted. Not "this
   should be faster".

Measuring duty cycle, not single readings:

```powershell
1..30 | ForEach-Object { nvidia-smi --query-gpu=utilization.gpu,memory.used --format=csv,noheader,nounits; Start-Sleep 2 }
Get-Process python | Measure-Object -Sum WorkingSet64   # worker RAM, summed
```

## 8. Reference numbers

Everything measured on one machine (RTX 5060 Ti 16 GB, Windows 11, 32 GB RAM) with the
reference dataset. Treat them as calibration, not as guarantees.

| | |
|---|---|
| usable LoRA dataset | 69 clips + 62 clips ≈ 15 minutes |
| speaker-embedding dataset | 80 clips, 1000 steps, 16 tokens |
| LoRA training | batch 16, 1.9–2.3 s/step, 2000 steps in 50–90 min |
| best LoRA checkpoint | step 1000 by val loss (2000 had overfit) |
| LoRA similarity | 0.815–0.818, ±0.002 |
| speaker-embedding similarity | 0.77–0.82, ±0.028 |
| 1-clip / 8-clip cloning | 0.77–0.82 — indistinguishable from the embedding |
| corpus LOO ceiling | mean 0.771, p10 0.703 |
| audible failure | batch minimum 0.651 |
| adapter on disk | ~100 MiB (+ ~202 MiB of resume state you do not need) |
| embedding on disk | 49,264 bytes |

## 9. When it goes wrong

| symptom | cause |
|---|---|
| `LoRA fine-tuning requires --init-checkpoint` | you called upstream `train.py` directly; `run_training.py` resolves it |
| `ModuleNotFoundError: torch` | wrong interpreter; the scripts print the ones they looked for |
| manifest points at latents that are not there | the manifest and its latent folder were separated; `latent_path` is relative to the manifest |
| `Speaker Inversion embeddings must use the '.speaker.safetensors' suffix` | the embedding was renamed; restore the suffix |
| encoding produced far fewer rows than you gave it | read the skip counts; usually `empty_text` or `audio_decode` |
| GPU at 2–12% with the CPU idle too | `dataloader_persistent_workers` is off, or `num_workers: 0` with a slow disk |
| RAM near capacity during training | `num_workers` too high; ~700 MB each, doubled by the valid loader |
| a `FileNotFoundError` naming a HuggingFace repo id | `HF_HOME`/`HF_HUB_CACHE` are not pointing at your cache; use the wrappers, or their `--hf-home` |
| offline failure on the text encoder | the pinned `text_encoder_revision` is not in your cache; download it or pass `--allow-hf-download` |
| the new voice is not in `voice-core.exe voices` | `config.json` did not parse — the runtime keeps the last good list and says why once on stderr |

## 10. What this page is not

It is not "how voice-core makes sound". It is how *this* backend is trained. The engine
seam is a trait with one implementation today (`src/engine.rs`), the loopback protocol
between the runtime and a worker is engine-agnostic, and every pack already carries
`engine` and `languages` — the two fields a second backend would be routed by. When a
backend for another language lands, it brings its own weights, its own data requirements
and its own page beside this one; nothing here is the only way this system can ever speak.
