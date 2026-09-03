//! The daemon. Its only job is to run; every control action is a client call
//! against the API. v1's binary was named `voice-core-cli` and hosted the
//! server behind a `serve` subcommand, which is the category error this split
//! removes.
//!
//! This binary also owns *layout*: where the interpreter, engine, models and
//! state live. The library stays layout-agnostic, so a packaged tree, a dev
//! checkout and a hand-configured install all reach it through the same
//! `Config`. See `docs/deployment.md`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::Parser;
use serde::Deserialize;
use voice_core::config::{ensure_token, Config, EnginePlacement, WorkerSource, WorkerSpec};

#[derive(Parser)]
#[command(
    name = "voice-core-runtime",
    version,
    about = "voice-core runtime: owns the engine process, serves the only public API"
)]
struct Args {
    /// Loopback address for the public API.
    #[arg(long, default_value = "127.0.0.1:8760")]
    bind: String,

    /// Token, voice packs, spool, logs and metrics live here.
    /// Defaults to `<install root>/data`, falling back to `%APPDATA%\voice-core`
    /// when the install directory is not writable.
    #[arg(long, env = "VC_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Attach to a worker somebody else runs, instead of managing one.
    #[arg(long, env = "VC_TTS_URL", conflicts_with = "tts_python")]
    tts_url: Option<String>,

    /// Interpreter owning the engine environment (managed mode).
    #[arg(long, env = "VC_TTS_PYTHON")]
    tts_python: Option<PathBuf>,

    /// Worker entry script.
    #[arg(long, env = "VC_TTS_SCRIPT")]
    tts_script: Option<PathBuf>,

    /// Engine source/model root, passed to the worker as `--root`.
    #[arg(long, env = "VC_TTS_ROOT")]
    tts_root: Option<PathBuf>,

    /// HuggingFace cache for the engine process.
    #[arg(long, env = "VC_HF_HOME")]
    hf_home: Option<PathBuf>,

    /// Stop the engine after this many idle seconds and give the GPU back.
    /// 0 keeps it resident. Defaults to `runtime.json`, then 900.
    #[arg(long)]
    idle_stop_secs: Option<u64>,

    #[arg(long, default_value_t = 3600)]
    spool_ttl_secs: u64,

    #[arg(long, default_value_t = 2048)]
    spool_max_mb: u64,

    #[arg(long, default_value_t = 90)]
    worker_ready_secs: u64,

    #[arg(long, default_value_t = 600)]
    synth_timeout_secs: u64,

    /// Print the resolved layout and exit. Use it to diagnose an install
    /// without starting anything.
    #[arg(long)]
    print_layout: bool,
}

/// `<data dir>/runtime.json`. Engine paths belong to the runtime, so a frontend
/// can launch it without knowing where Python, models or voice packs live —
/// which is exactly the knowledge v1's tray had to carry.
///
/// Precedence is flag, then file, then the packaged layout. Relative paths in
/// the file resolve against the install root, which is what makes an unzipped
/// tree work unchanged after being moved or copied to another machine.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RuntimeFile {
    #[serde(default)]
    tts_url: Option<String>,
    #[serde(default)]
    tts_python: Option<PathBuf>,
    #[serde(default)]
    tts_script: Option<PathBuf>,
    #[serde(default)]
    tts_root: Option<PathBuf>,
    #[serde(default)]
    hf_home: Option<PathBuf>,
    #[serde(default)]
    idle_stop_secs: Option<u64>,
}

fn load_runtime_file(data_dir: &Path) -> Result<RuntimeFile> {
    let path = data_dir.join("runtime.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Ok(RuntimeFile::default());
    };
    // A malformed config must fail loudly at startup, not degrade into "no
    // engine configured" three minutes later when somebody tries to speak.
    serde_json::from_str(&raw).with_context(|| format!("{} is not valid", path.display()))
}

