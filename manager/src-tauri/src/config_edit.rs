//! `data/config.json` — and the two files beside it — without destroying them.
//!
//! Those files are JSONC: comments explaining every key, a trailing comma left
//! behind after deleting a line, possibly a BOM from Notepad or PowerShell 5.1's
//! `Set-Content -Encoding UTF8`. A human reads those comments — the tray's
//! settings entry opens the file in an editor — so a file is never reserialised.
//! Instead the byte span of exactly the value being changed is located and only
//! that span is replaced: every comment, the key order, the indentation, the BOM
//! and every section this app knows nothing about come out byte-identical.
//!
//! That is why a write addresses a **leaf**, not a section. `dialog.reveal` is
//! spliced on its own; the six lines of Chinese prose explaining the three reveal
//! styles sit inside the `dialog` object and would be inside the span of any write
//! that replaced the section. Saving a whole form is therefore N leaf splices over
//! one read and one atomic rename, which costs nothing at these file sizes and is
//! the difference between a settings screen and a formatter that eats the user's
//! notes. The `voicePacks` array is the one span with structure in it, and comments
//! *inside* it are still the one casualty — the shipped file keeps its explanation
//! above the key, where it survives.
//!
//! The JSONC rules are re-implemented here rather than borrowed from the runtime
//! crate. This app is an HTTP client of that daemon, not a linked part of it, and
//! linking axum, clap and a tokio server to reuse thirty lines of text handling
//! would invert the seam ADR-0001 draws. The three rules are the same ones the
//! runtime applies when it reads this same file: a BOM is expected input,
//! comments are whitespace, one dangling comma is forgivable.

use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::config_view::{PackConfig, Settings};
use crate::contract::Pack;
use crate::host::Host;

pub fn config_path(host: &Host) -> PathBuf {
    host.data_dir.join("config.json")
}

/// The runtime's own file. Separate from `config.json` because the runtime owns it —
/// `idleStopSecs` is the one key in it a user has a reason to change, and the 设置 screen
/// says out loud that it lands on the next service start.
pub fn runtime_path(host: &Host) -> PathBuf {
    host.data_dir.join("runtime.json")
}

/// The record of every change this app has made to those two files.
///
/// One file rather than a directory of copies: what a person wants back is the value they
/// just changed, not a 1.6 KiB snapshot of a file they can already read - and a copy per
/// write buys that at the price of a directory nobody can read at a glance. See
/// `HISTORY_LIMIT`.
pub fn history_path(host: &Host) -> PathBuf {
    host.data_dir.join("settings.history.jsonl")
}

/// Every registered pack, or an empty list when there is no config file yet.
///
/// A single malformed entry is skipped rather than failing the whole read: one
/// hand-edited pack should not make the panel claim there are no voices.
///
/// Each entry is hydrated the way the runtime hydrates it (`docs/voicepack-spec.md`):
/// the path is resolved against the data dir, and anything the entry does not say is
/// taken from the pack's own `voicepack.json`. Without this the panel would show a slim
/// entry as a pack with no engine and no languages, and hand its unresolved relative
/// path to `open_path`, which validates against real directories and would rightly
/// refuse it. This runs only while the runtime is down; when it is up, `/api/voices`
/// serves the merge and this is not called.
pub fn read_packs(host: &Host) -> Vec<Pack> {
    let Ok(raw) = std::fs::read_to_string(config_path(host)) else {
        return Vec::new();
    };
    let Ok(root) = serde_json::from_str::<Value>(&normalize(&raw)) else {
        host.log("config.json could not be parsed; treating the pack registry as empty");
        return Vec::new();
    };
    let Some(items) = root.get("voicePacks").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut packs = Vec::with_capacity(items.len());
    for item in items {
        match serde_json::from_value::<Pack>(item.clone()) {
            Ok(mut pack) => {
                hydrate(host, &mut pack);
                packs.push(pack);
            }
            Err(err) => host.log(&format!("skipping unreadable voice pack entry: {err}")),
        }
    }
    packs
}

/// Absolute payload path, plus whatever the pack's manifest says.
///
/// The manifest wins over the entry, for the reason the runtime's copy of this states:
/// the registry is generated (installer seed, panel registration) and must not outrank
/// the pack's own description. Fields the manifest omits fall back to the entry, then to
/// a program default.
fn hydrate(host: &Host, pack: &mut Pack) {
    let raw = PathBuf::from(&pack.path);
    let path = if raw.is_absolute() {
        raw
    } else {
        host.data_dir.join(raw)
    };
    pack.path = native(&path);

    let Some(manifest) = manifest_beside(&path) else {
        // No manifest: the entry is all there is, so only the program defaults apply.
        if pack.name.is_empty() {
            pack.name = pack.id.clone();
        }
        if pack.kind.is_empty() {
            pack.kind = infer_kind(&path);
        }
        return;
    };
    let text = |key: &str| manifest.get(key).and_then(Value::as_str).map(str::to_string);

    pack.name = text("name")
        .or_else(|| Some(pack.name.clone()).filter(|value| !value.is_empty()))
        .unwrap_or_else(|| pack.id.clone());
    pack.kind = text("kind")
        .or_else(|| Some(pack.kind.clone()).filter(|value| !value.is_empty()))
        .unwrap_or_else(|| infer_kind(&path));
    if let Some(engine) = text("engine") {
        pack.engine = engine;
    }
    if let Some(languages) = manifest.get("languages").and_then(Value::as_array) {
        pack.languages = languages.iter().filter_map(Value::as_str).map(str::to_string).collect();
    }
    if let Some(character) = text("character") {
        pack.character = Some(character);
    }
    if let Some(avatar) = text("avatar") {
        // Relative to the pack, which is what makes a pack movable.
        let base = if path.is_dir() {
            path.clone()
        } else {
            path.parent().unwrap_or(&path).to_path_buf()
        };
        pack.avatar = Some(native(&base.join(avatar)));
    } else if let Some(avatar) = pack.avatar.clone() {
        pack.avatar = Some(native(&host.data_dir.join(avatar)));
    }
}

