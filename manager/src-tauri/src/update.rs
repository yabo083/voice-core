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
//! remembered only as the download's preferred first candidate, never as a fact
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
    /// GitHub computes and stores SHA-256 per asset. Unsigned installer +
    /// published hash is this project's integrity model, so the check runs
    /// whenever the API offers it and the download is refused when the hash is
    /// offered and does not match.
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
    /// The mirror table, so the dropdown renders from the same list the backend
    /// walks rather than a second copy of the names.
    pub mirrors: Vec<(&'static str, &'static str)>,
    /// The mirror that answered, so a check that succeeded through ghproxy.net
    /// can say something about the network that 有新版本 alone does not.
    pub via: Option<String>,
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
        // Response-headers budget for every call made through this client; body
        // streaming gets its own idle timeout at the read site.
        .connect_timeout(PER_MIRROR_TIMEOUT)
        .timeout(PER_MIRROR_TIMEOUT)
        .user_agent(concat!("voice-core/", env!("CARGO_PKG_VERSION")));
    let builder = match explicit_proxy()? {
        Some(proxy) => builder.proxy(proxy),
        None => builder,
    };
    builder.build().map_err(|err| err.to_string())
}

/// One GitHub URL as the candidate list: the preferred mirror first when the
/// check found one, then everything else in [`MIRRORS`] order. Wrapping is the
/// whole proxy scheme: `<mirror>/<original-url>`.
fn candidates(preferred: Option<&str>, url: &str) -> Vec<(String, String)> {
    let wrap = |prefix: &str| {
        if prefix.is_empty() {
            url.to_string()
        } else {
            format!("{prefix}/{url}")
        }
    };
    let mut out = Vec::new();
    if let Some(name) = preferred {
        if let Some((_, prefix)) = MIRRORS.iter().find(|(n, _)| *n == name) {
            out.push((name.to_string(), wrap(prefix)));
        }
    }
    for (name, prefix) in MIRRORS {
        if Some(name) != preferred {
            out.push((name.to_string(), wrap(prefix)));
        }
    }
    out
}

/// `GET /releases/latest`, mirrors in order, first success wins. A
/// `no-releases` answer short-circuits: it is the API speaking, and no other
/// mirror can improve on it.
async fn fetch_release_any() -> Result<(ReleaseInfo, Option<String>), String> {
    let client = outbound_client()?;
    let mut errors: Vec<String> = Vec::new();
    for (name, prefix) in MIRRORS {
        match fetch_release(&client, prefix).await {
            Ok(info) => return Ok((info, Some(name.to_string()))),
            Err(err) if err == "no-releases" => return Err(err),
            Err(err) => errors.push(format!("{name}: {err}")),
        }
    }
    Err(errors.join("；"))
}

/// Is there a newer release? Answers with both ends of the comparison and the
/// release itself, newer or not — the panel shows 最新版本 either way. The 404
/// case is this project's real "nothing published yet", phrased as its own
/// sentence rather than as a failure of all four mirrors.
#[tauri::command]
pub async fn update_check(preferred_mirror: Option<String>) -> Result<CheckOutcome, String> {
    let _ = preferred_mirror; // the check itself walks the whole list; kept for symmetry
    let current = env!("CARGO_PKG_VERSION").to_string();
    let (release, via) = fetch_release_any().await.map_err(|err| {
        if err == "no-releases" {
            "GitHub 上还没有任何 release".to_string()
        } else {
            format!("所有镜像都失败了 — {err}")
        }
    })?;
    Ok(CheckOutcome {
        update_available: is_newer(&release.version, &current),
        current,
        release,
        mirrors: MIRRORS.to_vec(),
        via,
    })
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
pub async fn update_download(
    app: tauri::AppHandle,
    asset: AssetInfo,
    preferred_mirror: Option<String>,
) -> Result<(), String> {
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
        run_download(app, staged_path, asset, preferred_mirror).await;
    });
    Ok(())
}

