//! Worker process ownership.
//!
//! v1 put this in the tray GUI, which is why the product could not run without
//! a Windows GUI and why "stop" needed a PID ledger with identity matching. Here
//! the runtime owns the process and a job object owns the tree: when the job
//! handle closes, the kernel terminates every process inside it, so the runtime
//! can neither leak a worker nor kill somebody else's Python.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex as SyncMutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio::sync::Mutex;

use crate::config::{WorkerSource, WorkerSpec};
use crate::engine::{EngineHealth, IrodoriEngine, TtsEngine};
use crate::obs::{Bus, Event};

#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("worker is managed elsewhere and not reachable at {base_url}")]
    ExternalUnreachable { base_url: String },
    #[error("cannot start worker: {0}")]
    Spawn(String),
    #[error("worker did not become ready after {elapsed_ms} ms{detail}")]
    NotReady { elapsed_ms: u64, detail: String },
    /// A worker that is up, answered `/health`, and then refuses to load or release
    /// its model is neither a spawn failure nor a readiness timeout.
    #[error("worker rejected {route}: {reason}")]
    Control {
        route: &'static str,
        reason: String,
    },
}

struct Running {
    child: tokio::process::Child,
    port: u16,
    base_url: String,
    started: Instant,
    #[cfg(windows)]
    _job: job::Job,
}

pub struct Worker {
    source: WorkerSource,
    log_dir: PathBuf,
    bus: std::sync::Arc<Bus>,
    ready_timeout: Duration,
    engine: IrodoriEngine,
    running: Mutex<Option<Running>>,
    last_used: SyncMutex<Instant>,
    /// Loads in flight. A load is the one activity that takes tens of seconds while
    /// touching nothing else, so it has to make the worker look busy (see `load_model`).
    loading: AtomicUsize,
}

#[derive(Clone, Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkerStatus {
    pub managed: bool,
    pub running: bool,
    pub ready: bool,
    pub model_loaded: bool,
    pub port: Option<u16>,
    pub uptime_ms: Option<u64>,
    pub idle_ms: u64,
    /// Configured resources that are not on disk. Empty means the engine can at
    /// least be launched. Reported before anyone tries to speak, so a missing
    /// interpreter or engine tree is visible state rather than a failure five
    /// minutes later.
    pub missing: Vec<String>,
}

impl Worker {
    pub fn new(
        source: WorkerSource,
        log_dir: PathBuf,
        bus: std::sync::Arc<Bus>,
        ready_timeout: Duration,
    ) -> Self {
        Self {
            source,
            log_dir,
            bus,
            ready_timeout,
            engine: IrodoriEngine::new(),
            running: Mutex::new(None),
            last_used: SyncMutex::new(Instant::now()),
            loading: AtomicUsize::new(0),
        }
    }

    pub fn engine(&self) -> &IrodoriEngine {
        &self.engine
    }

    pub fn managed(&self) -> bool {
        matches!(self.source, WorkerSource::Managed(_))
    }

    /// Configured resources that are not on disk. Checks only what the runtime
    /// itself configured — model completeness belongs to the engine, which
    /// reports it as `model_load_failed`.
    pub fn preflight(&self) -> Vec<String> {
        let WorkerSource::Managed(spec) = &self.source else {
            return Vec::new();
        };
        let mut missing = Vec::new();
        if !spec.python.exists() {
            missing.push(format!("interpreter: {}", spec.python.display()));
        }
        if !spec.script.exists() {
            missing.push(format!("worker script: {}", spec.script.display()));
        }
        if let Some(root) = &spec.root {
            if !root.is_dir() {
                missing.push(format!("engine root: {}", root.display()));
            }
        }
        for (key, value) in &spec.env {
            if key == "HF_HOME" && !std::path::Path::new(value).is_dir() {
                missing.push(format!("model cache: {value}"));
            }
        }
        missing
    }

    pub fn touch(&self) {
        if let Ok(mut last) = self.last_used.lock() {
            *last = Instant::now();
        }
    }

    /// Time since the worker was last used. ZERO while a model load is in flight: the
    /// reaper's whole input is this number, and a loading worker is the opposite of an
    /// idle one.
    pub fn idle_for(&self) -> Duration {
        if self.loading.load(Ordering::SeqCst) > 0 {
            return Duration::ZERO;
        }
        self.last_used
            .lock()
            .map(|last| last.elapsed())
            .unwrap_or_default()
    }

