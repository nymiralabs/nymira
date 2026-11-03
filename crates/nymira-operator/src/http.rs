//! The HTTP surface: routes, CORS and the body limit.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{to_bytes, Body};
use axum::extract::{Path, State};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::error::{ApiError, Code};
use crate::operator::{Operator, LEASE_BODY_LIMIT};

const PUZZLE_HEADER: &str = "x-nymira-puzzle";

pub fn router(operator: Arc<Operator>) -> Router {
    let origin =
        HeaderValue::from_str(&operator.config().web_origin).expect("web origin is a header value");
    let cors = CorsLayer::new()
        // A predicate, not `exact`: that would send the allowed origin to every caller.
        .allow_origin(AllowOrigin::predicate(move |requested, _| {
            *requested == origin
        }))
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            HeaderName::from_static("x-nymira-lease-token"),
            HeaderName::from_static("x-nymira-lease-id"),
            HeaderName::from_static(PUZZLE_HEADER),
            HeaderName::from_static("x-signing-algo"),
            HeaderName::from_static("x-client-pub-key"),
            HeaderName::from_static("x-model-pub-key"),
            HeaderName::from_static("x-encryption-version"),
            HeaderName::from_static("x-encrypt-all-fields"),
            HeaderName::from_static("x-no-aliasing"),
        ])
        .expose_headers([header::RETRY_AFTER])
        .max_age(Duration::from_secs(600));
    Router::new()
        .route("/v1/status", get(status))
        .route("/v2/openrouter/leases", post(open_lease))
        .route("/v2/openrouter/leases/{id}", get(lease_status).post(retire))
        .route("/v2/openrouter/leases/{id}/expire", post(expire))
        .route("/v2/requests/{id}", get(request_status))
        .route("/v2/nullifiers/{n}", get(nullifier_status))
        .route("/v2/nullifiers/{n}/evidence", get(nullifier_evidence))
        .route("/v2/withdraw/clearance", post(clearance))
        .route("/v1/close", post(close))
        .route("/v2/reconcile", post(reconcile))
        .route("/v1/chat/completions", post(chat))
        .route("/v2/billing/quote", get(quote))
        .route("/v1/attestation", get(attestation))
        .layer(cors)
        .with_state(operator)
}

async fn status(State(op): State<Arc<Operator>>) -> Json<serde_json::Value> {
    Json(op.status_json().await)
}

/// The body of a request, or the 413 that answers one over `limit`.
async fn bounded(request: Request<Body>, limit: usize) -> Result<axum::body::Bytes, ApiError> {
    let too_large = || {
        ApiError::new(
            Code::BodyTooLarge,
            format!("request body over {} KiB", limit / 1024),
        )
    };
    if request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .is_some_and(|len| len > limit)
    {
        return Err(too_large());
    }
    to_bytes(request.into_body(), limit)
        .await
        .map_err(|_| too_large())
}

