# voice-core API v1

The single contract, and the whole of it: HTTP, no SDK, no client library. Every frontend
in this project — the tray presenter, the CLI, the panel — is written against exactly what
is below.

Loopback by default, and only by default: `--bind` is `127.0.0.1:8760` unless a launcher
says otherwise, and nothing else constrains it — a bearer token with no TLS is the whole of
the auth, so any other address publishes synthesis to that network. Auth is
`Authorization: Bearer <token>`, where the token is `token.txt` in the runtime's data dir:
`<install root>\data`, falling back to `%APPDATA%\voice-core` when the install directory is
not writable. `bin\voice-core-runtime.exe --print-layout` prints that path and every other
resolved location without starting anything. `GET /api/health` is the sole unauthenticated
route.

A request body must arrive as `Content-Type: application/json`. There is no CORS layer and
no `Access-Control-*` header on any response, so a page served from another origin cannot
call this API out of a browser; a local process can. Nothing is rate-limited, and the
surface is versioned by `apiVersion` — `1`, this document — not by a header.

## Two invariants

1. **Audio never travels inside JSON.** `POST /api/speak` returns an `audioId`;
   bytes come from `GET /api/audio/{audioId}` as `audio/wav`. No base64 exists
   anywhere in the system, and the runtime never holds a sample in memory.
2. **The runtime never calls a frontend back.** Presenters subscribe to
   `GET /api/events`. A GUI, a CLI and an agent are peers; none has a private
   mode or a privileged path. `POST /api/played` does not bend this: a frontend
   reports *in* about its own playback, which is the direction that keeps it true.

## Two costs to design around

Both are properties of the machine rather than failure modes, and a client that reads them
as errors is the usual first bug.

**A cold start is tens of seconds.** The model loads lazily on first use, and that load —
checkpoint I/O and building the model, not synthesis — is the expensive part. A first
utterance that carries it has measured between 17 s and 50 s across runs, depending mostly
on the page cache, against a p50 of 0.64 s and a p95 of 0.70 s once the model is resident
(reference machine: RTX 5060 Ti 16 GB, i5-12600KF; 32 steps, n=15 over three texts). The
same bill arrives again after idle reclaim has unloaded it. So either allow well over a
minute before calling a speak a timeout, or call `POST /api/warm` first and be slow there,
where nothing is waiting on the answer.

`POST /api/warm` buys the model load and not the warm latency: the first utterance after a
load is still about twice a subsequent one — measured on the reference machine at 1413 ms and
1591 ms in two separate processes, against 515–556 ms for the utterances that followed the
same text. The extra second is the engine's own first-call work: allocator growth, kernel
autotuning and the first CUDA graph capture. So a caller that warms and then sizes its timeout
from the resident figure will still see one slow reply. Warm twice, or size the first call
generously.

Treat the resident figure as the best case rather than a guarantee: it is measured with the
engine's CUDA graph capture in force, which is the default and needs roughly 3.2 GiB of
reserved VRAM. On a card that cannot spare that, capture raises, the engine says so once in
its log and samples eagerly for the rest of the process — correct, bitwise-identical audio
at about 2.5 s an utterance instead of 0.6 s. `VC_ENGINE_CUDA_GRAPHS=0` selects that regime
deliberately. Nothing about the API changes with it; only how fast a reply arrives.

**The GPU is single-tenant.** One device, one utterance: synthesis is serialized behind a
permit, and a second concurrent `POST /api/speak` waits for the first — `queueMs` in the
reply is how long it waited. A request that waits out its own `timeoutMs` in that queue
answers `resource_busy` (429) and never reached the engine. That code, not
`deadline_exceeded`, is what concurrency looks like from the outside.

## Routes

| Method | Path | Purpose |
|---|---|---|
| GET | `/api/health` | Liveness before you know the token. No secrets. |
| GET | `/api/status` | Runtime, engine, spool, presenter and in-flight state. |
| GET | `/api/metrics` | Counters and speak latency percentiles. |
| GET | `/api/voices` | Installed voice packs (the `voicePacks` section of `config.json`, reloaded when that file changes). |
| POST | `/api/speak` | Synthesize one utterance. |
| POST | `/api/played` | A frontend reports that it started or finished playing an `audioId`. |
| GET | `/api/audio/{audioId}` | Stream the WAV bytes. |
| GET | `/api/events` | SSE: subtitles, engine state, progress, failures. |
| POST | `/api/warm` | Load the model now; returns when it is resident. |
| POST | `/api/sleep` | Stop the engine process, release GPU memory, keep serving. |
| DELETE | `/api/requests/{requestId}` | Cancel a speak (see the caveat below). |
| POST | `/api/shutdown` | Exit the process. |

