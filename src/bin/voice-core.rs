//! The client. Agents and humans drive the runtime through exactly this API —
//! there is no privileged path. Notably it decides whether to play audio from
//! the runtime's reported presenter count rather than probing another
//! frontend's port, which is how v1 guessed.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use reqwest::header::AUTHORIZATION;
use serde_json::Value;

#[derive(Parser)]
#[command(
    name = "voice-core",
    version,
    about = "Speak, inspect and control a local voice-core runtime"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    #[arg(long, env = "VC_URL", default_value = "http://127.0.0.1:8760", global = true)]
    url: String,

    /// Defaults to VC_TOKEN, then `token.txt` in the data dir.
    #[arg(long, env = "VC_TOKEN", global = true)]
    token: Option<String>,

    #[arg(long, env = "VC_DATA_DIR", global = true)]
    data_dir: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum PlayMode {
    /// Play only when no other frontend is subscribed to the event stream.
    Auto,
    Always,
    Never,
}

#[derive(Subcommand)]
enum Command {
    /// Synthesize one utterance.
    Speak {
        /// Spoken text (Japanese for this project's voice packs).
        #[arg(long)]
        text: String,
        /// Text a human reads; never synthesized.
        #[arg(long)]
        display: Option<String>,
        /// Segment alignment between `--display` and `--text`, as JSON:
        /// `[{"base":"欢迎回来","ruby":"おかえりなさい"}]`. A presenter renders these
        /// directly instead of guessing which fragment means which; `@path.json` reads
        /// the array from a file and `-` reads it from stdin. Optional.
        #[arg(long)]
        ruby_pairs: Option<String>,
        /// Voice pack id, see `voice-core voices`.
        #[arg(long)]
        voice: Option<String>,
        /// What language `--text` is in (`ja`, `zh-CN`). Refused when the pack does not
        /// declare it; omit it to speak whatever the text is.
        #[arg(long)]
        language: Option<String>,
        #[arg(long)]
        seed: Option<u64>,
        #[arg(long)]
        steps: Option<u32>,
        #[arg(long)]
        display_seconds: Option<f64>,
        /// Subtitle colours for this one line: `#rgb`, `#rrggbb` or `#aarrggbb`. Highest
        /// tier there is - above the pack's manifest, which is above `config.json`.
        #[arg(long)]
        name_color: Option<String>,
        #[arg(long)]
        text_color: Option<String>,
        #[arg(long)]
        ruby_color: Option<String>,
        #[arg(long)]
        countdown_color: Option<String>,
        /// How the line arrives on screen: `typewriter`, `sweep` or `fade`. Anything else
        /// is refused by name rather than quietly becoming the default.
        #[arg(long)]
        reveal: Option<String>,
        /// Expression caption: a style annotation the engine conditions on separately from
        /// the words, so it changes delivery without being spoken. The checkpoint's 45
        /// emoji work here and inline in `--text`; repeating one strengthens it. Omit it to
        /// use the pack's own `expression.emotion`; pass `""` to speak it plainly for once.
        #[arg(long)]
        emotion: Option<String>,
        /// How hard `--emotion` steers, 0-10. The engine's default is 3.0.
        #[arg(long)]
        cfg_scale_caption: Option<f64>,
        #[arg(long)]
        timeout_ms: Option<u64>,
        #[arg(long, value_enum, default_value_t = PlayMode::Auto)]
        play: PlayMode,
        /// Keep the WAV at this path instead of a temporary file.
        #[arg(long)]
        out: Option<PathBuf>,
        /// Return only once a frontend reports that the audio finished playing, instead
        /// of as soon as it exists. This is how consecutive lines stay in order without
        /// guessing a sleep; it works whether the presenter or this process played.
        #[arg(long)]
        wait: bool,
        /// Whole budget for `--wait`, in ms. Defaults to the clip's `durationMs` + 5000.
        #[arg(long, requires = "wait")]
        wait_timeout_ms: Option<u64>,
    },
    /// List installed voice packs.
    Voices,
    /// Runtime, engine and spool state.
    Status,
    /// Latency and throughput counters.
    Metrics,
    /// Pay the model load now instead of during a conversation.
    Warm,
    /// Stop the engine process and release GPU memory; runtime keeps serving.
    Sleep,
    /// Follow the event stream (subtitles, worker state, progress).
    Events,
    /// Ask the runtime to exit.
    Stop,
    /// Check reachability, auth, engine state and voice packs.
    Doctor,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let base = cli.url.trim_end_matches('/').to_string();
    let http = reqwest::Client::new();

