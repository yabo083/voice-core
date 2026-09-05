//! The configuration files themselves, for a screen that only reads them.
//!
//! Two files decide how this program behaves — `data/config.json` for the program and a
//! pack's own `voicepack.json` — and both are JSONC written for a human: comments
//! explaining every key, and a `//` note above the line somebody changed. So nothing
//! here parses, merges or re-serialises. It hands over bytes, a path and a size, and the
//! screen shows the file as it is.
//!
//! Which file *won* a field is not computed here either. `src/packs.rs::hydrate` in the
//! runtime is the one implementation of that precedence, and it now reports its own
//! verdict per field, so `pack_effective` forwards that answer instead of a second
//! opinion about the same two files.
//!
//! Read-only by construction: no command in this module writes. Editing goes on being
//! what it already was — `config_edit` for the registry, an editor for the comments.

use std::path::Path;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::config_edit;
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

/// The program's two files, in the order the screen shows them.
#[tauri::command]
pub async fn config_files(app: AppHandle) -> Vec<ConfigFile> {
    let host = app.state::<Host>();
    vec![
        read_file(
            "config.json · 对话框、快捷键、装了哪些音色包".to_string(),
            &config_edit::config_path(&host),
        ),
        read_file(
            "runtime.json · 部署写下的引擎位置与空闲释放时间".to_string(),
            &host.data_dir.join("runtime.json"),
        ),
    ]
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
