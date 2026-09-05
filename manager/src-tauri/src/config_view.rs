//! What the two configuration screens read.
//!
//! Two files decide how this program behaves — `data/config.json` for the program and a
//! pack's own `voicepack.json` — and both are JSONC written for a human: comments
//! explaining every key, and a `//` note above the line somebody changed. So the raw
//! reads here hand over bytes, a path and a size, and the "查看原始文件" affordance shows
//! the file as it is.
//!
//! Beside them are the typed reads the forms are built on. Typed, not blobs: a form needs
//! to know that `displaySeconds` is a number and `reveal` is one of three words, and a
//! screen that discovers that by poking at a `Value` is a screen that discovers it
//! wrongly. Absent keys come back as the value actually in force with the key listed as
//! unwritten, because a control showing a blank where the program has a built-in is a
//! control that lies.
//!
//! Which file *won* a field is not computed here. `src/packs.rs::hydrate` in the runtime
//! is the one implementation of that precedence, and it reports its own verdict per
//! field, so `pack_effective` forwards that answer instead of a second opinion about the
//! same two files.
//!
//! Nothing here writes: `config_edit` owns every byte that reaches those files.
//! `speak_preview` is the one command that leaves the machine's disk alone and asks the
//! runtime for something — it exists so a style change can be *heard* from the page that
//! made it, and it reaches the same `POST /api/speak` an agent or the CLI would.

use std::path::Path;
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::config_edit;
use crate::contract::Pack;
use crate::host::Host;

/// One file as the screen shows it. Every field name is a single word, so this struct's
/// snake_case and the frontend's camelCase are the same bytes — the note `contract.rs`
/// makes about `Pack`.
///
/// `exists: false` is an answer rather than a failure: `runtime.json` is not there until
/// a deployment writes it, and a pack's manifest is optional by design.
#[derive(Serialize)]
pub struct ConfigFile {
    /// What the screen calls this file, including which file it turned out to be — a
    /// single-file pack's manifest is a sidecar named after the payload.
    pub label: String,
    pub path: String,
    /// The file as written, comments and all. Empty when there is nothing to read.
    pub text: String,
    pub exists: bool,
    /// Size on disk, so the screen can show a file it could not read as the size it is
    /// rather than as an empty one.
    pub bytes: u64,
}

/// A file the screen can name, locate and open, present or not.
///
/// Nothing is propagated as an error, for the reason `list_voices` answers with the
/// runtime stopped: the states this screen exists to explain include "that file does not
/// exist yet". A file that is there but unreadable this instant — Notepad rewrites in
/// place, so a read can land on a truncated prefix — comes back with its size and no
/// text, which the screen reports as its own state instead of as an empty file.
fn read_file(label: String, path: &Path) -> ConfigFile {
    let bytes = std::fs::metadata(path).map(|meta| meta.len());
    ConfigFile {
        label,
        path: config_edit::native(path),
        text: std::fs::read_to_string(path).unwrap_or_default(),
        exists: bytes.is_ok(),
        bytes: bytes.unwrap_or(0),
    }
}

/// One JSONC file as a `Value`, or `None` when it is absent or will not parse.
///
/// The same three tolerances the runtime applies to these files — a BOM, comments, one
/// dangling comma — because it is the same `config_edit::normalize` that implements them.
/// A form that could not read a hand-edited file the runtime reads fine would be a second,
/// stricter parser for the one format.
fn read_jsonc(path: &Path) -> Option<Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&config_edit::normalize(&raw)).ok()
}

/// The manifest of the pack registered under `id`, as the file on disk.
///
/// Resolved through `read_packs`, so an id means here what it means everywhere else and
/// the payload path arrives absolute, and through `manifest_file`, so the sidecar naming
/// rule is not restated. Deliberately not through `manifest_beside`: that one parses, and
/// a `Value` re-serialised for display would reach the screen without the comments and
/// with its keys reordered — which is no longer the file the user is looking at.
///
/// `None` means no pack is registered under that id. A pack that simply wrote no manifest
/// answers with `exists: false`, because the difference the screen has to draw is between
/// "there is no such pack" and "this pack describes itself nowhere".
#[tauri::command]
pub async fn pack_manifest_file(app: AppHandle, id: String) -> Option<ConfigFile> {
    let host = app.state::<Host>();
    let pack = config_edit::read_packs(&host).into_iter().find(|pack| pack.id == id)?;
    let file = config_edit::manifest_file(Path::new(&pack.path))?;
    let name = file.file_name()?.to_string_lossy().to_string();
    Some(read_file(format!("{name} · 音色包自己的描述"), &file))
}

