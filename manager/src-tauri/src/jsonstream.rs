//! One child process, one JSON object per line, relayed to the frontend as it arrives.
//!
//! `scripts/bootstrap.ps1 -Json` established the shape and `scripts/training/_layout.py`
//! joined it: exactly one JSON object per line on stdout and nothing else, every key
//! always present. So this reads lines rather than accumulating output — a 4.4 GiB
//! download or a fifty-minute training run reports progress while it happens or it does
//! not report at all.
//!
//! Anything on stdout that is not JSON is forwarded as a `log` event instead of being
//! dropped or treated as fatal: a stray `Write-Host`, or a library that writes to the
//! process handle instead of Python's `sys.stdout`, should look like noise in the panel
//! rather than like a crash.
//!
//! Two things this deliberately does not do. It never parses a child's *content* — the step
//! that produces a number is the step that reports it, next to the process that printed it.
//! And it never fabricates a failure: a stage that fails says so through the stream and
//! exits 0, so a non-zero exit means the argv was wrong, which is a bug in the caller
//! rather than something a user can act on.

use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};

use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use crate::host::{hidden, now_ms, Host};
use crate::jobobj::{kill_tree, Job};
use crate::layout;

/// The slot one streamed run occupies. Provisioning and training own separate slots,
/// because cancelling one must not touch the other.
#[derive(Default)]
pub struct StreamRun {
    /// Dropping this kills the child *and* everything it started — a git clone, uv, pip, a
    /// HuggingFace download, upstream's trainer and its DataLoader worker processes.
    /// Killing the direct child alone leaves all of those running with nobody reading
    /// their output.
    job: Option<Job>,
    pid: Option<u32>,
    /// Claimed for the whole run, so a second click cannot start a second run into the
    /// same directories.
    busy: bool,
    /// A cancelled run exits non-zero. That is not a failure to report as one.
    cancelled: bool,
}

impl StreamRun {
    /// Take the slot, or report that it is taken. Refusing is the right answer: two runs
    /// into one set of directories is not something to queue.
    pub fn claim(&mut self) -> bool {
        if self.busy {
            return false;
        }
        self.busy = true;
        self.cancelled = false;
        true
    }

    /// Give the slot back. The job handle goes with it, which is what kills anything the
    /// child left behind.
    pub fn release(&mut self) {
        self.busy = false;
        self.pid = None;
        self.job = None;
    }
}

/// How the run ended, for a caller that has to decide what happens next.
///
/// `provision` ignores both fields — it runs one script and the panel reads the stream. A
/// pipeline of six steps cannot: it has to stop rather than feed a dataset that was never
/// written to the step after it.
#[derive(Debug)]
pub struct Outcome {
    /// A terminal `fail` event crossed the stream. The child has already explained itself
    /// to the frontend, remedy included, so a caller stops the chain and adds nothing.
    pub failed: bool,
    /// The run was killed on purpose, which is why its exit code is not a failure.
    pub cancelled: bool,
}

/// What one streamed run is called, where its noise goes, and what a line belongs to before
/// the child has named a stage.
pub struct Spec<'a> {
    /// Tauri event name every line is re-emitted on, verbatim.
    pub event: &'a str,
    /// What the child is called in an error a user reads: `bootstrap.ps1`, not
    /// `powershell.exe`.
    pub label: &'a str,
    /// Where the child's stderr goes, under `data/logs`. Truncated per run: the point of
    /// that file is to name the cause of *this* failure, and a stale tail would name the
    /// wrong one.
    pub stderr_log: &'a str,
    /// The stage a non-JSON line belongs to before the first `start` arrives.
    pub first_stage: &'a str,
}

/// Spawn, stream, and wait. Blocking by nature — the loop below is a blocking read — so
/// callers give it a thread of its own.
pub fn run(
    app: &AppHandle,
    spec: &Spec,
    mut command: Command,
    slot: &Mutex<StreamRun>,
) -> Result<Outcome, String> {
    let host = app.state::<Host>();

    // stderr to a file, never a second pipe. A pipe nobody drains while waiting on the first
    // one is exactly how this deadlocks, and the file is also what keeps a traceback that
    // never reached the protocol.
    match host.fresh_child_log(spec.stderr_log) {
        Ok(file) => {
            command.stderr(Stdio::from(file));
        }
        Err(err) => {
            host.log(&format!("{} unavailable: {err}", spec.stderr_log));
            command.stderr(Stdio::null());
        }
    }

    let mut emit = |value: Value| {
        let _ = app.emit(spec.event, value);
    };
    let log = |line: &str| host.log(line);
    drive(
        command,
        spec,
        &layout::logs_dir(&host.data_dir).join(spec.stderr_log),
        slot,
        &mut emit,
        &log,
    )
}

