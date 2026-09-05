//! LoRA training, driven from the panel: the pipeline in `scripts/training/` run as one job.
//!
//! Six steps, in order, each a script that already exists and already knows how to do its
//! job — dataset, latents, train, samples, score, and then install as a separate, explicit
//! act. Nothing here reimplements any of them. What this owns is the four things that are
//! not any single step's business:
//!
//! * the scratch directory the whole run lives in, one per voice under the data dir,
//! * the four knobs, written into a COPY of the shipped `lora.yaml` (the template is read
//!   only, and everything else in it is frozen for reasons its own comments give),
//! * the GPU handover: `POST /api/sleep` before the first step that needs the card, because
//!   the engine and the trainer both want about two gigabytes and only one of them can have
//!   them,
//! * relaying each step's progress stream, unparsed, on `train://event`.
//!
//! Progress is emitted where the truth is. `run_training.py` parses upstream's tqdm bar
//! next to the process that draws it; this file forwards lines and counts nothing.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter, Manager};

use crate::contract::{
    Checkpoint, EVENT_TRAIN, InstallRequest, TrainRequest, TrainingPreflight, TrainingResult,
};
use crate::host::{hidden, Host};
use crate::jsonstream::{self, Outcome, Spec};
use crate::layout;
use crate::runtime_api;

/// The config the run is derived from. Read, never written: its comments are the only
/// documentation of why each frozen value is what it is.
const TEMPLATE: &str = "scripts/training/irodori/lora.yaml";

/// The record of which checkpoints of a run became packs, inside that run's scratch
/// directory. Named once, read by `at_risk` and written by `record_installed`.
const INSTALLED: &str = "installed.txt";

/// `lora.yaml`'s own warmup, which the derived `stable_steps` has to leave room for.
const WARMUP_STEPS: u32 = 100;

/// One interpreter, one line, no imports that are not being tested. `mem_get_info` is the
/// only part that touches the GPU: it creates a CUDA context (a few hundred MiB, released on
/// exit) and it is the only way to answer "is there room to train" with a number instead of
/// a guess.
const PROBE_SOURCE: &str = "import json,importlib.util as u;mods=['torch','datasets','peft','soundfile','resemblyzer','yaml'];missing=[m for m in mods if u.find_spec(m) is None];torch=__import__('torch') if 'torch' not in missing else None;ok=bool(torch) and bool(torch.cuda.is_available());free,total=torch.cuda.mem_get_info(0) if ok else (None,None);print(json.dumps({'missing':missing,'cudaVersion':torch.version.cuda if torch else None,'cudaAvailable':ok,'gpuName':torch.cuda.get_device_name(0) if ok else None,'freeMib':free>>20 if ok else None,'totalMib':total>>20 if ok else None}))";

#[tauri::command]
pub async fn training_preflight(app: AppHandle) -> TrainingPreflight {
    let (python, running, pack_id) = {
        let host = app.state::<Host>();
        let run = jsonstream::lock(&host.training);
        (resolve_python(&host), run.busy(), run.label())
    };

    let status = runtime_api::runtime_status(app.clone()).await;
    let model_loaded = status
        .body
        .as_ref()
        .and_then(|body| body.pointer("/worker/modelLoaded"))
        .and_then(Value::as_bool)
        .unwrap_or(false);

    let mut answer = TrainingPreflight {
        python: python.as_deref().map(text),
        missing: Vec::new(),
        cuda: None,
        gpu_name: None,
        vram_free_mib: None,
        vram_total_mib: None,
        runtime_reachable: status.reachable,
        model_loaded,
        running,
        pack_id,
        blockers: Vec::new(),
    };

    if running {
        // The probe imports torch and opens a CUDA context. Doing that beside a live run
        // would take memory from it to answer a question whose answer is already yes.
        answer
            .blockers
            .push("a training run is already in progress".to_string());
        return answer;
    }

    let Some(python) = python else {
        answer.blockers.push(
            "no Python interpreter for training: provision one on the deployment screen, or \
             point runtime.json's ttsPython at an existing one"
                .to_string(),
        );
        return answer;
    };

    let probe = {
        let app = app.clone();
        let python = python.clone();
        tauri::async_runtime::spawn_blocking(move || probe(&app.state::<Host>(), &python))
            .await
            .unwrap_or(None)
    };
    let Some(probe) = probe else {
        answer.blockers.push(format!(
            "{} did not answer the dependency probe; see logs\\training-probe.err.log",
            python.display()
        ));
        return answer;
    };

    answer.missing = probe.missing;
    answer.cuda = probe.cuda_version;
    answer.gpu_name = probe.gpu_name;
    answer.vram_free_mib = probe.free_mib;
    answer.vram_total_mib = probe.total_mib;

    if !answer.missing.is_empty() {
        answer.blockers.push(format!(
            "{} cannot import {}: install them into that interpreter (uv pip install --python \
             \"{}\" resemblyzer webrtcvad-wheels covers the scoring half)",
            python.display(),
            answer.missing.join(", "),
            python.display()
        ));
    }
    if !probe.cuda_available {
        answer.blockers.push(
            "torch cannot see a CUDA GPU: the Irodori backend trains on CUDA and has no CPU \
             path"
                .to_string(),
        );
    }
    answer
}

