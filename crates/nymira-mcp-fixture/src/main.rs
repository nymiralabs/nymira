//! The stack the MCP server's tests run against: the sandbox vault, the real operator behind the
//! fake NEAR AI gateway, and `nymirad`'s real agent routes on loopback TCP and on a Unix socket,
//! each with funded budget notes and signed policies. It prints one JSON line when ready, then
//! answers `report` on stdin with what left the machine, and stops when stdin closes.
//!
//! Not a product: it exists so the MCP package's tests can run the official client against
//! `nymirad` without importing the server. The sandbox, the stack and the policy helpers are
//! the ones `nymirad`'s own tests use, included by path.

#![allow(dead_code)]

#[path = "../../nymirad/tests/support/policy.rs"]
mod policy;
#[path = "../../nymirad/tests/support/socks.rs"]
mod socks;
#[path = "../../nymirad/tests/support/stack.rs"]
mod stack;

use std::net::SocketAddr;
use std::sync::Arc;

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use nymira_fake_near_rpc::{AccessKeySpec, AccountSpec, FakeRpc};
use nymirad::agents::{Registry, Spec, Transport};
use nymirad::agentsock::{self, AgentListener};
use nymirad::guard::{router_for, Side};
use nymirad::rpc::{Rpc, Transport as Net};
use nymirad::secrets::NoteStore;
use nymirad::token::OwnerToken;
use serde_json::json;
use stack::*;
use tokio::io::{AsyncBufReadExt, BufReader};

/// The peer ID the socket listener is told every connection has (a second OS user is not
/// available to a test).
const FAKE_UID: u32 = 4242;

fn pol(call: &str, confirm: &str) -> String {
    format!(
        "allowed_models:\n  - {MODEL}\nmax_lease_usd: 0.5\nmax_call_usd: {call}\nmax_daily_usd: 2\nconfirm_above_usd: {confirm}\nrate_limit_per_min: 30\nrequire_tee: false\n"
    )
}

