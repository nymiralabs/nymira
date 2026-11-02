//! The operator's error body and codes (operator spec, error table).

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::json;

/// Every `code` of the error table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Code {
    MalformedRequest,
    UnsupportedVersion,
    BoundClassInvalid,
    ProofExpired,
    UnsupportedRequest,
    MaxTokensInvalid,
    MaxTokensTooLarge,
    LeaseTokenInvalid,
    LeaseNotFound,
    RootStale,
    NullifierSeen,
    NullifierInExit,
    ClearanceSigned,
    LeaseOpen,
    LeaseUsed,
    NoteNotActive,
    LeaseEnded,
    BodyTooLarge,
    ProofInvalid,
    OperatorCapacity,
    NullifierRateLimited,
    LeaseConcurrency,
    ProviderRateLimit,
    PuzzleRequired,
    CloseRateLimited,
    ProviderError,
    RecoveryMode,
    ReplicaUnreachable,
    Emergency,
    ProviderTimeout,
}

impl Code {
    pub const ALL: [Code; 30] = [
        Code::MalformedRequest,
        Code::UnsupportedVersion,
        Code::BoundClassInvalid,
        Code::ProofExpired,
        Code::UnsupportedRequest,
        Code::MaxTokensInvalid,
        Code::MaxTokensTooLarge,
        Code::LeaseTokenInvalid,
        Code::LeaseNotFound,
        Code::RootStale,
        Code::NullifierSeen,
        Code::NullifierInExit,
        Code::ClearanceSigned,
        Code::LeaseOpen,
        Code::LeaseUsed,
        Code::NoteNotActive,
        Code::LeaseEnded,
        Code::BodyTooLarge,
        Code::ProofInvalid,
        Code::OperatorCapacity,
        Code::NullifierRateLimited,
        Code::LeaseConcurrency,
        Code::ProviderRateLimit,
        Code::PuzzleRequired,
        Code::CloseRateLimited,
        Code::ProviderError,
        Code::RecoveryMode,
        Code::ReplicaUnreachable,
        Code::Emergency,
        Code::ProviderTimeout,
    ];

    pub fn parse(word: &str) -> Option<Code> {
        Self::ALL.into_iter().find(|c| c.as_str() == word)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Code::MalformedRequest => "malformed_request",
            Code::UnsupportedVersion => "unsupported_version",
            Code::BoundClassInvalid => "bound_class_invalid",
            Code::ProofExpired => "proof_expired",
            Code::UnsupportedRequest => "unsupported_request",
            Code::MaxTokensInvalid => "max_tokens_invalid",
            Code::MaxTokensTooLarge => "max_tokens_too_large",
            Code::LeaseTokenInvalid => "lease_token_invalid",
            Code::LeaseNotFound => "lease_not_found",
            Code::RootStale => "root_stale",
            Code::NullifierSeen => "nullifier_seen",
            Code::NullifierInExit => "nullifier_in_exit",
            Code::ClearanceSigned => "clearance_signed",
            Code::LeaseOpen => "lease_open",
            Code::LeaseUsed => "lease_used",
            Code::NoteNotActive => "note_not_active",
            Code::LeaseEnded => "lease_ended",
            Code::BodyTooLarge => "body_too_large",
            Code::ProofInvalid => "proof_invalid",
            Code::OperatorCapacity => "operator_capacity",
            Code::NullifierRateLimited => "nullifier_rate_limited",
            Code::LeaseConcurrency => "lease_concurrency",
            Code::ProviderRateLimit => "provider_rate_limit",
            Code::PuzzleRequired => "puzzle_required",
            Code::CloseRateLimited => "close_rate_limited",
            Code::ProviderError => "provider_error",
            Code::RecoveryMode => "recovery_mode",
            Code::ReplicaUnreachable => "replica_unreachable",
            Code::Emergency => "emergency",
            Code::ProviderTimeout => "provider_timeout",
        }
    }

    pub fn status(self) -> StatusCode {
        match self {
            Code::MalformedRequest
            | Code::UnsupportedVersion
            | Code::BoundClassInvalid
            | Code::ProofExpired
            | Code::UnsupportedRequest
            | Code::MaxTokensInvalid
            | Code::MaxTokensTooLarge => StatusCode::BAD_REQUEST,
            Code::LeaseTokenInvalid => StatusCode::UNAUTHORIZED,
            Code::LeaseNotFound => StatusCode::NOT_FOUND,
            Code::RootStale
            | Code::NullifierSeen
            | Code::NullifierInExit
            | Code::ClearanceSigned
            | Code::LeaseOpen
            | Code::LeaseUsed
            | Code::NoteNotActive => StatusCode::CONFLICT,
            Code::LeaseEnded => StatusCode::GONE,
            Code::BodyTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Code::ProofInvalid => StatusCode::UNPROCESSABLE_ENTITY,
            Code::OperatorCapacity
            | Code::NullifierRateLimited
            | Code::LeaseConcurrency
            | Code::ProviderRateLimit
            | Code::PuzzleRequired
            | Code::CloseRateLimited => StatusCode::TOO_MANY_REQUESTS,
            Code::ProviderError => StatusCode::BAD_GATEWAY,
            Code::RecoveryMode | Code::ReplicaUnreachable | Code::Emergency => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            Code::ProviderTimeout => StatusCode::GATEWAY_TIMEOUT,
        }
    }
}

/// An error answer: `{"error": {"code", "message", "retry_after_secs"}}`; the last field and the
/// `Retry-After` header only on 429 and 503.
#[derive(Debug)]
pub struct ApiError {
    pub code: Code,
    pub message: String,
    pub retry_after_secs: Option<u64>,
}

impl ApiError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retry_after_secs: None,
        }
    }

    pub fn retry_after(mut self, secs: u64) -> Self {
        self.retry_after_secs = Some(secs);
        self
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = self.code.status();
        let mut error = json!({ "code": self.code.as_str(), "message": self.message });
        let retry = self.retry_after_secs.filter(|_| {
            status == StatusCode::TOO_MANY_REQUESTS || status == StatusCode::SERVICE_UNAVAILABLE
        });
        if let Some(secs) = retry {
            error["retry_after_secs"] = secs.into();
        }
        let mut response = (status, axum::Json(json!({ "error": error }))).into_response();
        if let Some(secs) = retry {
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(secs));
        }
        response
    }
}