/// Print to stdout AND append to `logs/runtime.out.log`.
///
/// The runtime owns its own log file for the same reason it owns the engine's:
/// a GUI that launches it must not have to pump its pipes, because then the
/// runtime's diagnostics would die with the GUI. Startup failures go to
/// `logs/runtime.err.log` the same way.
fn say(data_dir: &Path, line: &str) {
    println!("{line}");
    append_log(data_dir, "runtime.out.log", line);
}

fn append_log(data_dir: &Path, file: &str, line: &str) {
    use std::io::Write;
    let dir = data_dir.join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    if let Ok(mut handle) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join(file))
    {
        let _ = writeln!(handle, "{line}");
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let root = install_root();
    let data_dir = match args.data_dir.clone() {
        Some(dir) => dir,
        None => resolve_data_dir(&root),
    };
    match run(args, &root, &data_dir).await {
        Ok(()) => Ok(()),
        Err(err) => {
            // A GUI launcher sees no console, so a startup failure that only
            // reached stderr would be invisible. Put it where the logs are.
            append_log(&data_dir, "runtime.err.log", &format!("startup failed: {err:#}"));
            Err(err)
        }
    }
}

async fn run(args: Args, root: &Path, data_dir: &Path) -> Result<()> {
    let bind: SocketAddr = args
        .bind
        .parse()
        .with_context(|| format!("--bind is not an address: {}", args.bind))?;

    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("cannot create data dir {}", data_dir.display()))?;
    let file = load_runtime_file(data_dir)?;
    if let Some(note) = relocate_bundled_venv(root) {
        say(data_dir, &format!("  venv       {note}"));
    }
    let resolved = resolve_worker(&args, &file, root);

    if args.print_layout {
        // A diagnostic that fails when the install is broken is a diagnostic
        // that never gets used: print the reason instead of exiting on it.
        print_layout(root, data_dir, resolved.as_ref());
        return Ok(());
    }
    let worker = resolved?;

    let token = ensure_token(data_dir).context("cannot read or mint token.txt")?;
    let idle_stop_secs = args.idle_stop_secs.or(file.idle_stop_secs).unwrap_or(900);
    let idle_stop = (idle_stop_secs > 0).then(|| Duration::from_secs(idle_stop_secs));

    let cfg = Config {
        bind,
        data_dir: data_dir.to_path_buf(),
        token,
        worker: worker.clone(),
        idle_stop,
        spool_ttl: Duration::from_secs(args.spool_ttl_secs),
        spool_max_bytes: args.spool_max_mb * 1024 * 1024,
        worker_ready_timeout: Duration::from_secs(args.worker_ready_secs),
        synth_timeout: Duration::from_secs(args.synth_timeout_secs),
    };

    // Bind BEFORE assembling: assemble() opens the spool, and opening the spool clears
    // every WAV in it. That directory belongs to whichever runtime already owns this port,
    // so a second launch must fail on the port and touch nothing - otherwise it silences
    // the live instance, whose in-memory index still points at the files it just deleted.
    let listener = voice_core::bind(bind)
        .await
        .with_context(|| format!("cannot bind {bind}; is another runtime already running?"))?;
    let assembled = voice_core::assemble(cfg).context("cannot assemble runtime")?;

    // Never log the token itself; say where it lives.
    say(
        data_dir,
        &format!("voice-core-runtime {}", voice_core::RUNTIME_VERSION),
    );
    say(
        data_dir,
        &format!("  api        http://{}", listener.local_addr()?),
    );
    say(data_dir, &format!("  root       {}", root.display()));
    say(data_dir, &format!("  data dir   {}", data_dir.display()));
    say(
        data_dir,
        &format!("  token      {}", data_dir.join("token.txt").display()),
    );
    match &worker {
        WorkerSource::Managed(spec) => {
            say(
                data_dir,
                &format!("  engine     managed: {}", spec.script.display()),
            );
            say(
                data_dir,
                &format!(
                    "  idle stop  {}",
                    match idle_stop {
                        Some(d) => format!("{}s", d.as_secs()),
                        None => "never (engine stays resident)".to_string(),
                    }
                ),
            );
        }
        WorkerSource::External { base_url } => {
            say(data_dir, &format!("  engine     attached: {base_url}"));
        }
    }
    say(
        data_dir,
        "  events     GET /api/events (subtitles, worker state, progress)",
    );

    // Resource preflight. Serving still starts: a frontend must be able to
    // connect and be told what is missing, which beats exiting into a void.
    let missing = assembled.service.status().await.worker.missing;
    if !missing.is_empty() {
        let summary = format!(
            "missing {} configured resource(s): {}",
            missing.len(),
            missing.join("; ")
        );
        say(data_dir, &format!("  WARNING    {summary}"));
        say(
            data_dir,
            "             synthesis will fail until these exist; run with --print-layout to inspect",
        );
        append_log(data_dir, "runtime.err.log", &summary);
        assembled.service.note("preflight", summary);
    }

    voice_core::serve(listener, assembled.service, assembled.shutdown).await?;
    say(data_dir, "voice-core-runtime stopped");
    Ok(())
}

