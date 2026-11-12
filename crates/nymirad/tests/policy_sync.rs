//! A running nymirad takes a newer signed policy from the web hosts, through the proxy, and
//! ignores everything else a host could answer.

mod support;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{http::StatusCode, routing::get, Router};
use ed25519_dalek::SigningKey;
use nymirad::attest::{Policy, Unavailable};
use nymirad::policy_sync::{Follower, FILE};
use nymirad::rpc::Transport;
use support::socks::{Behavior, FakeSocks};

fn key() -> SigningKey {
    SigningKey::from_bytes(&[9; 32])
}

fn signed(version: u32) -> Vec<u8> {
    serde_json::to_vec(
        &Policy {
            version,
            attestation_root_pk: "aa".into(),
            accepted_measurements: vec!["m1".into()],
            deployments: vec![],
            accepted_tcb_statuses: vec!["UpToDate".into()],
            nvidia_pins: vec![],
            signature: String::new(),
        }
        .sign(&key()),
    )
    .unwrap()
}

async fn host(status: StatusCode, body: Vec<u8>) -> SocketAddr {
    let app = Router::new().route(
        "/attestation-policy.json",
        get(move || {
            let body = body.clone();
            async move { (status, body) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

fn follower(home: &std::path::Path, version: u32) -> Arc<Follower> {
    Follower::new(
        home,
        key().verifying_key().to_bytes(),
        version,
        Arc::new(Unavailable),
        Box::new(|_| Ok(Arc::new(Unavailable))),
    )
}

#[tokio::test]
async fn a_newer_policy_from_a_host_is_taken_through_the_proxy() {
    let dir = tempfile::tempdir().unwrap();
    let proxy = FakeSocks::start(Behavior::Tunnel(host(StatusCode::OK, signed(9)).await)).await;
    let f = follower(dir.path(), 6);
    let got = f
        .fetch_once(
            &Transport::Socks(proxy.addr.to_string()),
            &["http://nymira.pages.dev"],
        )
        .await;
    assert_eq!(got, Ok(Some(9)));
    assert_eq!(f.version(), 9);
    assert_eq!(std::fs::read(dir.path().join(FILE)).unwrap(), signed(9));
    // The host was reached by name through the proxy, not by address.
    assert!(proxy.seen().iter().any(|s| s.host.contains("nymira.pages.dev")));
}

#[tokio::test]
async fn a_host_that_serves_junk_an_old_policy_or_an_error_changes_nothing() {
    let dir = tempfile::tempdir().unwrap();
    for (status, body) in [
        (StatusCode::OK, b"<html>captive portal</html>".to_vec()),
        (StatusCode::OK, signed(6)),
        (StatusCode::OK, vec![b'x'; 300 * 1024]),
        (StatusCode::NOT_FOUND, vec![]),
    ] {
        let proxy = FakeSocks::start(Behavior::Tunnel(host(status, body).await)).await;
        let f = follower(dir.path(), 6);
        let _ = f
            .fetch_once(
                &Transport::Socks(proxy.addr.to_string()),
                &["http://nymira.pages.dev"],
            )
            .await;
        assert_eq!(f.version(), 6);
    }
    assert!(!dir.path().join(FILE).exists());
}

#[tokio::test]
async fn the_second_host_is_tried_when_the_first_fails() {
    let dir = tempfile::tempdir().unwrap();
    let proxy = FakeSocks::start(Behavior::Tunnel(host(StatusCode::OK, signed(9)).await)).await;
    let f = follower(dir.path(), 6);
    // Both names go to the same fake backend, so the first one fails on a refused SOCKS name.
    let bad = FakeSocks::start(Behavior::Refuse(2)).await;
    assert!(f
        .fetch_once(
            &Transport::Socks(bad.addr.to_string()),
            &["http://a.example"]
        )
        .await
        .is_err());
    let got = f
        .fetch_once(
            &Transport::Socks(proxy.addr.to_string()),
            &["http://nymira.pages.dev"],
        )
        .await;
    assert_eq!(got, Ok(Some(9)));
}