    /// Returns a base URL for a worker that is up and answering `/health`,
    /// starting one first if this runtime owns it.
    pub async fn ensure(&self, reason: &str) -> Result<String, WorkerError> {
        let spec = match &self.source {
            WorkerSource::External { base_url } => {
                if self.engine.health(base_url).await.ready {
                    return Ok(base_url.clone());
                }
                return Err(WorkerError::ExternalUnreachable {
                    base_url: base_url.clone(),
                });
            }
            WorkerSource::Managed(spec) => spec.clone(),
        };

        let mut guard = self.running.lock().await;
        if let Some(running) = guard.as_mut() {
            // Read the pid before try_wait: reaping the child clears it, and a worker
            // that died on its own is exactly when the log needs it.
            let pid = running.child.id().unwrap_or(0);
            let port = running.port;
            let uptime_ms = running.started.elapsed().as_millis();
            let exit = match running.child.try_wait() {
                Ok(None) => None,
                Ok(Some(status)) => Some(status.to_string()),
                Err(err) => Some(format!("unknown ({err})")),
            };
            match exit {
                None => return Ok(running.base_url.clone()),
                Some(exit) => {
                    // Exited on its own; fall through and start a fresh one.
                    *guard = None;
                    let reason = format!("worker on port {port} exited unexpectedly");
                    self.append_worker_log(&format!(
                        "[supervisor] stage=worker.stopped reason={} pid={pid} port={port} \
                         uptime_ms={uptime_ms} exit={}",
                        quoted(&reason),
                        quoted(&exit)
                    ));
                    self.bus.publish(Event::WorkerStopped { reason });
                }
            }
        }

        self.bus.publish(Event::WorkerStarting {
            reason: reason.to_string(),
        });
        self.append_worker_log(&format!(
            "[supervisor] stage=worker.starting reason={} python={} root={}",
            quoted(reason),
            quoted(spec.python.display()),
            quoted(
                spec.root
                    .as_ref()
                    .map(|root| root.display().to_string())
                    .unwrap_or_default()
            )
        ));
        let running = self.spawn(&spec).await?;
        let base_url = running.base_url.clone();
        let port = running.port;
        let pid = running.child.id().unwrap_or(0);
        let spawned_at = running.started;
        *guard = Some(running);
        drop(guard);

        let health = self.await_ready(&base_url).await?;
        self.bus.publish(Event::WorkerReady {
            port: Some(port),
            model_loaded: health.model_loaded,
        });
        self.append_worker_log(&format!(
            "[supervisor] stage=worker.ready pid={pid} port={port} model_loaded={} ms={}",
            health.model_loaded,
            spawned_at.elapsed().as_millis()
        ));
        Ok(base_url)
    }

    /// Loads the model, starting the worker first if it is not up. Warming used to
    /// stop at "the process answers /health", which is true the moment uvicorn binds
    /// and says nothing about the model, so the first utterance paid the whole load.
    ///
    /// The load counts as ACTIVITY for the whole time it runs. A cold load is 14-25 s
    /// (`tts-worker.out.log`), and nothing else touches the worker while it happens, so
    /// without this the idle reaper reads a loading worker as an unused one: measured, a
    /// `--idle-stop-secs 20` runtime killed the process mid-load and `/api/warm` came back
    /// `model_load_failed: engine unreachable`. The default 900 s window hides it; a short
    /// one makes warming impossible.
    pub async fn load_model(&self) -> Result<(), WorkerError> {
        let base_url = self.ensure("model load").await?;
        self.loading.fetch_add(1, Ordering::SeqCst);
        let result = self.engine.load_model(&base_url).await;
        self.loading.fetch_sub(1, Ordering::SeqCst);
        self.touch();
        result.map_err(|err| WorkerError::Control {
            route: "/load",
            reason: err.to_string(),
        })
    }

    /// Hands the VRAM back without killing the process, so the next utterance repays
    /// the model load but not the multi-second torch import. A worker that is not
    /// running holds no VRAM, which makes this a no-op rather than a start.
    ///
    /// `reason` is written to the worker log for the same reason `stop` writes one: with
    /// no frontend subscribed, the event bus reaches nobody, and "why is the model not
    /// loaded any more" has to be answerable from disk afterwards.
    pub async fn release_vram(&self, reason: &str) -> Result<(), WorkerError> {
        let Some(base_url) = self.running_base_url().await else {
            return Ok(());
        };
        self.append_worker_log(&format!("[supervisor] stage=vram.released reason={reason:?}"));
        self.engine
            .unload_model(&base_url)
            .await
            .map_err(|err| WorkerError::Control {
                route: "/unload",
                reason: err.to_string(),
            })
    }

    /// The URL of a worker that is up right now, without starting one.
    async fn running_base_url(&self) -> Option<String> {
        match &self.source {
            WorkerSource::External { base_url } => Some(base_url.clone()),
            WorkerSource::Managed(_) => {
                let mut guard = self.running.lock().await;
                let running = guard.as_mut()?;
                if matches!(running.child.try_wait(), Ok(None)) {
                    Some(running.base_url.clone())
                } else {
                    None
                }
            }
        }
    }

