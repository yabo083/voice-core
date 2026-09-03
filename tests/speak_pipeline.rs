//! End-to-end proof of the framework's claims, with a fake engine so no GPU or
//! 7 GB model is needed:
//!
//! * audio never appears in JSON — `speak` returns an id, bytes come from
//!   `GET /api/audio/{id}` with `Content-Type: audio/wav`;
//! * the event stream carries the subtitle and reports itself as a presenter;
//! * one `requestId` links the response, the event and `metrics.jsonl`;
//! * `/api/health` is reachable without a token while everything else is not;
//! * `/api/shutdown` actually terminates the server.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::{json, Value};
use voice_core::config::{Config, WorkerSource};

/// 24 kHz mono PCM_16 WAV with a short ramp; enough to be a real file.
fn wav_bytes() -> Vec<u8> {
    let samples: Vec<i16> = (0..2400).map(|i| ((i % 256) as i16) * 100).collect();
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&1u16.to_le_bytes()); // mono
    out.extend_from_slice(&24000u32.to_le_bytes());
    out.extend_from_slice(&48000u32.to_le_bytes()); // byte rate
    out.extend_from_slice(&2u16.to_le_bytes()); // block align
    out.extend_from_slice(&16u16.to_le_bytes()); // bits
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

/// The fake owns one bit of engine state: whether a model is loaded. `/load`,
/// `/unload` and `/synthesize` move it and `/health` reports it, so the middle state
/// idle reclaim aims for — process up, model gone — is observable rather than
/// asserted. It starts loaded because the speak path probes `/health` per utterance
/// to decide `coldStart`, and a fake that boots unloaded would report every first
/// utterance as a cold start.
type Loaded = State<Arc<AtomicBool>>;

async fn health(State(loaded): Loaded) -> Json<Value> {
    Json(json!({ "ready": true, "modelLoaded": loaded.load(Ordering::Relaxed) }))
}

async fn load(State(loaded): Loaded) -> Json<Value> {
    loaded.store(true, Ordering::Relaxed);
    Json(json!({ "modelLoaded": true, "loadMs": 0 }))
}

async fn unload(State(loaded): Loaded) -> Json<Value> {
    loaded.store(false, Ordering::Relaxed);
    Json(json!({ "modelLoaded": false, "freedMs": 0 }))
}

/// Writes into the spool path the runtime reserved, exactly like the real
/// worker. Returning `bytes` is informational; the runtime stats the file.
async fn synthesize(State(loaded): Loaded, Json(body): Json<Value>) -> Json<Value> {
    // The real worker loads lazily on the first utterance, so speaking implies loaded.
    loaded.store(true, Ordering::Relaxed);
    let out_path = body["outPath"].as_str().expect("outPath is required");
    let bytes = wav_bytes();
    std::fs::write(out_path, &bytes).expect("worker can write into the spool");
    Json(json!({
        "sampleRate": 24000,
        "durationMs": 100,
        "bytes": bytes.len(),
    }))
}

async fn start_fake_engine() -> String {
    let app = Router::new()
        .route("/health", get(health))
        .route("/load", post(load))
        .route("/unload", post(unload))
        .route("/synthesize", post(synthesize))
        .with_state(Arc::new(AtomicBool::new(true)));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    format!("http://127.0.0.1:{port}")
}

struct Harness {
    base: String,
    token: String,
    data_dir: PathBuf,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
}

