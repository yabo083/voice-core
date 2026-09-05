//! Training observability: an agent runs the pipeline in `scripts/training/`, and this is
//! what the panel knows about it.
//!
//! The six steps — dataset, latents, train, samples, score, install — are scripts that
//! already know how to do their jobs, and each one now writes its own record:
//! `--status-file` points it at `<data dir>\logs\training-<pack id>.status.json` and it
//! appends the event stream to the `training-<pack id>.jsonl` beside it
//! (`scripts/training/_layout.py`). Nothing in this file writes either one, and that is the
//! design rather than an omission: a run an agent started from a shell with this window
//! closed leaves exactly what a run started from this window would leave, so the 训练 screen
//! can show a run it knows nothing about.
//!
//! What this owns is reading — the runs on disk, the transcript from a byte offset, the
//! scratch tree measured — plus the two acts that are decisions rather than steps:
//! installing one chosen checkpoint as a voice pack, and deleting a finished run's files.

use std::cmp::Ordering;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::contract::{Checkpoint, EVENT_TRAIN, InstallRequest};
use crate::host::Host;
use crate::jsonstream::{self, Spec};
use crate::layout;

/// The record of which checkpoints of a run became packs, inside that run's scratch
/// directory. Named once, read by `at_risk` and written by `record_installed`.
const INSTALLED: &str = "installed.txt";

/// The installer, and the one child process this file still starts.
const INSTALLER: &str = "scripts/training/install_pack.py";

// -------------------------------------------------------------------------- the runs --

/// Every run this install has a record of, newest first, live ones before the rest.
///
/// The list comes from the log directory rather than from anything this process remembers:
/// `training-<id>.status.json` is written by whoever ran the steps, so enumerating those
/// files is what makes a run started outside the GUI visible in it. There is no "current
/// run" left for the panel to disagree with.
#[tauri::command]
pub async fn training_runs(app: AppHandle) -> Vec<RunStatus> {
    let dir = {
        let host = app.state::<Host>();
        layout::logs_dir(&host.data_dir)
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };

    let mut runs: Vec<RunStatus> = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let Some(id) = run_id(&name) else { continue };
        // The id becomes a directory name in every call the panel makes next, so it is
        // checked here, at the boundary, rather than trusted because it came off the disk.
        if validate_id(&id).is_err() {
            continue;
        }
        let Some(mut status) = read_status(&entry.path()) else {
            continue;
        };
        // The file's own `pack_id` is what its writer put there; the name is what the panel
        // will ask about next. They are the same string in every run either side wrote, and
        // when they are not, the name is the one that can be acted on.
        status.pack_id = id;
        resolve_live(&mut status);
        runs.push(status);
    }

    runs.sort_by(|left, right| {
        right
            .live
            .cmp(&left.live)
            .then_with(|| right.updated.cmp(&left.updated))
    });
    runs
}

/// `training-my-voice.status.json` -> `my-voice`, and `None` for anything else in the log
/// directory. The same rule `_layout.py::_run_stem` applies from the writing side.
fn run_id(file_name: &str) -> Option<String> {
    let stem = file_name.strip_suffix(".status.json")?;
    Some(stem.strip_prefix("training-")?.to_string())
}

/// Decide whether a file that claims a live run is telling the truth, and make it consistent
/// when it is not.
///
/// A step writes `live: false` as it exits, whatever ended it — a finished stage, a refusal,
/// a traceback, a Ctrl-C. What it cannot write is the case where it was killed outright, and
/// that is what `pid` answers for. This is the rule SKILL.md §6 gives an agent reading the
/// file by hand, applied here so the panel and the agent cannot disagree about the same
/// bytes.
///
/// Settled at the last event's timestamp rather than at now, because that is when the run
/// stopped: claiming it ran until somebody happened to open this window would be inventing
/// an hour of GPU time.
fn resolve_live(status: &mut RunStatus) {
    if status.live && !alive(status.pid) {
        let stopped = status.updated;
        settle(status, stopped);
    }
}