    match cli.command {
        Command::Speak {
            text,
            display,
            ruby_pairs,
            voice,
            language,
            seed,
            steps,
            display_seconds,
            timeout_ms,
            name_color,
            text_color,
            ruby_color,
            countdown_color,
            reveal,
            emotion,
            cfg_scale_caption,
            play,
            out,
            wait,
            wait_timeout_ms,
        } => {
            let began = Instant::now();
            let token = resolve_token(&cli.token, &cli.data_dir)?;
            let mut body = serde_json::json!({ "text": text });
            if let Some(display) = &display {
                body["displayText"] = Value::String(display.clone());
            }
            if let Some(pairs) = &ruby_pairs {
                body["rubyPairs"] = parse_ruby_pairs(pairs)?;
            }
            if let Some(voice) = &voice {
                body["voicePackId"] = Value::String(voice.clone());
            }
            if let Some(language) = &language {
                body["language"] = Value::String(language.clone());
            }
            if let Some(seed) = seed {
                body["seed"] = serde_json::json!(seed);
            }
            if let Some(steps) = steps {
                body["numSteps"] = serde_json::json!(steps);
            }
            if let Some(seconds) = display_seconds {
                body["displaySeconds"] = serde_json::json!(seconds);
            }
            if let Some(ms) = timeout_ms {
                body["timeoutMs"] = serde_json::json!(ms);
            }
            // One object, because that is the shape the API takes and the shape the
            // `speech` event carries back: five fields of one theme, not five parameters.
            let mut dialog = serde_json::Map::new();
            for (key, value) in [
                ("nameColor", &name_color),
                ("textColor", &text_color),
                ("rubyColor", &ruby_color),
                ("countdownColor", &countdown_color),
                ("reveal", &reveal),
            ] {
                if let Some(value) = value {
                    dialog.insert(key.to_string(), Value::String(value.clone()));
                }
            }
            if !dialog.is_empty() {
                body["dialog"] = Value::Object(dialog);
            }
            // Sent even when empty: `--emotion ""` is how a caller says "plainly, ignoring
            // this pack's default", which is not the same as saying nothing.
            if let Some(emotion) = &emotion {
                body["emotion"] = Value::String(emotion.clone());
            }
            if let Some(scale) = cfg_scale_caption {
                body["cfgScaleCaption"] = serde_json::json!(scale);
            }

            // Subscribed BEFORE the request: a playback report that lands between the
            // reply and the wait must not be missed. It also makes this process a
            // presenter, which is why `--play auto` below discounts one subscriber.
            let mut events = match wait {
                true => Some(EventStream::open(&http, &base, &token).await?),
                false => None,
            };

            let response = http
                .post(format!("{base}/api/speak"))
                .header(AUTHORIZATION, format!("Bearer {token}"))
                .json(&body)
                // The runtime owns the real deadline; this only bounds the wait.
                .timeout(Duration::from_secs(timeout_ms.unwrap_or(600_000) / 1000 + 30))
                .send()
                .await
                .context("runtime unreachable; is voice-core-runtime running?")?;
            let result = decode(response).await?;

            let audio_id = result["audioId"].as_str().unwrap_or_default().to_string();
            // `presenters` counts every event-stream subscriber, this process included
            // when it is waiting. What `--play auto` has to know is whether anybody ELSE
            // is listening, and that is the number worth printing too.
            let others = result["presenters"]
                .as_u64()
                .unwrap_or(0)
                .saturating_sub(u64::from(events.is_some()));
            println!(
                "{}",
                result["displayText"].as_str().unwrap_or(text.as_str())
            );
            // Without `--wait` the runtime's own total is the whole story. With it, the
            // number a caller cares about is not known until the audio has been heard.
            if events.is_none() {
                let total_ms = result["totalMs"].as_u64().unwrap_or(0);
                eprintln!("{}", summary(&result, total_ms, others));
            }

            let should_play = match play {
                PlayMode::Never => false,
                PlayMode::Always => true,
                PlayMode::Auto => others == 0,
            };
            if should_play || out.is_some() {
                let bytes = http
                    .get(format!("{base}/api/audio/{audio_id}"))
                    .header(AUTHORIZATION, format!("Bearer {token}"))
                    .timeout(Duration::from_secs(60))
                    .send()
                    .await
                    .context("cannot fetch audio")?
                    .bytes()
                    .await
                    .context("cannot read audio body")?;
                let path = match out {
                    Some(path) => path,
                    None => std::env::temp_dir().join(format!("voice-core-{audio_id}.wav")),
                };
                std::fs::write(&path, &bytes)
                    .with_context(|| format!("cannot write {}", path.display()))?;
                if should_play {
                    play_reported(&http, &base, &token, &audio_id, &path).await?;
                } else {
                    println!("{}", path.display());
                }
            }

            if let Some(events) = events.as_mut() {
                // `durationMs` plus enough for a presenter to fetch the bytes and open the
                // device. Short enough that a frontend which never plays is a failure
                // rather than a hang, and overridable for a machine where it is not.
                let budget = Duration::from_millis(
                    wait_timeout_ms
                        .unwrap_or_else(|| result["durationMs"].as_u64().unwrap_or(0) + 5_000),
                );
                let closure = wait_for_playback(events, &audio_id, budget).await?;
                eprintln!(
                    "{}",
                    summary(&result, began.elapsed().as_millis() as u64, others)
                );
                if !closure.finished {
                    bail!(
                        "--wait: nothing reported audio {audio_id} finished playing within \
                         {} ms; {}",
                        budget.as_millis(),
                        match closure.started_by {
                            Some(by) =>
                                format!("{by} reported starting it but never reported the end"),
                            None => "no frontend reported playing it at all: start the \
                                     presenter, or pass --play always to play it here"
                                .to_string(),
                        }
                    );
                }
            }
        }
        Command::Voices => {
            let token = resolve_token(&cli.token, &cli.data_dir)?;
            let packs = get(&http, &base, &token, "/api/voices").await?;
            match packs.as_array() {
                Some(list) if !list.is_empty() => {
                    for pack in list {
                        println!(
                            "{:<20} {:<24} {}",
                            pack["id"].as_str().unwrap_or("?"),
                            pack["name"].as_str().unwrap_or(""),
                            pack["kind"].as_str().unwrap_or("")
                        );
                    }
                }
                _ => println!("no voice packs registered in config.json"),
            }
        }
        Command::Status => {
            let token = resolve_token(&cli.token, &cli.data_dir)?;
            print_json(get(&http, &base, &token, "/api/status").await?);
        }
        Command::Metrics => {
            let token = resolve_token(&cli.token, &cli.data_dir)?;
            print_json(get(&http, &base, &token, "/api/metrics").await?);
        }
        Command::Warm => {
            let token = resolve_token(&cli.token, &cli.data_dir)?;
            print_json(post(&http, &base, &token, "/api/warm", 300).await?);
        }
        Command::Sleep => {
            let token = resolve_token(&cli.token, &cli.data_dir)?;
            print_json(post(&http, &base, &token, "/api/sleep", 30).await?);
        }
        Command::Stop => {
            let token = resolve_token(&cli.token, &cli.data_dir)?;
            print_json(post(&http, &base, &token, "/api/shutdown", 10).await?);
        }
        Command::Events => {
            let token = resolve_token(&cli.token, &cli.data_dir)?;
            follow_events(&http, &base, &token).await?;
        }
        Command::Doctor => {
            doctor(&http, &base, &cli).await?;
        }
    }
    Ok(())
}