#[tokio::main]
async fn main() {
    let proxy = socks::FakeSocks::start(socks::Behavior::Passthrough).await;
    let s = build(Net::Socks(proxy.addr.to_string()), false).await;
    // (name, transport, policy): `tight` holds every call for approval.
    let agents = [
        ("main", Transport::Tcp, pol("0.05", "5")),
        ("tight", Transport::Tcp, pol("0.05", "0.00001")),
        ("sock", Transport::Unix, pol("0.05", "5")),
    ];
    let home = s.home.path().to_path_buf();
    let notes = NoteStore::open(&home.join("notes.sealed"), PASS, 10).unwrap();
    for n in 0..agents.len() as u64 {
        deposit(&s.usdc, &s.alice, &s.vault, &notes, n, &s.clock).await;
    }
    let commitments: Vec<String> = notes
        .list()
        .unwrap()
        .into_iter()
        .map(|n| n.commitment.clone())
        .collect();
    drop(notes);
    sync_root(&s.worker, &s.alice, &s.vault).await;

    let registry = Registry::at(&home);
    let registry_for_sign = registry.clone();
    for (i, (name, transport, text)) in agents.iter().enumerate() {
        Spec {
            name: (*name).into(),
            transport: *transport,
            uid: (*transport == Transport::Unix).then_some(FAKE_UID),
            group: (*transport == Transport::Unix).then(|| "agents".to_string()),
            gid: (*transport == Transport::Unix).then(|| unsafe { libc::getegid() }),
            token_file: home.join("tokens").join(format!("{name}.token")),
        }
        .register(&registry, &commitments[i], "owner.test", 1)
        .unwrap();
        policy::give_policy_text(&registry, name, 1, text);
    }

    let owner = Arc::new(OwnerToken::load_or_create(&home.join("owner.token")).unwrap());
    let tcp = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = tcp.local_addr().unwrap().port();
    let app = router_for(
        owner.clone(),
        Some(s.gateway.clone()),
        registry.clone(),
        Side::Tcp,
    );
    tokio::spawn(async move {
        axum::serve(tcp, app.into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap()
    });

    // A short path: a Unix socket address is limited to about 100 bytes.
    let dir = tempfile::Builder::new()
        .prefix("nymira-mcp-")
        .tempdir_in("/tmp")
        .unwrap();
    let bound = agentsock::bind(dir.path(), None).unwrap();
    let listener = AgentListener::new(bound, registry.clone(), Arc::new(|_| Some(FAKE_UID)));
    let sock_app = router_for(owner, Some(s.gateway.clone()), registry, Side::AgentSocket);
    tokio::spawn(agentsock::serve(listener, sock_app));

    // The owner's wallet key, known to a fake chain, so `approve` can sign as the owner does.
    let owner_key = SigningKey::from_bytes(&[1u8; 32]);
    let node = FakeRpc::start().await;
    node.set_account(policy::OWNER, AccountSpec::default());
    node.set_access_keys(
        policy::OWNER,
        vec![AccessKeySpec::full_access(
            &policy::public_key(&owner_key),
            0,
        )],
    );
    s.gateway
        .set_key_rpcs(vec![Rpc::new(node.url(), &Net::Direct).unwrap()]);

    let token_file = |name: &str| home.join("tokens").join(format!("{name}.token"));
    println!(
        "{}",
        json!({
            "ready": true,
            "port": port,
            "socket": dir.path().join(agentsock::SOCKET),
            "model": MODEL,
            "deposit_micro": ONE.to_string(),
            "token_files": {
                "main": token_file("main"),
                "tight": token_file("tight"),
                "sock": token_file("sock"),
            },
            "owner_token_file": home.join("owner.token"),
        })
    );

    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let (word, rest) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
        let reply = match word {
            "report" => json!({
                "leases": s.gateway.records().len(),
                "chats": s.fake.handle.chats().len(),
                "outbound": proxy.seen().len(),
                "bound_classes": s.gateway.records().iter().map(|r| r.bound_class.clone()).collect::<Vec<_>>(),
                "max_tokens": s.fake.handle.chats().iter()
                    .map(|c| serde_json::from_slice::<serde_json::Value>(&c.body).unwrap()["max_tokens"].clone())
                    .collect::<Vec<_>>(),
            }),
            // Set the fake provider's switches, such as `reply`, `latency_ms` or `usage`.
            "switch" => {
                let patch = serde_json::from_str(rest).unwrap();
                s.fake
                    .handle
                    .patch(&patch)
                    .map_or_else(|e| json!({"error": e}), |_| json!({"ok": true}))
            }
            // The owner signs and submits the approval of a held call.
            "approve" => approve(&s, &registry_for_sign, &owner_key, rest).await,
            // Retire open leases and let the operator and nymirad settle them.
            "settle" => {
                s.gateway.reap(true).await;
                for _ in 0..3 {
                    s.op.tick().await;
                    s.clock.advance_secs(11);
                }
                s.op.tick().await;
                s.op.tick().await;
                json!({"settled": s.gateway.settle().await.len()})
            }
            _ => continue,
        };
        println!("{reply}");
    }
}

async fn approve(s: &Stack, registry: &Registry, key: &SigningKey, id: &str) -> serde_json::Value {
    let held = s.gateway.approvals().list(s.gateway.now_secs());
    let Some(h) = held
        .as_array()
        .unwrap()
        .iter()
        .find(|h| h["approval_id"] == id)
    else {
        return json!({"error": "no such held call"});
    };
    let agent = h["agent"].as_str().unwrap();
    let book = registry.load().unwrap();
    let note = book
        .agents
        .iter()
        .find(|a| a.name == agent)
        .unwrap()
        .note
        .clone();
    let recipient = nymirad::approvals::recipient(agent, &note);
    let message = json!({
        "kind": "nymira-approval", "id": h["approval_id"], "request_hash": h["request_hash"],
        "amount_micro": h["amount_micro"], "nonce": h["nonce"],
    })
    .to_string();
    let nonce = [7u8; 32];
    let hash = nymirad::policy::nep413_hash(&message, &nonce, &recipient);
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    let file = json!({
        "account_id": policy::OWNER, "public_key": policy::public_key(key),
        "signature": b64(&key.sign(&hash).to_bytes()), "nonce": b64(&nonce),
        "message": message, "recipient": recipient,
    });
    match s
        .gateway
        .approvals()
        .approve(
            file.to_string().as_bytes(),
            &book,
            &s.gateway.key_rpcs(),
            s.gateway.now_secs(),
        )
        .await
    {
        Ok(id) => json!({"approved": id}),
        Err(_) => json!({"error": "refused"}),
    }
}