/// The three-way split the panel's picker makes, from the payload on disk.
fn infer_kind(path: &Path) -> String {
    if path.is_dir() {
        return "lora-adapter".to_string();
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    if [".wav", ".flac", ".mp3", ".ogg", ".m4a"].iter().any(|ext| name.ends_with(ext)) {
        "reference-audio".to_string()
    } else {
        "speaker-embedding".to_string()
    }
}

/// Windows-native separators, because these strings are shown and pasted.
#[cfg(windows)]
pub(crate) fn native(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\")
}

#[cfg(not(windows))]
pub(crate) fn native(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

/// The file that would hold the manifest for the payload at `path`, there or not.
///
/// A directory pack keeps it inside: `<dir>/voicepack.json`. A single-file pack has no
/// inside, so it gets a sibling named after the file with its last extension replaced:
/// `miyu.speaker.safetensors` -> `miyu.speaker.voicepack.json`.
///
/// Separate from reading it because the 配置 screen shows *which* file it read and how
/// big that file is, and this naming rule gets exactly one owner in this crate.
pub(crate) fn manifest_file(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        return Some(path.join("voicepack.json"));
    }
    let stem = path.file_stem()?.to_string_lossy().to_string();
    Some(path.parent()?.join(format!("{stem}.voicepack.json")))
}

/// The pack's own manifest as raw JSON, or None when it has none.
///
/// Returned as a `Value` rather than a typed struct on purpose: the panel shows this
/// file to the user verbatim, and a field this build has never heard of has to survive
/// the round trip to the screen.
pub fn manifest_beside(path: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(manifest_file(path)?).ok()?;
    serde_json::from_str::<Value>(&normalize(&raw)).ok()
}

/// Register a pack: describe it in its own manifest, and put a pointer in the registry.
///
/// The split follows the precedence (`docs/voicepack-spec.md`). The manifest is where a
/// pack's description belongs and it is what wins at read time, so writing the panel's
/// form into the registry instead would produce an edit that silently does nothing. The
/// registry keeps what it is actually authoritative about: which packs exist and where.
///
/// A pack whose directory cannot be written (read-only media, a share) still registers -
/// the fields go into the registry entry, which the merge falls back to. That is a
/// degraded but honest outcome, and it is reported.
#[tauri::command]
pub async fn register_pack(app: AppHandle, pack: Pack) -> Result<(), String> {
    let host = app.state::<Host>();

    let raw = PathBuf::from(&pack.path);
    let path = if raw.is_absolute() { raw } else { host.data_dir.join(raw) };
    let described = write_manifest(&host, &path, &pack).is_ok();

    // Relative to the data dir when it lives under it, which is what keeps the tree
    // movable; absolute when the pack is somewhere else on this machine.
    let pointer = match path.strip_prefix(&host.data_dir) {
        Ok(rest) => rest.to_string_lossy().replace('\\', "/"),
        Err(_) => native(&path),
    };

    let entry = if described {
        Pack {
            id: pack.id.clone(),
            path: pointer,
            name: String::new(),
            kind: String::new(),
            engine: String::new(),
            languages: Vec::new(),
            character: None,
            avatar: None,
        }
    } else {
        host.log("register_pack: could not write the pack's manifest; keeping the description in config.json");
        Pack { path: pointer, ..pack.clone() }
    };

    let mut packs = read_packs(&host);
    match packs.iter_mut().find(|existing| existing.id == entry.id) {
        Some(existing) => *existing = entry,
        None => packs.push(entry),
    }
    write_packs(&host, &packs)
}

/// `voicepack.json` inside the pack, or a sidecar for a single-file pack.
///
/// Merged into whatever is already there rather than overwritten: a hand-written
/// manifest may carry `dialog`, `synthesis` or fields this build has never heard of, and
/// registering a pack again must not delete them.
fn write_manifest(host: &Host, path: &Path, pack: &Pack) -> std::io::Result<()> {
    let file = if path.is_dir() {
        path.join("voicepack.json")
    } else {
        let stem = path.file_stem().unwrap_or_default().to_string_lossy().to_string();
        match path.parent() {
            Some(parent) => parent.join(format!("{stem}.voicepack.json")),
            None => return Err(std::io::Error::other("pack has no parent directory")),
        }
    };

    let mut root = match std::fs::read_to_string(&file) {
        Ok(raw) => serde_json::from_str::<Value>(&normalize(&raw)).unwrap_or_else(|_| Value::Object(Default::default())),
        Err(_) => Value::Object(Default::default()),
    };
    let Some(object) = root.as_object_mut() else {
        return Err(std::io::Error::other("manifest is not an object"));
    };

    object.insert("schema".into(), Value::from(1));
    object.insert("id".into(), Value::from(pack.id.clone()));
    object.insert("name".into(), Value::from(pack.name.clone()));
    object.insert("engine".into(), Value::from(pack.engine.clone()));
    object.insert("kind".into(), Value::from(pack.kind.clone()));
    object.insert("languages".into(), Value::from(pack.languages.clone()));
    match pack.character.as_ref() {
        Some(character) => object.insert("character".into(), Value::from(character.clone())),
        None => object.remove("character"),
    };
    match pack.avatar.as_ref() {
        Some(avatar) => object.insert("avatar".into(), Value::from(avatar.clone())),
        None => object.remove("avatar"),
    };

    let body = serde_json::to_string_pretty(&root).unwrap_or_default();
    std::fs::write(&file, format!("{body}\n"))?;
    host.log(&format!("register_pack: described {} in {}", pack.id, file.display()));
    Ok(())
}

/// Remove a pack. Removing one that is not there is not an error — the caller's
/// list was merely stale, and the end state it asked for is the end state.
#[tauri::command]
pub async fn remove_pack(app: AppHandle, id: String) -> Result<(), String> {
    let host = app.state::<Host>();
    let mut packs = read_packs(&host);
    let before = packs.len();
    packs.retain(|pack| pack.id != id);
    if packs.len() == before {
        host.log(&format!("remove_pack: no pack with id {id}"));
        return Ok(());
    }
    write_packs(&host, &packs)
}

/// Copy a picked image into the pack and return the file name to put in its manifest.
///
/// Into the pack, not into a folder beside it: the manifest's `avatar` is relative to
/// the pack, so the portrait travels with the voice. A pack that points at a picture
/// somewhere else on this machine loses its face the moment the tree is copied.
#[tauri::command]
pub async fn import_avatar(app: AppHandle, path: String, pack_path: String) -> Result<String, String> {
    let host = app.state::<Host>();
    let source = PathBuf::from(&path);
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .map(|value| value.to_ascii_lowercase())
        .filter(|value| matches!(value.as_str(), "png" | "jpg" | "jpeg" | "webp" | "bmp"))
        .ok_or("头像必须是 png / jpg / webp / bmp")?;

    let raw = PathBuf::from(&pack_path);
    let pack = if raw.is_absolute() { raw } else { host.data_dir.join(raw) };
    // A single-file pack has no inside, so its portrait sits beside it under the pack's
    // own stem - the same rule its manifest sidecar follows.
    let (dir, stem) = if pack.is_dir() {
        (pack.clone(), "avatar".to_string())
    } else {
        let parent = pack.parent().ok_or("这个音色包没有所在目录")?.to_path_buf();
        let stem = pack.file_stem().unwrap_or_default().to_string_lossy().to_string();
        (parent, stem)
    };

    let name = format!("{stem}.{extension}");
    let target = dir.join(&name);
    std::fs::copy(&source, &target).map_err(|err| format!("复制头像失败：{err}"))?;
    host.log(&format!("import_avatar: {} -> {}", source.display(), target.display()));
    Ok(name)
}

/// The file this app writes when there is none yet.
///
/// Minimal on purpose: the presenter and the runtime both default every key they do not
/// find, and copying their documented templates here would give each template two owners.
/// No BOM, matching what the rest of the project writes.
fn seed_config(packs: &[Pack]) -> String {
    format!(
        "// voice-core 设置。注释和尾随逗号都会被保留。\n{{\n  \"voicePacks\": {}\n}}\n",
        render(packs)
    )
}

fn write_packs(host: &Host, packs: &[Pack]) -> Result<(), String> {
    let path = config_path(host);
    let updated = match std::fs::read_to_string(&path) {
        // The registry is generated, so what it replaced is of no interest to anybody: only
        // the 设置 screen's leaves are recorded.
        Ok(raw) => splice_at(&raw, &["voicePacks"], &render(packs))?.0,
        Err(_) => seed_config(packs),
    };
    replace(&path, &updated)
}

/// Write `text` over `path` without any reader ever seeing a prefix.
///
/// Beside the target and renamed, because both the runtime and the presenter reload these
/// files on mtime change: an in-place write is observable as zero bytes or half an array.
fn replace(path: &Path, text: &str) -> Result<(), String> {
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, text.as_bytes())
        .map_err(|err| format!("writing {}: {err}", temp.display()))?;
    std::fs::rename(&temp, path).map_err(|err| format!("replacing {}: {err}", path.display()))
}

/// Replace the value at `path` with `rendered`, inserting the member — and any missing
/// object on the way to it — when it is not there. Answers with the new text and with what
/// was there, which is the edit that puts it back.
///
/// `rendered` is already-formatted JSON text, indented for where it will land. Everything
/// outside the value's own byte span comes out unchanged, which is the entire contract of
/// this module.
fn splice_at(raw: &str, path: &[&str], rendered: &str) -> Result<(String, Leaf), String> {
    let normalized = normalize(raw);
    let target = locate(&normalized, path).ok_or_else(|| {
        format!(
            "在这个文件里找不到能安全改写 {} 的位置：它不是本程序愿意盲目重写的 JSON 对象。请先修好或删掉它。",
            path.join(".")
        )
    })?;
    let mut out = String::with_capacity(raw.len() + rendered.len() + 64);
    let previous = match target {
        Target::Value(range) => {
            out.push_str(&raw[..range.start]);
            out.push_str(rendered);
            out.push_str(&raw[range.end..]);
            // The file's own bytes, comments inside an object value included, so putting
            // them back is a byte-for-byte restoration and not a re-rendering.
            Leaf::Set(raw[range].to_string())
        }
        Target::Insert { at, needs_comma, depth } => {
            out.push_str(&raw[..at]);
            if needs_comma {
                out.push(',');
            }
            let indent = "  ".repeat(depth + 1);
            out.push('\n');
            out.push_str(&indent);
            out.push_str(&nest(&path[depth..], rendered, depth + 1));
            out.push_str(&raw[at..]);
            // `depth` keys resolved, so `path[depth]` is the first one that had to be
            // created: that member is what this write added, and removing it is the inverse.
            Leaf::Absent { member: path[..=depth].iter().map(|key| (*key).to_string()).collect() }
        }
    };
    Ok((out, previous))
}

/// Remove the member at `path`: the inverse of the insertion arm above.
///
/// That arm writes an optional comma, a newline, one indent and the member, all immediately
/// before the byte it inserted at, so undoing it deletes from the end of the previous member
/// through the end of this one. The span is found again from the key rather than remembered
/// as an offset, because a hand edit between the write and the undo moves every offset in
/// the file and moves no key.
///
/// Whitespace is walked in `raw`, not in the normalized copy, so a comment somebody added
/// above the line since — whose bytes are spaces in the copy and prose in the file — stops
/// the walk instead of being swallowed by it.
fn splice_out(raw: &str, path: &[&str]) -> Result<String, String> {
    let normalized = normalize(raw);
    let Some(Target::Value(value)) = locate(&normalized, path) else {
        // Already gone. The end state asked for is the end state, which is the judgement
        // `remove_pack` makes about the same situation.
        return Ok(raw.to_string());
    };
    let bytes = raw.as_bytes();
    let key_at = key_start(normalized.as_bytes(), value.start).ok_or_else(|| {
        format!("在这个文件里找不到 {} 的键名，没有改动。", path.join("."))
    })?;

    let mut start = key_at;
    while start > 0 && bytes[start - 1].is_ascii_whitespace() {
        start -= 1;
    }
    let mut end = value.end;
    // The comma is confirmed against the normalized copy as well: one inside a comment is a
    // space there, and is not this member's separator.
    if start > 0 && bytes[start - 1] == b',' && normalized.as_bytes()[start - 1] == b',' {
        start -= 1;
    } else {
        // Nothing before it, so the comma this member brought is the one after it — leaving
        // that behind would produce `{,`.
        let mut after = end;
        while after < bytes.len() && bytes[after].is_ascii_whitespace() {
            after += 1;
        }
        if bytes.get(after) == Some(&b',') {
            end = after + 1;
        }
    }

    let mut out = String::with_capacity(raw.len());
    out.push_str(&raw[..start]);
    out.push_str(&raw[end..]);
    Ok(out)
}

