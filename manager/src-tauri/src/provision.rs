//! `scripts/bootstrap.ps1 -Json`, streamed to the frontend one line at a time.
//!
//! With `-Json` the script writes exactly one JSON object per line to stdout and
//! nothing else, so this reads lines rather than accumulating output: a 4.4 GiB
//! download reports progress while it happens or it does not report at all.
//! Anything on stdout that is not JSON is forwarded as a `log` event instead of
//! being dropped or treated as fatal — a stray `Write-Host` should look like
//! noise in the panel, not like a crash.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::contract::{ProvisionOpts, EVENT_BOOTSTRAP};
use crate::host::{hidden, now_ms, Host};
use crate::jobobj::{kill_tree, Job};
use crate::layout;

/// Where bootstrap's own stderr goes. Truncated per run: the point of this file
/// is to name the cause of *this* failure, and a stale tail would name the wrong
/// one.
const STDERR_LOG: &str = "bootstrap.err.log";

#[derive(Default)]
pub struct ProvisionRun {
    /// Dropping this kills PowerShell *and* the git clone, uv, pip and
    /// huggingface downloads it started. Killing the PowerShell PID alone leaves
    /// all of those running with nobody reading their output.
    job: Option<Job>,
    pid: Option<u32>,
    /// Claimed for the whole run, so a second click cannot start a second
    /// bootstrap into the same directories.
    busy: bool,
    /// A cancelled run exits non-zero. That is not a failure to report as one.
    cancelled: bool,
}

#[tauri::command]
pub async fn provision(app: AppHandle, opts: ProvisionOpts) -> Result<(), String> {
    let script = {
        let host = app.state::<Host>();
        let script = layout::bootstrap_script(&host.root).ok_or_else(|| {
            format!(
                "scripts\\bootstrap.ps1 not found under {} — this does not look like a voice-core install",
                host.root.display()
            )
        })?;
        let mut run = lock(&host);
        if run.busy {
            return Err("a provision run is already in progress".to_string());
        }
        run.busy = true;
        run.cancelled = false;
        script
    };

    let args = {
        let host = app.state::<Host>();
        build_args(&host, &opts)
    };

    // A blocking thread for the whole run: the loop that reads stdout is a
    // blocking read by nature, and giving it a thread is simpler and more
    // obviously correct than driving a pipe through the async runtime.
    let worker = {
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || run_bootstrap(&app, script, args))
    };
    let outcome = worker.await.map_err(|err| err.to_string());

    {
        let host = app.state::<Host>();
        let mut run = lock(&host);
        run.busy = false;
        run.pid = None;
        run.job = None;
        // A run can put a different interpreter on disk, which is the one thing
        // that invalidates the cached torch probe.
        *host.probe.lock().unwrap_or_else(|err| err.into_inner()) = None;
    }

    outcome?
}

/// Kill the whole process tree.
///
/// Two mechanisms, deliberately: `taskkill /T /F` first, because it walks parent
/// links and catches anything spawned in the window between `spawn` and the job
/// assignment, and then the job handle, because KILL_ON_JOB_CLOSE is the only
/// guarantee that does not depend on a PID still meaning what it meant.
#[tauri::command]
pub async fn cancel_provision(app: AppHandle) {
    let host = app.state::<Host>();
    let pid = {
        let mut run = lock(&host);
        if run.pid.is_none() {
            return;
        }
        run.cancelled = true;
        run.pid
    };
    if let Some(pid) = pid {
        kill_tree(pid);
    }
    lock(&host).job = None;
    host.log("provision cancelled");
}