#[tauri::command]
pub async fn start_training(app: AppHandle, req: TrainRequest) -> Result<(), String> {
    validate(&req)?;

    let (python, paths, template) = {
        let host = app.state::<Host>();
        let python = resolve_python(&host).ok_or_else(|| {
            "no Python interpreter for training: provision one first, or point runtime.json's \
             ttsPython at an existing one"
                .to_string()
        })?;
        let template = host.root.join(TEMPLATE);
        if !template.is_file() {
            return Err(format!(
                "{} is missing — this does not look like a voice-core install",
                template.display()
            ));
        }
        (python, Paths::new(&host, &req.pack_id), template)
    };

    // An hour of GPU time is not something to delete because a form was submitted. The run is
    // refused, by name and by count, until the user says those checkpoints are expendable.
    // Before the claim, so a refusal leaves the slot free.
    let risked = at_risk(&paths.lora, &paths.installed);
    if !risked.is_empty() && !req.overwrite {
        return Err(overwrite_refusal(&paths.dir, risked.len()));
    }

    {
        let host = app.state::<Host>();
        if !jsonstream::lock(&host.training).claim(Some(req.pack_id.clone())) {
            return Err("a training run is already in progress".to_string());
        }
    }

    let prepared = prepare(&paths, &template, &req);
    let result = match prepared {
        Ok(cleared) => {
            announce(&app, &paths, &req, cleared);
            pipeline(&app, &python, &paths, &req).await
        }
        Err(err) => Err(err),
    };

    {
        let host = app.state::<Host>();
        jsonstream::lock(&host.training).release();
    }
    result
}

/// Kill the run.
///
/// The job object is what makes this complete: upstream's trainer spawns DataLoader worker
/// processes, each a fresh interpreter holding ~700 MB, and they are in the job with their
/// parent. Killing the Python PID alone would leave them resident with nobody reading them.
#[tauri::command]
pub async fn cancel_training(app: AppHandle) {
    let host = app.state::<Host>();
    if jsonstream::cancel(&host.training) {
        host.log("training cancelled");
    }
}

/// Install one chosen checkpoint as a voice pack.
///
/// Separate from the run on purpose: training produces several candidates and picking one is
/// a decision, not a step. `install_pack.py` does the copying and the surgical `config.json`
/// edit; nothing here touches either.
#[tauri::command]
pub async fn install_trained_pack(app: AppHandle, req: InstallRequest) -> Result<(), String> {
    validate_id(&req.pack_id)?;
    let checkpoint = PathBuf::from(req.checkpoint.trim());
    if !checkpoint.join("adapter_config.json").is_file() {
        return Err(format!(
            "{} is not a LoRA adapter directory: an adapter is adapter_config.json plus its \
             weights",
            checkpoint.display()
        ));
    }

    let (python, root, data_dir) = {
        let host = app.state::<Host>();
        let python = resolve_python(&host)
            .ok_or_else(|| "no Python interpreter for the installer".to_string())?;
        if !jsonstream::lock(&host.training).claim(Some(req.pack_id.clone())) {
            return Err("a training run is already in progress".to_string());
        }
        (python, host.root.clone(), host.data_dir.clone())
    };

    let mut args = vec![
        "--json".to_string(),
        "--pack".to_string(),
        text(&checkpoint),
        "--id".to_string(),
        req.pack_id.clone(),
        "--data-dir".to_string(),
        text(&data_dir),
        // The user chose this id in a form that showed them the packs that already exist,
        // so a second install under the same name is a replacement, not an accident.
        "--force".to_string(),
    ];
    let name = req.display_name.trim();
    if !name.is_empty() {
        args.push("--name".to_string());
        args.push(name.to_string());
    }
    for (flag, value) in [("--character", &req.character), ("--avatar", &req.avatar)] {
        if let Some(value) = value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
            args.push(flag.to_string());
            args.push(value.to_string());
        }
    }

    let step = StepPlan {
        stage: "install",
        label: "install_pack.py",
        script: "scripts/training/install_pack.py",
        args,
        gpu: false,
    };
    let outcome = {
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            one_step(&app, &python, &root, &step)
        })
        .await
        .map_err(|err| err.to_string())
    };

    {
        let host = app.state::<Host>();
        jsonstream::lock(&host.training).release();
    }
    let finished = outcome??;
    // Only a real install counts. A step that reported `fail` explained itself on the stream
    // and copied nothing, so recording it would license deleting a checkpoint that is still
    // the only copy of this voice.
    if !finished.failed && !finished.cancelled {
        record_installed(&checkpoint);
    }
    Ok(())
}

