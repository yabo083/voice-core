//! Voice pack registry. Packs are data plus metadata, never logic: a pack is a
//! kind and a path, and only the engine knows what to do with either.
//!
//! Two sources, merged. The registry lives in the app's one settings file,
//! `config.json`, under `voicePacks`; the pack itself may carry `voicepack.json`
//! describing what it is (see `docs/voicepack-spec.md`).
//!
//! The pack wins. The registry is not a considered opinion about a pack - it is
//! generated, by the installer that seeded it and by the panel that registered the
//! pack, and generated boilerplate must not outrank the pack's own description of
//! itself. What the registry is actually authoritative about is *where the packs are*:
//! an entry needs nothing but `id` and `path`.
//!
//! Where the manifest is silent the entry answers, and where both are the program
//! default does - which is why a pack with no manifest keeps working exactly as it did
//! before the format existed. The manifest is an enhancement, not an entry requirement.
//!
//! `config.json` is hand-editable (the tray's 设置 entry opens it), so the registry
//! reloads itself when the file's mtime changes rather than demanding a restart, and it
//! is parsed as JSONC because a human wrote it. Manifests are read the same way.

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

impl PackKind {
    /// What the payload on disk says it is, for a pack whose neither registry entry nor
    /// manifest declared a kind. A directory is a LoRA adapter, an audio file is
    /// reference audio, anything else is an embedding - the same three-way split the
    /// panel's picker makes, in one place instead of two.
    fn infer(path: &Path) -> Self {
        if path.is_dir() {
            return Self::LoraAdapter;
        }
        let name = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        if name.ends_with(".wav")
            || name.ends_with(".flac")
            || name.ends_with(".mp3")
            || name.ends_with(".ogg")
            || name.ends_with(".m4a")
        {
            Self::ReferenceAudio
        } else {
            Self::SpeakerEmbedding
        }
    }
}

/// Joins `value` onto `base` unless it is already absolute.
///
/// Separators are normalised on the way out: these strings are shown in a panel and
/// pasted into shells, and `C:\\a\\b/c` is a path Windows accepts but nobody wants to
/// read.
fn absolute(base: &Path, value: &str) -> String {
    let raw = Path::new(value);
    let joined = if raw.is_absolute() { raw.to_path_buf() } else { base.join(raw) };
    native(&joined)
}

#[cfg(windows)]
fn native(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\")
}

#[cfg(not(windows))]
fn native(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// A pack as everything downstream sees it: the registry entry and the pack's own
/// manifest already merged, every field concrete, `avatar` already absolute.
///
/// Serialized as-is by `GET /api/voices`, which is why the merge happens here and not
/// in each frontend: two frontends resolving a relative path against different bases is
/// exactly the bug the manifest exists to end.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoicePack {
    pub id: String,
    pub name: String,
    pub languages: Vec<String>,
    pub kind: PackKind,
    /// Absolute after hydration.
    pub path: String,
    pub engine: String,
    /// Speaker name a dialog frontend shows. `None` means "use `name`"; a frontend with
    /// no portrait falls back to this string's first character.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub character: Option<String>,
    /// Absolute path to the portrait, or `None`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// Appearance the pack asks for. Forwarded, never interpreted here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dialog: Option<DialogStyle>,
    /// Inference preferences the pack asks for. Forwarded, never interpreted here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthesis: Option<SynthesisPrefs>,
    /// Absolute path of the manifest that contributed, when there was one. Frontends
    /// show it; nothing depends on it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
}

/// A `voicePacks` entry exactly as `config.json` holds it: a pointer, plus whatever the
/// user chose to pin on this machine. Only `id` and `path` are required - a slim entry
/// is the normal case once a pack carries its own manifest.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    id: String,
    path: String,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    kind: Option<PackKind>,
    #[serde(default)]
    languages: Option<Vec<String>>,
    #[serde(default)]
    engine: Option<String>,
    #[serde(default)]
    character: Option<String>,
    #[serde(default)]
    avatar: Option<String>,
    #[serde(default)]
    dialog: Option<DialogStyle>,
    #[serde(default)]
    synthesis: Option<SynthesisPrefs>,
}

/// Subtitle appearance a pack asks for. Every field optional: an absent one means
/// "whatever the dialog already does", which is what makes a partial manifest useful.
///
/// Carried through to frontends rather than interpreted here - the runtime has no
/// opinion about colour.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DialogStyle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ruby_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub countdown_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reveal: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_seconds: Option<f64>,
}

/// Inference parameters a pack prefers. Same rule as `DialogStyle`: absent means the
/// program default, and the runtime only forwards what it was given.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthesisPrefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub num_steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f64>,
}

/// The newest `schema` this build understands. A manifest declaring more is read for
/// its core fields and its unknown sections are ignored - see the spec for why
/// degrading beats refusing.
const SCHEMA: u32 = 1;

