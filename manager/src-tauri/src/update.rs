//! Update check and apply, against GitHub Releases.
//!
//! One command family answers three questions: is there a newer release, which
//! asset should be downloaded, and where the download is. Applying is
//! [`update_install`], which hands the already-verified installer to the same
//! Inno Setup chain the README's manual upgrade follows — CloseApplications via
//! AppMutex, `data\` preserved, [Run] relaunching the panel — so an update and a
//! manual install-over exercise the same code path by construction.
//!
//! The version source is this crate's `CARGO_PKG_VERSION`, which `package.ps1`
//! keeps in lockstep with the runtime crate's and `tauri.conf.json`'s; release
//! tags are expected to be that same number with a leading `v`.
//!
//! China mirrors: GitHub's release endpoints are reachable or not depending on
//! region and ISP. Every mirror voice-core ships with is a *prefix* proxy — the
//! URL becomes `<mirror>/<github-url>` — so one rule covers both the
//! `api.github.com` JSON and the `releases/download` asset. Each candidate is
//! tried in order; the first answer wins, and the winner of the *check* is
//! remembered only as the download's first candidate, never as a fact
//! about the network: a mirror that answered seconds ago can be dead now, so
//! the download walks the whole list again. A user behind their own proxy names
//! it in `VC_UPDATE_PROXY` — the string goes straight to reqwest's proxy
//! support (`http(s)://`, `socks5://`) and applies to every candidate, which is
//! the distinction the two controls draw: mirrors choose *which host*, the env
//! var chooses *how to reach any host*.
//!
//! Progress: the download streams into [`Host::update`] and the panel polls
//! [`update_status`] — the same shape the train screen uses to watch a run it
//! did not start, and the reason a download survives closing the screen that
//! started it: it belongs to the process, not to the page.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::Manager;

use crate::host::{hidden, now_ms, Host};
use crate::layout;

/// The only repo this build looks at, baked at compile time.
const REPO: &str = "yabo083/voice-core";

/// Direct GitHub first: it is the only source that cannot lag or lie, and for
/// the machines that reach it there is no reason to prefer anything else. The
/// mirrors after it are public prefix proxies run by different operators, so
/// one being down says nothing about the others.
const MIRRORS: [(&str, &str); 4] = [
    ("direct", ""),
    ("ghproxy.net", "https://ghproxy.net"),
    ("gh-proxy.com", "https://gh-proxy.com"),
    ("ghfast.top", "https://ghfast.top"),
];

/// How long one mirror gets to answer a check or produce response headers.
/// Long enough for a trans-Pacific TLS handshake, short enough that a dead
/// mirror costs seconds. The *body* stream gets [`DOWNLOAD_IDLE_TIMEOUT`]
/// instead: a five-hundred-second installer over a slow link is healthy, and a
/// total-transfer timeout is how a working download gets abandoned.
const PER_MIRROR_TIMEOUT: Duration = Duration::from_secs(20);
/// Max silence between body chunks: a stalled connection is cut, a slow one is not.
const DOWNLOAD_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// The release JSON actually read. Everything else the API answers with is
/// dropped at this boundary: a field the panel does not show cannot become a
/// thing the panel has to explain.
#[derive(Clone, Debug, Serialize)]
pub struct ReleaseInfo {
    pub tag: String,
    /// `v1.8.0` → `1.8.0`; verbatim when the tag carries no leading `v`. The
    /// comparison reports "cannot compare" rather than guessing.
    pub version: String,
    /// The author's headline for the release.
    pub name: String,
    /// The author's own markdown, forwarded for 查看更新说明.
    pub notes: String,
    /// The release page, for the browser button.
    pub url: String,
    /// Published epoch ms, so the panel can say how recent it is.
    pub published_ms: u64,
    /// The installer asset, once found.
    pub asset: Option<AssetInfo>,
}

/// One downloadable file of the release.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetInfo {
    pub name: String,
    /// Bytes, the progress bar's total.
    pub size: u64,
    /// The bare GitHub URL; a mirror wraps it at download time.
    pub url: String,
    /// GitHub computes and stores SHA-256 per asset. The digest is mandatory at
    /// install time: a hash we do not have is a hash we cannot check.
    #[serde(default)]
    pub digest: Option<String>,
}

/// What [`update_check`] answers.
#[derive(Debug, Serialize)]
pub struct CheckOutcome {
    /// What this build is, from the compiler: the baseline of the comparison.
    pub current: String,
    /// `true` when the release's version parses strictly higher than `current`.
    /// A non-parsing pair answers `false` — the panel then shows both versions
    /// and lets a human decide, which is all a parser that cannot read a tag
    /// gets to claim.
    pub update_available: bool,
    pub release: ReleaseInfo,
}

/// Where a download is, right now — the polled shape.
#[derive(Clone, Debug, Default, Serialize)]
pub struct DownloadProgress {
    /// True between the first byte and the end, however it ends; a terminal
    /// state is `done` or `failed` with `active` false, so the panel renders
    /// the last state without ever confusing it with "running".
    pub active: bool,
    pub downloaded: u64,
    pub total: u64,
    /// Terminal success: staged, verified, awaiting the install button.
    pub done: bool,
    /// Terminal failure; `error` carries the sentence.
    pub failed: bool,
    /// Terminal cancel, requested by the user. The partial stays on disk as a
    /// resume point, so this is not `failed`: the retry button continues from
    /// the bytes already on disk instead of starting over.
    pub cancelled: bool,
    pub error: String,
}

/// The whole state of the one update this process runs at a time.
#[derive(Clone, Debug, Default, Serialize)]
pub struct UpdateState {
    pub progress: DownloadProgress,
    /// The staged installer's path, set the moment the file is verified — so a
    /// crash between verify and install cannot cause a second download.
    pub staged: Option<String>,
    /// The installer's path once spawned detached, so the panel can say 安装程序
    /// 已启动 and mean it.
    pub launched: Option<String>,
}

/// Shared through `Host`, one download at a time. A `Mutex` rather than
/// atomics because the payload is a struct written and read as a whole; the
/// lock is held for field writes, never across I/O.
#[derive(Default)]
pub struct UpdateDownload {
    inner: Mutex<UpdateState>,
    /// Set by [`update_cancel`] while a transfer runs; the read loop in
    /// [`try_candidate`] checks it between chunks and bails. A separate flag
    /// rather than a `progress.active = false` write because the run loop owns
    /// `progress` — a cancel that only cleared fields would race the writer.
    cancelled: std::sync::atomic::AtomicBool,
}