/// Note that a pack was installed from this checkpoint, in the scratch directory of the run
/// that produced it.
///
/// Derived from the checkpoint's own path rather than from the request's `pack_id`, because a
/// user may install under a different id than the one they trained under, and the record
/// belongs to the RUN.
fn record_installed(checkpoint: &Path) {
    let Some(record) = checkpoint
        .parent()
        .and_then(Path::parent)
        .map(|scratch| scratch.join(INSTALLED))
    else {
        return;
    };
    let name = checkpoint
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let existing = std::fs::read_to_string(&record).unwrap_or_default();
    if existing.lines().any(|line| line.trim() == name) {
        return;
    }
    // Best effort: a record this cannot write only makes the next run ask before deleting.
    let _ = std::fs::write(&record, format!("{existing}{name}\n"));
}

/// The checkpoints in `lora/` that no pack has been installed from.
///
/// Conservative by construction: one installed by hand, or registered through the 音色
/// screen, is not in the record and still counts. Asking too often costs a tick of a box;
/// asking too rarely costs the run.
fn at_risk(lora: &Path, record: &Path) -> Vec<String> {
    let noted = std::fs::read_to_string(record).unwrap_or_default();
    let installed: Vec<&str> = noted
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let Ok(entries) = std::fs::read_dir(lora) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter(|entry| entry.path().join("adapter_config.json").is_file())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| !installed.contains(&name.as_str()))
        .collect()
}

/// The refusal, in the words the user reads. Here rather than inline at the one call site
/// that is a `#[tauri::command]`, so the sentence can be asserted without an app handle.
fn overwrite_refusal(dir: &Path, count: usize) -> String {
    format!(
        "{} holds {count} checkpoint(s) that no pack was installed from, and starting again \
         deletes them. Install the one you want first, or tick the overwrite box to start \
         over without it.",
        dir.display()
    )
}

/// What a run left on disk. Read-only, and answers for a pack that was never trained.
#[tauri::command]
pub async fn training_result(app: AppHandle, pack_id: String) -> Result<TrainingResult, String> {
    validate_id(&pack_id)?;
    let paths = {
        let host = app.state::<Host>();
        Paths::new(&host, &pack_id)
    };
    Ok(TrainingResult {
        dir: text(&paths.dir),
        exists: paths.dir.is_dir(),
        qa: read_json(&paths.qa),
        request: read_json(&paths.request),
        checkpoints: checkpoints(&paths, &pack_id),
        at_risk: at_risk(&paths.lora, &paths.installed).len(),
    })
}

// ------------------------------------------------------------------------------ the run --

/// One step of the pipeline, as data, so the whole plan can be built and read at once.
struct StepPlan {
    stage: &'static str,
    /// What the step is called in an error a user reads.
    label: &'static str,
    /// Relative to the install root, so one plan works in a checkout and in a package.
    script: &'static str,
    args: Vec<String>,
    /// Needs the card. The first of these is where the engine is asked to let go of it.
    gpu: bool,
}