/// `voicepack.json`, as written on disk. Deliberately all-optional except nothing:
/// even `schema` defaults, because a manifest that forgot it is still more information
/// than no manifest at all.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    #[serde(default)]
    pub schema: Option<u32>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub engine: Option<String>,
    #[serde(default)]
    pub kind: Option<PackKind>,
    #[serde(default)]
    pub languages: Option<Vec<String>>,
    #[serde(default)]
    pub character: Option<String>,
    /// Relative to the pack itself, which is the whole point: the portrait travels
    /// with the voice instead of living in a shared folder beside it.
    #[serde(default)]
    pub avatar: Option<String>,
    #[serde(default)]
    pub dialog: Option<DialogStyle>,
    #[serde(default)]
    pub synthesis: Option<SynthesisPrefs>,
}

impl Manifest {
    /// The manifest that belongs to the payload at `path`, if it wrote one.
    ///
    /// A directory pack keeps it inside: `<dir>/voicepack.json`. A single-file pack has
    /// no inside, so it gets a sibling named after the file with its last extension
    /// replaced: `miyu.speaker.safetensors` -> `miyu.speaker.voicepack.json`.
    ///
    /// Returns the path separately so a panel can show which file spoke, and so an
    /// unreadable manifest is distinguishable from an absent one: absent yields
    /// `(None, None)`, broken yields `(None, Some(path))` after saying why once.
    fn beside(path: &Path) -> (Option<Self>, Option<String>) {
        let file = if path.is_dir() {
            path.join("voicepack.json")
        } else {
            let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
            match path.parent() {
                Some(parent) => parent.join(format!("{stem}.voicepack.json")),
                None => return (None, None),
            }
        };

        let Ok(raw) = std::fs::read_to_string(&file) else {
            return (None, None);
        };
        let shown = native(&file);
        match serde_json::from_str::<Self>(&crate::jsonc::to_json(&raw)) {
            Ok(manifest) => {
                // A manifest from the future is read for what this build understands and
                // its unknown sections are ignored. Refusing would mean a newer pack
                // makes the whole voice disappear, which is worse than an unstyled one.
                if manifest.schema.unwrap_or(SCHEMA) > SCHEMA {
                    eprintln!(
                        "{shown}: schema {} is newer than this build understands ({SCHEMA}); reading core fields only",
                        manifest.schema.unwrap_or(SCHEMA)
                    );
                }
                (Some(manifest), Some(shown))
            }
            Err(err) => {
                eprintln!("{shown} is not valid: {err}; falling back to the registry entry");
                (None, Some(shown))
            }
        }
    }
}

/// The one section of `config.json` this crate reads. Everything else in that file
/// belongs to the tray, and an unknown key is ignored rather than rejected: the runtime
/// must not fail to start because a frontend added a preference it has never heard of.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PackSection {
    #[serde(default)]
    voice_packs: Vec<Entry>,
}

pub struct Registry {
    file: PathBuf,
    data_dir: PathBuf,
    packs: Vec<VoicePack>,
    /// mtime of the bytes currently loaded. Only ever set from a mtime sampled BEFORE a
    /// successful parse, so it can be older than the file on disk but never newer.
    loaded_from: Option<SystemTime>,
    /// One entry per pack that had a manifest when it was last read, with that file's mtime.
    /// The merged view depends on these files as much as on `config.json`, and the spec tells
    /// people to edit them, so they are part of the reload check - a renamed voice must not
    /// need a restart to show up.
    manifests: Vec<(PathBuf, Option<SystemTime>)>,
    /// mtime already reported as unparseable. A broken file is retried on every access
    /// (it may be mid-save) but only complained about once.
    complained_about: Option<SystemTime>,
}

/// The manifest files behind a loaded view, with their mtimes, for the reload check.
fn stamps(packs: &[VoicePack]) -> Vec<(PathBuf, Option<SystemTime>)> {
    packs
        .iter()
        .filter_map(|pack| pack.manifest.as_deref())
        .map(PathBuf::from)
        .map(|path| {
            let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            (path, mtime)
        })
        .collect()
}

impl Registry {
    pub fn load(file: PathBuf, data_dir: PathBuf) -> Self {
        let mut registry = Self {
            file,
            data_dir,
            packs: Vec::new(),
            loaded_from: None,
            manifests: Vec::new(),
            complained_about: None,
        };
        registry.reload_if_changed();
        registry
    }

    /// Whether every manifest behind the loaded view is still the file it was read from.
    ///
    /// A manifest that has appeared since the last read is caught too: a pack with none had no
    /// stamp, so writing one changes what the merge would produce and this must not report
    /// "unchanged". Hence the count of manifests found is compared, not just their mtimes.
    fn manifests_unchanged(&self) -> bool {
        let mut found = 0usize;
        for pack in &self.packs {
            let (_, path) = Manifest::beside(Path::new(&pack.path));
            let Some(path) = path else { continue };
            found += 1;
            let path = PathBuf::from(path);
            let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
            if !self.manifests.iter().any(|(known, stamp)| *known == path && *stamp == mtime) {
                return false;
            }
        }
        found == self.manifests.len()
    }

