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

use std::collections::BTreeMap;
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

/// Which file decided one effective field. Serialized as `pack` | `config` | `derived`,
/// which is what a settings screen puts next to the value it shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Source {
    Pack,
    Config,
    Derived,
}

/// One field of the merge, plus the record of who won it.
///
/// The record is produced by the same expression that produces the value - this is
/// `Option::or` with a note taken - because a second pass that re-compares the two
/// inputs afterwards is a second implementation of the precedence, and the panel exists
/// precisely so nobody has to trust one of those.
///
/// `None` out means neither file spoke, so whatever `hydrate` falls back to is derived:
/// the id, the payload on disk, an empty list, or the program's own behaviour.
fn pick<T>(
    sources: &mut BTreeMap<String, Source>,
    key: &str,
    from_pack: Option<T>,
    from_config: Option<T>,
) -> Option<T> {
    let (value, source) = match (from_pack, from_config) {
        (Some(value), _) => (Some(value), Source::Pack),
        (None, Some(value)) => (Some(value), Source::Config),
        (None, None) => (None, Source::Derived),
    };
    sources.insert(key.to_string(), source);
    value
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
    /// Default expression the pack asks for: what this character sounds like when the
    /// caller says nothing. Forwarded, never interpreted here.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expression: Option<ExpressionPrefs>,
    /// Absolute path of the manifest that contributed, when there was one. Frontends
    /// show it; nothing depends on it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest: Option<String>,
    /// Which file decided each effective field, for a settings screen that must not lie.
    /// Keys are the camelCase wire names; values are `pack` | `config` | `derived`.
    pub sources: BTreeMap<String, Source>,
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
    #[serde(default)]
    expression: Option<ExpressionPrefs>,
}

/// Subtitle appearance a pack asks for. Every field optional: an absent one means
/// "whatever the dialog already does", which is what makes a partial manifest useful.
///
/// The runtime still has no opinion about colour - it forwards these - but it does
/// VALIDATE them, because the alternative is a typo in a hand-edited manifest reaching a
/// brush parser inside the presenter's event handler, where there is nothing to catch it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
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

/// The reveal animations the presenter actually implements
/// (`app/VoiceCoreTray/Dialog/AppConfig.cs`, `enum RevealStyle`, consumed by
/// `DialogWindow.Reveal`). The spec used to claim `instant | per-char`, which named
/// nothing that exists in any build; this list is the truth, and validating against it
/// here is what turns a typo into a message instead of a silent fallback to typewriter.
pub const REVEALS: [&str; 3] = ["typewriter", "sweep", "fade"];

/// Upper bound on a dwell. Ten minutes of caption is already absurd, so past it the
/// value is a mistake with a recognisable shape: milliseconds typed into a seconds field.
const MAX_DISPLAY_SECONDS: f64 = 600.0;

/// `#rgb`, `#rrggbb` or `#aarrggbb`. Deliberately not a named-colour table: the
/// presenter parses these into ARGB and a name it does not know would be a colour that
/// validates here and disappears there.
fn is_hex_color(value: &str) -> bool {
    let Some(body) = value.strip_prefix('#') else {
        return false;
    };
    matches!(body.len(), 3 | 6 | 8) && body.bytes().all(|b| b.is_ascii_hexdigit())
}

fn check_color(field: &str, value: Option<&String>) -> Result<(), String> {
    match value {
        Some(value) if !is_hex_color(value) => Err(format!(
            "dialog.{field} '{value}' is not a colour: write #rgb, #rrggbb or #aarrggbb"
        )),
        _ => Ok(()),
    }
}

fn check_reveal(value: Option<&String>) -> Result<(), String> {
    match value {
        Some(value) if !REVEALS.contains(&value.as_str()) => Err(format!(
            "dialog.reveal '{value}' is not a reveal mode: one of {}",
            REVEALS.join(", ")
        )),
        _ => Ok(()),
    }
}

fn check_display_seconds(value: Option<f64>) -> Result<(), String> {
    match value {
        Some(value) if !(value > 0.0 && value <= MAX_DISPLAY_SECONDS) => Err(format!(
            "dialog.displaySeconds {value} is out of range: greater than 0 and at most {MAX_DISPLAY_SECONDS}"
        )),
        _ => Ok(()),
    }
}

impl DialogStyle {
    /// Nothing was asked for, so every tier below this one answers.
    pub fn is_empty(&self) -> bool {
        self.name_color.is_none()
            && self.text_color.is_none()
            && self.ruby_color.is_none()
            && self.countdown_color.is_none()
            && self.reveal.is_none()
            && self.display_seconds.is_none()
    }

