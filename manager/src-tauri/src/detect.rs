//! What provisioning would find if it ran right now.
//!
//! Everything here is a read: no file is created, no download is started, no GPU
//! is touched. The expensive part is the interpreter probe, which imports torch
//! and therefore costs about five seconds cold — so it only runs when an
//! interpreter actually exists (a fresh install has none, and answers instantly)
//! and its answer is cached until a provision run could have replaced it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use tauri::{AppHandle, Manager};

use crate::config_edit;
use crate::contract::{Inventory, ModelState};
use crate::host::{hidden, Host};
use crate::layout;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// The three repositories the Irodori backend loads: repository id, the file that
/// proves the download finished, and what the repository costs on disk.
///
/// The payload name is per-repository on purpose. The codec ships `weights.pth`,
/// not `model.safetensors`, so one generic probe would report it missing forever
/// and re-download 429 MB every time. Sizes are measured directory totals;
/// together they are 4,770,462,001 B = 4.44 GiB, which is the 4.8 the docs quote
/// in decimal GB.
const MODELS: [(&str, &str, u64); 3] = [
    (
        "Aratako/Irodori-TTS-v4.1-Small",
        "model.safetensors",
        3_071_026_671,
    ),
    (
        "sbintuitions/modernbert-ja-310m",
        "model.safetensors",
        1_269_815_225,
    ),
    (
        "Aratako/Semantic-DACVAE-Japanese-32dim",
        "weights.pth",
        429_620_105,
    ),
];

/// What an interpreter environment for this engine occupies once torch and its
/// CUDA wheels are installed. Measured from a working `env/` on this machine
/// (5,354,381,952 B). It counts into `needs_gib` because that number sits next to
/// `disk_free_gib` and therefore has to mean "space you must have free".
const ENV_BYTES: u64 = 5_354_381_952;

/// Proof that the engine source is really there, matching what the worker itself
/// imports. Never `.git`: neither engine tree on the reference machine is a
/// clone, and an unpacked zip is a perfectly good engine.
const ENGINE_MARKER: &str = "webui/Irodori-TTS/irodori_tts/inference_runtime.py";

const PROBE_SOURCE: &str = "import json,sys,torch;print(json.dumps({'python':'.'.join(map(str,sys.version_info[:3])),'torch':torch.__version__,'cudaAvailable':torch.cuda.is_available(),'cudaVersion':torch.version.cuda}))";

/// A cached interpreter answer, keyed by the interpreter it came from.
#[derive(Clone, Debug)]
pub struct Probe {
    python: PathBuf,
    ok: bool,
    cuda: Option<String>,
}

/// The keys of `runtime.json` that say where the engine lives. Unknown keys are
/// ignored rather than rejected: this app must not fail to describe an install
/// because the runtime learned a new setting.
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RuntimeFile {
    tts_python: Option<PathBuf>,
    tts_root: Option<PathBuf>,
    hf_home: Option<PathBuf>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProbeOutput {
    python: String,
    torch: String,
    cuda_available: bool,
    cuda_version: Option<String>,
}

#[tauri::command]
pub async fn detect(app: AppHandle) -> Inventory {
    // A blocking thread: this walks directories and may run an interpreter, and
    // neither belongs on an async worker.
    let worker = {
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || inventory(&app))
    };
    match worker.await {
        Ok(inventory) => inventory,
        Err(err) => {
            let host = app.state::<Host>();
            host.log(&format!("detect failed: {err}"));
            unknown(&host)
        }
    }
}

fn inventory(app: &AppHandle) -> Inventory {
    let host = app.state::<Host>();
    let runtime_file = read_runtime_file(&host);

    let engine_root = resolve_engine_root(&host, &runtime_file);
    // Where to look for an interpreter when no engine tree has been validated:
    // the layout bootstrap would create.
    let engine_dir = engine_root
        .clone()
        .unwrap_or_else(|| host.root.join("runtime/engine"));

    let engine_python = resolve_python(&runtime_file, &host.root, &engine_dir);
    let probe = engine_python
        .as_deref()
        .map(|python| cached_probe(&host, python));

    let hf_cache = resolve_hf_cache(&host, &runtime_file);
    let models = model_states(hf_cache.as_deref());

    let python_ok = probe.as_ref().is_some_and(|probe| probe.ok);
    let missing_bytes: u64 = MODELS
        .iter()
        .zip(&models)
        .filter(|(_, state)| !state.present)
        .map(|((_, _, bytes), _)| *bytes)
        .sum();
    let needs_bytes = missing_bytes + if python_ok { 0 } else { ENV_BYTES };

    Inventory {
        engine_root: engine_root.map(display),
        engine_python: engine_python.map(display),
        python_ok,
        cuda: probe.and_then(|probe| probe.cuda),
        hf_cache: hf_cache.map(display),
        models,
        packs: config_edit::read_packs(&host),
        runtime_json: Some(display(host.data_dir.join("runtime.json"))),
        disk_free_gib: gib(disk_free_bytes(&host.root).unwrap_or(0)),
        needs_gib: gib(needs_bytes),
    }
}

