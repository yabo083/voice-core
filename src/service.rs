//! Orchestration: the single owner of a request's lifecycle.
//!
//! Routes stay dumb on purpose. Everything that must be identical for every
//! request lives here exactly once — request id, queueing against the single
//! GPU, worker readiness, deadline, cancellation, spool registration, event
//! publication, metrics. v1 spread these across handlers, which is how it ended
//! up with a 120 s timeout in one route and 300 s in another.

use std::collections::HashMap;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as SyncMutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Notify, Semaphore};

use crate::config::Config;
use crate::engine::{EngineError, PackTarget, SynthOutput, SynthRequest, TtsEngine};
use crate::error::{ApiError, ErrorCode, RecoveryKind};
use crate::obs::{short_id, Bus, Event, Metrics, MetricsSnapshot, Reporter};
use crate::packs::{Registry, VoicePack};
use crate::spool::{Spool, SpoolStats};
use crate::supervise::{Worker, WorkerError, WorkerStatus};

pub const RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Bumped only on a breaking change to the public surface.
pub const API_VERSION: u32 = 1;

const DEFAULT_NUM_STEPS: u32 = 32;

/// The one pause primitive: `[pause:N]`, N in milliseconds.
const PAUSE_OPEN: &str = "[pause:";
/// Below a millisecond there is nothing to hear, and above ten seconds the caller
/// meant two utterances — a mistyped `[pause:600000]` must not become ten minutes of
/// silence somebody has to wait out.
const PAUSE_RANGE: std::ops::RangeInclusive<u32> = 1..=10_000;

/// How long `/api/status` may reuse the last `/health`-backed worker status.
///
/// The tray polls status every 5 s and an agent can poll far faster, so without
/// this each poller multiplies HTTP into a worker that may be mid-synthesis.
/// 10 s is deliberately longer than the tray's interval, so even the intended
/// poller stops probing on every tick, and every transition the runtime itself
/// causes (warm, sleep, a speak, either reclaim tier) drops the entry. What is
/// left to read stale is a worker that died on its own, and that surfaces within
/// two tray ticks.
const WORKER_STATUS_TTL: Duration = Duration::from_secs(10);

/// Second idle window, as a multiple of `idle_stop`.
///
/// Tier 1 gives back VRAM, which is the contended resource, and keeps the
/// process. What tier 2 additionally reclaims is a few hundred MB of pageable
/// host memory, and what it costs is a process spawn plus the torch import:
/// 3.3-3.9 s on this machine, 13.7 s worst observed on a cold page cache
/// (`data/metrics.jsonl`, `totalMs - synthMs - queueMs` on cold-start speaks).
/// Four windows — 60 min at the 15 min default — outlives a lunch break without
/// holding that memory overnight. Derived rather than configured: a second flag
/// would be a second thing to get wrong, and the value is only ever "some
/// multiple of the first".
const PROCESS_STOP_MULTIPLE: u32 = 4;

/// One aligned segment of an utterance: what the human reads, and the part of the
/// spoken line that means it.
///
/// The caller supplies these because only the caller knows. Chinese and Japanese do
/// not line up positionally - SOV against SVO, and a translation freely merges,
/// splits or reorders clauses - so a client that guesses the mapping from punctuation
/// or character ratios renders a correspondence that is not there. An agent that
/// produced both strings already knows which fragment means which, and passing that
/// along costs one array.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RubyPair {
    /// Fragment of `displayText`.
    pub base: String,
    /// Fragment of `text` that `base` corresponds to. May be empty for punctuation
    /// that exists on only one side.
    #[serde(default)]
    pub ruby: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakInput {
    /// Spoken text. For this project that is Japanese; the caller translates.
    ///
    /// May carry `[pause:N]` markers, which the runtime honours by splitting the
    /// utterance there and splicing N ms of silence in (see [`Script`]). They are not
    /// spoken and do not reach the engine.
    pub text: String,
    /// Text shown to the human. Never synthesized.
    #[serde(default)]
    pub display_text: Option<String>,
    /// Segment-by-segment alignment between `displayText` and `text`. Optional: a
    /// presenter that gets none falls back to its own (necessarily coarser) pairing.
    #[serde(default)]
    pub ruby_pairs: Option<Vec<RubyPair>>,
    #[serde(default)]
    pub voice_pack_id: Option<String>,
    /// What language `text` is in, as a short BCP-47 tag (`ja`, `zh-CN`). Optional and
    /// validated, not routed: when the resolved pack declares languages and none of
    /// them matches, the utterance is refused instead of feeding the wrong text to a
    /// model that will confidently mispronounce it.
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub seed: Option<u64>,
    #[serde(default)]
    pub num_steps: Option<u32>,
    #[serde(default)]
    pub display_seconds: Option<f64>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeakOutput {
    pub request_id: String,
    /// Fetch the bytes at `GET /api/audio/{audioId}`. No audio travels in JSON.
    pub audio_id: String,
    pub sample_rate: u32,
    pub duration_ms: u64,
    pub bytes: u64,
    pub display_text: Option<String>,
    pub voice_pack_id: Option<String>,
    /// Event-stream subscribers at the moment of synthesis. A CLI uses this to
    /// decide whether it must play the audio itself instead of guessing by
    /// probing another frontend's port.
    pub presenters: usize,
    pub cold_start: bool,
    pub queue_ms: u64,
    pub synth_ms: u64,
    pub total_ms: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub name: &'static str,
    pub runtime_version: &'static str,
    pub api_version: u32,
    pub uptime_ms: u64,
    pub worker: WorkerStatus,
    pub voice_packs: usize,
    pub presenters: usize,
    pub in_flight: usize,
    pub spool: SpoolStats,
    pub idle_stop_ms: Option<u64>,
}

/// A frontend reporting *in* that it played something (`POST /api/played`).
///
/// The direction matters: the runtime still never calls a frontend back, and this is
/// what lets a caller wait for the audio to be over instead of sleeping for a guessed
/// duration. Nothing here identifies a request — the reporter knows the `audioId` it
/// fetched, and the runtime knows which request produced it.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayedInput {
    pub audio_id: String,
    /// `started` or `finished`.
    pub event: String,
    /// How long the reporter really played, which is not `durationMs`: a clip the next
    /// utterance cut short played for less than it lasts.
    #[serde(default)]
    pub played_ms: Option<u64>,
    /// `presenter` or `cli`. Omitted means `presenter`: a frontend that played audio
    /// is one, and only this project's own CLI has a reason to say otherwise.
    #[serde(default)]
    pub by: Option<String>,
}

/// A pack id resolved against the registry: what the engine is handed, and what the
/// pack claims it can speak.
struct ResolvedPack {
    id: String,
    languages: Vec<String>,
    target: PackTarget,
}

struct Cancel {
    flag: AtomicBool,
    notify: Notify,
}

/// Worker status as last observed, with the instant it was taken. See
/// [`WORKER_STATUS_TTL`].
struct CachedWorkerStatus {
    at: Instant,
    status: WorkerStatus,
}

pub struct Service {
    cfg: Arc<Config>,
    spool: Arc<Spool>,
    packs: SyncMutex<Registry>,
    bus: Arc<Bus>,
    metrics: Arc<Metrics>,
    worker: Arc<Worker>,
    /// One GPU, one synthesis at a time. Explicit queueing beats two requests
    /// thrashing VRAM and beats an opaque 500.
    gpu: Arc<Semaphore>,
    in_flight: SyncMutex<HashMap<String, Arc<Cancel>>>,
    /// Last observed worker status. Reused for [`WORKER_STATUS_TTL`] so that a
    /// frequent poller cannot turn `/api/status` into per-poll worker HTTP.
    last_worker_status: SyncMutex<Option<CachedWorkerStatus>>,
    started: Instant,
    shutdown: SyncMutex<Option<oneshot::Sender<()>>>,
}

