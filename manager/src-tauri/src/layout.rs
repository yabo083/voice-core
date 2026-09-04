//! Where everything is.
//!
//! Every path this app touches is derived from `VoiceCore.exe`'s own location.
//! That is the invariant that lets the whole tree be moved, copied to another
//! machine or installed twice side by side — the same reason `runtime.json`'s
//! relative paths resolve against the install root and never against the working
//! directory, since a shortcut, a tray launch and a shell each start with a
//! different cwd.

use std::path::{Path, PathBuf};

/// Install root: the directory holding `bin/`, `scripts/` and `data/`.
///
/// `voice-core-runtime` finds it by stepping out of `bin/` and then probing
/// ancestors for `Cargo.toml`. Neither rule fits this executable: packaged, it
/// sits *at* the root rather than in `bin/`, and in a checkout it sits in
/// `manager/src-tauri/target/<profile>/`, whose nearest `Cargo.toml` is this
/// crate's — four levels below the repo root the runtime would have found. So
/// the marker is `scripts/bootstrap.ps1`, which exists in both trees, and the
/// walk starts at the executable's own directory so a packaged install matches
/// immediately and can never escape into a parent.
///
/// When the marker is gone the executable's directory is the answer, which is
/// still correct for a packaged tree; provisioning is broken in that state
/// either way, because provisioning *is* that script.
pub fn install_root() -> PathBuf {
    let Ok(exe) = std::env::current_exe() else {
        return PathBuf::from(".");
    };
    let dir = exe
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    // Six levels: `target/<triple>/<profile>` plus `src-tauri`, `manager`, repo.
    for candidate in dir.ancestors().take(6) {
        if candidate.join("scripts/bootstrap.ps1").is_file() {
            return candidate.to_path_buf();
        }
    }
    dir
}

/// `<root>/data`, unless the install directory is read-only — a Program Files
/// install must still be able to keep a token, a spool and logs somewhere.
///
/// Byte-for-byte the runtime's rule, because both processes must land on the
/// same directory. The GUI then passes the result to `--data-dir` explicitly, so
/// the two cannot diverge even if the rule changes on one side.
pub fn resolve_data_dir(root: &Path) -> PathBuf {
    let preferred = root.join("data");
    if is_writable(&preferred) {
        return preferred;
    }
    match std::env::var_os("APPDATA") {
        Some(appdata) => PathBuf::from(appdata).join("voice-core"),
        None => preferred,
    }
}

fn is_writable(dir: &Path) -> bool {
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    let probe = dir.join(".write-probe");
    match std::fs::write(&probe, b"") {
        Ok(()) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Relative paths in `runtime.json` resolve against the install root. Absolute
/// ones are honoured as they are, which is what makes reusing an engine tree
/// that already exists somewhere else possible at all.
pub fn absolute(root: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    }
}

/// The bearer token, in `voice-core`'s order: the environment first, then
/// `token.txt` in the data dir.
///
/// The CLI has a third candidate, `<exe dir>/../data/token.txt`, because the CLI
/// lives in `bin/`. This executable lives at the root, where the same relative
/// path points *outside* the install and could pick up a stranger's token, so it
/// is replaced by `<root>/data/token.txt` — which matters when the data dir fell
/// back to `%APPDATA%` but a token still exists in the tree.
///
/// Absent is not an error: before the runtime has ever started there is nothing
/// to read.
pub fn read_token(root: &Path, data_dir: &Path) -> Option<String> {
    if let Some(token) = std::env::var("VC_TOKEN").ok().filter(|t| !t.trim().is_empty()) {
        return Some(token.trim().to_string());
    }
    let candidates = [data_dir.join("token.txt"), root.join("data/token.txt")];
    for candidate in candidates {
        if let Ok(contents) = std::fs::read_to_string(&candidate) {
            let token = contents.trim();
            if !token.is_empty() {
                return Some(token.to_string());
            }
        }
    }
    None
}

/// `data/logs`, created on demand. Every child process this app spawns has its
/// pipes pointed here.
pub fn logs_dir(data_dir: &Path) -> PathBuf {
    let dir = data_dir.join("logs");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// The backend service. `bin/` in a package, `target/<profile>/` in a checkout.
pub fn runtime_exe(root: &Path) -> Option<PathBuf> {
    first_existing([
        root.join("bin/voice-core-runtime.exe"),
        root.join("target/release/voice-core-runtime.exe"),
        root.join("target/debug/voice-core-runtime.exe"),
    ])
}

/// The subtitle presenter.
///
/// The assembly is `VoiceCorePresenter`; the pre-rename `VoiceCoreTray.exe`
/// stays in the list so an install packaged before the rename still starts one.
/// `bin/app/` — where v1.1.0 put it — is deliberately absent: that build has no
/// `--presenter` flag, so it would add a second tray icon to a tray this app
/// owns, which is exactly the confusion this round removes.
pub fn presenter_exe(root: &Path) -> Option<PathBuf> {
    const DEV_TFM: &str = "net8.0-windows10.0.22621.0";
    first_existing([
        root.join("bin/presenter/VoiceCorePresenter.exe"),
        root.join("bin/presenter/VoiceCoreTray.exe"),
        root.join(format!(
            "app/VoiceCoreTray/bin/x64/Release/{DEV_TFM}/VoiceCorePresenter.exe"
        )),
        root.join(format!(
            "app/VoiceCoreTray/bin/x64/Release/{DEV_TFM}/VoiceCoreTray.exe"
        )),
        root.join(format!(
            "app/VoiceCoreTray/bin/x64/Debug/{DEV_TFM}/VoiceCorePresenter.exe"
        )),
        root.join(format!(
            "app/VoiceCoreTray/bin/x64/Debug/{DEV_TFM}/VoiceCoreTray.exe"
        )),
    ])
}

/// `scripts/bootstrap.ps1`. Also the marker [`install_root`] looks for, so this
/// returning `None` means the tree is not an install at all.
pub fn bootstrap_script(root: &Path) -> Option<PathBuf> {
    first_existing([root.join("scripts/bootstrap.ps1")])
}

fn first_existing<I: IntoIterator<Item = PathBuf>>(candidates: I) -> Option<PathBuf> {
    candidates.into_iter().find(|path| path.is_file())
}

/// Is `target` inside one of `roots`?
///
/// Both sides are canonicalised, so `..`, a short 8.3 name and a symlink cannot
/// walk out. Comparison is by path component, so `<root>data-evil` does not pass
/// as being inside `<root>data`. A path that does not exist fails: the only
/// caller shell-opens what it is given, and opening something absent is not a
/// thing anyone asked for.
pub fn is_inside(roots: &[PathBuf], target: &Path) -> bool {
    let Ok(target) = target.canonicalize() else {
        return false;
    };
    roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| target.starts_with(&root))
}
