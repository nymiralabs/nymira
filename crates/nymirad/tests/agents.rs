//! Budget notes and agent tokens: the registry, the two listeners, the scope that keeps each
//! token on its own note, and the refusals, against the mock stack.
//!
//! Tests that need a second OS user cannot make one in CI. The peer-UID check is exercised with
//! an injected UID source instead (see `agent_socket_checks_the_peer_on_every_connection`).

#[path = "support/socks.rs"]
mod socks;
#[path = "support/stack.rs"]
mod stack;

use std::net::SocketAddr;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use ed25519_dalek::SigningKey;
use near_workspaces::types::{AccessKey, PublicKey};
use nymira_operator::store::Store;
use nymira_operator::tx::{NearTx, TxConfig};
use nymirad::agents::{self, Registry, Spec, Transport};
use nymirad::agentsock::{self, AgentListener};
use nymirad::backup;
use nymirad::deposit::{Confirm, Ctx, DepositError, Summary};
use nymirad::guard::{router_for, Side};
use nymirad::mode::Mode;
use nymirad::pins::{read_live, Tripwire};
use nymirad::rpc::{Rpc, Transport as Net};
use nymirad::secrets::NoteStore;
use nymirad::token::OwnerToken;
use serde_json::{json, Value};
use stack::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower::ServiceExt;

const OWNER_UID: u32 = 1000;
const AGENT_UID: u32 = 4242;

fn chat_body(text: &str) -> Value {
    json!({"model": MODEL, "stream": false, "max_tokens": 50, "messages": [{"role": "user", "content": text}]})
}