/// The offset of the opening quote of the key whose value starts at `value_at`.
///
/// Walks back over the separator: whitespace, one `:`, whitespace, then the key literal.
/// Reads the normalized copy, where a comment is spaces and cannot be mistaken for any of
/// that.
fn key_start(bytes: &[u8], value_at: usize) -> Option<usize> {
    let mut at = value_at;
    while at > 0 && bytes[at - 1].is_ascii_whitespace() {
        at -= 1;
    }
    if at == 0 || bytes[at - 1] != b':' {
        return None;
    }
    at -= 1;
    while at > 0 && bytes[at - 1].is_ascii_whitespace() {
        at -= 1;
    }
    if at == 0 || bytes[at - 1] != b'"' {
        return None;
    }
    // Back to the opening quote, skipping one that is escaped.
    let mut quote = at - 1;
    while quote > 0 {
        quote -= 1;
        if bytes[quote] == b'"' {
            let mut slashes = 0;
            while quote > slashes && bytes[quote - 1 - slashes] == b'\\' {
                slashes += 1;
            }
            if slashes % 2 == 0 {
                return Some(quote);
            }
        }
    }
    None
}

/// `"a": { "b": <rendered> }` for the keys that were missing, at `depth` levels of indent.
///
/// Only ever called for a member that is absent, so it never has to preserve anything: the
/// bytes it produces are new.
fn nest(keys: &[&str], rendered: &str, depth: usize) -> String {
    let key = serde_json::to_string(keys[0]).unwrap_or_else(|_| format!("\"{}\"", keys[0]));
    if keys.len() == 1 {
        return format!("{key}: {rendered}");
    }
    let inner = "  ".repeat(depth + 1);
    let close = "  ".repeat(depth);
    format!(
        "{key}: {{\n{inner}{}\n{close}}}",
        nest(&keys[1..], rendered, depth + 1)
    )
}

/// The array as it will appear in the file: `serde_json`'s two-space pretty form,
/// re-indented by one level so it nests under a key at indent two.
fn render(packs: &[Pack]) -> String {
    let Ok(text) = serde_json::to_string_pretty(packs) else {
        return "[]".to_string();
    };
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            out.push('\n');
            out.push_str("  ");
        }
        out.push_str(line);
    }
    out
}

enum Target {
    /// Byte range of the existing value.
    Value(Range<usize>),
    /// Byte offset to insert at, whether a separating comma is needed there, and how many
    /// path keys were resolved before the missing one — which is both the indent level and
    /// the index of the first key that has to be created.
    Insert { at: usize, needs_comma: bool, depth: usize },
}

/// One member of one object: its value's span, or where it would have to be inserted.
enum Member {
    Value(Range<usize>),
    Absent { at: usize, needs_comma: bool },
}

/// JSONC → JSON **without moving a single byte**.
///
/// A BOM, a comment and a dangling comma each become spaces in place, one space
/// per byte, so every offset in the result is also an offset in the original —
/// which is what lets one pass serve both `serde_json` and the span search.
/// Newlines inside comments survive so a parse error still points at the line the
/// user is looking at. Replacing per byte rather than per character matters here:
/// the comments in this file are Chinese, and one space per *character* would
/// shift every offset after them.
pub(crate) fn normalize(raw: &str) -> String {
    let mut out = raw.as_bytes().to_vec();
    if out.starts_with(&[0xEF, 0xBB, 0xBF]) {
        out[..3].fill(b' ');
    }

    let mut i = 0;
    let mut in_string = false;
    let mut escaped = false;
    // The last comma seen with only whitespace and comments after it. A `}` or `]`
    // makes it trailing; anything else makes it a real separator.
    let mut pending_comma: Option<usize> = None;

    while i < out.len() {
        let byte = out[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            i += 1;
            continue;
        }
        match byte {
            b'"' => {
                in_string = true;
                pending_comma = None;
                i += 1;
            }
            b'/' if out.get(i + 1) == Some(&b'/') => {
                while i < out.len() && out[i] != b'\n' {
                    out[i] = b' ';
                    i += 1;
                }
            }
            b'/' if out.get(i + 1) == Some(&b'*') => {
                out[i] = b' ';
                i += 1;
                while i < out.len() {
                    if out[i] == b'*' && out.get(i + 1) == Some(&b'/') {
                        out[i] = b' ';
                        out[i + 1] = b' ';
                        i += 2;
                        break;
                    }
                    if out[i] != b'\n' {
                        out[i] = b' ';
                    }
                    i += 1;
                }
            }
            b',' => {
                pending_comma = Some(i);
                i += 1;
            }
            b'}' | b']' => {
                if let Some(comma) = pending_comma.take() {
                    out[comma] = b' ';
                }
                i += 1;
            }
            _ if byte.is_ascii_whitespace() => i += 1,
            _ => {
                pending_comma = None;
                i += 1;
            }
        }
    }

    // Only ASCII spaces were written, and only over whole comment byte ranges or
    // ASCII structural bytes, so the result is still valid UTF-8.
    String::from_utf8(out).unwrap_or_else(|_| raw.to_string())
}

/// Follow `path` from the top-level object down to the value it names.
///
/// Comments are already whitespace by the time this runs, so this only has to understand
/// JSON. A key missing anywhere along the way stops the walk and reports where the rest of
/// the path would have to be created; a key that is present but not an object where the
/// path expects one refuses, because guessing would mean overwriting whatever it is.
fn locate(normalized: &str, path: &[&str]) -> Option<Target> {
    let bytes = normalized.as_bytes();
    let mut scan = Scan { bytes, at: 0 };
    scan.whitespace();
    if scan.peek()? != b'{' {
        return None;
    }
    let mut object_at = scan.at;

    for (depth, key) in path.iter().enumerate() {
        match member(bytes, object_at, key)? {
            Member::Value(range) => {
                if depth + 1 == path.len() {
                    return Some(Target::Value(range));
                }
                if bytes.get(range.start) != Some(&b'{') {
                    return None;
                }
                object_at = range.start;
            }
            Member::Absent { at, needs_comma } => {
                return Some(Target::Insert { at, needs_comma, depth })
            }
        }
    }
    None
}

/// Walk one object's members looking for `key`. `object_at` is the offset of its `{`.
fn member(bytes: &[u8], object_at: usize, key: &str) -> Option<Member> {
    let mut scan = Scan { bytes, at: object_at };
    if scan.take()? != b'{' {
        return None;
    }
    let mut last_member_end = scan.at;
    let mut members = 0usize;

    loop {
        scan.whitespace();
        match scan.peek()? {
            b'}' => {
                return Some(Member::Absent {
                    at: last_member_end,
                    needs_comma: members > 0,
                })
            }
            b',' => scan.at += 1,
            b'"' => {
                let found = scan.string()?;
                scan.whitespace();
                if scan.take()? != b':' {
                    return None;
                }
                scan.whitespace();
                let start = scan.at;
                scan.value()?;
                let end = scan.at;
                if found == key {
                    return Some(Member::Value(start..end));
                }
                members += 1;
                last_member_end = end;
            }
            // Not an object this app understands; refuse rather than guess.
            _ => return None,
        }
    }
}

struct Scan<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Scan<'_> {
    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.at).copied()
    }

    fn take(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.at += 1;
        Some(byte)
    }

    fn whitespace(&mut self) {
        while matches!(self.peek(), Some(byte) if byte.is_ascii_whitespace()) {
            self.at += 1;
        }
    }

    /// Consume a string literal starting at the opening quote and return its raw
    /// contents. Only used for member keys, which never carry escapes worth
    /// decoding — an escaped key would simply not match `voicePacks`.
    fn string(&mut self) -> Option<String> {
        if self.take()? != b'"' {
            return None;
        }
        let start = self.at;
        let mut escaped = false;
        loop {
            let byte = self.take()?;
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return String::from_utf8(self.bytes[start..self.at - 1].to_vec()).ok();
            }
        }
    }

    /// Consume exactly one JSON value.
    fn value(&mut self) -> Option<()> {
        match self.peek()? {
            b'"' => {
                self.string()?;
                Some(())
            }
            b'{' | b'[' => {
                let mut depth = 0usize;
                let mut in_string = false;
                let mut escaped = false;
                while let Some(byte) = self.peek() {
                    if in_string {
                        if escaped {
                            escaped = false;
                        } else if byte == b'\\' {
                            escaped = true;
                        } else if byte == b'"' {
                            in_string = false;
                        }
                        self.at += 1;
                        continue;
                    }
                    match byte {
                        b'"' => in_string = true,
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                self.at += 1;
                                return Some(());
                            }
                        }
                        _ => {}
                    }
                    self.at += 1;
                }
                None
            }
            // A number, `true`, `false` or `null`: ends at the next structural
            // byte or whitespace.
            _ => {
                while let Some(byte) = self.peek() {
                    if byte == b',' || byte == b'}' || byte == b']' || byte.is_ascii_whitespace() {
                        break;
                    }
                    self.at += 1;
                }
                Some(())
            }
        }
    }
}

// --- the typed write surface the two forms drive --------------------------------------
//
// One command per file, and each takes ONE edit: a variant that names the field and
// carries its value. Not a patch struct with every field optional — that shape cannot
// tell "leave it alone" from "set it to null" without `Option<Option<T>>` on half its
// members, and it lets a caller ask for six writes in a call that can only report one
// error. A tagged enum makes the two impossible states impossible and puts the field's
// path, its rendering and its validation in the same arm.
//
// Validation happens here and again in the form. Two gates on purpose: a hand-edited file
// reaches this code without passing the form's, and a form that trusted the backend would
// have to round-trip to say "that is not a colour".

/// Which file a setting lives in.
enum Which {
    /// `data/config.json`: read by the runtime for `voicePacks`, by the presenter for
    /// everything else here, and reloaded by both on mtime change.
    Config,
    /// `data/runtime.json`: read once, at runtime startup.
    Runtime,
}