/// Is that process still running?
///
/// A pid the OS has reused answers `true` for the wrong process, which is why `updated` is
/// on screen beside it: a live run whose last event is ten minutes old is not running, and
/// that stays legible without this function being infallible.
#[cfg(windows)]
fn alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    if pid == 0 {
        return false;
    }
    // SAFETY: a failed open yields a null handle, which is checked; the exit code goes into
    // a local, and the handle is closed on the one path that has one.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return false;
        }
        // A process that has exited but is still open somewhere — the shell that started it
        // holding a handle is enough — is a handle this can open and a run that is over.
        let mut code: u32 = 0;
        let running = GetExitCodeProcess(handle, &mut code) != 0 && code == STILL_ACTIVE as u32;
        CloseHandle(handle);
        running
    }
}

/// Without a way to ask the OS, the file's own claim is the only answer there is.
#[cfg(not(windows))]
fn alive(_pid: u32) -> bool {
    true
}

// -------------------------------------------------------------------- the transcript --

/// At most this much of a transcript on a first read. A fifty-minute run's `.jsonl` is
/// megabytes of tqdm-derived progress lines, the console shows the last screenful of it, and
/// the file itself is one click away in the 产物文件 panel.
const TAIL_BYTES: u64 = 256 * 1024;

/// And at most this many lines per call, which is the console's own cap: a panel left on
/// another screen for an hour must not be handed fifty thousand list items to build.
const TAIL_LINES: usize = 2000;

/// The transcript from where the caller left off.
#[derive(Serialize)]
pub struct LogTail {
    /// Resume here next time. Always a line boundary, so a resumed read never starts mid
    /// object.
    offset: u64,
    /// Whole lines, verbatim, exactly as the step wrote them.
    lines: Vec<String>,
}

/// One run's transcript, forward from `offset`.
///
/// Incremental because it is polled: re-reading a megabyte a second to render the twenty
/// lines that changed would be the panel's whole CPU budget. `offset: 0` means "start me
/// off", and answers with the tail rather than with the whole file.
#[tauri::command]
pub async fn training_log(app: AppHandle, pack_id: String, offset: u64) -> Result<LogTail, String> {
    validate_id(&pack_id)?;
    let path = {
        let host = app.state::<Host>();
        transcript_path(&host, &pack_id)
    };
    Ok(tail(&path, offset))
}

fn tail(path: &Path, offset: u64) -> LogTail {
    let Ok(mut file) = File::open(path) else {
        // Not an error: the run may not have started, and its transcript is written by
        // whoever runs it rather than reserved in advance.
        return LogTail {
            offset: 0,
            lines: Vec::new(),
        };
    };
    let Ok(size) = file.metadata().map(|meta| meta.len()) else {
        return LogTail {
            offset,
            lines: Vec::new(),
        };
    };
    // A transcript that shrank was truncated by a new run of the same voice — the first
    // stage starts the record over — so the offset the caller is holding points into a file
    // that no longer exists. Reading from the top is the only honest answer.
    let mut from = if offset > size { 0 } else { offset };
    let jumped = from == 0 && size > TAIL_BYTES;
    if jumped {
        from = size - TAIL_BYTES;
    }
    if file.seek(SeekFrom::Start(from)).is_err() {
        return LogTail {
            offset: size,
            lines: Vec::new(),
        };
    }

    let mut raw = Vec::new();
    if file.read_to_end(&mut raw).is_err() {
        return LogTail {
            offset: from,
            lines: Vec::new(),
        };
    }
    // Whole lines only. A step flushes per line, so a read that caught one half written must
    // leave it for the next call rather than hand the panel half an object to parse.
    let end = raw
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |at| at + 1);
    // Lossy rather than a refused read: a tail seek lands on an arbitrary byte, which may be
    // the middle of a Japanese transcript's UTF-8, and that partial first line is dropped
    // immediately below.
    let text = String::from_utf8_lossy(&raw[..end]);
    let mut lines: Vec<String> = text
        .lines()
        .skip(usize::from(jumped))
        .map(str::to_string)
        .collect();
    if lines.len() > TAIL_LINES {
        lines.drain(..lines.len() - TAIL_LINES);
    }
    LogTail {
        offset: from + end as u64,
        lines,
    }
}