/// One request through a router, as if it arrived from `peer`.
async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    token: &str,
    extra: &[(&str, &str)],
    peer: &str,
    body: Value,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost:8788")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let mut request = b.body(Body::from(body.to_string())).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    let reply = app.clone().oneshot(request).await.unwrap();
    let status = reply.status();
    let bytes = axum::body::to_bytes(reply.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

const LOCAL: &str = "127.0.0.1:50000";

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

/// A plain HTTP/1.1 request over a Unix socket. `None` when the server closed the connection
/// without a reply.
async fn raw(
    sock: &Path,
    method: &str,
    path: &str,
    token: &str,
    body: &str,
) -> Option<(u16, Value)> {
    let mut stream = tokio::net::UnixStream::connect(sock).await.ok()?;
    let text = format!(
        "{method} {path} HTTP/1.1\r\nhost: localhost\r\nauthorization: Bearer {token}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(text.as_bytes()).await.ok()?;
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(60), stream.read_to_end(&mut out)).await;
    let reply = String::from_utf8_lossy(&out).into_owned();
    let status = reply.split(' ').nth(1)?.parse().ok()?;
    let json = reply
        .split_once("\r\n\r\n")
        .and_then(|(_, b)| {
            // The body may arrive chunked; take the first JSON object in it.
            let start = b.find('{')?;
            let end = b.rfind('}')?;
            serde_json::from_str(&b[start..=end]).ok()
        })
        .unwrap_or(Value::Null);
    Some((status, json))
}

fn tcp_agent(registry: &Registry, dir: &Path, name: &str, note: &str) -> String {
    let spec = Spec {
        name: name.into(),
        transport: Transport::Tcp,
        uid: None,
        group: None,
        gid: None,
        token_file: dir.join(format!("{name}.token")),
    };
    spec.register(registry, note, "owner.test", 1).unwrap();
    std::fs::read_to_string(&spec.token_file).unwrap()
}

fn unix_agent(registry: &Registry, dir: &Path, name: &str, note: &str, uid: u32) -> String {
    let spec = Spec {
        name: name.into(),
        transport: Transport::Unix,
        uid: Some(uid),
        group: Some("agents".into()),
        gid: Some(unsafe { libc::getegid() }),
        token_file: dir.join(format!("{name}.token")),
    };
    spec.register(registry, note, "owner.test", 1).unwrap();
    std::fs::read_to_string(&spec.token_file).unwrap()
}

/// A group the test user is not in, if there is one (a non-root user cannot give a file to it).
fn foreign_gid() -> Option<u32> {
    if unsafe { libc::geteuid() } == 0 {
        return None;
    }
    let mut mine = vec![0 as libc::gid_t; 256];
    let n = unsafe { libc::getgroups(256, mine.as_mut_ptr()) }.max(0) as usize;
    mine.truncate(n);
    mine.push(unsafe { libc::getegid() });
    (1..100_000u32).find(|g| !mine.contains(g))
}

// ---- the registry and the token files ----

#[test]
fn the_registry_is_owner_only_and_holds_hashes_never_tokens() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Registry::at(dir.path());
    let token = tcp_agent(&registry, dir.path(), "cursor", "0xaa");
    assert!(token.starts_with("nyma_") && token.len() == 5 + 64);
    let file = dir.path().join(agents::FILE);
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let text = std::fs::read_to_string(&file).unwrap();
    assert!(!text.contains(&token), "only the hash is stored");
    let book = registry.load().unwrap();
    let a = &book.agents[0];
    assert_eq!(
        (a.name.as_str(), a.note.as_str(), a.owner_account.as_str()),
        ("cursor", "0xaa", "owner.test")
    );
    assert_eq!(a.token_hash, agents::hash_token(&token));
    assert_eq!(a.policy_version, None);
    assert_eq!(a.transport, Transport::Tcp);
    // A tcp token file is private to the owner.
    assert_eq!(
        std::fs::metadata(dir.path().join("cursor.token"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    // A second agent of the same name is refused and changes nothing.
    let spec = Spec {
        name: "cursor".into(),
        transport: Transport::Tcp,
        uid: None,
        group: None,
        gid: None,
        token_file: dir.path().join("again.token"),
    };
    assert!(spec.register(&registry, "0xbb", "owner.test", 1).is_err());
    assert_eq!(registry.load().unwrap().agents.len(), 1);
    assert!(!dir.path().join("again.token").exists());
}

#[test]
fn a_socket_token_file_is_0640_in_the_agent_group_or_waits_for_a_sudo_step() {
    let dir = tempfile::tempdir().unwrap();
    let registry = Registry::at(dir.path());
    // The owner is in the group: the file gets it and the agent's group can read it.
    unix_agent(&registry, dir.path(), "inside", "0x01", AGENT_UID);
    let meta = std::fs::metadata(dir.path().join("inside.token")).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o640);
    assert_eq!(meta.gid(), unsafe { libc::getegid() });
    assert_eq!(meta.uid(), unsafe { libc::geteuid() });
    // The owner is not in the group: the file stays 0640 in the owner's own group and the
    // command prints the sudo step.
    let Some(gid) = foreign_gid() else {
        eprintln!("skipped the foreign-group half: running as root");
        return;
    };
    let spec = Spec {
        name: "outside".into(),
        transport: Transport::Unix,
        uid: Some(AGENT_UID + 1),
        group: Some("other".into()),
        gid: Some(gid),
        token_file: dir.path().join("outside.token"),
    };
    let made = spec.register(&Registry::at(dir.path()), "0x02", "owner.test", 1);
    // Another group name than the registry's socket group is refused outright.
    assert!(made.is_err(), "every socket agent shares one group");
    let other = tempfile::tempdir().unwrap();
    let fresh = Registry::at(other.path());
    let spec = Spec {
        token_file: other.path().join("outside.token"),
        ..spec
    };
    let made = spec.register(&fresh, "0x02", "owner.test", 1).unwrap();
    assert!(
        made.notes.iter().any(|n| n.contains("sudo chgrp other ")),
        "{:?}",
        made.notes
    );
    let meta = std::fs::metadata(other.path().join("outside.token")).unwrap();
    assert_eq!(meta.permissions().mode() & 0o777, 0o640);
    assert_ne!(meta.gid(), gid, "the agent group has not been applied yet");
}

#[test]
fn the_agent_socket_is_reachable_only_through_the_agent_group() {
    let dir = tempfile::tempdir().unwrap();
    let sock_dir = dir.path().join("agent");
    let gid = unsafe { libc::getegid() };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();
    let _listener = agentsock::bind(&sock_dir, Some(gid)).unwrap();
    let d = std::fs::metadata(&sock_dir).unwrap();
    let s = std::fs::metadata(sock_dir.join(agentsock::SOCKET)).unwrap();
    // Owner and group only; "other" has no bit on the directory or the socket.
    assert_eq!(d.permissions().mode() & 0o777, 0o750);
    assert_eq!(s.permissions().mode() & 0o777, 0o660);
    assert_eq!((d.gid(), s.gid()), (gid, gid));
    // A group the owner cannot give away: the bind says which sudo command to run.
    if let Some(foreign) = foreign_gid() {
        let err = agentsock::bind(&dir.path().join("other"), Some(foreign)).unwrap_err();
        assert!(err.to_string().contains("sudo chgrp"), "{err}");
    }
}

// ---- the listeners, without a gateway ----

fn owner_token(home: &Path) -> (Arc<OwnerToken>, String) {
    let path = home.join("owner.token");
    let token = OwnerToken::load_or_create(&path).unwrap();
    (Arc::new(token), std::fs::read_to_string(path).unwrap())
}

#[tokio::test]
async fn an_agent_token_reaches_agent_routes_only_and_only_on_its_transport() {
    let home = tempfile::tempdir().unwrap();
    let registry = Registry::at(home.path());
    let (owner, owner_text) = owner_token(home.path());
    let t = tcp_agent(&registry, home.path(), "t", "0x01");
    let u = unix_agent(&registry, home.path(), "u", "0x02", AGENT_UID);
    registry.set_policy_version("t", Some(1)).unwrap();
    registry.set_policy_version("u", Some(1)).unwrap();
    let tcp = router_for(owner.clone(), None, registry.clone(), Side::Tcp);
    let sock = router_for(owner, None, registry.clone(), Side::AgentSocket);

    // On TCP: the tcp token reaches models, never an owner or unknown route; the socket token
    // is refused; the owner token still reaches only chat and models.
    let (s, _) = call(&tcp, "GET", "/v1/models", &t, &[], LOCAL, json!({})).await;
    assert_eq!(s, StatusCode::OK);
    for (m, p) in [
        ("POST", "/v1/deposit"),
        ("POST", "/v1/close"),
        ("GET", "/v1/notes"),
        ("GET", "/v1/chat/completions"),
        ("POST", "/v1/models"),
        ("GET", "/v1/status"),
    ] {
        let (s, body) = call(&tcp, m, p, &t, &[], LOCAL, json!({})).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{m} {p}");
        assert_eq!(code(&body), "agent_route_forbidden");
    }
    let (s, body) = call(&tcp, "GET", "/v1/models", &u, &[], LOCAL, json!({})).await;
    assert_eq!((s, code(&body)), (StatusCode::FORBIDDEN, "wrong_transport"));
    let (s, _) = call(
        &tcp,
        "GET",
        "/v1/models",
        &owner_text,
        &[],
        LOCAL,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = call(
        &tcp,
        "POST",
        "/v1/close",
        &owner_text,
        &[],
        LOCAL,
        json!({}),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // On the socket router: the tcp token and the owner token are refused; the socket token
    // needs its own user ID, and only agent routes answer.
    let on_sock = |token: String, uid: u32, m: &'static str, p: &'static str| {
        let sock = sock.clone();
        async move {
            let mut r = Request::builder()
                .method(m)
                .uri(p)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap();
            r.extensions_mut()
                .insert(ConnectInfo(agentsock::PeerUid(uid)));
            let reply = sock.oneshot(r).await.unwrap();
            let status = reply.status();
            let bytes = axum::body::to_bytes(reply.into_body(), usize::MAX)
                .await
                .unwrap();
            (
                status,
                serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null),
            )
        }
    };
    let (s, b) = on_sock(t.clone(), AGENT_UID, "GET", "/v1/models").await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, "wrong_transport"));
    let (s, b) = on_sock(owner_text.clone(), AGENT_UID, "GET", "/v1/models").await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, "agent_routes_only"));
    let (s, _) = on_sock(u.clone(), AGENT_UID, "GET", "/v1/models").await;
    assert_eq!(s, StatusCode::OK);
    let (s, b) = on_sock(u.clone(), AGENT_UID + 1, "GET", "/v1/models").await;
    assert_eq!((s, code(&b)), (StatusCode::FORBIDDEN, "wrong_transport"));
    let (s, b) = on_sock(u, AGENT_UID, "POST", "/v1/deposit").await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::FORBIDDEN, "agent_route_forbidden")
    );
}

