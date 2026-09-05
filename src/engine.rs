//! The one seam that has earned its keep: the TTS engine.
//!
//! Three engines have already come and gone in this project (IndexTTS,
//! Qwen3-TTS, Irodori), so the boundary is a trait. The boundary is also a
//! process boundary: an engine is a worker speaking four HTTP routes, which
//! means "adding an engine" needs no dynamic loading, no ABI and no plugin
//! registry — just another worker that answers `/health`, `/load`, `/unload`
//! and `/synthesize`.

use std::future::Future;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;

#[derive(Clone, Debug)]
pub struct PackTarget {
    pub kind: &'static str,
    pub path: String,
}

pub struct SynthRequest<'a> {
    pub text: &'a str,
    pub pack: Option<PackTarget>,
    pub seed: Option<u64>,
    pub num_steps: u32,
    /// The engine's caption channel: a style annotation conditioned by a head separate
    /// from the text one (`use_caption_condition`). `None` and `""` are the same thing to
    /// the engine - it zeroes the caption mask either way - so only a non-empty one is
    /// ever sent, which is what keeps a request without expression byte-identical to
    /// what this runtime sent before the channel existed.
    pub caption: Option<&'a str>,
    /// Guidance strength for that channel. `None` leaves the engine's own default (3.0).
    pub cfg_scale_caption: Option<f64>,
    /// Where the engine must write its WAV. The runtime owns this path and
    /// never sees the samples themselves.
    pub out_path: &'a Path,
}

#[derive(Clone, Copy, Debug)]
pub struct SynthOutput {
    pub sample_rate: u32,
    pub duration_ms: u64,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct EngineHealth {
    pub ready: bool,
    pub model_loaded: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("engine unreachable: {0}")]
    Unreachable(String),
    #[error("engine returned HTTP {status}: {body}")]
    Status { status: u16, body: String },
    #[error("engine response malformed: {0}")]
    Malformed(String),
    // Both carry the worker's own text: the reason exists only inside the engine
    // process, and the two stages need different error codes.
    #[error("engine could not load its model: {0}")]
    ModelLoad(String),
    #[error("engine could not synthesize this utterance: {0}")]
    Synthesis(String),
    #[error("synthesis exceeded its {0} ms deadline")]
    Deadline(u64),
}

/// Implementors talk to one synthesis backend. Deliberately not object-safe:
/// there is exactly one engine in the binary, so dynamic dispatch would buy
/// nothing and cost an allocation per call. The futures are spelled out rather
/// than written `async fn` so that `Send` is part of the contract — the
/// orchestrator drives them inside a spawned task.
pub trait TtsEngine {
    fn health(&self, base_url: &str) -> impl Future<Output = EngineHealth> + Send;

    /// Load the model now, and do not return until it is loaded. `/health` reports
    /// `ready` from the moment the worker's port answers, which says nothing about
    /// the model: without this route the first utterance pays the whole load.
    fn load_model(&self, base_url: &str) -> impl Future<Output = Result<(), EngineError>> + Send;

    /// Drop the model and hand the VRAM back. The worker keeps serving, so the next
    /// load pays for weights again but not for the multi-second torch import.
    fn unload_model(&self, base_url: &str) -> impl Future<Output = Result<(), EngineError>> + Send;

    fn synthesize(
        &self,
        base_url: &str,
        req: SynthRequest<'_>,
        deadline: Duration,
    ) -> impl Future<Output = Result<SynthOutput, EngineError>> + Send;
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HealthWire {
    #[serde(default)]
    ready: bool,
    #[serde(default)]
    model_loaded: bool,
}

/// Both control routes answer the same two fields, and both answer 200 with an
/// `error` field instead of a status code: the reason exists only inside the worker
/// process and a status code cannot carry it.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ModelWire {
    #[serde(default)]
    model_loaded: bool,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SynthWire {
    #[serde(default)]
    sample_rate: u32,
    #[serde(default)]
    duration_ms: u64,
    #[serde(default)]
    error: Option<String>,
}

/// Irodori TTS v4.1-Small worker (`worker/irodori/worker.py`).
pub struct IrodoriEngine {
    http: reqwest::Client,
}

/// A cold load is minutes on a cold disk and there is no useful shorter bound: the
/// caller asked for the model, and giving up here only means the first utterance
/// pays the same load again.
const LOAD_TIMEOUT: Duration = Duration::from_secs(600);
/// Dropping references, a gc pass and `empty_cache()`. Seconds, not minutes.
const UNLOAD_TIMEOUT: Duration = Duration::from_secs(30);

impl IrodoriEngine {
    pub fn new() -> Self {
        Self {
            http: reqwest::Client::new(),
        }
    }