fn print_layout(root: &Path, data_dir: &Path, worker: Result<&WorkerSource, &anyhow::Error>) {
    println!("install root   {}", root.display());
    println!("data dir       {}", data_dir.display());
    println!("packs          {}", data_dir.join("config.json").display());
    match worker {
        Err(err) => {
            println!("engine         UNRESOLVED");
            println!("               {err:#}");
        }
        Ok(WorkerSource::External { base_url }) => {
            println!("engine         attached at {base_url}")
        }
        Ok(WorkerSource::Managed(spec)) => {
            let mark = |p: &Path| if p.exists() { "ok     " } else { "MISSING" };
            println!("interpreter    {} {}", mark(&spec.python), spec.python.display());
            println!("worker script  {} {}", mark(&spec.script), spec.script.display());
            match &spec.root {
                Some(dir) => println!("engine root    {} {}", mark(dir), dir.display()),
                None => println!("engine root    unset"),
            }
            for (key, value) in &spec.env {
                println!("{key:<14} {} {value}", mark(Path::new(value)));
            }
        }
    }
}

/// Install root. A packaged tree puts executables in `<root>/bin`; a dev build
/// puts them in `<repo>/target/{release,debug}`.
fn install_root() -> PathBuf {
    let Ok(exe) = std::env::current_exe() else {
        return PathBuf::from(".");
    };
    let dir = exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    if dir.file_name().and_then(|name| name.to_str()) == Some("bin") {
        if let Some(parent) = dir.parent() {
            return parent.to_path_buf();
        }
    }
    let mut probe: Option<&Path> = Some(dir.as_path());
    for _ in 0..4 {
        let Some(current) = probe else { break };
        if current.join("Cargo.toml").is_file() {
            return current.to_path_buf();
        }
        probe = current.parent();
    }
    dir
}

/// `<root>/data`, unless the install directory is read-only — a Program Files
/// install must still be able to keep a token, a spool and logs somewhere.
fn resolve_data_dir(root: &Path) -> PathBuf {
    let preferred = root.join("data");
    if is_writable(&preferred) {
        return preferred;
    }
    match std::env::var_os("APPDATA") {
        Some(appdata) => PathBuf::from(appdata).join("voice-core"),
        None => preferred,
    }
}

fn is_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Relative paths are resolved against the install root, never the working
/// directory: a shortcut, a tray launch and a shell all start with a different
/// cwd, and an install must not care.
fn absolute(root: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        root.join(path)
    }
}