/// All of it except the two things only a running app can provide: somewhere to send events
/// and somewhere to log. Passing those in rather than reaching for an `AppHandle` is what
/// lets this be tested against a real child process.
///
/// `spec.event` is the caller's business; `stderr` is where the child's own stderr was already
/// pointed, and is read back only to quote the cause of a rejected argv.
fn drive(
    mut command: Command,
    spec: &Spec,
    stderr: &std::path::Path,
    slot: &Mutex<StreamRun>,
    emit: &mut dyn FnMut(Value),
    log: &dyn Fn(&str),
) -> Result<Outcome, String> {
    // Cancelling in the window between claiming the slot and spawning has to count, or a
    // click landing there would start the child it was meant to prevent.
    if lock(slot).cancelled {
        return Ok(Outcome {
            failed: false,
            cancelled: true,
        });
    }
    command.stdin(Stdio::null()).stdout(Stdio::piped());
    hidden(&mut command);

    // The program for a spawn failure, the label for everything after it: "powershell.exe
    // could not be started" and "bootstrap.ps1 rejected its arguments" are different
    // sentences about different things, and only the first is about the executable.
    let program = command.get_program().to_string_lossy().into_owned();
    let mut child = command.spawn().map_err(|err| format!("{program}: {err}"))?;

    {
        let mut run = lock(slot);
        match Job::new() {
            Ok(job) => {
                if let Err(err) = job.assign(&child) {
                    log(&format!(
                        "could not assign {} to job object: {err}",
                        spec.label
                    ));
                }
                run.job = Some(job);
            }
            Err(err) => log(&format!("job object unavailable: {err}")),
        }
        run.pid = Some(child.id());
    }

    let mut stage = spec.first_stage.to_string();
    let mut failed = false;
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
                    if value.get("event").and_then(Value::as_str) == Some("fail") {
                        failed = true;
                    }
                    emit(value);
                }
                _ => emit(log_event(&stage, line)),
            }
        }
    }

    let status = child
        .wait()
        .map_err(|err| format!("waiting for {}: {err}", spec.label))?;

    // The PID stops meaning this child the moment it exits, and Windows recycles numbers.
    // Between two steps of a pipeline the slot is still claimed but there is nothing to
    // kill, which is why `cancel` keys on `busy` rather than on this.
    let cancelled = {
        let mut run = lock(slot);
        run.pid = None;
        run.cancelled
    };
    if cancelled || status.success() {
        return Ok(Outcome { failed, cancelled });
    }

    // A failed *stage* still exits 0 and reports itself through the event stream. A
    // non-zero exit means the child rejected its own arguments, which is a bug in the argv
    // its caller built rather than something the user can act on — so it is reported as a
    // rejected call, not as a fabricated `fail` event on a stage that never ran.
    let code = status.code().unwrap_or(-1);
    let tail = stderr_tail(stderr);
    Err(if tail.is_empty() {
        format!("{} exited with code {code}", spec.label)
    } else {
        format!("{} exited with code {code}: {tail}", spec.label)
    })
}

/// Cancel the run in `slot`, killing its process tree. False when no run was claimed.
///
/// Two mechanisms for the kill, deliberately: `taskkill /T /F` first, because it walks parent
/// links and catches anything spawned in the window between `spawn` and the job assignment,
/// and then the job handle, because KILL_ON_JOB_CLOSE is the only guarantee that does not
/// depend on a PID still meaning what it meant.
///
/// Keyed on the claim rather than on a live PID: a pipeline between two of its steps has
/// nothing to kill and still has to stop.
pub fn cancel(slot: &Mutex<StreamRun>) -> bool {
    let pid = {
        let mut run = lock(slot);
        if !run.busy {
            return false;
        }
        run.cancelled = true;
        run.pid.take()
    };
    // Outside the lock: taskkill is a process spawn, and the thread reading the stream
    // wants this lock for every line it forwards.
    if let Some(pid) = pid {
        kill_tree(pid);
    }
    lock(slot).job = None;
    true
}

