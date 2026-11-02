//! The HTTP front door: loopback peers, an exact `Host`, no browser origins, the owner token,
//! and the owner route table.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use crate::token::OwnerToken;

pub const PORT: u16 = 8788;
const HOSTS: [&str; 3] = ["localhost:8788", "127.0.0.1:8788", "[::1]:8788"];

/// What the owner token may do over HTTP. Everything that moves money, funds a note or exposes
/// a secret runs in the CLI instead.
pub const OWNER_ROUTES: [(&str, &str); 2] =
    [("POST", "/v1/chat/completions"), ("GET", "/v1/models")];

fn refuse(status: StatusCode, code: &str, message: &str) -> Response {
    (
        status,
        Json(json!({ "error": { "message": message, "type": "invalid_request_error", "code": code } })),
    )
        .into_response()
}

fn host_ok(headers: &HeaderMap) -> bool {
    headers
        .get(header::HOST)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| HOSTS.contains(&h))
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

async fn guard(
    State(token): State<Arc<OwnerToken>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let headers = request.headers();
    if !peer.ip().is_loopback() {
        return refuse(
            StatusCode::FORBIDDEN,
            "peer_not_loopback",
            "loopback peers only",
        );
    }
    if !host_ok(headers) {
        return refuse(StatusCode::FORBIDDEN, "bad_host", "unexpected Host header");
    }
    if headers.contains_key(header::ORIGIN) {
        return refuse(
            StatusCode::FORBIDDEN,
            "origin_present",
            "browser requests are refused",
        );
    }
    if headers
        .get("sec-fetch-site")
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"cross-site"))
    {
        return refuse(
            StatusCode::FORBIDDEN,
            "cross_site",
            "cross-site requests are refused",
        );
    }
    // No CORS headers, so a browser preflight never succeeds.
    if request.method() == Method::OPTIONS {
        return StatusCode::NO_CONTENT.into_response();
    }
    if !bearer(headers).is_some_and(|presented| token.matches(presented)) {
        return refuse(
            StatusCode::UNAUTHORIZED,
            "invalid_token",
            "missing or wrong owner token",
        );
    }
    let (method, path) = (request.method().as_str(), request.uri().path());
    if !OWNER_ROUTES.iter().any(|(m, p)| *m == method && *p == path) {
        return refuse(
            StatusCode::FORBIDDEN,
            "owner_route_forbidden",
            "the owner token is not accepted on this route over HTTP",
        );
    }
    next.run(request).await
}

async fn models() -> axum::Json<serde_json::Value> {
    axum::Json(crate::openai::models(
        &nymira_operator::proxy::ProxyConfig::default().models,
    ))
}

async fn not_built() -> Response {
    refuse(
        StatusCode::NOT_IMPLEMENTED,
        "not_implemented",
        "chat through the operator is not built yet",
    )
}

/// The guard wraps every route, including paths that do not exist, so a probe learns nothing
/// before it passes. Without a gateway, chat answers 501.
pub fn router(token: Arc<OwnerToken>) -> Router {
    router_with(token, None)
}

pub fn router_with(
    token: Arc<OwnerToken>,
    gateway: Option<Arc<crate::gateway::Gateway>>,
) -> Router {
    let routes = match gateway {
        Some(gw) => Router::new()
            .route("/v1/chat/completions", post(crate::gateway::chat_handler))
            .route("/v1/models", get(crate::gateway::models_handler))
            .with_state(gw),
        None => Router::new()
            .route("/v1/chat/completions", post(not_built))
            .route("/v1/models", get(models)),
    };
    routes
        .fallback(not_built)
        .layer(middleware::from_fn_with_state(token.clone(), guard))
        .with_state(token)
}

/// Serves on `127.0.0.1:8788`.
pub async fn serve(
    token: Arc<OwnerToken>,
    gateway: Option<Arc<crate::gateway::Gateway>>,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", PORT)).await?;
    axum::serve(
        listener,
        router_with(token, gateway).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
}
