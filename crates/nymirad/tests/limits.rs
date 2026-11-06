//! An agent's spending limits against the mock stack: the call limit through `max_tokens`, the
//! day's reservation, the rate limit, the bound class, `budget_exhausted` and `busy`, and the
//! counters in the owner's state directory. Every refusal must leave nothing at the operator:
//! no lease, no proof, no chat.

#[path = "support/policy.rs"]
mod policy;
#[path = "support/socks.rs"]
mod socks;
#[path = "support/stack.rs"]
mod stack;

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode};
use nymira_operator::clock::Clock;
use nymirad::agents::{Registry, Spec, Transport};
use nymirad::guard::{router_for, Side};
use nymirad::rpc::Transport as Net;
use nymirad::secrets::NoteStore;
use nymirad::token::OwnerToken;
use serde_json::{json, Value};
use stack::*;
use tower::ServiceExt;

const LOCAL: &str = "127.0.0.1:50000";

fn body(text: &str, max_tokens: Option<u64>) -> Value {
    let mut b =
        json!({"model": MODEL, "stream": false, "messages": [{"role": "user", "content": text}]});
    if let Some(m) = max_tokens {
        b["max_tokens"] = json!(m);
    }
    b
}

struct Rig {
    s: Stack,
    registry: Registry,
    app: axum::Router,
    proxy: socks::FakeSocks,
    owner: String,
}

fn commitments(home: &Path) -> Vec<String> {
    NoteStore::open(&home.join("notes.sealed"), PASS, 10)
        .unwrap()
        .list()
        .unwrap()
        .into_iter()
        .map(|n| n.commitment.clone())
        .collect()
}

fn token_of(home: &Path, name: &str) -> String {
    std::fs::read_to_string(home.join("tokens").join(format!("{name}.token"))).unwrap()
}

fn register(registry: &Registry, home: &Path, name: &str, note: &str) {
    Spec {
        name: name.into(),
        transport: Transport::Tcp,
        uid: None,
        group: None,
        gid: None,
        token_file: home.join("tokens").join(format!("{name}.token")),
    }
    .register(registry, note, "owner.test", 1)
    .unwrap();
}

impl Rig {
    /// `agents` are named in note order (note 0, 1, ...) with their policy text; `owner_notes`
    /// more 1 USDC notes follow for the owner.
    async fn new(agents: &[(&str, &str)], owner_notes: usize) -> Self {
        let proxy = socks::FakeSocks::start(socks::Behavior::Passthrough).await;
        let s = build(Net::Socks(proxy.addr.to_string()), false).await;
        let notes = NoteStore::open(&s.home.path().join("notes.sealed"), PASS, 10).unwrap();
        for n in 0..(agents.len() + owner_notes) as u64 {
            deposit(&s.usdc, &s.alice, &s.vault, &notes, n, &s.clock).await;
        }
        drop(notes);
        sync_root(&s.worker, &s.alice, &s.vault).await;
        let home = s.home.path().to_path_buf();
        let registry = Registry::at(&home);
        let c = commitments(&home);
        for (i, (name, text)) in agents.iter().enumerate() {
            register(&registry, &home, name, &c[i]);
            policy::give_policy_text(&registry, name, 1, text);
        }
        let owner_path = home.join("owner.token");
        let owner_token = Arc::new(OwnerToken::load_or_create(&owner_path).unwrap());
        let owner = std::fs::read_to_string(&owner_path).unwrap();
        let app = router_for(
            owner_token,
            Some(s.gateway.clone()),
            registry.clone(),
            Side::Tcp,
        );
        Self {
            s,
            registry,
            app,
            proxy,
            owner,
        }
    }

    fn token(&self, name: &str) -> String {
        token_of(self.s.home.path(), name)
    }

    /// Nothing has reached the operator, the attestation host or the model.
    fn untouched(&self) {
        assert!(self.proxy.seen().is_empty(), "something left the machine");
        assert!(self.s.gateway.records().is_empty(), "a lease was opened");
        assert!(self.s.fake.handle.chats().is_empty());
    }
}

async fn ask(
    app: &axum::Router,
    token: &str,
    extra: &[(&str, &str)],
    body: Value,
) -> (StatusCode, HeaderMap, Value) {
    let mut b = Request::builder()
        .method("POST")
        .uri("/v1/chat/completions")
        .header("host", "localhost:8788")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    for (k, v) in extra {
        b = b.header(*k, *v);
    }
    let mut request = b.body(Body::from(body.to_string())).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(LOCAL.parse::<SocketAddr>().unwrap()));
    let reply = app.clone().oneshot(request).await.unwrap();
    let (status, headers) = (reply.status(), reply.headers().clone());
    let bytes = axum::body::to_bytes(reply.into_body(), usize::MAX)
        .await
        .unwrap();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn sent_max_tokens(s: &Stack) -> Vec<u64> {
    s.fake
        .handle
        .chats()
        .iter()
        .map(|c| {
            serde_json::from_slice::<Value>(&c.body).unwrap()["max_tokens"]
                .as_u64()
                .unwrap()
        })
        .collect()
}