## GET /api/health

The one route that answers before a caller knows the token, and it carries no secret:

```json
{"name":"voice-core","runtimeVersion":"1.4.0","apiVersion":1,"ready":true}
```

`ready` is a constant `true`: the process is answering, which is all this route claims.
Engine and model state are in `GET /api/status`, behind the token.

## GET /api/voices

One entry per installed pack, already merged: the `voicePacks` entry in `config.json` and
the pack's own `voicepack.json` resolved into a single object with `avatar` and `path` made
absolute, so two frontends cannot resolve one relative path against different bases. The
list is re-read whenever `config.json`'s mtime moves — adding a voice needs no restart.

```json
[{"id":"ba-miyu-lora","name":"霞沢美游 (LoRA)","languages":["ja"],"kind":"lora-adapter",
  "path":"C:\\voice-core\\data\\voicepacks\\ba-miyu-lora","engine":"irodori-tts-v4.1-small",
  "character":"霞沢美游","avatar":"C:\\voice-core\\data\\voicepacks\\ba-miyu-lora\\avatar.png",
  "synthesis":{"numSteps":32,"seed":0},
  "manifest":"C:\\voice-core\\data\\voicepacks\\ba-miyu-lora\\voicepack.json",
  "sources":{"avatar":"pack","character":"pack","dialog":"derived","engine":"pack",
             "expression":"derived","kind":"pack","languages":"pack","name":"pack",
             "path":"config","synthesis":"pack"}}]
```