impl Which {
    /// Its file name, which is also how a recorded change names it.
    fn name(&self) -> &'static str {
        match self {
            Self::Config => "config.json",
            Self::Runtime => "runtime.json",
        }
    }
}

/// One setting the 设置 screen can change.
///
/// The wire form is `{ "field": "reveal", "value": "sweep" }`.
#[derive(Debug, Deserialize)]
#[serde(tag = "field", content = "value", rename_all = "camelCase")]
pub enum SettingEdit {
    AnnotationAbove(bool),
    Reveal(String),
    NameColor(String),
    TextColor(String),
    RubyColor(String),
    CountdownColor(String),
    DisplaySeconds(f64),
    ToggleDialog(String),
    ToggleHold(String),
    IdleStopSecs(u64),
}

impl SettingEdit {
    fn target(&self) -> (Which, &'static [&'static str]) {
        match self {
            Self::AnnotationAbove(_) => (Which::Config, &["dialog", "annotationAbove"]),
            Self::Reveal(_) => (Which::Config, &["dialog", "reveal"]),
            Self::NameColor(_) => (Which::Config, &["dialog", "nameColor"]),
            Self::TextColor(_) => (Which::Config, &["dialog", "textColor"]),
            Self::RubyColor(_) => (Which::Config, &["dialog", "rubyColor"]),
            Self::CountdownColor(_) => (Which::Config, &["dialog", "countdownColor"]),
            Self::DisplaySeconds(_) => (Which::Config, &["dialog", "displaySeconds"]),
            Self::ToggleDialog(_) => (Which::Config, &["hotkeys", "toggleDialog"]),
            Self::ToggleHold(_) => (Which::Config, &["hotkeys", "toggleHold"]),
            Self::IdleStopSecs(_) => (Which::Runtime, &["idleStopSecs"]),
        }
    }

    fn check(&self) -> Result<(), String> {
        match self {
            Self::AnnotationAbove(_) => Ok(()),
            Self::Reveal(value) => reveal(value),
            Self::NameColor(value)
            | Self::TextColor(value)
            | Self::RubyColor(value)
            | Self::CountdownColor(value) => colour(value),
            Self::DisplaySeconds(value) => seconds(*value),
            Self::ToggleDialog(value) | Self::ToggleHold(value) => hotkey(value),
            // 0 keeps the engine resident. The ceiling is a day, past which "release the
            // GPU when idle" has stopped meaning anything.
            Self::IdleStopSecs(value) => {
                if *value > 86_400 {
                    Err("空闲回收最多 86400 秒（一天）；0 表示常驻不回收。".to_string())
                } else {
                    Ok(())
                }
            }
        }
    }

    fn rendered(&self) -> String {
        match self {
            Self::AnnotationAbove(value) => value.to_string(),
            Self::Reveal(value)
            | Self::NameColor(value)
            | Self::TextColor(value)
            | Self::RubyColor(value)
            | Self::CountdownColor(value)
            | Self::ToggleDialog(value)
            | Self::ToggleHold(value) => quoted(value),
            Self::DisplaySeconds(value) => number(*value),
            Self::IdleStopSecs(value) => value.to_string(),
        }
    }
}

/// One field of one pack's manifest.
///
/// Every nullable arm writes an explicit `null` rather than deleting the key: `null` is
/// what the spec already means by "unset" (`"seed": null` is in it), the runtime and the
/// presenter both read it that way, and a line that says `"nameColor": null` tells the next
/// person reading the file that the field exists and is deliberately not set — which
/// removing it does not.
#[derive(Debug, Deserialize)]
#[serde(tag = "field", content = "value", rename_all = "camelCase")]
pub enum PackEdit {
    Name(String),
    Character(Option<String>),
    Kind(String),
    Languages(Vec<String>),
    Engine(String),
    Avatar(Option<String>),
    NameColor(Option<String>),
    TextColor(Option<String>),
    RubyColor(Option<String>),
    CountdownColor(Option<String>),
    Reveal(Option<String>),
    DisplaySeconds(Option<f64>),
    NumSteps(Option<u32>),
    Seed(Option<i64>),
    Temperature(Option<f64>),
    Emotion(Option<String>),
    CfgScaleCaption(Option<f64>),
}

impl PackEdit {
    fn path(&self) -> &'static [&'static str] {
        match self {
            Self::Name(_) => &["name"],
            Self::Character(_) => &["character"],
            Self::Kind(_) => &["kind"],
            Self::Languages(_) => &["languages"],
            Self::Engine(_) => &["engine"],
            Self::Avatar(_) => &["avatar"],
            Self::NameColor(_) => &["dialog", "nameColor"],
            Self::TextColor(_) => &["dialog", "textColor"],
            Self::RubyColor(_) => &["dialog", "rubyColor"],
            Self::CountdownColor(_) => &["dialog", "countdownColor"],
            Self::Reveal(_) => &["dialog", "reveal"],
            Self::DisplaySeconds(_) => &["dialog", "displaySeconds"],
            Self::NumSteps(_) => &["synthesis", "numSteps"],
            Self::Seed(_) => &["synthesis", "seed"],
            Self::Temperature(_) => &["synthesis", "temperature"],
            Self::Emotion(_) => &["expression", "emotion"],
            Self::CfgScaleCaption(_) => &["expression", "cfgScaleCaption"],
        }
    }

    fn check(&self) -> Result<(), String> {
        match self {
            Self::Name(value) => {
                if value.trim().is_empty() {
                    Err("名称不能为空。".to_string())
                } else {
                    Ok(())
                }
            }
            Self::Kind(value) => {
                if ["lora-adapter", "speaker-embedding", "reference-audio"].contains(&value.as_str())
                {
                    Ok(())
                } else {
                    Err(format!(
                        "kind 只能是 lora-adapter、speaker-embedding 或 reference-audio，收到 {value}"
                    ))
                }
            }
            Self::Languages(value) => {
                if value.is_empty() {
                    return Err("至少写一种语言，例如 ja。".to_string());
                }
                match value.iter().find(|tag| !is_language_tag(tag)) {
                    // A pack whose languages the runtime cannot match refuses every
                    // utterance with a language, so a typo here is not cosmetic.
                    Some(bad) => Err(format!("{bad} 不像语言标签；写 ja、zh 或 zh-CN 这种。")),
                    None => Ok(()),
                }
            }
            Self::Engine(value) => {
                if value.trim().is_empty() {
                    Err("引擎不能为空，Irodori 引擎填 irodori。".to_string())
                } else {
                    Ok(())
                }
            }
            Self::NameColor(Some(value))
            | Self::TextColor(Some(value))
            | Self::RubyColor(Some(value))
            | Self::CountdownColor(Some(value)) => colour(value),
            Self::Reveal(Some(value)) => reveal(value),
            Self::DisplaySeconds(Some(value)) => seconds(*value),
            Self::NumSteps(Some(value)) => {
                if (1..=200).contains(value) {
                    Ok(())
                } else {
                    Err("推理步数在 1 到 200 之间；默认 32。".to_string())
                }
            }
            Self::Temperature(Some(value)) => {
                if (0.0..=2.0).contains(value) {
                    Ok(())
                } else {
                    Err("temperature 在 0 到 2 之间。".to_string())
                }
            }
            // Expression's range, and the runtime refuses out-of-range rather than clamping,
            // so writing one would produce a pack that cannot speak.
            Self::CfgScaleCaption(Some(value)) => {
                if (0.0..=10.0).contains(value) {
                    Ok(())
                } else {
                    Err("cfgScaleCaption 在 0 到 10 之间；默认 3。".to_string())
                }
            }
            // Seed is any integer, emotion is free text (emoji written into the input), an
            // avatar name comes from `import_avatar`, and every `None` means "not set".
            _ => Ok(()),
        }
    }

    fn rendered(&self) -> String {
        match self {
            Self::Name(value) | Self::Kind(value) | Self::Engine(value) => quoted(value.trim()),
            Self::Languages(value) => {
                let items: Vec<String> = value.iter().map(|tag| quoted(tag.trim())).collect();
                format!("[{}]", items.join(", "))
            }
            Self::Character(value)
            | Self::Avatar(value)
            | Self::NameColor(value)
            | Self::TextColor(value)
            | Self::RubyColor(value)
            | Self::CountdownColor(value)
            | Self::Reveal(value)
            | Self::Emotion(value) => match value {
                Some(text) => quoted(text),
                None => "null".to_string(),
            },
            Self::DisplaySeconds(value) | Self::Temperature(value) | Self::CfgScaleCaption(value) => {
                value.map(|value| number(value)).unwrap_or_else(|| "null".to_string())
            }
            Self::NumSteps(value) => value.map(|v| v.to_string()).unwrap_or_else(|| "null".to_string()),
            Self::Seed(value) => value.map(|v| v.to_string()).unwrap_or_else(|| "null".to_string()),
        }
    }
}

/// A JSON string literal, escaped by the serializer rather than by hand: a pack whose
/// character name contains a quote must not be able to break the file.
fn quoted(value: &str) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "\"\"".to_string())
}

