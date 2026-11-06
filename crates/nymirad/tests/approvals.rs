//! Owner approval above `confirm_above_usd` against the mock stack: held requests with approval
//! IDs, owner-signed NEP-413 approvals through the owner token, the single-use retry and every
//! refusal. A held request must leave nothing at the operator: no lease, no proof, no chat.

#[path = "support/policy.rs"]
mod policy;
#[path = "support/socks.rs"]
mod socks;
#[path = "support/stack.rs"]
mod stack;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode};
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use nymira_fake_near_rpc::{AccessKeySpec, AccountSpec, FakeRpc};
use nymirad::agents::{Registry, Spec, Transport};
use nymirad::guard::{router_for, Side};
use nymirad::rpc::{Rpc, Transport as Net};
use nymirad::secrets::NoteStore;
use nymirad::token::OwnerToken;
use serde_json::{json, Value};
use stack::*;
use tower::ServiceExt;

const LOCAL: &str = "127.0.0.1:50000";
const PATH: &str = "/v1/nymira/approvals";
/// Worth about 0.0026 USD in the worst case; the policies below ask from 0.001 USD up.
const BIG: u64 = 5_000;

fn body(text: &str, max_tokens: u64) -> Value {
    json!({"model": MODEL, "stream": false, "max_tokens": max_tokens,
        "messages": [{"role": "user", "content": text}]})
}

fn text(call: &str, daily: &str) -> String {
    policy::yaml_limits(&[MODEL], "0.5", call, daily, 30)
        .replace("confirm_above_usd: 5", "confirm_above_usd: 0.001")
}

struct Rig {
    s: Stack,
    app: axum::Router,
    proxy: socks::FakeSocks,
    owner: String,
    registry: Registry,
    node: FakeRpc,
    owner_key: SigningKey,
    call_key: SigningKey,
    stranger_key: SigningKey,
}

fn pubkey(k: &SigningKey) -> String {
    policy::public_key(k)
}

impl Rig {
    async fn new(agents: &[(&str, &str)]) -> Self {
        let proxy = socks::FakeSocks::start(socks::Behavior::Passthrough).await;
        let s = build(Net::Socks(proxy.addr.to_string()), false).await;
        let notes = NoteStore::open(&s.home.path().join("notes.sealed"), PASS, 10).unwrap();
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
        let home = s.home.path().to_path_buf();
        let registry = Registry::at(&home);
        for (i, (name, yaml)) in agents.iter().enumerate() {
            Spec {
                name: (*name).into(),
                transport: Transport::Tcp,
                uid: None,
                group: None,
                gid: None,
                token_file: home.join("tokens").join(format!("{name}.token")),
            }
            .register(&registry, &commitments[i], policy::OWNER, 1)
            .unwrap();
            policy::give_policy_text(&registry, name, 1, yaml);
        }
        let owner_path = home.join("owner.token");
        let token = Arc::new(OwnerToken::load_or_create(&owner_path).unwrap());
        let owner = std::fs::read_to_string(&owner_path).unwrap();
        let app = router_for(token, Some(s.gateway.clone()), registry.clone(), Side::Tcp);
        let (owner_key, call_key, stranger_key) = (
            SigningKey::from_bytes(&[1u8; 32]),
            SigningKey::from_bytes(&[2u8; 32]),
            SigningKey::from_bytes(&[3u8; 32]),
        );
        let node = FakeRpc::start().await;
        node.set_account(policy::OWNER, AccountSpec::default());
        node.set_access_keys(
            policy::OWNER,
            vec![
                AccessKeySpec::full_access(&pubkey(&owner_key), 0),
                AccessKeySpec::function_call(&pubkey(&call_key), 0, "x.test", &["m"]),
            ],
        );
        s.gateway
            .set_key_rpcs(vec![Rpc::new(node.url(), &Net::Direct).unwrap()]);
        Self {
            s,
            app,
            proxy,
            owner,
            registry,
            node,
            owner_key,
            call_key,
            stranger_key,
        }
    }

    fn token(&self, name: &str) -> String {
        std::fs::read_to_string(
            self.s
                .home
                .path()
                .join("tokens")
                .join(format!("{name}.token")),
        )
        .unwrap()
    }