/// Install one chosen checkpoint as a voice pack.
///
/// The one act on this screen that changes something, and it is here rather than only in the
/// handover prompt because choosing among several candidates is the human's judgement — the
/// checkpoint table exists to serve exactly that decision. An agent can call the same script
/// itself, with a display name and a portrait, when the user tells it which one.
///
/// The pack is named by its id: `install_pack.py --name` defaults to it, and a voice's
/// display name, character and portrait live in its own `voicepack.json`, which the 音色
/// screen edits. A second set of fields for them here would be the same thing twice.
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

    let (python, root, args) = {
        let host = app.state::<Host>();
        let python = resolve_python(&host)
            .ok_or_else(|| "no Python interpreter for the installer".to_string())?;
        if !jsonstream::lock(&host.training).claim() {
            return Err("an install is already in progress".to_string());
        }
        let args = vec![
            "--json".to_string(),
            // The sixth stage of the run, writing into the run's own record beside what the
            // five before it wrote. Same file, same schema, same writer (`_layout.py`), so
            // nothing downstream can tell this stage was started from a window.
            "--status-file".to_string(),
            text(&status_path(&host, &req.pack_id)),
            "--pack".to_string(),
            text(&checkpoint),
            "--id".to_string(),
            req.pack_id.clone(),
            "--data-dir".to_string(),
            text(&host.data_dir),
            // The id names a run whose checkpoints are on screen, so a second install under
            // it is a replacement, not an accident.
            "--force".to_string(),
        ];
        (python, host.root.clone(), args)
    };

    let outcome = {
        let app = app.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let host = app.state::<Host>();
            let mut command = Command::new(&python);
            command
                .arg(root.join(INSTALLER))
                .args(&args)
                .current_dir(&root);
            let spec = Spec {
                event: EVENT_TRAIN,
                label: "install_pack.py",
                stderr_log: "train-install.err.log",
                first_stage: "install",
            };
            host.log("training install: install_pack.py");
            jsonstream::run(&app, &spec, command, &host.training)
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
    // Best effort: a record this cannot write only makes the next discard ask before deleting.
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
fn discard_refusal(dir: &Path, count: usize) -> String {
    format!(
        "{} holds {count} checkpoint(s) that no pack was installed from, and deleting this \
         tree deletes them. Install the one you want first, or confirm the deletion to take \
         them with it.",
        dir.display()
    )
}

/// What one run left on disk, measured: what exists, how big, and where to open it.
///
/// Recursive by directory rather than per file: `latents/` is one entry per clip and
/// `lora/` is one directory per checkpoint, and a list of two thousand rows is not a file
/// panel. The two log files are named separately because a discard keeps them.
#[tauri::command]
pub async fn training_scratch(app: AppHandle, pack_id: String) -> Result<ScratchTree, String> {
    validate_id(&pack_id)?;
    let (paths, transcript, status) = {
        let host = app.state::<Host>();
        (
            Paths::new(&host, &pack_id),
            transcript_path(&host, &pack_id),
            status_path(&host, &pack_id),
        )
    };
    let (bytes, _) = measure(&paths.dir);
    Ok(ScratchTree {
        dir: text(&paths.dir),
        exists: paths.dir.is_dir(),
        bytes,
        entries: paths.artefacts().into_iter().map(entry).collect(),
        checkpoints: checkpoints(&paths, &pack_id),
        transcript: entry(&transcript),
        status: entry(&status),
    })
}

/// Delete a finished run's scratch tree, and report what that freed.
///
/// Refuses until the loss is acknowledged, because it is a real loss: a checkpoint no pack
/// was installed from is an hour of GPU time nobody kept. The transcript and the status file
/// are deliberately left behind — they are the record of what happened, and they are what an
/// agent reads afterwards.
#[tauri::command]
pub async fn training_discard(
    app: AppHandle,
    pack_id: String,
    confirmed: bool,
) -> Result<u64, String> {
    validate_id(&pack_id)?;
    let (paths, status) = {
        let host = app.state::<Host>();
        (Paths::new(&host, &pack_id), status_path(&host, &pack_id))
    };
    if !paths.dir.is_dir() {
        return Err(format!("{} does not exist", paths.dir.display()));
    }
    // The run's own file is the only thing that knows whether a step is writing into this
    // tree right now, and after `resolve_live` it can be trusted: nothing in this app starts
    // a run any more, so there is no in-process state to consult instead.
    if let Some(mut run) = read_status(&status) {
        resolve_live(&mut run);
        if run.live {
            return Err(format!(
                "{pack_id} is training right now (stage {}, pid {}): stop the run before \
                 deleting what it is writing into",
                run.stage, run.pid
            ));
        }
    }

    let risked = at_risk(&paths.lora, &paths.installed);
    if !risked.is_empty() && !confirmed {
        return Err(discard_refusal(&paths.dir, risked.len()));
    }
    let (bytes, _) = measure(&paths.dir);
    std::fs::remove_dir_all(&paths.dir)
        .map_err(|err| format!("could not delete {}: {err}", paths.dir.display()))?;
    app.state::<Host>()
        .log(&format!("deleted scratch {}", paths.dir.display()));
    Ok(bytes)
}

// -------------------------------------------------------------- the record on disk --

/// What a run looks like from outside, for a caller that is not watching it happen.
///
/// Written by `scripts/training/_layout.py`, field for field, and read here. `state` is the
/// event kind that last described a stage - `pending`, `running`, `ok`, `skip`, `fail` -
/// plus one word the stream cannot produce: `interrupted`, for a stage that was still
/// running when the process ended. That distinction is the point. A `fail` was explained by
/// the step that failed, with a remedy; an `interrupted` was killed, and there is nothing to
/// explain.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct RunStatus {
    /// Bumped only if a field's meaning changes, so a reader can refuse an answer it does
    /// not understand instead of misreading one.
    schema: u32,
    pack_id: String,
    /// True from the moment a step starts writing until that process exits. One that was
    /// killed cannot write `false` here, which is what `pid` is for.
    live: bool,
    /// The process that wrote this file: one step of the pipeline, not the panel.
    pid: u32,
    /// The stage the stream last mentioned, and that stage's state. Not "the furthest stage":
    /// a log line belonging to a step that has not started yet says `pending`, which is true.
    stage: String,
    state: String,
    message: String,
    done: Option<u64>,
    total: Option<u64>,
    /// The failure, kept at the top level because "what failed" must not require walking
    /// `stages` — and kept sticky, because the run stops at the stage that failed.
    failed_stage: Option<String>,
    failure: Option<String>,
    remedy: Option<String>,
    started: u64,
    /// When the last event arrived. A `live` run whose `updated` is minutes old is a run
    /// whose process died.
    updated: u64,
    ended: Option<u64>,
    stages: Vec<StageStatus>,
    /// The transcript this status was folded from, so one read says where to look next.
    log: String,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct StageStatus {
    stage: String,
    state: String,
    message: String,
    done: Option<u64>,
    total: Option<u64>,
    started: Option<u64>,
    ended: Option<u64>,
}

/// The run is over, whatever the last stage said.
///
/// A stage still marked `running` was killed or died without a terminal event, and leaving
/// it running in a file whose `live` is false would be a contradiction the reader has to
/// resolve. `interrupted` is the panel's 已中断, in the file's vocabulary: distinct from
/// `fail`, which came with an explanation, because this one did not.
///
/// The writing side does this for itself on the way out (`_layout.py::_Record.close`); this
/// copy is for the file it never got to write.
fn settle(status: &mut RunStatus, now: u64) {
    status.live = false;
    status.ended = Some(now);
    status.updated = now;
    for row in &mut status.stages {
        if row.state == "running" {
            row.state = "interrupted".to_string();
            row.ended = Some(now);
        }
    }
    if status.state == "running" {
        status.state = "interrupted".to_string();
    }
}

fn read_status(path: &Path) -> Option<RunStatus> {
    serde_json::from_value(read_json(path)?).ok()
}

/// `data/logs/training-<pack id>.jsonl`, the transcript, and its status sibling. In
/// `data/logs` because every other child's output this app keeps is there, and because a
/// discarded run's record has to survive the run's files.
fn transcript_path(host: &Host, pack_id: &str) -> PathBuf {
    layout::logs_dir(&host.data_dir).join(format!("training-{pack_id}.jsonl"))
}

fn status_path(host: &Host, pack_id: &str) -> PathBuf {
    layout::logs_dir(&host.data_dir).join(format!("training-{pack_id}.status.json"))
}

/// One file or directory of a run, measured.
#[derive(Serialize)]
pub struct ScratchEntry {
    /// The name on disk. The panel labels it from this rather than being told a label,
    /// because what a file is called is a fact and what it is called in Chinese is not.
    name: String,
    path: String,
    dir: bool,
    exists: bool,
    /// Recursive for a directory, so the number is what the tree costs.
    bytes: u64,
    /// How many files a directory holds, which is what distinguishes a `lora/` with no
    /// checkpoints in it from one that was never created. Zero for a file.
    files: u64,
}

#[derive(Serialize)]
pub struct ScratchTree {
    dir: String,
    exists: bool,
    /// Everything under `dir`, including what is not in `entries`: trainer state, a partial
    /// download, anything a step left that this file does not name.
    bytes: u64,
    entries: Vec<ScratchEntry>,
    /// What the run produced and the screen selects from. Here rather than behind a second
    /// command because it comes out of the same tree in the same walk, and the screen shows
    /// both at once.
    checkpoints: Vec<Checkpoint>,
    /// Named separately from `entries` because a discard keeps them.
    transcript: ScratchEntry,
    status: ScratchEntry,
}

fn entry(path: &Path) -> ScratchEntry {
    let meta = std::fs::symlink_metadata(path).ok();
    let dir = meta.as_ref().is_some_and(|meta| meta.is_dir());
    let (bytes, files) = measure(path);
    ScratchEntry {
        name: path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned(),
        path: text(path),
        dir,
        exists: meta.is_some(),
        bytes,
        files: if dir { files } else { 0 },
    }
}

/// Bytes and file count, recursively. Symlinks are reported as themselves rather than
/// followed: nothing a training step writes is one, and a walk that follows them can loop.
fn measure(path: &Path) -> (u64, u64) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return (0, 0);
    };
    if meta.is_file() {
        return (meta.len(), 1);
    }
    if !meta.is_dir() {
        return (0, 0);
    }
    let Ok(entries) = std::fs::read_dir(path) else {
        return (0, 0);
    };
    entries.flatten().fold((0, 0), |(bytes, files), entry| {
        let (found, counted) = measure(&entry.path());
        (bytes + found, files + counted)
    })
}