/// A number without a pointless `.0`, because a human reads these files: `6`, not `6.0`.
fn number(value: f64) -> String {
    if value.fract() == 0.0 && value.abs() < 1e15 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

fn reveal(value: &str) -> Result<(), String> {
    // The presenter implements exactly these three (`RevealStyle`), and the runtime rejects
    // anything else by name.
    if ["typewriter", "sweep", "fade"].contains(&value) {
        Ok(())
    } else {
        Err(format!(
            "文字出现方式只能是 typewriter、sweep 或 fade，收到 {value}"
        ))
    }
}

/// `#rgb`, `#rrggbb` or `#aarrggbb` — the presenter's own notation, alpha first.
fn colour(value: &str) -> Result<(), String> {
    let digits = value.strip_prefix('#').unwrap_or_default();
    if matches!(digits.len(), 3 | 6 | 8) && digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(format!("颜色要写成 #rgb、#rrggbb 或 #aarrggbb，收到 {value}"))
    }
}

fn seconds(value: f64) -> Result<(), String> {
    if value > 0.0 && value <= 600.0 {
        Ok(())
    } else {
        Err("停留秒数要大于 0 且不超过 600。".to_string())
    }
}

/// A hotkey the presenter can actually register: at least one modifier, then one key.
///
/// Without a modifier the hook swallows a bare key for every other program on the desktop,
/// which is the one failure mode a settings screen must not let a user type into.
fn hotkey(value: &str) -> Result<(), String> {
    let parts: Vec<&str> = value.split('+').map(str::trim).filter(|part| !part.is_empty()).collect();
    let modifiers = ["ctrl", "control", "alt", "shift", "win", "super", "meta"];
    let is_modifier =
        |part: &&str| modifiers.contains(&part.to_ascii_lowercase().as_str());
    if parts.len() < 2 || !parts[..parts.len() - 1].iter().all(is_modifier) || is_modifier(&parts[parts.len() - 1]) {
        return Err("快捷键要写成「修饰键+键」，例如 Ctrl+Alt+D；没有修饰键会吞掉其他程序的按键。".to_string());
    }
    Ok(())
}

/// A short BCP-47 tag: `ja`, `zh`, `zh-CN`.
fn is_language_tag(tag: &str) -> bool {
    let tag = tag.trim();
    if tag.is_empty() || tag.len() > 12 {
        return false;
    }
    tag.split('-').all(|part| {
        !part.is_empty() && part.len() <= 8 && part.bytes().all(|byte| byte.is_ascii_alphanumeric())
    })
}

/// One settings write at a time.
///
/// Both write paths below are read-modify-write of two whole files: the splice needs the bytes
/// around the value it replaces, and the record needs the entries around the one it appends. So
/// two of them in flight lose one of the two edits, and measured on the shipped build they do:
/// five `settings_write` calls issued together landed one of five colours in `config.json`, kept
/// two of five entries in the history — one of them for a key that never reached the file — and
/// failed the fifth outright, because `replace` names its temp file after its target and two
/// renames collided on it.
///
/// A person changes one control at a time, but a click and a keystroke a millisecond apart are
/// two writes in flight, so this is a real interleaving and not a theoretical one. Serialising
/// is the whole fix: these writes are sub-millisecond, and a settings screen means them in the
/// order they were made.
static WRITING: Mutex<()> = Mutex::new(());

/// Poisoning is ignored on purpose: the lock guards a sequence of file operations rather than
/// an invariant in memory, so a panic in one write leaves the next one nothing to distrust.
fn writing() -> MutexGuard<'static, ()> {
    WRITING.lock().unwrap_or_else(|err| err.into_inner())
}

/// Write one setting, and record the change so it can be taken back.
#[tauri::command]
pub async fn settings_write(app: AppHandle, edit: SettingEdit) -> Result<Settings, String> {
    let host = app.state::<Host>();
    edit.check()?;
    let (which, path) = edit.target();
    let file = match which {
        Which::Config => config_path(&host),
        Which::Runtime => runtime_path(&host),
    };

    // Scoped, and every line inside it synchronous: the guard is not Send, and the read this
    // command answers with is not part of what has to be serialised.
    {
        let _writing = writing();
        let raw = match std::fs::read_to_string(&file) {
            Ok(raw) => raw,
            // Nothing there yet. A packaged install's config.json is seeded with `voicePacks`
            // and nothing else, and runtime.json only exists after a deployment - so both
            // absent and section-less are normal, and the splice inserts what it needs.
            Err(_) => match which {
                Which::Config => seed_config(&[]),
                Which::Runtime => {
                    "// 运行时自己的文件。引擎位置由部署写入；这里只有空闲回收秒数。\n{\n}\n".to_string()
                }
            },
        };
        let rendered = edit.rendered();
        let (updated, before) = splice_at(&raw, path, &rendered)?;
        replace(&file, &updated)?;
        // After the write, not before it: an edit that never reached the file is not a change,
        // and the way back is already in hand rather than in a copy of the file.
        record(&host, which.name(), path, before, Leaf::Set(rendered));
    }
    host.log(&format!("settings_write: {} in {}", path.join("."), file.display()));
    Ok(crate::config_view::settings_read(app.clone()).await)
}

/// Write one field of one pack's own `voicepack.json`.
///
/// The pack's manifest, never the registry entry: the manifest is what wins at read time
/// (`docs/voicepack-spec.md`), so writing the form into `config.json` would produce an edit
/// that silently does nothing. Read, splice, write — so a field a newer build put in that
/// file is still there afterwards, comments included.
#[tauri::command]
pub async fn pack_config_write(app: AppHandle, id: String, edit: PackEdit) -> Result<PackConfig, String> {
    let host = app.state::<Host>();
    edit.check()?;
    let pack = read_packs(&host)
        .into_iter()
        .find(|pack| pack.id == id)
        .ok_or_else(|| format!("没有登记为 {id} 的音色包"))?;
    let payload = Path::new(&pack.path);
    let file = manifest_file(payload).ok_or("这个音色包没有可以写清单的位置")?;

    let raw = match std::fs::read_to_string(&file) {
        Ok(raw) => raw,
        // A pack that has never described itself. `schema` and `id` are the two keys the
        // spec calls for; everything else arrives through this same command.
        Err(_) => format!("{{\n  \"schema\": 1,\n  \"id\": {}\n}}\n", quoted(&pack.id)),
    };
    let (updated, _) = splice_at(&raw, edit.path(), &edit.rendered())?;
    replace(&file, &updated)?;
    host.log(&format!(
        "pack_config_write: {} {} in {}",
        id,
        edit.path().join("."),
        file.display()
    ));
    crate::config_view::pack_config(app.clone(), id.clone())
        .await
        .ok_or_else(|| format!("写完之后读不回 {id} 了"))
}

// --- the change record ------------------------------------------------------------------

/// One value of one leaf as the file carries it — or the absence of the member holding it.
///
/// Both ends of a recorded change are this, which is what makes an entry invertible: apply
/// `before` and the write is undone, apply `after` and it is redone, through the same splice
/// either way. Absence is a state and not a missing value: `null` is a value these files
/// use, and a form that confused the two would write `"reveal": null` where the user's file
/// had no `reveal` line at all.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Leaf {
    /// These bytes, at the change's own path.
    Set(String),
    /// No member. Reached by removing `member`, which is the highest key a creation had to
    /// create: writing `dialog.reveal` into a file with no `dialog` created `dialog`, and
    /// deleting only the leaf would leave behind an empty section the file never had.
    Absent { member: Vec<String> },
}

/// One change to one leaf of a settings file, and both of its ends.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Change {
    /// The handle `settings_restore` takes. Monotonic, and stable across the rotation that
    /// an index into the list would not survive.
    pub seq: u64,
    /// When it landed, epoch ms.
    ///
    /// Formatted by the panel rather than here: the webview knows the machine's timezone and
    /// its locale, and hand-rolling civil-time arithmetic in Rust to print a local stamp
    /// would be a date library this app deliberately does not ship.
    pub ts_ms: u64,
    /// `config.json` or `runtime.json`.
    pub file: String,
    /// `["dialog", "reveal"]`. Its last key is what this setting is called everywhere else
    /// in this app — `Settings::written` lists the same word — so the panel needs no second
    /// table to turn an entry into a label.
    pub path: Vec<String>,
    pub before: Leaf,
    pub after: Leaf,
}

/// How many changes are kept.
///
/// A recovery surface, not an audit log: what a person reaches for is the value they changed
/// in this sitting, and 50 covers a long one — a stepper records one change per press, and a
/// run of presses on the same leaf collapses into one entry (`MERGE_MS`). It also keeps the
/// file around 10 KiB, small enough that the panel reads the whole thing on every navigation
/// and rewrites it on every write.
const HISTORY_LIMIT: usize = 50;

/// How long a run of writes to the same leaf counts as one change.
///
/// Dragging a stepper from 6 to 12 is one decision and has to read as one line, the way it
/// raises one 已保存. The entry keeps the value the run started from, so its inverse still
/// lands where the user was before they touched it.
const MERGE_MS: u64 = 5_000;

/// Every recorded change, oldest first.
pub fn history(host: &Host) -> Vec<Change> {
    read_history(&history_path(host))
}

/// A line that will not parse is skipped rather than failing the read, for the reason
/// `read_packs` skips a malformed pack: one bad line must not make the panel claim there is
/// nothing to go back to.
fn read_history(file: &Path) -> Vec<Change> {
    let Ok(text) = std::fs::read_to_string(file) else {
        return Vec::new();
    };
    text.lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Change>(line).ok())
        .collect()
}

/// Record a change.
///
/// A failure here does not fail the write: losing the ability to take an edit back is worse
/// than nothing, but it is not worth refusing the edit the user asked for. It is logged.
fn record(host: &Host, file: &str, path: &[&str], before: Leaf, after: Leaf) {
    let at = history_path(host);
    if let Err(err) = write_change(&at, file, path, before, after) {
        host.log(&format!("record: {}: {err}; this edit is not recoverable", at.display()));
    }
}

