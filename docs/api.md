# voice-core API v1

The single contract. There is no SDK: the surface is small enough that a
frontend is a few HTTP calls, and a hand-maintained type mirror in a second
language is a synchronisation tax with no consumer to pay it for.

Base URL is loopback only, default `http://127.0.0.1:8760`.
Auth is `Authorization: Bearer <token>` where the token is `token.txt` in the
runtime's data dir. `GET /api/health` is the sole unauthenticated route.

## Two invariants

1. **Audio never travels inside JSON.** `POST /api/speak` returns an `audioId`;
   bytes come from `GET /api/audio/{audioId}` as `audio/wav`. No base64 exists
   anywhere in the system, and the runtime never holds a sample in memory.
2. **The runtime never calls a frontend back.** Presenters subscribe to
   `GET /api/events`. A GUI, a CLI and an agent are peers; none has a private
   mode or a privileged path.

## Routes

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/health` | Liveness before you know the token. No secrets. |
| GET | `/api/status` | Runtime, engine, spool, presenter and in-flight state. |
| GET | `/api/metrics` | Counters and speak latency percentiles. |
| GET | `/api/voices` | Installed voice packs (the `voicePacks` section of `config.json`, reloaded when that file changes). |
| POST | `/api/speak` | Synthesize one utterance. |
| GET | `/api/audio/{audioId}` | Stream the WAV bytes. |
| GET | `/api/events` | SSE: subtitles, engine state, progress, failures. |
| POST | `/api/warm` | Load the model now; returns when it is resident. |
| POST | `/api/sleep` | Stop the engine process, release GPU memory, keep serving. |
| DELETE | `/api/requests/{requestId}` | Cancel a speak (see the caveat below). |
| POST | `/api/shutdown` | Exit the process. |

`GET /api/audio/{audioId}` also answers `HEAD`, which is how a history view asks
whether an older utterance is still replayable without transferring it. Spool
entries expire by age and by total size, and are dropped when the runtime
restarts, so availability must be asked rather than assumed.

## POST /api/speak

```json
{
  "text": "おかえりなさい、先生。",
  "displayText": "欢迎回来，老师。",
  "rubyPairs": [
    { "base": "欢迎回来", "ruby": "おかえりなさい" },
    { "base": "，",       "ruby": "、" },
    { "base": "老师",     "ruby": "先生" },
    { "base": "。",       "ruby": "。" }
  ],
  "voicePackId": "ba-shun-kid-lora",
  "seed": 1234,
  "numSteps": 32,
  "displaySeconds": 4.0,
  "timeoutMs": 600000
}
```

`text` is spoken; `displayText` is shown to a human and never synthesized.
Translation is the caller's job. Only `text` is required.

`rubyPairs` is optional and is how a caller hands over the ALIGNMENT between the two
strings: `base` is a fragment of `displayText`, `ruby` the fragment of `text` that
means it, in reading order. Concatenating each side must reproduce the corresponding
full string.

Send punctuation pairs too (`{"base":"，","ruby":"、"}`) - the concatenation rule requires
them, and no caller-side stripping is expected: a presenter is free to drop punctuation
from what it draws as an annotation, and the tray dialog does exactly that.

It exists because the alignment is not derivable downstream. Chinese and Japanese do
not line up positionally - the verb sits mid-sentence in one and at the end of the
other, and a translation freely merges, splits or reorders clauses - so a presenter
that pairs by punctuation or character ratio displays a correspondence that is not
there. An agent that produced both strings already knows the mapping; sending it costs
one array and lets a presenter render true per-segment ruby. Callers that send nothing
still work: the presenter falls back to a coarser pairing (see
`docs/dialog-presenter.md`).

```json
{
  "requestId": "9f1ee8dd72ea4f2a",
  "audioId": "111daef4763a4cb7",
  "sampleRate": 48000,
  "durationMs": 3920,
  "bytes": 376364,
  "displayText": "欢迎回来，老师。",
  "voicePackId": "ba-shun-kid-lora",
  "presenters": 1,
  "coldStart": true,
  "queueMs": 0,
  "synthMs": 49589,
  "totalMs": 63284
}
```

`presenters` is how many frontends are subscribed to the event stream. A CLI
uses it to decide whether it must play the audio itself; that is why no client
needs to guess by probing another frontend's port. The count rises when a
`GET /api/events` response starts and falls when that connection closes, so it
is back to 0 once every subscriber is gone.

`coldStart: true` means this call paid the model load. Synthesis is serialized:
one GPU, one utterance, and `queueMs` reports how long the request waited.

`timeoutMs` is charged twice and never shared: once as the bound on the queue
wait, once as the bound on the synthesis, which starts when the engine call
starts. A request that waited in line therefore cannot fail with
`deadline_exceeded` — running out of patience while queued is `resource_busy`
(429), and that code means the engine never saw the utterance. Worst case wall
time for one call is just under twice `timeoutMs`, so a client that wants to see
the 429 rather than its own client-side timeout must be at least that patient.

## GET /api/status

```json
{
  "name": "voice-core", "runtimeVersion": "2.0.0-alpha.1", "apiVersion": 1,
  "uptimeMs": 239080, "voicePacks": 3, "presenters": 1, "inFlight": 0,
  "idleStopMs": 900000,
  "worker": {
    "managed": true, "running": true, "ready": true, "modelLoaded": true,
    "port": 62632, "uptimeMs": 187541, "idleMs": 8607,
    "missing": []
  },
  "spool": { "entries": 2, "bytes": 622168, "maxBytes": 2147483648 }
}
```

`worker.missing` lists configured resources that are not on disk — interpreter,
worker script, engine root, model cache. It is populated at startup rather than
on first use, so a frontend can show a broken install immediately instead of
surfacing it as a failed utterance. Empty means the engine can at least be
launched; model completeness is the engine's own report (`model_load_failed`).

The `worker` block is the runtime's last observation of the engine, reused for up
to 10 s so that polling cannot turn into per-poll HTTP against a worker that may
be mid-synthesis — the tray polls every 5 s and an agent can poll far faster.
Every transition the runtime causes (`/api/warm`, `/api/sleep`, a speak, either
idle-reclaim tier) drops that observation, so `running` and `modelLoaded` are
current immediately after the call that changed them; only an engine that exits
on its own can read stale, and for at most 10 s. `worker.idleMs` is always live.

## POST /api/warm

Returns the same `worker` block `GET /api/status` carries, and returns it only
once `modelLoaded` is `true`. Warm exists to move the cold start off the moment a
human is waiting on it, so returning as soon as the engine process answered would
defeat the point: the model loads lazily on first use, and that load is the
expensive part — a first utterance measured 15.6-49.6 s against 1.4-1.9 s once
the model is resident (`metrics.jsonl`, 78 speaks on the development machine).
Idempotent — a warm against a resident model returns immediately, and a second
warm while a load is in flight waits for that load and reports its outcome. A
load that fails is `model_load_failed`.

## Idle reclaim

Two tiers, both derived from `idleStopMs` (`--idle-stop-secs`, 900 s by default,
`0` disables reclaim entirely). Neither tier fires while a synthesis holds the
device.

| Idle for | What happens | What a frontend sees |
|---|---|---|
| `idleStopMs` | The model is unloaded. The engine process stays alive. | `progress` with `phase: "idle_reclaim"`; then `worker.running: true` with `worker.modelLoaded: false`. |
| 4 × `idleStopMs` | The engine process is stopped. | `workerStopped` whose `reason` begins `idle reclaim tier 2`. |

The tiers exist because the two resources cost different amounts to give back.
Unloading returns GPU memory — the contended resource — and keeps the Python
process, so the next utterance pays the model load and nothing else. Stopping the
process also gives back a few hundred MB of pageable host memory, and costs a
spawn plus a torch import on top of that load: 3.3-3.9 s on the development
machine, 13.7 s worst observed on a cold page cache (`metrics.jsonl`,
`totalMs - synthMs - queueMs` across cold-start speaks). That asymmetry is why
the second window is a multiple of the first rather than a knob of its own. Both
tiers append one `idle_reclaim` line to `metrics.jsonl` naming the tier.
`POST /api/sleep` is the explicit, immediate form of tier 2.

## GET /api/events

`text/event-stream`. Each frame is one JSON envelope:

```json
{"seq":5,"tsMs":1788355390785,"kind":"speech","requestId":"9f1ee8dd72ea4f2a",
 "audioId":"111daef4763a4cb7","text":"おかえりなさい、先生。","displayText":"欢迎回来，老师。",
 "rubyPairs":[{"base":"欢迎回来","ruby":"おかえりなさい"},{"base":"，","ruby":"、"},
              {"base":"老师","ruby":"先生"},{"base":"。","ruby":"。"}],
 "durationMs":3920,"sampleRate":48000,"displaySeconds":null}
