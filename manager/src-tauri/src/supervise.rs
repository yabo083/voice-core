//! The two children this app exists to own: the backend service and the subtitle
//! presenter.
//!
//! Neither is something a user launches any more. `VoiceCore.exe` starts them,
//! points their pipes at `data/logs/`, and holds them in a job object so that
//! Quit — or a crash, or Task Manager — takes them with it.

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use tauri::{AppHandle, Emitter, Manager};

use crate::contract::{StackState, EVENT_STACK};
use crate::host::{hidden, Host};
use crate::jobobj::{kill_tree, Job};
use crate::layout;

#[derive(Default)]
pub struct Stack {
    /// Dropping this kills both children and everything they started. It is what
    /// makes Quit final even when this process is killed instead of asked, and
    /// the reason no PID is ever targeted by number.
    job: Option<Job>,
    runtime: Option<Child>,
    presenter: Option<Child>,
    /// Set while a start is in flight. Two fast clicks on the tray's start entry
    /// would otherwise spawn a second runtime that dies on `bind`, with the
    /// failure visible only in a log.
    starting: bool,
    /// Last published tuple, so the event fires on transitions and not on a
    /// timer.
    published: Option<StackState>,
}

#[tauri::command]
pub async fn start_stack(app: AppHandle) -> Result<(), String> {
    start(&app).await
}

#[tauri::command]
pub async fn stop_stack(app: AppHandle) -> Result<(), String> {
    {
        let host = app.state::<Host>();
        stop(&host);
    }
    let state = probe(&app).await;
    publish(&app, state);
    Ok(())
}

/// The PIDs of the two children this app owns.
///
/// Exposed for measurement only - `usage.rs` needs a starting point for the process
/// tree, and nothing here targets a PID for control. Killing still goes through the
/// job object, which is what makes it impossible to signal the wrong process.
#[derive(Default)]
pub struct Pids {
    pub runtime: Option<u32>,
    pub presenter: Option<u32>,
}

pub fn pids(host: &Host) -> Pids {
    let stack = lock(host);
    Pids {
        runtime: stack.runtime.as_ref().map(std::process::Child::id),
        presenter: stack.presenter.as_ref().map(std::process::Child::id),
    }
}

/// Start whatever is not already running: the runtime first, then the presenter
/// once the API answers.
///
/// The order matters. The presenter subscribes to `GET /api/events` and reads the
/// token from disk; starting it against a socket nobody is listening on turns its
/// first job into a retry loop. `/api/health` is unauthenticated precisely so a
/// launcher can wait for readiness before it knows the token.
pub async fn start(app: &AppHandle) -> Result<(), String> {
    let host = app.state::<Host>();

    let runtime_exe = layout::runtime_exe(&host.root).ok_or_else(|| {
        format!(
            "voice-core-runtime.exe not found in {0}\\bin or {0}\\target\\<profile>",
            host.root.display()
        )
    })?;

    {
        let mut stack = lock(&host);
        if stack.starting {
            return Ok(());
        }
        stack.starting = true;
        if stack.job.is_none() {
            match Job::new() {
                Ok(job) => stack.job = Some(job),
                // Without the job each child is still killed individually on
                // Quit; only their grandchildren would survive. Worth a log line,
                // not worth refusing to start.
                Err(err) => host.log(&format!("job object unavailable: {err}")),
            }
        }
    }

    let result = start_inner(&host, &runtime_exe).await;
    lock(&host).starting = false;
    let state = probe(app).await;
    publish(app, state);
    result
}

async fn start_inner(host: &Host, runtime_exe: &Path) -> Result<(), String> {
    if !alive(host, Which::Runtime) {
        let mut command = Command::new(runtime_exe);
        command
            .arg("--data-dir")
            .arg(&host.data_dir)
            .current_dir(&host.root)
            .stdin(Stdio::null());
        redirect(host, &mut command, "runtime");
        hidden(&mut command);
        let child = command
            .spawn()
            .map_err(|err| format!("spawn {}: {err}", runtime_exe.display()))?;
        adopt(host, child, Which::Runtime);
        host.log(&format!("runtime started from {}", runtime_exe.display()));
    }

    wait_for_api(host).await;

    if !alive(host, Which::Presenter) {
        let presenter_exe = layout::presenter_exe(&host.root).ok_or_else(|| {
            format!(
                "presenter not found: looked for bin\\presenter\\VoiceCorePresenter.exe under {}",
                host.root.display()
            )
        })?;
        let mut command = Command::new(&presenter_exe);
        // `--presenter` alone already implies no runtime; both flags are passed
        // because the contract says so and the presenter ignores the redundancy.
        command
            .arg("--presenter")
            .arg("--no-runtime")
            .current_dir(&host.root)
            .stdin(Stdio::null());
        redirect(host, &mut command, "presenter");
        hidden(&mut command);
        let child = command
            .spawn()
            .map_err(|err| format!("spawn {}: {err}", presenter_exe.display()))?;
        adopt(host, child, Which::Presenter);
        host.log(&format!("presenter started from {}", presenter_exe.display()));

        // The presenter is single-instance through a named mutex, and a second
        // copy calls Environment.Exit(0) at once — so exit code 0 here means
        // "someone else already owns the dialog", not "ran fine". Say so, or the
        // panel claims the presenter is off while the user's hotkeys still work.
        tokio::time::sleep(Duration::from_millis(1200)).await;
        if !alive(host, Which::Presenter) {
            let note = "presenter exited immediately: another presenter instance is probably \
                        already running, and its named mutex made this one exit";
            host.log(note);
            return Err(note.to_string());
        }
    }

    Ok(())
}