/// All seven keys, always, so the frontend never has to test for their presence.
///
/// Deliberately no `checkpoint`: that key belongs to the training pipeline's own emitter,
/// and a line this file invents has no artefact to name. Adding it here would change what a
/// bootstrap run emits for a stray `Write-Host`.
pub fn log_event(stage: &str, message: &str) -> Value {
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

/// The last of the child's own words, for quoting inside a rejected call.
fn stderr_tail(path: &std::path::Path) -> String {
    const MAX: usize = 2048;
    let Ok(bytes) = std::fs::read(path) else {
        return String::new();
    };
    let start = bytes.len().saturating_sub(MAX);
    String::from_utf8_lossy(&bytes[start..])
        .replace(['\r', '\n'], " ")
        .trim()
        .to_string()
}

/// A poisoned slot is still a usable slot: what it guards is four scalars and a handle, and
/// refusing to cancel a run because a reader thread panicked would be worse than the panic.
pub fn lock(slot: &Mutex<StreamRun>) -> MutexGuard<'_, StreamRun> {
    slot.lock().unwrap_or_else(|err| err.into_inner())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    const SPEC: Spec<'static> = Spec {
        event: "test://event",
        label: "child",
        stderr_log: "unused-in-tests.log",
        first_stage: "dataset",
    };

    /// Collect what `drive` would have emitted.
    fn collect(spec: &Spec, mut command: Command) -> (Result<Outcome, String>, Vec<Value>) {
        let slot = Mutex::new(StreamRun::default());
        let mut events: Vec<Value> = Vec::new();
        let tail = std::env::temp_dir().join("voice-core-jsonstream-test.err.log");
        let file = std::fs::File::create(&tail).expect("a temp file for the child's stderr");
        command.stderr(Stdio::from(file));
        let outcome = {
            let mut emit = |value: Value| events.push(value);
            drive(command, spec, &tail, &slot, &mut emit, &|_| {})
        };
        (outcome, events)
    }

    /// The repository root, two levels above this crate.
    ///
    /// Popped rather than canonicalised: `canonicalize` returns an extended-length
    /// `\\?\E:\...` path on Windows, and PowerShell's `Join-Path` cannot read a drive out of
    /// one — which makes bootstrap.ps1 fail on its own argument rather than on anything this
    /// is testing.
    fn repo() -> PathBuf {
        let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        root.pop();
        root.pop();
        root
    }

    fn powershell(script: &str) -> Command {
        let mut command = Command::new("powershell.exe");
        command
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-Command")
            .arg(script);
        command
    }

    /// A protocol object is forwarded byte for byte, a line that is not one becomes a `log`
    /// event on whatever stage was last named, and a `fail` is reported as a failed stage
    /// rather than as a failed call — the child exited 0, which is the protocol's rule.
    #[test]
    fn relays_objects_and_wraps_the_rest() {
        let (outcome, events) = collect(&SPEC, powershell(
            r#"
            [Console]::Out.WriteLine('{"ts":1,"stage":"dataset","event":"start","message":"80 clips","done":null,"total":null,"remedy":null,"checkpoint":null}')
            [Console]::Out.WriteLine('a stray Write-Host')
            [Console]::Out.WriteLine('{"ts":2,"stage":"latents","event":"fail","message":"nothing was encoded","done":null,"total":null,"remedy":"read the skip counts","checkpoint":null}')
            exit 0
            "#,
        ));

        let outcome = outcome.expect("a stage that fails still exits 0");
        assert!(outcome.failed, "the fail event has to reach the caller");
        assert!(!outcome.cancelled);

        assert_eq!(events.len(), 3);
        assert_eq!(events[0]["event"], "start");
        assert_eq!(events[0]["message"], "80 clips");
        assert_eq!(events[0]["checkpoint"], Value::Null);

        // The stray line inherits `latents`? No: it arrived before it, so it carries the
        // stage in force at the time, which is the one the first object named.
        assert_eq!(events[1]["event"], "log");
        assert_eq!(events[1]["stage"], "dataset");
        assert_eq!(events[1]["message"], "a stray Write-Host");
        assert_eq!(events[1]["remedy"], Value::Null);

        assert_eq!(events[2]["event"], "fail");
        assert_eq!(events[2]["remedy"], "read the skip counts");
    }

    /// A non-zero exit is the one outcome that is not the stage's business: the argv was
    /// wrong, and the child's own stderr is quoted because it is the only thing that says
    /// how.
    #[test]
    fn a_rejected_argv_is_a_rejected_call() {
        let (outcome, events) = collect(
            &SPEC,
            powershell("[Console]::Error.WriteLine('unrecognised argument --nope'); exit 3"),
        );
        assert!(events.is_empty());
        let message = outcome.expect_err("a non-zero exit is an error");
        assert!(message.starts_with("child exited with code 3"), "{message}");
        assert!(message.contains("unrecognised argument --nope"), "{message}");
    }

    /// The refactor's own regression test: `bootstrap.ps1 -Json -CheckOnly` through the same
    /// runner, which is what `provision` now calls. Detects and reports; mutates nothing.
    #[test]
    fn relays_bootstrap_check_only() {
        let root = repo();
        let spec = Spec {
            event: "test://event",
            label: "bootstrap.ps1",
            stderr_log: "unused-in-tests.log",
            first_stage: "preflight",
        };
        let mut command = Command::new("powershell.exe");
        command
            .arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(root.join("scripts/bootstrap.ps1"))
            .arg("-Json")
            .arg("-InstallRoot")
            .arg(&root)
            .arg("-CheckOnly")
            .current_dir(&root);

        let (outcome, events) = collect(&spec, command);
        outcome.expect("a check-only pass exits 0 whatever it finds");
        assert!(events.len() > 20, "seven stages report themselves");
        assert_eq!(events[0]["stage"], "preflight");
        assert!(events.iter().all(|event| {
            ["ts", "stage", "event", "message", "done", "total", "remedy"]
                .iter()
                .all(|key| event.get(key).is_some())
        }));
    }

    /// The real thing: the shipped `prepare_dataset.py --json` over the corpus in `assets/`,
    /// relayed by the same function the Tauri command uses. Skipped where there is no
    /// provisioned interpreter to run it with, because then there is nothing to prove rather
    /// than something that failed.
    #[test]
    fn relays_the_real_dataset_stage() {
        let root = repo();
        let audio = root.join("assets/training/audio");
        let Some(python) = test_python(&root) else {
            eprintln!("no provisioned interpreter: skipping");
            return;
        };
        if !audio.is_dir() {
            eprintln!("no corpus at {}: skipping", audio.display());
            return;
        }

        let out = std::env::temp_dir().join("voice-core-jsonstream-test/dataset.jsonl");
        let mut command = Command::new(python);
        command
            .arg(root.join("scripts/training/irodori/prepare_dataset.py"))
            .arg("--json")
            .arg("--audio-dir")
            .arg(&audio)
            .arg("--transcripts")
            .arg(root.join("assets/training/data/shun_latent_manifest.jsonl"))
            .arg("--speaker-id")
            .arg("ba_shun_kid")
            .arg("--out-dataset")
            .arg(&out)
            .current_dir(&root);

        let (outcome, events) = collect(&SPEC, command);
        outcome.expect("the dataset step runs on the CPU in seconds");

        // Every line was a protocol object rather than a human one the runner wrapped: the
        // pipeline's emitter always writes `checkpoint`, and the runner never invents it.
        assert!(events.iter().all(|event| event.get("checkpoint").is_some()));
        let start = &events[0];
        assert_eq!(start["event"], "start");
        assert_eq!(start["stage"], "dataset");

        let progress: Vec<&Value> = events
            .iter()
            .filter(|event| event["event"] == "progress")
            .collect();
        assert!(progress.len() > 50, "one event per clip, {} clips", progress.len());
        assert_eq!(progress[0]["done"], 1);
        assert!(progress[0]["total"].as_u64().is_some_and(|total| total > 50));

        let last = events.last().expect("a terminal event");
        assert_eq!(last["event"], "ok");
        assert!(last["done"].as_u64().is_some_and(|done| done > 50));
        assert!(out.with_extension("jsonl.qa.json").is_file() || out.is_file());
    }

    /// `runtime.json`'s interpreter, the same one the training commands resolve.
    fn test_python(root: &std::path::Path) -> Option<PathBuf> {
        let raw = std::fs::read_to_string(root.join("data/runtime.json")).ok()?;
        let file: Value = serde_json::from_str(&raw).ok()?;
        let path = PathBuf::from(file.get("ttsPython")?.as_str()?);
        path.is_file().then_some(path)
    }
}