fn run_bootstrap(app: &AppHandle, script: std::path::PathBuf, args: Vec<String>) -> Result<(), String> {
    let host = app.state::<Host>();

    let mut command = Command::new("powershell.exe");
    command
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(&script)
        .args(&args)
        .current_dir(&host.root)
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    match host.fresh_child_log(STDERR_LOG) {
        Ok(file) => {
            command.stderr(Stdio::from(file));
        }
        Err(err) => {
            host.log(&format!("{STDERR_LOG} unavailable: {err}"));
            command.stderr(Stdio::null());
        }
    }
    hidden(&mut command);

    let mut child = command
        .spawn()
        .map_err(|err| format!("powershell.exe: {err}"))?;

    {
        let mut run = lock(&host);
        match Job::new() {
            Ok(job) => {
                if let Err(err) = job.assign(&child) {
                    host.log(&format!("could not assign bootstrap to job object: {err}"));
                }
                run.job = Some(job);
            }
            Err(err) => host.log(&format!("job object unavailable: {err}")),
        }
        run.pid = Some(child.id());
    }
    host.log(&format!("bootstrap started: {}", args.join(" ")));

    // Present even when the script emits nothing, so a non-JSON line that
    // arrives before the first `start` still carries a stage the frontend can
    // key on. `preflight` is the first stage the script runs.
    let mut stage = "preflight".to_string();
    if let Some(stdout) = child.stdout.take() {
        for line in BufReader::new(stdout).lines() {
            // A read error here is the pipe dying with the process, which is what
            // cancellation looks like from this side.
            let Ok(line) = line else { break };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<Value>(line) {
                Ok(value) if value.is_object() => {
                    if let Some(current) = value.get("stage").and_then(Value::as_str) {
                        stage = current.to_string();
                    }
                    let _ = app.emit(EVENT_BOOTSTRAP, value);
                }
                _ => {
                    let _ = app.emit(EVENT_BOOTSTRAP, log_event(&stage, line));
                }
            }
        }
    }

    let status = child
        .wait()
        .map_err(|err| format!("waiting for bootstrap.ps1: {err}"))?;

    if lock(&host).cancelled {
        return Ok(());
    }
    if status.success() {
        return Ok(());
    }

    // A failed *stage* still exits 0 and reports itself through the event stream.
    // A non-zero exit means the script rejected its own arguments, which is a bug
    // in the argv built above, not something the user can act on — so it is
    // reported as a rejected call rather than as a fabricated `fail` event on a
    // stage that never ran.
    let code = status.code().unwrap_or(-1);
    let tail = stderr_tail(&host);
    Err(if tail.is_empty() {
        format!("bootstrap.ps1 exited with code {code}")
    } else {
        format!("bootstrap.ps1 exited with code {code}: {tail}")
    })
}

fn build_args(host: &Host, opts: &ProvisionOpts) -> Vec<String> {
    // -InstallRoot is always explicit. The script would otherwise derive the root
    // from its own location, and this app already knows the answer from its own
    // executable's location; passing it keeps one derivation authoritative.
    let mut args = vec![
        "-Json".to_string(),
        "-InstallRoot".to_string(),
        host.root.display().to_string(),
    ];
    for (flag, value) in [
        ("-EngineRoot", &opts.engine_root),
        ("-HfHome", &opts.hf_home),
        ("-VoicePacks", &opts.voice_packs),
        ("-Only", &opts.only),
    ] {
        // An empty text field in the panel must not become an empty argument that
        // overrides a default with nothing.
        if let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            args.push(flag.to_string());
            args.push(value.to_string());
        }
    }
    if opts.check_only {
        args.push("-CheckOnly".to_string());
    }
    args
}

/// All seven keys, always, so the frontend never has to test for their presence.
fn log_event(stage: &str, message: &str) -> Value {
    json!({
        "ts": now_ms(),
        "stage": stage,
        "event": "log",
        "message": message,
        "done": Value::Null,
        "total": Value::Null,
        "remedy": Value::Null,
    })
}

fn stderr_tail(host: &Host) -> String {
    const MAX: usize = 2048;
    let path = layout::logs_dir(&host.data_dir).join(STDERR_LOG);
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    let start = bytes.len().saturating_sub(MAX);
    String::from_utf8_lossy(&bytes[start..])
        .replace(['\r', '\n'], " ")
        .trim()
        .to_string()
}

fn lock(host: &Host) -> std::sync::MutexGuard<'_, ProvisionRun> {
    host.provision.lock().unwrap_or_else(|err| err.into_inner())
}