// -------------------------------------------------------------------- the scratch tree --

/// Where one voice's run lives. Under the data dir, because latents and checkpoints are
/// large, regenerable, and not something anyone should be backing up.
struct Paths {
    dir: PathBuf,
    dataset: PathBuf,
    /// Where `prepare_dataset.py` puts its QA report by default: `<out-dataset>.qa.json`.
    qa: PathBuf,
    latents: PathBuf,
    manifest: PathBuf,
    lora: PathBuf,
    samples: PathBuf,
    score: PathBuf,
    /// Which checkpoints of this run a pack has been installed from, one name per line.
    /// `install_pack.py` records nothing about where a pack came from - the manifest
    /// describes the voice, not its provenance - so this is the only thing that can tell an
    /// hour of GPU time nobody kept from one that is already a voice pack.
    installed: PathBuf,
}

impl Paths {
    /// This is also the layout the handover prompt hands an agent: a run written anywhere
    /// else is a run this panel cannot measure, which is why the paths are named in one
    /// place and given out rather than described.
    fn new(host: &Host, pack_id: &str) -> Self {
        let dir = host.data_dir.join("cache").join("train").join(pack_id);
        Self {
            dataset: dir.join("dataset.jsonl"),
            qa: dir.join("dataset.jsonl.qa.json"),
            latents: dir.join("latents"),
            manifest: dir.join("train_manifest.jsonl"),
            lora: dir.join("lora"),
            samples: dir.join("samples"),
            score: dir.join("score"),
            installed: dir.join(INSTALLED),
            dir,
        }
    }