fn resolve_worker(args: &Args, file: &RuntimeFile, root: &Path) -> Result<WorkerSource> {
    if let Some(base_url) = args.tts_url.as_ref().or(file.tts_url.as_ref()) {
        return Ok(WorkerSource::External {
            base_url: base_url.trim_end_matches('/').to_string(),
        });
    }

    // Packaged layout defaults; each is used only when it actually exists, so a
    // dev checkout is not handed paths that were never installed.
    // A bundled interpreter is either a virtualenv (python.exe under Scripts\)
    // or an embeddable distribution (python.exe at the root); accept both.
    let packaged_pythons = [
        root.join("runtime/python/Scripts/python.exe"),
        root.join("runtime/python/python.exe"),
    ];
    let packaged_script = root.join("runtime/worker/irodori/worker.py");
    let dev_script = root.join("worker/irodori/worker.py");
    let packaged_engine = root.join("runtime/engine");
    let packaged_models = root.join("models/huggingface");

    let python = args
        .tts_python
        .clone()
        .or_else(|| file.tts_python.clone())
        .map(|path| absolute(root, path))
        .or_else(|| packaged_pythons.into_iter().find(|path| path.is_file()));
    let Some(python) = python else {
        bail!(
            "no TTS engine configured: expected an interpreter at \
             <root>/runtime/python/Scripts/python.exe (virtualenv) or \
             <root>/runtime/python/python.exe (embeddable), or set ttsPython in \
             the data dir's runtime.json, or pass --tts-python, or --tts-url to \
             attach to a worker you run yourself"
        );
    };

    let script = args
        .tts_script
        .clone()
        .or_else(|| file.tts_script.clone())
        .map(|path| absolute(root, path))
        .or_else(|| packaged_script.is_file().then_some(packaged_script))
        .or_else(|| dev_script.is_file().then_some(dev_script))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "cannot find the worker script: expected <root>/runtime/worker/irodori/worker.py; \
                 set ttsScript in runtime.json or pass --tts-script"
            )
        })?;

    let engine_root = args
        .tts_root
        .clone()
        .or_else(|| file.tts_root.clone())
        .map(|path| absolute(root, path))
        .or_else(|| packaged_engine.is_dir().then_some(packaged_engine));

    let mut env = Vec::new();
    let hf_home = args
        .hf_home
        .clone()
        .or_else(|| file.hf_home.clone())
        .map(|path| absolute(root, path))
        .or_else(|| packaged_models.is_dir().then_some(packaged_models));
    if let Some(hf_home) = hf_home {
        env.push(("HF_HOME".to_string(), hf_home.display().to_string()));
        env.push((
            "HF_HUB_CACHE".to_string(),
            hf_home.join("hub").display().to_string(),
        ));
    }

    Ok(WorkerSource::Managed(WorkerSpec {
        python,
        script,
        root: engine_root,
        env,
        // No flag and no runtime.json key: the placement is a measurement knob on the
        // worker itself, so the daemon only ever hands the engine its shipped defaults.
        placement: EnginePlacement::default(),
    }))
}

/// Repoint a bundled virtual environment at the interpreter shipped beside it.
///
/// A Windows venv is not relocatable: `pyvenv.cfg` records an absolute `home`
/// pointing at the base interpreter that created it. A portable package
/// therefore ships that interpreter as `runtime/python-base`, and this rewrites
/// `home` whenever the recorded path has gone missing — which is exactly what
/// happens after unzipping somewhere else or copying to another machine.
/// Returns a note when it changed something.
fn relocate_bundled_venv(root: &Path) -> Option<String> {
    let cfg_path = root.join("runtime/python/pyvenv.cfg");
    let base = root.join("runtime/python-base");
    let cfg = std::fs::read_to_string(&cfg_path).ok()?;

    let current = cfg.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        (key.trim() == "home").then(|| value.trim().to_string())
    })?;
    if Path::new(&current).is_dir() {
        return None;
    }
    if !base.is_dir() {
        return Some(format!(
            "bundled venv points at a missing interpreter ({current}) and {} does not exist",
            base.display()
        ));
    }

    let repaired: String = cfg
        .lines()
        .map(|line| match line.split_once('=') {
            Some((key, _)) if key.trim() == "home" => format!("home = {}", base.display()),
            _ => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    match std::fs::write(&cfg_path, repaired + "\n") {
        Ok(()) => Some(format!("repointed bundled venv at {}", base.display())),
        Err(err) => Some(format!("cannot repair {}: {err}", cfg_path.display())),
    }
}

