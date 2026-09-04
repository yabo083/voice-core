//! The three commands that reach outside this process: two native pickers and one
//! shell-open.
//!
//! The pickers exist here, in Rust, rather than as a plugin permission the
//! frontend holds — the dialog plugin's own commands are granted to nobody. The
//! difference matters for `open_path`, which is the only command in this app that
//! hands a path to the shell, and therefore the only one that has to justify
//! every path it accepts.

use std::path::PathBuf;

use tauri::{AppHandle, Manager};
use tauri_plugin_dialog::DialogExt;

use crate::detect;
use crate::host::Host;
use crate::layout;

/// Extensions Windows would *run* rather than open.
///
/// The allow-list below already limits `open_path` to this install's own
/// directories — but those directories contain three executables and a PowerShell
/// script, so without this an injected string in the frontend would be arbitrary
/// local execution. Nothing the panel legitimately offers to open is in this list.
const NEVER_OPEN: [&str; 16] = [
    "exe", "com", "bat", "cmd", "ps1", "psm1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "msi",
    "scr", "lnk", "hta",
];

#[tauri::command]
pub async fn pick_folder(app: AppHandle, title: String) -> Option<String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file().set_title(title);
    // Parented so the picker cannot end up behind the window that opened it.
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    dialog.pick_folder(move |picked| {
        let _ = sender.send(picked);
    });
    receiver
        .await
        .ok()
        .flatten()
        .and_then(|picked| picked.into_path().ok())
        .map(|path| path.display().to_string())
}

#[tauri::command]
pub async fn pick_file(app: AppHandle, title: String, extensions: Vec<String>) -> Option<String> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut dialog = app.dialog().file().set_title(title);
    if let Some(window) = app.get_webview_window("main") {
        dialog = dialog.set_parent(&window);
    }
    if !extensions.is_empty() {
        // The filter's name is the pattern list, because a name like "files" tells
        // the user nothing about what the dialog will show.
        let label = extensions
            .iter()
            .map(|extension| format!("*.{extension}"))
            .collect::<Vec<_>>()
            .join(" ");
        let patterns: Vec<&str> = extensions.iter().map(String::as_str).collect();
        dialog = dialog.add_filter(label, &patterns);
    }
    dialog.pick_file(move |picked| {
        let _ = sender.send(picked);
    });
    receiver
        .await
        .ok()
        .flatten()
        .and_then(|picked| picked.into_path().ok())
        .map(|path| path.display().to_string())
}

/// Show a folder or a file in the shell.
///
/// Validated against the directories this install owns, not against what the
/// caller claims. A voice pack's path deliberately does not widen the allow-list:
/// `register_pack` lets the frontend put any string into `config.json`, so trusting
/// a pack path here would mean the frontend could name its own sandbox.
#[tauri::command]
pub async fn open_path(app: AppHandle, path: String) -> Result<(), String> {
    let host = app.state::<Host>();
    let target = PathBuf::from(path.trim());

    if !target.exists() {
        return Err(format!("{} does not exist", target.display()));
    }
    if let Some(extension) = target.extension().and_then(|ext| ext.to_str()) {
        let extension = extension.to_ascii_lowercase();
        if NEVER_OPEN.contains(&extension.as_str()) {
            return Err(format!(
                "refusing to shell-open a .{extension} file: that would run it, not show it"
            ));
        }
    }
    if !layout::is_inside(&detect::trusted_roots(&host), &target) {
        return Err(format!(
            "{} is outside this install: open_path reaches the install root, the data dir, the \
             engine root and the model cache, and nothing else",
            target.display()
        ));
    }

    tauri_plugin_opener::open_path(&target, None::<&str>).map_err(|err| err.to_string())
}
