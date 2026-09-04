//! Startup configuration. Every path and deadline is explicit: the runtime
//! never guesses where Python, models or voice packs live. A missing value is
//! a startup error naming the exact flag, not a silent fallback.

use std::io;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Where the TTS worker process comes from.
///
/// `Managed` is the product path: the runtime starts the worker on demand and
/// a job object guarantees it dies with the runtime. `External` exists because
/// during development (and on platforms without job objects) you want to run
/// the worker yourself and attach; it is also what the integration tests use.
#[derive(Clone, Debug)]
pub enum WorkerSource {
    Managed(WorkerSpec),
    External { base_url: String },
}

#[derive(Clone, Debug)]
pub struct WorkerSpec {
    /// Interpreter that owns the engine's virtualenv.
    pub python: PathBuf,
    /// Worker entry script.
    pub script: PathBuf,
    /// Engine source/model root handed to the worker as `--root`.
    pub root: Option<PathBuf>,
    /// Extra environment for the child process (e.g. `HF_HOME`).
    pub env: Vec<(String, String)>,
    /// Where the engine puts the model and the codec.
    pub placement: EnginePlacement,
}

/// Device and precision for the engine, handed to the worker as CLI flags.
///
/// The defaults are the shipped behaviour byte for byte. The knob exists because
/// the cheap latency experiments — codec on CPU, lower precision — have to be
/// measured against the similarity harness before any of them becomes policy, and
/// a measurement needs a flag, not a config surface every frontend then has to
/// show.
#[derive(Clone, Debug)]
pub struct EnginePlacement {
    pub model_device: String,
    pub codec_device: String,
    pub model_precision: String,
    pub codec_precision: String,
}

impl Default for EnginePlacement {
    fn default() -> Self {
        Self {
            model_device: "cuda".into(),
            codec_device: "cuda".into(),
            model_precision: "bf16".into(),
            codec_precision: "bf16".into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Config {
    pub bind: SocketAddr,
    /// Owns token, voice packs, spool, logs and metrics. Created if absent.
    pub data_dir: PathBuf,
    pub token: String,
    pub worker: WorkerSource,
    /// Stop the worker process after this much idle time. `None` keeps it hot.
    pub idle_stop: Option<Duration>,
    pub spool_ttl: Duration,
    pub spool_max_bytes: u64,
    /// How long to wait for a freshly spawned worker to answer `/health`.
    pub worker_ready_timeout: Duration,
    /// Upper bound on one synthesis call, including cold model load.
    pub synth_timeout: Duration,
}

impl Config {
    pub fn spool_dir(&self) -> PathBuf {
        self.data_dir.join("spool")
    }

    pub fn log_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    pub fn metrics_file(&self) -> PathBuf {
        self.data_dir.join("metrics.jsonl")
    }

    /// The app's one settings file. Owned and written by the tray (it is the settings
    /// UI); read here for the voice pack registry, which lives in its `voicePacks`
    /// section. Hand-edited, so it is JSONC - see `crate::jsonc`.
    pub fn config_file(&self) -> PathBuf {
        self.data_dir.join("config.json")
    }

    pub fn prepare_dirs(&self) -> io::Result<()> {
        std::fs::create_dir_all(&self.data_dir)?;
        std::fs::create_dir_all(self.spool_dir())?;
        std::fs::create_dir_all(self.log_dir())?;
        Ok(())
    }
}

/// Read `token.txt`, minting one on first run. The token is the only auth
/// boundary, so it is generated here rather than by whichever frontend
/// happened to start first (v1 minted it in three places).
pub fn ensure_token(data_dir: &Path) -> io::Result<String> {
    let file = data_dir.join("token.txt");
    if let Ok(existing) = std::fs::read_to_string(&file) {
        let trimmed = existing.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }
    std::fs::create_dir_all(data_dir)?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    std::fs::write(&file, &token)?;
    Ok(token)
}

/// Constant-time token comparison; length leaks, contents do not.
pub fn token_matches(expected: &str, presented: &str) -> bool {
    let a = expected.as_bytes();
    let b = presented.as_bytes();
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for i in 0..a.len() {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}