/// The five steps, in order. Install is deliberately absent: it is a separate command
/// because choosing a checkpoint is a decision the user makes after seeing the scores.
fn plan(paths: &Paths, req: &TrainRequest) -> Vec<StepPlan> {
    let mut dataset = vec![
        "--json".to_string(),
        // A folder picker points at a corpus root, and a corpus arranged in subfolders is
        // still one corpus.
        "--recursive".to_string(),
        "--audio-dir".to_string(),
        req.audio_dir.trim().to_string(),
        "--speaker-id".to_string(),
        req.speaker_id.trim().to_string(),
        "--out-dataset".to_string(),
        text(&paths.dataset),
    ];
    if let Some(transcripts) = req.transcripts.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        dataset.push("--transcripts".to_string());
        dataset.push(transcripts.to_string());
    }

    vec![
        StepPlan {
            stage: "dataset",
            label: "prepare_dataset.py",
            script: "scripts/training/irodori/prepare_dataset.py",
            args: dataset,
            gpu: false,
        },
        StepPlan {
            stage: "latents",
            label: "encode_latents.py",
            script: "scripts/training/irodori/encode_latents.py",
            args: vec![
                "--json".to_string(),
                "--dataset-file".to_string(),
                text(&paths.dataset),
                "--latent-dir".to_string(),
                text(&paths.latents),
                "--out-manifest".to_string(),
                text(&paths.manifest),
            ],
            gpu: true,
        },
        StepPlan {
            stage: "train",
            label: "run_training.py",
            script: "scripts/training/irodori/run_training.py",
            args: vec![
                "--json".to_string(),
                "--config".to_string(),
                text(&paths.config),
                "--manifest".to_string(),
                text(&paths.manifest),
                "--output-dir".to_string(),
                text(&paths.lora),
            ],
            gpu: true,
        },
        StepPlan {
            stage: "samples",
            label: "generate_samples.py",
            script: "scripts/training/irodori/generate_samples.py",
            args: vec![
                "--json".to_string(),
                "--lora".to_string(),
                text(&paths.lora),
                "--out-dir".to_string(),
                text(&paths.samples),
            ],
            gpu: true,
        },
        StepPlan {
            stage: "score",
            label: "evaluate_similarity.py",
            script: "scripts/training/irodori/evaluate_similarity.py",
            args: vec![
                "--json".to_string(),
                "--label".to_string(),
                req.pack_id.clone(),
                // The corpus is its own ceiling: what "the same human twice" scores is the
                // only honest upper bound for what was generated from it.
                "--ref-dir".to_string(),
                req.audio_dir.trim().to_string(),
                "--tests".to_string(),
                text(&paths.samples.join("*.wav")),
                "--out-dir".to_string(),
                text(&paths.score),
            ],
            // The d-vector encoder runs on the CPU by the step's own default, which is what
            // leaves the card to the trainer.
            gpu: false,
        },
    ]
}

async fn pipeline(
    app: &AppHandle,
    python: &Path,
    paths: &Paths,
    req: &TrainRequest,
) -> Result<(), String> {
    let mut released = false;
    for step in plan(paths, req) {
        // A step boundary is where cancellation is noticed. Killing the child is what stops
        // the step it was in; this is what stops the next one from starting.
        {
            let host = app.state::<Host>();
            if jsonstream::lock(&host.training).cancelled() {
                return Ok(());
            }
        }
        if step.gpu && !released {
            released = true;
            let message = release_gpu(app).await;
            let _ = app.emit(EVENT_TRAIN, jsonstream::log_event(step.stage, &message));
        }

        // One blocking thread per step rather than one for the whole pipeline: the reading
        // loop blocks, but the handover between steps is an await, and this way it can be.
        let outcome = {
            let app = app.clone();
            let python = python.to_path_buf();
            let root = paths.root.clone();
            tauri::async_runtime::spawn_blocking(move || one_step(&app, &python, &root, &step))
                .await
                .map_err(|err| err.to_string())??
        };
        // A step that failed has already said so on the stream, with the remedy its own
        // author wrote. Repeating it as a rejected call would put the same sentence in a
        // toast; stopping here is the whole response.
        if outcome.cancelled || outcome.failed {
            return Ok(());
        }
    }
    Ok(())
}

fn one_step(
    app: &AppHandle,
    python: &Path,
    root: &Path,
    step: &StepPlan,
) -> Result<Outcome, String> {
    let host = app.state::<Host>();
    let mut command = Command::new(python);
    command
        .arg(root.join(step.script))
        .args(&step.args)
        .current_dir(root);
    // Per stage, and truncated per run by `jsonstream`: each file is the human transcript of
    // one step, which is exactly what is worth reading when that step is the one that failed.
    let stderr_log = format!("train-{}.err.log", step.stage);
    let spec = Spec {
        event: EVENT_TRAIN,
        label: step.label,
        stderr_log: &stderr_log,
        first_stage: step.stage,
    };
    host.log(&format!("training step {}: {}", step.stage, step.label));
    jsonstream::run(app, &spec, command, &host.training)
}

/// Ask the runtime to let go of the GPU before the first step that needs it.
///
/// `POST /api/sleep` is the documented way (`src/api.rs`): on this build it stops the engine
/// worker outright rather than merely unloading, which is more than enough — the trainer
/// needs about 14 GiB at batch 16 and the engine holds ~1.9 GiB of model plus its reserved
/// pool. The cost is real and belongs in the log: the runtime's next utterance repays the
/// process start and the torch import as well as the model load.
///
/// It cannot prevent a caller from waking the engine again mid-training. Nothing here can —
/// there is one card and two programs. What it can do is not start the fight, and say so.
async fn release_gpu(app: &AppHandle) -> String {
    let host = app.state::<Host>();
    let Some(token) = host.token() else {
        return "no runtime token yet, so nothing is holding the GPU".to_string();
    };
    let url = format!("{}/api/sleep", host.base_url);
    match host.http.post(url).bearer_auth(token).send().await {
        Ok(response) if response.status().is_success() => "asked the runtime to release the GPU \
             (POST /api/sleep); its next utterance pays the engine start and the model load again"
            .to_string(),
        Ok(response) => format!(
            "the runtime would not release the GPU: it answered HTTP {}",
            response.status().as_u16()
        ),
        Err(_) => "the runtime is not running, so nothing else is holding the GPU".to_string(),
    }
}