    /// One tier over another, field by field: `self` wins where it spoke.
    ///
    /// This is `Option::or` six times, and it is the ONLY place the runtime layers
    /// dialog tiers - per-call over pack over `config.json` - so the precedence exists
    /// once instead of once per consumer.
    pub fn or(self, fallback: &DialogStyle) -> DialogStyle {
        DialogStyle {
            name_color: self.name_color.or_else(|| fallback.name_color.clone()),
            text_color: self.text_color.or_else(|| fallback.text_color.clone()),
            ruby_color: self.ruby_color.or_else(|| fallback.ruby_color.clone()),
            countdown_color: self
                .countdown_color
                .or_else(|| fallback.countdown_color.clone()),
            reveal: self.reveal.or_else(|| fallback.reveal.clone()),
            display_seconds: self.display_seconds.or(fallback.display_seconds),
        }
    }

    /// The first thing wrong with it, named, or `Ok`. For a value that came from a
    /// CALLER: it gets refused, because a request the runtime silently reinterpreted is
    /// the bug this project keeps deleting.
    pub fn check(&self) -> Result<(), String> {
        check_color("nameColor", self.name_color.as_ref())?;
        check_color("textColor", self.text_color.as_ref())?;
        check_color("rubyColor", self.ruby_color.as_ref())?;
        check_color("countdownColor", self.countdown_color.as_ref())?;
        check_reveal(self.reveal.as_ref())?;
        check_display_seconds(self.display_seconds)
    }

    /// Drop whatever does not pass, saying so once per field. For a value that came from
    /// a FILE: a mistyped colour must not cost the pack its reveal mode, and refusing the
    /// pack outright would make one bad line look like an uninstalled voice.
    fn sanitize(&mut self, whose: &str) {
        for (field, slot) in [
            ("nameColor", &mut self.name_color),
            ("textColor", &mut self.text_color),
            ("rubyColor", &mut self.ruby_color),
            ("countdownColor", &mut self.countdown_color),
        ] {
            if let Err(why) = check_color(field, slot.as_ref()) {
                eprintln!("{whose}: {why}; ignoring that field");
                *slot = None;
            }
        }
        if let Err(why) = check_reveal(self.reveal.as_ref()) {
            eprintln!("{whose}: {why}; ignoring that field");
            self.reveal = None;
        }
        if let Err(why) = check_display_seconds(self.display_seconds) {
            eprintln!("{whose}: {why}; ignoring that field");
            self.display_seconds = None;
        }
    }
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

/// How a pack wants to be spoken: the engine's caption channel, which is a separate
/// conditioning head from the text (`use_caption_condition` in the v4.1-Small
/// checkpoint), and how hard to steer with it.
///
/// This is what lets a voice be "this character speaks softly" without every caller
/// repeating it. Same rule as the other two sections: absent means the engine default.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionPrefs {
    /// Caption text. Free prose the model was trained to read as style, plus the
    /// checkpoint's 45 emoji annotations (see `skills/voice-core-tts/SKILL.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emotion: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cfg_scale_caption: Option<f64>,
}

/// Inclusive bound on `cfgScaleCaption`. The engine's own default is 3.0 and every CFG
/// scale it takes is single-digit; 0 turns caption guidance off, and past 10 the sampler
/// produces artefacts rather than more emotion. Out of range is refused, never clamped:
/// a clamp is a request the caller never made, answered as if they had.
const CFG_SCALE_CAPTION_RANGE: std::ops::RangeInclusive<f64> = 0.0..=10.0;

/// Longest caption accepted. The checkpoint's own budget is 512 TOKENS
/// (`max_caption_len` in its config), and this field is a style annotation of a few
/// words - so a value this long is already a mistake, and refusing it here is how a
/// pasted paragraph fails loudly instead of being truncated inside the tokenizer.
const MAX_EMOTION_CHARS: usize = 512;

impl ExpressionPrefs {
    pub fn is_empty(&self) -> bool {
        self.emotion.is_none() && self.cfg_scale_caption.is_none()
    }

    pub fn check(&self) -> Result<(), String> {
        if let Some(emotion) = self.emotion.as_deref() {
            let chars = emotion.chars().count();
            if chars > MAX_EMOTION_CHARS {
                return Err(format!(
                    "expression.emotion is {chars} characters, over the {MAX_EMOTION_CHARS} this engine's caption channel accepts"
                ));
            }
        }
        match self.cfg_scale_caption {
            Some(scale) if !CFG_SCALE_CAPTION_RANGE.contains(&scale) => Err(format!(
                "expression.cfgScaleCaption {scale} is out of range: {}..={}",
                CFG_SCALE_CAPTION_RANGE.start(),
                CFG_SCALE_CAPTION_RANGE.end()
            )),
            _ => Ok(()),
        }
    }