impl UpdateDownload {
    fn set<F: FnOnce(&mut UpdateState)>(&self, f: F) {
        let mut guard = self.inner.lock().unwrap_or_else(|err| err.into_inner());
        f(&mut guard);
    }

    fn snapshot(&self) -> UpdateState {
        self.inner
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .clone()
    }

    /// Raise the cancel flag. True when a run was in flight — the caller's cue
    /// to say so; false when nothing was running.
    fn cancel(&self) -> bool {
        self.cancelled
            .swap(true, std::sync::atomic::Ordering::SeqCst)
    }

    /// Is the flag raised? The transfer loop polls this between body chunks.
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Clear the flag before a fresh transfer starts.
    fn reset_cancel(&self) {
        self.cancelled
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
}

/// `GET /repos/{REPO}/releases/latest` through one mirror prefix.
///
/// `prefix` is `""` for direct. The 404 pass-through matters: "the repo has no
/// release yet" is a real state a fresh project can be in, reported as data —
/// the panel says so and stops offering a check.
async fn fetch_release(http: &reqwest::Client, prefix: &str) -> Result<ReleaseInfo, String> {
    let url = match prefix {
        "" => format!("https://api.github.com/repos/{REPO}/releases/latest"),
        mirror => format!("{mirror}/https://api.github.com/repos/{REPO}/releases/latest"),
    };
    let response = http
        .get(&url)
        // GitHub's API requires a UA, and naming the version is what makes a
        // rate-limit line in someone's mirror log readable.
        .header("Accept", "application/vnd.github+json")
        // A few KiB of JSON: a total budget is right, and the client carries
        // none since the asset download moved its deadline to the read loop.
        .timeout(PER_MIRROR_TIMEOUT)
        .send()
        .await
        .map_err(|err| err.to_string())?;

    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err("no-releases".to_string());
    }
    let body = response.text().await.map_err(|err| err.to_string())?;
    if !status.is_success() {
        return Err(format!("HTTP {}", status.as_u16()));
    }
    parse_release(&body).ok_or_else(|| "release JSON unreadable".to_string())
}

/// The fields the app reads, and nothing else.
fn parse_release(raw: &str) -> Option<ReleaseInfo> {
    let value: serde_json::Value = serde_json::from_str(raw).ok()?;
    let tag = value.get("tag_name")?.as_str()?.to_string();
    let asset = value
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .and_then(|assets| {
            assets
                .iter()
                .filter_map(|entry| {
                    let name = entry.get("name")?.as_str()?.to_string();
                    // One asset whose name ends in `-setup.exe`: the installer
                    // package.ps1 built. Zips, SHA256 sidecars and source
                    // archives are not an install and are not offered as one.
                    if !name.ends_with("-setup.exe") {
                        return None;
                    }
                    Some(AssetInfo {
                        name,
                        size: entry
                            .get("size")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(0),
                        url: entry
                            .get("browser_download_url")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string)?,
                        digest: entry
                            .get("digest")
                            .and_then(serde_json::Value::as_str)
                            // `sha256:<hex>` on the wire; the bare hex is what compares.
                            .and_then(|d| d.strip_prefix("sha256:"))
                            .map(str::to_string),
                    })
                })
                .next()
        });
    Some(ReleaseInfo {
        version: tag.strip_prefix('v').unwrap_or(&tag).to_string(),
        tag,
        name: value
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        notes: value
            .get("body")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string(),
        url: value
            .get("html_url")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("/releases/latest")
            .to_string(),
        published_ms: value
            .get("published_at")
            .and_then(serde_json::Value::as_str)
            .and_then(parse_rfc3339_ms)
            .unwrap_or(0),
        asset,
    })
}

/// The one bit of RFC 3339 this needs: `2026-09-16T08:15:00Z` to epoch ms.
/// GitHub answers UTC with a `Z`; anything else parses as absent — the panel
/// shows nothing instead of a wrong date.
fn parse_rfc3339_ms(text: &str) -> Option<u64> {
    if !text.ends_with('Z') {
        return None;
    }
    let (date, time) = text.split_once('T')?;
    let mut date = date.split('-');
    let year: i64 = date.next()?.parse().ok()?;
    let month: i64 = date.next()?.parse().ok()?;
    let day: i64 = date.next()?.parse().ok()?;
    let mut time = time.trim_end_matches('Z').split(':');
    let hour: i64 = time.next()?.parse().ok()?;
    let minute: i64 = time.next()?.parse().ok()?;
    let second: f64 = time.next()?.parse().ok()?;
    // Days since the Unix epoch by the civil-calendar algorithm (Howard
    // Hinnant's), valid across any year this project will ship in.
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second.floor() as i64;
    let millis = (second.fract() * 1000.0) as i64;
    u64::try_from(secs * 1000 + millis).ok()
}

/// The dotted-number prefix of `text`, plus whether the tag is a clean release
/// rather than a pre-release: `1.8.0` → `([1,8,0], true)`,
/// `1.8.0-beta.2` → `([1,8,0], false)`, `latest` → `None`.
fn dotted(text: &str) -> Option<(Vec<u64>, bool)> {
    let mut numbers = Vec::new();
    let mut rest = text;
    loop {
        let (head, tail) = rest.split_once('.').unwrap_or((rest, ""));
        let head = match head.split(['-', '+']).next() {
            Some(h) if !h.is_empty() => h,
            _ => break,
        };
        numbers.push(head.parse().ok()?);
        rest = tail;
        if rest.is_empty() || rest.starts_with('-') || rest.starts_with('+') {
            break;
        }
    }
    if numbers.is_empty() {
        return None;
    }
    Some((numbers, rest.is_empty()))
}

/// Strictly-greater on the dotted-number prefixes; a pre-release is older than
/// the same number released, semver's own rule. Missing components count as
/// zero, so `1.8` < `1.8.1`.
fn is_newer(candidate: &str, current: &str) -> bool {
    let Some((c, c_clean)) = dotted(candidate) else {
        return false;
    };
    let Some((m, m_clean)) = dotted(current) else {
        return false;
    };
    for i in 0..c.len().max(m.len()) {
        let a = c.get(i).copied().unwrap_or(0);
        let b = m.get(i).copied().unwrap_or(0);
        if a != b {
            return a > b;
        }
    }
    c_clean && !m_clean
}