// -------------------------------------------------------------------- the scratch tree --

/// Where one voice's run lives. Under the data dir, because latents and checkpoints are
/// large, regenerable, and not something anyone should be backing up.
struct Paths {
    root: PathBuf,
    dir: PathBuf,
    dataset: PathBuf,
    /// Where `prepare_dataset.py` puts its QA report by default: `<out-dataset>.qa.json`.
    qa: PathBuf,
    latents: PathBuf,
    manifest: PathBuf,
    config: PathBuf,
    lora: PathBuf,
    samples: PathBuf,
    score: PathBuf,
    request: PathBuf,
    /// Which checkpoints of this run a pack has been installed from, one name per line.
    /// `install_pack.py` records nothing about where a pack came from - the manifest
    /// describes the voice, not its provenance - so this is the only thing that can tell an
    /// hour of GPU time nobody kept from one that is already a voice pack.
    installed: PathBuf,
}

impl Paths {
    fn new(host: &Host, pack_id: &str) -> Self {
        let dir = host.data_dir.join("cache").join("train").join(pack_id);
        Self {
            root: host.root.clone(),
            dataset: dir.join("dataset.jsonl"),
            qa: dir.join("dataset.jsonl.qa.json"),
            latents: dir.join("latents"),
            manifest: dir.join("train_manifest.jsonl"),
            config: dir.join("lora.yaml"),
            lora: dir.join("lora"),
            samples: dir.join("samples"),
            score: dir.join("score"),
            request: dir.join("request.json"),
            installed: dir.join(INSTALLED),
            dir,
        }
    }
}

/// Make the scratch directory, write the config and record the request. Returns whether a
/// previous run was removed.
///
/// Removed rather than merged: a second run of the same voice would otherwise leave the
/// first run's checkpoints beside the new ones, and the results table would offer a mixture
/// of two runs as if it were one. What may be removed is not this function's judgement -
/// `start_training` has already refused the call unless every checkpoint here is either a
/// pack already or explicitly expendable.
fn prepare(paths: &Paths, template: &Path, req: &TrainRequest) -> Result<bool, String> {
    let cleared = paths.dir.exists();
    if cleared {
        std::fs::remove_dir_all(&paths.dir)
            .map_err(|err| format!("could not clear {}: {err}", paths.dir.display()))?;
    }
    std::fs::create_dir_all(&paths.dir)
        .map_err(|err| format!("could not create {}: {err}", paths.dir.display()))?;

    let source = std::fs::read_to_string(template)
        .map_err(|err| format!("could not read {}: {err}", template.display()))?;
    std::fs::write(&paths.config, render_config(&source, req))
        .map_err(|err| format!("could not write {}: {err}", paths.config.display()))?;

    let recorded = serde_json::to_string_pretty(req).map_err(|err| err.to_string())?;
    std::fs::write(&paths.request, recorded)
        .map_err(|err| format!("could not write {}: {err}", paths.request.display()))?;
    Ok(cleared)
}

/// The shipped template with the four exposed knobs replaced and nothing else touched.
///
/// Line-wise, not parse-and-rewrite: every value in that file has a comment above it saying
/// why it is what it is, and a YAML round-trip would delete all of them. Same argument
/// `install_pack.py` makes about `config.json`.
///
/// `stable_steps` is not a knob but it moves anyway, because the template says it must:
/// warmup 100 + stable 1500 is what leaves 400 steps of decay in a 2000-step budget, and a
/// `stable_steps` at or above `max_steps` is a schedule that never decays at all.
fn render_config(template: &str, req: &TrainRequest) -> String {
    let decay = (req.max_steps / 5).max(1);
    let stable = req.max_steps.saturating_sub(WARMUP_STEPS + decay);

    let mut out = String::with_capacity(template.len() + 64);
    let mut in_train = false;
    for line in template.lines() {
        // A top-level key. `model:` describes the checkpoint's shape and nothing in it is
        // ours to touch, so the substitutions below only apply under `train:`.
        if !line.starts_with(char::is_whitespace) && !line.trim_start().starts_with('#') && !line.trim().is_empty() {
            in_train = line.trim_end() == "train:";
        }
        match in_train.then(|| substitute(line, req, stable)).flatten() {
            Some(replaced) => out.push_str(&replaced),
            None => out.push_str(line),
        }
        out.push('\n');
    }
    out
}

