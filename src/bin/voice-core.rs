//! The client. Agents and humans drive the runtime through exactly this API —
//! there is no privileged path. Notably it decides whether to play audio from
//! the runtime's reported presenter count rather than probing another
//! frontend's port, which is how v1 guessed.

use std::path::PathBuf;
use std::time::Duration;

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
        /// the array from a file. Optional.
        #[arg(long)]
        ruby_pairs: Option<String>,
        /// Voice pack id, see `voice-core voices`.
        #[arg(long)]
        voice: Option<String>,
        #[arg(long)]
        seed: Option<u64>,
        #[arg(long)]
        steps: Option<u32>,
        #[arg(long)]
        display_seconds: Option<f64>,
        #[arg(long)]
        timeout_ms: Option<u64>,
        #[arg(long, value_enum, default_value_t = PlayMode::Auto)]
        play: PlayMode,
        /// Keep the WAV at this path instead of a temporary file.
        #[arg(long)]
        out: Option<PathBuf>,
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
            seed,
            steps,
            display_seconds,
            timeout_ms,
            play,
            out,
        } => {
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
            let presenters = result["presenters"].as_u64().unwrap_or(0);
            println!(
                "{}",
                result["displayText"].as_str().unwrap_or(text.as_str())
            );
            eprintln!(
                "request {} | {} ms total ({} ms synth{}) | {} presenter(s)",
                result["requestId"].as_str().unwrap_or("?"),
                result["totalMs"].as_u64().unwrap_or(0),
                result["synthMs"].as_u64().unwrap_or(0),
                if result["coldStart"].as_bool().unwrap_or(false) {
                    ", cold start"
                } else {
                    ""
                },
                presenters
            );

            let should_play = match play {
                PlayMode::Never => false,
                PlayMode::Always => true,
                PlayMode::Auto => presenters == 0,
            };
            if !should_play && out.is_none() {
                return Ok(());
            }

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
                play_wav(&path)?;
            } else {
                println!("{}", path.display());
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

async fn follow_events(http: &reqwest::Client, base: &str, token: &str) -> Result<()> {
    let mut response = http
        .get(format!("{base}/api/events"))
        .header(AUTHORIZATION, format!("Bearer {token}"))
        .send()
        .await
        .context("runtime unreachable")?;
    if !response.status().is_success() {
        return decode(response).await.map(|_| ());
    }
    let mut buffer = String::new();
    while let Some(chunk) = response.chunk().await.context("event stream broke")? {
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(index) = buffer.find("\n\n") {
            let frame: String = buffer.drain(..index + 2).collect();
            for line in frame.lines() {
                if let Some(payload) = line.strip_prefix("data:") {
                    println!("{}", payload.trim());
                }
            }
        }
    }
    Ok(())
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

/// Read the alignment array from inline JSON or, with a leading `@`, from a file. It is
/// validated here rather than forwarded blindly: a malformed array is a caller bug worth
/// a message, not a silently ignored field.
fn parse_ruby_pairs(spec: &str) -> Result<Value> {
    let raw = match spec.strip_prefix('@') {
        Some(path) => std::fs::read_to_string(path)
            .with_context(|| format!("cannot read ruby pairs from {path}"))?,
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

#[cfg(windows)]
fn play_wav(path: &std::path::Path) -> Result<()> {
    let escaped = path.display().to_string().replace('\'', "''");
    let script = format!("(New-Object Media.SoundPlayer '{escaped}').PlaySync()");
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .status()
        .context("cannot start powershell to play audio")?;
    if !status.success() {
        bail!("playback failed; the wav is at {}", path.display());
    }
    Ok(())
}

#[cfg(not(windows))]
fn play_wav(path: &std::path::Path) -> Result<()> {
    // Honest limitation rather than a silent no-op: the only playback backend
    // implemented so far is the Windows one.
    println!("{}", path.display());
    eprintln!("playback is not implemented on this platform; the wav path is above");
    Ok(())
}
