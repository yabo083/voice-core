//! Shared state, and the three things every module here needs: a log file, a
//! spawn that does not flash a console window, and a clock.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::detect::Probe;
use crate::jsonstream::StreamRun;
use crate::layout;
use crate::supervise::Stack;

pub struct Host {
    pub root: PathBuf,
    pub data_dir: PathBuf,
    /// The runtime's own clap default. This app never passes `--bind`, so the
    /// port is not a choice made here and the frontend can print it as a
    /// literal.
    pub base_url: String,
    pub http: reqwest::Client,
    pub stack: Mutex<Stack>,
    /// The bootstrap run, and the LoRA training run. Two slots rather than one: they own
    /// different directories, they are cancelled by different buttons, and training a
    /// voice while an engine is being provisioned is a thing to refuse per slot, not a
    /// thing to serialise globally.
    pub provision: Mutex<StreamRun>,
    pub training: Mutex<StreamRun>,
    /// Importing torch costs about 3 s and the whole interpreter probe about 5 s
    /// cold, which is too slow to repeat on every `detect()`. The answer is kept
    /// for the process lifetime and dropped when a provision run ends — the only
    /// event that can put a different interpreter on disk.
    pub probe: Mutex<Option<Probe>>,
}

impl Host {
    pub fn new() -> Self {
        let root = layout::install_root();
        let data_dir = layout::resolve_data_dir(&root);
        layout::logs_dir(&data_dir);
        Self {
            root,
            data_dir,
            base_url: "http://127.0.0.1:8760".to_string(),
            // Short connect timeout because the only peer is loopback: when the
            // runtime is down the connection is refused immediately, and a
            // second of patience would only make the UI feel broken. The read
            // timeout is longer than a status call can plausibly take.
            http: reqwest::Client::builder()
                .connect_timeout(Duration::from_millis(600))
                .timeout(Duration::from_secs(3))
                .build()
                .unwrap_or_else(|_| reqwest::Client::new()),
            stack: Mutex::new(Stack::default()),
            provision: Mutex::new(StreamRun::default()),
            training: Mutex::new(StreamRun::default()),
            probe: Mutex::new(None),
        }
    }

    pub fn token(&self) -> Option<String> {
        layout::read_token(&self.root, &self.data_dir)
    }

    /// This app's own diary, `data/logs/manager.log`. Separate from the
    /// children's redirected pipes: this is what *this* process decided, and it
    /// is the only record of a supervision decision that has no stdout.
    pub fn log(&self, line: &str) {
        let path = layout::logs_dir(&self.data_dir).join("manager.log");
        if let Ok(mut handle) = OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(handle, "{} {line}", now_ms());
        }
    }

    /// A child's pipe target. Append, because the previous run's crash output is
    /// the thing worth reading.
    pub fn child_log(&self, name: &str) -> std::io::Result<File> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(layout::logs_dir(&self.data_dir).join(name))
    }

    /// Same, but truncated, for output whose meaning is "the last run": a stale
    /// tail read back after a failure would name the wrong cause.
    pub fn fresh_child_log(&self, name: &str) -> std::io::Result<File> {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(layout::logs_dir(&self.data_dir).join(name))
    }
}

/// No console window for a background child. Without it, spawning
/// `powershell.exe` or the console-subsystem runtime flashes a black rectangle
/// over whatever the user is looking at.
pub fn hidden(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    {
        let _ = command;
    }
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}