fn substitute(line: &str, req: &TrainRequest, stable: u32) -> Option<String> {
    let body = line.trim_start();
    let key = body.split(':').next()?;
    let value = match key {
        "batch_size" => req.batch_size.to_string(),
        "max_steps" => req.max_steps.to_string(),
        // Rust's f64 never prints as `1e-4`, which matters: YAML 1.1 reads an exponent
        // without a decimal point as a STRING, and PyYAML would hand the trainer "1e-4".
        "learning_rate" => req.learning_rate.to_string(),
        "save_every" => req.save_every.to_string(),
        "stable_steps" => stable.to_string(),
        _ => return None,
    };
    Some(format!("{}{key}: {value}", &line[..line.len() - body.len()]))
}

/// The three things about this run that are decisions rather than measurements, said once at
/// the top of the stream so the console is a record of what was actually run.
fn announce(app: &AppHandle, paths: &Paths, req: &TrainRequest, cleared: bool) {
    let mut lines = vec![format!("scratch directory {}", paths.dir.display())];
    if cleared {
        lines.push(format!(
            "removed the previous run for {}: its checkpoints are gone unless they were installed",
            req.pack_id
        ));
    }
    lines.push(format!(
        "batch {}, {} steps, learning rate {}, checkpoint every {} -> {}",
        req.batch_size,
        req.max_steps,
        req.learning_rate,
        req.save_every,
        paths.config.display()
    ));
    for line in lines {
        let _ = app.emit(EVENT_TRAIN, jsonstream::log_event("dataset", &line));
    }
}

// -------------------------------------------------------------------------- the results --

fn checkpoints(paths: &Paths, pack_id: &str) -> Vec<Checkpoint> {
    let scores = read_scores(&paths.score.join(format!("{pack_id}.json")));
    let Ok(entries) = std::fs::read_dir(&paths.lora) else {
        return Vec::new();
    };

    let mut items: Vec<Checkpoint> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        // An adapter is `adapter_config.json` plus its weights (`irodori_tts/lora.py`).
        // Everything else under the output directory is trainer state, not a pack.
        if !path.join("adapter_config.json").is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let (step, val_loss) = parse_checkpoint(&name);
        // `generate_samples.py` names a condition `lora_<adapter directory>`, which is what
        // ties a score back to the checkpoint it came from.
        let scored = scores.iter().find(|(group, _, _)| *group == format!("lora_{name}"));
        items.push(Checkpoint {
            name,
            path: text(&path),
            step,
            val_loss,
            lower_bound: scored.map(|(_, lower, _)| *lower),
            mean: scored.map(|(_, _, mean)| *mean),
            best: false,
        });
    }

    // Lowest validation loss first: that is what the trainer's own best-checkpoint selection
    // means, and it is what the screen pre-selects. A checkpoint with no val loss sorts after
    // every one that has it, because there is nothing to select it on.
    items.sort_by(|a, b| {
        let left = a.val_loss.unwrap_or(f64::MAX);
        let right = b.val_loss.unwrap_or(f64::MAX);
        left.partial_cmp(&right)
            .unwrap_or(Ordering::Equal)
            .then_with(|| a.step.cmp(&b.step))
    });
    if let Some(first) = items.first_mut() {
        first.best = true;
    }
    items
}

/// `checkpoint_best_val_loss_0001000_0.885155` -> step 1000, loss 0.885155. A periodic
/// `checkpoint_0001000` has a step and no loss, which is exactly why it is not selected by
/// default.
fn parse_checkpoint(name: &str) -> (Option<u64>, Option<f64>) {
    if let Some(rest) = name.strip_prefix("checkpoint_best_val_loss_") {
        let mut parts = rest.splitn(2, '_');
        let step = parts.next().and_then(|part| part.parse().ok());
        let loss = parts.next().and_then(|part| part.parse().ok());
        return (step, loss);
    }
    (
        name.strip_prefix("checkpoint_")
            .and_then(|rest| rest.parse().ok()),
        None,
    )
}