`id` is what a speak request sends as `voicePackId`. `kind` is `lora-adapter` |
`speaker-embedding` | `reference-audio`. `languages` is what the pack claims it can speak
and is what a request's `language` is checked against; a pack that declares none
contradicts nobody. `character` and `avatar` are for a subtitle frontend and are absent
when the pack names neither. `dialog` and `expression` are the pack's own requests, absent
unless it made one, and each field in them is one tier under the same field on a speak call.
`synthesis` (`numSteps`, `seed`, `temperature`) is published rather than applied: the
runtime sends its own 32 steps and whatever `seed` the request carried, and no engine
parameter corresponds to `temperature` today — a caller that wants a pack's preferred
sampling reads it here and puts it in the request. `sources` says which file decided each
key (`pack`, `config`, or `derived` for "nobody said, so the program's own behaviour
stands"), which is how a settings screen can show a value without inventing its provenance.
`engine` is declared, not routed: one engine exists today.

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
  "language": "ja",
  "seed": 1234,
  "numSteps": 32,
  "displaySeconds": 4.0,
  "timeoutMs": 600000,
  "dialog": { "nameColor": "#ff6ab5", "reveal": "fade", "displaySeconds": 45 },
  "emotion": "😭😭",
  "cfgScaleCaption": 3.0
}
```

| field | type | absent means |
|---|---|---|
| `text` | string, **required** | `invalid_request`. Whitespace counts as empty. |
| `displayText` | string | a presenter shows `text` itself. |
| `rubyPairs` | `[{base, ruby}]` | a presenter pairs the two strings itself, coarsely. |
| `voicePackId` | string | nothing is sent to the engine and the engine refuses the call; treat it as required (below). |
| `language` | short BCP-47 tag | no language check happens. |
| `seed` | integer | the engine samples unseeded. |
| `numSteps` | integer | 32. |
| `displaySeconds` | number, >0 and <=600 | the presenter's own dwell. Same tier as `dialog.displaySeconds` and wins over it. |
| `timeoutMs` | integer, ms | 600000, or `--synth-timeout-secs`. Bounds the queue wait and the synthesis separately. |
| `dialog` | object | the pack's manifest, then `config.json`, then the presenter's built-ins. |
| `emotion` | string | the pack's `expression.emotion`; an explicit `""` suppresses even that. |
| `cfgScaleCaption` | number, 0..=10 | the engine's own 3.0. Needs a caption to act on. |

Fields this API does not know are ignored, which is what lets a newer client talk to an
older runtime without a negotiation step.

`text` is spoken; `displayText` is shown to a human and never synthesized. Translation is
the caller's job. `text` is the only field a request must carry, but it is not the only one
a caller needs: the Irodori engine synthesizes from a voice pack, and an utterance sent
without `voicePackId` comes back `internal` with the engine's own words — `Specify
ref_wav/ref_wavs/ref_latent/ref_latents, or set no_ref=True`. Treat `voicePackId` as
required until an engine ships a default voice.

`rubyPairs` is optional and is how a caller hands over the ALIGNMENT between the two
strings: `base` is a fragment of `displayText`, `ruby` the fragment of `text` that
means it, in reading order. Concatenating each side must reproduce the corresponding
full string, once `[pause:N]` markers have been taken out of `text`.

An array that does not reconcile is `invalid_request`, naming the first pair that
diverges and both fragments at that point:

```
rubyPairs[2].ruby does not line up with text: the first 2 pair(s) reconstructed
8 character(s), then this one offers `せんせい。` where text has `先生。`
```

Nothing downstream can repair the array — the alignment is knowledge only the caller has —
so the runtime refuses it instead of guessing. An empty array is not a mismatch: it means
the same as sending none. When there is no `displayText`, `base` is checked against `text`,
because that is the string a presenter shows.

Send punctuation pairs too (`{"base":"，","ruby":"、"}`): the concatenation rule requires
them, and no caller-side stripping is expected — a presenter is free to drop punctuation
from what it draws as an annotation, and the bundled one does exactly that.

Why it is the caller's to send: the alignment is not derivable downstream. Chinese and
Japanese do not line up positionally — the verb sits mid-sentence in one and at the end of
the other, and a translation merges, splits and reorders clauses — so a presenter that
pairs by punctuation or character ratio draws a correspondence that is not there. Whoever
produced both strings already knows the mapping; sending it costs one array and buys true
per-segment ruby. A caller that sends nothing still works, on that coarser pairing.

### Appearance: `dialog`

Optional, and the TOP tier of four: this call beats the voice pack's `voicepack.json`, which
beats `config.json`'s own `dialog` section, which beats the presenter's built-ins. Merged
field by field, so `{"reveal":"fade"}` changes the reveal and leaves every colour alone.
Fields: `nameColor`, `textColor`, `rubyColor`, `countdownColor` (`#rgb`, `#rrggbb` or
`#aarrggbb`), `reveal` (`typewriter` | `sweep` | `fade`), `displaySeconds` (>0, <=600).

A colour that is not hex and a `reveal` that is not one of those three are
`invalid_request` naming the field and the value — never a field the runtime drops quietly.
`displaySeconds` is accepted both here and as the older top-level field; when a request
carries both, the top-level one wins.

The resolved result rides on the `speech` event for that utterance, which is how a presenter
re-themes per line without reading a pack or a config file itself.

`config.json`'s `dialog` section is re-read by the runtime whenever its mtime moves, and by
the presenter the same way, so a change to it takes effect on the next utterance with
nothing restarted.

### Expression: `emotion` and `cfgScaleCaption`

The engine conditions on a CAPTION separately from the words (`use_caption_condition` in
the v4.1-Small checkpoint), so `emotion` changes delivery without being spoken. It takes
free prose (`泣きながら、震える声で`) and the checkpoint's 45 emoji annotations, which also
work inline in `text`; `skills/voice-core-tts/SKILL.md` carries the table.

* `cfgScaleCaption` is how hard it steers, `0..=10`; the engine's own default is 3.0. Out of
  range is `invalid_request`, never a clamp, and sending it with no caption for it to act on
  is `invalid_request` too — a knob with no effect that answers 200 is the failure this API
  does not do.
* Absent `emotion` falls back to the pack's `expression.emotion`; an explicit `""` suppresses
  that default for this one call, which is the only way to hear a pack plainly without
  editing it.
