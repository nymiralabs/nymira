//! Egress in Tor mode: every client nymirad builds reaches its service by NAME through the SOCKS
//! proxy, one conversation per identity, and nothing else is contacted. `scripts/egress-check.sh`
//! runs this binary under `strace` and fails on any connect() that is not to loopback, or to a
//! DNS port, which is the network-monitoring half of the check.

mod support;

use std::sync::Arc;

use nymirad::lease::LeaseClient;
use nymirad::quotes::{OneClick, QuoteSource, BASE_URL, SHORTLIST};
use nymirad::rpc::{client, Rpc, Transport};
use nymirad::tor::{Tor, TorState};
use serde_json::json;
use support::socks::{Behavior, FakeSocks};

/// Names a Tor-mode build contacts, with the port each is reached on.
const EXPECTED: &[(&str, u16)] = &[
    ("rpc.mainnet.near.org", 443),
    ("rpc.testnet.near.org", 443),
    ("operator.nymira.example", 443),
    ("cloud-api.near.ai", 443),
    ("1click.chaindefuser.com", 443),
    ("nymira.pages.dev", 443),
    ("nymiralabs.github.io", 443),
];

#[tokio::test]
async fn every_client_goes_out_by_name_through_the_proxy() {
    // A target that accepts and says nothing: the TLS handshake fails after the proxy has seen
    // the name, which is all this test needs.
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sink_addr = sink.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let _ = sink.accept().await;
        }
    });
    let proxy = FakeSocks::start(Behavior::Tunnel(sink_addr)).await;
    let (tor, _state) = Tor::for_tests(proxy.addr, TorState::Ready);
    let transport = Transport::Tor(Arc::new(tor));

    for url in [
        "https://rpc.mainnet.near.org",
        "https://rpc.testnet.near.org",
    ] {
        let _ = Rpc::new(url, &transport)
            .unwrap()
            .call("status", json!({}))
            .await;
    }
    let conversation = client(&transport).unwrap();
    let _ = LeaseClient::new(conversation.clone(), "https://operator.nymira.example")
        .status()
        .await;
    let _ = nymirad::models::fetch_live(&client(&transport).unwrap(), "https://cloud-api.near.ai")
        .await;
    let _ = conversation
        .get("https://cloud-api.near.ai/v1/attestation/report")
        .send()
        .await;
    let _ = nymirad::status::operator_status("https://operator.nymira.example", &transport).await;
    let _ = OneClick::new(BASE_URL, &transport)
        .unwrap()
        .quote(&SHORTLIST[0])
        .await;

    // verify-web without the user's yes: both origins by name through the proxy, and no direct
    // request (the strace half of the check sees no connect() that leaves loopback).
    let plan = nymirad::webverify::Plan {
        key: "",
        profile: "testnet",
        primary: "https://nymira.pages.dev",
        mirror: "https://nymiralabs.github.io/nymira",
    };
    let report = nymirad::webverify::verify(&plan, &transport, false).await;
    assert!(!report.ok() && report.skipped.len() == 1);

    let seen = proxy.seen();
    assert!(!seen.is_empty());
    for s in &seen {
        assert_eq!(
            s.atyp, 3,
            "{} was sent as an address, so this machine resolved it",
            s.host
        );
        assert!(
            EXPECTED.contains(&(s.host.as_str(), s.port)),
            "unexpected destination {}:{}",
            s.host,
            s.port
        );
    }
    for (host, port) in EXPECTED {
        assert!(
            seen.iter().any(|s| s.host == *host && s.port == *port),
            "{host}:{port} was never contacted through the proxy"
        );
    }
}

/// The real attestor fetches evidence through the proxy, by name, on the identity it was given.
/// (Intel's collateral and NVIDIA's service use clients built from the same proxy URL; the SDK's
/// `tests/socks_proxy.rs` covers those two hosts.)
#[tokio::test]
async fn the_attestor_fetches_the_report_through_the_proxy_by_name() {
    use nymirad::attest::{Attestor, Policy, ReportRequest};
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sink_addr = sink.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let _ = sink.accept().await;
        }
    });
    let proxy = FakeSocks::start(Behavior::Tunnel(sink_addr)).await;
    let identity = nymirad::net::identity();
    let socks_url = format!("socks5h://{identity}:x@{}", proxy.addr);
    let policy = Policy {
        version: 1,
        attestation_root_pk: String::new(),
        accepted_measurements: vec!["m".into()],
        deployments: vec![],
        accepted_tcb_statuses: vec!["UpToDate".into()],
        signature: String::new(),
    };
    let http = reqwest::Client::new();
    let result = nymirad::nearai::NearAiAttestor::new(policy)
        .verify_model(&ReportRequest {
            gateway_url: "https://cloud-api.near.ai",
            model: "z-ai/glm-5.3-flash",
            nonce: "unused",
            http: &http,
            socks_url: Some(&socks_url),
        })
        .await;
    assert!(result.is_err(), "the sink serves no report");
    let seen = proxy.seen();
    assert!(!seen.is_empty());
    for s in &seen {
        assert_eq!(
            (s.host.as_str(), s.port, s.atyp),
            ("cloud-api.near.ai", 443, 3)
        );
        assert_eq!(s.user, identity);
    }
}

/// The funding transaction of `deposit` and `recover` leaves through the transport like every
/// other request: its sender carries the transport's client, so the proxy sees the node's name.
/// (It used to be built with the default client and went out from the user's own address, even
/// with Tor down.)
#[tokio::test]
async fn the_funding_transaction_goes_through_the_proxy() {
    let sink = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let sink_addr = sink.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let _ = sink.accept().await;
        }
    });
    let proxy = FakeSocks::start(Behavior::Tunnel(sink_addr)).await;
    let (tor, _state) = Tor::for_tests(proxy.addr, TorState::Ready);
    let transport = Transport::Tor(Arc::new(tor));
    let dir = tempfile::tempdir().unwrap();
    let tx = nymirad::deposit::funding_tx(
        "https://rpc.testnet.near.org",
        "alice.testnet",
        "usdc.testnet".into(),
        ed25519_dalek::SigningKey::from_bytes(&[3u8; 32]),
        nymira_operator::store::Store::open(&dir.path().join("tx.db")).unwrap(),
        Arc::new(nymira_operator::clock::ManualClock::new(1_900_000_000_000)),
        &transport,
    )
    .unwrap();
    let _ = tx
        .send(nymira_operator::tx::TxRequest {
            kind: "deposit",
            method: "ft_transfer_call",
            args: b"{}".to_vec(),
            gas: 1,
            idem_key: None,
        })
        .await;
    let seen = proxy.seen();
    assert!(
        seen.iter()
            .any(|s| s.host == "rpc.testnet.near.org" && s.atyp == 3),
        "the transaction did not reach the proxy by name: {seen:?}"
    );
}
