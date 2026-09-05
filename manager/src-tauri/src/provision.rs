//! `scripts/bootstrap.ps1 -Json`, streamed to the frontend one line at a time by
//! [`crate::jsonstream`].
//!
//! What is left in this file is what belongs to bootstrap alone: the argv, the claim on the
//! slot, and the one piece of cached state a run invalidates. Reading the stream, owning the
//! process tree and deciding what a non-zero exit means are in `jsonstream`, which the
//! training pipeline reuses unchanged rather than re-deriving.

use std::process::Command;

use tauri::{AppHandle, Manager};

use crate::contract::{ProvisionOpts, EVENT_BOOTSTRAP};
use crate::host::Host;
use crate::jsonstream::{self, Outcome, Spec};
use crate::layout;

/// Where bootstrap's own stderr goes.
const STDERR_LOG: &str = "bootstrap.err.log";

/// `preflight` is the first stage the script runs, so a non-JSON line arriving before the
/// first `start` still carries a stage the frontend can key on. The label is the script
/// rather than the shell: `powershell.exe exited with code 1` names the wrong thing.
const SPEC: Spec<'static> = Spec {
    event: EVENT_BOOTSTRAP,
    label: "bootstrap.ps1",
    stderr_log: STDERR_LOG,
    first_stage: "preflight",
};

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
        if !jsonstream::lock(&host.provision).claim() {
            return Err("a provision run is already in progress".to_string());
        }
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
        jsonstream::lock(&host.provision).release();
        // A run can put a different interpreter on disk, which is the one thing
        // that invalidates the cached torch probe.
        *host.probe.lock().unwrap_or_else(|err| err.into_inner()) = None;
    }

    // A failed stage is not a failed call: the panel read it off the stream, with its
    // remedy, while it was happening.
    outcome?.map(|_: Outcome| ())
}

/// Kill the whole process tree.
#[tauri::command]
pub async fn cancel_provision(app: AppHandle) {
    let host = app.state::<Host>();
    if jsonstream::cancel(&host.provision) {
        host.log("provision cancelled");
    }
}

fn run_bootstrap(
    app: &AppHandle,
    script: std::path::PathBuf,
    args: Vec<String>,
) -> Result<Outcome, String> {
    let host = app.state::<Host>();

    let mut command = Command::new("powershell.exe");
    command
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-File")
        .arg(&script)
        .args(&args)
        .current_dir(&host.root);
    host.log(&format!("bootstrap started: {}", args.join(" ")));

    jsonstream::run(app, &SPEC, command, &host.provision)
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