/// Append a change, merging a run of writes to the same leaf and dropping the oldest once
/// there are more than `HISTORY_LIMIT`.
///
/// The whole file is rewritten rather than appended to, because both bounds are on the tail:
/// the newest entry absorbs a run and the oldest falls off the front. At `HISTORY_LIMIT`
/// lines that is one small read and one small write beside the config write that caused it.
fn write_change(at: &Path, file: &str, path: &[&str], before: Leaf, after: Leaf) -> std::io::Result<()> {
    let now = crate::host::now_ms();
    let path: Vec<String> = path.iter().map(|key| (*key).to_string()).collect();
    let mut kept = read_history(at);

    match kept.last_mut() {
        Some(last)
            if last.file == file && last.path == path && now.saturating_sub(last.ts_ms) <= MERGE_MS =>
        {
            last.ts_ms = now;
            last.after = after;
        }
        _ => {
            // Max rather than len: the file is rotated, so the count is not the counter.
            let seq = kept.iter().map(|change| change.seq).max().unwrap_or(0) + 1;
            kept.push(Change { seq, ts_ms: now, file: file.to_string(), path, before, after });
        }
    }

    // A merged run that ended where it started changed nothing, and a row saying `6 → 6` is
    // a record of a decision nobody made.
    if kept.last().is_some_and(|last| last.before == last.after) {
        kept.pop();
    }
    if kept.len() > HISTORY_LIMIT {
        kept.drain(..kept.len() - HISTORY_LIMIT);
    }

    let mut text = String::with_capacity(kept.len() * 192);
    for change in &kept {
        // Infallible for this type - every field is a string, a number or a Vec of them - so
        // a failure here is a bug and not a state to render.
        let Ok(line) = serde_json::to_string(change) else {
            continue;
        };
        text.push_str(&line);
        text.push('\n');
    }
    std::fs::write(at, text.as_bytes())
}

/// Put one leaf back the way it was, through the same splice every other write goes through.
fn apply(raw: &str, path: &[&str], leaf: &Leaf) -> Result<String, String> {
    match leaf {
        Leaf::Set(value) => splice_at(raw, path, value).map(|(text, _)| text),
        Leaf::Absent { member } => {
            let member: Vec<&str> = member.iter().map(String::as_str).collect();
            splice_out(raw, &member)
        }
    }
}

/// Take one recorded change back: the inverse edit, spliced into the file as it is now.
///
/// Byte-for-byte, because the inverse of replacing a value is writing the previous bytes into
/// the same span, and the inverse of creating a member is removing it. Everything around it —
/// comments, key order, indentation, the BOM — is never in the span either way.
///
/// Recorded in its turn: a mis-click on 还原 must not be the one edit in this app that cannot
/// be undone.
#[tauri::command]
pub async fn settings_restore(app: AppHandle, seq: u64) -> Result<Settings, String> {
    let host = app.state::<Host>();
    // Held across the lookup as well as the write: the entry has to still be the newest state
    // of that leaf when its inverse lands, and `WRITING` is what makes "still" mean anything.
    {
        let _writing = writing();
        let change = history(&host)
            .into_iter()
            .find(|change| change.seq == seq)
            .ok_or_else(|| format!("这条改动记录已经不在历史里了：{seq}"))?;

        let file = match change.file.as_str() {
            "config.json" => config_path(&host),
            "runtime.json" => runtime_path(&host),
            other => return Err(format!("历史里记的不是本程序的设置文件：{other}")),
        };
        let raw = std::fs::read_to_string(&file)
            .map_err(|err| format!("读不出 {}：{err}", change.file))?;

        let path: Vec<&str> = change.path.iter().map(String::as_str).collect();
        let updated = apply(&raw, &path, &change.before)?;
        // Refuse to write a file the runtime would then refuse to start on. The splice cannot
        // produce one from a file that parsed, so this catches the case where the file on disk
        // was already broken by hand.
        serde_json::from_str::<Value>(&normalize(&updated))
            .map_err(|err| format!("还原后的 {} 不是有效的 JSON，没有写入：{err}", change.file))?;

        replace(&file, &updated)?;
        record(&host, &change.file, &path, change.after, change.before);
        host.log(&format!(
            "settings_restore: {} {} in {}",
            seq,
            change.path.join("."),
            file.display()
        ));
    }
    Ok(crate::config_view::settings_read(app.clone()).await)
}