async fn settle(s: &Stack) -> Vec<nymirad::gateway::Settled> {
    s.gateway.reap(true).await;
    for _ in 0..3 {
        s.op.tick().await;
        s.clock.advance_secs(11);
    }
    s.op.tick().await;
    s.op.tick().await;
    s.gateway.settle().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_above_max_call_usd_is_refused_and_a_fitting_one_is_capped_by_max_tokens() {
    // 0.001 USD a call: 1,000,000 nano-USD, so about 1,900 output tokens after a short prompt.
    let text = policy::yaml_limits(&[MODEL], "0.5", "0.001", "2", 30);
    let rig = Rig::new(&[("a", &text)], 1).await;
    let a = rig.token("a");

    // Its own max_tokens asks for more than the limit allows.
    let (status, _, reply) = ask(&rig.app, &a, &[], body("hi", Some(100_000))).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::PAYMENT_REQUIRED, "call_limit")
    );
    // The input alone passes the limit: 20,000 bytes at 80 nano-USD each is 1.6 million.
    let (status, _, reply) = ask(&rig.app, &a, &[], body(&"x".repeat(20_000), Some(1))).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::PAYMENT_REQUIRED, "call_limit")
    );
    rig.untouched();

    // A call with no max_tokens of its own gets the cap that keeps it within the limit.
    let (status, _, reply) = ask(&rig.app, &a, &[], body("hello", None)).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let sent = sent_max_tokens(&rig.s);
    assert_eq!(sent.len(), 1);
    assert!(
        sent[0] > 1_000 && sent[0] <= 1_999,
        "the call limit lowers max_tokens: {sent:?}"
    );
    // A call that asks for less keeps its own figure.
    let (status, _, reply) = ask(&rig.app, &a, &[], body("hello", Some(40))).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(sent_max_tokens(&rig.s)[1], 40);
    // The lease bound is the default class whatever the call limit is.
    let records = rig.s.gateway.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].bound_class, "100000");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_second_call_with_a_disallowed_model_on_an_open_lease_is_refused() {
    let text = policy::yaml_limits(&[MODEL], "0.5", "0.05", "2", 30);
    let rig = Rig::new(&[("a", &text)], 0).await;
    let a = rig.token("a");
    let (status, _, reply) = ask(&rig.app, &a, &[], body("first", Some(20))).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let (seen, chats) = (rig.proxy.seen().len(), rig.s.fake.handle.chats().len());
    assert_eq!(rig.s.gateway.records().len(), 1);

    let mut other = body("second", Some(20));
    other["model"] = json!("Qwen/Qwen3.8-27B");
    let (status, _, reply) = ask(&rig.app, &a, &[], other).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::PAYMENT_REQUIRED, "model_not_allowed")
    );
    assert_eq!(
        (rig.proxy.seen().len(), rig.s.fake.handle.chats().len()),
        (seen, chats)
    );
    // The same lease still serves an allowed call.
    let (status, _, _) = ask(&rig.app, &a, &[], body("third", Some(20))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(rig.s.gateway.records().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_calls_at_once_cannot_pass_max_daily_usd_and_settlement_replaces_the_reservation() {
    // The lease bound (0.10) plus one worst-case call (0.05) fits 0.17 USD; a second call does not.
    let text = policy::yaml_limits(&[MODEL], "0.5", "0.05", "0.17", 30);
    let rig = Rig::new(&[("a", &text)], 0).await;
    let a = rig.token("a");
    let (one, two) = tokio::join!(
        ask(&rig.app, &a, &[], body("one", None)),
        ask(&rig.app, &a, &[], body("two", None)),
    );
    let mut results = [one, two];
    results.sort_by_key(|r| r.0);
    assert_eq!(results[0].0, StatusCode::OK, "{}", results[0].2);
    assert_eq!(
        (results[1].0, code(&results[1].2)),
        (StatusCode::PAYMENT_REQUIRED, "daily_limit")
    );
    assert_eq!(rig.s.fake.handle.chats().len(), 1, "only one call went out");
    assert_eq!(rig.s.gateway.records().len(), 1);

    // The first call is done: its worst case is released, the open lease's bound stays.
    let (status, _, reply) = ask(&rig.app, &a, &[], body("three", None)).await;
    assert_eq!(status, StatusCode::OK, "{reply}");

    // Settlement replaces the bound with the charge, and the total is saved in the state
    // directory where only the owner can write.
    let settled = settle(&rig.s).await;
    assert_eq!(settled.len(), 1);
    let charge = settled[0].charge;
    assert!(charge > 0 && charge < 100_000, "{charge}");
    let note = &commitments(rig.s.home.path())[0];
    assert_eq!(rig.s.gateway.agent_spend_today("a", note), charge);
    let counters = rig.s.home.path().join(nymirad::limits::file_name("a"));
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode();
    assert_eq!(mode(&counters) & 0o777, 0o600);
    assert_eq!(
        mode(rig.s.home.path()) & 0o022,
        0,
        "the state directory is closed to the agent's OS user"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn after_a_restore_todays_total_is_never_below_the_charges_in_the_lease_records() {
    let text = policy::yaml_limits(&[MODEL], "0.5", "0.05", "2", 30);
    let rig = Rig::new(&[("a", &text)], 0).await;
    let a = rig.token("a");
    let (status, _, _) = ask(&rig.app, &a, &[], body("charge me", Some(20))).await;
    assert_eq!(status, StatusCode::OK);
    let settled = settle(&rig.s).await;
    let charge = settled[0].charge;
    let note = commitments(rig.s.home.path())[0].clone();
    let counters = rig.s.home.path().join(nymirad::limits::file_name("a"));
    assert_eq!(rig.s.gateway.agent_spend_today("a", &note), charge);

    let proxy = rig.proxy.addr.to_string();
    // The counter file is gone, as after a restore onto a new machine.
    std::fs::remove_file(&counters).unwrap();
    let gw = make_gateway(
        rig.s.home.path(),
        &rig.s.parts,
        Net::Socks(proxy.clone()),
        rig.s.clock.clone(),
    )
    .await;
    assert_eq!(gw.agent_spend_today("a", &note), charge);
    // The counter file is older than the records.
    std::fs::write(
        &counters,
        format!(
            r#"{{"day":{},"charged_micro":"0"}}"#,
            rig.s.clock.now_secs() / 86_400
        ),
    )
    .unwrap();
    let gw = make_gateway(
        rig.s.home.path(),
        &rig.s.parts,
        Net::Socks(proxy.clone()),
        rig.s.clock.clone(),
    )
    .await;
    assert!(gw.agent_spend_today("a", &note) >= charge);
    assert_eq!(
        rig.s
            .gateway
            .records()
            .iter()
            .filter_map(|r| r.charge.clone())
            .count(),
        1
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn more_than_the_rate_limit_in_a_minute_gets_429_with_retry_after_and_sends_nothing() {
    let text = policy::yaml_limits(&[MODEL], "0.5", "0.05", "2", 2);
    let rig = Rig::new(&[("a", &text)], 0).await;
    let a = rig.token("a");
    for i in 0..2 {
        let (status, _, reply) = ask(&rig.app, &a, &[], body(&format!("call {i}"), Some(20))).await;
        assert_eq!(status, StatusCode::OK, "{reply}");
    }
    let (seen, chats) = (rig.proxy.seen().len(), rig.s.fake.handle.chats().len());
    let (status, headers, reply) = ask(&rig.app, &a, &[], body("one too many", Some(20))).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::TOO_MANY_REQUESTS, "policy_rate_limit")
    );
    let wait: u64 = headers["retry-after"].to_str().unwrap().parse().unwrap();
    assert!((1..=60).contains(&wait), "{wait}");
    assert_eq!(
        (rig.proxy.seen().len(), rig.s.fake.handle.chats().len()),
        (seen, chats),
        "no proof or chat for a rate-limited request"
    );
    // The window moves on.
    rig.s.clock.advance_secs(wait);
    let (status, _, reply) = ask(&rig.app, &a, &[], body("later", Some(20))).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_large_bound_class_needs_both_the_request_and_a_policy_of_at_least_half_a_usdc() {
    let small = policy::yaml_limits(&[MODEL], "0.4", "0.05", "2", 30);
    let large = policy::yaml_limits(&[MODEL], "0.5", "0.05", "2", 30);
    let rig = Rig::new(
        &[("small", &small), ("large", &large), ("plain", &large)],
        0,
    )
    .await;
    let ask_class = |name: &'static str, lease: Option<&'static str>| {
        let (app, token) = (rig.app.clone(), rig.token(name));
        async move {
            let extra: Vec<(&str, &str)> = lease
                .map(|l| vec![("x-nymira-max-lease-usd", l)])
                .unwrap_or_default();
            let (status, _, reply) = ask(&app, &token, &extra, body("class", Some(20))).await;
            assert_eq!(status, StatusCode::OK, "{reply}");
        }
    };
    ask_class("small", Some("5")).await;
    ask_class("large", Some("5")).await;
    ask_class("plain", None).await;
    let mut classes: Vec<(u32, String)> = rig
        .s
        .gateway
        .records()
        .into_iter()
        .map(|r| (r.note_id, r.bound_class))
        .collect();
    classes.sort();
    assert_eq!(
        classes,
        vec![
            (0, "100000".to_string()), // max_lease_usd 0.4: never the 0.50 class
            (1, "500000".to_string()),
            (2, "100000".to_string()), // the default class unless the request asks
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_budget_note_gets_budget_exhausted_whatever_the_owner_holds() {
    let text = policy::yaml_limits(&[MODEL], "0.5", "0.05", "5", 30);
    // Note 0 is the owner's, funded and ready.
    let rig = Rig::new(&[], 1).await;
    let home = rig.s.home.path().to_path_buf();

    // The agent's budget note holds nothing: its commitment is no funded note in this vault.
    register(&rig.registry, &home, "c", &format!("0x{}", "ab".repeat(32)));
    policy::give_policy_text(&rig.registry, "c", 1, &text);
    let (status, _, reply) = ask(&rig.app, &rig.token("c"), &[], body("empty", Some(20))).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::PAYMENT_REQUIRED, "budget_exhausted")
    );
    // The model was attested on the way (the balance check comes last), but no lease was
    // opened and no chat was sent.
    assert!(rig.s.gateway.records().is_empty());
    assert!(rig.s.fake.handle.chats().is_empty());

    // The owner's funded note was not offered to the agent, and still serves the owner.
    let (status, _, reply) = ask(&rig.app, &rig.owner, &[], body("owner", Some(20))).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    let records = rig.s.gateway.records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].note_id, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_third_request_on_a_lease_with_two_in_flight_gets_409_busy() {
    let text = policy::yaml_limits(&[MODEL], "0.5", "0.05", "2", 30);
    let rig = Rig::new(&[("a", &text)], 0).await;
    let a = rig.token("a");
    let (status, _, reply) = ask(&rig.app, &a, &[], body("open the lease", Some(20))).await;
    assert_eq!(status, StatusCode::OK, "{reply}");

    rig.s.fake.handle.update(|sw| sw.latency_ms = 4_000);
    let slow: Vec<_> = (0..2)
        .map(|i| {
            let (app, a) = (rig.app.clone(), a.clone());
            tokio::spawn(
                async move { ask(&app, &a, &[], body(&format!("slow {i}"), Some(20))).await },
            )
        })
        .collect();
    tokio::time::sleep(std::time::Duration::from_millis(1_500)).await;
    let (status, _, reply) = ask(&rig.app, &a, &[], body("third", Some(20))).await;
    assert_eq!((status, code(&reply)), (StatusCode::CONFLICT, "busy"));
    for handle in slow {
        let (status, _, reply) = handle.await.unwrap();
        assert_eq!(status, StatusCode::OK, "{reply}");
    }
    assert_eq!(rig.s.gateway.records().len(), 1);
    // The slots are free again.
    rig.s.fake.handle.update(|sw| sw.latency_ms = 0);
    let (status, _, _) = ask(&rig.app, &a, &[], body("fourth", Some(20))).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_checks_run_in_order_model_rate_call_then_day_and_none_reaches_the_operator() {
    // A day of 0.05 USD cannot hold the 0.10 lease bound, so every call that gets as far as the
    // day's reservation is refused; the rate limit is 3 a minute.
    let text = policy::yaml_limits(&[MODEL], "0.5", "0.001", "0.05", 3);
    let rig = Rig::new(&[("a", &text)], 0).await;
    let a = rig.token("a");
    let reason = |(status, _, reply): &(StatusCode, HeaderMap, Value)| {
        (status.as_u16(), code(reply).to_string())
    };
    let wrong_model = || {
        let mut b = body("hi", Some(100_000));
        b["model"] = json!("Qwen/Qwen3.8-27B");
        b
    };

    // The model comes first: it is refused without using a rate slot.
    let r = ask(&rig.app, &a, &[], wrong_model()).await;
    assert_eq!(reason(&r), (402, "model_not_allowed".into()));
    // Then the call's cost, ahead of the day.
    let r = ask(&rig.app, &a, &[], body("hi", Some(100_000))).await;
    assert_eq!(reason(&r), (402, "call_limit".into()));
    let r = ask(&rig.app, &a, &[], body("hi", Some(20))).await;
    assert_eq!(reason(&r), (402, "daily_limit".into()));
    let r = ask(&rig.app, &a, &[], body("hi", Some(20))).await;
    assert_eq!(reason(&r), (402, "daily_limit".into()));
    // Three slots are used: the rate limit now comes before the call's cost, after the model.
    let r = ask(&rig.app, &a, &[], body("hi", Some(100_000))).await;
    assert_eq!(reason(&r), (429, "policy_rate_limit".into()));
    let r = ask(&rig.app, &a, &[], wrong_model()).await;
    assert_eq!(reason(&r), (402, "model_not_allowed".into()));
    rig.untouched();
}