impl Service {
    pub fn new(
        cfg: Arc<Config>,
        spool: Arc<Spool>,
        bus: Arc<Bus>,
        metrics: Arc<Metrics>,
        worker: Arc<Worker>,
        shutdown: oneshot::Sender<()>,
    ) -> Self {
        let packs = Registry::load(cfg.config_file(), cfg.data_dir.clone());
        Self {
            cfg,
            spool,
            packs: SyncMutex::new(packs),
            bus,
            metrics,
            worker,
            gpu: Arc::new(Semaphore::new(1)),
            in_flight: SyncMutex::new(HashMap::new()),
            last_worker_status: SyncMutex::new(None),
            started: Instant::now(),
            shutdown: SyncMutex::new(Some(shutdown)),
        }
    }

    pub fn bus(&self) -> &Arc<Bus> {
        &self.bus
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn spool(&self) -> &Arc<Spool> {
        &self.spool
    }

    pub fn metrics(&self) -> &Arc<Metrics> {
        &self.metrics
    }

    pub fn voices(&self) -> Vec<VoicePack> {
        let mut packs = match self.packs.lock() {
            Ok(packs) => packs,
            Err(_) => return Vec::new(),
        };
        packs.reload_if_changed();
        packs.all().to_vec()
    }

    /// Resolve a pack id into the engine-facing target, or an error naming the
    /// installed alternatives.
    fn resolve_pack(&self, id: Option<&str>) -> Result<Option<ResolvedPack>, ApiError> {
        let Some(id) = id else { return Ok(None) };
        let mut packs = self
            .packs
            .lock()
            .map_err(|_| ApiError::new(ErrorCode::Internal, "voice pack registry poisoned"))?;
        packs.reload_if_changed();
        let Some(pack) = packs.get(id) else {
            let known: Vec<&str> = packs.all().iter().map(|p| p.id.as_str()).collect();
            return Err(ApiError::new(
                ErrorCode::VoicePackNotFound,
                format!("voice pack '{id}' is not installed"),
            )
            .with_recovery(
                RecoveryKind::InstallVoicePack,
                if known.is_empty() {
                    "no voice packs are registered in config.json".to_string()
                } else {
                    format!("installed: {}", known.join(", "))
                },
            ));
        };
        Ok(Some(ResolvedPack {
            id: pack.id.clone(),
            languages: pack.languages.clone(),
            target: PackTarget {
                kind: pack.kind.as_wire(),
                path: packs.resolve_path(pack),
            },
        }))
    }

    /// The whole point of this module: one path, one set of semantics.
    pub async fn speak(&self, input: SpeakInput) -> Result<SpeakOutput, ApiError> {
        let request_id = short_id();
        let started = Instant::now();
        let SpeakInput {
            text,
            display_text,
            ruby_pairs,
            voice_pack_id: requested_pack,
            language,
            seed,
            num_steps,
            display_seconds,
            timeout_ms,
        } = input;

        if text.trim().is_empty() {
            return Err(
                ApiError::new(ErrorCode::InvalidRequest, "text is empty").with_recovery(
                    RecoveryKind::FixRequest,
                    "send non-empty `text`; `displayText` is what humans read",
                ),
            );
        }

        // Everything a caller can get wrong is rejected here, before a request id is
        // worth anything and before it can queue for the device: a malformed marker, an
        // alignment that does not reconstruct its own strings, a pack that does not speak
        // the language. All three are caller bugs, and all three used to be silent.
        let mut script = Script::parse(&text)?;
        let spoken = script.spoken();
        // An explicitly empty array is not an alignment. Reconciling it against text that
        // is genuinely there would only invent a failure.
        let ruby_pairs = ruby_pairs.filter(|pairs| !pairs.is_empty());
        if let Some(pairs) = ruby_pairs.as_deref() {
            check_alignment(pairs, &spoken, display_text.as_deref())?;
        }

        let pack = self.resolve_pack(requested_pack.as_deref())?;
        if let (Some(language), Some(pack)) = (language.as_deref(), pack.as_ref()) {
            check_language(language, pack)?;
        }
        let voice_pack_id = pack.as_ref().map(|pack| pack.id.clone());
        let pack_target = pack.map(|pack| pack.target);

        self.bus.publish(Event::SpeakStarted {
            request_id: request_id.clone(),
            voice_pack_id: voice_pack_id.clone(),
            chars: spoken.chars().count(),
        });
        // A dropped marker changed what the caller asked for, so it is said out loud on
        // the one channel a frontend already watches rather than only into a log.
        for message in script.notes.drain(..) {
            self.bus.publish(Event::Progress {
                request_id: Some(request_id.clone()),
                phase: "pause_marker".into(),
                message,
            });
        }

        let cancel = Arc::new(Cancel {
            flag: AtomicBool::new(false),
            notify: Notify::new(),
        });
        if let Ok(mut map) = self.in_flight.lock() {
            map.insert(request_id.clone(), Arc::clone(&cancel));
        }

        let deadline = timeout_ms
            .map(Duration::from_millis)
            .unwrap_or(self.cfg.synth_timeout);
        let (audio_id, out_path) = self.spool.reserve();

        // The pipeline runs in its own task so that a cancelled caller can be
        // released immediately while the GPU permit stays held until the worker
        // genuinely finishes. Aborting the task would hand the permit to a new
        // request while the old synthesis was still occupying the device.
        let job = SynthJob {
            gpu: Arc::clone(&self.gpu),
            worker: Arc::clone(&self.worker),
            spool: Arc::clone(&self.spool),
            bus: Arc::clone(&self.bus),
            metrics: Arc::clone(&self.metrics),
            cancel: Arc::clone(&cancel),
            request_id: request_id.clone(),
            audio_id: audio_id.clone(),
            out_path,
            segments: script.segments,
            gaps: script.gaps,
            display_text,
            ruby_pairs,
            display_seconds,
            voice_pack_id: voice_pack_id.clone(),
            pack: pack_target,
            seed,
            num_steps: num_steps.unwrap_or(DEFAULT_NUM_STEPS),
            deadline,
            started,
        };
        let handle = tokio::spawn(job.run());

        let outcome = tokio::select! {
            joined = handle => match joined {
                Ok(result) => result,
                Err(err) => Err(ApiError::new(
                    ErrorCode::Internal,
                    format!("synthesis task failed: {err}"),
                )),
            },
            _ = cancel.notify.notified() => Err(ApiError::new(
                ErrorCode::Cancelled,
                "cancelled by caller; the engine finishes its current step before the device frees",
            )),
        };

        if let Ok(mut map) = self.in_flight.lock() {
            map.remove(&request_id);
        }

        match outcome {
            Ok(mut output) => {
                // A cold utterance is the one that moved the worker: the process
                // may have started and the model is resident now, so a poller must
                // not be served an observation from before it. A warm utterance
                // changed nothing a probe would reveal, and probing per speak is
                // exactly the per-request worker HTTP the cache exists to avoid.
                if output.cold_start {
                    self.invalidate_worker_status();
                }
                output.presenters = self.bus.presenters();
                Ok(output)
            }
            Err(err) => {
                // A failure may have started the process, or lost it mid-request.
                self.invalidate_worker_status();
                self.bus.publish(Event::SpeakFailed {
                    request_id,
                    code: err.code_str().to_string(),
                    message: err.message.clone(),
                });
                Err(err)
            }
        }
    }

    /// Frees the caller now; the device frees when the engine returns.
    pub fn cancel(&self, request_id: &str) -> bool {
        let Ok(map) = self.in_flight.lock() else {
            return false;
        };
        match map.get(request_id) {
            Some(cancel) => {
                cancel.flag.store(true, Ordering::SeqCst);
                cancel.notify.notify_waiters();
                true
            }
            None => false,
        }
    }

    /// Publish what a frontend reported about its own playback, and nothing else.
    ///
    /// Shape first, then the one thing that can be genuinely out of date: the audio id.
    /// It is checked against the spool both because a report about audio the runtime
    /// never produced is worth refusing, and because the entry is where the request that
    /// produced it is remembered — a reporter names the clip it played, and the event
    /// stream names the request, so the runtime supplies the join.
    pub fn report_playback(&self, input: PlayedInput) -> Result<(), ApiError> {
        let started = match input.event.as_str() {
            "started" => true,
            "finished" => false,
            other => {
                return Err(ApiError::new(
                    ErrorCode::InvalidRequest,
                    format!("unknown playback event '{other}'"),
                )
                .with_recovery(
                    RecoveryKind::FixRequest,
                    "`event` is `started` or `finished`",
                ))
            }
        };
        let by = match input.by.as_deref() {
            None | Some("presenter") => Reporter::Presenter,
            Some("cli") => Reporter::Cli,
            Some(other) => {
                return Err(ApiError::new(
                    ErrorCode::InvalidRequest,
                    format!("unknown playback reporter '{other}'"),
                )
                .with_recovery(
                    RecoveryKind::FixRequest,
                    "`by` is `presenter` (the default) or `cli`",
                ))
            }
        };
        let entry = self.spool.get(&input.audio_id).ok_or_else(|| {
            ApiError::new(
                ErrorCode::NotFound,
                format!("no audio with id '{}'", input.audio_id),
            )
            .with_recovery(
                RecoveryKind::FixRequest,
                "report the `audioId` /api/speak answered with; spool entries expire",
            )
        })?;

        let request_id = entry.request_id;
        let audio_id = input.audio_id;
        self.bus.publish(if started {
            Event::PlaybackStarted {
                request_id,
                audio_id,
                by,
            }
        } else {
            Event::PlaybackFinished {
                request_id,
                audio_id,
                by,
                played_ms: input.played_ms,
            }
        });
        Ok(())
    }

    /// Pay the cold start before a human is waiting on it.
    ///
    /// `ensure` alone is not that: it returns as soon as the process answers
    /// `/health`, and the model loads lazily inside the first `/synthesize`, so
    /// a caller that warmed and then spoke still paid the load with a human in
    /// front of it. This returns when the model is resident, which is what the
    /// `modelLoaded` in the reply means.
    pub async fn warm(&self) -> Result<WorkerStatus, ApiError> {
        self.worker.load_model().await.map_err(worker_error)?;
        self.worker.touch();
        // Probed, not cached: the load just changed the one field a caller reads
        // this reply for.
        Ok(self.refresh_worker_status().await)
    }

    /// Give the GPU back without stopping the runtime.
    pub async fn sleep(&self) -> WorkerStatus {
        if self.worker.managed() {
            self.worker.stop("sleep requested").await;
        }
        self.refresh_worker_status().await
    }

    pub async fn status(&self) -> Status {
        Status {
            name: "voice-core",
            runtime_version: RUNTIME_VERSION,
            api_version: API_VERSION,
            uptime_ms: self.started.elapsed().as_millis() as u64,
            worker: self.worker_status().await,
            voice_packs: self.voices().len(),
            presenters: self.bus.presenters(),
            in_flight: self.in_flight.lock().map(|m| m.len()).unwrap_or(0),
            spool: self.spool.stats(),
            idle_stop_ms: self.cfg.idle_stop.map(|d| d.as_millis() as u64),
        }
    }

    /// Worker status, reusing the last observation for [`WORKER_STATUS_TTL`].
    /// The idle clock is re-read on a hit because it is authoritative here and
    /// free, and a frozen one reads as a wedged runtime.
    async fn worker_status(&self) -> WorkerStatus {
        if let Ok(cached) = self.last_worker_status.lock() {
            if let Some(entry) = cached.as_ref() {
                if entry.at.elapsed() < WORKER_STATUS_TTL {
                    let mut status = entry.status.clone();
                    status.idle_ms = self.worker.idle_for().as_millis() as u64;
                    return status;
                }
            }
        }
        self.refresh_worker_status().await
    }

    /// Probes the worker now and replaces the cached observation.
    async fn refresh_worker_status(&self) -> WorkerStatus {
        let status = self.worker.status().await;
        if let Ok(mut cached) = self.last_worker_status.lock() {
            *cached = Some(CachedWorkerStatus {
                at: Instant::now(),
                status: status.clone(),
            });
        }
        status
    }

    /// Drops the cached observation after a transition this runtime caused, so
    /// the next poll reports the new state instead of a TTL of the old one.
    fn invalidate_worker_status(&self) {
        if let Ok(mut cached) = self.last_worker_status.lock() {
            *cached = None;
        }
    }

    pub fn metrics_snapshot(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Publish a diagnostic onto the event stream. Used for facts a frontend
    /// must see immediately — a failed resource preflight, for instance —
    /// without inventing a second channel for them.
    pub fn note(&self, phase: &str, message: String) {
        self.bus.publish(Event::Progress {
            request_id: None,
            phase: phase.to_string(),
            message,
        });
    }

    /// Actually stops the process. v1's `/api/shutdown` only mutated a status
    /// field and left the daemon serving, which made `stop` a lie.
    pub fn request_shutdown(&self) -> bool {
        let Ok(mut slot) = self.shutdown.lock() else {
            return false;
        };
        match slot.take() {
            Some(tx) => {
                self.bus.publish(Event::RuntimeStopping);
                tx.send(()).is_ok()
            }
            None => false,
        }
    }

    /// Spool TTL sweeping and idle worker reaping. Both are cheap timers; the
    /// reaper is what makes "low footprint" true, since an idle Python worker
    /// holding CUDA memory is the entire cost of this product.
    ///
    /// Reclaim is two-tier because the two resources cost different amounts to
    /// give back. Killing the worker at `idle_stop` also throws away the process
    /// and its torch import, so the next utterance repays that spawn (measured
    /// 3.3-3.9 s, see [`PROCESS_STOP_MULTIPLE`]) on top of a model load it was
    /// going to pay anyway — for a saving that is only pageable host memory. So
    /// `idle_stop` unloads the model and keeps the process; only after
    /// [`PROCESS_STOP_MULTIPLE`] windows does the process go too.
    pub fn spawn_background(self: &Arc<Self>) {
        let service = Arc::clone(self);
        tokio::spawn(async move {
            let mut ticker = tokio::time::interval(Duration::from_secs(30));
            // Owned by this task alone. `/unload` is idempotent, but re-issuing
            // it every 30 s would spend an HTTP request and an event on a worker
            // that is already empty.
            let mut vram_released = false;
            loop {
                ticker.tick().await;
                service.spool.sweep();

                let Some(idle_stop) = service.cfg.idle_stop else {
                    continue;
                };
                if !service.worker.managed() || !service.worker.is_running().await {
                    // Nothing resident, and a process that starts again starts
                    // with no model.
                    vram_released = false;
                    continue;
                }
                let idle = service.worker.idle_for();
                if idle < idle_stop {
                    // Used since the last reclaim: both windows start over.
                    vram_released = false;
                    continue;
                }
                // Hold the device across the reclaim rather than merely checking
                // that it is free: between a check and an `/unload` a speak can
                // acquire the permit and start synthesizing against the model
                // being dropped. `try_acquire` because a reclaim must never
                // queue — if a synthesis owns the device, this tick skips and the
                // next one is 30 s away.
                let Ok(_device) = service.gpu.try_acquire() else {
                    continue;
                };

                if idle >= idle_stop.saturating_mul(PROCESS_STOP_MULTIPLE) {
                    service
                        .worker
                        .stop(&format!(
                            "idle reclaim tier 2: idle for {}s; stopped the engine process, so the next utterance repays the torch import",
                            idle.as_secs()
                        ))
                        .await;
                    service.reclaimed("process", idle, idle_stop);
                    vram_released = false;
                    continue;
                }

                if vram_released {
                    continue;
                }
                // One string for both sinks: the worker log (so it is answerable from disk
                // with no frontend attached) and the event bus.
                let reason = format!(
                    "idle reclaim tier 1: idle for {}s; released GPU memory, engine process kept warm",
                    idle.as_secs()
                );
                match service.worker.release_vram(&reason).await {
                    Ok(()) => {
                        service.note("idle_reclaim", reason);
                        service.reclaimed("vram", idle, idle_stop);
                        vram_released = true;
                    }
                    // The latch stays down so the next tick retries: giving up
                    // here would leave VRAM held with nobody watching.
                    Err(err) => service.note(
                        "idle_reclaim",
                        format!("idle reclaim tier 1 failed: {err}; GPU memory is still held"),
                    ),
                }
            }
        });
    }

    /// One `metrics.jsonl` line per reclaim, plus the dropped status cache.
    /// `metrics.jsonl` is the runtime's only durable sink for this: a GUI
    /// launches the runtime with stdout unredirected, so a `println!` here would
    /// reach nobody.
    fn reclaimed(&self, tier: &str, idle: Duration, idle_stop: Duration) {
        self.metrics.record(serde_json::json!({
            "ts": crate::obs::now_ms(),
            "op": "idle_reclaim",
            "tier": tier,
            "idleMs": idle.as_millis() as u64,
            "idleStopMs": idle_stop.as_millis() as u64,
        }));
        self.invalidate_worker_status();
    }
}

/// Owned pipeline state so the synthesis can outlive its caller.
struct SynthJob {
    gpu: Arc<Semaphore>,
    worker: Arc<Worker>,
    spool: Arc<Spool>,
    bus: Arc<Bus>,
    metrics: Arc<Metrics>,
    cancel: Arc<Cancel>,
    request_id: String,
    audio_id: String,
    out_path: PathBuf,
    /// What to say, in order; `[pause:N]` split it. One entry is the common case.
    segments: Vec<String>,
    /// Silence in ms to splice between the segments; empty for a single segment.
    gaps: Vec<u32>,
    display_text: Option<String>,
    ruby_pairs: Option<Vec<RubyPair>>,
    display_seconds: Option<f64>,
    voice_pack_id: Option<String>,
    pack: Option<PackTarget>,
    seed: Option<u64>,
    num_steps: u32,
    deadline: Duration,
    started: Instant,
}

impl SynthJob {
    async fn run(self) -> Result<SpeakOutput, ApiError> {
        let queue_started = Instant::now();
        // Two clocks, on purpose. `deadline` is handed to the engine call below
        // and starts when that call starts, so time spent in line is never
        // charged to it: a queued utterance cannot fail with `deadline_exceeded`
        // for a synthesis the engine never began. The wait has its own bound —
        // the same number, the caller's stated patience, spent once in line and
        // once on the device — and exhausting it is `resource_busy`, which says
        // precisely that the request never reached the engine. The alternative,
        // an unbounded acquire, is a caller stuck behind a wedged worker with
        // nothing to report.
        let _permit = match tokio::time::timeout(self.deadline, self.gpu.acquire()).await {
            Ok(Ok(permit)) => permit,
            Ok(Err(_)) => return Err(ApiError::new(ErrorCode::Internal, "device queue closed")),
            Err(_) => {
                let waited = queue_started.elapsed().as_millis() as u64;
                self.metrics.speak_err();
                self.metrics.record(serde_json::json!({
                    "ts": crate::obs::now_ms(),
                    "op": "speak",
                    "requestId": self.request_id,
                    "ok": false,
                    "error": "device queue wait exceeded the caller's timeout",
                    "queueMs": waited,
                    "synthMs": 0,
                }));
                // Nothing to abandon: the reservation never reached the engine,
                // so no producer can be writing that path.
                return Err(ApiError::new(
                    ErrorCode::ResourceBusy,
                    format!("waited {waited} ms for the device; another utterance still holds it"),
                )
                .with_recovery(
                    RecoveryKind::Wait,
                    "one utterance at a time; retry, or raise timeoutMs - it bounds the wait separately from the synthesis",
                ));
            }
        };
        let queue_ms = queue_started.elapsed().as_millis() as u64;

        self.worker.touch();
        let base_url = self
            .worker
            .ensure("speak request")
            .await
            .map_err(worker_error)?;

        // Cold means the model is not resident yet, so this call pays the load.
        let cold_start = !self.worker.engine().health(&base_url).await.model_loaded;
        if cold_start {
            self.bus.publish(Event::Progress {
                request_id: Some(self.request_id.clone()),
                phase: "model_load".into(),
                message: "loading the voice model; first call after start is slow".into(),
            });
        }

        let synth_started = Instant::now();
        let result = self.synthesize_all(&base_url).await;
        let synth_ms = synth_started.elapsed().as_millis() as u64;
        self.worker.touch();

        let output = match result {
            Ok(output) => output,
            Err(err) => {
                // A `deadline_exceeded` here means the worker is still synthesizing and
                // will write this path later, so handing it to the spool is the
                // only safe cleanup: the spool deletes it once it appears and
                // never indexes it. See `Spool::abandon`.
                self.spool.abandon(&self.audio_id);
                self.metrics.speak_err();
                self.metrics.record(serde_json::json!({
                    "ts": crate::obs::now_ms(),
                    "op": "speak",
                    "requestId": self.request_id,
                    "ok": false,
                    "error": err.message,
                    "queueMs": queue_ms,
                    "synthMs": synth_ms,
                }));
                return Err(err);
            }
        };

        // A cancelled utterance must not reach a presenter's speakers. The engine
        // has already returned here, so the file is whole and `abandon` deletes it
        // outright; cancelling frees the caller, never the device.
        if self.cancel.flag.load(Ordering::SeqCst) {
            self.spool.abandon(&self.audio_id);
            self.metrics.speak_err();
            self.metrics.record(serde_json::json!({
                "ts": crate::obs::now_ms(),
                "op": "speak",
                "requestId": self.request_id,
                "ok": false,
                "error": "cancelled",
                "queueMs": queue_ms,
                "synthMs": synth_ms,
            }));
            return Err(ApiError::new(ErrorCode::Cancelled, "cancelled by caller"));
        }

        let bytes = self
            .spool
            .commit(
                &self.audio_id,
                &self.request_id,
                output.sample_rate,
                output.duration_ms,
            )
            .map_err(|err| {
                ApiError::new(
                    ErrorCode::Internal,
                    format!("engine reported success but wrote no audio: {err}"),
                )
                .with_recovery(RecoveryKind::CheckWorkerLogs, "see logs/tts-worker.err.log")
            })?;

        let total_ms = self.started.elapsed().as_millis() as u64;
        let spoken = self.segments.concat();
        self.metrics.speak_ok(total_ms, bytes, cold_start);
        let mut record = serde_json::json!({
            "ts": crate::obs::now_ms(),
            "op": "speak",
            "requestId": self.request_id,
            "ok": true,
            "audioId": self.audio_id,
            "voicePackId": self.voice_pack_id,
            "chars": spoken.chars().count(),
            "coldStart": cold_start,
            "queueMs": queue_ms,
            "synthMs": synth_ms,
            "totalMs": total_ms,
            "audioBytes": bytes,
            "durationMs": output.duration_ms,
            "sampleRate": output.sample_rate,
        });
        // Only when a pause split the utterance: the record for an ordinary speak keeps
        // exactly the shape everything reading `metrics.jsonl` already knows.
        if !self.gaps.is_empty() {
            record["segments"] = self.segments.len().into();
            record["pauseMs"] = self.gaps.iter().sum::<u32>().into();
        }
        self.metrics.record(record);

        self.bus.publish(Event::Speech {
            request_id: self.request_id.clone(),
            audio_id: self.audio_id.clone(),
            // The markers are gone: what a presenter shows as the spoken line, and what
            // `rubyPairs` was reconciled against, is what the engine was actually asked.
            text: spoken,
            display_text: self.display_text.clone(),
            ruby_pairs: self.ruby_pairs.clone(),
            voice_pack_id: self.voice_pack_id.clone(),
            duration_ms: output.duration_ms,
            sample_rate: output.sample_rate,
            display_seconds: self.display_seconds,
        });

        Ok(SpeakOutput {
            request_id: self.request_id,
            audio_id: self.audio_id,
            sample_rate: output.sample_rate,
            duration_ms: output.duration_ms,
            bytes,
            display_text: self.display_text,
            voice_pack_id: self.voice_pack_id,
            presenters: 0, // filled in by the caller, which owns the bus
            cold_start,
            queue_ms,
            synth_ms,
            total_ms,
        })
    }

    /// One engine call per segment, and one WAV out of them.
    ///
    /// A single segment — every utterance without a `[pause:N]` — is exactly what it
    /// always was: the engine writes the spool file the caller was promised and nothing
    /// copies a sample. A pause splits the line, so the parts land in reservations of
    /// their own and are spliced into that file, which is why the caller still gets one
    /// `audioId`, one `Speech` and one `durationMs` that counts the silence.
    async fn synthesize_all(&self, base_url: &str) -> Result<SynthOutput, ApiError> {
        if self.gaps.is_empty() {
            return self
                .say(base_url, &self.segments[0], &self.out_path, self.deadline)
                .await;
        }

        let began = Instant::now();
        let mut parts: Vec<(String, PathBuf)> = Vec::with_capacity(self.segments.len());
        let spliced = self.synthesize_parts(base_url, began, &mut parts).await;
        // The parts were never committed, so nothing could have served them; from here
        // the spool owns their paths and deletes them even if a timed-out worker is
        // still writing one.
        for (id, _) in &parts {
            self.spool.abandon(id);
        }
        spliced
    }

    /// The segment loop, split out so its reservations are cleaned up on every exit.
    async fn synthesize_parts(
        &self,
        base_url: &str,
        began: Instant,
        parts: &mut Vec<(String, PathBuf)>,
    ) -> Result<SynthOutput, ApiError> {
        for segment in &self.segments {
            // One budget for the whole utterance, not one per segment: `timeoutMs` is
            // what the caller said it would wait for a synthesis, and a five-part line
            // must not be able to spend five times it.
            let left = self
                .deadline
                .checked_sub(began.elapsed())
                .filter(|left| !left.is_zero())
                .ok_or_else(|| {
                    engine_error(EngineError::Deadline(self.deadline.as_millis() as u64))
                })?;
            // Reserved, then remembered before the call is awaited: the caller of this
            // function deletes every path in `parts`, and a segment that fails halfway
            // must not be the one left behind.
            let (id, path) = self.spool.reserve();
            let said = self.say(base_url, segment, &path, left).await;
            parts.push((id, path));
            said?;
        }

        let paths: Vec<&Path> = parts.iter().map(|(_, path)| path.as_path()).collect();
        splice(&paths, &self.gaps, &self.out_path).map_err(|err| {
            ApiError::new(
                ErrorCode::Internal,
                format!(
                    "cannot splice {} pause-separated segment(s) into one WAV: {err}",
                    self.segments.len()
                ),
            )
            .with_recovery(
                RecoveryKind::FixRequest,
                "send the same text without `[pause:N]` to get one unsplit segment",
            )
        })
    }

    /// One engine call. The engine writes the WAV; the runtime only says where.
    async fn say(
        &self,
        base_url: &str,
        text: &str,
        out_path: &Path,
        deadline: Duration,
    ) -> Result<SynthOutput, ApiError> {
        self.worker
            .engine()
            .synthesize(
                base_url,
                SynthRequest {
                    text,
                    pack: self.pack.clone(),
                    seed: self.seed,
                    num_steps: self.num_steps,
                    out_path,
                },
                deadline,
            )
            .await
            .map_err(engine_error)
    }
}

fn worker_error(err: WorkerError) -> ApiError {
    match err {
        WorkerError::ExternalUnreachable { ref base_url } => {
            ApiError::new(ErrorCode::WorkerUnavailable, err.to_string()).with_recovery(
                RecoveryKind::Retry,
                format!("start the worker you attached at {base_url}, then retry"),
            )
        }
        WorkerError::Spawn(_) => ApiError::new(ErrorCode::WorkerStartFailed, err.to_string())
            .with_recovery(
                RecoveryKind::FixRequest,
                "check --tts-python and --tts-script point at real files",
            ),
        WorkerError::NotReady { .. } => {
            ApiError::new(ErrorCode::WorkerStartFailed, err.to_string())
                .with_recovery(RecoveryKind::CheckWorkerLogs, "see logs/tts-worker.err.log")
        }
        // A worker that started, answered `/health` and then refused to load its
        // model is a third failure, not a start failure: the actionable detail is
        // the checkpoint, not the interpreter.
        WorkerError::Control { .. } => ApiError::new(ErrorCode::ModelLoadFailed, err.to_string())
            .with_recovery(RecoveryKind::CheckWorkerLogs, "see logs/tts-worker.err.log"),
    }
}

fn engine_error(err: EngineError) -> ApiError {
    match err {
        EngineError::Deadline(ms) => ApiError::new(
            ErrorCode::DeadlineExceeded,
            format!("synthesis exceeded {ms} ms"),
        )
        .with_recovery(
            RecoveryKind::Retry,
            "the first call after start loads the model; retry with a larger timeoutMs",
        ),
        EngineError::ModelLoad(_) => ApiError::new(ErrorCode::ModelLoadFailed, err.to_string())
            .with_recovery(RecoveryKind::CheckWorkerLogs, "see logs/tts-worker.err.log"),
        // The engine loaded and then refused this utterance — a pack with no reference
        // audio, a device out of memory. `internal` is the documented bucket for that;
        // what makes it actionable is the worker's own reason now inside the message.
        EngineError::Synthesis(_) => ApiError::new(ErrorCode::Internal, err.to_string())
            .with_recovery(RecoveryKind::CheckWorkerLogs, "see logs/tts-worker.err.log"),
        EngineError::Unreachable(_) => ApiError::new(ErrorCode::WorkerUnavailable, err.to_string())
            .with_recovery(RecoveryKind::Retry, "the worker died mid-request; retry"),
        EngineError::Status { .. } | EngineError::Malformed(_) => {
            ApiError::new(ErrorCode::Internal, err.to_string())
                .with_recovery(RecoveryKind::CheckWorkerLogs, "see logs/tts-worker.err.log")
        }
    }
}

// -- what the caller wrote ---------------------------------------------------

/// An utterance split at its `[pause:N]` markers: what to synthesize, and how much
/// silence to splice between the pieces.
///
/// One primitive, honestly implemented: the marker is removed from the text and paid
/// for in samples, so `durationMs` counts it, one `audioId` covers it, and no engine
/// has to learn a markup language. Prosody markup that the engine cannot honour is a
/// promise this project has already broken once (`docs/v1/adr/0003-prosody-markup.md`).
#[derive(Debug)]
struct Script {
    /// In reading order, never empty. Concatenated, this is what was spoken.
    segments: Vec<String>,
    /// Silence in ms between segment `i` and `i + 1`; `segments.len() - 1` long.
    gaps: Vec<u32>,
    /// Markers that were dropped rather than honoured, for the event stream.
    notes: Vec<String>,
}

impl Script {
    /// Splits at every marker, and rejects a malformed one by name instead of reading
    /// it out loud as text.
    fn parse(text: &str) -> Result<Self, ApiError> {
        let mut script = Self {
            segments: Vec::new(),
            gaps: Vec::new(),
            notes: Vec::new(),
        };
        let mut owed = 0u32;
        let mut rest = text;

        while let Some(at) = next_marker(rest) {
            let (before, from_marker) = rest.split_at(at);
            let (ms, after) = parse_marker(from_marker)?;
            // Markers with nothing speakable between them sum: two beats in a row are
            // one longer beat, not an empty utterance in the middle.
            owed = script.push(before, owed).saturating_add(ms);
            rest = after;
        }

        let owed = script.push(rest, owed);
        if owed > 0 && !script.segments.is_empty() {
            script.notes.push(Self::dropped("trailing", owed));
        }
        if script.segments.is_empty() {
            return Err(ApiError::new(
                ErrorCode::InvalidRequest,
                "text has nothing to speak but pause markers",
            )
            .with_recovery(
                RecoveryKind::FixRequest,
                "`[pause:N]` splices silence between spoken segments; it is not an utterance",
            ));
        }
        Ok(script)
    }

    /// Adds `piece` as a segment when there is anything to say in it, taking `owed`
    /// silence as the gap in front of it. Returns the silence still owed, which is how
    /// a piece with nothing speakable in it keeps the surrounding pauses together.
    fn push(&mut self, piece: &str, owed: u32) -> u32 {
        if piece.trim().is_empty() {
            return owed;
        }
        if self.segments.is_empty() {
            if owed > 0 {
                self.notes.push(Self::dropped("leading", owed));
            }
        } else {
            self.gaps.push(owed);
        }
        self.segments.push(piece.to_string());
        0
    }

    /// What the engine was asked to say: the text minus its markers.
    fn spoken(&self) -> String {
        self.segments.concat()
    }

    /// Silence at an edge is the caller's own wait, and honouring it would put dead air
    /// at the front or back of every clip where nobody can see where it came from.
    fn dropped(edge: &str, ms: u32) -> String {
        format!(
            "dropped a {edge} {ms} ms pause: `[pause:N]` splices silence BETWEEN spoken \
             segments, and a pause at the edge of an utterance is the caller's own wait"
        )
    }
}

/// Byte offset of the next marker opener, matched case-insensitively — a `[Pause:600]`
/// read out as literal text is exactly the silent failure this primitive removes. `[`
/// never occurs inside a multi-byte UTF-8 sequence, so the offset is a char boundary.
fn next_marker(text: &str) -> Option<usize> {
    text.as_bytes()
        .windows(PAUSE_OPEN.len())
        .position(|window| window.eq_ignore_ascii_case(PAUSE_OPEN.as_bytes()))
}

/// Reads one marker off the front of `text`, returning its milliseconds and what
/// follows it.
fn parse_marker(text: &str) -> Result<(u32, &str), ApiError> {
    let body = &text[PAUSE_OPEN.len()..];
    let Some(end) = body.find(']') else {
        return Err(bad_marker(&clip(text, 24), "it has no closing `]`"));
    };
    let marker = &text[..PAUSE_OPEN.len() + end + 1];
    let Ok(ms) = body[..end].trim().parse::<u32>() else {
        return Err(bad_marker(
            marker,
            "N must be a whole number of milliseconds",
        ));
    };
    if !PAUSE_RANGE.contains(&ms) {
        return Err(bad_marker(
            marker,
            &format!(
                "{ms} ms is outside {}-{} ms",
                PAUSE_RANGE.start(),
                PAUSE_RANGE.end()
            ),
        ));
    }
    Ok((ms, &body[end + 1..]))
}

fn bad_marker(marker: &str, why: &str) -> ApiError {
    ApiError::new(
        ErrorCode::InvalidRequest,
        format!("invalid pause marker `{marker}` in text: {why}"),
    )
    .with_recovery(
        RecoveryKind::FixRequest,
        format!(
            "the only pause primitive is `[pause:N]`, N in {}-{} ms; it is never spoken \
             aloud, so a marker the runtime cannot read is refused instead",
            PAUSE_RANGE.start(),
            PAUSE_RANGE.end()
        ),
    )
}

/// Rejects an alignment that does not reconstruct the strings it claims to align.
///
/// The array is the caller's own segmentation and nothing downstream can repair it: a
/// presenter handed pairs that do not reconcile can only fall back to one coarse
/// annotation, which reads as a rendering choice rather than the caller bug it is.
fn check_alignment(
    pairs: &[RubyPair],
    spoken: &str,
    display_text: Option<&str>,
) -> Result<(), ApiError> {
    // With no `displayText` a presenter shows the spoken line itself, so that is the
    // string `base` has to segment (`DialogPresenter::Present`).
    let (display, display_name) = match display_text {
        Some(display) => (display, "displayText"),
        None => (spoken, "text (no displayText was sent)"),
    };
    reconcile("base", display_name, display, pairs, |pair| &pair.base)?;
    reconcile("ruby", "text", spoken, pairs, |pair| &pair.ruby)
}

/// Walks one side of the alignment against its source string and says exactly where
/// the two stopped agreeing: which pair, how far it had got, and both fragments.
fn reconcile(
    field: &str,
    source_name: &str,
    source: &str,
    pairs: &[RubyPair],
    fragment: impl Fn(&RubyPair) -> &str,
) -> Result<(), ApiError> {
    let mut cursor = 0usize;
    let mut chars = 0usize;
    for (index, pair) in pairs.iter().enumerate() {
        let piece = fragment(pair);
        // An empty fragment is legal and matches nothing: punctuation exists on one
        // side only often enough that the pair still has to be sent.
        if source[cursor..].starts_with(piece) {
            cursor += piece.len();
            chars += piece.chars().count();
            continue;
        }
        return Err(mismatch(
            field,
            source_name,
            format!(
                "rubyPairs[{index}].{field} does not line up with {source_name}: the first \
                 {index} pair(s) reconstructed {chars} character(s), then this one offers \
                 `{piece}` where {source_name} has `{}`",
                clip(&source[cursor..], piece.chars().count().max(8)),
            ),
        ));
    }
    if cursor < source.len() {
        return Err(mismatch(
            field,
            source_name,
            format!(
                "the rubyPairs `{field}` fragments do not reconstruct {source_name}: {} pair(s) \
                 reached character {chars} of {}, leaving `{}` unaccounted for",
                pairs.len(),
                source.chars().count(),
                clip(&source[cursor..], 24),
            ),
        ));
    }
    Ok(())
}

fn mismatch(field: &str, source_name: &str, message: String) -> ApiError {
    ApiError::new(ErrorCode::InvalidRequest, message).with_recovery(
        RecoveryKind::FixRequest,
        format!(
            "concatenating every `{field}` must reproduce `{source_name}` exactly, punctuation \
             included and pause markers excluded; send no rubyPairs at all rather than an array \
             that does not"
        ),
    )
}

/// First `chars` characters, marked when there is more. Error messages quote the
/// caller's own text, and a whole paragraph inside one is unreadable.
fn clip(text: &str, chars: usize) -> String {
    let mut out = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index == chars {
            out.push('…');
            break;
        }
        out.push(ch);
    }
    out
}

/// Refuses text in a language the resolved pack does not claim to speak.
///
/// Nothing routes on this yet — routing needs a second engine, which is a later
/// slice. What exists now is the field, this check and the code, because the failure
/// it replaces is silent: Chinese text through a Japanese-only adapter produces
/// confident garbage that no error anywhere reports.
fn check_language(asked: &str, pack: &ResolvedPack) -> Result<(), ApiError> {
    // A pack that declares nothing cannot contradict the caller. Manifest-less packs
    // are legal (`docs/voicepack-spec.md`), so this must not invent a claim for them.
    if pack.languages.is_empty()
        || pack
            .languages
            .iter()
            .any(|declared| tags_match(declared, asked))
    {
        return Ok(());
    }
    Err(ApiError::new(
        ErrorCode::VoiceLanguageUnsupported,
        format!(
            "voice pack '{}' declares [{}] and cannot speak '{asked}'",
            pack.id,
            pack.languages.join(", ")
        ),
    )
    .with_recovery(
        RecoveryKind::FixRequest,
        format!(
            "send `language` as one of [{}], pick a pack that declares '{asked}', or omit \
             `language` to speak whatever the text is",
            pack.languages.join(", ")
        ),
    ))
}

/// BCP-47 tags compare case-insensitively, and one side may be more specific than the
/// other: `ja-JP` asked of a pack that says `ja` is the same voice. A difference below
/// the primary subtag is not — `zh-TW` against `zh-CN` is a different reading.
fn tags_match(declared: &str, asked: &str) -> bool {
    let mut declared = declared.trim().split('-');
    let mut asked = asked.trim().split('-');
    loop {
        match (declared.next(), asked.next()) {
            (Some(left), Some(right)) if left.eq_ignore_ascii_case(right) => continue,
            (Some(_), Some(_)) => return false,
            // One ran out of subtags: the shorter is a prefix of the longer.
            _ => return true,
        }
    }
}

// -- splicing the pauses in --------------------------------------------------

/// What splicing needs from a part's header: enough to verify the parts agree, to size
/// the silence in frames, and to re-emit a header for the whole.
#[derive(Clone, Copy, PartialEq, Eq)]
struct WavFormat {
    audio_format: u16,
    channels: u16,
    sample_rate: u32,
    block_align: u16,
    bits: u16,
}

impl WavFormat {
    fn byte_rate(&self) -> u32 {
        self.sample_rate * self.block_align as u32
    }
}

/// Concatenates `parts` into `out_path` with `gaps[i]` ms of silence between them, and
/// reports what the result really is rather than what the engine claimed the pieces
/// were.
///
/// This is the only place the runtime touches samples, and it still never holds an
/// utterance: each part streams through a fixed buffer and the silence comes from a
/// zero page. Zeros are silence in every format the engine can write, which is why the
/// format only has to be *agreed* between the parts, not interpreted.
fn splice(parts: &[&Path], gaps: &[u32], out_path: &Path) -> io::Result<SynthOutput> {
    let mut out = io::BufWriter::new(std::fs::File::create(out_path)?);
    // A WAV header states sizes it cannot know yet, so leave room and patch it.
    out.write_all(&[0u8; WAV_HEADER_BYTES])?;

    let mut format: Option<WavFormat> = None;
    let mut data_bytes = 0u64;
    for (index, part) in parts.iter().enumerate() {
        let (part_format, file, size) = open_pcm(part)?;
        match format {
            None => format = Some(part_format),
            Some(first) if first == part_format => {}
            Some(first) => {
                return Err(io::Error::other(format!(
                    "segment {index} is {} Hz/{} channel(s) but the first is {} Hz/{}: the \
                     engine changed format mid-utterance",
                    part_format.sample_rate, part_format.channels, first.sample_rate, first.channels
                )))
            }
        }
        if let Some(gap) = index.checked_sub(1).map(|previous| gaps[previous]) {
            data_bytes += write_silence(&mut out, gap, part_format)?;
        }
        data_bytes += io::copy(&mut file.take(size), &mut out)?;
    }

    let Some(format) = format else {
        return Err(io::Error::other("nothing to splice"));
    };
    if data_bytes > u32::MAX as u64 - WAV_HEADER_BYTES as u64 {
        return Err(io::Error::other("spliced audio exceeds the 4 GiB WAV limit"));
    }

    let mut file = out
        .into_inner()
        .map_err(|err| io::Error::other(err.to_string()))?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&wav_header(format, data_bytes as u32))?;
    file.flush()?;