    async fn spawn(&self, spec: &WorkerSpec) -> Result<Running, WorkerError> {
        if !spec.python.exists() {
            return Err(WorkerError::Spawn(format!(
                "interpreter not found: {}",
                spec.python.display()
            )));
        }
        if !spec.script.exists() {
            return Err(WorkerError::Spawn(format!(
                "worker script not found: {}",
                spec.script.display()
            )));
        }
        std::fs::create_dir_all(&self.log_dir).map_err(|e| WorkerError::Spawn(e.to_string()))?;

        let port = free_port().map_err(|e| WorkerError::Spawn(e.to_string()))?;
        let out = open_log(&self.log_dir.join("tts-worker.out.log"))?;
        let err = open_log(&self.log_dir.join("tts-worker.err.log"))?;

        let mut cmd = tokio::process::Command::new(&spec.python);
        cmd.arg(&spec.script)
            .arg("--port")
            .arg(port.to_string())
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::from(out))
            .stderr(std::process::Stdio::from(err))
            .kill_on_drop(true);
        if let Some(root) = &spec.root {
            cmd.arg("--root").arg(root);
        }
        cmd.arg("--model-device")
            .arg(&spec.placement.model_device)
            .arg("--codec-device")
            .arg(&spec.placement.codec_device)
            .arg("--model-precision")
            .arg(&spec.placement.model_precision)
            .arg("--codec-precision")
            .arg(&spec.placement.codec_precision);
        for (key, value) in &spec.env {
            cmd.env(key, value);
        }
        cmd.env("PYTHONUNBUFFERED", "1");
        #[cfg(windows)]
        {
            // tokio's Command exposes this directly on Windows; no window for a
            // background engine process.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        // The worker cannot time the interpreter's own startup from inside, so it gets
        // the spawn instant as the anchor for its boot.interpreter line; both sides
        // read the same OS wall clock.
        if let Ok(epoch) = SystemTime::now().duration_since(UNIX_EPOCH) {
            cmd.arg("--spawn-epoch-ms")
                .arg(epoch.as_millis().to_string());
        }

        let child = cmd.spawn().map_err(|e| WorkerError::Spawn(e.to_string()))?;

        #[cfg(windows)]
        let _job = {
            let handle = child
                .raw_handle()
                .ok_or_else(|| WorkerError::Spawn("child handle unavailable".into()))?;
            let job = job::Job::new().map_err(|e| WorkerError::Spawn(e.to_string()))?;
            job.assign(handle)
                .map_err(|e| WorkerError::Spawn(e.to_string()))?;
            job
        };

        Ok(Running {
            child,
            port,
            base_url: format!("http://127.0.0.1:{port}"),
            started: Instant::now(),
            #[cfg(windows)]
            _job,
        })
    }