* Neither field supplied sends exactly the request the runtime sent before this channel
  existed — same text, same seed, same pack, byte-identical audio — so an existing client
  hears no change. A caption is audible in the length as well as the delivery: on the
  reference machine one line measured 3120 ms plain, 2920 ms with `emotion`, and 3880 ms
  with an inline `😭😭`.
* An utterance that carried one records `caption` (and `cfgScaleCaption`) in its
  `metrics.jsonl` line; one that did not keeps exactly the record shape it always had.

### Pauses

`text` may carry `[pause:N]` markers, N in 1-10000 ms, and that is the whole of the
prosody vocabulary. The runtime splits the utterance at each marker, synthesizes the
segments, and splices N ms of silence between them:

* one `audioId`, one `speech` event, one `durationMs` that counts the silence;
* the markers never reach the engine, and `text` in the reply and in the event is the
  marker-free line — which is also what `rubyPairs` is reconciled against, so an
  alignment is written for the words and is never renumbered or resegmented by a split;
* two adjacent markers sum, and so do markers separated by nothing but whitespace;
* a marker at the very start or end is dropped, with a `progress` event whose `phase`
  is `pause_marker` saying so: silence at the edge of an utterance is the caller's own
  wait, and honouring it would put dead air where nobody can see where it came from;
* a marker the runtime cannot read (`[pause:abc]`, `[pause:99999]`) is
  `invalid_request` quoting the marker. It is never spoken aloud as literal text, which
  is the failure this primitive exists to prevent;
* `timeoutMs` still bounds the whole synthesis, not each segment;
* an utterance that was split adds `segments` and `pauseMs` to its `metrics.jsonl`
  line; one without a marker keeps exactly the record shape it always had.

Splitting is not free and the caller can hear it: each segment is its own utterance to
the engine, with its own lead-in and tail. On the reference machine,
`おかえりなさい、先生。今日はいい天気ですね。` is 5760 ms in one piece; with a
`[pause:600]` between the sentences it is 6880 ms, of which exactly 600 ms is the
spliced silence (the two halves alone measure 3120 + 3160 ms) and the rest is that
lead-in. Use a marker for a beat you want, not to shave milliseconds.

### Language

`language` is an optional short BCP-47 tag (`ja`, `zh-CN`, `en-US`). When it is present
and the resolved pack declares languages that do not include it, the utterance is
refused with `voice_language_unsupported` naming the pack, what it declares and what
was asked. Absent, nothing changes: the runtime does not detect, guess or translate.

Tags compare case-insensitively and by subtag, so `ja-JP` satisfies a pack that says
`ja` and `zh-TW` does not satisfy one that says `zh-CN`. A pack that declares no
language at all cannot contradict anybody and is accepted.

**Nothing routes on this field.** A second engine is what would make routing real, and
that is a later change; what exists today is the field, the check and the error code, so
a caller can be written against them now. What it replaces is silent: Chinese text
through a Japanese-only adapter produces confident garbage and no error anywhere.

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

## GET /api/audio/{audioId}

```
200 OK
content-type: audio/wav
content-length: 376364
cache-control: private, max-age=60
```

Uncompressed 16-bit PCM exactly as the engine wrote it — the runtime streams the spool file
and never re-encodes, resamples or holds the clip in memory. `sampleRate` and `bytes` in the
speak reply describe these bytes. `HEAD` answers the same headers with no body, which is how
a history view asks whether an older utterance is still replayable without transferring it.
An id the spool no longer holds is `not_found` (404) from both verbs.

The spool is a cache with two bounds and no persistence: an entry expires after
`--spool-ttl-secs` (3600), the oldest are dropped once the total passes `--spool-max-mb`
(2048), and the directory is cleared when the runtime starts. `GET /api/status` reports
`spool.entries`, `spool.bytes` and `spool.maxBytes`. A caller that needs to keep an
utterance copies the bytes; this is not an archive, and availability is asked rather than
assumed.

## POST /api/played

```json
{ "audioId": "111daef4763a4cb7", "event": "finished", "playedMs": 3512, "by": "cli" }
```

`204`, no body. `event` is `started` or `finished`; `playedMs` is what the reporter
really played and belongs on `finished`; `by` is `presenter` or `cli` and defaults to
`presenter`, because a frontend that played audio is one. An `audioId` the spool does
not have is `not_found`, the same as fetching it.