#[tokio::test]
async fn agent_requests_on_tcp_pass_the_loopback_checks_and_a_tailnet_peer_is_refused() {
    let home = tempfile::tempdir().unwrap();
    let registry = Registry::at(home.path());
    let (owner, _) = owner_token(home.path());
    let t = tcp_agent(&registry, home.path(), "t", "0x01");
    registry.set_policy_version("t", Some(1)).unwrap();
    let app = router_for(owner, None, registry, Side::Tcp);
    for host in ["localhost:8788", "127.0.0.1:8788", "[::1]:8788"] {
        let mut r = Request::builder()
            .uri("/v1/models")
            .header("host", host)
            .header("authorization", format!("Bearer {t}"))
            .body(Body::empty())
            .unwrap();
        r.extensions_mut()
            .insert(ConnectInfo(LOCAL.parse::<SocketAddr>().unwrap()));
        assert_eq!(
            app.clone().oneshot(r).await.unwrap().status(),
            StatusCode::OK,
            "{host}"
        );
    }
    let refused =
        |extra: Vec<(&'static str, &'static str)>, host: &'static str, peer: &'static str| {
            let (app, t) = (app.clone(), t.clone());
            async move {
                let mut b = Request::builder()
                    .uri("/v1/models")
                    .header("host", host)
                    .header("authorization", format!("Bearer {t}"));
                for (k, v) in extra {
                    b = b.header(k, v);
                }
                let mut r = b.body(Body::empty()).unwrap();
                r.extensions_mut()
                    .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
                app.oneshot(r).await.unwrap().status()
            }
        };
    let f = StatusCode::FORBIDDEN;
    assert_eq!(refused(vec![], "evil.test:8788", LOCAL).await, f);
    assert_eq!(
        refused(
            vec![("origin", "http://localhost:8788")],
            "localhost:8788",
            LOCAL
        )
        .await,
        f
    );
    assert_eq!(
        refused(
            vec![("sec-fetch-site", "cross-site")],
            "localhost:8788",
            LOCAL
        )
        .await,
        f
    );
    // A direct connection over the tailnet (or any non-loopback address) is refused.
    for peer in [
        "100.64.0.7:4000",
        "192.168.1.5:4000",
        "[fd7a:115c:a1e0::1]:4000",
    ] {
        assert_eq!(refused(vec![], "localhost:8788", peer).await, f, "{peer}");
    }
    // The forward from an SSH host arrives from loopback with Host localhost:8788 and passes.
    assert_eq!(
        refused(vec![], "localhost:8788", "[::1]:5000").await,
        StatusCode::OK
    );
    // And the listener itself binds loopback only.
    let src = include_str!("../src/guard.rs");
    assert!(src.contains("TcpListener::bind((\"127.0.0.1\", port))"));
}