    /// POST a body-less control route and decode the worker's reply.
    async fn control(
        &self,
        base_url: &str,
        route: &'static str,
        timeout: Duration,
    ) -> Result<ModelWire, EngineError> {
        let response = self
            .http
            .post(format!("{base_url}{route}"))
            .timeout(timeout)
            .send()
            .await
            .map_err(|err| {
                if err.is_timeout() {
                    EngineError::Deadline(timeout.as_millis() as u64)
                } else {
                    EngineError::Unreachable(err.to_string())
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            return Err(EngineError::Status {
                status: status.as_u16(),
                body: response.text().await.unwrap_or_default(),
            });
        }

        let mut wire: ModelWire = response
            .json()
            .await
            .map_err(|err| EngineError::Malformed(err.to_string()))?;
        if let Some(reason) = wire.error.take() {
            return Err(engine_failure(reason));
        }
        Ok(wire)
    }
}

impl Default for IrodoriEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl TtsEngine for IrodoriEngine {
    async fn health(&self, base_url: &str) -> EngineHealth {
        let response = self
            .http
            .get(format!("{base_url}/health"))
            .timeout(Duration::from_secs(2))
            .send()
            .await;
        match response {
            Ok(resp) if resp.status().is_success() => resp
                .json::<HealthWire>()
                .await
                .map(|w| EngineHealth {
                    ready: w.ready,
                    model_loaded: w.model_loaded,
                })
                .unwrap_or_default(),
            _ => EngineHealth::default(),
        }
    }

    async fn load_model(&self, base_url: &str) -> Result<(), EngineError> {
        let wire = self.control(base_url, "/load", LOAD_TIMEOUT).await?;
        if !wire.model_loaded {
            return Err(EngineError::ModelLoad(
                "worker answered /load without loading the model".into(),
            ));
        }
        Ok(())
    }

    async fn unload_model(&self, base_url: &str) -> Result<(), EngineError> {
        let wire = self.control(base_url, "/unload", UNLOAD_TIMEOUT).await?;
        if wire.model_loaded {
            return Err(EngineError::Malformed(
                "worker answered /unload with the model still loaded".into(),
            ));
        }
        Ok(())
    }

    async fn synthesize(
        &self,
        base_url: &str,
        req: SynthRequest<'_>,
        deadline: Duration,
    ) -> Result<SynthOutput, EngineError> {
        let mut body = serde_json::json!({
            "text": req.text,
            "numSteps": req.num_steps,
            "outPath": req.out_path.to_string_lossy(),
        });
        if let Some(seed) = req.seed {
            body["seed"] = serde_json::json!(seed);
        }
        if let Some(pack) = &req.pack {
            body["voicePack"] = serde_json::json!({ "kind": pack.kind, "path": pack.path });
        }
        // Both omitted unless asked for: an absent key is the engine's own default, and
        // sending `caption: null` would be a wire change on every ordinary utterance.
        if let Some(caption) = req.caption.filter(|caption| !caption.is_empty()) {
            body["caption"] = serde_json::json!(caption);
        }
        if let Some(scale) = req.cfg_scale_caption {
            body["cfgScaleCaption"] = serde_json::json!(scale);
        }

        let response = self
            .http
            .post(format!("{base_url}/synthesize"))
            .json(&body)
            .timeout(deadline)
            .send()
            .await
            .map_err(|err| {
                if err.is_timeout() {
                    EngineError::Deadline(deadline.as_millis() as u64)
                } else {
                    EngineError::Unreachable(err.to_string())
                }
            })?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(EngineError::Status {
                status: status.as_u16(),
                body,
            });
        }

        let wire: SynthWire = response
            .json()
            .await
            .map_err(|err| EngineError::Malformed(err.to_string()))?;
        if let Some(reason) = wire.error {
            return Err(engine_failure(reason));
        }
        if wire.sample_rate == 0 {
            return Err(EngineError::Malformed("sampleRate missing".into()));
        }
        Ok(SynthOutput {
            sample_rate: wire.sample_rate,
            duration_ms: wire.duration_ms,
        })
    }
}

/// The worker reports why it failed in the reply's single `error` field, tagged with
/// the stage that failed (`worker/irodori/worker.py`): a model that never loaded and
/// an utterance the engine refused need different codes and different advice. An
/// untagged reason comes from a worker this runtime did not ship (`--tts-url`), so it
/// is passed through rather than guessed at.
fn engine_failure(reason: String) -> EngineError {
    if let Some(detail) = reason.strip_prefix("model load failed: ") {
        return EngineError::ModelLoad(detail.to_string());
    }
    if let Some(detail) = reason.strip_prefix("synthesis failed: ") {
        return EngineError::Synthesis(detail.to_string());
    }
    EngineError::Synthesis(reason)
}