async fn start_runtime(label: &str, with_packs: bool) -> Harness {
    let data_dir = std::env::temp_dir().join(format!(
        "voice-core-v2-test-{label}-{}",
        uuid_like()
    ));
    std::fs::create_dir_all(&data_dir).unwrap();
    if with_packs {
        // Packs live in the app's one settings file, and it is JSONC on purpose - the
        // comment here is part of the fixture: the runtime must read what the tray writes.
        std::fs::write(
            data_dir.join("config.json"),
            format!(
                "// test fixture\n{}\n",
                json!({
                    "voicePacks": [{
                        "id": "test-voice",
                        "name": "Test Voice",
                        "languages": ["ja"],
                        "kind": "lora-adapter",
                        "path": "voicepacks/test-voice",
                        "engine": "fake"
                    }]
                })
            ),
        )
        .unwrap();
    }

    let engine_url = start_fake_engine().await;
    let token = "test-token-0123456789".to_string();
    let cfg = Config {
        bind: "127.0.0.1:0".parse().unwrap(),
        data_dir: data_dir.clone(),
        token: token.clone(),
        worker: WorkerSource::External {
            base_url: engine_url,
        },
        idle_stop: None,
        spool_ttl: Duration::from_secs(600),
        spool_max_bytes: 64 * 1024 * 1024,
        worker_ready_timeout: Duration::from_secs(5),
        synth_timeout: Duration::from_secs(30),
    };

    let assembled = voice_core::assemble(cfg).unwrap();
    let listener = voice_core::bind("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let service = Arc::clone(&assembled.service);
    let server = tokio::spawn(async move {
        voice_core::serve(listener, service, assembled.shutdown).await
    });

    Harness {
        base,
        token,
        data_dir,
        server,
    }
}

fn uuid_like() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    format!(
        "{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    )
}

#[tokio::test]
async fn speak_returns_an_id_and_audio_comes_from_the_spool() {
    let harness = start_runtime("speak", true).await;
    let http = reqwest::Client::new();

    // Subscribe first so this test counts as a presenter and sees the event.
    let mut stream = http
        .get(format!("{}/api/events", harness.base))
        .bearer_auth(&harness.token)
        .send()
        .await
        .unwrap();
    assert!(stream.status().is_success());

    // Give the subscription a moment to register on the bus.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let response = http
        .post(format!("{}/api/speak", harness.base))
        .bearer_auth(&harness.token)
        .json(&json!({
            "text": "こんにちは",
            "displayText": "你好",
            "voicePackId": "test-voice"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();

    let audio_id = body["audioId"].as_str().expect("audioId").to_string();
    let request_id = body["requestId"].as_str().expect("requestId").to_string();
    assert_eq!(body["sampleRate"], 24000);
    assert_eq!(body["displayText"], "你好");
    assert_eq!(body["voicePackId"], "test-voice");
    assert_eq!(body["presenters"], 1, "the SSE subscriber must be counted");
    assert_eq!(body["coldStart"], false);
    assert!(body["bytes"].as_u64().unwrap() > 44);
    // The contract: no audio payload in the control response.
    assert!(body.get("chunks").is_none());
    assert!(body.get("wavBase64").is_none());
    assert!(!body.to_string().contains("RIFF"));

    // Bytes come from the byte endpoint, unmodified.
    let audio = http
        .get(format!("{}/api/audio/{audio_id}", harness.base))
        .bearer_auth(&harness.token)
        .send()
        .await
        .unwrap();
    assert_eq!(audio.status(), 200);
    assert_eq!(audio.headers()["content-type"], "audio/wav");
    let bytes = audio.bytes().await.unwrap();
    assert_eq!(bytes.as_ref(), wav_bytes().as_slice());

    // A history view probes replay availability with HEAD, so that must answer
    // for a live id and 404 for an unknown one without transferring anything.
    let probe = http
        .head(format!("{}/api/audio/{audio_id}", harness.base))
        .bearer_auth(&harness.token)
        .send()
        .await
        .unwrap();
    assert_eq!(probe.status(), 200);
    assert!(probe.bytes().await.unwrap().is_empty());
    let gone = http
        .head(format!("{}/api/audio/0000000000000000", harness.base))
        .bearer_auth(&harness.token)
        .send()
        .await
        .unwrap();
    assert_eq!(gone.status(), 404);

    // The subtitle event carries the same request and audio ids.
    let speech = read_event(&mut stream, "speech", &request_id).await;
    assert_eq!(speech["audioId"].as_str().unwrap(), audio_id);
    assert_eq!(speech["displayText"].as_str().unwrap(), "你好");
    assert_eq!(speech["text"].as_str().unwrap(), "こんにちは");

    // And the same id lands in metrics.jsonl.
    let metrics_file = harness.data_dir.join("metrics.jsonl");
    let mut found = false;
    for _ in 0..40 {
        if let Ok(text) = std::fs::read_to_string(&metrics_file) {
            if text.contains(&request_id) {
                found = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(found, "metrics.jsonl must carry the request id");

    // Status reflects the spool and the live presenter.
    let status: Value = http
        .get(format!("{}/api/status", harness.base))
        .bearer_auth(&harness.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["spool"]["entries"], 1);
    assert_eq!(status["worker"]["managed"], false);
    assert_eq!(status["voicePacks"], 1);

    // Deliberately keep the event stream open across shutdown: an SSE
    // connection never ends on its own, and a runtime that waits for it can
    // never exit. Regression guard for exactly that hang.
    shutdown(&http, harness).await;
    drop(stream);
}

/// Reads SSE frames until an event of `kind` for `request_id` shows up. Every
/// read is bounded so a broken stream fails the test instead of hanging it.
async fn read_event(response: &mut reqwest::Response, kind: &str, request_id: &str) -> Value {
    let mut buffer = String::new();
    for _ in 0..200 {
        let next = tokio::time::timeout(Duration::from_secs(5), response.chunk())
            .await
            .expect("event stream produced no data within 5s");
        let Some(chunk) = next.unwrap() else {
            break;
        };
        buffer.push_str(&String::from_utf8_lossy(&chunk));
        while let Some(index) = buffer.find("\n\n") {
            let frame: String = buffer.drain(..index + 2).collect();
            for line in frame.lines() {
                let Some(payload) = line.strip_prefix("data:") else {
                    continue;
                };
                let Ok(event) = serde_json::from_str::<Value>(payload.trim()) else {
                    continue;
                };
                if event["kind"] == kind && event["requestId"] == request_id {
                    return event;
                }
            }
        }
    }
    panic!("event {kind} for {request_id} never arrived");
}

#[tokio::test]
async fn health_is_open_and_everything_else_needs_the_token() {
    let harness = start_runtime("auth", false).await;
    let http = reqwest::Client::new();

    let health = http
        .get(format!("{}/api/health", harness.base))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    let body: Value = health.json().await.unwrap();
    assert_eq!(body["apiVersion"], 1);

    let unauthorized = http
        .get(format!("{}/api/status", harness.base))
        .send()
        .await
        .unwrap();
    assert_eq!(unauthorized.status(), 401);
    let error: Value = unauthorized.json().await.unwrap();
    assert_eq!(error["code"], "unauthorized");
    assert_eq!(error["recovery"]["kind"], "check_token");

    // An unknown pack is a structured 404 that names what is installed.
    let missing = http
        .post(format!("{}/api/speak", harness.base))
        .bearer_auth(&harness.token)
        .json(&json!({ "text": "テスト", "voicePackId": "nope" }))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 404);
    let error: Value = missing.json().await.unwrap();
    assert_eq!(error["code"], "voice_pack_not_found");
    assert_eq!(error["recovery"]["kind"], "install_voice_pack");

    // Empty text is rejected before any device work happens.
    let empty = http
        .post(format!("{}/api/speak", harness.base))
        .bearer_auth(&harness.token)
        .json(&json!({ "text": "   " }))
        .send()
        .await
        .unwrap();
    assert_eq!(empty.status(), 400);
    assert_eq!(
        empty.json::<Value>().await.unwrap()["code"],
        "invalid_request"
    );

    shutdown(&http, harness).await;
}

/// `/api/shutdown` must end the process, not merely flip a status field.
async fn shutdown(http: &reqwest::Client, harness: Harness) {
    let response = http
        .post(format!("{}/api/shutdown", harness.base))
        .bearer_auth(&harness.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.json::<Value>().await.unwrap()["stopping"], true);

    let stopped = tokio::time::timeout(Duration::from_secs(10), harness.server).await;
    assert!(
        stopped.is_ok(),
        "the server must return after /api/shutdown"
    );
    let _ = std::fs::remove_dir_all(&harness.data_dir);
}