/// The transfer: candidates in order, the first one that yields a verified file
/// wins. Every failure recorded and the next tried; the last error is what the
/// panel shows when the list runs out.
async fn run_download(
    app: tauri::AppHandle,
    staged_path: PathBuf,
    asset: AssetInfo,
    preferred: Option<String>,
) {
    let host = app.state::<Host>();
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
    for (name, url) in candidates(preferred.as_deref(), &asset.url) {
        match try_candidate(&client, &host, &staged_path, &asset, &url, &name).await {
            Ok(bytes) => {
                host.update.set(|state| {
                    state.progress.downloaded = bytes;
                    state.progress.total = bytes;
                    state.progress.active = false;
                    state.progress.done = true;
                    state.staged = Some(staged_path.display().to_string());
                });
                host.log(&format!("update: staged {}", staged_path.display()));
                return;
            }
            Err(err) => {
                let _ = std::fs::remove_file(&staged_path);
                last_error = err;
            }
        }
    }
    finish_failed(&host, last_error);
}

/// One mirror, start to finish: response headers, body streamed to disk while
/// hashed, size then digest checked. Any `Err` leaves no file behind — the
/// caller removes the partial on its way to the next candidate.
async fn try_candidate(
    client: &reqwest::Client,
    host: &Host,
    staged_path: &Path,
    asset: &AssetInfo,
    url: &str,
    name: &str,
) -> Result<u64, String> {
    let response = client
        .get(url)
        .timeout(PER_MIRROR_TIMEOUT)
        .send()
        .await
        .map_err(|err| format!("{name}: {err}"))?;
    if !response.status().is_success() {
        return Err(format!("{name}: HTTP {}", response.status().as_u16()));
    }

    let mut file = tokio::fs::File::create(staged_path)
        .await
        .map_err(|err| format!("无法写入 {}：{err}", staged_path.display()))?;
    let mut hasher = Sha256::new();
    let mut downloaded: u64 = 0;
    let mut last_tick = now_ms();
    let mut stream = response;
    loop {
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

    // A mirror that truncates at 90% but answers 200 must not stage a partial
    // installer: the API said how big the file is.
    if asset.size != 0 && downloaded != asset.size {
        return Err(format!(
            "{name}: 收到 {downloaded} / {} 字节，下载被截断",
            asset.size
        ));
    }
    // Integrity. This is the only thing standing between an unsigned installer
    // and a tampered one, so a mismatch is a refusal, not a warning.
    if let Some(expected) = &asset.digest {
        let actual = format!("{:x}", hasher.finalize());
        if !actual.eq_ignore_ascii_case(expected) {
            host.log(&format!("update: digest mismatch from {name}"));
            return Err(format!("{name}: SHA256 不匹配，已丢弃"));
        }
    }
    Ok(downloaded)
}

fn finish_failed(host: &Host, message: String) {
    host.log(&format!("update: failed: {message}"));
    host.update.set(|state| {
        state.progress.active = false;
        state.progress.failed = true;
        state.progress.error = message;
    });
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

    // Stack down first. The installer's CloseApplications would close the
    // runtime anyway; closing it here means the state change reaches the panel
    // before anything starts disappearing under it.
    let _ = crate::supervise::stop_stack(app.clone()).await;

    let mut command = std::process::Command::new("cmd");
    command
        .arg("/C")
        .arg("start")
        .arg("")
        .arg(&staged)
        .arg("/VERYSILENT")
        .arg("/SUPPRESSMSGBOXES")
        .arg("/NORESTART")
        .arg("/CLOSEAPPLICATIONS")
        .arg("/RESTARTAPPLICATIONS");
    hidden(&mut command);
    match command.spawn() {
        // Deliberately not waited on: the installer is Windows's problem now,
        // and `std::mem::forget` is what keeps the Child destructor from
        // concluding the parent should reap it.
        Ok(child) => {
            std::mem::forget(child);
            host.log(&format!("update: installer launched from {staged}"));
            host.update.set(|state| {
                state.launched = Some(staged.clone());
                state.progress = DownloadProgress::default();
                state.staged = None;
            });
            Ok(())
        }
        Err(err) => Err(format!("无法启动安装程序：{err}")),
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
