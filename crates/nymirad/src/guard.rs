//! The HTTP front door: loopback peers, an exact `Host`, no browser origins, the tokens, and
//! the route tables.
//!
//! Two listeners share this module. Loopback TCP on 8788 takes the owner token (chat and models
//! only) and `tcp` agent tokens. The agent socket takes `unix` agent tokens only. An agent
//! token reaches agent routes on both and nothing else; the owner token never selects a budget
//! note.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{header, HeaderMap, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use crate::agents::{self, Book, Registry, Scope, Transport, AGENT_ROUTES};
use crate::agentsock::PeerUid;
use crate::token::{self, OwnerToken};

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

/// The listener a request arrived on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// Loopback TCP on 8788: the owner token and `tcp` agent tokens.
    Tcp,
    /// The agent Unix socket: `unix` agent tokens only.
    AgentSocket,
}

/// Who may ask, kept per listener.
#[derive(Clone)]
struct Auth {
    owner: Arc<OwnerToken>,
    registry: Registry,
    side: Side,
    policy: crate::policy::Context,
}

fn route_listed(table: &[(&str, &str)], method: &str, path: &str) -> bool {
    table.iter().any(|(m, p)| *m == method && *p == path)
}

/// An agent token: it must exist, be enabled, belong to this transport (and, on the socket, to
/// this peer), reach an agent route, and have a signed policy in force.
#[allow(clippy::result_large_err)]
fn agent_scope(
    auth: &Auth,
    book: &Book,
    presented: &str,
    transport: Transport,
    peer: Option<u32>,
    method: &str,
    path: &str,
) -> Result<Scope, Response> {
    let Some(agent) = book.find(presented) else {
        return Err(refuse(
            StatusCode::UNAUTHORIZED,
            "invalid_token",
            "missing or wrong token",
        ));
    };
    if agent.disabled {
        return Err(refuse(
            StatusCode::UNAUTHORIZED,
            "token_disabled",
            "this agent token was disabled by the owner",
        ));
    }
    if agent.transport != transport || (transport == Transport::Unix && agent.uid != peer) {
        return Err(refuse(
            StatusCode::FORBIDDEN,
            "wrong_transport",
            "this token is not accepted here",
        ));
    }
    if !route_listed(&AGENT_ROUTES, method, path) {
        return Err(refuse(
            StatusCode::FORBIDDEN,
            "agent_route_forbidden",
            "the agent token is not accepted on this route",
        ));
    }
    // The stored policy is checked again on every request: an unsigned, altered, replaced or
    // unconfirmed one is no policy.
    let policy = auth
        .registry
        .home()
        .and_then(|home| crate::policy::in_force(home, agent, &auth.policy));
    let Some(policy) = policy else {
        return Err(refuse(
            StatusCode::PAYMENT_REQUIRED,
            "policy_missing",
            "the owner has not loaded a valid signed policy for this agent",
        ));
    };
    Ok(Scope::Agent {
        name: agent.name.clone(),
        note: agent.note.clone(),
        policy,
    })
}

/// The token check shared by both listeners. `peer` is the socket peer's user ID.
#[allow(clippy::result_large_err)]
fn authorize(auth: &Auth, request: &Request, peer: Option<u32>) -> Result<Scope, Response> {
    // A registry that cannot be read stops everything: the owner's scope depends on it.
    let book = auth
        .registry
        .load()
        .map_err(|why| refuse(StatusCode::SERVICE_UNAVAILABLE, "registry_unreadable", &why))?;
    let (method, path) = (request.method().as_str(), request.uri().path());
    match (auth.side, bearer(request.headers())) {
        (Side::Tcp, Some(t)) if t.starts_with(token::PREFIX) => {
            if !auth.owner.matches(t) {
                return Err(refuse(
                    StatusCode::UNAUTHORIZED,
                    "invalid_token",
                    "missing or wrong owner token",
                ));
            }
            if !route_listed(&OWNER_ROUTES, method, path) {
                return Err(refuse(
                    StatusCode::FORBIDDEN,
                    "owner_route_forbidden",
                    "the owner token is not accepted on this route over HTTP",
                ));
            }
            Ok(Scope::Owner {
                budget: book.budget_notes(),
            })
        }
        (Side::Tcp, Some(t)) if t.starts_with(agents::PREFIX) => {
            agent_scope(auth, &book, t, Transport::Tcp, None, method, path)
        }
        (Side::AgentSocket, Some(t)) if t.starts_with(agents::PREFIX) => {
            agent_scope(auth, &book, t, Transport::Unix, peer, method, path)
        }
        (Side::AgentSocket, Some(t)) if t.starts_with(token::PREFIX) => Err(refuse(
            StatusCode::FORBIDDEN,
            "agent_routes_only",
            "this socket serves agent routes only",
        )),
        _ => Err(refuse(
            StatusCode::UNAUTHORIZED,
            "invalid_token",
            "missing or wrong token",
        )),
    }
}