/// The runtime's own merged view of one pack, its `sources` map included, or `None` when
/// the runtime is not answering.
///
/// Forwarded as raw JSON for the same reason `Status.body` is: `GET /api/voices` owns
/// that contract, and a typed mirror here would be a second definition of it that
/// silently drops whatever this build has not heard of — a provenance key added later
/// included.
///
/// `None` while the runtime is down is the honest answer and the screen renders it as
/// one. The merge lives in the runtime; with the runtime stopped, nothing on this machine
/// can say which file won a field, and the only alternative to saying so is guessing.
#[tauri::command]
pub async fn pack_effective(app: AppHandle, id: String) -> Option<Value> {
    let host = app.state::<Host>();
    let token = host.token()?;
    let url = format!("{}/api/voices", host.base_url);
    let response = host.http.get(url).bearer_auth(token).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    response
        .json::<Vec<Value>>()
        .await
        .ok()?
        .into_iter()
        .find(|pack| pack.get("id").and_then(Value::as_str) == Some(id.as_str()))
}

// --- the typed reads the two forms are built on --------------------------------------

/// Every setting this app offers a control for, resolved to the value in force.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Settings {
    pub annotation_above: bool,
    pub reveal: String,
    pub name_color: String,
    pub text_color: String,
    pub ruby_color: String,
    pub countdown_color: String,
    pub display_seconds: f64,
    pub toggle_dialog: String,
    pub toggle_hold: String,
    pub idle_stop_secs: u64,
    /// The camelCase names the files actually carry. A key not in here is showing the
    /// built-in, and the form marks it as such rather than implying the user chose it.
    pub written: Vec<String>,
}

/// The program's built-in behaviour for every one of those settings.
///
/// Not a third configuration layer: these are the values the presenter and the runtime
/// already use when a key is absent (`AppConfig.DialogSection`, the presenter's
/// `DialogTheme`, `voice-core-runtime`'s `--idle-stop-secs` default). They are restated
/// here so a control can show what is in force instead of a blank — and `written` is what
/// keeps that honest.
///
/// The two translucent colours are `#aarrggbb`, which is the presenter's own notation and
/// the reason the hex field accepts eight digits.
impl Default for Settings {
    fn default() -> Self {
        Self {
            annotation_above: false,
            reveal: "typewriter".to_string(),
            name_color: "#a48bff".to_string(),
            text_color: "#f2f2f2".to_string(),
            ruby_color: "#9effffff".to_string(),
            countdown_color: "#d98b6cef".to_string(),
            display_seconds: 6.0,
            toggle_dialog: "Ctrl+Alt+D".to_string(),
            toggle_hold: "Ctrl+Alt+H".to_string(),
            idle_stop_secs: 900,
            written: Vec::new(),
        }
    }
}

/// One member of a section, recording the key as written when it is there.
///
/// A free function rather than a closure so it can borrow `written` without also holding
/// the `Settings` the caller is filling in.
fn authored<'a>(section: Option<&'a Value>, key: &str, written: &mut Vec<String>) -> Option<&'a Value> {
    let found = section.and_then(|section| section.get(key))?;
    written.push(key.to_string());
    Some(found)
}