/// `VC_UPDATE_PROXY` into a reqwest proxy, or `None` for default routing. An
/// unparseable value is reported rather than silently ignored: the user who set
/// it is debugging their network and deserves the truth.
fn explicit_proxy() -> Result<Option<reqwest::Proxy>, String> {
    let value = match std::env::var("VC_UPDATE_PROXY") {
        Ok(v) if !v.trim().is_empty() => v.trim().to_string(),
        _ => return Ok(None),
    };
    let proxy = reqwest::Proxy::all(&value)
        .map_err(|err| format!("VC_UPDATE_PROXY 不可用：{err}"))?;
    Ok(Some(proxy))
}

/// The client for outbound traffic. The shared `Host::http` is pinned to
/// loopback with a 3 s ceiling — right for status polls, wrong for GitHub — so
/// the updater builds its own and lets it drop.
///
/// System proxying is reqwest's default behaviour; nothing names it because
/// naming it would *disable* it. The `system-proxy` feature is what makes an
/// already-running Clash count without a word typed anywhere.
fn outbound_client() -> Result<reqwest::Client, String> {
    let builder = reqwest::Client::builder()
        // Handshake budget. No client-level `.timeout()`: in reqwest that is a
        // *total* budget — connect through the last body byte — and a 45 MB
        // asset on a healthy link outlives it, so every transfer got cut at
        // 20 s and "resumed" on the next mirror in 20 s windows. Each caller
        // owns its own deadline instead: small fetches set a per-request
        // total, the asset stream is bounded per-read in [`try_candidate`].
        .connect_timeout(PER_MIRROR_TIMEOUT)
        .user_agent(concat!("voice-core/", env!("CARGO_PKG_VERSION")));
    let builder = match explicit_proxy()? {
        Some(proxy) => builder.proxy(proxy),
        None => builder,
    };
    builder.build().map_err(|err| err.to_string())
}

/// One GitHub URL as the candidate list: direct first, then every mirror in
/// [`MIRRORS`] order. Wrapping is the whole proxy scheme:
/// `<mirror>/<original-url>`.
fn candidates(url: &str) -> Vec<(String, String)> {
    let wrap = |prefix: &str| {
        if prefix.is_empty() {
            url.to_string()
        } else {
            format!("{prefix}/{url}")
        }
    };
    // Note the leading direct entry: mirrors are a fallback, never a preference.
    MIRRORS
        .iter()
        .map(|(name, prefix)| (name.to_string(), wrap(prefix)))
        .collect()
}

/// `GET /releases/latest`. Direct only — measured on 2026-09-17, the public
/// prefix proxies (ghproxy.net, ghfast.top) refuse the API endpoint with 403
/// while happily proxying release assets, so a mirror list here would be a
/// list of known failures. A machine that cannot reach api.github.com at all
/// is rare and gets an honest error.
async fn fetch_release_any() -> Result<ReleaseInfo, String> {
    let client = outbound_client()?;
    fetch_release(&client, "").await
}

/// The last-checked stamp, `<data dir>\update\last-check.txt`: one epoch-ms
/// line. A file, not registry state, so a portable tree carries its own check
/// history like every other fact it owns.
fn last_check_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    layout::update_dir(data_dir).join("last-check.txt")
}

/// How long after an update-relaunched boot a failed interpreter probe is
/// treated as a suspect sample rather than a verdict. The installer's [Run]
/// brings the panel back while the machine is still settling: the antivirus
/// scans every file Setup just wrote, and the measured 1.9.8 install failed
/// three probes with `entity not found` over twenty seconds against a venv the
/// installer never touched. Sixty seconds of re-probing costs a few torch
/// imports; reporting 需重建 for a healthy install costs the update's whole
/// contract.
pub const PROBE_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

/// The restart marker, `<data dir>\update\restart.json`: proof that the next
/// panel to come up is the one an installer's [Run] relaunch brought back, not a
/// fresh boot by a user who never asked the stack to run.
///
/// Written by [`update_install`] after the stack is stopped, consumed and
/// deleted by [`crate::resume_after_update`] at boot. A file, not memory, because
/// the process that set the intent is the process the installer kills — the only
/// thing that survives the update is the data directory.
fn restart_marker_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    layout::update_dir(data_dir).join("restart.json")
}

/// The hand-off file, `<data dir>\update\resume.json`. Written by the installer-
/// born panel when it relaunches itself outside the installer's process tree;
/// read by [`wait_for_previous_panel`] in `main()` (before the single-instance
/// plugin claims the mutex) and consumed by [`resume_after_update`] on that
/// second boot.
fn resume_marker_path(data_dir: &std::path::Path) -> std::path::PathBuf {
    layout::update_dir(data_dir).join("resume.json")
}

/// Block until `pid` exits, polling every 200 ms up to `limit`.
fn wait_for_process(pid: u32, limit: std::time::Duration) {
    let deadline = std::time::Instant::now() + limit;
    while process_alive(pid) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// Before the builder: a panel relaunched through [`resume_marker_path`]'s
/// hand-off waits for the process it is replacing to die, so the single-instance
/// mutex is free when this instance claims it. An `exitPid` of 0 means the
/// hand-off was pre-consumed by `update_install` — there is no predecessor to
/// wait for (pid 0 is the System process and never exits). Everything else
/// starts instantly.
pub fn wait_for_previous_panel(data_dir: &std::path::Path) {
    let Ok(raw) = std::fs::read_to_string(resume_marker_path(data_dir)) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return;
    };
    let pid = value.get("exitPid").and_then(serde_json::Value::as_u64);
    match pid {
        Some(0) | None => return,
        Some(pid) => wait_for_process(pid as u32, std::time::Duration::from_secs(15)),
    }
}