/// `(group, lower_bound, mean)` out of the score stage's report.
fn read_scores(path: &Path) -> Vec<(String, f64, f64)> {
    let Some(report) = read_json(path) else {
        return Vec::new();
    };
    let Some(groups) = report.get("groups").and_then(Value::as_array) else {
        return Vec::new();
    };
    groups
        .iter()
        .filter_map(|group| {
            Some((
                group.get("group")?.as_str()?.to_string(),
                group.get("lower_bound")?.as_f64()?,
                group.get("mean")?.as_f64()?,
            ))
        })
        .collect()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

// ---------------------------------------------------------------- interpreter and input --

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ProbeOutput {
    missing: Vec<String>,
    cuda_version: Option<String>,
    cuda_available: bool,
    gpu_name: Option<String>,
    free_mib: Option<u64>,
    total_mib: Option<u64>,
}

/// The interpreter the steps run under, resolved the way the runtime resolves it:
/// `runtime.json`'s `ttsPython` first, because that is the one bootstrap wrote and the worker
/// is using, then the two places a packaged install puts one. A checkout has no
/// `runtime/python` at all, so honouring `runtime.json` is what makes training work outside
/// an install.
fn resolve_python(host: &Host) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    candidates.extend(runtime_python(host));
    candidates.push(host.root.join("runtime/python/Scripts/python.exe"));
    candidates.push(host.root.join("runtime/python/python.exe"));
    candidates.into_iter().find(|path| path.is_file())
}

/// `ttsPython` out of `<data dir>/runtime.json`. Unknown keys are ignored rather than
/// rejected: training must not stop working because the runtime learned a new setting.
fn runtime_python(host: &Host) -> Option<PathBuf> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct RuntimeFile {
        tts_python: Option<PathBuf>,
    }
    let raw = std::fs::read_to_string(host.data_dir.join("runtime.json")).ok()?;
    let file: RuntimeFile = serde_json::from_str(&raw).ok()?;
    file.tts_python
        .map(|path| layout::absolute(&host.root, &path))
}

fn probe(host: &Host, python: &Path) -> Option<ProbeOutput> {
    let mut command = Command::new(python);
    command
        .arg("-c")
        .arg(PROBE_SOURCE)
        .stdin(Stdio::null())
        .stdout(Stdio::piped());
    // stderr to a file, never a pipe: importing torch prints warnings, and a second pipe
    // nobody drains while waiting on the first is how this deadlocks. The file also keeps the
    // traceback, which is the only useful thing about a broken environment.
    match host.child_log("training-probe.err.log") {
        Ok(file) => {
            command.stderr(Stdio::from(file));
        }
        Err(_) => {
            command.stderr(Stdio::null());
        }
    }
    hidden(&mut command);

    let mut child = command.spawn().ok()?;
    // Bounded, because a half-installed environment can hang on import and the panel must not
    // hang with it. Generous, because a cold torch import plus a CUDA context is measured in
    // seconds.
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => break,
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            host.log(&format!("training probe timed out: {}", python.display()));
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }

    // Safe to read stdout only after exit: the probe prints one short JSON line, orders of
    // magnitude below the pipe buffer.
    let output = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    let line = text.lines().find(|line| line.trim_start().starts_with('{'))?;
    serde_json::from_str(line.trim()).ok()
}

/// Nothing the frontend sends is trusted: `pack_id` becomes a directory under the data dir,
/// and the knobs become a config the trainer runs for an hour.
fn validate(req: &TrainRequest) -> Result<(), String> {
    validate_id(&req.pack_id)?;

    let audio = Path::new(req.audio_dir.trim());
    if req.audio_dir.trim().is_empty() || !audio.is_dir() {
        return Err(format!(
            "the audio folder is not a directory: {}",
            audio.display()
        ));
    }
    if let Some(transcripts) = req
        .transcripts
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let path = Path::new(transcripts);
        if !path.exists() {
            return Err(format!("the transcript source does not exist: {transcripts}"));
        }
    }
    if !(1..=64).contains(&req.batch_size) {
        return Err("batch size must be between 1 and 64".to_string());
    }
    if !(100..=20_000).contains(&req.max_steps) {
        return Err("step budget must be between 100 and 20000".to_string());
    }
    if !(1e-6..=1e-2).contains(&req.learning_rate) {
        return Err("learning rate must be between 0.000001 and 0.01".to_string());
    }
    if req.save_every < 50 || req.save_every > req.max_steps {
        return Err("checkpoint interval must be at least 50 and no more than the step budget".to_string());
    }
    Ok(())
}