async fn get(http: &reqwest::Client, base: &str, token: &str, path: &str) -> Result<Value> {
    let response = http
        .get(format!("{base}{path}"))
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .context("runtime unreachable; is voice-core-runtime running?")?;
    decode(response).await
}

async fn post(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    path: &str,
    timeout_secs: u64,
) -> Result<Value> {
    let response = http
        .post(format!("{base}{path}"))
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .timeout(Duration::from_secs(timeout_secs))
        .send()
        .await
        .context("runtime unreachable; is voice-core-runtime running?")?;
    decode(response).await
}

/// Structured errors stay structured: surface `code` and `recovery` instead of
/// collapsing everything into an exit status.
async fn decode(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    if status.is_success() {
        return Ok(body);
    }
    let code = body["code"].as_str().unwrap_or("unknown");
    let message = body["message"].as_str().unwrap_or("no message");
    match body["recovery"]["detail"].as_str() {
        Some(detail) => bail!("[{code}] {message}\n  try: {detail}"),
        None => bail!("[{code}] {message}"),
    }
}

/// The one line this CLI prints about a call.
///
/// `total_ms` is the runtime's own end-to-end number, or this process's wall clock
/// under `--wait` — which is the number a caller reading three paragraphs in order
/// actually spends, because it includes the playback.
fn summary(result: &Value, total_ms: u64, presenters: u64) -> String {
    format!(
        "request {} | {total_ms} ms total ({} ms synth{}) | {presenters} presenter(s)",
        result["requestId"].as_str().unwrap_or("?"),
        result["synthMs"].as_u64().unwrap_or(0),
        if result["coldStart"].as_bool().unwrap_or(false) {
            ", cold start"
        } else {
            ""
        },
    )
}

