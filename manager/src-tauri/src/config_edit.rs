//! `data/config.json` without destroying it.
//!
//! That file is JSONC: comments explaining every key, a trailing comma left
//! behind after deleting a line, possibly a BOM from Notepad or PowerShell 5.1's
//! `Set-Content -Encoding UTF8`. A human reads those comments — the tray's
//! settings entry opens the file in an editor — so the file is never
//! reserialised. Instead the byte span of the `voicePacks` value is located and
//! only that span is replaced: every comment, the key order, the indentation, the
//! BOM and every section this app knows nothing about come out byte-identical.
//! Comments *inside* the array are the one casualty, because they live in the
//! span; the shipped file keeps its explanation above the key, where it survives.
//!
//! The JSONC rules are re-implemented here rather than borrowed from the runtime
//! crate. This app is an HTTP client of that daemon, not a linked part of it, and
//! linking axum, clap and a tokio server to reuse thirty lines of text handling
//! would invert the seam ADR-0001 draws. The three rules are the same ones the
//! runtime applies when it reads this same file: a BOM is expected input,
//! comments are whitespace, one dangling comma is forgivable.

use std::ops::Range;
use std::path::{Path, PathBuf};

use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::contract::Pack;
use crate::host::Host;

pub fn config_path(host: &Host) -> PathBuf {
    host.data_dir.join("config.json")
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

/// The manifest of the pack registered under `id`, for the panel's pack detail view.
#[tauri::command]
pub async fn pack_manifest(app: AppHandle, id: String) -> Option<Value> {
    let host = app.state::<Host>();
    let pack = read_packs(&host).into_iter().find(|pack| pack.id == id)?;
    manifest_beside(Path::new(&pack.path))
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

fn write_packs(host: &Host, packs: &[Pack]) -> Result<(), String> {
    let path = config_path(host);
    let updated = match std::fs::read_to_string(&path) {
        Ok(raw) => splice(&raw, packs)?,
        // No config file yet. A minimal one is enough: the presenter defaults
        // every key it does not find, and inventing a copy of its documented
        // template here would give that template two owners. No BOM, matching
        // what the rest of the project writes.
        Err(_) => format!(
            "// voice-core 设置。注释和尾随逗号都会被保留。\n{{\n  \"voicePacks\": {}\n}}\n",
            render(packs)
        ),
    };

    // Written beside the target and renamed, because the runtime reloads this
    // file on mtime change: it must never observe a half-written array.
    let temp = path.with_extension("json.tmp");
    std::fs::write(&temp, updated.as_bytes())
        .map_err(|err| format!("writing {}: {err}", temp.display()))?;
    std::fs::rename(&temp, &path).map_err(|err| format!("replacing {}: {err}", path.display()))?;
    Ok(())
}

/// Replace the `voicePacks` value in place, or insert the key when it is absent.
fn splice(raw: &str, packs: &[Pack]) -> Result<String, String> {
    let normalized = normalize(raw);
    let target = locate(&normalized).ok_or_else(|| {
        "could not find the voicePacks member in config.json: the file is not a JSON object this \
         app is willing to rewrite blindly. Fix or remove it and try again."
            .to_string()
    })?;
    let mut out = String::with_capacity(raw.len() + 256);
    match target {
        Target::Value(range) => {
            out.push_str(&raw[..range.start]);
            out.push_str(&render(packs));
            out.push_str(&raw[range.end..]);
        }
        Target::Insert { at, needs_comma } => {
            out.push_str(&raw[..at]);
            if needs_comma {
                out.push(',');
            }
            out.push_str("\n\n  \"voicePacks\": ");
            out.push_str(&render(packs));
            out.push_str(&raw[at..]);
        }
    }
    Ok(out)
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
    /// Byte offset to insert `"voicePacks": …` at, and whether a separating comma
    /// is needed there.
    Insert { at: usize, needs_comma: bool },
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
fn normalize(raw: &str) -> String {
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

/// Walk the top-level object's members. Comments are already whitespace by the
/// time this runs, so this only has to understand JSON.
fn locate(normalized: &str) -> Option<Target> {
    let mut scan = Scan {
        bytes: normalized.as_bytes(),
        at: 0,
    };
    scan.whitespace();
    if scan.take()? != b'{' {
        return None;
    }
    let mut last_member_end = scan.at;
    let mut members = 0usize;

    loop {
        scan.whitespace();
        match scan.peek()? {
            b'}' => {
                return Some(Target::Insert {
                    at: last_member_end,
                    needs_comma: members > 0,
                })
            }
            b',' => scan.at += 1,
            b'"' => {
                let key = scan.string()?;
                scan.whitespace();
                if scan.take()? != b':' {
                    return None;
                }
                scan.whitespace();
                let start = scan.at;
                scan.value()?;
                let end = scan.at;
                if key == "voicePacks" {
                    return Some(Target::Value(start..end));
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
}