/// Consume the marker and decide this boot's relationship to the installer.
///
/// Measured on the reference machine (the 1.9.9 update): a panel born of the
/// installer's [Run] chain spawns broken children for its whole lifetime — the
/// interpreter probe's child either failed instantly with `uv trampoline:
/// entity not found` or, with a bare interpreter in its place, hung for the
/// probe's entire 90-second deadline without ever reaching `sitecustomize`.
/// The same executable launched normally probes fine in seconds. The poison
/// follows the launch chain — environment, handles, console, job, nothing in
/// the dumped process state explains it — so the answer is not to diagnose it
/// per-child but to refuse to live in it.
///
/// Two marker generations, one decision each:
///
/// * `restart.json` — written by [`update_install`]. This boot came up through
///   the installer's [Run]: it relaunches itself through Explorer and exits
///   before its first paint.
/// * `resume.json` — written by that relaunching panel. This boot came up
///   through Explorer: clean, it stays and resumes the stack the flags ask for.
pub fn resume_after_update(app: tauri::AppHandle) {
    let host = app.state::<Host>();
    let data_dir = host.data_dir.clone();
    let installer_marker = restart_marker_path(&data_dir);
    let handoff = resume_marker_path(&data_dir);

    // The hand-off takes precedence: it was written either by a relaunching
    // panel (exitPid = its pid, the successor must wait) or pre-consumed by
    // `update_install` (exitPid = 0, the updater's cmd chain drives the
    // relaunch itself and the installer's [Run] skipped itself). The restart
    // marker is consumed alongside the hand-off either way: leaving it would
    // make the next boot believe an installer just ran.
    let (marker, from_installer) = if handoff.is_file() {
        (&handoff, false)
    } else {
        (&installer_marker, true)
    };
    let Ok(raw) = std::fs::read_to_string(marker) else {
        return;
    };
    // One launch consumes one marker, however this panel ends: the restart it
    // asks for happens exactly once, and a crash loop that re-reads a stale
    // marker every boot is exactly what deleting it prevents.
    let _ = std::fs::remove_file(marker);
    let _ = std::fs::remove_file(&installer_marker);
    let _ = std::fs::remove_file(&handoff);
    // An installer-born panel is probe-suspect for a while (see `updated_at`);
    // a clean Explorer child never needs the grace window.
    if from_installer {
        *host
            .updated_at
            .lock()
            .unwrap_or_else(|err| err.into_inner()) = Some(std::time::Instant::now());
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return;
    };
    let wants = value
        .get("restartStack")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let was_running = value
        .get("wasRunning")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let target_version = value
        .get("targetVersion")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string);

    // An install that promised a version but did not deliver it. The marker
    // survived, so the installer ran — but this boot is not the target version,
    // which means it died mid-run (power loss, a taskkill racing the swap) or
    // the [Run] relaunch never happened. The staged file is usually still in
    // data\update; surface that as a retryable row instead of silence.
    if let Some(target) = &target_version {
        let running_version = env!("CARGO_PKG_VERSION");
        if target != running_version {
            host.log(&format!(
                "update: install did not complete — this panel is {running_version}, the install promised {target}; the staged package stays in data\\update for a retry"
            ));
            // Keep whatever installer is staged (the same file the failed run
            // verified) visible to the update row: restoring `staged` lets
            // 立即安装 offer a retry without re-downloading.
            let data_dir = host.data_dir.clone();
            let dir = layout::update_dir(&data_dir);
            if let Ok(entries) = std::fs::read_dir(&dir) {
                // The newest .exe in data\update is the one the failed run
                // verified; the sweep in run_download keeps it to one.
                let newest = entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.is_file()
                            && p.extension()
                                .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
                    })
                    .max_by_key(|p| {
                        p.metadata()
                            .and_then(|m| m.modified())
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map(|d| d.as_millis())
                            .unwrap_or(0)
                    });
                if let Some(candidate) = newest {
                    host.update.set(|state| {
                        state.staged = Some(candidate.display().to_string());
                        state.progress.done = true;
                        state.progress.active = false;
                        state.progress.failed = true;
                        state.progress.error = format!(
                            "上一次更新未完成（目标版本 {target}），安装包已就绪，可重试"
                        );
                    });
                }
            }
        }
    }

    if !from_installer {
        // A clean Explorer child: the poison is gone, so this panel simply lives.
        host.log("update: resumed as the clean relaunch; the installer environment is gone");
        resume_in_process(&app, wants && was_running);
        return;
    }

    // Carry the user's intent across the relaunch. The successor reads the pid
    // before its single-instance claim and the flags at setup.
    let _ = std::fs::write(
        &handoff,
        serde_json::json!({
            "restartStack": wants,
            "wasRunning": was_running,
            "exitPid": std::process::id(),
        })
        .to_string(),
    );

    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            host.log(&format!(
                "update: cannot resolve own executable for the clean relaunch: {err}; staying up in the installer environment"
            ));
            let _ = std::fs::remove_file(&handoff);
            resume_in_process(&app, wants && was_running);
            return;
        }
    };
    // Explorer performs the launch as its own child: a fresh environment, fresh
    // handle table, no installer anywhere in the ancestry. `explorer <path>` uses
    // the shell's file association, which for an .exe is ShellExecute — exactly
    // what the Start-menu shortcut does, and measured clean every time.
    match std::process::Command::new("explorer.exe").arg(&exe).spawn() {
        Ok(_) => {
            host.log("update: relaunching the panel outside the installer environment; this instance is exiting");
            std::process::exit(0);
        }
        Err(err) => {
            host.log(&format!(
                "update: clean relaunch failed ({err}); staying up in the installer environment"
            ));
            let _ = std::fs::remove_file(&handoff);
            resume_in_process(&app, wants && was_running);
        }
    }
}

/// The in-process fallback: resume the stack here if no clean relaunch happens.
fn resume_in_process(app: &tauri::AppHandle, should_resume: bool) {
    if !should_resume {
        app.state::<Host>()
            .log("update: restart marker present but the stack was not running before the update; leaving the stack down");
        return;
    }
    let app = app.clone();
    app.state::<Host>()
        .log("update: relaunching the stack this installer's [Run] interrupted");
    tauri::async_runtime::spawn(async move {
        if let Err(err) = crate::supervise::start(&app).await {
            app.state::<Host>()
                .log(&format!("update: auto-restart failed: {err}"));
        }
    });
}