#[tokio::test]
async fn disabling_a_token_stops_the_next_request_and_a_broken_registry_stops_all() {
    let home = tempfile::tempdir().unwrap();
    let registry = Registry::at(home.path());
    let (owner, owner_text) = owner_token(home.path());
    let t = tcp_agent(&registry, home.path(), "t", "0x01");
    registry.set_policy_version("t", Some(1)).unwrap();
    let app = router_for(owner, None, registry.clone(), Side::Tcp);
    let (s, _) = call(&app, "GET", "/v1/models", &t, &[], LOCAL, json!({})).await;
    assert_eq!(s, StatusCode::OK);
    registry.disable("t").unwrap();
    let (s, b) = call(&app, "GET", "/v1/models", &t, &[], LOCAL, json!({})).await;
    assert_eq!((s, code(&b)), (StatusCode::UNAUTHORIZED, "token_disabled"));
    // A registry the server cannot read fails closed, for the owner too: the owner's scope
    // depends on knowing which notes are budget notes.
    std::fs::write(home.path().join(agents::FILE), b"not json").unwrap();
    let (s, b) = call(
        &app,
        "GET",
        "/v1/models",
        &owner_text,
        &[],
        LOCAL,
        json!({}),
    )
    .await;
    assert_eq!(
        (s, code(&b)),
        (StatusCode::SERVICE_UNAVAILABLE, "registry_unreadable")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_socket_checks_the_peer_on_every_connection() {
    let home = tempfile::tempdir().unwrap();
    let registry = Registry::at(home.path());
    let (owner, owner_text) = owner_token(home.path());
    let u = unix_agent(&registry, home.path(), "u", "0x02", AGENT_UID);
    let t = tcp_agent(&registry, home.path(), "t", "0x01");
    registry.set_policy_version("u", Some(1)).unwrap();
    registry.set_policy_version("t", Some(1)).unwrap();
    let sock_dir = home.path().join("sock");
    let listener = agentsock::bind(&sock_dir, Some(unsafe { libc::getegid() })).unwrap();
    // Without a second OS user in CI, the peer's UID comes from this cell.
    let peer = Arc::new(AtomicU32::new(AGENT_UID));
    let source = {
        let peer = peer.clone();
        Arc::new(move |_: &tokio::net::UnixStream| Some(peer.load(Ordering::SeqCst)))
    };
    let app = router_for(owner, None, registry.clone(), Side::AgentSocket);
    let listener = AgentListener::with_owner(listener, registry.clone(), OWNER_UID, source);
    tokio::spawn(agentsock::serve(listener, app));
    let path = sock_dir.join(agentsock::SOCKET);

    let ok = raw(&path, "GET", "/v1/models", &u, "").await.unwrap();
    assert_eq!(ok.0, 200);
    // The owner's own OS user is refused: the connection is closed with no reply, even with a
    // good agent token.
    peer.store(OWNER_UID, Ordering::SeqCst);
    assert!(raw(&path, "GET", "/v1/models", &u, "").await.is_none());
    // So is a user no socket agent is registered for.
    peer.store(AGENT_UID + 7, Ordering::SeqCst);
    assert!(raw(&path, "GET", "/v1/models", &u, "").await.is_none());
    // A registered user with another agent's token: the token is bound to its own user.
    let other = unix_agent(&registry, home.path(), "w", "0x03", AGENT_UID + 7);
    registry.set_policy_version("w", Some(1)).unwrap();
    let (s, b) = raw(&path, "GET", "/v1/models", &u, "").await.unwrap();
    assert_eq!((s, code(&b)), (403, "wrong_transport"));
    assert_eq!(
        raw(&path, "GET", "/v1/models", &other, "").await.unwrap().0,
        200
    );
    // Every connection is checked, not only the first: back to the first user, it works again.
    peer.store(AGENT_UID, Ordering::SeqCst);
    assert_eq!(
        raw(&path, "GET", "/v1/models", &u, "").await.unwrap().0,
        200
    );
    // The owner token and a tcp token do not work here either.
    let (s, b) = raw(&path, "GET", "/v1/models", &owner_text, "")
        .await
        .unwrap();
    assert_eq!((s, code(&b)), (403, "agent_routes_only"));
    let (s, b) = raw(&path, "GET", "/v1/models", &t, "").await.unwrap();
    assert_eq!((s, code(&b)), (403, "wrong_transport"));
    // Disabled at once.
    registry.disable("u").unwrap();
    let (s, b) = raw(&path, "GET", "/v1/models", &u, "").await.unwrap();
    assert_eq!((s, code(&b)), (401, "token_disabled"));
}

// ---- the mock stack ----

fn commitments(home: &Path) -> Vec<String> {
    NoteStore::open(&home.join("notes.sealed"), PASS, 10)
        .unwrap()
        .list()
        .unwrap()
        .into_iter()
        .map(|n| n.commitment.clone())
        .collect()
}

/// Three 1 USDC notes in the vault, each for one agent: a and b on TCP, c on the socket.
async fn three_budget_notes(s: &Stack) -> (Registry, [String; 3]) {
    let notes = NoteStore::open(&s.home.path().join("notes.sealed"), PASS, 10).unwrap();
    for n in 0..3 {
        deposit(&s.usdc, &s.alice, &s.vault, &notes, n, &s.clock).await;
    }
    drop(notes);
    sync_root(&s.worker, &s.alice, &s.vault).await;
    let c = commitments(s.home.path());
    let registry = Registry::at(s.home.path());
    let dir = s.home.path().join("tokens");
    tcp_agent(&registry, &dir, "a", &c[0]);
    tcp_agent(&registry, &dir, "b", &c[1]);
    unix_agent(&registry, &dir, "c", &c[2], AGENT_UID);
    (registry, [c[0].clone(), c[1].clone(), c[2].clone()])
}

fn token_of(home: &Path, name: &str) -> String {
    std::fs::read_to_string(home.join("tokens").join(format!("{name}.token"))).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_draws_on_its_own_note_only_and_policy_missing_sends_nothing_to_the_operator() {
    let proxy = socks::FakeSocks::start(socks::Behavior::Passthrough).await;
    let s = build(Net::Socks(proxy.addr.to_string()), false).await;
    let (registry, _) = three_budget_notes(&s).await;
    let home = s.home.path();
    let (owner, owner_text) = owner_token(home);
    let app = router_for(
        owner.clone(),
        Some(s.gateway.clone()),
        registry.clone(),
        Side::Tcp,
    );
    let (a, b, c) = (
        token_of(home, "a"),
        token_of(home, "b"),
        token_of(home, "c"),
    );

    // No signed policy yet: 402 policy_missing on chat and on models, and nothing goes out.
    for (m, p, body) in [
        ("POST", "/v1/chat/completions", chat_body("hi")),
        ("GET", "/v1/models", json!({})),
    ] {
        let (status, reply) = call(&app, m, p, &a, &[], LOCAL, body).await;
        assert_eq!(
            (status, code(&reply)),
            (StatusCode::PAYMENT_REQUIRED, "policy_missing")
        );
    }
    assert!(
        proxy.seen().is_empty(),
        "no connection to the operator or the attestation host"
    );
    assert!(s.gateway.records().is_empty());
    assert!(s.fake.handle.chats().is_empty());

    for name in ["a", "b", "c"] {
        registry.set_policy_version(name, Some(1)).unwrap();
    }

    // The owner token never selects a budget note: with only budget notes in the vault the
    // owner's chat has no note to draw on.
    let (status, reply) = call(
        &app,
        "POST",
        "/v1/chat/completions",
        &owner_text,
        &[],
        LOCAL,
        chat_body("owner"),
    )
    .await;
    assert_eq!(status, StatusCode::PAYMENT_REQUIRED);
    assert_eq!(code(&reply), "no_note_can_cover_request");
    assert!(s.gateway.records().is_empty());

    // The agent token works as an OpenAI API key; agent a leases its own note (0) and b its own (1).
    let (status, reply) = call(
        &app,
        "POST",
        "/v1/chat/completions",
        &a,
        &[],
        LOCAL,
        chat_body("from a"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(
        reply["choices"][0]["message"]["content"],
        "fake reply to: from a"
    );
    let notes_leased = || {
        let mut ids: Vec<u32> = s.gateway.records().iter().map(|r| r.note_id).collect();
        ids.sort();
        ids
    };
    assert_eq!(notes_leased(), vec![0]);
    let (status, _) = call(
        &app,
        "POST",
        "/v1/chat/completions",
        &b,
        &[],
        LOCAL,
        chat_body("from b"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        notes_leased(),
        vec![0, 1],
        "b opened its own lease; a's note was not used for it"
    );
    // a again: it keeps using its own note, no third lease.
    let (status, again) = call(
        &app,
        "POST",
        "/v1/chat/completions",
        &a,
        &[],
        LOCAL,
        chat_body("a again"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{again}");
    assert_eq!(notes_leased(), vec![0, 1]);

    // The model list tells an agent nothing about notes, and is the same list the owner gets.
    let (_, models_a) = call(&app, "GET", "/v1/models", &a, &[], LOCAL, json!({})).await;
    let (_, models_owner) = call(
        &app,
        "GET",
        "/v1/models",
        &owner_text,
        &[],
        LOCAL,
        json!({}),
    )
    .await;
    assert_eq!(models_a, models_owner);
    let text = models_a.to_string();
    for c in commitments(home) {
        assert!(!text.contains(&c));
    }
    assert!(!text.contains("note"));

    // Agent c on the socket leases its own note (2) and no other.
    let sock_dir = home.join("sock");
    let listener = agentsock::bind(&sock_dir, None).unwrap();
    let source: agentsock::UidSource = Arc::new(|_| Some(AGENT_UID));
    let listener = AgentListener::with_owner(listener, registry.clone(), OWNER_UID, source);
    let sock_app = router_for(
        owner,
        Some(s.gateway.clone()),
        registry.clone(),
        Side::AgentSocket,
    );
    tokio::spawn(agentsock::serve(listener, sock_app));
    let path = sock_dir.join(agentsock::SOCKET);
    let (status, reply) = raw(
        &path,
        "POST",
        "/v1/chat/completions",
        &c,
        &chat_body("from c").to_string(),
    )
    .await
    .unwrap();
    assert_eq!(status, 200, "{reply}");
    assert_eq!(notes_leased(), vec![0, 1, 2]);
    // The tcp token is refused on the socket.
    let (status, reply) = raw(&path, "GET", "/v1/models", &a, "").await.unwrap();
    assert_eq!((status, code(&reply)), (403, "wrong_transport"));

    // Disabling a stops it at once; b is untouched.
    registry.disable("a").unwrap();
    let (status, reply) = call(
        &app,
        "POST",
        "/v1/chat/completions",
        &a,
        &[],
        LOCAL,
        chat_body("late"),
    )
    .await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::UNAUTHORIZED, "token_disabled")
    );
    let (status, _) = call(
        &app,
        "POST",
        "/v1/chat/completions",
        &b,
        &[],
        LOCAL,
        chat_body("still b"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(notes_leased(), vec![0, 1, 2]);
}

struct Yes;

impl Confirm for Yes {
    fn confirm(&self, _: &Summary) -> bool {
        true
    }
}

async fn deposit_ctx(s: &Stack) -> Ctx {
    let key = SigningKey::from_bytes(&[21u8; 32]);
    let public: PublicKey = format!(
        "ed25519:{}",
        bs58::encode(key.verifying_key().as_bytes()).into_string()
    )
    .parse()
    .unwrap();
    s.alice
        .batch(s.alice.id())
        .add_key(public, AccessKey::full_access())
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    s.worker.fast_forward(5).await.unwrap();
    let rpc = Rpc::new(s.worker.rpc_addr(), &Net::Direct).unwrap();
    let pins = read_live(&rpc, s.vault.id().as_str()).await.unwrap();
    let tx_store = Store::open(&s.home.path().join("deposit-tx.db")).unwrap();
    let tx = NearTx::new(
        s.worker.rpc_addr(),
        s.alice.id().as_str(),
        s.usdc.contract.id().as_str(),
        key,
        tx_store.clone(),
        s.clock.clone(),
    )
    .with_deposit(1)
    .with_config(TxConfig {
        send_timeout: Duration::from_secs(60),
        retry_delay: Duration::from_millis(200),
        max_attempts: 5,
    });
    let (_, recipient) = backup::generate_identity();
    Ctx {
        mode: Mode::Mainnet,
        rpc,
        pins,
        tripwire: Arc::new(Tripwire::default()),
        notes: NoteStore::open(&s.home.path().join("notes.sealed"), PASS, 10).unwrap(),
        tx,
        tx_store,
        sender: s.alice.id().to_string(),
        clock: s.clock.clone(),
        backup: Some(backup::Target::new(
            backup::parse_recipient(&recipient).unwrap(),
            vec![s.home.path().join("backup")],
        )),
        calendar_dir: s.home.path().join("cal"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_budget_note_is_a_normal_deposit_paid_out_to_the_owner_and_registered_before_the_transfer(
) {
    let s = build(Net::Direct, false).await;
    let ctx = deposit_ctx(&s).await;
    let registry = Registry::at(s.home.path());
    let spec = Spec {
        name: "cursor".into(),
        transport: Transport::Tcp,
        uid: None,
        group: None,
        gid: None,
        token_file: s.home.path().join("agents").join("cursor.token"),
    };
    let owner = ctx.sender.clone();
    let before = s.usdc.balance_of(s.alice.id()).await;
    let register = |commitment: &str| {
        spec.register(&registry, commitment, &owner, 5)?;
        Ok(())
    };
    let accepted = ctx
        .deposit_with(ONE, Some(&owner), &Yes, Some(&register))
        .await
        .unwrap();
    assert_eq!(
        s.usdc.balance_of(s.alice.id()).await,
        before - ONE - FEE,
        "the normal deposit"
    );
    let note = ctx.notes.list().unwrap().remove(0);
    assert_eq!(
        note.payout,
        s.alice.id().as_str(),
        "payout is the owner's own account"
    );
    let book = registry.load().unwrap();
    assert_eq!(book.agents[0].note, note.commitment);
    assert_eq!(book.agents[0].owner_account, s.alice.id().as_str());
    // On chain it is a note like any other of its denomination, paying out to the owner.
    let record = &s
        .vault
        .view("note_records")
        .args_json(json!({ "from": accepted.note_id, "limit": 1 }))
        .await
        .unwrap()
        .json::<Value>()
        .unwrap()[0];
    assert_eq!(record["deposit_amount"], ONE.to_string());
    let token = std::fs::read_to_string(&spec.token_file).unwrap();
    assert_eq!(book.agents[0].token_hash, agents::hash_token(&token));

    // A name that is taken stops the deposit before the transfer: no money moves, no note.
    let second = Spec {
        token_file: s.home.path().join("agents").join("second.token"),
        ..spec
    };
    let balance = s.usdc.balance_of(s.alice.id()).await;
    let again = |commitment: &str| {
        second.register(&registry, commitment, &owner, 6)?;
        Ok(())
    };
    let refused = ctx
        .deposit_with(ONE, Some(&owner), &Yes, Some(&again))
        .await;
    assert!(
        matches!(refused, Err(DepositError::Budget(_))),
        "{refused:?}"
    );
    assert_eq!(s.usdc.balance_of(s.alice.id()).await, balance);
    assert_eq!(registry.load().unwrap().agents.len(), 1);
}

// ---- the binary ----

fn run(home: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_nymirad"))
        .arg("--home")
        .arg(home)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .unwrap()
}

fn me() -> String {
    // SAFETY: getpwuid returns null or a pointer valid until the next call; read at once.
    let p = unsafe { libc::getpwuid(libc::geteuid()) };
    assert!(!p.is_null());
    unsafe { std::ffi::CStr::from_ptr((*p).pw_name) }
        .to_string_lossy()
        .into_owned()
}

#[test]
fn the_owner_commands_list_and_disable_and_refuse_an_agent_that_is_the_owner() {
    let home = tempfile::tempdir().unwrap();
    let registry = Registry::at(home.path());
    tcp_agent(&registry, home.path(), "cursor", "0xaa");
    let out = run(home.path(), &["agent", "list"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && text.contains("cursor: Tcp token, active, policy none"),
        "{text}"
    );
    let out = run(home.path(), &["agent", "disable", "--name", "cursor"]);
    assert!(out.status.success());
    assert!(registry.load().unwrap().agents[0].disabled);
    let out = run(home.path(), &["agent", "disable", "--name", "nobody"]);
    assert!(!out.status.success());
    // An agent that runs as the owner's own OS user is not given the owner's nymirad.
    let own = me();
    let out = run(
        home.path(),
        &[
            "agent",
            "create",
            "--name",
            "mine",
            "--transport",
            "unix",
            "--agent-user",
            &own,
            "--token-file",
            "/tmp/never-written.token",
            "--key-file",
            "/nonexistent",
            "--amount",
            "1000000",
        ],
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains("dedicated nymirad instance"),
        "{err}"
    );
    // Intents funding is not offered for a budget note.
    let out = run(
        home.path(),
        &[
            "agent",
            "create",
            "--name",
            "x",
            "--transport",
            "tcp",
            "--source",
            "list",
        ],
    );
    assert!(!out.status.success());
}

/// `nymirad` with no controlling terminal, so the confirmation is read from `input`.
fn cli(home: &Path, args: &[String], input: &str) -> std::process::Output {
    use std::io::Write;
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_nymirad"));
    cmd.env("NYMIRA_MODE", "mainnet")
        .arg("--home")
        .arg(home)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // SAFETY: setsid is async-signal-safe and has no preconditions here.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

async fn blocking_cli(home: &Path, args: Vec<String>, input: &'static str) -> std::process::Output {
    let home = home.to_path_buf();
    tokio::task::spawn_blocking(move || cli(&home, &args, input))
        .await
        .unwrap()
}

async fn agent_request(port: u16, token: &str) -> (u16, Value) {
    // The listener checks the Host against 8788 whatever port a test build binds.
    let client = reqwest::Client::new();
    let mut tries = 0;
    let reply = loop {
        match client
            .get(format!("http://127.0.0.1:{port}/v1/models"))
            .header("host", "localhost:8788")
            .bearer_auth(token)
            .send()
            .await
        {
            Ok(r) => break r,
            Err(e) if tries > 100 => panic!("{e}"),
            Err(_) => {
                tries += 1;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };
    let status = reply.status().as_u16();
    (status, reply.json().await.unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_create_through_the_binary_funds_an_ordinary_looking_note_and_disable_cuts_the_token()
{
    use std::io::{BufRead, BufReader};
    use std::os::unix::process::CommandExt;

    // Note 0 is an ordinary deposit; the agent's note is note 1.
    let s = stack::stack(Net::Direct).await;
    let seed = [22u8; 32];
    let key = SigningKey::from_bytes(&seed);
    let public: PublicKey = format!(
        "ed25519:{}",
        bs58::encode(key.verifying_key().as_bytes()).into_string()
    )
    .parse()
    .unwrap();
    s.alice
        .batch(s.alice.id())
        .add_key(public, AccessKey::full_access())
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    s.worker.fast_forward(5).await.unwrap();

    let home = tempfile::tempdir().unwrap();
    let files = tempfile::tempdir().unwrap();
    let write = |name: &str, text: &str| {
        let path = files.path().join(name);
        std::fs::write(&path, text).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        path.display().to_string()
    };
    let key_file = write("key", &hex::encode(seed));
    let pass_file = write("pass", PASS);
    let backup_place = tempfile::tempdir().unwrap();
    let (_, recipient) = backup::generate_identity();
    nymirad::config::Config {
        rpc_url: s.worker.rpc_addr(),
        operator_url: s.parts.operator_url.clone(),
        account: s.alice.id().to_string(),
        direct: true,
        backup_recipient: Some(recipient),
        backup_places: vec![backup_place.path().into()],
        ..Default::default()
    }
    .save(home.path())
    .unwrap();
    let rpc = Rpc::new(s.worker.rpc_addr(), &Net::Direct).unwrap();
    let pins = read_live(&rpc, s.vault.id().as_str()).await.unwrap();
    std::fs::write(
        home.path().join("demo-pins.json"),
        serde_json::to_vec(&pins).unwrap(),
    )
    .unwrap();

    let token_file = files.path().join("cursor.token").display().to_string();
    let args: Vec<String> = [
        "--passphrase-file",
        &pass_file,
        "agent",
        "create",
        "--name",
        "cursor",
        "--transport",
        "tcp",
        "--token-file",
        &token_file,
        "--key-file",
        &key_file,
        "--amount",
        &ONE.to_string(),
    ]
    .map(String::from)
    .to_vec();
    let out = blocking_cli(home.path(), args, "yes\n").await;
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{said}\n{err}");

    // (a) The hint, with the note fee read from the vault.
    assert!(
        said.contains("deposit a fresh 1 USDC note each day")
            && said.contains("a note fee every day (0.050000 USDC)")
            && said.contains("daily deposit pattern on your account"),
        "{said}"
    );
    assert!(said.contains("Transport tcp"), "{said}");

    // (b) The vault's own view: the note is a normal one, paid out to the owner.
    let book = Registry::at(home.path()).load().unwrap();
    let agent = &book.agents[0];
    let view = |from: u64| {
        let vault = s.vault.clone();
        async move {
            vault
                .view("note_records")
                .args_json(json!({ "from": from, "limit": 1 }))
                .await
                .unwrap()
                .json::<Value>()
                .unwrap()[0]
                .clone()
        }
    };
    let (ordinary, budget) = (view(0).await, view(1).await);
    assert_eq!(budget["deposit_amount"], ordinary["deposit_amount"]);
    assert_eq!(budget["deposit_amount"], ONE.to_string());
    let fields = |v: &Value| v.as_object().unwrap().keys().cloned().collect::<Vec<_>>();
    assert_eq!(
        fields(&budget),
        fields(&ordinary),
        "no extra field marks it"
    );
    for record in [&ordinary, &budget] {
        let text = record.to_string();
        assert!(
            !text.contains("cursor") && !text.contains("nyma_"),
            "{text}"
        );
    }

    // (c) The registry entry, the token, and only the hash stored.
    let token = std::fs::read_to_string(&token_file).unwrap();
    assert!(token.starts_with("nyma_"), "{token}");
    assert_eq!(agent.token_hash, agents::hash_token(&token));
    assert_eq!(agent.owner_account, s.alice.id().as_str());
    assert_eq!(agent.transport, Transport::Tcp);
    let stored = std::fs::read_to_string(home.path().join(agents::FILE)).unwrap();
    assert!(!stored.contains(token.trim()), "the token is not stored");

    // (d) list shows it; disable makes the next request on a running serve a 401.
    let listed = blocking_cli(home.path(), vec!["agent".into(), "list".into()], "").await;
    assert!(
        String::from_utf8_lossy(&listed.stdout).contains("cursor: Tcp token, active"),
        "{}",
        String::from_utf8_lossy(&listed.stdout)
    );
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    // No config, no gateway: the listener still checks tokens against the registry.
    std::fs::remove_file(home.path().join("config.json")).unwrap();
    let mut serve = std::process::Command::new(env!("CARGO_BIN_EXE_nymirad"));
    serve
        .env("NYMIRA_MODE", "mainnet")
        .arg("--home")
        .arg(home.path())
        .arg("serve")
        .arg("--port")
        .arg(port.to_string())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    // SAFETY: setsid is async-signal-safe and has no preconditions here.
    unsafe {
        serve.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut serve = serve.spawn().unwrap();
    let mut lines = BufReader::new(serve.stdout.take().unwrap());
    let mut first = String::new();
    lines.read_line(&mut first).unwrap();
    assert!(first.contains("listening"), "{first}");
    let (before, body) = agent_request(port, token.trim()).await;
    assert_ne!(before, 401, "{body}");
    let off = blocking_cli(
        home.path(),
        vec![
            "agent".into(),
            "disable".into(),
            "--name".into(),
            "cursor".into(),
        ],
        "",
    )
    .await;
    assert!(off.status.success());
    let (after, body) = agent_request(port, token.trim()).await;
    serve.kill().unwrap();
    serve.wait().unwrap();
    assert_eq!((after, code(&body)), (401, "token_disabled"), "{body}");
}
