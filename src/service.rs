//! Orchestration: the single owner of a request's lifecycle.
//!
//! Routes stay dumb on purpose. Everything that must be identical for every
//! request lives here exactly once — request id, queueing against the single
//! GPU, worker readiness, deadline, cancellation, spool registration, event
//! publication, metrics. v1 spread these across handlers, which is how it ended
//! up with a 120 s timeout in one route and 300 s in another.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex as SyncMutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{oneshot, Notify, Semaphore};

use crate::config::Config;
use crate::engine::{EngineError, PackTarget, SynthRequest, TtsEngine};
use crate::error::{ApiError, ErrorCode, RecoveryKind};
use crate::obs::{short_id, Bus, Event, Metrics, MetricsSnapshot};
use crate::packs::{Registry, VoicePack};
use crate::spool::{Spool, SpoolStats};
use crate::supervise::{Worker, WorkerError, WorkerStatus};

pub const RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");
/// Bumped only on a breaking change to the public surface.
pub const API_VERSION: u32 = 1;

const DEFAULT_NUM_STEPS: u32 = 32;

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
    fn resolve_pack(&self, id: Option<&str>) -> Result<Option<(String, PackTarget)>, ApiError> {
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
        let target = PackTarget {
            kind: pack.kind.as_wire(),
            path: packs.resolve_path(pack),
        };
        Ok(Some((pack.id.clone(), target)))
    }

    /// The whole point of this module: one path, one set of semantics.
    pub async fn speak(&self, input: SpeakInput) -> Result<SpeakOutput, ApiError> {
        let request_id = short_id();
        let started = Instant::now();

        if input.text.trim().is_empty() {
            return Err(
                ApiError::new(ErrorCode::InvalidRequest, "text is empty").with_recovery(
                    RecoveryKind::FixRequest,
                    "send non-empty `text`; `displayText` is what humans read",
                ),
            );
        }

        let pack = self.resolve_pack(input.voice_pack_id.as_deref())?;
        let voice_pack_id = pack.as_ref().map(|(id, _)| id.clone());
        let pack_target = pack.map(|(_, target)| target);

        self.bus.publish(Event::SpeakStarted {
            request_id: request_id.clone(),
            voice_pack_id: voice_pack_id.clone(),
            chars: input.text.chars().count(),
        });

        let cancel = Arc::new(Cancel {
            flag: AtomicBool::new(false),
            notify: Notify::new(),
        });
        if let Ok(mut map) = self.in_flight.lock() {
            map.insert(request_id.clone(), Arc::clone(&cancel));
        }

        let deadline = input
            .timeout_ms
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
            text: input.text.clone(),
            display_text: input.display_text.clone(),
            ruby_pairs: input.ruby_pairs.clone(),
            display_seconds: input.display_seconds,
            voice_pack_id: voice_pack_id.clone(),
            pack: pack_target,
            seed: input.seed,
            num_steps: input.num_steps.unwrap_or(DEFAULT_NUM_STEPS),
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
    out_path: std::path::PathBuf,
    text: String,
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
        let result = self
            .worker
            .engine()
            .synthesize(
                &base_url,
                SynthRequest {
                    text: &self.text,
                    pack: self.pack.clone(),
                    seed: self.seed,
                    num_steps: self.num_steps,
                    out_path: &self.out_path,
                },
                self.deadline,
            )
            .await;
        let synth_ms = synth_started.elapsed().as_millis() as u64;
        self.worker.touch();

        let output = match result {
            Ok(output) => output,
            Err(err) => {
                // A `Deadline` here means the worker is still synthesizing and
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
                    "error": err.to_string(),
                    "queueMs": queue_ms,
                    "synthMs": synth_ms,
                }));
                return Err(engine_error(err));
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
            .commit(&self.audio_id, output.sample_rate, output.duration_ms)
            .map_err(|err| {
                ApiError::new(
                    ErrorCode::Internal,
                    format!("engine reported success but wrote no audio: {err}"),
                )
                .with_recovery(RecoveryKind::CheckWorkerLogs, "see logs/tts-worker.err.log")
            })?;

        let total_ms = self.started.elapsed().as_millis() as u64;
        self.metrics.speak_ok(total_ms, bytes, cold_start);
        self.metrics.record(serde_json::json!({
            "ts": crate::obs::now_ms(),
            "op": "speak",
            "requestId": self.request_id,
            "ok": true,
            "audioId": self.audio_id,
            "voicePackId": self.voice_pack_id,
            "chars": self.text.chars().count(),
            "coldStart": cold_start,
            "queueMs": queue_ms,
            "synthMs": synth_ms,
            "totalMs": total_ms,
            "audioBytes": bytes,
            "durationMs": output.duration_ms,
            "sampleRate": output.sample_rate,
        }));

        self.bus.publish(Event::Speech {
            request_id: self.request_id.clone(),
            audio_id: self.audio_id.clone(),
            text: self.text.clone(),
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
