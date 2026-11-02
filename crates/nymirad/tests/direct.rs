//! The direct client: one host, no redirects, no proxy from the environment, no other name.

use axum::http::{header, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use nymirad::direct::{DirectClient, DirectError};
use serde_json::json;

async fn server() -> u16 {
    let app = Router::new()
        .route(
            "/close",
            post(|| async {
                (
                    StatusCode::FOUND,
                    [(header::LOCATION, "http://other.example/steal")],
                )
            }),
        )
        .route("/ok", post(|| async { Json(json!({ "sent": true })) }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    port
}

#[tokio::test]
async fn a_redirect_is_not_followed_and_no_other_name_is_resolved() {
    let port = server().await;
    let direct = DirectClient::new(&format!("http://localhost:{port}")).unwrap();
    let (status, _) = direct.post_json("/close", &json!({})).await.unwrap();
    assert_eq!(
        status, 302,
        "the redirect is the answer, not a thing to follow"
    );
    let (status, body) = direct.post_json("/ok", &json!({})).await.unwrap();
    assert_eq!((status, body["sent"].as_bool()), (200, Some(true)));
    assert!(
        direct.names_asked().iter().all(|n| n == "localhost"),
        "resolved {:?}",
        direct.names_asked()
    );
}

#[tokio::test]
async fn a_path_cannot_leave_the_host() {
    let direct = DirectClient::new("http://localhost:1").unwrap();
    for path in [
        "http://other.example/x",
        "//other.example/x",
        "relative",
        "/\\other.example",
    ] {
        assert!(
            matches!(
                direct.post_json(path, &json!({})).await,
                Err(DirectError::Path(_))
            ),
            "{path}"
        );
    }
    assert!(direct.names_asked().is_empty());
}

#[tokio::test]
async fn the_proxy_environment_is_ignored() {
    let port = server().await;
    // A proxy nobody listens on: a client that read it would fail.
    std::env::set_var("HTTP_PROXY", "http://127.0.0.1:1");
    std::env::set_var("ALL_PROXY", "http://127.0.0.1:1");
    let direct = DirectClient::new(&format!("http://localhost:{port}")).unwrap();
    assert_eq!(direct.post_json("/ok", &json!({})).await.unwrap().0, 200);
}

#[test]
fn bad_base_urls_are_refused() {
    for base in ["", "ftp://x/", "file:///etc", "not a url"] {
        assert!(DirectClient::new(base).is_err(), "{base}");
    }
}