fn read_last_check(data_dir: &std::path::Path) -> u64 {
    std::fs::read_to_string(last_check_path(data_dir))
        .ok()
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

fn write_last_check(data_dir: &std::path::Path, ms: u64) {
    let _ = std::fs::create_dir_all(layout::update_dir(data_dir));
    let _ = std::fs::write(last_check_path(data_dir), ms.to_string());
}

/// One sweep: a check, the timestamp recorded. Failures are silent here — this
/// runs at boot and every six hours without a person watching, and a toast
/// would punish an offline machine four times an hour. The panel's own check
/// is the loud path.
async fn sweep(app: &tauri::AppHandle) {
    let data_dir = app.state::<Host>().data_dir.clone();
    if update_check().await.is_ok() {
        write_last_check(&data_dir, now_ms());
    }
}

/// Boot and heartbeat: check once shortly after startup, then every six hours.
/// Nothing here talks to the UI, so a check while the panel is closed costs
/// nothing and the next visit sees the fresh answer.
pub fn start(app: tauri::AppHandle) {
    tauri::async_runtime::spawn(async move {
        // Give the machine a minute to settle its network before the first
        // outbound call; a check that starts before Wi-Fi is up just fails.
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        loop {
            sweep(&app).await;
            tokio::time::sleep(std::time::Duration::from_secs(6 * 3600)).await;
        }
    });
}

/// Is there a newer release? Answers with both ends of the comparison and the
/// release itself, newer or not. The 404 case is this project's real "nothing
/// published yet", phrased as its own sentence rather than as a failure.
#[tauri::command]
pub async fn update_check() -> Result<CheckOutcome, String> {
    let current = env!("CARGO_PKG_VERSION").to_string();
    let release = fetch_release_any().await.map_err(|err| {
        if err == "no-releases" {
            "GitHub 上还没有任何 release".to_string()
        } else {
            format!("检查失败 — {err}")
        }
    })?;
    Ok(CheckOutcome {
        update_available: is_newer(&release.version, &current),
        current,
        release,
    })
}

/// The timestamp a check last answered, for the idle row's last-checked fact.
/// 0 means this install has never checked — its own fact, shown as such.
#[tauri::command]
pub async fn update_last_check(app: tauri::AppHandle) -> u64 {
    read_last_check(&app.state::<Host>().data_dir)
}

/// Where the download is. Never fails: "nothing has happened yet" is the
/// default state, and a poll racing a finish deserves the last state, not an
/// error.
#[tauri::command]
pub async fn update_status(app: tauri::AppHandle) -> UpdateState {
    app.state::<Host>().update.snapshot()
}

/// Download the release's installer into `data\update\`, verify it, stage it.
///
/// Spawns the transfer and returns at once; progress lands in [`Host::update`]
/// for [`update_status`] to poll. A run in progress refuses a second one — one
/// slot, like provision and training, and for the same reasons: two writers to
/// one file, two progress bars for one download.
#[tauri::command]
pub async fn update_download(app: tauri::AppHandle, asset: AssetInfo) -> Result<(), String> {
    let staged_path = {
        let host = app.state::<Host>();
        if host.update.snapshot().progress.active {
            return Err("已经在下载了".to_string());
        }
        let dir = layout::update_dir(&host.data_dir);
        std::fs::create_dir_all(&dir).map_err(|err| format!("无法创建下载目录：{err}"))?;
        dir.join(&asset.name)
    };

    tauri::async_runtime::spawn(async move {
        run_download(app, staged_path, asset).await;
    });
    Ok(())
}

/// Stop the running download. The partial stays on disk as a resume point, so
/// the next 下载 continues from the bytes already there instead of starting
/// over. Returns at once — the transfer loop notices the flag between chunks
/// (at most one idle-timeout window later) and reports the terminal state
/// itself; this command does not own the slot.
#[tauri::command]
pub async fn update_cancel(app: tauri::AppHandle) -> Result<bool, String> {
    let host = app.state::<Host>();
    let active = host.update.snapshot().progress.active;
    Ok(host.update.cancel() && active)
}

/// The transfer: candidates in order, the first one that yields a verified file
/// wins. Every failure recorded and the next tried; the last error is what the
/// panel shows when the list runs out.
async fn run_download(app: tauri::AppHandle, staged_path: PathBuf, asset: AssetInfo) {
    let host = app.state::<Host>();
    host.update.reset_cancel();
    host.update.set(|state| {
        state.progress = DownloadProgress {
            active: true,
            total: asset.size,
            ..DownloadProgress::default()
        };
        // A re-download over an earlier failed one starts clean; a staged path
        // from a finished run is not reachable here (the button refuses), but
        // clearing it costs nothing and keeps the invariant "staged ⇒ done".
        state.staged = None;
    });
    host.log(&format!("update: downloading {} ({} bytes)", asset.name, asset.size));

    let client = match outbound_client() {
        Ok(c) => c,
        Err(err) => return finish_failed(&host, err),
    };

    let mut last_error = String::from("没有可用镜像");
    for (name, url) in candidates(&asset.url) {
        match try_candidate(&client, &host, &staged_path, &asset, &url, &name).await {
            Ok(bytes) => {
                // Resume artifacts are the transfer's scratch, not its product:
                // the verified file is `staged_path`, the sidecar's job is done.
                let _ = std::fs::remove_file(part_path(&staged_path));
                host.update.set(|state| {
                    state.progress.downloaded = bytes;
                    state.progress.total = bytes;
                    state.progress.active = false;
                    state.progress.done = true;
                    state.staged = Some(staged_path.display().to_string());
                });
                // Superseded packages have no second life: the winner is verified,
                // everything else in data\update is a stale download. Sweep them.
                if let Some(dir) = staged_path.parent() {
                    if let Ok(entries) = std::fs::read_dir(dir) {
                        for entry in entries.flatten() {
                            let path = entry.path();
                            if path != staged_path
                                && path.is_file()
                                && path
                                    .extension()
                                    .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
                            {
                                let _ = std::fs::remove_file(&path);
                                host.log(&format!("update: removed superseded {}", path.display()));
                            }
                        }
                    }
                }
                host.log(&format!("update: staged {}", staged_path.display()));
                return;
            }
            Err(err) => {
                // A resume-capable failure keeps the partial for the next
                // candidate to continue from; only the final failure wipes it,
                // because the panel's retry button comes back here. A user
                // cancel is resume-shaped by the same logic: the bytes on disk
                // are a prefix of the asset, and a retry continues from them.
                last_error = err;
                if host.update.is_cancelled() {
                    break;
                }
            }
        }
    }
    if host.update.is_cancelled() {
        // The part file stays — it is exactly the resume point a later 下载
        // continues from. `cancelled` is a third terminal state beside done and
        // failed: the panel shows 重试/继续 without ever calling it an error.
        let _ = std::fs::remove_file(part_path(&staged_path).with_extension("exe.part.meta"));
        host.update.set(|state| {
            state.progress.active = false;
            state.progress.failed = false;
            state.progress.error = "已取消下载，断点已保留".to_string();
            state.progress.cancelled = true;
        });
        host.log("update: download cancelled by user; partial kept");
        return;
    }
    let _ = std::fs::remove_file(part_path(&staged_path));
    finish_failed(&host, last_error);
}

/// The partial download, written beside its target. The sidecar records which
/// asset the bytes belong to, so a partial of 1.9.9 can never resume into a
/// staged 1.9.10.
fn part_path(staged_path: &Path) -> PathBuf {
    staged_path.with_extension("exe.part")
}

/// The partial's provenance: asset name, size, and the digest promised. A part
/// file that does not describe the asset being downloaded is deleted, not used.
#[derive(Serialize, Deserialize)]
struct PartMeta {
    name: String,
    size: u64,
    digest: Option<String>,
}

fn read_partial(staged_path: &Path, asset: &AssetInfo) -> u64 {
    let part = part_path(staged_path);
    let Ok(meta_raw) = std::fs::read_to_string(part.with_extension("exe.part.meta")) else {
        return 0;
    };
    let Ok(meta) = serde_json::from_str::<PartMeta>(&meta_raw) else {
        return 0;
    };
    let usable = meta.name == asset.name
        && meta.size == asset.size
        && meta.digest == asset.digest;
    if !usable {
        let _ = std::fs::remove_file(&part);
        return 0;
    }
    std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0)
}

