//! `require_tee` for an agent's answers against the mock stack: an answer, or a stream, is held
//! until its signature has been read from the operator and checked, comes back with its
//! signature kind, and is replaced by `not_verified` with no content when the check fails.

#[path = "support/policy.rs"]
mod policy;
#[path = "support/socks.rs"]
mod socks;
#[path = "support/stack.rs"]
mod stack;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, Request, StatusCode};
use nymira_fake_near_ai::SignatureKind;
use nymirad::agents::{Registry, Spec, Transport};
use nymirad::attest::Trust;
use nymirad::guard::{router_for, Side};
use nymirad::rpc::Transport as Net;
use nymirad::secrets::NoteStore;
use nymirad::token::OwnerToken;
use serde_json::{json, Value};
use stack::*;
use tower::ServiceExt;

const LOCAL: &str = "127.0.0.1:50000";
const KIND: &str = "x-nymira-signature-kind";

fn tee_policy(require: bool) -> String {
    policy::yaml_limits(&[MODEL], "0.5", "0.05", "2", 30)
        .replace("require_tee: false", &format!("require_tee: {require}"))
}

struct Rig {
    s: Stack,
    app: axum::Router,
    token: String,
    _proxy: socks::FakeSocks,
}

impl Rig {
    async fn new(require_tee: bool) -> Self {
        let proxy = socks::FakeSocks::start(socks::Behavior::Passthrough).await;
        let s = build(Net::Socks(proxy.addr.to_string()), false).await;
        let notes = NoteStore::open(&s.home.path().join("notes.sealed"), PASS, 10).unwrap();
        deposit(&s.usdc, &s.alice, &s.vault, &notes, 0, &s.clock).await;
        let commitment = notes.list().unwrap()[0].commitment.clone();
        drop(notes);
        sync_root(&s.worker, &s.alice, &s.vault).await;
        let home = s.home.path().to_path_buf();
        let registry = Registry::at(&home);
        let token_file = home.join("tokens").join("a.token");
        Spec {
            name: "a".into(),
            transport: Transport::Tcp,
            uid: None,
            group: None,
            gid: None,
            token_file: token_file.clone(),
        }
        .register(&registry, &commitment, "owner.test", 1)
        .unwrap();
        policy::give_policy_text(&registry, "a", 1, &tee_policy(require_tee));
        let owner_path = Path::new(&home).join("owner.token");
        let owner = Arc::new(OwnerToken::load_or_create(&owner_path).unwrap());
        let app = router_for(owner, Some(s.gateway.clone()), registry, Side::Tcp);
        let token = std::fs::read_to_string(token_file).unwrap();
        Self {
            s,
            app,
            token,
            _proxy: proxy,
        }
    }