    fn sanitize(&mut self, whose: &str) {
        if let Err(why) = self.check() {
            eprintln!("{whose}: {why}; ignoring that section");
            *self = Self::default();
        }
    }
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
    #[serde(default)]
    pub expression: Option<ExpressionPrefs>,
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
    /// The app-wide subtitle appearance: the tier under every pack. The tray owns the
    /// rest of this section (`annotationAbove` is a layout choice no pack overrides), and
    /// serde drops what it does not know, so reading it here takes only the fields a pack
    /// could also have asked for.
    #[serde(default)]
    dialog: Option<DialogStyle>,
}

pub struct Registry {
    file: PathBuf,
    data_dir: PathBuf,
    packs: Vec<VoicePack>,
    /// `config.json`'s own `dialog` section, validated: the tier below every pack, and
    /// what makes the panel's settings page honest without a second file.
    dialog: DialogStyle,
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

/// Which file to name when a merged value has to be reported as wrong: the one the merge
/// took it from, which is the only file editing can fix it in.
fn blame(
    sources: &BTreeMap<String, Source>,
    key: &str,
    manifest: Option<&str>,
    config: &Path,
) -> String {
    match (sources.get(key), manifest) {
        (Some(Source::Pack), Some(path)) => path.to_string(),
        _ => native(config),
    }
}

impl Registry {
    pub fn load(file: PathBuf, data_dir: PathBuf) -> Self {
        let mut registry = Self {
            file,
            data_dir,
            packs: Vec::new(),
            loaded_from: None,
            dialog: DialogStyle::default(),
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
                    let mut dialog = section.dialog.unwrap_or_default();
                    dialog.sanitize(&native(&self.file));
                    self.dialog = dialog;
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
                self.dialog = DialogStyle::default();
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

    /// The app-wide dialog tier, under every pack and over the presenter's built-ins.
    /// Reloaded with the rest of the file, so editing `config.json` by hand or from the
    /// panel changes the next utterance's appearance without restarting anything.
    pub fn dialog(&self) -> &DialogStyle {
        &self.dialog
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

        let mut sources = BTreeMap::new();
        let name =
            pick(&mut sources, "name", m.name, entry.name).unwrap_or_else(|| entry.id.clone());
        let kind = pick(&mut sources, "kind", m.kind, entry.kind)
            .unwrap_or_else(|| PackKind::infer(&path));
        let languages =
            pick(&mut sources, "languages", m.languages, entry.languages).unwrap_or_default();
        let engine = pick(&mut sources, "engine", m.engine, entry.engine).unwrap_or_default();
        let character = pick(&mut sources, "character", m.character, entry.character);
        // Validated after the pick, and the complaint names the file that actually lost
        // the field: sending someone to `config.json` for a line they wrote in
        // `voicepack.json` is worse than saying nothing at all. A section left empty by
        // that scrub reverts to `derived`, for the same reason `avatar` does below - the
        // panel must never point at a value the runtime threw away.
        let mut dialog = pick(&mut sources, "dialog", m.dialog, entry.dialog);
        if let Some(style) = dialog.as_mut() {
            style.sanitize(&blame(&sources, "dialog", manifest_path.as_deref(), &self.file));
            if style.is_empty() {
                dialog = None;
                sources.insert("dialog".to_string(), Source::Derived);
            }
        }
        let synthesis = pick(&mut sources, "synthesis", m.synthesis, entry.synthesis);
        let mut expression = pick(&mut sources, "expression", m.expression, entry.expression);
        if let Some(prefs) = expression.as_mut() {
            prefs.sanitize(&blame(
                &sources,
                "expression",
                manifest_path.as_deref(),
                &self.file,
            ));
            if prefs.is_empty() {
                expression = None;
                sources.insert("expression".to_string(), Source::Derived);
            }
        }
        // Where a pack lives is the one thing the registry is authoritative about, so
        // there is nothing to pick between: an entry without a path never deserialized.
        sources.insert("path".to_string(), Source::Config);

        // Two different bases, which is why this is resolved here and not downstream: a
        // manifest avatar is relative to the pack, a registry avatar is relative to the
        // data dir (that is where the retired `data/avatars/` layout put it).
        //
        // It is also the one field decided in two steps, and the record follows both: a
        // pack that names a portrait it did not ship has no effective avatar, and calling
        // that `pack` would send someone to a manifest line the runtime threw away.
        let avatar = match (m.avatar.as_deref(), entry.avatar.as_deref()) {
            (Some(value), _) => {
                let base = if path.is_dir() {
                    path.clone()
                } else {
                    path.parent().map(Path::to_path_buf).unwrap_or_else(|| path.clone())
                };
                Some((absolute(&base, value), Source::Pack))
            }
            (None, Some(value)) => Some((absolute(&self.data_dir, value), Source::Config)),
            (None, None) => None,
        }
        .filter(|(shown, _)| Path::new(shown).exists());
        sources.insert(
            "avatar".to_string(),
            avatar.as_ref().map_or(Source::Derived, |(_, source)| *source),
        );

        VoicePack {
            id: entry.id,
            name,
            languages,
            kind,
            path: native(&path),
            engine,
            character,
            avatar: avatar.map(|(shown, _)| shown),
            dialog,
            synthesis,
            expression,
            manifest: manifest_path,
            sources,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DialogStyle, ExpressionPrefs, PackKind, Registry, Source};
    use std::collections::BTreeMap;
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

    /// The expected provenance map, spelled one line per field. The array length is fixed
    /// on purpose: a field that quietly stopped being recorded would render as a blank row
    /// in the settings screen, and here it is a compile error instead.
    fn expect_sources(pairs: [(&str, Source); 10]) -> BTreeMap<String, Source> {
        pairs.into_iter().map(|(key, source)| (key.to_string(), source)).collect()
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
        // Four fields the pack decided, two the entry did, four no file mentioned.
        assert_eq!(
            pack.sources,
            expect_sources([
                ("avatar", Source::Pack),
                ("character", Source::Pack),
                ("dialog", Source::Derived),
                ("expression", Source::Derived),
                ("engine", Source::Config),
                ("kind", Source::Derived),
                ("languages", Source::Pack),
                ("name", Source::Pack),
                ("path", Source::Config),
                ("synthesis", Source::Derived),
            ])
        );

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
        // Asserted as JSON rather than as the enum because the wire words are what the
        // panel puts in a chip: only `path` and the one field the entry states come from
        // `config`, and nothing at all comes from a pack that wrote no manifest.
        assert_eq!(
            serde_json::to_string(&pack.sources).unwrap(),
            r#"{"avatar":"derived","character":"derived","dialog":"derived","engine":"derived","expression":"derived","kind":"derived","languages":"derived","name":"config","path":"config","synthesis":"derived"}"#
        );

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
        // `avatar` is the case worth pinning: the manifest named a portrait, the file is
        // not there, so the effective value is the built-in fallback and the record says
        // `derived` instead of pointing at a manifest line the runtime discarded.
        assert_eq!(
            pack.sources,
            expect_sources([
                ("avatar", Source::Derived),
                ("character", Source::Pack),
                ("dialog", Source::Derived),
                ("expression", Source::Derived),
                ("engine", Source::Derived),
                ("kind", Source::Derived),
                ("languages", Source::Derived),
                ("name", Source::Derived),
                ("path", Source::Config),
                ("synthesis", Source::Derived),
            ])
        );

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn dialog_tiers_layer_without_losing_a_field() {
        let call = DialogStyle {
            reveal: Some("fade".into()),
            ..DialogStyle::default()
        };
        let pack = DialogStyle {
            reveal: Some("sweep".into()),
            name_color: Some("#ff0000".into()),
            ..DialogStyle::default()
        };
        let config = DialogStyle {
            name_color: Some("#00ff00".into()),
            text_color: Some("#0000ff".into()),
            display_seconds: Some(9.0),
            ..DialogStyle::default()
        };

        let merged = call.or(&pack).or(&config);

        // Each field is decided by the highest tier that spoke about THAT field, which is
        // the whole point: a pack stating one colour must not blank the rest of the theme.
        assert_eq!(merged.reveal.as_deref(), Some("fade"));
        assert_eq!(merged.name_color.as_deref(), Some("#ff0000"));
        assert_eq!(merged.text_color.as_deref(), Some("#0000ff"));
        assert_eq!(merged.display_seconds, Some(9.0));
        // Nothing claimed it, so it stays absent for the presenter's built-in to answer.
        assert_eq!(merged.ruby_color, None);
        assert!(!merged.is_empty());
        assert!(DialogStyle::default().is_empty());
    }

    #[test]
    fn a_reveal_mode_the_presenter_cannot_play_is_named_not_guessed() {
        let style = DialogStyle {
            reveal: Some("instant".into()),
            ..DialogStyle::default()
        };
        let why = style.check().unwrap_err();
        // `instant` is what the old spec claimed; the message has to say what IS playable,
        // because silently falling back to typewriter is the bug this replaced.
        assert!(why.contains("instant"), "{why}");
        assert!(why.contains("typewriter, sweep, fade"), "{why}");

        for good in super::REVEALS {
            DialogStyle {
                reveal: Some(good.into()),
                ..DialogStyle::default()
            }
            .check()
            .unwrap();
        }
    }

    #[test]
    fn colours_and_dwells_are_checked_by_shape() {
        for good in ["#fff", "#a48bff", "#D98B6CEF"] {
            DialogStyle {
                name_color: Some(good.into()),
                ..DialogStyle::default()
            }
            .check()
            .unwrap();
        }
        for bad in ["a48bff", "#a48bf", "violet", "#gggggg"] {
            let why = DialogStyle {
                text_color: Some(bad.into()),
                ..DialogStyle::default()
            }
            .check()
            .unwrap_err();
            assert!(why.contains("textColor"), "{bad}: {why}");
        }
        // Zero would be a caption that is gone before it is read, and 6000 is milliseconds
        // typed into a seconds field.
        for bad in [0.0, -1.0, 6000.0] {
            DialogStyle {
                display_seconds: Some(bad),
                ..DialogStyle::default()
            }
            .check()
            .unwrap_err();
        }
    }

    #[test]
    fn an_out_of_range_caption_scale_is_refused_rather_than_clamped() {
        for bad in [-0.5, 10.5] {
            let why = ExpressionPrefs {
                emotion: Some("😭".into()),
                cfg_scale_caption: Some(bad),
            }
            .check()
            .unwrap_err();
            assert!(why.contains("cfgScaleCaption"), "{why}");
            assert!(why.contains("0..=10"), "{why}");
        }
        // The engine's own default and both ends of the range are legal.
        for good in [0.0, 3.0, 10.0] {
            ExpressionPrefs {
                emotion: None,
                cfg_scale_caption: Some(good),
            }
            .check()
            .unwrap();
        }
    }

    #[test]
    fn expression_merges_like_every_other_section() {
        let dir = scratch("expression-merge");
        write(
            &dir.join("config.json"),
            r#"{ "voicePacks": [
                 { "id": "p", "path": "voicepacks/p",
                   "expression": { "emotion": "seeded", "cfgScaleCaption": 1.0 } }
               ] }"#,
        );
        write(
            &dir.join("voicepacks/p/voicepack.json"),
            r#"{ "schema": 1, "expression": { "emotion": "🫶🫶 優しく" } }"#,
        );

        let registry = Registry::load(dir.join("config.json"), dir.clone());
        let pack = &registry.all()[0];

        // The whole section is one field to the merge, exactly like `dialog` and
        // `synthesis`: the pack wrote one, so the pack's is the effective one and the
        // entry's `cfgScaleCaption` does not leak into it.
        let expression = pack.expression.as_ref().unwrap();
        assert_eq!(expression.emotion.as_deref(), Some("🫶🫶 優しく"));
        assert_eq!(expression.cfg_scale_caption, None);
        assert_eq!(pack.sources.get("expression"), Some(&Source::Pack));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn one_bad_colour_costs_only_that_field() {
        let dir = scratch("dialog-typo");
        write(
            &dir.join("config.json"),
            r#"{ "dialog": { "reveal": "sweep", "nameColor": "puce" },
                 "voicePacks": [{ "id": "p", "path": "voicepacks/p" }] }"#,
        );
        write(
            &dir.join("voicepacks/p/voicepack.json"),
            // `r##` because the body contains `"#`, which would close an `r#` literal.
            r##"{ "schema": 1, "dialog": { "reveal": "nope", "textColor": "#f2f2f2" } }"##,
        );

        let registry = Registry::load(dir.join("config.json"), dir.clone());

        // A mistyped value is dropped and complained about; the rest of the section
        // survives, because refusing the pack over one line would look like an uninstall.
        let dialog = registry.all()[0].dialog.as_ref().unwrap();
        assert_eq!(dialog.reveal, None);
        assert_eq!(dialog.text_color.as_deref(), Some("#f2f2f2"));
        assert_eq!(registry.dialog().reveal.as_deref(), Some("sweep"));
        assert_eq!(registry.dialog().name_color, None);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