/// Everything unknown, but still carrying the one path the frontend derives the
/// data dir and the install root from.
fn unknown(host: &Host) -> Inventory {
    Inventory {
        engine_root: None,
        engine_python: None,
        python_ok: false,
        cuda: None,
        hf_cache: None,
        models: model_states(None),
        packs: Vec::new(),
        runtime_json: Some(display(host.data_dir.join("runtime.json"))),
        disk_free_gib: 0.0,
        needs_gib: 0.0,
    }
}

fn read_runtime_file(host: &Host) -> RuntimeFile {
    let path = host.data_dir.join("runtime.json");
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return RuntimeFile::default();
    };
    match serde_json::from_str(&raw) {
        Ok(file) => file,
        Err(err) => {
            host.log(&format!("{} is not readable JSON: {err}", path.display()));
            RuntimeFile::default()
        }
    }
}

/// An engine root is reported only when the marker is present, because "found"
/// has to mean usable. A path that is configured but empty is logged instead —
/// bootstrap's preflight is the thing that explains it with a remedy.
fn resolve_engine_root(host: &Host, runtime_file: &RuntimeFile) -> Option<PathBuf> {
    let configured = runtime_file
        .tts_root
        .as_deref()
        .map(|path| layout::absolute(&host.root, path));
    let candidates: Vec<PathBuf> = configured
        .iter()
        .cloned()
        .chain([host.root.join("runtime/engine")])
        .collect();

    for candidate in &candidates {
        if candidate.join(ENGINE_MARKER).is_file() {
            return Some(candidate.clone());
        }
    }
    // Only the configured path is worth a line. `runtime/engine` exists and is
    // empty on every fresh install, and saying so on every detect() would fill
    // the log with the one fact the panel already shows.
    if let Some(configured) = configured {
        host.log(&format!(
            "runtime.json points ttsRoot at {} but there is no {ENGINE_MARKER} there",
            configured.display()
        ));
    }
    None
}

/// Interpreter candidates, in the order the reference machine makes true: the
/// configured one first, then `env/` beside the engine (what the engine's own
/// `setup_env.ps1` creates), then a venv inside the engine repo, then a portable
/// interpreter shipped in the tree.
fn resolve_python(runtime_file: &RuntimeFile, root: &Path, engine_dir: &Path) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(configured) = runtime_file.tts_python.as_deref() {
        candidates.push(layout::absolute(root, configured));
    }
    candidates.push(engine_dir.join("env/Scripts/python.exe"));
    candidates.push(engine_dir.join("webui/Irodori-TTS/.venv/Scripts/python.exe"));
    candidates.push(root.join("runtime/python/Scripts/python.exe"));
    candidates.push(root.join("runtime/python/python.exe"));
    candidates.into_iter().find(|path| path.is_file())
}

fn resolve_hf_cache(host: &Host, runtime_file: &RuntimeFile) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(configured) = runtime_file.hf_home.as_deref() {
        candidates.push(layout::absolute(&host.root, configured));
    }
    candidates.push(host.root.join("models/huggingface"));
    candidates.into_iter().find(|path| path.is_dir())
}

/// The directories `open_path` is allowed to reach.
///
/// Every one of them is either part of the install tree or named by
/// `runtime.json`, which bootstrap writes and no command in this app exposes to
/// the frontend. That is what makes widening the sandbox with the engine root and
/// the model cache safe while a voice pack's path — a string the frontend itself
/// can store — deliberately widens nothing.
pub fn trusted_roots(host: &Host) -> Vec<PathBuf> {
    let runtime_file = read_runtime_file(host);
    let mut roots = vec![host.root.clone(), host.data_dir.clone()];
    roots.extend(resolve_engine_root(host, &runtime_file));
    roots.extend(resolve_hf_cache(host, &runtime_file));
    roots
}