/// `data/config.json` and `data/runtime.json`, read as settings rather than as bytes.
///
/// A file that will not parse is not an error here: the form still has to render, and it
/// renders the built-ins with nothing marked as written — which is exactly what a user
/// whose file is broken needs to see, next to the raw view that shows them the file.
#[tauri::command]
pub async fn settings_read(app: AppHandle) -> Settings {
    let host = app.state::<Host>();
    let config = read_jsonc(&config_edit::config_path(&host));
    let runtime = read_jsonc(&config_edit::runtime_path(&host));
    let dialog = config.as_ref().and_then(|root| root.get("dialog"));
    let hotkeys = config.as_ref().and_then(|root| root.get("hotkeys"));

    let mut out = Settings::default();
    let seen = &mut out.written;

    // Per key rather than per section: a file that says `reveal` and nothing else has one
    // written key, and a form that marked the whole section as authored would be wrong
    // about six controls to be right about one.
    let mut annotation_above = None;
    let mut reveal = None;
    let mut name_color = None;
    let mut text_color = None;
    let mut ruby_color = None;
    let mut countdown_color = None;
    let mut display_seconds = None;
    let mut toggle_dialog = None;
    let mut toggle_hold = None;
    let mut idle_stop_secs = None;

    if let Some(value) = authored(dialog, "annotationAbove", seen) {
        annotation_above = value.as_bool();
    }
    if let Some(value) = authored(dialog, "reveal", seen) {
        reveal = value.as_str().map(str::to_string);
    }
    if let Some(value) = authored(dialog, "nameColor", seen) {
        name_color = value.as_str().map(str::to_string);
    }
    if let Some(value) = authored(dialog, "textColor", seen) {
        text_color = value.as_str().map(str::to_string);
    }
    if let Some(value) = authored(dialog, "rubyColor", seen) {
        ruby_color = value.as_str().map(str::to_string);
    }
    if let Some(value) = authored(dialog, "countdownColor", seen) {
        countdown_color = value.as_str().map(str::to_string);
    }
    if let Some(value) = authored(dialog, "displaySeconds", seen) {
        display_seconds = value.as_f64();
    }
    if let Some(value) = authored(hotkeys, "toggleDialog", seen) {
        toggle_dialog = value.as_str().map(str::to_string);
    }
    if let Some(value) = authored(hotkeys, "toggleHold", seen) {
        toggle_hold = value.as_str().map(str::to_string);
    }
    if let Some(value) = authored(runtime.as_ref(), "idleStopSecs", seen) {
        idle_stop_secs = value.as_u64();
    }

    // A key of the wrong type keeps its place in `written` and the built-in as its value:
    // the file does carry it, so the form must not claim otherwise, and the raw view right
    // below is where the user sees why the control disagrees with the line they typed.
    out.annotation_above = annotation_above.unwrap_or(out.annotation_above);
    out.reveal = reveal.unwrap_or(out.reveal);
    out.name_color = name_color.unwrap_or(out.name_color);
    out.text_color = text_color.unwrap_or(out.text_color);
    out.ruby_color = ruby_color.unwrap_or(out.ruby_color);
    out.countdown_color = countdown_color.unwrap_or(out.countdown_color);
    out.display_seconds = display_seconds.unwrap_or(out.display_seconds);
    out.toggle_dialog = toggle_dialog.unwrap_or(out.toggle_dialog);
    out.toggle_hold = toggle_hold.unwrap_or(out.toggle_hold);
    out.idle_stop_secs = idle_stop_secs.unwrap_or(out.idle_stop_secs);
    out
}

/// Every recorded change, newest first — which is the order the 版本 list reads in, and the
/// order the panel would otherwise have to impose on an append-only file.
///
/// The entries themselves are `config_edit`'s: it is what writes them, and a second shape
/// here would be a second definition of what a change is.
#[tauri::command]
pub async fn settings_history(app: AppHandle) -> Vec<config_edit::Change> {
    let host = app.state::<Host>();
    let mut out = config_edit::history(&host);
    out.reverse();
    out
}

/// What a pack's own manifest says about its subtitle style. `None` per field means the
/// manifest is silent there, which is a different thing from an empty string.
#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DialogFields {
    pub name_color: Option<String>,
    pub text_color: Option<String>,
    pub ruby_color: Option<String>,
    pub countdown_color: Option<String>,
    pub reveal: Option<String>,
    pub display_seconds: Option<f64>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SynthesisFields {
    pub num_steps: Option<u32>,
    pub seed: Option<i64>,
    pub temperature: Option<f64>,
}

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExpressionFields {
    /// The caption text, which for this engine is emoji written into the input
    /// (`EMOJI_ANNOTATIONS.md` in the model card).
    pub emotion: Option<String>,
    pub cfg_scale_caption: Option<f64>,
}