    Ok(SynthOutput {
        sample_rate: format.sample_rate,
        // From the bytes on disk, not the sum of what the engine reported plus the
        // pauses: `durationMs` is what a caller waits out, so it has to be the file.
        duration_ms: data_bytes * 1000 / format.byte_rate() as u64,
    })
}

/// Canonical PCM header: `RIFF`, a 16-byte `fmt `, `data`.
const WAV_HEADER_BYTES: usize = 44;

/// Opens a part and leaves it at the first byte of PCM, with the format it declared
/// and how many bytes of samples follow.
fn open_pcm(path: &Path) -> io::Result<(WavFormat, std::fs::File, u64)> {
    let mut file = std::fs::File::open(path)?;
    let mut riff = [0u8; 12];
    file.read_exact(&mut riff)?;
    if &riff[..4] != b"RIFF" || &riff[8..] != b"WAVE" {
        return Err(io::Error::other(format!(
            "{} is not a RIFF/WAVE file",
            path.display()
        )));
    }

    let mut format: Option<WavFormat> = None;
    let mut header = [0u8; 8];
    loop {
        // An `UnexpectedEof` here is a file with no `data` chunk, which is a broken
        // part and reads as one.
        file.read_exact(&mut header)?;
        let size = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as u64;
        // Chunks are word aligned, and the pad byte is not part of the size.
        let advance = (size + (size & 1)) as i64;
        match &header[..4] {
            b"fmt " if size >= 16 => {
                let mut body = [0u8; 16];
                file.read_exact(&mut body)?;
                let read_u16 = |at: usize| u16::from_le_bytes([body[at], body[at + 1]]);
                let candidate = WavFormat {
                    audio_format: read_u16(0),
                    channels: read_u16(2),
                    sample_rate: u32::from_le_bytes([body[4], body[5], body[6], body[7]]),
                    block_align: read_u16(12),
                    bits: read_u16(14),
                };
                // Extensible and compressed WAVs would need their `fmt ` chunk copied
                // verbatim, and their silence is not zeros. The engine writes PCM_16
                // (`worker/irodori/worker.py::_write_wav`), so refuse rather than guess.
                if candidate.audio_format != 1 && candidate.audio_format != 3 {
                    return Err(io::Error::other(format!(
                        "{} is WAV format {} (only uncompressed PCM and float can be spliced)",
                        path.display(),
                        candidate.audio_format
                    )));
                }
                if candidate.block_align == 0 || candidate.sample_rate == 0 {
                    return Err(io::Error::other(format!(
                        "{} declares no frame size",
                        path.display()
                    )));
                }
                format = Some(candidate);
                file.seek(SeekFrom::Current(advance - 16))?;
            }
            b"data" => {
                let Some(format) = format else {
                    return Err(io::Error::other(format!(
                        "{} has audio before it says what format it is in",
                        path.display()
                    )));
                };
                return Ok((format, file, size));
            }
            _ => {
                file.seek(SeekFrom::Current(advance))?;
            }
        }
    }
}