    /// Everything a run writes, in the order it writes it, for the file panel.
    ///
    /// Not a `read_dir`: the point of the list is that a file which is *missing* is worth
    /// showing — a run that stopped after the dataset stage should read as six empty rows,
    /// not as a short list. `dir` itself carries the recursive total, so anything a step
    /// left that this list does not name is still counted.
    fn artefacts(&self) -> [&Path; 8] {
        [
            &self.dataset,
            &self.qa,
            &self.manifest,
            &self.latents,
            &self.lora,
            &self.samples,
            &self.score,
            &self.installed,
        ]
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

/// The interpreter the steps run under, resolved the way the runtime resolves it:
/// `runtime.json`'s `ttsPython` first, because that is the one bootstrap wrote and the worker
/// is using, then the two places a packaged install puts one. A checkout has no
/// `runtime/python` at all, so honouring `runtime.json` is what makes training work outside
/// an install.
fn resolve_python(host: &Host) -> Option<PathBuf> {
    let mut candidates = Vec::new();
    candidates.extend(runtime_python(host));
    // Joined per component rather than as one slash-separated literal: this path is printed
    // into the handover prompt, and `Path::display()` prints what it was given — a mixed
    // `E:\install\runtime/python/Scripts/python.exe` runs fine and reads like a bug.
    candidates.push(
        host.root
            .join("runtime")
            .join("python")
            .join("Scripts")
            .join("python.exe"),
    );
    candidates.push(host.root.join("runtime").join("python").join("python.exe"));
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

    /// The refusal that keeps an hour of GPU time. A checkpoint no pack was installed from
    /// stops a discard; installing it takes it out of the count; a directory that is not an
    /// adapter was never in it.
    #[test]
    fn a_checkpoint_no_pack_came_from_stops_a_discard() {
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
            discard_refusal(&dir, risked.len()),
            format!(
                "{} holds 2 checkpoint(s) that no pack was installed from, and deleting this \
                 tree deletes them. Install the one you want first, or confirm the deletion \
                 to take them with it.",
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

    /// A status file as `_layout.py` writes one, mid-run: the schema the two sides share,
    /// and the one judgement this side makes about it.
    ///
    /// The file claims a live run at a pid that cannot be alive, which is the single case
    /// its writer could not report — killed outright, nothing to explain. It has to read as
    /// interrupted at the last event's timestamp, and the stages that finished have to stay
    /// finished.
    #[test]
    fn a_killed_run_reads_as_interrupted_at_its_last_event() {
        let written = r#"{
          "schema": 1,
          "pack_id": "smoke",
          "live": true,
          "pid": 0,
          "stage": "train",
          "state": "running",
          "message": "step 40/100   loss 0.9312   2.38s/step   ETA 0:02:22",
          "done": 40,
          "total": 100,
          "failed_stage": null,
          "failure": null,
          "remedy": null,
          "started": 1000,
          "updated": 9000,
          "ended": null,
          "stages": [
            {"stage": "dataset", "state": "ok", "message": "3 clips, 0.1 min",
             "done": 6, "total": 6, "started": 1000, "ended": 1200},
            {"stage": "latents", "state": "ok", "message": "3 row(s), 194 frames",
             "done": 3, "total": 3, "started": 1300, "ended": 2000},
            {"stage": "train", "state": "running", "message": "step 40/100",
             "done": 40, "total": 100, "started": 2100, "ended": null},
            {"stage": "samples", "state": "pending", "message": "",
             "done": null, "total": null, "started": null, "ended": null},
            {"stage": "score", "state": "pending", "message": "",
             "done": null, "total": null, "started": null, "ended": null},
            {"stage": "install", "state": "pending", "message": "",
             "done": null, "total": null, "started": null, "ended": null}
          ],
          "log": "C:\\data\\logs\\training-smoke.jsonl"
        }"#;
        let mut status: RunStatus = serde_json::from_str(written).expect("the writer's shape");
        assert_eq!(status.stages.len(), 6, "one row per stage, always");
        assert_eq!((status.done, status.total), (Some(40), Some(100)));

        resolve_live(&mut status);
        assert!(!status.live, "pid 0 is nobody");
        assert_eq!(status.state, "interrupted");
        assert_eq!(status.stages[2].state, "interrupted");
        assert_eq!(
            status.stages[2].ended,
            Some(9000),
            "when it stopped, not when it was read"
        );
        assert_eq!(status.ended, Some(9000));
        assert_eq!(status.stages[0].state, "ok", "a finished stage stays finished");
        assert_eq!(status.stages[3].state, "pending");
        assert!(status.remedy.is_none(), "nothing explained itself");

        // The same file with a pid that IS alive is left exactly as written: this process
        // wrote none of it, so a run in flight must not be settled behind its back.
        let mut running: RunStatus = serde_json::from_str(written).expect("the writer's shape");
        running.pid = std::process::id();
        resolve_live(&mut running);
        assert!(running.live);
        assert_eq!(running.state, "running");
        assert!(running.ended.is_none());
    }

    /// The console reads the transcript forward, in whole lines, and a run that started over
    /// takes the reader back to the top.
    #[test]
    fn a_transcript_is_read_forward_in_whole_lines() {
        let dir = std::env::temp_dir().join("voice-core-training-tail");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("a temp directory");
        let path = dir.join("training-smoke.jsonl");
        std::fs::write(&path, "{\"one\":1}\n{\"two\":2}\n").expect("two lines");

        let first = tail(&path, 0);
        assert_eq!(first.lines, vec!["{\"one\":1}", "{\"two\":2}"]);
        assert_eq!(first.offset, 20);

        // Nothing new: no lines and the same offset, so a poll costs the panel nothing.
        let idle = tail(&path, first.offset);
        assert!(idle.lines.is_empty());
        assert_eq!(idle.offset, first.offset);

        // A line still being written is left for the next call rather than handed over half
        // parsed, and the offset stays behind it.
        std::fs::write(&path, "{\"one\":1}\n{\"two\":2}\n{\"thr").expect("a partial line");
        let partial = tail(&path, first.offset);
        assert!(partial.lines.is_empty());
        assert_eq!(partial.offset, first.offset);
        std::fs::write(&path, "{\"one\":1}\n{\"two\":2}\n{\"three\":3}\n").expect("the rest");
        let rest = tail(&path, partial.offset);
        assert_eq!(rest.lines, vec!["{\"three\":3}"]);

        // The first stage of a new run truncates the transcript, which leaves every offset a
        // caller is holding pointing past the end of a file it no longer describes.
        std::fs::write(&path, "{\"fresh\":1}\n").expect("a new run");
        let reset = tail(&path, rest.offset);
        assert_eq!(reset.lines, vec!["{\"fresh\":1}"]);
        assert_eq!(reset.offset, 12);

        // A transcript nobody wrote is not an error: the run may not have started yet.
        let missing = tail(&dir.join("training-nobody.jsonl"), 0);
        assert!(missing.lines.is_empty());
        assert_eq!(missing.offset, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The file panel's numbers. A directory is worth its whole tree, and an artefact that
    /// was never written is a row that says so rather than a row that is absent.
    #[test]
    fn a_scratch_tree_is_measured_by_what_it_holds() {
        let dir = std::env::temp_dir().join("voice-core-training-measure");
        let checkpoint = dir.join("lora").join("checkpoint_0000100");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&checkpoint).expect("a temp checkpoint");
        std::fs::write(dir.join("dataset.jsonl"), "0123456789").expect("a temp dataset");
        std::fs::write(checkpoint.join("adapter_model.safetensors"), vec![0u8; 2048])
            .expect("temp weights");

        assert_eq!(measure(&dir), (2058, 2));

        let checkpoints = entry(&dir.join("lora"));
        assert!(checkpoints.dir && checkpoints.exists);
        assert_eq!((checkpoints.bytes, checkpoints.files), (2048, 1));

        let dataset = entry(&dir.join("dataset.jsonl"));
        assert!(!dataset.dir && dataset.exists);
        assert_eq!((dataset.bytes, dataset.files), (10, 0));

        let never_written = entry(&dir.join("samples"));
        assert!(!never_written.exists);
        assert_eq!((never_written.bytes, never_written.files), (0, 0));
        assert_eq!(never_written.name, "samples");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The log directory is the run list, so a file's name is the run's id. Anything else in
    /// there is somebody else's log.
    #[test]
    fn a_run_is_named_by_its_status_file() {
        assert_eq!(
            run_id("training-my-voice.status.json").as_deref(),
            Some("my-voice")
        );
        assert_eq!(run_id("training-my-voice.jsonl"), None);
        assert_eq!(run_id("runtime.err.log"), None);
        // The name can still be nonsense, which is why the id is validated rather than
        // trusted: an empty one, or one climbing out of the log directory, is not a run.
        assert_eq!(run_id("training-.status.json").as_deref(), Some(""));
        assert!(validate_id("").is_err());
        assert!(validate_id("..").is_err());
        assert!(validate_id("my-voice").is_ok());
    }
}