The route exists because `POST /api/speak` returns when the audio *exists*, and until
this existed nothing said when it had been *heard*. An agent reading three paragraphs in
order had to sleep for a duration it could only guess, and guessed wrong in both
directions. Now whoever played it says so, and the report becomes `playbackStarted` /
`playbackFinished` on the event stream with the `requestId` the runtime looks up from the
spool entry — the reporter only has to know which clip it played.

This does not weaken invariant 2. The runtime still never calls a frontend back: a
frontend reports *in*, on a route it chose to call, and the runtime does nothing with it
beyond publishing it. No state, no bookkeeping, no effect on idle reclaim — playback
happens in another process and the runtime owns no audio device.

Both bundled frontends report: the tray presenter around its own player, and the CLI around
its local one. `voice-core speak --wait` consumes that — it subscribes before speaking and
returns on the first `playbackFinished` for its `audioId`, whoever sent it, or exits
non-zero naming what it never observed once `durationMs + 5 s` (or `--wait-timeout-ms`) is
spent. A caller therefore never has to know which frontend played the line.

`playedMs` is not `durationMs`: a clip the next utterance cut short played for less than
it lasts, and a presenter that reports `4391` for a `4320` ms clip has spent the
difference opening the device.

## GET /api/status

```json
{
  "name": "voice-core", "runtimeVersion": "1.4.0", "apiVersion": 1,
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

## GET /api/metrics

```json
{"speakTotal":1,"speakFailed":0,"coldStarts":1,"audioBytes":284204,
 "servedBytes":284204,"speakSamples":1,"speakP50Ms":23312,"speakP95Ms":23312}
```

Counters live in the process and start at zero with it; the durable record is one
JSON line per utterance in `data/metrics.jsonl`. `speakTotal` counts attempts, so a
failed speak raises it and `speakFailed` together. The percentiles are
nearest-rank over the last 256 *successful* speaks and are `null` until the first
one lands, which is why `speakSamples` travels with them.

## POST /api/warm

Returns the same `worker` block `GET /api/status` carries, and returns it only once
`modelLoaded` is `true`. Warm exists to move the cold start off the moment a human is
waiting on it, so returning as soon as the engine process answered would defeat the point.
Idempotent — a warm against a resident model returns immediately, and a second warm while a
load is in flight waits for that load and reports its outcome. A load that fails is
`model_load_failed`.

`POST /api/sleep` answers the same block, taken after the engine process is gone.

## Idle reclaim

Two tiers, both derived from `idleStopMs` (`--idle-stop-secs`, 900 s by default;
`0` disables reclaim entirely, and `GET /api/status` then reports `idleStopMs` as
`null` rather than `0`). Neither tier fires while a synthesis holds the device.

| Idle for | What happens | What a frontend sees |
|---|---|---|
| `idleStopMs` | The model is unloaded. The engine process stays alive. | `progress` with `phase: "idle_reclaim"`; then `worker.running: true` with `worker.modelLoaded: false`. |
| 4 × `idleStopMs` | The engine process is stopped. | `workerStopped` whose `reason` begins `idle reclaim tier 2`. |

The tiers exist because the two resources cost different amounts to give back.
Unloading returns GPU memory — the contended resource — and keeps the Python
process, so the next utterance pays the model load and nothing else. Stopping the
process also gives back a few hundred MB of pageable host memory, and costs a
spawn plus a torch import on top of that load: 3.3-3.9 s on the reference
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
 "voicePackId":"ba-shun-kid-lora","durationMs":3920,"sampleRate":48000,
 "dialog":{"nameColor":"#3ddc84","textColor":"#d0ffe4","rubyColor":"#ffd166",
           "countdownColor":"#06d6a0","reveal":"typewriter","displaySeconds":45.0}}
```

`dialog` is this utterance's resolved appearance — per-call over the pack's manifest over
`config.json`'s own section (see `POST /api/speak`) — and it is always present, possibly
empty. Empty means nothing was asked for anywhere, so a presenter uses its built-ins. It
replaced the standalone `displaySeconds` field, which now lives inside it: one place per
fact. The tray presenter re-themes per line from this, which is what lets one voice pack
look different from the next in the same window.