    /// Cheap stat; re-parses only when something actually changed.
    ///
    /// The mtime is sampled BEFORE the read and only cached after a successful parse. Both
    /// halves matter: an editor that saves in place (Notepad does) truncates and rewrites,
    /// so a read can land on a prefix, and Windows file times only advance with the ~15.6 ms
    /// timer tick - caching the mtime of a failed parse would match the guard below forever
    /// and pin an empty registry against a file that visibly lists packs.
    ///
    /// "Something" includes every pack manifest, which costs one stat per pack per call. That
    /// is the price of the promise the docs make: edit a pack's `voicepack.json` and the next
    /// listing shows it. Pack counts are in the tens, and a stat is orders of magnitude below
    /// the JSON parse it guards.
    pub fn reload_if_changed(&mut self) {
        let mtime = std::fs::metadata(&self.file).and_then(|m| m.modified()).ok();
        if mtime.is_some() && mtime == self.loaded_from && self.manifests_unchanged() {
            return;
        }
        match std::fs::read_to_string(&self.file) {
            Ok(raw) => match serde_json::from_str::<PackSection>(&crate::jsonc::to_json(&raw)) {
                Ok(section) => {
                    let hydrated: Vec<VoicePack> = section
                        .voice_packs
                        .into_iter()
                        .map(|entry| self.hydrate(entry))
                        .collect();
                    self.manifests = stamps(&hydrated);
                    self.packs = hydrated;
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

    /// Already absolute after hydration; kept as a method because callers read like the
    /// path still needs resolving and one of them is in another crate's mental model.
    pub fn resolve_path(&self, pack: &VoicePack) -> String {
        pack.path.clone()
    }

    /// Pack manifest over registry entry over program default, field by field.
    ///
    /// The pack wins because the registry is not a user's considered opinion - it is
    /// generated: the installer seeds it, and the panel writes an entry when a pack is
    /// first registered. Letting generated boilerplate outrank the pack's own
    /// description is how a pack ends up unable to state its own name.
    ///
    /// There is deliberately no way to pin a field locally against the manifest. The
    /// panel edits the manifest instead, which is the same information in the place that
    /// owns it. A read-only pack that needs a local override would need that mechanism;
    /// nothing needs it yet, and inventing it now would add a third precedence tier for
    /// a case nobody has.
    ///
    /// Reading the manifest here rather than at use time is deliberate: it happens once
    /// per config change, and every consumer - the API, the CLI, the panel, the dialog -
    /// then sees one already-merged answer instead of re-implementing the precedence.
    fn hydrate(&self, entry: Entry) -> VoicePack {
        let raw = Path::new(&entry.path);
        let path = if raw.is_absolute() {
            raw.to_path_buf()
        } else {
            self.data_dir.join(raw)
        };

        let (manifest, manifest_path) = Manifest::beside(&path);
        let m = manifest.unwrap_or_default();

        let name = m.name.or(entry.name).unwrap_or_else(|| entry.id.clone());
        let kind = m.kind.or(entry.kind).unwrap_or_else(|| PackKind::infer(&path));
        let languages = m.languages.or(entry.languages).unwrap_or_default();

        // Two different bases, which is why this is resolved here and not downstream: a
        // manifest avatar is relative to the pack, a registry avatar is relative to the
        // data dir (that is where the retired `data/avatars/` layout put it).
        let avatar = match m.avatar.as_deref() {
            Some(value) => {
                let base = if path.is_dir() {
                    path.clone()
                } else {
                    path.parent().map(Path::to_path_buf).unwrap_or_else(|| path.clone())
                };
                Some(absolute(&base, value))
            }
            None => entry.avatar.as_deref().map(|value| absolute(&self.data_dir, value)),
        };

        VoicePack {
            id: entry.id,
            name,
            languages,
            kind,
            path: native(&path),
            engine: m.engine.or(entry.engine).unwrap_or_default(),
            character: m.character.or(entry.character),
            avatar: avatar.filter(|p| Path::new(p).exists()),
            dialog: m.dialog.or(entry.dialog),
            synthesis: m.synthesis.or(entry.synthesis),
            manifest: manifest_path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{PackKind, Registry};
    use std::path::{Path, PathBuf};

    /// A private data dir under the OS temp dir. Named after the case so a leftover from a
    /// crashed run is identifiable, and removed on the way in rather than on the way out:
    /// a failing assert must leave the tree behind for a human to look at.
    fn scratch(case: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("voice-core-packs-{case}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("voicepacks/p")).unwrap();
        dir
    }

    fn write(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn the_pack_manifest_outranks_the_registry_entry() {
        let dir = scratch("manifest-wins");
        write(
            &dir.join("config.json"),
            r#"{ "voicePacks": [
                 { "id": "p", "path": "voicepacks/p", "name": "seeded by the installer",
                   "engine": "seeded-engine", "languages": ["en"] }
               ] }"#,
        );
        write(
            &dir.join("voicepacks/p/voicepack.json"),
            r#"{ "schema": 1, "name": "what the pack calls itself",
                 "character": "霞沢美游", "avatar": "avatar.png", "languages": ["ja"] }"#,
        );
        write(&dir.join("voicepacks/p/avatar.png"), "not really a png");

        let registry = Registry::load(dir.join("config.json"), dir.clone());
        let pack = &registry.all()[0];

        assert_eq!(pack.name, "what the pack calls itself");
        assert_eq!(pack.character.as_deref(), Some("霞沢美游"));
        assert_eq!(pack.languages, vec!["ja".to_string()]);
        // Silent in the manifest, so the entry answers - the fallback, not a loser.
        assert_eq!(pack.engine, "seeded-engine");
        // Resolved against the pack, and reported only because the file is really there.
        assert_eq!(
            pack.avatar.as_deref(),
            Some(super::native(&dir.join("voicepacks/p/avatar.png")).as_str())
        );
        assert_eq!(
            pack.manifest.as_deref(),
            Some(super::native(&dir.join("voicepacks/p/voicepack.json")).as_str())
        );
        // Nobody stated a kind; the payload on disk decides.
        assert_eq!(pack.kind, PackKind::LoraAdapter);

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn editing_a_manifest_reloads_without_touching_config_json() {
        let dir = scratch("manifest-reload");
        let config = dir.join("config.json");
        write(
            &config,
            r#"{ "voicePacks": [{ "id": "p", "path": "voicepacks/p" }] }"#,
        );
        let manifest = dir.join("voicepacks/p/voicepack.json");
        write(&manifest, r#"{ "schema": 1, "name": "before" }"#);

        let mut registry = Registry::load(config.clone(), dir.clone());
        assert_eq!(registry.all()[0].name, "before");

        // Only the manifest changes. config.json keeps its mtime, which is exactly the case
        // that used to need a restart.
        write(&manifest, r#"{ "schema": 1, "name": "after" }"#);
        registry.reload_if_changed();
        assert_eq!(registry.all()[0].name, "after");

        // A manifest appearing where there was none counts as a change too.
        let dir2 = scratch("manifest-appears");
        let config2 = dir2.join("config.json");
        write(
            &config2,
            r#"{ "voicePacks": [{ "id": "p", "path": "voicepacks/p", "name": "from entry" }] }"#,
        );
        let mut registry = Registry::load(config2.clone(), dir2.clone());
        assert_eq!(registry.all()[0].name, "from entry");
        write(
            &dir2.join("voicepacks/p/voicepack.json"),
            r#"{ "schema": 1, "name": "from the pack" }"#,
        );
        registry.reload_if_changed();
        assert_eq!(registry.all()[0].name, "from the pack");

        std::fs::remove_dir_all(&dir).unwrap();
        std::fs::remove_dir_all(&dir2).unwrap();
    }

    #[test]
    fn a_pack_with_no_manifest_keeps_working_off_the_entry() {
        let dir = scratch("no-manifest");
        write(
            &dir.join("config.json"),
            r#"{ "voicePacks": [{ "id": "p", "path": "voicepacks/p", "name": "only here" }] }"#,
        );

        let registry = Registry::load(dir.join("config.json"), dir.clone());
        let pack = &registry.all()[0];

        assert_eq!(pack.name, "only here");
        assert_eq!(pack.manifest, None);
        assert_eq!(pack.avatar, None);
        // No file says a language, so none is invented.
        assert!(pack.languages.is_empty());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_absent_avatar_file_is_not_reported() {
        let dir = scratch("avatar-missing");
        write(
            &dir.join("config.json"),
            r#"{ "voicePacks": [{ "id": "p", "path": "voicepacks/p" }] }"#,
        );
        write(
            &dir.join("voicepacks/p/voicepack.json"),
            r#"{ "schema": 1, "character": "幼年瞬", "avatar": "gone.png" }"#,
        );

        let registry = Registry::load(dir.join("config.json"), dir.clone());
        let pack = &registry.all()[0];

        // The dialog draws a glyph placeholder from `character`; a path that resolves to
        // nothing would make it draw a broken image instead.
        assert_eq!(pack.avatar, None);
        assert_eq!(pack.character.as_deref(), Some("幼年瞬"));
        // Neither file names it, so the id is the name.
        assert_eq!(pack.name, "p");

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
