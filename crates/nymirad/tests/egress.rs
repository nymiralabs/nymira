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