Frames are `data:` lines with no event name and no `id:`, so there is no `Last-Event-ID`
resumption: a client that reconnects re-reads the tail and de-duplicates on `seq`, which is
monotonic within one runtime process and starts at 0. An idle stream carries an SSE comment
line (`:`) every 15 s to hold the connection open, so a client must tolerate comment frames.
One envelope breaks the sequence on purpose — the notice that a subscriber fell behind and
lost events arrives as `progress` with `phase: "event_stream"` and `seq`
`18446744073709551615`, a value no real event can collide with.

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
| `playbackStarted` / `playbackFinished` | What a frontend reported through `POST /api/played`; `finished` carries `playedMs`. |
| `speakFailed` | Carries the same `code` as the HTTP error. |
| `progress` | Long operations (model load), idle reclaim, dropped `[pause:N]` markers, dropped-event notices. |

The playback pair is the only thing on this stream the runtime did not observe itself,
and the reason it is here rather than in a reply is that the frontend which played is
not the process that asked for the utterance. One line of a real stream, `by` naming
the reporter:

```json
{"seq":53,"tsMs":1788595645369,"kind":"playbackStarted","requestId":"1ae88bc9c7a646b4",
 "audioId":"584596dd702c44c2","by":"presenter"}
{"seq":54,"tsMs":1788595649761,"kind":"playbackFinished","requestId":"1ae88bc9c7a646b4",
 "audioId":"584596dd702c44c2","by":"presenter","playedMs":4391}
```

A frontend that plays audio should handle both kinds even if it ignores them, because
its own reports come back around the bus: the tray presenter drops them deliberately
rather than echoing its own playback into the line the user is reading.

## Errors

Every non-2xx reply from a route is the same shape. Switch on `code`, never on prose.

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
| `invalid_request` | 400 | Input the runtime will not guess at: empty `text`, an unreadable `[pause:N]`, `rubyPairs` that does not reconstruct its own strings, a colour or `reveal` outside the accepted values, `cfgScaleCaption` out of `0..=10` or sent with no caption to act on. |
| `not_found` | 404 | Unknown `audioId` (spool entries expire), from `/api/audio` or `/api/played`. |
| `voice_pack_not_found` | 404 | Unknown `voicePackId`; `recovery.detail` lists installed ids. |
| `voice_language_unsupported` | 400 | The pack exists but does not declare the `language` asked for; the message names both. |
| `worker_unavailable` | 503 | Attached engine is not answering. |
| `worker_start_failed` | 500 | Managed engine could not start or become ready. |
| `model_load_failed` | 500 | Engine started but could not load its model. |
| `resource_busy` | 429 | Waited `timeoutMs` for the device without ever reaching the engine. |
| `deadline_exceeded` | 504 | Synthesis exceeded `timeoutMs`; queue time is not charged to it. |
| `cancelled` | 499 | Abandoned by its own caller. |
| `internal` | 500 | Anything else; `recovery` points at the engine log. |

`recovery.kind` is one of `retry`, `wait`, `check_token`, `check_worker_logs`,
`install_voice_pack`, `fix_request`.

Three rejections happen before a route is reached and therefore carry no envelope at all,
which a client that only ever parses `code` reads as a crash:

| What | Reply |
|---|---|
| A body sent without `Content-Type: application/json` | 415, `text/plain`, body `Expected request with Content-Type: application/json` |
| A body that is not parseable JSON | 400, `text/plain`, body beginning `Failed to parse the request body as JSON:` |
| A path this API does not serve | 404 with an empty body |

## Cancellation caveat

`DELETE /api/requests/{requestId}` frees the caller immediately and guarantees
the utterance is never delivered to a presenter. It does **not** free the GPU
instantly: the engine finishes its current step first, and the device permit is
held until it genuinely returns. Reporting otherwise would be a lie a caller
could not act on. True mid-step cancellation needs a worker-side abort and is
not implemented.

The reply is `{"cancelled": bool}`, and `false` means the id was not in flight —
already finished, already cancelled, or never issued. `POST /api/shutdown`
answers `{"stopping": bool}` the same way, before the event streams end.

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
                   "seed": int|null, "numSteps": int,
                   "caption": str, "cfgScaleCaption": float}
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
