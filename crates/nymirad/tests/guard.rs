use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use nymirad::guard::router;
use nymirad::token::{OwnerToken, TokenError};
use tower::ServiceExt;

struct Fixture {
    _dir: tempfile::TempDir,
    app: axum::Router,
    token: String,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("owner.token");
    let token = OwnerToken::load_or_create(&path).unwrap();
    Fixture {
        token: std::fs::read_to_string(&path).unwrap(),
        app: router(Arc::new(token)),
        _dir: dir,
    }
}

const LOCAL: &str = "127.0.0.1:50000";

/// A request with the given host and the owner token.
fn req(f: &Fixture, method: &str, path: &str, host: &str) -> axum::http::request::Builder {
    Request::builder()
        .method(method)
        .uri(path)
        .header("host", host)
        .header("authorization", format!("Bearer {}", f.token))
}

async fn send(f: &Fixture, b: axum::http::request::Builder, peer: &str) -> StatusCode {
    let mut request = b.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    f.app.clone().oneshot(request).await.unwrap().status()
}

#[tokio::test]
async fn the_token_file_is_nymo_prefixed_0600_and_stable() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t");
    OwnerToken::load_or_create(&path).unwrap();
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("nymo_") && text.len() == 5 + 64);
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(OwnerToken::load_or_create(&path).unwrap().matches(&text));
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(
        OwnerToken::load_or_create(&path),
        Err(TokenError::Mode(0o644))
    ));
}

#[tokio::test]
async fn the_owner_token_reaches_chat_and_models_only() {
    let f = fixture();
    for (m, p) in [("POST", "/v1/chat/completions"), ("GET", "/v1/models")] {
        let s = send(&f, req(&f, m, p, "localhost:8788"), LOCAL).await;
        let reached = if p == "/v1/models" {
            StatusCode::OK
        } else {
            StatusCode::NOT_IMPLEMENTED
        };
        assert_eq!(s, reached, "{m} {p} passes the guard");
    }
    for (m, p) in [
        ("POST", "/v1/deposit"),
        ("POST", "/v1/close"),
        ("GET", "/v1/notes"),
        ("GET", "/v1/chat/completions"),
        ("POST", "/v1/models"),
    ] {
        let s = send(&f, req(&f, m, p, "localhost:8788"), LOCAL).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} {p}");
    }
}

#[tokio::test]
async fn a_missing_or_wrong_token_is_refused() {
    let f = fixture();
    let none = Request::builder()
        .uri("/v1/models")
        .header("host", "localhost:8788");
    assert_eq!(send(&f, none, LOCAL).await, StatusCode::UNAUTHORIZED);
    let wrong = Request::builder()
        .uri("/v1/models")
        .header("host", "localhost:8788")
        .header("authorization", format!("Bearer nymo_{}", "0".repeat(64)));
    assert_eq!(send(&f, wrong, LOCAL).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn browser_and_remote_requests_are_refused() {
    let f = fixture();
    for host in ["localhost:8788", "127.0.0.1:8788", "[::1]:8788"] {
        let b = req(&f, "GET", "/v1/models", host);
        assert_eq!(send(&f, b, LOCAL).await, StatusCode::OK, "{host}");
    }
    for host in [
        "localhost",
        "localhost:8787",
        "evil.test:8788",
        "127.0.0.1.evil.test:8788",
    ] {
        let b = req(&f, "GET", "/v1/models", host);
        assert_eq!(send(&f, b, LOCAL).await, StatusCode::FORBIDDEN, "{host}");
    }
    let origin =
        req(&f, "GET", "/v1/models", "localhost:8788").header("origin", "http://localhost:8788");
    assert_eq!(send(&f, origin, LOCAL).await, StatusCode::FORBIDDEN);
    let cross =
        req(&f, "GET", "/v1/models", "localhost:8788").header("sec-fetch-site", "cross-site");
    assert_eq!(send(&f, cross, LOCAL).await, StatusCode::FORBIDDEN);
    let direct = req(&f, "GET", "/v1/models", "localhost:8788").header("sec-fetch-site", "none");
    assert_eq!(send(&f, direct, LOCAL).await, StatusCode::OK);
    for peer in ["192.168.1.5:4000", "[2001:db8::1]:4000"] {
        let b = req(&f, "GET", "/v1/models", "localhost:8788");
        assert_eq!(send(&f, b, peer).await, StatusCode::FORBIDDEN, "{peer}");
    }
    let v6 = req(&f, "GET", "/v1/models", "localhost:8788");
    assert_eq!(send(&f, v6, "[::1]:4000").await, StatusCode::OK);
}

#[tokio::test]
async fn options_answers_with_no_cors_headers() {
    let f = fixture();
    let mut request = Request::builder()
        .method("OPTIONS")
        .uri("/v1/chat/completions")
        .header("host", "localhost:8788")
        .body(Body::empty())
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()));
    let reply = f.app.clone().oneshot(request).await.unwrap();
    assert_eq!(reply.status(), StatusCode::NO_CONTENT);
    assert!(reply
        .headers()
        .keys()
        .all(|k| !k.as_str().starts_with("access-control-")));
}