fn write_partial_meta(staged_path: &Path, asset: &AssetInfo) {
    let meta = PartMeta {
        name: asset.name.clone(),
        size: asset.size,
        digest: asset.digest.clone(),
    };
    if let Ok(json) = serde_json::to_string(&meta) {
        let _ = std::fs::write(part_path(staged_path).with_extension("exe.part.meta"), json);
    }
}

/// One mirror, start to finish: response headers, body streamed to disk while
/// hashed, size then digest checked. A failed candidate leaves its partial on
/// disk for the next candidate (or the next run) to resume from — that is the
/// point of the `.part` file; the caller moves it to `staged_path` only after
/// the whole file verified.
async fn try_candidate(
    client: &reqwest::Client,
    host: &Host,
    staged_path: &Path,
    asset: &AssetInfo,
    url: &str,
    name: &str,
) -> Result<u64, String> {
    let part = part_path(staged_path);

    // Bytes already on disk from an earlier attempt of the *same* asset. The
    // sidecar meta makes a stale partial impossible: name, size and digest must
    // all match, or the part is deleted and the transfer starts over.
    let resume_from = read_partial(staged_path, asset);
    if resume_from > 0 {
        host.log(&format!(
            "update: resuming {name} from {resume_from} bytes"
        ));
    }
    write_partial_meta(staged_path, asset);

    // No total-transfer timeout here: `timeout()` on the request would budget
    // the whole body, and a 45 MB asset on a slow-but-alive link is healthy.
    // The connect timeout bounds the handshake; the per-read idle timeout at
    // the loop below bounds a stalled connection.
    let mut request = client.get(url);
    if resume_from > 0 && asset.size != 0 && resume_from < asset.size {
        request = request.header("Range", format!("bytes={resume_from}-"));
    }
    let response = request.send().await.map_err(|err| format!("{name}: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("{name}: HTTP {}", response.status().as_u16()));
    }

    // 206 Partial Content is the resumed transfer; a mirror that answers 200 to
    // a Range request is restarting from zero, and its body is taken as such.
    let resumed = response.status() == reqwest::StatusCode::PARTIAL_CONTENT && resume_from > 0;
    let start = if resumed { resume_from } else { 0 };
    if !resumed && resume_from > 0 {
        // The mirror ignored the Range header; the partial's bytes are not a
        // prefix of this body, so discard and write from zero.
        let _ = tokio::fs::remove_file(&part).await;
    }

    // A 1 MiB buffered writer: reqwest hands the stream over in small chunks,
    // and an unbuffered write per chunk makes every 64 KiB a syscall — the
    // difference between saturating a gigabit link and crawling at a tenth of it.
    // Resume appends to the part file; a fresh transfer truncates it.
    let raw = if resumed {
        tokio::fs::OpenOptions::new()
            .append(true)
            .open(&part)
            .await
            .map_err(|err| format!("无法写入 {}：{err}", part.display()))?
    } else {
        tokio::fs::File::create(&part)
            .await
            .map_err(|err| format!("无法写入 {}：{err}", part.display()))?
    };
    let mut file = tokio::io::BufWriter::with_capacity(1024 * 1024, raw);
    // The hash starts over with every attempt: the part file's bytes are hashed
    // by re-reading them, then the body continues into the same hasher.
    let mut hasher = Sha256::new();
    let mut downloaded: u64 = start;
    {
        // Re-hash the resumed prefix. 45 MB of SHA-256 is well under a second,
        // and it keeps the integrity model identical between fresh and resumed
        // transfers: the final check always covers every byte on disk.
        if start > 0 {
            let prefix = tokio::fs::read(&part)
                .await
                .map_err(|err| format!("无法读取断点文件：{err}"))?;
            hasher.update(&prefix);
        }
        let mut last_tick = now_ms();
        let mut stream = response;
        loop {
            // A user cancel outranks everything the loop is doing: the check
            // sits before the chunk read, so a click during the 30 s idle
            // window stops within it rather than after the timeout.
            if host.update.is_cancelled() {
                let _ = tokio::io::AsyncWriteExt::flush(&mut file).await;
                return Err("已取消".to_string());
            }
            // `chunk` rather than an `AsyncRead` wrapper: reqwest's response is
            // its own stream type, and this is the API that needs no adapter. The
            // timeout is per read, not per download — a slow link is healthy, a
            // stalled one is cut.
            let bytes = match tokio::time::timeout(DOWNLOAD_IDLE_TIMEOUT, stream.chunk()).await {
                Ok(Ok(Some(bytes))) => bytes,
                Ok(Ok(None)) => break,
                Ok(Err(err)) => return Err(format!("{name}: {err}")),
                Err(_) => return Err(format!("{name}: 连接停滞超过 30 秒")),
            };
            hasher.update(&bytes);
            tokio::io::AsyncWriteExt::write_all(&mut file, &bytes)
                .await
                .map_err(|err| format!("写入失败：{err}"))?;
            downloaded += bytes.len() as u64;
            if now_ms() - last_tick >= 250 {
                last_tick = now_ms();
                host.update.set(|state| state.progress.downloaded = downloaded);
            }
        }
        tokio::io::AsyncWriteExt::flush(&mut file)
            .await
            .map_err(|err| format!("写入失败：{err}"))?;
    }

    // A mirror that truncates at 90% but answers 200 must not stage a partial
    // installer: the API said how big the file is.
    if asset.size != 0 && downloaded != asset.size {
        // The part file stays for the next attempt; this is a resume point, not
        // a failure to clean.
        return Err(format!(
            "{name}: 收到 {downloaded} / {} 字节，下载被截断",
            asset.size
        ));
    }
    // Complete. Move the part onto the staged path; everything after this point
    // (size was checked, digest below) treats it as the real file.
    tokio::fs::rename(&part, staged_path)
        .await
        .map_err(|err| format!("无法落盘 {}：{err}", staged_path.display()))?;

    // Integrity. This is the only thing standing between an unsigned installer
    // and a tampered one. GitHub issues a digest for every release asset, so a
    // missing one is not a pass: the official updater plugins treat "no
    // signature" as a refusal, and a hash we do not have is a hash we cannot
    // check — a truncated mirror must never reach the installer.
    let expected = asset.digest.as_ref().ok_or_else(|| {
        host.log(&format!("update: no digest published for {name}; refusing to install"));
        format!("{name}: release 未提供 SHA256，拒绝安装")
    })?;
    {
        let actual = format!("{:x}", hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected) {
            host.log(&format!("update: digest mismatch from {name}"));
            return Err(format!("{name}: SHA256 不匹配，已丢弃"));
        }
    }
    // The signature turns the trust model from "GitHub said so" into "only the
    // release key could have said so": a mirror that serves a tampered file
    // cannot forge a signature, no matter what metadata it fakes. The private
    // key never leaves the release machine; this public half proves the bytes.
    // Fetched from the bare GitHub URL (never a mirror): the mirror could as
    // easily serve a matching fake signature alongside a tampered file.
    verify_signature(client, host, staged_path, asset, name).await?;
    Ok(downloaded)
}

/// The minisign public key that signs every release. Generated once with
/// `vc-sign gen` (scripts/sign); the private half lives in the release
/// machine's key file and nowhere else. Verification here is what makes a
/// tampered mirror fail closed.
const RELEASE_PUBLIC_KEY: &str =
    "RWRacsM1emTEf64kTeIiXhOctDb/qVOooRW8SMyKWyuvBrMi97y/o1z5";

/// minisign-verify against the embedded public key. The signature is fetched
/// from the release as `<asset name>.sig` and verified in full-file mode: the
/// whole installer is in memory anyway (it was just hashed), so no streaming
/// variant is needed.
async fn verify_signature(
    client: &reqwest::Client,
    host: &Host,
    staged_path: &Path,
    asset: &AssetInfo,
    name: &str,
) -> Result<(), String> {
    use minisign_verify::{PublicKey, Signature};
    let sig_url = format!("{}.sig", asset.url);
    let sig_text = client
        .get(&sig_url)
        // A minisign signature is ~300 bytes; a total budget is right.
        .timeout(PER_MIRROR_TIMEOUT)
        .send()
        .await
        .map_err(|err| format!("无法取回签名: {err}"))?
        .error_for_status()
        .map_err(|err| format!("无法取回签名: {err}"))?
        .text()
        .await
        .map_err(|err| format!("无法读取签名: {err}"))?;
    let pk = PublicKey::from_base64(RELEASE_PUBLIC_KEY)
        .map_err(|err| format!("内置公钥无效: {err}"))?;
    let signature = Signature::decode(&sig_text).map_err(|err| format!("签名文件无效: {err}"))?;
    let bytes = tokio::fs::read(staged_path)
        .await
        .map_err(|err| format!("无法读取安装包: {err}"))?;
    pk.verify(&bytes, &signature, false)
        .map_err(|err| format!("签名与文件不匹配: {err}"))?;
    host.log(&format!("update: signature verified for {name}"));
    Ok(())
}

fn finish_failed(host: &Host, message: String) {
    host.log(&format!("update: failed: {message}"));
    host.update.set(|state| {
        state.progress.active = false;
        state.progress.failed = true;
        state.progress.error = message;
    });
}

/// Is the backend API answering right now? Read before the stack stops, so the
/// restart marker records what the user actually had, not what this command
/// just tore down.
async fn probe_running(app: &tauri::AppHandle) -> bool {
    let host = app.state::<Host>();
    let url = format!("{}/api/health", host.base_url);
    matches!(host.http.get(url).send().await, Ok(response) if response.status().is_success())
}

/// Hand the staged installer to Windows and get out of the way.
///
/// `cmd /c start` exists so the installer is not this process's child: the panel
/// exiting mid-install (the installer closes it via AppMutex) must not take the
/// installer down with it. `/RESTARTAPPLICATIONS` is what brings the panel back
/// — the installer's own [Run] entry, not this command, so nothing here relaunches
/// anything twice.
///
/// The staged state is cleared only after a successful spawn: a failed launch
/// leaves the verified file and its state, so 再试一次 does not re-download what
/// is already on disk.
#[tauri::command]
pub async fn update_install(app: tauri::AppHandle) -> Result<(), String> {
    let host = app.state::<Host>();
    let staged = {
        let state = host.update.snapshot();
        match state.staged {
            Some(path) if state.progress.done => path,
            _ => return Err("还没有就绪的更新包".to_string()),
        }
    };
    if !Path::new(&staged).is_file() {
        return Err("更新包文件已不在：data\\update\\ 下的文件被移动或删除了".to_string());
    }

    // Stack down first, then this panel. The installer's AppMutex check runs
    // before anything else and counts the panel itself: leave VoiceCore.exe
    // alive and a suppressed message box answers Cancel for the user (measured:
    // exit 1, 'Setup has detected that voice-core is currently running'). The
    // runtime goes through the supervisor; the panel schedules its own exit —
    // after this command returns, so the IPC response is delivered first.
    //
    // The marker is written only after the stop answers, so `wasRunning` means
    // "the stack was genuinely up when the user pressed 安装" and not "was
    // running until we stopped it a millisecond ago". A panel that comes back
    // from this install restores the stack; a fresh boot after an update does
    // not — the same rule a plain reboot follows.
    let was_running = {
        let snapshot = probe_running(&app).await;
        let _ = crate::supervise::stop_stack(app.clone()).await;
        snapshot
    };
    let _ = std::fs::create_dir_all(layout::update_dir(&host.data_dir));
    let _ = std::fs::write(
        restart_marker_path(&host.data_dir),
        serde_json::json!({
            "restartStack": true,
            "wasRunning": was_running,
            // The version this install promised. A boot that consumes the marker
            // without reaching it means the installer died mid-run (power loss,
            // a taskkill racing the swap): the failure becomes a visible,
            // retryable state instead of a silent no-op.
            "targetVersion": env!("CARGO_PKG_VERSION"),
        })
        .to_string(),
    );
    // Pre-consumed hand-off: the installer's [Run] entry checks this file and
    // skips itself (the updater's own chain drives the relaunch), so the panel
    // an update brings up boots exactly once — through the clean Explorer
    // relay — instead of the open-close-open flash the old flow showed.
    let _ = std::fs::write(
        resume_marker_path(&host.data_dir),
        serde_json::json!({
            "restartStack": true,
            "wasRunning": was_running,
            "exitPid": 0,
            "targetVersion": env!("CARGO_PKG_VERSION"),
        })
        .to_string(),
    );

    // Drop the interpreter probe. The installer is about to replace the venv's
    // files, and a detect() that runs inside that window (the panel's own
    // bootstrap -CheckOnly did exactly this, measured) races the extract, fails
    // with 'uv trampoline failed to spawn', and the failure would sit in the
    // process-lifetime cache as 需重建 for an environment that is fine.
    *host.probe.lock().unwrap_or_else(|err| err.into_inner()) = None;

    // The chain owns the whole transition: kill this panel, run the installer to
    // completion (`start /wait`), then hand the launch to Explorer — whose child
    // is the clean-relaunched panel, the one and only window this update shows.
    // The installer's [Run] skips itself (it sees resume.json), so nothing else
    // opens a window in between.
    let panel_exe = host.root.join("VoiceCore.exe");
    let mut command = std::process::Command::new("cmd");
    command
        .arg("/C")
        .arg("timeout")
        .arg("/t")
        .arg("2")
        .arg("/nobreak")
        .arg(">nul")
        .arg("&")
        .arg("taskkill")
        .arg("/F")
        .arg("/IM")
        .arg("VoiceCore.exe")
        .arg("&")
        .arg("start")
        .arg("")
        .arg("/wait")
        .arg(&staged)
        .arg("/VERYSILENT")
        .arg("/SUPPRESSMSGBOXES")
        .arg("/NORESTART")
        .arg("/RESTARTAPPLICATIONS")
        .arg("&")
        .arg("start")
        .arg("")
        .arg("explorer.exe")
        .arg(&panel_exe);
    hidden(&mut command);
    match command.spawn() {
        // Not forgotten this time — the PID is the liveness probe below.
        Ok(child) => {
            let installer_pid = child.id();
            host.log(&format!(
                "update: installer launched from {staged} (pid {installer_pid})"
            ));
            host.update.set(|state| {
                state.launched = Some(staged.clone());
                state.progress = DownloadProgress::default();
                state.staged = Some(staged.clone());
            });

            // The spinner is a promise the installer may not keep: SmartScreen
            // refusal, a cancelled UAC, a corrupt download — all leave this
            // panel spinning forever. Watch the installer: while it lives, an
            // install is in progress; the moment it exits AND this panel is
            // still the same process (a successful install restarted us under
            // a newer version), the run failed — hand the staged file back to
            // the row as a retryable install instead of an eternal spinner.
            let watcher_app = app.clone();
            let watcher_staged = staged.clone();
            tauri::async_runtime::spawn(async move {
                // The panel taskkills itself ~2 s in, so this loop usually dies
                // with the process — it only ever finishes when the panel
                // outlived the installer, which means the installer bailed
                // (SmartScreen, a refused elevation) and someone is still
                // looking at a spinner. Restoring the staged file is for that
                // survivor; a successful install simply never reaches it.
                let pid = installer_pid;
                let mut alive = true;
                for _ in 0..300 {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    alive = process_alive(pid);
                    if !alive {
                        break;
                    }
                }
                let app = watcher_app;
                if !alive {
                    let host = app.state::<Host>();
                    host.log("update: installer exited without closing the panel; restoring the install button");
                    host.update.set(|state| {
                        state.launched = None;
                        state.staged = Some(watcher_staged.clone());
                        state.progress.done = true;
                    });
                }
            });
            Ok(())
        }
        Err(err) => Err(format!("无法启动安装程序：{err}")),
    }
}

/// Is this PID still running? An OpenProcess probe, not a handle hold — the
/// caller deliberately does not keep the Child, and a leaked handle would keep
/// the process alive as far as the OS cares.
fn process_alive(pid: u32) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, GetExitCodeProcess,
        };
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == STILL_ACTIVE as u32
        }
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        false
    }
}

