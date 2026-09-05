//! One error shape for the whole public surface.
//!
//! Callers switch on `code`, never on prose, and `recovery` carries an action a
//! frontend can actually take instead of advice to read a repository runbook.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    Unauthorized,
    InvalidRequest,
    NotFound,
    VoicePackNotFound,
    /// The pack exists but does not declare the language the caller asked for.
    VoiceLanguageUnsupported,
    WorkerUnavailable,
    WorkerStartFailed,
    ModelLoadFailed,
    ResourceBusy,
    DeadlineExceeded,
    Cancelled,
    Internal,
}

/// What a frontend should do next, as a machine-readable kind.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryKind {
    Retry,
    Wait,
    CheckToken,
    CheckWorkerLogs,
    InstallVoicePack,
    FixRequest,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Recovery {
    pub kind: RecoveryKind,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recovery: Option<Recovery>,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            recovery: None,
        }
    }

    pub fn with_recovery(mut self, kind: RecoveryKind, detail: impl Into<String>) -> Self {
        self.recovery = Some(Recovery {
            kind,
            detail: detail.into(),
        });
        self
    }

    pub fn status(&self) -> StatusCode {
        match self.code {
            ErrorCode::Unauthorized => StatusCode::UNAUTHORIZED,
            // The pack exists and the request is well-formed; what cannot be honoured
            // is the pair. That is a caller error, not a missing resource.
            ErrorCode::InvalidRequest | ErrorCode::VoiceLanguageUnsupported => {
                StatusCode::BAD_REQUEST
            }
            ErrorCode::NotFound | ErrorCode::VoicePackNotFound => StatusCode::NOT_FOUND,
            ErrorCode::WorkerUnavailable => StatusCode::SERVICE_UNAVAILABLE,
            ErrorCode::ResourceBusy => StatusCode::TOO_MANY_REQUESTS,
            ErrorCode::DeadlineExceeded => StatusCode::GATEWAY_TIMEOUT,
            // Non-standard by design: the request was abandoned by its own
            // caller, which is neither a client nor a server fault.
            ErrorCode::Cancelled => StatusCode::from_u16(499).unwrap_or(StatusCode::CONFLICT),
            ErrorCode::WorkerStartFailed | ErrorCode::ModelLoadFailed | ErrorCode::Internal => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    }

    pub fn code_str(&self) -> &'static str {
        match self.code {
            ErrorCode::Unauthorized => "unauthorized",
            ErrorCode::InvalidRequest => "invalid_request",
            ErrorCode::NotFound => "not_found",
            ErrorCode::VoicePackNotFound => "voice_pack_not_found",
            ErrorCode::VoiceLanguageUnsupported => "voice_language_unsupported",
            ErrorCode::WorkerUnavailable => "worker_unavailable",
            ErrorCode::WorkerStartFailed => "worker_start_failed",
            ErrorCode::ModelLoadFailed => "model_load_failed",
            ErrorCode::ResourceBusy => "resource_busy",
            ErrorCode::DeadlineExceeded => "deadline_exceeded",
            ErrorCode::Cancelled => "cancelled",
            ErrorCode::Internal => "internal",
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.status();
        (status, Json(self)).into_response()
    }
}