async fn open_lease(
    State(op): State<Arc<Operator>>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Response {
    let body = match bounded(request, LEASE_BODY_LIMIT).await {
        Ok(body) => body,
        Err(e) => return e.into_response(),
    };
    let puzzle = headers.get(PUZZLE_HEADER).and_then(|v| v.to_str().ok());
    match op.open_lease(&body, puzzle).await {
        Ok((status, body)) => (status, Json(body)).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn clearance(State(op): State<Arc<Operator>>, request: Request<Body>) -> Response {
    let body = match bounded(request, LEASE_BODY_LIMIT).await {
        Ok(body) => body,
        Err(e) => return e.into_response(),
    };
    answer(op.clearance(&body).await.map(|v| (StatusCode::OK, v)))
}

async fn reconcile(State(op): State<Arc<Operator>>, request: Request<Body>) -> Response {
    let body = match bounded(request, LEASE_BODY_LIMIT).await {
        Ok(body) => body,
        Err(e) => return e.into_response(),
    };
    answer(op.reconcile(&body).await.map(|v| (StatusCode::OK, v)))
}

async fn close(State(op): State<Arc<Operator>>, request: Request<Body>) -> Response {
    let body = match bounded(request, crate::closer::CLOSE_BODY_LIMIT).await {
        Ok(body) => body,
        Err(e) => return e.into_response(),
    };
    answer(op.close(&body).await.map(|v| (StatusCode::OK, v)))
}

async fn nullifier_status(State(op): State<Arc<Operator>>, Path(n): Path<String>) -> Response {
    answer(op.nullifier_status(&n).await)
}

async fn nullifier_evidence(State(op): State<Arc<Operator>>, Path(n): Path<String>) -> Response {
    answer(op.nullifier_evidence(&n).await)
}

/// zkapi's billing-quote route, kept for wire compatibility: Nymira prices in USDC 1:1 and
/// puts no quote in the payload.
async fn quote() -> Json<serde_json::Value> {
    Json(
        json!({ "status": "ok", "pricing": "usdc_1_to_1", "units_per_usd": "1000000", "quote": null }),
    )
}

/// zkapi's attestation route; the operator runs outside a TEE in this phase.
async fn attestation() -> Json<serde_json::Value> {
    Json(json!({ "status": "ok", "attestation": null, "mode": "none", "version": 1 }))
}

fn lease_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get("x-nymira-lease-token")
        .and_then(|v| v.to_str().ok())
}

fn answer(result: Result<(StatusCode, serde_json::Value), ApiError>) -> Response {
    match result {
        Ok((status, body)) => (status, Json(body)).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn lease_status(
    State(op): State<Arc<Operator>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    answer(
        op.lease_status(&id, lease_token(&headers))
            .await
            .map(|v| (StatusCode::OK, v)),
    )
}

async fn retire(
    State(op): State<Arc<Operator>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    answer(op.retire(&id, lease_token(&headers)).await)
}

async fn expire(
    State(op): State<Arc<Operator>>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    answer(
        op.expire(&id, lease_token(&headers))
            .await
            .map(|v| (StatusCode::OK, v)),
    )
}

async fn request_status(State(op): State<Arc<Operator>>, Path(id): Path<String>) -> Response {
    answer(op.request_status(&id).await.map(|v| (StatusCode::OK, v)))
}

async fn chat(
    State(op): State<Arc<Operator>>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Response {
    let lease = headers
        .get("x-nymira-lease-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // The lease token is checked before up to 4 MiB of body is read into memory: a caller with
    // no lease costs one lookup, not a buffer.
    if let Err(e) = op.authorized(lease, lease_token(&headers)).await {
        return e.into_response();
    }
    let Ok(body) = to_bytes(request.into_body(), crate::proxy::CHAT_BODY_LIMIT).await else {
        return ApiError::new(Code::BodyTooLarge, "request body over 4 MiB").into_response();
    };
    match op.chat(lease, lease_token(&headers), &headers, &body).await {
        Err(e) => e.into_response(),
        Ok(reply) => {
            use tokio_stream::StreamExt;
            let first = tokio_stream::once(Ok::<_, std::io::Error>(reply.first));
            let rest = tokio_stream::wrappers::ReceiverStream::new(reply.rest).map(|item| {
                item.map_err(|e| {
                    std::io::Error::other(match e {
                        // A fixed word: upstream text never reaches the connection error.
                        crate::proxy::PumpError::Timeout => "upstream timed out",
                        crate::proxy::PumpError::Cut => "answer cut",
                        crate::proxy::PumpError::Provider(_) => "upstream failed",
                    })
                })
            });
            let mut response = Response::new(Body::from_stream(first.chain(rest)));
            if let Ok(value) = HeaderValue::from_str(&reply.content_type) {
                response.headers_mut().insert(header::CONTENT_TYPE, value);
            }
            response
        }
    }
}
