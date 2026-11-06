//! OpenAI's response shapes: the model list and the error body, and how an operator error maps
//! into them.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use nymira_operator::error::Code;
use nymira_operator::proxy::ModelSpec;
use serde_json::{json, Value};

/// `GET /v1/models`.
pub fn models(models: &[ModelSpec]) -> Value {
    json!({
        "object": "list",
        "data": models
            .iter()
            .map(|m| json!({
                "id": m.id,
                "object": "model",
                "created": 0,
                "owned_by": "near-ai",
            }))
            .collect::<Vec<_>>(),
    })
}

/// An OpenAI error answer: `{"error": {"message", "type", "code"}}`.
#[derive(Debug, PartialEq)]
pub struct ApiError {
    pub status: StatusCode,
    pub kind: &'static str,
    pub code: String,
    pub message: String,
    pub retry_after_secs: Option<u64>,
    /// The ID of a held request, for `approval_required`.
    pub approval_id: Option<String>,
}

impl ApiError {
    fn new(status: StatusCode, kind: &'static str, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            kind,
            code: code.into(),
            message: message.into(),
            retry_after_secs: None,
            approval_id: None,
        }
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_request_error",
            "invalid_request",
            message,
        )
    }

    /// The agent's signed policy does not list the requested model.
    pub fn model_not_allowed() -> Self {
        Self::new(
            StatusCode::PAYMENT_REQUIRED,
            "insufficient_funds",
            "model_not_allowed",
            "the owner's policy does not allow this model",
        )
    }

    /// The call, as sized, would pass the policy's `max_call_usd`.
    pub fn call_limit() -> Self {
        Self::new(
            StatusCode::PAYMENT_REQUIRED,
            "insufficient_funds",
            "call_limit",
            "the call would pass the owner's per-call limit; shorten the prompt or lower max_tokens",
        )
    }

    /// The call would take today's total past the policy's `max_daily_usd`.
    pub fn daily_limit() -> Self {
        Self::new(
            StatusCode::PAYMENT_REQUIRED,
            "insufficient_funds",
            "daily_limit",
            "the call would pass the owner's daily limit",
        )
    }

    /// The note's balance cannot cover the agent's bound class.
    pub fn budget_exhausted() -> Self {
        Self::new(
            StatusCode::PAYMENT_REQUIRED,
            "insufficient_funds",
            "budget_exhausted",
            "the budget note cannot cover a lease",
        )
    }

    /// More requests in a minute than the policy allows.
    pub fn policy_rate_limit(retry_after_secs: u64) -> Self {
        let mut e = Self::new(
            StatusCode::TOO_MANY_REQUESTS,
            "rate_limit_error",
            "policy_rate_limit",
            "the owner's rate limit is reached",
        );
        e.retry_after_secs = Some(retry_after_secs);
        e
    }

    /// The note's lease already has two requests in flight.
    pub fn busy() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "invalid_request_error",
            "busy",
            "the note's lease has two requests in flight; retry in a moment",
        )
    }

    /// No note can cover the lease bound.
    pub fn no_funds(reason: impl Into<String>) -> Self {
        Self::new(
            StatusCode::PAYMENT_REQUIRED,
            "insufficient_funds",
            "no_note_can_cover_request",
            reason,
        )
    }

    /// An operator error, by its `code`; a code this build does not know keeps the HTTP status.
    pub fn from_operator(status: u16, code: &str, message: &str, retry_after: Option<u64>) -> Self {
        let http = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
        let mut error = match Code::parse(code) {
            // Two 429s the user must tell apart: ours (the operator is full) and the model
            // provider's own limit.
            Some(Code::OperatorCapacity | Code::LeaseConcurrency | Code::NullifierRateLimited) => {
                Self::new(
                    http,
                    "rate_limit_error",
                    code,
                    format!("the operator is busy: {message}; wait and retry, or use another note"),
                )
            }
            Some(Code::ProviderRateLimit) => Self::new(
                http,
                "rate_limit_error",
                code,
                format!(
                    "the model provider is rate limiting: {message}; retry later or switch model"
                ),
            ),
            Some(Code::Emergency | Code::RecoveryMode | Code::ReplicaUnreachable) => Self::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "server_error",
                code,
                format!("the operator has stopped new leases: {message}; consider an exit"),
            ),
            Some(Code::ProviderError | Code::ProviderTimeout) => {
                Self::new(http, "server_error", code, message)
            }
            Some(_) => Self::new(http, "invalid_request_error", code, message),
            None => Self::new(
                http,
                "api_error",
                "operator_error",
                format!("operator error {status}: {message}"),
            ),
        };
        error.retry_after_secs = retry_after;
        error
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut body = json!({
            "error": {
                "message": self.message,
                "type": self.kind,
                "param": null,
                "code": self.code,
            }
        });
        if let Some(id) = &self.approval_id {
            body["error"]["approval_id"] = json!(id);
        }
        let mut response = (self.status, axum::Json(body)).into_response();
        if let Some(id) = &self.approval_id {
            if let Ok(v) = id.parse() {
                response.headers_mut().insert(crate::approvals::HEADER, v);
            }
        }
        if let Some(secs) = self.retry_after_secs {
            response
                .headers_mut()
                .insert(axum::http::header::RETRY_AFTER, secs.into());
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_model_list_has_openai_shape() {
        let list = models(&nymira_operator::proxy::ProxyConfig::default().models);
        assert_eq!(list["object"], "list");
        assert_eq!(list["data"][0]["id"], "z-ai/glm-5.3-flash");
        assert_eq!(list["data"][0]["object"], "model");
        assert_eq!(list["data"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn the_two_429s_read_differently() {
        let ours = ApiError::from_operator(429, "operator_capacity", "full", Some(5));
        let theirs = ApiError::from_operator(429, "provider_rate_limit", "slow down", Some(5));
        assert_ne!(ours.message, theirs.message);
        assert!(ours.message.contains("operator is busy"));
        assert!(theirs.message.contains("model provider"));
        assert_eq!(ours.retry_after_secs, Some(5));
    }

    #[test]
    fn an_unknown_code_keeps_its_status() {
        let e = ApiError::from_operator(418, "teapot", "short and stout", None);
        assert_eq!(e.status, StatusCode::IM_A_TEAPOT);
        assert_eq!(e.code, "operator_error");
        assert!(e.message.contains("418"));
    }

    #[test]
    fn no_funds_is_402_with_a_reason() {
        let e = ApiError::no_funds("every note has an open lease; keep two notes");
        assert_eq!(e.status, StatusCode::PAYMENT_REQUIRED);
        assert!(e.message.contains("keep two notes"));
    }
}