    async fn await_ready(&self, base_url: &str) -> Result<EngineHealth, WorkerError> {
        let started = Instant::now();
        let deadline = started + self.ready_timeout;
        loop {
            let health = self.engine.health(base_url).await;
            if health.ready {
                return Ok(health);
            }
            // A worker that died during import will never answer, and the caller is
            // holding the single GPU permit while it waits: without this check a
            // broken venv or an ImportError costs the whole ready_timeout (90 s by
            // default) with nothing on /api/events. Taken and released here, because
            // `stop` below wants the same guard.
            let exited = {
                let mut guard = self.running.lock().await;
                match guard.as_mut() {
                    Some(running) => !matches!(running.child.try_wait(), Ok(None)),
                    // Stopped from elsewhere while we waited; nobody is left to answer.
                    None => true,
                }
            };
            if exited || Instant::now() >= deadline {
                let mut detail = String::new();
                if exited {
                    // A timeout means "still loading"; an exit means the reason is
                    // already in the log the tail below carries.
                    detail.push_str("; the worker exited before answering /health");
                }
                if let Some(stderr) = tail(&self.log_dir.join("tts-worker.err.log"), 400) {
                    detail.push_str("; last stderr: ");
                    detail.push_str(&stderr);
                }
                // The reason reaches every frontend as WorkerStopped on the bus.
                let reason = if exited {
                    "exited before becoming ready"
                } else {
                    "failed to become ready"
                };
                self.stop(reason).await;
                return Err(WorkerError::NotReady {
                    // Elapsed, not the configured timeout: an import that died in two
                    // seconds must not report a 90-second wait.
                    elapsed_ms: started.elapsed().as_millis() as u64,
                    detail,
                });
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    /// Terminates the worker tree. Dropping the job handle is what actually
    /// guarantees the kill; `child.kill()` only covers the direct child.
    pub async fn stop(&self, reason: &str) {
        let mut guard = self.running.lock().await;
        if let Some(mut running) = guard.take() {
            let pid = running.child.id().unwrap_or(0);
            let port = running.port;
            let uptime_ms = running.started.elapsed().as_millis();
            let _ = running.child.start_kill();
            let _ = running.child.wait().await;
            self.bus.publish(Event::WorkerStopped {
                reason: reason.to_string(),
            });
            self.append_worker_log(&format!(
                "[supervisor] stage=worker.stopped reason={} pid={pid} port={port} \
                 uptime_ms={uptime_ms}",
                quoted(reason)
            ));
        }
    }

    pub async fn status(&self) -> WorkerStatus {
        let idle_ms = self.idle_for().as_millis() as u64;
        match &self.source {
            WorkerSource::External { base_url } => {
                let health = self.engine.health(base_url).await;
                WorkerStatus {
                    managed: false,
                    running: health.ready,
                    ready: health.ready,
                    model_loaded: health.model_loaded,
                    port: None,
                    uptime_ms: None,
                    idle_ms,
                    missing: Vec::new(),
                }
            }
            WorkerSource::Managed(_) => {
                let mut guard = self.running.lock().await;
                let Some(running) = guard.as_mut() else {
                    return WorkerStatus {
                        managed: true,
                        running: false,
                        ready: false,
                        model_loaded: false,
                        port: None,
                        uptime_ms: None,
                        idle_ms,
                        missing: self.preflight(),
                    };
                };
                let alive = matches!(running.child.try_wait(), Ok(None));
                let base_url = running.base_url.clone();
                let port = running.port;
                let uptime = running.started.elapsed().as_millis() as u64;
                drop(guard);
                let health = if alive {
                    self.engine.health(&base_url).await
                } else {
                    EngineHealth::default()
                };
                WorkerStatus {
                    managed: true,
                    running: alive,
                    ready: health.ready,
                    model_loaded: health.model_loaded,
                    port: Some(port),
                    uptime_ms: Some(uptime),
                    idle_ms,
                    missing: self.preflight(),
                }
            }
        }
    }

    pub async fn is_running(&self) -> bool {
        match &self.source {
            WorkerSource::External { .. } => true,
            WorkerSource::Managed(_) => {
                let mut guard = self.running.lock().await;
                match guard.as_mut() {
                    Some(running) => matches!(running.child.try_wait(), Ok(None)),
                    None => false,
                }
            }
        }
    }

    /// The worker lifecycle used to reach the in-memory bus only, so with nothing
    /// subscribed — the normal case, since the tray connects only while its window is
    /// open — no record survived that the engine ever started or why it stopped. This
    /// is the same file the worker prints its own `[worker] stage=` lines to, so one
    /// tail shows the whole cold path. Opened per line: three lines per worker
    /// lifetime do not justify holding a second handle on the child's stdout file.
    fn append_worker_log(&self, line: &str) {
        use std::io::Write;

        if let Ok(mut file) = open_log(&self.log_dir.join("tts-worker.out.log")) {
            let _ = writeln!(file, "{line}");
        }
    }
}

/// Reserve-and-release: bind port 0, learn the number, hand it to the worker.
/// The loopback race window is negligible and the alternative — a hardcoded
/// port — fails silently when something else already holds it.
fn free_port() -> std::io::Result<u16> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

fn open_log(path: &Path) -> Result<std::fs::File, WorkerError> {
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| WorkerError::Spawn(format!("cannot open {}: {e}", path.display())))
}

/// Last `max` bytes of a log file, for turning "did not become ready" into an
/// actionable error instead of a shrug.
fn tail(path: &Path, max: usize) -> Option<String> {
    let data = std::fs::read(path).ok()?;
    let start = data.len().saturating_sub(max);
    let text = String::from_utf8_lossy(&data[start..]).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

/// Log lines are `key=value` pairs and the values here are sentences — an OS error, a
/// stop reason — so they are quoted, and an embedded quote would split the pair for
/// whoever parses the file.
fn quoted(value: impl std::fmt::Display) -> String {
    format!("\"{}\"", value.to_string().replace('"', "'"))
}

#[cfg(windows)]
mod job {
    use std::io;

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    /// A job object with KILL_ON_JOB_CLOSE. Every process assigned to it dies
    /// when this handle closes — including on runtime crash, which is exactly
    /// the guarantee a PID ledger cannot make.
    pub struct Job(HANDLE);

    impl Job {
        pub fn new() -> io::Result<Self> {
            unsafe {
                let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if handle.is_null() {
                    return Err(io::Error::last_os_error());
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    handle,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const std::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    let err = io::Error::last_os_error();
                    CloseHandle(handle);
                    return Err(err);
                }
                Ok(Job(handle))
            }
        }

        pub fn assign(&self, process: std::os::windows::io::RawHandle) -> io::Result<()> {
            unsafe {
                if AssignProcessToJobObject(self.0, process as HANDLE) == 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            }
        }
    }

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    // The handle is owned exclusively by this struct and only used through
    // thread-safe Win32 calls.
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}
}
