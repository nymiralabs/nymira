//! Identities on the SOCKS proxy, fail-closed gating, and the three kinds of failure told apart.

mod support;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::routing::{get, post};
use axum::{http::StatusCode, Json, Router};
use nymirad::rpc::{client, send_error, Rpc, RpcError, Transport};
use nymirad::tor::{Tor, TorState};
use serde_json::json;
use support::socks::{Behavior, FakeSocks};

/// A JSON-RPC backend that answers every call, and a status route.
async fn backend(status: StatusCode) -> SocketAddr {
    let app = Router::new()
        .route(
            "/",
            post(move || async move { (status, Json(json!({ "result": { "ok": true } }))) }),
        )
        .route(
            "/v1/status",
            get(move || async move { (status, Json(json!({}))) }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn socks(proxy: &FakeSocks) -> Transport {
    Transport::Socks(proxy.addr.to_string())
}

#[tokio::test]
async fn each_rpc_read_gets_a_fresh_identity_and_names_stay_with_the_proxy() {
    let proxy = FakeSocks::start(Behavior::Tunnel(backend(StatusCode::OK).await)).await;
    let rpc = Rpc::new("http://rpc.mainnet.near.org:80/", &socks(&proxy)).unwrap();
    for _ in 0..3 {
        rpc.call("status", json!({})).await.unwrap();
    }
    let seen = proxy.seen();
    assert_eq!(seen.len(), 3);
    // socks5h: the proxy gets the name, this machine resolves nothing.
    assert!(seen
        .iter()
        .all(|s| s.atyp == 3 && s.host == "rpc.mainnet.near.org" && s.port == 80));
    let mut users: Vec<_> = seen.iter().map(|s| s.user.clone()).collect();
    users.sort();
    users.dedup();
    assert_eq!(users.len(), 3, "every read has its own identity");
    assert!(seen
        .iter()
        .all(|s| s.user.len() == 64 && s.user.bytes().all(|b| b.is_ascii_hexdigit())));
}

#[tokio::test]
async fn a_conversation_keeps_one_identity_across_connections() {
    let proxy = FakeSocks::start(Behavior::Tunnel(backend(StatusCode::OK).await)).await;
    let conversation = client(&socks(&proxy)).unwrap();
    // Two names force two connections on the same client.
    for url in [
        "http://cloud-api.near.ai/v1/status",
        "http://operator.example/v1/status",
    ] {
        conversation.get(url).send().await.unwrap();
    }
    let other = client(&socks(&proxy)).unwrap();
    other
        .get("http://operator.example/v1/status")
        .send()
        .await
        .unwrap();
    let seen = proxy.seen();
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[0].user, seen[1].user);
    assert_ne!(seen[0].user, seen[2].user);
}

#[tokio::test]
async fn tor_that_is_not_ready_sends_nothing() {
    let proxy = FakeSocks::start(Behavior::Tunnel(backend(StatusCode::OK).await)).await;
    let (tor, state) = Tor::for_tests(proxy.addr, TorState::Starting(40));
    let transport = Transport::Tor(Arc::new(tor));
    let rpc = Rpc::new("http://rpc.mainnet.near.org/", &transport).unwrap();
    let Err(RpcError::TorDown(why)) = rpc.call("status", json!({})).await else {
        panic!("not ready must refuse");
    };
    assert!(why.contains("40%"));
    state.send(TorState::Failed("tor exited".into())).unwrap();
    assert!(matches!(client(&transport), Err(RpcError::TorDown(_))));
    assert!(
        proxy.seen().is_empty(),
        "nothing reached the proxy, and nothing went direct"
    );

    state.send(TorState::Ready).unwrap();
    assert_eq!(rpc.call("status", json!({})).await.unwrap()["ok"], true);
    assert_eq!(proxy.seen().len(), 1);
}

#[tokio::test]
async fn tor_down_and_a_refusing_other_side_are_reported_apart() {
    // No proxy listening: Tor is down.
    let closed = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let rpc = Rpc::new(
        "http://rpc.mainnet.near.org/",
        &Transport::Socks(closed.to_string()),
    )
    .unwrap();
    let Err(RpcError::TorDown(_)) = rpc.call("status", json!({})).await else {
        panic!("a proxy that is not there means Tor is down");
    };

    // The proxy answers but cannot reach the destination (reply 2: not allowed by ruleset).
    let proxy = FakeSocks::start(Behavior::Refuse(2)).await;
    let rpc = Rpc::new("http://rpc.mainnet.near.org/", &socks(&proxy)).unwrap();
    let Err(RpcError::TorRefused(_)) = rpc.call("status", json!({})).await else {
        panic!("a SOCKS refusal is the other side, not Tor");
    };

    // The destination answers 403: it turns Tor exits away.
    let proxy = FakeSocks::start(Behavior::Tunnel(backend(StatusCode::FORBIDDEN).await)).await;
    let Err(RpcError::TorRefused(m)) =
        nymirad::status::operator_status("http://operator.example", &socks(&proxy)).await
    else {
        panic!("403 from the operator in Tor mode is a refusal");
    };
    assert!(m.contains("403"));

    // The same 403 on an RPC read.
    let rpc = Rpc::new("http://rpc.mainnet.near.org/", &socks(&proxy)).unwrap();
    assert!(matches!(
        rpc.call("status", json!({})).await,
        Err(RpcError::TorRefused(_))
    ));

    // A connection error with no proxy in it is neither.
    let e = reqwest::Client::new()
        .get("http://127.0.0.1:1/")
        .send()
        .await
        .unwrap_err();
    assert!(matches!(send_error(e), RpcError::Transport(_)));
}

#[tokio::test]
async fn note_status_goes_over_tor_only() {
    let app = Router::new().route(
        "/",
        post(|| async { Json(json!({ "result": { "result": b"\"Active\"".to_vec() } })) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    // Direct: refused before anything is sent.
    let direct = Rpc::new(format!("http://{backend}/"), &Transport::Direct).unwrap();
    assert!(matches!(
        direct.note_status("vault.near", "ab").await,
        Err(RpcError::Transport(m)) if m.contains("Tor only")
    ));

    let proxy = FakeSocks::start(Behavior::Tunnel(backend)).await;
    let over_tor = Rpc::new("http://rpc.mainnet.near.org/", &socks(&proxy)).unwrap();
    assert_eq!(
        over_tor
            .note_status("vault.near", "ab")
            .await
            .unwrap()
            .as_deref(),
        Some("Active")
    );
    assert_eq!(proxy.seen().len(), 1);
}