/// Whether this app could write the pack's manifest.
///
/// Probed by opening the file for append rather than by reading an attribute: a network
/// share and read-only media both answer honestly to that and to nothing else. A pack with
/// no manifest yet is judged by its directory.
pub fn pack_writable(payload: &Path) -> bool {
    let Some(file) = manifest_file(payload) else {
        return false;
    };
    if file.exists() {
        return std::fs::OpenOptions::new().append(true).open(&file).is_ok();
    }
    match file.parent() {
        // A probe file rather than a metadata check: `readonly()` on a directory means
        // nothing on Windows, and this is the same question the write itself will ask.
        Some(dir) => {
            let probe = dir.join(".voice-core-write-probe");
            let ok = std::fs::write(&probe, b"").is_ok();
            let _ = std::fs::remove_file(&probe);
            ok
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(id: &str) -> Pack {
        Pack {
            id: id.to_string(),
            name: id.to_string(),
            kind: "lora-adapter".to_string(),
            path: "voicepacks/x".to_string(),
            engine: "irodori".to_string(),
            languages: vec!["ja".to_string()],
            character: None,
            avatar: None,
        }
    }

    /// The registry write, which is the one span in these files with structure in it.
    fn splice(raw: &str, packs: &[Pack]) -> Result<String, String> {
        splice_at(raw, &["voicePacks"], &render(packs)).map(|(text, _)| text)
    }

    /// A leaf write, for the cases that only assert on the text it produced.
    fn at(raw: &str, path: &[&str], rendered: &str) -> String {
        splice_at(raw, path, rendered).unwrap().0
    }

    /// The reason this module exists: a comment the user reads must survive a
    /// write, and so must a section this app has never heard of.
    #[test]
    fn keeps_comments_and_foreign_sections() {
        let raw = "// 顶部说明\n{\n  \"dialog\": {\n    // 旁注在上还是在下\n    \"annotationAbove\": false,\n  },\n\n  // 声线包registry\n  \"voicePacks\": []\n}\n";
        let out = splice(raw, &[pack("a")]).unwrap();
        assert!(out.starts_with("// 顶部说明\n"));
        assert!(out.contains("// 旁注在上还是在下"));
        assert!(out.contains("// 声线包registry"));
        assert!(out.contains("\"annotationAbove\": false,"));
        assert!(out.contains("\"id\": \"a\""));
    }

    #[test]
    fn keeps_a_bom() {
        let raw = "\u{feff}{\n  \"voicePacks\": []\n}\n";
        let out = splice(raw, &[]).unwrap();
        assert!(out.starts_with('\u{feff}'));
    }

    /// Chinese comments are why `normalize` blanks per byte: one space per
    /// character would shift every offset after them and the splice would cut the
    /// file in the wrong place.
    #[test]
    fn offsets_survive_multibyte_comments() {
        let raw = "{\n  // 这是一段很长的中文注释，用来把偏移量推后\n  \"voicePacks\": [1],\n  \"keep\": 1\n}";
        let normalized = normalize(raw);
        assert_eq!(normalized.len(), raw.len());
        let out = splice(raw, &[]).unwrap();
        assert!(out.contains("这是一段很长的中文注释"));
        assert!(out.contains("\"keep\": 1"));
        assert!(out.contains("\"voicePacks\": []"));
    }

    #[test]
    fn inserts_the_key_when_absent() {
        let raw = "{\n  \"hotkeys\": {\n    \"toggleDialog\": \"Ctrl+Alt+D\"\n  }\n}\n";
        let out = splice(raw, &[pack("a")]).unwrap();
        assert!(out.contains("\"toggleDialog\": \"Ctrl+Alt+D\""));
        assert!(out.contains("\"voicePacks\": ["));
        serde_json::from_str::<Value>(&normalize(&out)).unwrap();
    }

    #[test]
    fn tolerates_a_trailing_comma_before_the_close() {
        let raw = "{\n  \"a\": 1,\n}\n";
        let out = splice(raw, &[]).unwrap();
        serde_json::from_str::<Value>(&normalize(&out)).unwrap();
    }

    #[test]
    fn refuses_a_file_it_cannot_understand() {
        assert!(splice("not json at all", &[]).is_err());
    }

    /// The whole point of writing a leaf instead of a section: the shipped `config.json`
    /// explains the three reveal styles in six lines of Chinese INSIDE the `dialog` object,
    /// and a write that replaced the section would take them with it.
    #[test]
    fn a_leaf_write_keeps_the_prose_around_it() {
        let raw = "{\n  \"dialog\": {\n    // 旁注在上还是在下\n    \"annotationAbove\": false,\n\n    // 文字出现方式：\n    //   \"typewriter\" 逐字打字机\n    //   \"sweep\"      一道柔光扫过\n    \"reveal\": \"typewriter\"\n  }\n}\n";
        let out = at(raw, &["dialog", "reveal"], "\"sweep\"");

        assert!(out.contains("// 旁注在上还是在下"));
        assert!(out.contains("//   \"typewriter\" 逐字打字机"));
        assert!(out.contains("//   \"sweep\"      一道柔光扫过"));
        assert!(out.contains("\"annotationAbove\": false,"));
        assert!(out.contains("\"reveal\": \"sweep\""));
        assert!(!out.contains("\"reveal\": \"typewriter\""));
        // Only the four bytes of the value moved.
        assert_eq!(out.len(), raw.len() - "typewriter".len() + "sweep".len());
    }

    /// What a packaged install actually looks like: `package.ps1` seeds `config.json` with
    /// `voicePacks` and no `dialog` section at all, so the first colour a user picks has to
    /// create the section around it.
    #[test]
    fn creates_a_missing_section_on_the_way_to_its_leaf() {
        let raw = "{\n  \"voicePacks\": []\n}\n";
        let out = at(raw, &["dialog", "nameColor"], "\"#a48bff\"");

        assert!(out.contains("\"voicePacks\": []"));
        assert!(out.contains("\"dialog\": {"));
        assert!(out.contains("\"nameColor\": \"#a48bff\""));
        let parsed: Value = serde_json::from_str(&normalize(&out)).unwrap();
        assert_eq!(parsed["dialog"]["nameColor"], "#a48bff");
    }

    #[test]
    fn adds_a_key_to_a_section_that_already_exists() {
        let raw = "{\n  // 字幕外观\n  \"dialog\": {\n    \"reveal\": \"fade\"\n  }\n}\n";
        let out = at(raw, &["dialog", "displaySeconds"], "6");

        assert!(out.contains("// 字幕外观"));
        let parsed: Value = serde_json::from_str(&normalize(&out)).unwrap();
        assert_eq!(parsed["dialog"]["reveal"], "fade");
        assert_eq!(parsed["dialog"]["displaySeconds"], 6);
    }

    /// A manifest a newer build wrote. The form must not be able to lose a field it has
    /// never heard of, and this is the property that guarantees it: the write never sees the
    /// rest of the file.
    #[test]
    fn a_manifest_write_keeps_keys_this_build_never_heard_of() {
        let raw = "{\n  \"schema\": 2,\n  \"id\": \"ba-miyu-lora\",\n  // 未来版本加的\n  \"phonemeOverrides\": { \"美游\": \"みゆ\" },\n  \"dialog\": {\n    \"nameColor\": \"#a48bff\"\n  }\n}\n";
        let out = at(raw, &["dialog", "nameColor"], "\"#ff0000\"");

        assert!(out.contains("// 未来版本加的"));
        let parsed: Value = serde_json::from_str(&normalize(&out)).unwrap();
        assert_eq!(parsed["phonemeOverrides"]["美游"], "みゆ");
        assert_eq!(parsed["schema"], 2);
        assert_eq!(parsed["dialog"]["nameColor"], "#ff0000");
    }

    /// A key present but not an object where the path wants one: refuse, because the only
    /// alternative is overwriting whatever the user actually put there.
    #[test]
    fn refuses_to_descend_into_something_that_is_not_an_object() {
        let raw = "{\n  \"dialog\": \"typewriter\"\n}\n";
        assert!(splice_at(raw, &["dialog", "reveal"], "\"fade\"").is_err());
    }

    #[test]
    fn validates_before_it_writes_anything() {
        assert!(SettingEdit::Reveal("per-char".to_string()).check().is_err());
        assert!(SettingEdit::Reveal("sweep".to_string()).check().is_ok());
        assert!(SettingEdit::NameColor("#nothex".to_string()).check().is_err());
        assert!(SettingEdit::NameColor("#a48bff".to_string()).check().is_ok());
        // The presenter's own notation: alpha first, eight digits.
        assert!(SettingEdit::RubyColor("#9effffff".to_string()).check().is_ok());
        assert!(SettingEdit::TextColor("#fff".to_string()).check().is_ok());
        assert!(SettingEdit::DisplaySeconds(0.0).check().is_err());
        assert!(SettingEdit::DisplaySeconds(601.0).check().is_err());
        assert!(SettingEdit::DisplaySeconds(6.0).check().is_ok());
        // A hotkey with no modifier swallows that key for every other program.
        assert!(SettingEdit::ToggleDialog("D".to_string()).check().is_err());
        assert!(SettingEdit::ToggleDialog("Ctrl+Alt".to_string()).check().is_err());
        assert!(SettingEdit::ToggleDialog("Ctrl+Alt+D".to_string()).check().is_ok());
        assert!(SettingEdit::IdleStopSecs(0).check().is_ok());
        assert!(SettingEdit::IdleStopSecs(90_000).check().is_err());

        assert!(PackEdit::CfgScaleCaption(Some(10.5)).check().is_err());
        assert!(PackEdit::CfgScaleCaption(Some(3.0)).check().is_ok());
        assert!(PackEdit::CfgScaleCaption(None).check().is_ok());
        assert!(PackEdit::Kind("embedding".to_string()).check().is_err());
        assert!(PackEdit::Languages(vec![]).check().is_err());
        assert!(PackEdit::Languages(vec!["日本語".to_string()]).check().is_err());
        assert!(PackEdit::Languages(vec!["ja".to_string(), "zh-CN".to_string()]).check().is_ok());
        assert!(PackEdit::Name("  ".to_string()).check().is_err());
        assert!(PackEdit::NumSteps(Some(0)).check().is_err());
    }

    /// These files are read by people, so `6` and not `6.0`, and an unset field says so.
    #[test]
    fn renders_values_the_way_a_person_would_type_them() {
        assert_eq!(PackEdit::DisplaySeconds(Some(6.0)).rendered(), "6");
        assert_eq!(PackEdit::DisplaySeconds(Some(4.5)).rendered(), "4.5");
        assert_eq!(PackEdit::DisplaySeconds(None).rendered(), "null");
        assert_eq!(PackEdit::Seed(None).rendered(), "null");
        assert_eq!(PackEdit::Languages(vec!["ja".into(), "zh".into()]).rendered(), "[\"ja\", \"zh\"]");
        // A character name with a quote in it must not be able to break the file.
        assert_eq!(PackEdit::Character(Some("a\"b".into())).rendered(), "\"a\\\"b\"");
        assert_eq!(SettingEdit::AnnotationAbove(true).rendered(), "true");
    }

    /// The wire form the frontend sends: the field name IS the discriminant, so "leave it
    /// alone" and "set it to null" cannot be confused.
    #[test]
    fn an_edit_arrives_as_one_named_field() {
        let edit: PackEdit =
            serde_json::from_str(r##"{"field":"nameColor","value":"#a48bff"}"##).unwrap();
        assert_eq!(edit.path(), ["dialog", "nameColor"]);
        assert_eq!(edit.rendered(), "\"#a48bff\"");

        let cleared: PackEdit = serde_json::from_str(r#"{"field":"seed","value":null}"#).unwrap();
        assert_eq!(cleared.rendered(), "null");

        let setting: SettingEdit =
            serde_json::from_str(r#"{"field":"idleStopSecs","value":900}"#).unwrap();
        assert_eq!(setting.rendered(), "900");
    }

    // --- the change record ---------------------------------------------------------------

    /// A private directory under the OS temp dir, named after the case and cleared on the way
    /// in: a failing assert has to leave its file behind to be looked at.
    fn scratch(case: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("voice-core-history-{case}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Write, then take it back: the file has to be the bytes it was, not an equivalent
    /// rendering of them.
    fn round_trip(raw: &str, path: &[&str], rendered: &str) -> String {
        let (written, before) = splice_at(raw, path, rendered).unwrap();
        assert_ne!(written, raw, "the write did nothing, so the restore proves nothing");
        let restored = apply(&written, path, &before).unwrap();
        assert_eq!(restored, raw);
        written
    }

    /// The guarantee the 版本 panel makes. Replacing a value is inverted by writing the
    /// previous bytes back into the same span, so every comment, the key order, the trailing
    /// comma and the BOM come out where they were - and CRLF stays CRLF, which a restore that
    /// re-rendered the file would silently normalise.
    #[test]
    fn a_restore_puts_a_replaced_value_back_byte_for_byte() {
        let raw = "\u{feff}// 顶部说明\r\n{\r\n  \"dialog\": {\r\n    // 文字出现方式：\r\n    //   \"typewriter\" 逐字打字机\r\n    \"reveal\": \"typewriter\",\r\n  },\r\n}\r\n";
        let written = round_trip(raw, &["dialog", "reveal"], "\"sweep\"");
        assert!(written.contains("\"reveal\": \"sweep\""));
    }

    /// The other half of the guarantee: a write that had to *create* the key is inverted by
    /// removing it, because putting `null` there instead would leave a line the user's file
    /// never had - and the panel would then show 默认值 beside a key that is written.
    #[test]
    fn a_restore_removes_a_key_the_write_created() {
        let raw = "{\n  // 字幕外观\n  \"dialog\": {\n    \"reveal\": \"fade\"\n  }\n}\n";
        let written = round_trip(raw, &["dialog", "displaySeconds"], "6");
        assert!(written.contains("\"displaySeconds\": 6"));
        assert!(written.contains("\"reveal\": \"fade\","));
    }

    /// A packaged install has no `dialog` section at all, so the first colour picked creates
    /// one. The inverse removes the section, not just the leaf inside it.
    #[test]
    fn a_restore_removes_a_section_the_write_created() {
        let raw = "// voice-core 设置。注释和尾随逗号都会被保留。\n{\n  \"voicePacks\": []\n}\n";
        let written = round_trip(raw, &["dialog", "nameColor"], "\"#a48bff\"");
        assert!(written.contains("\"dialog\": {"));

        // And the inverse is reached by the recorded member, which is the section rather than
        // the leaf: removing `dialog.nameColor` alone would leave `"dialog": {}` behind.
        let (_, before) = splice_at(raw, &["dialog", "nameColor"], "\"#a48bff\"").unwrap();
        assert_eq!(before, Leaf::Absent { member: vec!["dialog".to_string()] });
    }

    /// The whole feature, on the file a real install actually has: several settings changed in
    /// a row, then every change taken back. The bytes have to be the bytes, comments included,
    /// and each inverse has to be independent of the ones around it - two of these three edits
    /// create a key and one replaces a value, so the restores are not the same operation.
    #[test]
    fn a_session_of_edits_and_their_restores_come_back_to_the_same_bytes() {
        // `E:\NewToolBox\voice-core\data\config.json` as installed: prose above every key, a
        // `dialog` section with three of its ten keys written, and no colours at all.
        let raw = "// voice-core 设置。改完立刻生效，不用重启：dialog / hotkeys 由字幕进程按 mtime 重读，\n// voicePacks 与 dialog 由 runtime 自己重读。\n{\n  \"dialog\": {\n    // 旁注在正读的那行上方还是下方\n    \"annotationAbove\": false,\n\n    // 文字出现方式：\n    //   \"typewriter\" 逐字打字机\n    //   \"sweep\"      一道柔光扫过\n    \"reveal\": \"fade\",\n    \"displaySeconds\": 10.5\n  },\n\n  // 全局快捷键。至少要带一个修饰键。\n  \"hotkeys\": {\n    \"toggleDialog\": \"Ctrl+Alt+D\"\n  },\n\n  \"voicePacks\": [\n    {\n      \"id\": \"ba-miyu-lora\",\n      \"path\": \"voicepacks/ba-miyu-lora\"\n    }\n  ]\n}\n";

        let edits: [(&[&str], &str); 3] = [
            (&["dialog", "reveal"], "\"sweep\""),
            (&["dialog", "nameColor"], "\"#a48bff\""),
            (&["dialog", "displaySeconds"], "12"),
        ];

        // Forwards, the way three saves land.
        let mut text = raw.to_string();
        let mut recorded = Vec::new();
        for (path, rendered) in edits {
            let (next, before) = splice_at(&text, path, rendered).unwrap();
            recorded.push((path, before));
            text = next;
        }
        assert!(text.contains("\"reveal\": \"sweep\""));
        assert!(text.contains("\"nameColor\": \"#a48bff\""));
        assert!(text.contains("\"displaySeconds\": 12"));
        assert!(text.contains("// 文字出现方式："));

        // Backwards, oldest first, which is the row the user reaches for.
        for (path, before) in &recorded {
            text = apply(&text, path, before).unwrap();
        }
        assert_eq!(text, raw);
        // Said plainly, because it is the promise: the comments are still there and the file is
        // byte-for-byte what it was.
        assert!(text.contains("// voicePacks 与 dialog 由 runtime 自己重读。"));
        assert!(!text.contains("nameColor"));
    }

    /// The first key of an object, so the comma this member brought is the one after it.
    #[test]
    fn a_restore_removes_the_separator_it_brought_either_side() {
        let raw = "{\n}\n";
        round_trip(raw, &["idleStopSecs"], "900");

        let one = "{\n  \"idleStopSecs\": 900\n}\n";
        let (two, _) = splice_at(one, &["engineDir"], "\"C:\\\\x\"").unwrap();
        // Remove the one that was there first: its separator is the comma the second brought.
        let out = splice_out(&two, &["idleStopSecs"]).unwrap();
        serde_json::from_str::<Value>(&normalize(&out)).unwrap();
        assert!(!out.contains("idleStopSecs"));
        assert!(out.contains("engineDir"));
    }

    /// A comment written above the line since the panel created it. Byte-exactness is already
    /// forfeit - the user edited the file - so the rule that survives is the module's first
    /// one: a comment a person reads is never collateral.
    #[test]
    fn a_restore_does_not_swallow_a_comment_added_since() {
        let raw = "{\n  \"dialog\": {\n    \"reveal\": \"fade\"\n  }\n}\n";
        let (written, before) = splice_at(raw, &["dialog", "displaySeconds"], "6").unwrap();
        let annotated = written.replace(
            "    \"displaySeconds\": 6",
            "    // 这行是面板写的\n    \"displaySeconds\": 6",
        );

        let out = apply(&annotated, &["dialog", "displaySeconds"], &before).unwrap();
        assert!(out.contains("// 这行是面板写的"));
        assert!(!out.contains("displaySeconds"));
        serde_json::from_str::<Value>(&normalize(&out)).unwrap();
    }

    /// Removing something that is not there is not an error: the end state asked for is the
    /// end state, which is what a second click on 还原 asks for.
    #[test]
    fn removing_an_absent_member_leaves_the_file_alone() {
        let raw = "{\n  \"dialog\": {}\n}\n";
        assert_eq!(splice_out(raw, &["dialog", "reveal"]).unwrap(), raw);
    }

    /// One press of a stepper is not a decision. A run of writes to the same leaf inside
    /// `MERGE_MS` is one entry, and it keeps the value the run started from so its inverse
    /// still lands where the user was.
    #[test]
    fn a_run_of_writes_to_one_leaf_is_one_entry() {
        let at = scratch("merge").join("settings.history.jsonl");
        for step in 7..=12 {
            write_change(
                &at,
                "config.json",
                &["dialog", "displaySeconds"],
                Leaf::Set((step - 1).to_string()),
                Leaf::Set(step.to_string()),
            )
            .unwrap();
        }

        let kept = read_history(&at);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].before, Leaf::Set("6".to_string()));
        assert_eq!(kept[0].after, Leaf::Set("12".to_string()));
        assert_eq!(kept[0].seq, 1);
    }

    /// And a run that ended where it started is not an entry at all.
    #[test]
    fn a_run_that_came_back_to_its_own_value_records_nothing() {
        let at = scratch("noop").join("settings.history.jsonl");
        let path = &["dialog", "annotationAbove"];
        write_change(&at, "config.json", path, Leaf::Set("false".into()), Leaf::Set("true".into())).unwrap();
        write_change(&at, "config.json", path, Leaf::Set("true".into()), Leaf::Set("false".into())).unwrap();
        assert!(read_history(&at).is_empty());
    }

    /// A different leaf is a different decision, even one keystroke later.
    #[test]
    fn a_different_leaf_is_a_new_entry() {
        let at = scratch("distinct").join("settings.history.jsonl");
        write_change(&at, "config.json", &["dialog", "reveal"], Leaf::Set("\"fade\"".into()), Leaf::Set("\"sweep\"".into())).unwrap();
        write_change(&at, "runtime.json", &["idleStopSecs"], Leaf::Set("900".into()), Leaf::Set("0".into())).unwrap();

        let kept = read_history(&at);
        assert_eq!(kept.len(), 2);
        assert_eq!(kept[1].seq, 2);
        assert_eq!(kept[1].file, "runtime.json");
    }

    /// The cap, and what it means: the oldest falls off the front and the handles of the ones
    /// that stay do not move, because `settings_restore` takes a seq and not an index.
    #[test]
    fn the_history_is_bounded_and_drops_the_oldest() {
        let at = scratch("cap").join("settings.history.jsonl");
        for n in 1..=HISTORY_LIMIT + 10 {
            // A distinct leaf per write, so nothing merges and every one is its own entry.
            write_change(
                &at,
                "config.json",
                &["dialog", &format!("k{n}")],
                Leaf::Set("0".to_string()),
                Leaf::Set(n.to_string()),
            )
            .unwrap();
        }

        let kept = read_history(&at);
        assert_eq!(kept.len(), HISTORY_LIMIT);
        assert_eq!(kept[0].seq, 11);
        assert_eq!(kept[HISTORY_LIMIT - 1].seq, (HISTORY_LIMIT + 10) as u64);
        let lines = std::fs::read_to_string(&at).unwrap();
        assert_eq!(lines.lines().count(), HISTORY_LIMIT);
    }

    /// An entry a stale build wrote, or a line somebody truncated: skipped, never fatal.
    #[test]
    fn a_line_that_will_not_parse_is_skipped() {
        let at = scratch("garbage").join("settings.history.jsonl");
        std::fs::write(
            &at,
            "{\"seq\":1,\"tsMs\":1,\"file\":\"config.json\",\"path\":[\"dialog\",\"reveal\"],\"before\":{\"set\":\"\\\"fade\\\"\"},\"after\":{\"set\":\"\\\"sweep\\\"\"}}\n{ half a line\n",
        )
        .unwrap();

        let kept = read_history(&at);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].path, ["dialog", "reveal"]);
        assert_eq!(kept[0].after, Leaf::Set("\"sweep\"".to_string()));
    }

    /// The line an agent or a person reads: one object per change, with both ends of it in
    /// the file's own notation.
    #[test]
    fn an_entry_is_one_readable_line() {
        let change = Change {
            seq: 7,
            ts_ms: 1_757_060_000_000,
            file: "config.json".to_string(),
            path: vec!["dialog".to_string(), "reveal".to_string()],
            before: Leaf::Absent { member: vec!["dialog".to_string()] },
            after: Leaf::Set("\"sweep\"".to_string()),
        };
        let line = serde_json::to_string(&change).unwrap();
        assert_eq!(
            line,
            r#"{"seq":7,"tsMs":1757060000000,"file":"config.json","path":["dialog","reveal"],"before":{"absent":{"member":["dialog"]}},"after":{"set":"\"sweep\""}}"#
        );
        assert!(!line.contains('\n'));
        let back: Change = serde_json::from_str(&line).unwrap();
        assert_eq!(back.before, change.before);
        assert_eq!(back.after, change.after);
    }
}