fn model_states(hf_cache: Option<&Path>) -> Vec<ModelState> {
    MODELS
        .iter()
        .map(|(repo, payload, bytes)| ModelState {
            repo: (*repo).to_string(),
            present: hf_cache.is_some_and(|cache| has_payload(cache, repo, payload)),
            gib: gib(*bytes),
        })
        .collect()
}

/// `<cache>/hub/models--<org>--<name>/snapshots/<revision>/<payload>`.
///
/// Any revision counts: the cache is content-addressed, so a second revision is a
/// second snapshot directory rather than a replacement. The snapshot entry is an
/// NTFS reparse point into `blobs/`, which is why this asks for `metadata` —
/// `symlink_metadata` reports a zero-byte file and would call a finished download
/// missing.
fn has_payload(cache: &Path, repo: &str, payload: &str) -> bool {
    let snapshots = cache
        .join("hub")
        .join(format!("models--{}", repo.replace('/', "--")))
        .join("snapshots");
    let Ok(entries) = std::fs::read_dir(snapshots) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        std::fs::metadata(entry.path().join(payload))
            .map(|meta| meta.is_file() && meta.len() > 0)
            .unwrap_or(false)
    })
}

fn cached_probe(host: &Host, python: &Path) -> Probe {
    {
        let cache = host.probe.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(cached) = cache.as_ref() {
            if cached.python == python {
                return cached.clone();
            }
        }
    }
    let probe = run_probe(host, python);
    *host.probe.lock().unwrap_or_else(|err| err.into_inner()) = Some(probe.clone());
    probe
}

fn run_probe(host: &Host, python: &Path) -> Probe {
    let failed = Probe {
        python: python.to_path_buf(),
        ok: false,
        cuda: None,
    };

    let mut command = Command::new(python);
    command
        .arg("-c")
        .arg(PROBE_SOURCE)
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    // stderr to a file, never a pipe. Importing torch prints warnings, and a
    // second pipe nobody drains while waiting on the first is exactly how this
    // deadlocks. The file also keeps the traceback when the import fails, which is
    // the only useful thing about a broken environment.
    match host.child_log("probe.err.log") {
        Ok(file) => {
            command.stderr(Stdio::from(file));
        }
        Err(_) => {
            command.stderr(Stdio::null());
        }
    }
    hidden(&mut command);

    let Ok(mut child) = command.spawn() else {
        host.log(&format!("could not run {}", python.display()));
        return failed;
    };

    // Bounded, because a half-installed environment can hang on import and the
    // panel must not hang with it. Generous, because a cold torch import is
    // measured in seconds: 5.2 s on the reference machine.
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            host.log(&format!("interpreter probe timed out: {}", python.display()));
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Safe to read stdout only after exit: the probe prints one short JSON line,
    // orders of magnitude below the pipe buffer.
    let Ok(output) = child.wait_with_output() else {
        return failed;
    };
    let text = String::from_utf8_lossy(&output.stdout);
    let Some(line) = text.lines().find(|line| line.trim_start().starts_with('{')) else {
        host.log(&format!(
            "{} did not answer the probe; see logs\\probe.err.log",
            python.display()
        ));
        return failed;
    };
    match serde_json::from_str::<ProbeOutput>(line.trim()) {
        Ok(probe) => {
            host.log(&format!(
                "interpreter {} python {} torch {} cuda {}",
                python.display(),
                probe.python,
                probe.torch,
                probe.cuda_version.as_deref().unwrap_or("none")
            ));
            Probe {
                python: python.to_path_buf(),
                ok: true,
                // Reported only when torch can actually reach the GPU: a CUDA
                // version string from a build that cannot see a device reads as
                // "ready" and would be a lie.
                cuda: probe.cuda_version.filter(|_| probe.cuda_available),
            }
        }
        Err(err) => {
            host.log(&format!("probe output unparseable: {err}"));
            failed
        }
    }
}

#[cfg(windows)]
fn disk_free_bytes(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let mut wide: Vec<u16> = path.as_os_str().encode_wide().collect();
    wide.push(0);
    let mut free: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut free,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    (ok != 0).then_some(free)
}

#[cfg(not(windows))]
fn disk_free_bytes(_path: &Path) -> Option<u64> {
    None
}

/// Two decimals: this is a number a human reads next to a disk-space figure, not
/// an accounting quantity.
fn gib(bytes: u64) -> f64 {
    (bytes as f64 / GIB * 100.0).round() / 100.0
}

fn display(path: PathBuf) -> String {
    path.display().to_string()
}