    /// The agent's request, running in the background.
    fn ask(
        &self,
        text: &'static str,
        stream: bool,
    ) -> tokio::task::JoinHandle<(StatusCode, HeaderMap, String)> {
        let (app, token) = (self.app.clone(), self.token.clone());
        tokio::spawn(async move {
            let body = json!({"model": MODEL, "stream": stream, "max_tokens": 20,
                "messages": [{"role": "user", "content": text}]});
            let mut request = Request::builder()
                .method("POST")
                .uri("/v1/chat/completions")
                .header("host", "localhost:8788")
                .header("authorization", format!("Bearer {token}"))
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo(LOCAL.parse::<SocketAddr>().unwrap()));
            let reply = app.oneshot(request).await.unwrap();
            let (status, headers) = (reply.status(), reply.headers().clone());
            let bytes = axum::body::to_bytes(reply.into_body(), usize::MAX)
                .await
                .unwrap();
            (
                status,
                headers,
                String::from_utf8_lossy(&bytes).into_owned(),
            )
        })
    }

    /// Waits until the model has answered, then shows the request is still held: nothing, not
    /// even the status line, has reached the agent.
    async fn held(&self, call: &tokio::task::JoinHandle<(StatusCode, HeaderMap, String)>) {
        for _ in 0..600 {
            if !self.s.fake.handle.chats().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert_eq!(self.s.fake.handle.chats().len(), 1, "the model was asked");
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert!(
            !call.is_finished(),
            "the answer was released before verification"
        );
    }

    /// The operator's own loop, so the evidence is stored while the request waits.
    fn operator_ticks(&self) -> tokio::task::JoinHandle<()> {
        let op = self.s.op.clone();
        tokio::spawn(async move {
            loop {
                op.tick().await;
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
    }
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

fn code(text: &str) -> String {
    serde_json::from_str::<Value>(text).map_or(String::new(), |v| {
        v["error"]["code"].as_str().unwrap_or("").to_string()
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_is_held_until_its_signature_verifies_and_comes_back_with_the_kind() {
    let rig = Rig::new(true).await;
    let call = rig.ask("hi", false);
    rig.held(&call).await;
    let ticks = rig.operator_ticks();
    let (status, headers, text) = call.await.unwrap();
    ticks.abort();
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(headers[KIND], "model-signed");
    let answer: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        answer["choices"][0]["message"]["content"],
        "fake reply to: hi"
    );
    // The lease was closed to read the signature, and it settles as model-signed.
    let settled = settle(&rig.s).await;
    assert_eq!(settled.len(), 1);
    assert_eq!(settled[0].trust, Trust::ModelSigned);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_gateway_signed_answer_passes_and_says_so() {
    let rig = Rig::new(true).await;
    rig.s
        .fake
        .handle
        .update(|sw| sw.chat_signature_kind = SignatureKind::Gateway);
    let call = rig.ask("hi", false);
    let ticks = rig.operator_ticks();
    let (status, headers, text) = call.await.unwrap();
    ticks.abort();
    assert_eq!(status, StatusCode::OK, "{text}");
    assert_eq!(headers[KIND], "gateway-signed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_streamed_answer_reaches_the_agent_only_after_verification() {
    let rig = Rig::new(true).await;
    let call = rig.ask("stream me", true);
    rig.held(&call).await;
    let ticks = rig.operator_ticks();
    let (status, headers, text) = call.await.unwrap();
    ticks.abort();
    assert_eq!(status, StatusCode::OK, "{text}");
    // The fake follows the live gateway: it signs streams itself.
    assert_eq!(headers[KIND], "gateway-signed");
    let content: String = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| serde_json::from_str::<Value>(d).ok())
        .filter_map(|v| {
            v["choices"][0]["delta"]["content"]
                .as_str()
                .map(String::from)
        })
        .collect();
    assert_eq!(content, "fake reply to: stream me");
    assert!(text.contains("[DONE]"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_signature_gives_not_verified_and_no_content() {
    for stream in [false, true] {
        let rig = Rig::new(true).await;
        rig.s.fake.handle.update(|sw| sw.bad_signature = true);
        let call = rig.ask("secret answer", stream);
        let ticks = rig.operator_ticks();
        let (status, headers, text) = call.await.unwrap();
        ticks.abort();
        assert_eq!(status, StatusCode::BAD_GATEWAY, "{text}");
        assert_eq!(code(&text), "not_verified", "{text}");
        assert!(!headers.contains_key(KIND));
        assert!(
            !text.contains("fake reply") && !text.contains("choices"),
            "no content may leave: {text}"
        );
        assert_eq!(rig.s.fake.handle.chats().len(), 1, "the model did answer");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signature_by_a_key_the_report_does_not_list_gives_not_verified() {
    let rig = Rig::new(true).await;
    rig.s.fake.handle.update(|sw| sw.unbound_key = true);
    let call = rig.ask("hi", false);
    let ticks = rig.operator_ticks();
    let (status, _, text) = call.await.unwrap();
    ticks.abort();
    assert_eq!(status, StatusCode::BAD_GATEWAY, "{text}");
    assert_eq!(code(&text), "not_verified");
    assert!(text.contains("attested key"), "{text}");
    assert!(!text.contains("fake reply"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_report_that_fails_stops_the_request_before_anything_is_sent() {
    let rig = Rig::new(true).await;
    rig.s
        .fake
        .handle
        .update(|sw| sw.measurement = "ff".repeat(32));
    let (status, _, text) = rig.ask("hi", false).await.unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{text}");
    assert_eq!(code(&text), "attestation_failed");
    assert!(rig.s.fake.handle.chats().is_empty());
    assert!(rig.s.gateway.records().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn without_require_tee_the_answer_comes_at_once_on_an_open_lease() {
    let rig = Rig::new(false).await;
    // No operator ticks run: nothing here waits for a signature.
    let (status, headers, text) = rig.ask("hi", false).await.unwrap();
    assert_eq!(status, StatusCode::OK, "{text}");
    assert!(!headers.contains_key(KIND));
    let records = rig.s.gateway.records();
    assert_eq!(records.len(), 1);
    assert!(records[0].closed_at.is_none(), "the lease stays open");
}
