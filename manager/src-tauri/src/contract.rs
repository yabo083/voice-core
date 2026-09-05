//! Every shape that crosses the IPC boundary, in one file, so the contract can
//! be read without reading the implementation.
//!
//! Field names are snake_case and carry no `rename_all`: the frontend was
//! written against these names. `Pack` is the one exception that needs no
//! decision at all — every one of its fields is a single word, so camelCase and
//! snake_case are byte-identical, which is what lets the same struct serialise
//! into `config.json` (camelCase, read by the runtime) and deserialise from
//! `GET /api/voices` without a second definition.

use serde::{Deserialize, Serialize};

/// One bootstrap JSON line, forwarded verbatim.
pub const EVENT_BOOTSTRAP: &str = "bootstrap://event";
/// `{runtime, presenter, model_loaded}`, emitted on every transition.
pub const EVENT_STACK: &str = "stack://state";
/// One training step's JSON line, forwarded verbatim. The same shape as
/// `bootstrap://event` plus an optional `checkpoint`, which the train and score
/// stages use to name the artefact a line is about.
pub const EVENT_TRAIN: &str = "train://event";

/// A voice pack, as both `config.json`'s `voicePacks` array and
/// `GET /api/voices` spell it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Pack {
    pub id: String,
    /// Empty when the registry entry does not say - a slim entry is the normal shape
    /// once the pack carries `voicepack.json`, and the effective name then comes from
    /// the manifest (see `docs/voicepack-spec.md`). `/api/voices` serves the merged
    /// value; this app only sees the raw entry when the runtime is down.
    #[serde(default)]
    pub name: String,
    /// `lora-adapter` | `speaker-embedding` | `reference-audio`. A string rather
    /// than an enum deliberately: a kind a future engine adds must round-trip
    /// through this app untouched instead of failing to deserialise and taking
    /// the whole pack list with it. Empty when the entry is slim.
    #[serde(default)]
    pub kind: String,
    /// Absolute, or relative to the data dir.
    pub path: String,
    #[serde(default)]
    pub engine: String,
    #[serde(default)]
    pub languages: Vec<String>,
    /// Speaker name the dialog shows. Trained packs carry it; older ones do not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

/// One model repository the Irodori backend loads.
#[derive(Clone, Debug, Serialize)]
pub struct ModelState {
    pub repo: String,
    pub present: bool,
    /// What the repository costs on disk, present or not, so the frontend can
    /// show the price before it is paid.
    pub gib: f64,
}

/// What provisioning would find if it ran right now.
#[derive(Clone, Debug, Serialize)]
pub struct Inventory {
    pub engine_root: Option<String>,
    pub engine_python: Option<String>,
    pub python_ok: bool,
    pub cuda: Option<String>,
    pub hf_cache: Option<String>,
    pub models: Vec<ModelState>,
    pub packs: Vec<Pack>,
    /// Absolute path of `<data dir>/runtime.json`, present or not — it doubles as
    /// the frontend's only handle on the data dir (its parent) and the install
    /// root (its grandparent), so it is never null. Do not "fix" this to `None`
    /// when the file is missing without giving the frontend those two paths some
    /// other way.
    pub runtime_json: Option<String>,
    pub disk_free_gib: f64,
    /// Bytes still to fetch, as GiB: the models that are missing, plus the
    /// interpreter environment when there is no working interpreter.
    pub needs_gib: f64,
}

/// Arguments for one bootstrap run. Unknown keys are rejected rather than
/// ignored, so a camelCase slip in the frontend fails loudly at the first call
/// instead of silently provisioning into the default location.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProvisionOpts {
    pub engine_root: Option<String>,
    pub hf_home: Option<String>,
    pub voice_packs: Option<String>,
    /// Comma-separated stage names; everything else is skipped entirely.
    pub only: Option<String>,
    #[serde(default)]
    pub check_only: bool,
}

/// `GET /api/status`, plus whether it answered at all.
///
/// `body` is the runtime's own camelCase JSON, forwarded untouched. Restating
/// `service::Status` here in another casing would mean maintaining a copy of a
/// contract that already has one owner, and the HTTP API is that owner.
#[derive(Clone, Debug, Serialize)]
pub struct Status {
    pub reachable: bool,
    /// Why it did not answer. Down is the normal state before provisioning, so
    /// this is information, not an error to raise.
    pub error: Option<String>,
    pub body: Option<serde_json::Value>,
}

/// What the supervisor believes about the two children.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct StackState {
    /// The API answers on the loopback port, whoever started it.
    pub runtime: bool,
    /// This app's presenter child is alive. A presenter someone launched by hand
    /// is not claimed here, because this app cannot stop what it did not start.
    pub presenter: bool,
    pub model_loaded: bool,
}

/// One chosen checkpoint, becoming a voice pack.
///
/// The checkpoint and the run's id, and nothing else: the pack is named by its id
/// (`install_pack.py --name` defaults to it) and a voice's display name, character
/// and portrait live in its own `voicepack.json`, which the 音色 screen edits.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallRequest {
    /// The adapter directory the user picked out of the checkpoint table.
    pub checkpoint: String,
    pub pack_id: String,
}

#[derive(Serialize)]
pub struct Checkpoint {
    pub name: String,
    pub path: String,
    /// Out of the directory name the trainer chose:
    /// `checkpoint_best_val_loss_0001000_0.885155` is where the step and the loss
    /// are recorded, and there is no other copy of them.
    pub step: Option<u64>,
    pub val_loss: Option<f64>,
    /// From the score stage, when it ran: the worst clip in this checkpoint's
    /// group, which is the number selection should use. The mean hides the
    /// utterances that drift.
    pub lower_bound: Option<f64>,
    pub mean: Option<f64>,
    /// Pre-selected. The lowest validation loss, which is what the trainer's own
    /// best-checkpoint selection means.
    pub best: bool,
}