/// The runtime's event stream, one `data:` payload at a time. `voice-core events`
/// prints them and `speak --wait` matches them; while this lives, the runtime counts
/// this process as a presenter.
struct EventStream {
    response: reqwest::Response,
    buffer: String,
}

impl EventStream {
    async fn open(http: &reqwest::Client, base: &str, token: &str) -> Result<Self> {
        let response = http
            .get(format!("{base}/api/events"))
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .send()
            .await
            .context("runtime unreachable")?;
        let status = response.status();
        if !status.is_success() {
            // A refused stream is never usable, and its body carries the real reason.
            return Err(decode(response)
                .await
                .err()
                .unwrap_or_else(|| anyhow::anyhow!("event stream refused: HTTP {status}")));
        }
        Ok(Self {
            response,
            buffer: String::new(),
        })
    }

    /// Next payload verbatim, or `None` when the stream ended. Verbatim because
    /// `voice-core events` prints it, and re-serializing a parsed envelope would
    /// reorder the runtime's own fields.
    ///
    /// Unbounded on purpose: the caller owns the deadline, because only the caller
    /// knows what it is waiting for.
    async fn next(&mut self) -> Result<Option<String>> {
        loop {
            if let Some(payload) = self.take_frame() {
                return Ok(Some(payload));
            }
            let Some(chunk) = self.response.chunk().await.context("event stream broke")? else {
                return Ok(None);
            };
            self.buffer.push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    /// One complete frame out of the buffer. SSE frames end with a blank line, so a
    /// partial one has to stay buffered; a keep-alive frame carries no `data:` line.
    fn take_frame(&mut self) -> Option<String> {
        while let Some(index) = self.buffer.find("\n\n") {
            let frame: String = self.buffer.drain(..index + 2).collect();
            if let Some(payload) = frame.lines().find_map(|line| line.strip_prefix("data:")) {
                return Some(payload.trim().to_string());
            }
        }
        None
    }
}

async fn follow_events(http: &reqwest::Client, base: &str, token: &str) -> Result<()> {
    let mut events = EventStream::open(http, base, token).await?;
    while let Some(payload) = events.next().await? {
        println!("{payload}");
    }
    Ok(())
}

/// What the wait observed. Reported even when the budget ran out, because "started but
/// never finished" and "nobody ever played it" need different fixes.
#[derive(Default)]
struct Closure {
    started_by: Option<String>,
    finished: bool,
}

/// Waits for whoever played the audio to say it is over.
///
/// The reporter can be the tray presenter or this very process, and a caller must not
/// have to know which: both publish `playbackFinished` for the same `audioId`, so the
/// wait is one piece of code either way.
async fn wait_for_playback(
    events: &mut EventStream,
    audio_id: &str,
    budget: Duration,
) -> Result<Closure> {
    let mut seen = Closure::default();
    // A broken stream is a real failure worth reporting; a spent budget is the caller's
    // to report, since it knows what it was waiting for.
    if let Ok(result) = tokio::time::timeout(budget, watch_playback(events, audio_id, &mut seen)).await
    {
        result?;
    }
    Ok(seen)
}

async fn watch_playback(
    events: &mut EventStream,
    audio_id: &str,
    seen: &mut Closure,
) -> Result<()> {
    while let Some(payload) = events.next().await? {
        let Ok(event) = serde_json::from_str::<Value>(&payload) else {
            continue;
        };
        if event["audioId"].as_str() != Some(audio_id) {
            continue;
        }
        match event["kind"].as_str() {
            Some("playbackStarted") => {
                seen.started_by = event["by"].as_str().map(str::to_string);
            }
            Some("playbackFinished") => {
                seen.finished = true;
                return Ok(());
            }
            _ => {}
        }
    }
    // The stream ended without closure. The caller says so; there is nothing to retry.
    Ok(())
}

/// Plays the clip and tells the runtime both ends of it, so `--wait` — here or in
/// another process — closes on this playback exactly as it does on the presenter's.
async fn play_reported(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    audio_id: &str,
    path: &Path,
) -> Result<()> {
    if !PLAYS_AUDIO {
        // Nothing was played, so there is nothing to report.
        return play_wav(path).map(|_| ());
    }
    report_played(http, base, token, audio_id, "started", None).await?;
    let played = play_wav(path)?;
    report_played(
        http,
        base,
        token,
        audio_id,
        "finished",
        Some(played.as_millis() as u64),
    )
    .await
}

/// One playback fact into the event stream. The runtime cannot observe an audio device
/// it does not own, so the frontend that played says so itself.
async fn report_played(
    http: &reqwest::Client,
    base: &str,
    token: &str,
    audio_id: &str,
    event: &str,
    played_ms: Option<u64>,
) -> Result<()> {
    let mut body = serde_json::json!({ "audioId": audio_id, "event": event, "by": "cli" });
    if let Some(ms) = played_ms {
        body["playedMs"] = serde_json::json!(ms);
    }
    let response = http
        .post(format!("{base}/api/played"))
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .json(&body)
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .context("cannot report playback to the runtime")?;
    decode(response).await.map(|_| ())
}

async fn doctor(http: &reqwest::Client, base: &str, cli: &Cli) -> Result<()> {
    let health = http
        .get(format!("{base}/api/health"))
        .timeout(Duration::from_secs(5))
        .send()
        .await;
    match health {
        Ok(response) if response.status().is_success() => {
            let body: Value = response.json().await.unwrap_or(Value::Null);
            println!(
                "runtime      reachable, api v{}",
                body["apiVersion"].as_u64().unwrap_or(0)
            );
        }
        Ok(response) => {
            println!("runtime      HTTP {}", response.status());
            return Ok(());
        }
        Err(err) => {
            println!("runtime      NOT reachable: {err}");
            println!("  start it:  voice-core-runtime --tts-python <python.exe> --tts-root <root>");
            return Ok(());
        }
    }

    let token = match resolve_token(&cli.token, &cli.data_dir) {
        Ok(token) => token,
        Err(err) => {
            println!("token        {err}");
            return Ok(());
        }
    };
    match get(http, base, &token, "/api/status").await {
        Ok(status) => {
            println!("token        accepted");
            println!(
                "engine       managed={} running={} model_loaded={} idle={}ms",
                status["worker"]["managed"].as_bool().unwrap_or(false),
                status["worker"]["running"].as_bool().unwrap_or(false),
                status["worker"]["modelLoaded"].as_bool().unwrap_or(false),
                status["worker"]["idleMs"].as_u64().unwrap_or(0)
            );
            println!(
                "voice packs  {}",
                status["voicePacks"].as_u64().unwrap_or(0)
            );
            println!(
                "presenters   {}",
                status["presenters"].as_u64().unwrap_or(0)
            );
            println!(
                "spool        {} entr(ies), {} bytes",
                status["spool"]["entries"].as_u64().unwrap_or(0),
                status["spool"]["bytes"].as_u64().unwrap_or(0)
            );
        }
        Err(err) => println!("token        rejected: {err}"),
    }
    Ok(())
}

fn print_json(value: Value) {
    match serde_json::to_string_pretty(&value) {
        Ok(text) => println!("{text}"),
        Err(_) => println!("{value}"),
    }
}

/// Explicit flag, then environment, then `token.txt` in the data dir, then the
/// dist layout beside the executable. No silent unauthenticated fallback.
fn resolve_token(explicit: &Option<String>, data_dir: &Option<PathBuf>) -> Result<String> {
    if let Some(token) = explicit {
        if !token.trim().is_empty() {
            return Ok(token.trim().to_string());
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = data_dir {
        candidates.push(dir.join("token.txt"));
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent().and_then(|p| p.parent()) {
            candidates.push(parent.join("data").join("token.txt"));
        }
    }
    candidates.push(PathBuf::from("data").join("token.txt"));
    for candidate in &candidates {
        if let Ok(contents) = std::fs::read_to_string(candidate) {
            let token = contents.trim();
            if !token.is_empty() {
                return Ok(token.to_string());
            }
        }
    }
    bail!(
        "no token: pass --token, set VC_TOKEN, or point --data-dir at the runtime's data dir \
         (looked in {})",
        candidates
            .iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

/// Read the alignment array from inline JSON, from a file with a leading `@`, or from
/// stdin with `-`. It is validated here rather than forwarded blindly: a malformed
/// array is a caller bug worth a message, not a silently ignored field.
///
/// `-` exists because of Windows quoting: a JSON array full of quotes and CJK survives
/// a pipe intact, while the same array on a PowerShell 5.1 command line often does not.
fn parse_ruby_pairs(spec: &str) -> Result<Value> {
    let raw = match spec.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("cannot read ruby pairs from {path}"))?,
        None if spec == "-" => {
            std::io::read_to_string(std::io::stdin()).context("cannot read ruby pairs from stdin")?
        }
        None => spec.to_string(),
    };

    let parsed: Value = serde_json::from_str(&raw).context("--ruby-pairs is not valid JSON")?;
    let items = parsed
        .as_array()
        .context("--ruby-pairs must be a JSON array of {base, ruby} objects")?;
    for item in items {
        if !item.get("base").is_some_and(Value::is_string) {
            bail!("every ruby pair needs a string `base`; got {item}");
        }
        if let Some(ruby) = item.get("ruby") {
            if !ruby.is_string() {
                bail!("`ruby` must be a string when present; got {item}");
            }
        }
    }
    Ok(parsed)
}

/// Whether this build has a real player. The playback reports must not claim a device
/// that does not exist, and `--wait` must not close on a no-op.
#[cfg(windows)]
const PLAYS_AUDIO: bool = true;
#[cfg(not(windows))]
const PLAYS_AUDIO: bool = false;

/// Plays the clip and returns how long that took. The measurement is what the runtime
/// is told (`playedMs`), so it has to be the real thing rather than the clip's own
/// stated length.
#[cfg(windows)]
fn play_wav(path: &Path) -> Result<Duration> {
    let escaped = path.display().to_string().replace('\'', "''");
    let script = format!("(New-Object Media.SoundPlayer '{escaped}').PlaySync()");
    let began = Instant::now();
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .context("cannot start powershell to play audio")?;
    if !status.success() {
        bail!("playback failed; the wav is at {}", path.display());
    }
    Ok(began.elapsed())
}

#[cfg(not(windows))]
fn play_wav(path: &Path) -> Result<Duration> {
    // Honest limitation rather than a silent no-op: the only playback backend
    // implemented so far is the Windows one.
    println!("{}", path.display());
    eprintln!("playback is not implemented on this platform; the wav path is above");
    Ok(Duration::ZERO)
}
