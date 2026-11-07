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
    /// For `unavailable` on the agent API: which cause, from a fixed list.
    pub detail: Option<&'static str>,
}

/// The causes of `unavailable` that nymirad names itself; the operator's own codes come from
/// its code table.
const DETAILS: [&str; 16] = [
    "attestation_failed",
    "attestation_unreachable",
    "bound_class_missing",
    "charge_warning",
    "decrypt_failed",
    "lease_gone",
    "notes_unreadable",
    "operator_emergency",
    "operator_unreachable",
    "operator_untrusted",
    "pins_malformed",
    "proving_key_changed",
    "tor_down",
    "tor_refused",
    "vault_pins_changed",
    "wallet_unreadable",
];

/// Codes whose message nymirad wrote itself from its own figures; everything else is replaced
/// by a fixed text on the agent API.
const OWN_MESSAGE: [&str; 18] = [
    "invalid_request",
    "invalid_token",
    "token_disabled",
    "policy_missing",
    "model_not_allowed",
    "call_limit",
    "daily_limit",
    "approval_required",
    "budget_exhausted",
    "note_expiring",
    "note_moved",
    "policy_rate_limit",
    "busy",
    "approval_limit",
    "approval_unknown",
    "approval_used",
    "approval_expired",
    "approval_mismatch",
];

impl ApiError {
    fn new(status: StatusCode, kind: &'static str, code: &str, message: impl Into<String>) -> Self {
        Self {
            status,
            kind,
            code: code.into(),
            message: message.into(),
            retry_after_secs: None,
            approval_id: None,
            detail: None,
        }
    }

    /// The error as the agent API shows it: a reason from the spec's table and nothing the
    /// operator or the model wrote. Operator errors without a row become `unavailable` with
    /// the operator's code as `detail`; a 429 from the operator's own limiters is
    /// `operator_capacity`. Every message that is not nymirad's own is a fixed text.
    pub fn typed(mut self) -> Self {
        let was = std::mem::take(&mut self.code);
        let (code, text): (&str, &str) = match (self.status.as_u16(), was.as_str()) {
            (_, c) if OWN_MESSAGE.contains(&c) => {
                self.code = was;
                return self;
            }
            (429, "provider_rate_limit") => (
                "provider_rate_limit",
                "the model provider is rate limiting; retry later",
            ),
            (429, _) => (
                "operator_capacity",
                "the operator is at capacity; retry after the wait given",
            ),
            (502, "not_verified") => (
                "not_verified",
                "the answer was withheld because its signature did not verify",
            ),
            (402, "no_note_can_cover_request") => (
                "no_note_can_cover_request",
                "the budget note's lease cannot pay for this request",
            ),
            _ => {
                self.detail = DETAILS
                    .into_iter()
                    .find(|d| *d == was)
                    .or_else(|| Code::ALL.into_iter().map(Code::as_str).find(|c| *c == was));
                (
                    "unavailable",
                    "nymirad cannot serve this request now; see detail",
                )
            }
        };
        if code == "unavailable" {
            self.status = StatusCode::SERVICE_UNAVAILABLE;
            self.kind = "server_error";
        }
        self.code = code.into();
        self.message = text.into();
        self
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

    /// `require_tee` withheld an answer that did not verify. The message names no content.
    pub fn not_verified(reason: &str) -> Self {
        Self::new(
            StatusCode::BAD_GATEWAY,
            "server_error",
            "not_verified",
            format!("the answer was withheld because it did not verify: {reason}"),
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

    /// The budget note is inside its last 5 days, or its revocation, close or exit has begun.
    pub fn note_expiring() -> Self {
        Self::new(
            StatusCode::PAYMENT_REQUIRED,
            "insufficient_funds",
            "note_expiring",
            "the budget note is closing, being revoked or about to expire; the owner decides what happens next",
        )
    }

    /// The budget note runs on the agent's own machine; this copy never spends it.
    pub fn note_moved() -> Self {
        Self::new(
            StatusCode::PAYMENT_REQUIRED,
            "insufficient_funds",
            "note_moved",
            "the budget note was moved to the agent's own machine; this copy does not spend it",
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
        if let Some(detail) = &self.detail {
            body["error"]["detail"] = json!(detail);
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