/// Kill both children and release the job. Called by Quit and by the tray's stop
/// entry; safe to call when nothing is running.
pub fn stop(host: &Host) {
    let mut guard = lock(host);
    let stack = &mut *guard;

    // Before the job closes, while parent links still exist: anything a child
    // started in the window between `spawn` and `assign` is outside the job and
    // reachable only this way.
    for child in [stack.runtime.as_ref(), stack.presenter.as_ref()]
        .into_iter()
        .flatten()
    {
        kill_tree(child.id());
    }

    stack.job = None;

    for slot in [&mut stack.runtime, &mut stack.presenter] {
        if let Some(mut child) = slot.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    drop(guard);
    host.log("stack stopped");
}

/// Poll the children and the API every two seconds and publish transitions.
///
/// This is what makes a runtime that died on its own visible. It deliberately
/// does not restart anything: a crash loop that reopens a GPU every two seconds
/// is worse than a panel that says the backend is down.
pub fn watch(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        loop {
            let state = probe(&app).await;
            publish(&app, state);
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

async fn probe(app: &AppHandle) -> StackState {
    let host = app.state::<Host>();
    let presenter = alive(&host, Which::Presenter);
    let runtime = health(&host).await;
    let model_loaded = if runtime { model_loaded(&host).await } else { false };
    StackState {
        runtime,
        presenter,
        model_loaded,
    }
}

fn publish(app: &AppHandle, state: StackState) {
    {
        let host = app.state::<Host>();
        let mut stack = lock(&host);
        if stack.published == Some(state) {
            return;
        }
        stack.published = Some(state);
    }
    let _ = app.emit(EVENT_STACK, state);
}

async fn health(host: &Host) -> bool {
    let url = format!("{}/api/health", host.base_url);
    matches!(host.http.get(url).send().await, Ok(response) if response.status().is_success())
}

async fn model_loaded(host: &Host) -> bool {
    let Some(token) = host.token() else {
        return false;
    };
    let url = format!("{}/api/status", host.base_url);
    let Ok(response) = host.http.get(url).bearer_auth(token).send().await else {
        return false;
    };
    let Ok(body) = response.json::<serde_json::Value>().await else {
        return false;
    };
    body.pointer("/worker/modelLoaded")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false)
}

/// Wait for the API to answer before the presenter needs it. Generous, because a
/// cold start pays for binding a port and reading a config; bounded, because a
/// runtime that never answers must not hold the UI hostage.
async fn wait_for_api(host: &Host) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if health(host).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    host.log("runtime did not answer /api/health within 20s; starting the presenter anyway");
}

/// Files, not pipes.
///
/// The runtime already keeps its own `runtime.out.log` — a pipe this app failed
/// to drain would eventually block the process it is supposed to be supervising,
/// which is why the runtime owns that file in the first place. These two catch
/// what happens before any logging exists: a panic, a missing DLL, a rejected
/// `--data-dir`. The names differ from the runtime's own logs because two writers
/// appending to one file interleave into nonsense.
fn redirect(host: &Host, command: &mut Command, stem: &str) {
    match host.child_log(&format!("{stem}.stdout.log")) {
        Ok(file) => {
            command.stdout(Stdio::from(file));
        }
        Err(err) => host.log(&format!("{stem}.stdout.log unavailable: {err}")),
    }
    match host.child_log(&format!("{stem}.stderr.log")) {
        Ok(file) => {
            command.stderr(Stdio::from(file));
        }
        Err(err) => host.log(&format!("{stem}.stderr.log unavailable: {err}")),
    }
}

#[derive(Clone, Copy)]
enum Which {
    Runtime,
    Presenter,
}

fn adopt(host: &Host, child: Child, which: Which) {
    let mut stack = lock(host);
    if let Some(job) = stack.job.as_ref() {
        if let Err(err) = job.assign(&child) {
            host.log(&format!("could not assign child to job object: {err}"));
        }
    }
    match which {
        Which::Runtime => stack.runtime = Some(child),
        Which::Presenter => stack.presenter = Some(child),
    }
}

/// Is the child still running? Reaps it and logs the exit code when it is not, so
/// the log records *who* died rather than only that a count changed.
fn alive(host: &Host, which: Which) -> bool {
    let mut guard = lock(host);
    let (slot, name) = match which {
        Which::Runtime => (&mut guard.runtime, "runtime"),
        Which::Presenter => (&mut guard.presenter, "presenter"),
    };
    let Some(child) = slot.as_mut() else {
        return false;
    };
    // Bound before the match: the autoref temporary in a match scrutinee lives
    // until the match ends, and clearing `slot` inside an arm would still be
    // holding a borrow derived from it.
    let status = child.try_wait();
    match status {
        Ok(None) => true,
        Ok(Some(status)) => {
            let code = status.code().unwrap_or(-1);
            *slot = None;
            host.log(&format!("{name} exited with code {code}"));
            false
        }
        Err(err) => {
            host.log(&format!("{name} status unknown: {err}"));
            false
        }
    }
}

/// A poisoned lock here would mean a panic while holding process handles. Taking
/// the inner value keeps the app usable instead of poisoning every later call.
fn lock(host: &Host) -> std::sync::MutexGuard<'_, Stack> {
    host.stack.lock().unwrap_or_else(|err| err.into_inner())
}