async fn guard(
    State(auth): State<Auth>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    mut request: Request,
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
    match authorize(&auth, &request, None) {
        Ok(scope) => {
            request.extensions_mut().insert(scope);
            next.run(request).await
        }
        Err(refusal) => refusal,
    }
}

async fn socket_guard(
    State(auth): State<Auth>,
    ConnectInfo(PeerUid(peer)): ConnectInfo<PeerUid>,
    mut request: Request,
    next: Next,
) -> Response {
    match authorize(&auth, &request, Some(peer)) {
        Ok(scope) => {
            request.extensions_mut().insert(scope);
            next.run(request).await
        }
        Err(refusal) => refusal,
    }
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
    router_for(token, gateway, Registry::none(), Side::Tcp)
}

/// The routes for one listener. `registry` says which agents exist; `side` says which tokens
/// this listener takes.
pub fn router_for(
    token: Arc<OwnerToken>,
    gateway: Option<Arc<crate::gateway::Gateway>>,
    registry: Registry,
    side: Side,
) -> Router {
    let gateway_ctx = gateway.as_ref().map(|gw| gw.policy_context());
    let routes = match gateway {
        Some(gw) => Router::new()
            .route("/v1/chat/completions", post(crate::gateway::chat_handler))
            .route("/v1/models", get(crate::gateway::models_handler))
            .with_state(gw),
        None => Router::new()
            .route("/v1/chat/completions", post(not_built))
            .route("/v1/models", get(models)),
    };
    let policy = match &gateway_ctx {
        Some(ctx) => ctx.clone(),
        None => crate::policy::Context::new(
            nymira_operator::proxy::ProxyConfig::default()
                .models
                .into_iter()
                .map(|m| m.id)
                .collect(),
            &[100_000],
        ),
    };
    let auth = Auth {
        owner: token,
        registry,
        side,
        policy,
    };
    let routes = routes.fallback(not_built);
    match side {
        Side::Tcp => routes.layer(middleware::from_fn_with_state(auth.clone(), guard)),
        Side::AgentSocket => {
            routes.layer(middleware::from_fn_with_state(auth.clone(), socket_guard))
        }
    }
    .with_state(auth)
}

/// Serves on `127.0.0.1:port` (`PORT` outside tests). `on_bound` runs once the port is bound and before the first
/// request; if it fails, so does `serve`.
pub async fn serve(
    token: Arc<OwnerToken>,
    gateway: Option<Arc<crate::gateway::Gateway>>,
    port: u16,
    on_bound: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    serve_with(token, gateway, Registry::none(), port, on_bound).await
}

pub async fn serve_with(
    token: Arc<OwnerToken>,
    gateway: Option<Arc<crate::gateway::Gateway>>,
    registry: Registry,
    port: u16,
    on_bound: impl FnOnce() -> std::io::Result<()>,
) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await?;
    on_bound()?;
    let app = router_for(token, gateway.clone(), registry, Side::Tcp);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    // The conversation ends with the process: retire what is open so no slot fee runs on.
    if let Some(gw) = gateway {
        for event in gw.end_session().await {
            eprintln!("{event}");
        }
    }
    Ok(())
}