/// One pack as its editing page needs it: what its manifest says, where that manifest is,
/// and what a reader sees today.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PackConfig {
    pub id: String,
    /// The payload on disk, absolute.
    pub path: String,
    pub manifest_path: String,
    pub manifest_exists: bool,
    /// False when the pack sits somewhere this app cannot write — read-only media, a
    /// share. The page disables its controls and says why instead of failing per save.
    pub writable: bool,
    pub schema: Option<u32>,
    pub name: Option<String>,
    pub character: Option<String>,
    pub kind: Option<String>,
    pub languages: Option<Vec<String>>,
    pub engine: Option<String>,
    pub avatar: Option<String>,
    pub dialog: DialogFields,
    pub synthesis: SynthesisFields,
    pub expression: ExpressionFields,
    /// Top-level manifest keys this build has never heard of.
    ///
    /// Listed rather than hidden: the page promises a newer build's fields survive a save,
    /// and a promise nobody can see is not one.
    pub unknown: Vec<String>,
    /// The identity a reader sees today, merged the way the runtime merges it
    /// (`config_edit::read_packs`). It prefills a control the manifest is silent about, so
    /// the form starts from what is true rather than from a blank.
    pub effective: Pack,
}

/// Every top-level key this build reads out of a manifest. Anything else is `unknown`.
const MANIFEST_KEYS: [&str; 12] = [
    "schema",
    "id",
    "name",
    "character",
    "kind",
    "languages",
    "engine",
    "avatar",
    "dialog",
    "synthesis",
    "expression",
    // Written by `scripts/training/install_pack.py`; read by nothing, shown by nothing, and
    // named here so it is not reported to the user as a field this build might lose.
    "trainedFrom",
];

