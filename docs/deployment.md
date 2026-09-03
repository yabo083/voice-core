# Deployment layout

Everything the runtime needs is found relative to its own executable. No
absolute path is written into the tree, so an unzipped folder works after being
moved, renamed, or copied to another machine.

## Production tree

```
voice-core/
  bin/
    voice-core-runtime.exe          the service
    voice-core.exe                  the client
    app/VoiceCoreTray.exe           the GUI presenter (+ WindowsAppSDK files)
  runtime/
    python/                         engine virtualenv
    python-base/                    the interpreter that venv was built from
    worker/irodori/worker.py        engine worker
    engine/webui/                   engine source tree
  models/
    huggingface/hub/...             weights (HF cache layout)
  data/
    token.txt                       minted on first run
    config.json                     all settings; `voicePacks` paths relative to data/
    voicepacks/                     LoRA adapters (dirs) and embeddings (files)
    logs/                           runtime.{out,err}.log, tts-worker.{out,err}.log
    spool/                          generated audio, process-lifetime only
    runtime.json                    OPTIONAL layout override
  skills/
    voice-core/SKILL.md           the agent-facing contract, shipped with the tree
```

Build it with `scripts/package.ps1`; see the header comment for switches.

## How paths are resolved

`voice-core-runtime` computes an **install root** from its own location:

| executable at | install root |
|---|---|
| `<root>/bin/voice-core-runtime.exe` | `<root>` |
| `<repo>/target/{release,debug}/…` | the nearest ancestor holding `Cargo.toml` |
| anything else | the executable's own directory |

From that root, in order of precedence — CLI flag, then `runtime.json`, then the
packaged layout:

| resource | packaged default |
|---|---|
| interpreter | `<root>/runtime/python/Scripts/python.exe` (virtualenv), else `<root>/runtime/python/python.exe` (embeddable) |
| worker script | `<root>/runtime/worker/irodori/worker.py`, else `<root>/worker/irodori/worker.py` (dev) |
| engine root | `<root>/runtime/engine` |
| model cache (`HF_HOME`) | `<root>/models/huggingface` |
| data dir | `<root>/data`, else `%APPDATA%\voice-core` |

A packaged default is used only when the path actually exists, so a dev checkout
is never handed paths that were never installed.

`voice-core.exe` and the tray find the data dir the same way, which is how they
locate `token.txt` without being told.

Run `voice-core-runtime.exe --print-layout` to see every resolved path with an
`ok`/`MISSING` marker. It prints the diagnosis even when the engine cannot be
resolved at all — a diagnostic that fails on a broken install is useless.

## runtime.json

Only needed to override the layout, which in practice means dev checkouts and
installs that keep the engine elsewhere. Relative paths resolve against the
install root.

```json
{
  "ttsPython": "runtime/python/python.exe",
  "ttsScript": "runtime/worker/irodori/worker.py",
  "ttsRoot": "runtime/engine",
  "hfHome": "models/huggingface",
  "idleStopSecs": 900
}
```

`ttsUrl` instead of `ttsPython` attaches to a worker somebody else runs; the
runtime then manages no process at all.

Malformed JSON fails at startup with the file name and parse error rather than
silently degrading into "no engine configured".

## Writable state

`data/` is preferred. If the install directory is not writable — the usual case
under `C:\Program Files` — the runtime falls back to `%APPDATA%\voice-core` and
prints which one it chose. `--data-dir` overrides both.

## Bundled virtualenv relocation

A Windows virtualenv is not relocatable: `pyvenv.cfg` records an absolute `home`
pointing at the interpreter that created it. A portable package therefore ships
that interpreter as `runtime/python-base`, and on startup the runtime rewrites
`home` whenever the recorded path no longer exists. That is what makes the tree
survive being unzipped somewhere else or copied to another machine, and it is
logged when it happens.

## Missing resources

Startup does not abort when a configured resource is absent: a frontend must be
able to connect and be told what is wrong. Instead the runtime

1. logs the missing paths to `data/logs/runtime.err.log`,
2. publishes them as a `progress` event with phase `preflight`,
3. reports them in `GET /api/status` under `worker.missing`,

and a synthesis attempt fails with `worker_start_failed` naming the exact path.
Model completeness is not preflighted here — that belongs to the engine, which
reports it as `model_load_failed`.