/// Open the release page in the user's browser. Detached for the same reason as
/// the installer: a browser is nobody's child process to outlive.
#[tauri::command]
pub async fn open_url(app: tauri::AppHandle, url: String) -> Result<(), String> {
    // Only https URLs leave this process. The updater panel is exactly the
    // place where an injected `file://` or a custom scheme would be interesting.
    if !url.starts_with("https://") {
        return Err("只允许打开 https 链接".to_string());
    }
    let mut command = std::process::Command::new("cmd");
    command.arg("/C").arg("start").arg("").arg(url);
    hidden(&mut command);
    let _ = app.state::<Host>();
    command.spawn().map_err(|err| format!("无法打开浏览器：{err}"))?;
    Ok(())
}

#[cfg(test)]
mod release_signature_tests {
    use super::*;

    /// The signing contract, pinned with a real pair: this signature was made
    /// by `vc-sign sign` over exactly `PAYLOAD` (see scripts/sign); it must
    /// verify against the embedded public key, and one flipped byte must not.
    /// The secret half never enters this repository — the signature is the
    /// artifact, which is the whole trust model.
    #[test]
    fn release_signature_round_trip() {
        use minisign_verify::{PublicKey, Signature};
        const PAYLOAD: &[u8] = b"voice-core release signature round trip";
        const SIGNATURE: &str = "untrusted comment: signature from rsign secret key
RURacsM1emTEfyGgoQrCjVEzDSLF02JaPumNqOgw7ajyN10Wc/X9H3FpmGA3QLHNZRPEmTMxri5GWBSPfSHjUkma1V1UmbLM/QQ=
trusted comment: timestamp:1789608442
M+u2lUpXtew6obBX1ki/vkLWYf+Fjy3XL9QEpaVmS1RzzL85Es6GiRMTDpzPjnZYwui4Vjly704z8CXuUJf6Dg==";
        let pk = PublicKey::from_base64(RELEASE_PUBLIC_KEY).unwrap();
        let signature = Signature::decode(SIGNATURE).unwrap();
        pk.verify(PAYLOAD, &signature, false)
            .expect("the release key must verify its own signature");
        let mut tampered = PAYLOAD.to_vec();
        tampered[0] ^= 0xFF;
        assert!(pk.verify(&tampered, &signature, false).is_err());
    }
}