/// The pack registered under `id`, typed for its form. `None` when there is no such pack.
#[tauri::command]
pub async fn pack_config(app: AppHandle, id: String) -> Option<PackConfig> {
    let host = app.state::<Host>();
    let pack = config_edit::read_packs(&host).into_iter().find(|pack| pack.id == id)?;
    let payload = Path::new(&pack.path);
    let manifest_file = config_edit::manifest_file(payload)?;
    let manifest = config_edit::manifest_beside(payload);

    let get = |key: &str| manifest.as_ref().and_then(|root| root.get(key));
    let text = |key: &str| get(key).and_then(Value::as_str).map(str::to_string);
    let section = |key: &str| get(key).cloned().unwrap_or(Value::Null);
    let dialog = section("dialog");
    let synthesis = section("synthesis");
    let expression = section("expression");
    let member = |from: &Value, key: &str| from.get(key).cloned().unwrap_or(Value::Null);

    Some(PackConfig {
        id: pack.id.clone(),
        path: pack.path.clone(),
        manifest_exists: manifest_file.exists(),
        writable: config_edit::pack_writable(payload),
        manifest_path: config_edit::native(&manifest_file),
        schema: get("schema").and_then(Value::as_u64).map(|value| value as u32),
        name: text("name"),
        character: text("character"),
        kind: text("kind"),
        languages: get("languages").and_then(Value::as_array).map(|items| {
            items.iter().filter_map(Value::as_str).map(str::to_string).collect()
        }),
        engine: text("engine"),
        avatar: text("avatar"),
        dialog: DialogFields {
            name_color: member(&dialog, "nameColor").as_str().map(str::to_string),
            text_color: member(&dialog, "textColor").as_str().map(str::to_string),
            ruby_color: member(&dialog, "rubyColor").as_str().map(str::to_string),
            countdown_color: member(&dialog, "countdownColor").as_str().map(str::to_string),
            reveal: member(&dialog, "reveal").as_str().map(str::to_string),
            display_seconds: member(&dialog, "displaySeconds").as_f64(),
        },
        synthesis: SynthesisFields {
            num_steps: member(&synthesis, "numSteps").as_u64().map(|value| value as u32),
            seed: member(&synthesis, "seed").as_i64(),
            temperature: member(&synthesis, "temperature").as_f64(),
        },
        expression: ExpressionFields {
            emotion: member(&expression, "emotion").as_str().map(str::to_string),
            cfg_scale_caption: member(&expression, "cfgScaleCaption").as_f64(),
        },
        unknown: manifest
            .as_ref()
            .and_then(Value::as_object)
            .map(|object| {
                object
                    .keys()
                    .filter(|key| !MANIFEST_KEYS.contains(&key.as_str()))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default(),
        effective: pack,
    })
}

/// One synthesised line, so a style change can be heard from the page that made it.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preview {
    pub request_id: String,
    pub audio_id: String,
    pub bytes: u64,
    pub duration_ms: u64,
    pub total_ms: u64,
    pub cold_start: bool,
    /// Event-stream subscribers at the moment of synthesis. Zero means nothing was
    /// listening, so the clip is on the spool and nobody played it — which the page has to
    /// say, or a silent 试听 reads as a broken button.
    pub presenters: u64,
}

/// Speak one line with this pack, through the runtime.
///
/// No synthesis here and no engine flags: this is the same `POST /api/speak` the CLI and
/// an agent use, with the pack id and nothing else, so whatever the pack's own manifest
/// asks for server-side is what gets heard. That is the point — the preview has to be the
/// product of the file the form just wrote, not of this command's opinion.
#[tauri::command]
pub async fn speak_preview(app: AppHandle, id: String, text: String) -> Result<Preview, String> {
    let host = app.state::<Host>();
    let token = host.token().ok_or("还没有令牌：data\\token.txt 在运行时第一次启动时出现")?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Err("试听要有一句话".to_string());
    }

    let response = host
        .http
        .post(format!("{}/api/speak", host.base_url))
        .bearer_auth(token)
        // The client's own 3 s ceiling is sized for status polls. A cold model load plus
        // one utterance is measured in tens of seconds, so this call carries its own.
        .timeout(Duration::from_secs(180))
        .json(&serde_json::json!({ "voicePackId": id, "text": trimmed }))
        .send()
        .await
        .map_err(|err| format!("服务没有应答：{err}"))?;

    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    if !status.is_success() {
        // The runtime's own message names the field it refused, which is the whole value of
        // forwarding it: an invalid `expression.cfgScaleCaption` in the manifest shows up
        // here as that sentence rather than as "HTTP 400".
        let detail = serde_json::from_str::<Value>(&body)
            .ok()
            .and_then(|value| value.get("message").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_else(|| body.trim().to_string());
        return Err(if detail.is_empty() {
            format!("服务回了 HTTP {}", status.as_u16())
        } else {
            detail
        });
    }

    let out: Value = serde_json::from_str(&body).map_err(|err| format!("应答读不出来：{err}"))?;
    let number = |key: &str| out.get(key).and_then(Value::as_u64).unwrap_or(0);
    Ok(Preview {
        request_id: out
            .get("requestId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        audio_id: out
            .get("audioId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        bytes: number("bytes"),
        duration_ms: number("durationMs"),
        total_ms: number("totalMs"),
        cold_start: out.get("coldStart").and_then(Value::as_bool).unwrap_or(false),
        presenters: number("presenters"),
    })
}

#[cfg(test)]
mod tests {
    use super::read_file;
    use std::path::PathBuf;

    /// A private directory under the OS temp dir, named after the case and cleared on the
    /// way in: a failing assert has to leave its tree behind to be looked at.
    fn scratch(case: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("voice-core-configview-{case}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_absent_file_is_reported_rather_than_failing() {
        let dir = scratch("absent");
        let file = read_file("runtime.json".to_string(), &dir.join("runtime.json"));

        // The screen shows this as 文件不存在 with the path still usable, which is the
        // normal state of runtime.json before the first deployment.
        assert!(!file.exists);
        assert_eq!(file.bytes, 0);
        assert!(file.text.is_empty());
        assert!(file.path.ends_with("runtime.json"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_jsonc_file_arrives_byte_for_byte() {
        let dir = scratch("verbatim");
        let path = dir.join("config.json");
        // Comments, a trailing comma and CRLF: everything a re-serialised parse would
        // drop, and the reason this screen shows text instead of a re-rendered object.
        let raw = "// 顶部说明\r\n{\r\n  \"voicePacks\": [],\r\n}\r\n";
        std::fs::write(&path, raw).unwrap();

        let file = read_file("config.json".to_string(), &path);

        assert!(file.exists);
        assert_eq!(file.text, raw);
        assert_eq!(file.bytes, raw.len() as u64);

        std::fs::remove_dir_all(&dir).unwrap();
    }
}
