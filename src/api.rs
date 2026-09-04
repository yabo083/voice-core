//! The only public surface. Two access patterns over one contract: request
//! /response for commands, a single event stream for everything a frontend
//! would otherwise poll for. Agents and GUIs are peers here — neither gets a
//! private mode, and the runtime never calls a frontend back.

use std::convert::Infallible;
use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::Serialize;
use tokio::sync::broadcast;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};

use crate::config::token_matches;
use crate::error::{ApiError, ErrorCode, RecoveryKind};
use crate::obs::{Envelope, Event};
use crate::packs::VoicePack;
use crate::service::Service;
use crate::service::{SpeakInput, SpeakOutput, Status, API_VERSION, RUNTIME_VERSION};

pub fn router(service: Arc<Service>) -> Router {
    let guarded = Router::new()
        .route("/api/status", get(status))
        .route("/api/metrics", get(metrics))
        .route("/api/voices", get(voices))
        .route("/api/speak", post(speak))
        .route("/api/audio/:id", get(audio))
        .route("/api/events", get(events))
        .route("/api/warm", post(warm))
        .route("/api/sleep", post(sleep))
        .route("/api/requests/:id", delete(cancel))
        .route("/api/shutdown", post(shutdown))
        .layer(middleware::from_fn_with_state(
            Arc::clone(&service),
            require_token,
        ));

    Router::new()
        // Liveness only, deliberately unauthenticated: a launcher must be able
        // to tell "is it up" before it knows the token. Carries no secrets and
        // binds to loopback.
        .route("/api/health", get(health))
        .merge(guarded)
        .with_state(service)
}

async fn require_token(
    State(service): State<Arc<Service>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Response {
    let presented = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !token_matches(&service.config().token, presented) {
        return ApiError::new(ErrorCode::Unauthorized, "missing or invalid bearer token")
            .with_recovery(RecoveryKind::CheckToken, "read token.txt from the data dir")
            .into_response();
    }
    next.run(request).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Health {
    name: &'static str,
    runtime_version: &'static str,
    api_version: u32,
    ready: bool,
}

async fn health() -> Json<Health> {
    Json(Health {
        name: "voice-core",
        runtime_version: RUNTIME_VERSION,
        api_version: API_VERSION,
        ready: true,
    })
}

async fn status(State(service): State<Arc<Service>>) -> Json<Status> {
    Json(service.status().await)
}

async fn metrics(State(service): State<Arc<Service>>) -> impl IntoResponse {
    Json(service.metrics_snapshot())
}

async fn voices(State(service): State<Arc<Service>>) -> Json<Vec<VoicePack>> {
    Json(service.voices())
}

async fn speak(
    State(service): State<Arc<Service>>,
    Json(input): Json<SpeakInput>,
) -> Result<Json<SpeakOutput>, ApiError> {
    service.speak(input).await.map(Json)
}

/// Streams the WAV straight off the spool. No base64, no buffering the whole
/// clip in memory, and the id only resolves through the spool index so the
/// path can never be steered outside it.
async fn audio(
    State(service): State<Arc<Service>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let entry = service.spool().get(&id).ok_or_else(|| {
        ApiError::new(ErrorCode::NotFound, format!("no audio with id '{id}'")).with_recovery(
            RecoveryKind::Retry,
            "spool entries expire; call /api/speak again",
        )
    })?;
    let file = tokio::fs::File::open(&entry.path).await.map_err(|err| {
        ApiError::new(
            ErrorCode::NotFound,
            format!("audio '{id}' is registered but unreadable: {err}"),
        )
    })?;
    service.metrics().served(entry.bytes);
    let body = axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file));
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "audio/wav".to_string()),
            (header::CONTENT_LENGTH, entry.bytes.to_string()),
            (
                header::CACHE_CONTROL,
                "private, max-age=60".to_string(),
            ),
        ],
        body,
    )
        .into_response())
}

/// One stream for subtitles, worker state, progress and failures. A frontend
/// that reconnects receives the recent tail first so it can render current
/// state without a separate catch-up call.
///
/// A forwarder task owns the fan-in so that replay, lag reporting and shutdown
/// all live in one readable place. Shutdown matters: an SSE connection never
/// ends by itself, so without ending it here a single subscribed frontend would
/// keep the process alive forever. The opposite matters just as much — a client
/// that vanished must stop counting as a presenter — so the loop watches its own
/// channel for a dropped receiver instead of only finding out at the next event.
async fn events(
    State(service): State<Arc<Service>>,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Envelope>(64);
    let bus = Arc::clone(service.bus());
    // Subscription and replay tail arrive as a pair, and they are taken here
    // rather than inside the task: `presenters` must count this connection from
    // the moment the response starts, because `voice-core speak --play auto`
    // decides whether to play audio itself from that number.
    let (mut live, replay) = bus.subscribe_with_tail();
    tokio::spawn(async move {
        for envelope in replay {
            if tx.send(envelope).await.is_err() {
                return;
            }
        }
        let mut stopping = std::pin::pin!(bus.stopping());
        loop {
            let envelope = tokio::select! {
                _ = &mut stopping => return,
                // The one place a client that walked away is observable. Without
                // this arm the forwarder sits in `recv()` until the next event,
                // and until then its broadcast receiver still counts as a
                // presenter: on a quiet runtime the count only ever climbs, and
                // `--play auto` reads that as "somebody else will play it".
                _ = tx.closed() => return,
                received = live.recv() => match received {
                    Ok(envelope) => envelope,
                    // A slow subscriber lost events; say so rather than
                    // silently skipping them.
                    Err(broadcast::error::RecvError::Lagged(n)) => Envelope {
                        seq: u64::MAX,
                        ts_ms: crate::obs::now_ms(),
                        event: Event::Progress {
                            request_id: None,
                            phase: "event_stream".into(),
                            message: format!("dropped {n} event(s): subscriber too slow"),
                        },
                    },
                    Err(broadcast::error::RecvError::Closed) => return,
                },
            };
            if tx.send(envelope).await.is_err() {
                return;
            }
        }
    });

    let stream = ReceiverStream::new(rx).map(|envelope| {
        Ok(SseEvent::default()
            .json_data(&envelope)
            .unwrap_or_else(|_| SseEvent::default().data("{}")))
    });
    Sse::new(stream).keep_alive(KeepAlive::default())
}

async fn warm(State(service): State<Arc<Service>>) -> Result<Response, ApiError> {
    let status = service.warm().await?;
    Ok(Json(status).into_response())
}

async fn sleep(State(service): State<Arc<Service>>) -> Response {
    Json(service.sleep().await).into_response()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Cancelled {
    cancelled: bool,
}

async fn cancel(State(service): State<Arc<Service>>, Path(id): Path<String>) -> Response {
    Json(Cancelled {
        cancelled: service.cancel(&id),
    })
    .into_response()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Stopping {
    stopping: bool,
}

async fn shutdown(State(service): State<Arc<Service>>) -> Response {
    Json(Stopping {
        stopping: service.request_shutdown(),
    })
    .into_response()
}