/// The same rule `install_pack.py` applies, checked here too: this is the side that turns the
/// string into a path.
fn validate_id(id: &str) -> Result<(), String> {
    let mut chars = id.chars();
    let ok = chars.next().is_some_and(|first| first.is_ascii_alphanumeric())
        && chars.all(|rest| rest.is_ascii_alphanumeric() || matches!(rest, '.' | '-' | '_'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "id {id:?} must start with a letter or digit and hold only letters, digits, dot, \
             dash and underscore: it becomes a directory name and an API identifier"
        ))
    }
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(batch_size: u32, max_steps: u32, learning_rate: f64, save_every: u32) -> TrainRequest {
        TrainRequest {
            audio_dir: String::new(),
            transcripts: None,
            speaker_id: String::new(),
            pack_id: "my-voice".to_string(),
            display_name: String::new(),
            character: None,
            avatar: None,
            batch_size,
            max_steps,
            learning_rate,
            save_every,
            overwrite: false,
        }
    }

    /// `manager/src-tauri` -> the repository, so the test reads the template that ships.
    fn template() -> String {
        let mut root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        root.pop();
        root.pop();
        std::fs::read_to_string(root.join(TEMPLATE)).expect("the shipped lora.yaml")
    }

    /// The four knobs move, `stable_steps` follows `max_steps`, and NOTHING else does -
    /// comments included, because they are the only record of why each frozen value is what
    /// it is.
    #[test]
    fn only_the_knobs_move() {
        let source = template();
        let rendered = render_config(&source, &request(8, 1000, 0.00005, 250));

        let before: Vec<&str> = source.lines().collect();
        let after: Vec<&str> = rendered.lines().collect();
        assert_eq!(before.len(), after.len(), "no line was added or dropped");
        let changed: Vec<(&str, &str)> = before
            .iter()
            .zip(&after)
            .filter(|(left, right)| left != right)
            .map(|(left, right)| (*left, *right))
            .collect();
        assert_eq!(
            changed,
            vec![
                ("  batch_size: 16", "  batch_size: 8"),
                ("  max_steps: 2000", "  max_steps: 1000"),
                ("  learning_rate: 0.0001", "  learning_rate: 0.00005"),
                // 1000 - warmup 100 - decay 200.
                ("  stable_steps: 1500", "  stable_steps: 700"),
                ("  save_every: 500", "  save_every: 250"),
            ]
        );
    }

    /// Rendering the defaults reproduces the template line for line. That is what says the
    /// substitution has no formatting of its own: a run left at 2000 steps trains on the file
    /// the comments describe.
    #[test]
    fn the_defaults_reproduce_the_template() {
        let source = template();
        let rendered = render_config(&source, &request(16, 2000, 0.0001, 500));
        assert_eq!(
            source.lines().collect::<Vec<&str>>(),
            rendered.lines().collect::<Vec<&str>>()
        );
    }

    /// A learning rate must reach the trainer as a number. YAML 1.1 reads an exponent without
    /// a decimal point as a string, so `1e-5` would arrive as text and `float(...)` would be
    /// the trainer's problem rather than this one.
    #[test]
    fn a_learning_rate_is_never_written_in_exponent_form() {
        let rendered = render_config(&template(), &request(16, 2000, 0.000001, 500));
        let line = rendered
            .lines()
            .find(|line| line.trim_start().starts_with("learning_rate:"))
            .expect("the knob is in the file");
        assert_eq!(line, "  learning_rate: 0.000001");
    }

    /// The refusal that keeps an hour of GPU time. A checkpoint no pack was installed from
    /// stops a second run of the same voice; installing it takes it out of the count; a
    /// directory that is not an adapter was never in it.
    #[test]
    fn a_checkpoint_no_pack_came_from_stops_the_next_run() {
        let dir = std::env::temp_dir().join("voice-core-training-at-risk");
        let lora = dir.join("lora");
        let record = dir.join(INSTALLED);
        let _ = std::fs::remove_dir_all(&dir);
        let best = "checkpoint_best_val_loss_0001000_0.885155";
        for name in [best, "checkpoint_best_val_loss_0000500_0.906366"] {
            std::fs::create_dir_all(lora.join(name)).expect("a temp checkpoint");
            std::fs::write(lora.join(name).join("adapter_config.json"), "{}").expect("its config");
        }
        // Trainer state, not a pack: nothing to protect.
        std::fs::create_dir_all(lora.join("trainer_state")).expect("a temp directory");

        let risked = at_risk(&lora, &record);
        assert_eq!(risked.len(), 2, "{risked:?}");
        assert_eq!(
            overwrite_refusal(&dir, risked.len()),
            format!(
                "{} holds 2 checkpoint(s) that no pack was installed from, and starting again \
                 deletes them. Install the one you want first, or tick the overwrite box to \
                 start over without it.",
                dir.display()
            )
        );

        // Installing one takes it out of the count - and proves the record's location is
        // derived from the checkpoint the same way `Paths` derives it.
        record_installed(&lora.join(best));
        assert_eq!(
            at_risk(&lora, &record),
            vec!["checkpoint_best_val_loss_0000500_0.906366".to_string()]
        );

        record_installed(&lora.join("checkpoint_best_val_loss_0000500_0.906366"));
        assert!(at_risk(&lora, &record).is_empty(), "both are packs now");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
