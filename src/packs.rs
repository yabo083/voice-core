//! Voice pack registry. Packs are data plus metadata, never logic: a pack is a
//! kind and a path, and only the engine knows what to do with either.
//!
//! The registry lives in the app's one settings file, `config.json`, under
//! `voicePacks`. That file is hand-editable (the tray's 设置 entry opens it), so the
//! registry reloads itself when the file's mtime changes rather than demanding a
//! restart, and it is parsed as JSONC because a human wrote it.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PackKind {
    LoraAdapter,
    SpeakerEmbedding,
    ReferenceAudio,
}

impl PackKind {
    pub fn as_wire(&self) -> &'static str {
        match self {
            PackKind::LoraAdapter => "lora-adapter",
            PackKind::SpeakerEmbedding => "speaker-embedding",
            PackKind::ReferenceAudio => "reference-audio",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoicePack {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub languages: Vec<String>,
    pub kind: PackKind,
    /// Absolute, or relative to the data dir (portable installs).
    pub path: String,
    #[serde(default)]
    pub engine: String,
    /// Speaker name a dialog frontend shows. Falls back to `name` when absent.
    /// Trained packs are expected to carry this; older ones simply do not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
    /// Portrait for a dialog frontend, relative to the data dir (or absolute).
    /// Presentation metadata travels with the pack because the pack is what knows
    /// whose voice it is; the runtime only passes it through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
}

/// The one section of `config.json` this crate reads. Everything else in that file
/// belongs to the tray, and an unknown key is ignored rather than rejected: the runtime
/// must not fail to start because a frontend added a preference it has never heard of.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackSection {
    #[serde(default)]
    voice_packs: Vec<VoicePack>,
}

pub struct Registry {
    file: PathBuf,
    data_dir: PathBuf,
    packs: Vec<VoicePack>,
    /// mtime of the bytes currently loaded. Only ever set from a mtime sampled BEFORE a
    /// successful parse, so it can be older than the file on disk but never newer.
    loaded_from: Option<SystemTime>,
    /// mtime already reported as unparseable. A broken file is retried on every access
    /// (it may be mid-save) but only complained about once.
    complained_about: Option<SystemTime>,
}

impl Registry {
    pub fn load(file: PathBuf, data_dir: PathBuf) -> Self {
        let mut registry = Self {
            file,
            data_dir,
            packs: Vec::new(),
            loaded_from: None,
            complained_about: None,
        };
        registry.reload_if_changed();
        registry
    }

    /// Cheap stat; re-parses only when the file actually changed.
    ///
    /// The mtime is sampled BEFORE the read and only cached after a successful parse. Both
    /// halves matter: an editor that saves in place (Notepad does) truncates and rewrites,
    /// so a read can land on a prefix, and Windows file times only advance with the ~15.6 ms
    /// timer tick - caching the mtime of a failed parse would match the guard below forever
    /// and pin an empty registry against a file that visibly lists packs.
    pub fn reload_if_changed(&mut self) {
        let mtime = std::fs::metadata(&self.file).and_then(|m| m.modified()).ok();
        if mtime.is_some() && mtime == self.loaded_from {
            return;
        }
        match std::fs::read_to_string(&self.file) {
            Ok(raw) => match serde_json::from_str::<PackSection>(&crate::jsonc::to_json(&raw)) {
                Ok(section) => {
                    self.packs = section.voice_packs;
                    self.loaded_from = mtime;
                    self.complained_about = None;
                }
                Err(err) => {
                    // A malformed config must not silently look like "no voices installed":
                    // keep the last good list, say why once, and look again next time.
                    if self.complained_about != mtime {
                        eprintln!(
                            "config.json is not valid: {err}; keeping {} previously loaded pack(s)",
                            self.packs.len()
                        );
                        self.complained_about = mtime;
                    }
                }
            },
            // Only an absent file really means "no packs". Every other read failure is
            // transient - a sharing violation while an editor saves in place, a locked file,
            // a slow network share - and emptying the registry over one of those turns a
            // hiccup into "install a voice pack" on the next speak.
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                self.packs.clear();
                self.loaded_from = None;
            }
            Err(err) => {
                if self.complained_about != mtime {
                    eprintln!(
                        "cannot read config.json: {err}; keeping {} previously loaded pack(s)",
                        self.packs.len()
                    );
                    self.complained_about = mtime;
                }
            }
        }
    }

    pub fn all(&self) -> &[VoicePack] {
        &self.packs
    }

    pub fn get(&self, id: &str) -> Option<&VoicePack> {
        self.packs.iter().find(|p| p.id == id)
    }

    /// Resolves a pack's payload path against the data dir when relative.
    pub fn resolve_path(&self, pack: &VoicePack) -> String {
        let raw = Path::new(&pack.path);
        if raw.is_absolute() {
            pack.path.clone()
        } else {
            self.data_dir.join(raw).to_string_lossy().to_string()
        }
    }
}
