//! `data/config.json`, watched.
//!
//! The runtime and the presenter both reload this file on mtime change
//! (`config_edit.rs::replace`), and the panel was the one reader left finding
//! out on its next command instead. An agent registering or removing a pack
//! while the panel is open must reach it the way every other out-of-band state
//! change here reaches the screen: an event pushed on change, nothing between
//! changes.
//!
//! Only `config.json` is watched, on purpose. Every registration fact the panel
//! shows comes from its `voicePacks` array; a pack's own `voicepack.json` is
//! read fresh whenever the 配置 or 音色 screen opens, so its changes need no
//! push — they are fetched, not waited on.
//!
//! The mechanism is the supervisor's probe, not a file-system watcher: a 1 s
//! mtime poll over `supervise::watch`'s 2 s process poll. A watcher library
//! would buy event edges this panel has no use for — every consumer here
//! re-reads the file — at the cost of a thread per directory and a crate.

use std::path::Path;
use std::time::{Duration, SystemTime};

use tauri::{AppHandle, Emitter, Manager};

use crate::config_edit::config_path;
use crate::contract::{ConfigChange, EVENT_CONFIG};
use crate::host::Host;

/// How often the file is sampled. Fast enough that an agent's registration
/// lands while the user is still looking at the panel; slow enough that it is
/// one `metadata` call a second, which is nothing.
const POLL: Duration = Duration::from_secs(1);

/// Start the watcher. One background task, one piece of state — the mtime it
/// last saw — and it never writes anything.
pub fn start(app: AppHandle) {
    tauri::async_runtime::spawn(async move {
        // `None` is the baseline, not a fact about the file: a fresh install has
        // no `config.json` yet, and its first creation is exactly the kind of
        // change this exists to report. So the first real sample always differs
        // from the baseline and is emitted as a change — the panel's initial
        // read makes that harmless.
        let mut seen: Option<SystemTime> = None;
        loop {
            let host = app.state::<Host>();
            let mtime = sample(&config_path(&host));
            if changed(seen, mtime) {
                // The path is resolved through the same borrow the sample came
                // from, so the event cannot name a file this build resolved
                // from another data dir.
                let _ = app.emit(
                    EVENT_CONFIG,
                    ConfigChange {
                        path: config_path(&host).display().to_string(),
                    },
                );
                host.log("config.json changed on disk");
            }
            seen = mtime;
            tokio::time::sleep(POLL).await;
        }
    });
}

/// The file's mtime, or `None` when it does not exist. Read errors and absence
/// are the same answer, because for a poller they are the same situation: this
/// tick, there is nothing to report.
fn sample(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
}

/// Whether the new sample differs from the one the watcher is holding. Pure so
/// the four transitions can be read and tested without a filesystem: equal
/// mtimes and an absent file that was absent say nothing, anything else — a
/// first appearance, a deletion, a write — is one event.
fn changed(before: Option<SystemTime>, after: Option<SystemTime>) -> bool {
    before != after
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four transitions a poller can see, and what each is worth. `None`
    /// means the file is absent, which is a state rather than a failure to
    /// sample: a config.json that vanished while the panel was open is exactly
    /// a change the screen has to hear about.
    #[test]
    fn only_a_transition_is_a_change() {
        let t = SystemTime::UNIX_EPOCH;
        let later = t + Duration::from_secs(1);
        assert!(!changed(Some(t), Some(t)), "same mtime, nothing moved");
        assert!(changed(Some(t), Some(later)), "a write");
        assert!(changed(None, Some(t)), "first appearance");
        assert!(changed(Some(t), None), "a deletion");
        assert!(!changed(None, None), "still absent is still nothing");
    }

    /// An absent file is the empty answer rather than an error, which is what
    /// lets the watcher run before the first `config.json` exists at all.
    #[test]
    fn sampling_an_absent_file_answers_none() {
        assert_eq!(sample(Path::new("voice-core-watch-no-such-file")), None);
    }
}