```

A new subscriber receives the recent tail (up to 64 envelopes) before live
events, so a frontend that restarts mid-utterance can render current state
without a catch-up call. The subscription and the tail are taken together, so an
event published while a client is connecting arrives exactly once — in the tail
or in the live stream, never both and never neither. A subscriber counts towards
`presenters` from the moment its response starts until its connection closes.

| `kind` | Meaning |
|---|---|
| `runtimeReady` | Runtime assembled and serving. |
| `runtimeStopping` | Shutdown requested; streams end next. |
| `workerStarting` / `workerReady` / `workerStopped` | Engine process lifecycle, with the reason. |
| `speakStarted` | A request entered the pipeline. |
| `speech` | Everything a subtitle and playback frontend needs. |
| `speakFailed` | Carries the same `code` as the HTTP error. |
| `progress` | Long operations (model load), idle reclaim, dropped-event notices. |

## Errors

Every non-2xx reply is the same shape. Switch on `code`, never on prose.

```json
{
  "code": "voice_pack_not_found",
  "message": "voice pack 'nope' is not installed",
  "recovery": { "kind": "install_voice_pack", "detail": "installed: ba-shun-kid-lora, ba-miyu-lora" }
}
```

| `code` | HTTP | Meaning |
|---|---|---|
| `unauthorized` | 401 | Missing or wrong bearer token. |
| `invalid_request` | 400 | Malformed or empty input. |
| `not_found` | 404 | Unknown `audioId` (spool entries expire). |
| `voice_pack_not_found` | 404 | Unknown `voicePackId`; `recovery.detail` lists installed ids. |
| `worker_unavailable` | 503 | Attached engine is not answering. |
| `worker_start_failed` | 500 | Managed engine could not start or become ready. |
| `model_load_failed` | 500 | Engine started but could not load its model. |
| `resource_busy` | 429 | Waited `timeoutMs` for the device without ever reaching the engine. |
| `deadline_exceeded` | 504 | Synthesis exceeded `timeoutMs`; queue time is not charged to it. |
| `cancelled` | 499 | Abandoned by its own caller. |
| `internal` | 500 | Anything else; `recovery` points at the engine log. |

`recovery.kind` is one of `retry`, `wait`, `check_token`, `check_worker_logs`,
`install_voice_pack`, `fix_request`.

## Cancellation caveat

`DELETE /api/requests/{requestId}` frees the caller immediately and guarantees
the utterance is never delivered to a presenter. It does **not** free the GPU
instantly: the engine finishes its current step first, and the device permit is
held until it genuinely returns. Reporting otherwise would be a lie a caller
could not act on. True mid-step cancellation needs a worker-side abort and is
not implemented.

## Engine contract

The runtime speaks four routes to whatever process performs synthesis, which is
what makes "adding an engine" cost a worker rather than a plugin system:

```
GET  /health      -> {"ready": bool, "modelLoaded": bool}
POST /load        -> {"modelLoaded": true, "loadMs": int}
                  -> {"error": "model load failed: ..."}
POST /unload      -> {"modelLoaded": false, "freedMs": int}
POST /synthesize  {"text": str, "outPath": str,
                   "voicePack": {"kind": str, "path": str|[str]}|null,
                   "seed": int|null, "numSteps": int}
                  -> {"sampleRate": int, "durationMs": int, "bytes": int}
```

`outPath` is a spool path the runtime reserved. The engine writes its WAV there
directly; nothing copies the audio afterwards.

`ready` says the process answers, not that it can speak: `/health` is true from
the moment the port binds. `modelLoaded` is the expensive bit, and `/load` is how
warm makes it true — it must not return until the model is loaded, and a second
call while a load is in flight waits for that same load rather than starting
another. `/unload` is the reverse and keeps the process alive, so what it hands
back is VRAM, not the interpreter and its imports. Both are idempotent, and both
report failure the way `/synthesize` does: HTTP 200 with an `error` field, because
the reason exists only inside the engine process.