/// `ms` of silence, rounded down to whole frames. Returns the bytes written.
fn write_silence(out: &mut impl Write, ms: u32, format: WavFormat) -> io::Result<u64> {
    const ZEROS: [u8; 4096] = [0; 4096];
    let frames = ms as u64 * format.sample_rate as u64 / 1000;
    let total = frames * format.block_align as u64;
    let mut left = total;
    while left > 0 {
        let take = left.min(ZEROS.len() as u64) as usize;
        out.write_all(&ZEROS[..take])?;
        left -= take as u64;
    }
    Ok(total)
}

fn wav_header(format: WavFormat, data_bytes: u32) -> [u8; WAV_HEADER_BYTES] {
    let mut header = [0u8; WAV_HEADER_BYTES];
    let mut put = |at: usize, bytes: &[u8]| header[at..at + bytes.len()].copy_from_slice(bytes);
    put(0, b"RIFF");
    put(4, &(data_bytes + WAV_HEADER_BYTES as u32 - 8).to_le_bytes());
    put(8, b"WAVEfmt ");
    put(16, &16u32.to_le_bytes());
    put(20, &format.audio_format.to_le_bytes());
    put(22, &format.channels.to_le_bytes());
    put(24, &format.sample_rate.to_le_bytes());
    put(28, &format.byte_rate().to_le_bytes());
    put(32, &format.block_align.to_le_bytes());
    put(34, &format.bits.to_le_bytes());
    put(36, b"data");
    put(40, &data_bytes.to_le_bytes());
    header
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(items: &[(&str, &str)]) -> Vec<RubyPair> {
        items
            .iter()
            .map(|(base, ruby)| RubyPair {
                base: (*base).to_string(),
                ruby: (*ruby).to_string(),
            })
            .collect()
    }

    #[test]
    fn a_pause_splits_the_utterance_and_leaves_the_text_alone() {
        let script = Script::parse("おはよう[pause:600]先生。").unwrap();
        assert_eq!(script.segments, ["おはよう", "先生。"]);
        assert_eq!(script.gaps, [600]);
        assert_eq!(script.spoken(), "おはよう先生。");
        assert!(script.notes.is_empty());

        // No marker: one segment, no splicing, nothing said about it.
        let plain = Script::parse("おはよう先生。").unwrap();
        assert_eq!(plain.segments, ["おはよう先生。"]);
        assert!(plain.gaps.is_empty());
    }

    #[test]
    fn adjacent_markers_sum_and_edge_markers_are_dropped_with_a_note() {
        let script = Script::parse("[pause:200]あ[pause:300][pause:300]い[pause:400]").unwrap();
        assert_eq!(script.segments, ["あ", "い"]);
        assert_eq!(script.gaps, [600], "two markers in a row are one pause");
        assert_eq!(script.notes.len(), 2, "the leading and trailing ones");
        assert!(script.notes[0].contains("leading 200 ms"));
        assert!(script.notes[1].contains("trailing 400 ms"));

        // Whitespace between two markers is not an utterance, so they still sum.
        let spaced = Script::parse("あ[pause:100] [pause:100]い").unwrap();
        assert_eq!(spaced.gaps, [200]);
    }

    #[test]
    fn a_malformed_marker_is_named_and_never_spoken() {
        for text in [
            "あ[pause:abc]い",
            "あ[pause:99999]い",
            "あ[pause:0]い",
            "あ[pause:600",
            "あ[Pause:]い",
        ] {
            let err = Script::parse(text).expect_err(text);
            assert_eq!(err.code, ErrorCode::InvalidRequest, "{text}");
            assert!(
                err.message.contains("pause"),
                "the message must quote the marker: {}",
                err.message
            );
        }
        // Markers only: there is nothing left to synthesize.
        assert_eq!(
            Script::parse("[pause:500]").unwrap_err().code,
            ErrorCode::InvalidRequest
        );
    }

    #[test]
    fn alignment_names_the_pair_that_broke_it() {
        let good = pairs(&[("欢迎回来", "おかえりなさい"), ("，", "、"), ("老师。", "先生。")]);
        check_alignment(&good, "おかえりなさい、先生。", Some("欢迎回来，老师。")).unwrap();

        // Same text, one pair's ruby replaced: the error must point at index 2.
        let broken = pairs(&[("欢迎回来", "おかえりなさい"), ("，", "、"), ("老师。", "せんせい。")]);
        let err = check_alignment(&broken, "おかえりなさい、先生。", Some("欢迎回来，老师。"))
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidRequest);
        assert!(err.message.starts_with("rubyPairs[2].ruby"), "{}", err.message);
        assert!(err.message.contains("せんせい。"), "{}", err.message);
        assert!(err.message.contains("先生。"), "{}", err.message);

        // A short array reconstructs a prefix and stops; that is a mismatch too.
        let short = pairs(&[("欢迎回来", "おかえりなさい")]);
        let err = check_alignment(&short, "おかえりなさい、先生。", Some("欢迎回来，老师。"))
            .unwrap_err();
        assert!(err.message.contains("unaccounted for"), "{}", err.message);
    }

    #[test]
    fn alignment_is_checked_against_the_text_without_markers() {
        let script = Script::parse("おかえりなさい[pause:600]先生。").unwrap();
        let pairs = pairs(&[("欢迎回来", "おかえりなさい"), ("老师。", "先生。")]);
        check_alignment(&pairs, &script.spoken(), Some("欢迎回来老师。")).unwrap();
    }

    #[test]
    fn language_tags_compare_by_subtag() {
        assert!(tags_match("ja", "JA"));
        assert!(tags_match("ja", "ja-JP"), "a request may be more specific");
        assert!(tags_match("zh-CN", "zh"));
        assert!(!tags_match("zh-CN", "zh-TW"));
        assert!(!tags_match("ja", "zh-CN"));

        let pack = ResolvedPack {
            id: "ba-miyu-lora".into(),
            languages: vec!["ja".into()],
            target: PackTarget {
                kind: "lora-adapter",
                path: String::new(),
            },
        };
        check_language("ja-JP", &pack).unwrap();
        let err = check_language("zh-CN", &pack).unwrap_err();
        assert_eq!(err.code, ErrorCode::VoiceLanguageUnsupported);
        assert!(err.message.contains("ba-miyu-lora"), "{}", err.message);
        assert!(err.message.contains("ja"), "{}", err.message);

        // A pack that declares nothing cannot contradict anybody.
        let silent = ResolvedPack {
            languages: Vec::new(),
            ..pack
        };
        check_language("zh-CN", &silent).unwrap();
    }
}