    fn untouched(&self) {
        assert!(self.proxy.seen().is_empty(), "something left the machine");
        assert!(self.s.gateway.records().is_empty(), "a lease was opened");
        assert!(self.s.fake.handle.chats().is_empty());
    }

    async fn call(
        &self,
        method: &str,
        path: &str,
        token: &str,
        extra: &[(&str, &str)],
        body: Vec<u8>,
    ) -> (StatusCode, HeaderMap, Value) {
        let mut b = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost:8788")
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/json");
        for (k, v) in extra {
            b = b.header(*k, *v);
        }
        let mut request = b.body(Body::from(body)).unwrap();
        request
            .extensions_mut()
            .insert(ConnectInfo(LOCAL.parse::<SocketAddr>().unwrap()));
        let reply = self.app.clone().oneshot(request).await.unwrap();
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

    /// A chat request from `agent`, with the approval ID if one is given.
    async fn chat(
        &self,
        agent: &str,
        id: Option<&str>,
        b: Value,
    ) -> (StatusCode, HeaderMap, Value) {
        let extra: Vec<(&str, &str)> = id
            .map(|i| vec![("x-nymira-approval-id", i)])
            .unwrap_or_default();
        self.call(
            "POST",
            "/v1/chat/completions",
            &self.token(agent),
            &extra,
            b.to_string().into_bytes(),
        )
        .await
    }

    async fn waiting(&self) -> Vec<Value> {
        let (status, _, v) = self.call("GET", PATH, &self.owner, &[], vec![]).await;
        assert_eq!(status, StatusCode::OK);
        v.as_array().unwrap().clone()
    }

    async fn approve(&self, file: &Value) -> (StatusCode, Value) {
        let (status, _, v) = self
            .call(
                "POST",
                PATH,
                &self.owner,
                &[],
                file.to_string().into_bytes(),
            )
            .await;
        (status, v)
    }

    /// What the owner's wallet returns for the approval message of a waiting entry.
    fn sign(&self, key: &SigningKey, account: &str, agent: &str, held: &Value) -> Value {
        self.sign_with(key, account, agent, held, |_| {})
    }

    fn sign_with(
        &self,
        key: &SigningKey,
        account: &str,
        agent: &str,
        held: &Value,
        change: impl FnOnce(&mut Value),
    ) -> Value {
        let note = self
            .registry
            .load()
            .unwrap()
            .agents
            .iter()
            .find(|a| a.name == agent)
            .unwrap()
            .note
            .clone();
        let mut message = json!({
            "kind": "nymira-approval",
            "id": held["approval_id"],
            "request_hash": held["request_hash"],
            "amount_micro": held["amount_micro"],
            "nonce": held["nonce"],
        });
        change(&mut message);
        let recipient = nymirad::approvals::recipient(agent, &note);
        let message = message.to_string();
        let nonce = [7u8; 32];
        let hash = nymirad::policy::nep413_hash(&message, &nonce, &recipient);
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        json!({
            "account_id": account,
            "public_key": pubkey(key),
            "signature": b64(&key.sign(&hash).to_bytes()),
            "nonce": b64(&nonce),
            "message": message,
            "recipient": recipient,
        })
    }
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn files_under(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            files_under(&p, out);
        } else if p.is_file() {
            out.push(p);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_request_goes_out_only_after_the_owner_signs_and_then_once() {
    let rig = Rig::new(&[("a", &text("0.05", "2"))]).await;
    let req = body("a large question", BIG);

    // Held: 402 with an ID in the body and in a header; nothing leaves the machine, so the
    // approval check comes before any lease (and so before the balance check).
    let (status, headers, reply) = rig.chat("a", None, req.clone()).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::PAYMENT_REQUIRED, "approval_required")
    );
    let id = reply["error"]["approval_id"].as_str().unwrap().to_string();
    assert_eq!(headers["x-nymira-approval-id"].to_str().unwrap(), id);
    rig.untouched();

    // The same request without the header finds the same hold; it does not make a second one.
    let (_, _, again) = rig.chat("a", None, req.clone()).await;
    assert_eq!(again["error"]["approval_id"], json!(id));
    // The ID alone is not an approval.
    let (status, _, reply) = rig.chat("a", Some(&id), req.clone()).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::PAYMENT_REQUIRED, "approval_required")
    );
    rig.untouched();

    // The owner sees the model, the size and the estimated cost next to the ID.
    let waiting = rig.waiting().await;
    assert_eq!(waiting.len(), 1);
    let held = &waiting[0];
    assert_eq!(held["approval_id"], json!(id));
    assert_eq!(held["agent"], "a");
    assert_eq!(held["model"], MODEL);
    assert!(held["input_tokens"].as_u64().unwrap() > 0);
    let cost: f64 = held["estimated_cost_usd"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!(cost > 0.001 && cost < 0.05, "{cost}");
    assert_eq!(held["approved"], false);

    // Signed by a full-access key of the owner account.
    let file = rig.sign(&rig.owner_key, policy::OWNER, "a", held);
    let (status, reply) = rig.approve(&file).await;
    assert_eq!((status, &reply["approved"]), (StatusCode::OK, &json!(id)));
    assert_eq!(rig.waiting().await[0]["approved"], true);
    rig.untouched();

    // A changed request with the approved ID is refused and does not use the approval up.
    let (status, _, reply) = rig
        .chat("a", Some(&id), body("a large question", BIG + 1))
        .await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::CONFLICT, "approval_mismatch")
    );
    rig.untouched();

    // The same request goes out.
    let (status, _, reply) = rig.chat("a", Some(&id), req.clone()).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(rig.s.fake.handle.chats().len(), 1);

    // Once only.
    let (status, _, reply) = rig.chat("a", Some(&id), req).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::CONFLICT, "approval_used")
    );
    assert_eq!(rig.s.fake.handle.chats().len(), 1);
    assert!(rig.waiting().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approval_that_is_not_the_owners_or_not_for_this_request_is_refused() {
    let rig = Rig::new(&[("a", &text("0.05", "2")), ("b", &text("0.05", "2"))]).await;
    let (_, _, reply) = rig.chat("a", None, body("one", BIG)).await;
    let id = reply["error"]["approval_id"].as_str().unwrap().to_string();
    let held = rig.waiting().await[0].clone();
    let owner = &rig.owner_key;

    let signed_by = |key: &SigningKey, account: &str| rig.sign(key, account, "a", &held);
    let cases: Vec<(&str, Value, StatusCode, &str)> = vec![
        (
            "a function-call key of the owner account",
            signed_by(&rig.call_key, policy::OWNER),
            StatusCode::FORBIDDEN,
            "approval_wrong_signer",
        ),
        (
            "a key that is not on the owner account",
            signed_by(&rig.stranger_key, policy::OWNER),
            StatusCode::FORBIDDEN,
            "approval_wrong_signer",
        ),
        (
            "another account",
            signed_by(owner, "stranger.test"),
            StatusCode::FORBIDDEN,
            "approval_wrong_signer",
        ),
        (
            "a different request hash",
            rig.sign_with(owner, policy::OWNER, "a", &held, |m| {
                m["request_hash"] = json!("00".repeat(32))
            }),
            StatusCode::CONFLICT,
            "approval_mismatch",
        ),
        (
            "a different amount",
            rig.sign_with(owner, policy::OWNER, "a", &held, |m| {
                m["amount_micro"] = json!("1")
            }),
            StatusCode::CONFLICT,
            "approval_mismatch",
        ),
        (
            "a nonce nymirad did not issue",
            rig.sign_with(owner, policy::OWNER, "a", &held, |m| {
                m["nonce"] = json!("11".repeat(32))
            }),
            StatusCode::CONFLICT,
            "approval_mismatch",
        ),
        (
            "an unknown approval ID",
            rig.sign_with(owner, policy::OWNER, "a", &held, |m| {
                m["id"] = json!("apv_00")
            }),
            StatusCode::NOT_FOUND,
            "approval_unknown",
        ),
    ];
    for (what, file, status, expected) in cases {
        let (got, reply) = rig.approve(&file).await;
        assert_eq!((got, code(&reply)), (status, expected), "{what}");
    }
    // The approval ID belongs to agent a: agent b's recipient does not reach it.
    let file = rig.sign(owner, policy::OWNER, "b", &held);
    let (got, reply) = rig.approve(&file).await;
    assert_eq!(
        (got, code(&reply)),
        (StatusCode::NOT_FOUND, "approval_unknown")
    );

    // A signature that does not cover the message.
    let mut file = rig.sign(owner, policy::OWNER, "a", &held);
    file["message"] = json!(file["message"].as_str().unwrap().replace(&id, "apv_x"));
    let (got, reply) = rig.approve(&file).await;
    assert_eq!(
        (got, code(&reply)),
        (StatusCode::BAD_REQUEST, "approval_invalid")
    );

    // A policy signature is not an approval, and an approval is not a policy.
    let policy_file: Value = serde_json::from_slice(&policy::signed(
        &rig.registry,
        "a",
        owner,
        2,
        &text("0.05", "2"),
    ))
    .unwrap();
    let (got, reply) = rig.approve(&policy_file).await;
    assert_eq!(
        (got, code(&reply)),
        (StatusCode::BAD_REQUEST, "approval_invalid")
    );
    let approval = rig.sign(owner, policy::OWNER, "a", &held);
    let book = rig.registry.load().unwrap();
    let agent = book.agents.iter().find(|a| a.name == "a").unwrap();
    assert!(
        nymirad::policy::check_signed(approval.to_string().as_bytes(), agent, &policy::ctx())
            .is_err()
    );

    // The agent's own token, on either route, and no token at all.
    let agent_token = rig.token("a");
    let file = rig
        .sign(owner, policy::OWNER, "a", &held)
        .to_string()
        .into_bytes();
    for (method, body) in [("POST", file), ("GET", vec![])] {
        let (status, _, reply) = rig.call(method, PATH, &agent_token, &[], body).await;
        assert_eq!(
            (status, code(&reply)),
            (StatusCode::FORBIDDEN, "agent_route_forbidden")
        );
    }
    let (status, _, _) = rig.call("GET", PATH, "nymo_nothing", &[], vec![]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // After all of that, nothing is approved and nothing was sent.
    assert!(rig.waiting().await.iter().all(|h| h["approved"] == false));
    let (status, _, reply) = rig.chat("a", Some(&id), body("one", BIG)).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::PAYMENT_REQUIRED, "approval_required")
    );
    rig.untouched();
    // The one real signature still works.
    let (got, _) = rig
        .approve(&rig.sign(owner, policy::OWNER, "a", &held))
        .await;
    assert_eq!(got, StatusCode::OK);
    // And it cannot be given twice.
    let (got, reply) = rig
        .approve(&rig.sign(owner, policy::OWNER, "a", &held))
        .await;
    assert_eq!((got, code(&reply)), (StatusCode::CONFLICT, "approval_used"));
    let _ = &rig.node;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_request_and_its_approval_expire_after_ten_minutes() {
    let rig = Rig::new(&[("a", &text("0.05", "2"))]).await;
    let req = body("late", BIG);

    // Held, then too late to approve.
    let (_, _, reply) = rig.chat("a", None, req.clone()).await;
    let id = reply["error"]["approval_id"].as_str().unwrap().to_string();
    let held = rig.waiting().await[0].clone();
    rig.s.clock.advance_secs(601);
    let (got, reply) = rig
        .approve(&rig.sign(&rig.owner_key, policy::OWNER, "a", &held))
        .await;
    assert_eq!((got, code(&reply)), (StatusCode::GONE, "approval_expired"));
    let (status, _, reply) = rig.chat("a", Some(&id), req.clone()).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::GONE, "approval_expired")
    );
    assert!(rig.waiting().await.is_empty());

    // Approved in time, used too late.
    let (_, _, reply) = rig.chat("a", None, req.clone()).await;
    let fresh = reply["error"]["approval_id"].as_str().unwrap().to_string();
    assert_ne!(fresh, id);
    let held = rig.waiting().await[0].clone();
    let (got, _) = rig
        .approve(&rig.sign(&rig.owner_key, policy::OWNER, "a", &held))
        .await;
    assert_eq!(got, StatusCode::OK);
    rig.s.clock.advance_secs(601);
    let (status, _, reply) = rig.chat("a", Some(&fresh), req).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::GONE, "approval_expired")
    );
    rig.untouched();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sixth_held_request_of_one_agent_is_refused() {
    let rig = Rig::new(&[("a", &text("0.05", "2")), ("b", &text("0.05", "2"))]).await;
    for i in 0..5 {
        let (status, _, reply) = rig.chat("a", None, body("many", BIG + i)).await;
        assert_eq!(
            (status, code(&reply)),
            (StatusCode::PAYMENT_REQUIRED, "approval_required")
        );
    }
    let (status, _, reply) = rig.chat("a", None, body("many", BIG + 5)).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::TOO_MANY_REQUESTS, "approval_limit")
    );
    assert_eq!(rig.waiting().await.len(), 5);
    // Another agent has its own five.
    let (status, _, reply) = rig.chat("b", None, body("many", BIG)).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::PAYMENT_REQUIRED, "approval_required")
    );
    // Held requests that expire free their places.
    rig.s.clock.advance_secs(601);
    let (status, _, reply) = rig.chat("a", None, body("many", BIG + 5)).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::PAYMENT_REQUIRED, "approval_required")
    );
    rig.untouched();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_limits_are_checked_before_a_request_is_held_and_an_approval_does_not_lift_them() {
    // Over the call limit: refused as before, and nothing is held.
    let rig = Rig::new(&[("a", &text("0.002", "2"))]).await;
    let (status, _, reply) = rig.chat("a", None, body("too big", 100_000)).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::PAYMENT_REQUIRED, "call_limit")
    );
    assert!(rig.waiting().await.is_empty());
    rig.untouched();

    // Over the day: the reservation comes first, so there is no hold to approve.
    let rig = Rig::new(&[("a", &text("0.05", "0.05"))]).await;
    let (status, _, reply) = rig.chat("a", None, body("poor day", BIG)).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::PAYMENT_REQUIRED, "daily_limit")
    );
    assert!(rig.waiting().await.is_empty());

    // A model outside the list is refused before anything else.
    let mut other = body("other model", BIG);
    other["model"] = json!("not/listed");
    let (status, _, reply) = rig.chat("a", None, other).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::PAYMENT_REQUIRED, "model_not_allowed")
    );
    assert!(rig.waiting().await.is_empty());

    // A request under the confirmation amount is not held.
    let rig = Rig::new(&[("a", &text("0.05", "2"))]).await;
    let (status, _, reply) = rig.chat("a", None, body("small", 50)).await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert!(rig.waiting().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn held_requests_are_never_written_to_the_state_directory() {
    let rig = Rig::new(&[("a", &text("0.05", "2"))]).await;
    let marker = "zq-private-question-77";
    let req = body(marker, BIG);
    let (_, _, reply) = rig.chat("a", None, req.clone()).await;
    let id = reply["error"]["approval_id"].as_str().unwrap().to_string();
    let held = rig.waiting().await[0].clone();
    let hash = held["request_hash"].as_str().unwrap().to_string();
    let nonce = held["nonce"].as_str().unwrap().to_string();
    let (got, _) = rig
        .approve(&rig.sign(&rig.owner_key, policy::OWNER, "a", &held))
        .await;
    assert_eq!(got, StatusCode::OK);
    let (status, _, _) = rig.chat("a", Some(&id), req).await;
    assert_eq!(status, StatusCode::OK);
    let _ = rig.s.gateway.write_backup();

    let mut files = Vec::new();
    files_under(rig.s.home.path(), &mut files);
    for f in files {
        let bytes = std::fs::read(&f).unwrap();
        let hay = String::from_utf8_lossy(&bytes);
        for needle in [marker, id.as_str(), hash.as_str(), nonce.as_str()] {
            assert!(!hay.contains(needle), "{} holds {needle}", f.display());
        }
    }
}
