//! The owner's side of a budget note: the view of each agent's spend, revocation (token, lease,
//! clearance, close, else exit), the actions the agent token can never reach, and the expiry
//! guard on a budget note. Against the mock stack: a sandbox vault, the real operator behind the
//! fake NEAR AI gateway, and `nymirad`'s gateway in front. Nothing may reach the operator for an
//! agent request that is refused.

#[path = "support/policy.rs"]
mod policy;
#[path = "support/stack.rs"]
mod stack;

use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use near_workspaces::types::NearToken;
use nymira_operator::clock::Clock;
use nymirad::agents::{Registry, Scope, Spec, Transport};
use nymirad::expiry::{Event, DAY};
use nymirad::gateway::close::{Ask, LeaveError};
use nymirad::gateway::revoke::{RevokeOptions, Revoked};
use nymirad::gateway::ChatOptions;
use nymirad::guard::{router_for, Side};
use nymirad::policy::Policy;
use nymirad::rpc::Transport as Net;
use nymirad::secrets::{NoteStatus, NoteStore};
use nymirad::token::OwnerToken;
use serde_json::{json, Value};
use stack::*;
use tower::ServiceExt;

const LOCAL: &str = "127.0.0.1:50000";
const AGENTS: &str = "/v1/nymira/agents";

fn chat_body(text: &str) -> Value {
    json!({"model": MODEL, "stream": false, "max_tokens": 20,
        "messages": [{"role": "user", "content": text}]})
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

/// Answers every question the same way and keeps what was asked.
struct Scripted {
    yes: bool,
    asked: Mutex<Vec<String>>,
}

#[async_trait]
impl Ask for Scripted {
    async fn confirm_direct(&self, text: &str) -> bool {
        self.asked.lock().unwrap().push(text.into());
        self.yes
    }
}

async fn call(
    app: &axum::Router,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut b = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost:8788")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json");
    if method == "GET" {
        b = b.header("accept", "application/json");
    }
    let mut request = b
        .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
        .unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(LOCAL.parse::<SocketAddr>().unwrap()));
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

struct Rig {
    s: Stack,
    registry: Registry,
    app: axum::Router,
    owner: String,
    agent: String,
    note: String,
}

impl Rig {
    /// One budget note (note 0) for the agent `a`, in a vault whose notes live `ttl_days` days.
    async fn new(transport: Net, ttl_days: u64) -> Self {
        let mut s = build_ttl(transport.clone(), false, ttl_days).await;
        // The guard closes at once in tests; its default waits a random minute or more.
        let guard = nymirad::expiry::GuardConfig {
            close: no_delay(),
            retry_secs: 0,
            ..Default::default()
        };
        s.gateway =
            make_gateway_guarded(s.home.path(), &s.parts, transport, s.clock.clone(), guard).await;
        let notes = NoteStore::open(&s.home.path().join("notes.sealed"), PASS, 10).unwrap();
        deposit(&s.usdc, &s.alice, &s.vault, &notes, 0, &s.clock).await;
        let note = notes.list().unwrap()[0].commitment.clone();
        drop(notes);
        sync_root(&s.worker, &s.alice, &s.vault).await;
        let home = s.home.path().to_path_buf();
        let registry = Registry::at(&home);
        register(&registry, &home, "a", &note);
        policy::give_policy_text(
            &registry,
            "a",
            1,
            &policy::yaml_limits(&[MODEL], "0.5", "0.05", "2", 30),
        );
        let owner_path = home.join("owner.token");
        let owner_token = Arc::new(OwnerToken::load_or_create(&owner_path).unwrap());
        let owner = std::fs::read_to_string(&owner_path).unwrap();
        let app = router_for(
            owner_token,
            Some(s.gateway.clone()),
            registry.clone(),
            Side::Tcp,
        );
        let agent = std::fs::read_to_string(home.join("tokens").join("a.token")).unwrap();
        Self {
            s,
            registry,
            app,
            owner,
            agent,
            note,
        }
    }

    /// What the agent would hold if it got past the front door.
    fn scope(&self) -> Scope {
        Scope::Agent {
            name: "a".into(),
            note: self.note.clone(),
            policy: Policy {
                allowed_models: vec![MODEL.into()],
                max_lease_usd: 0.5,
                max_call_usd: 0.05,
                max_daily_usd: 2.0,
                confirm_above_usd: 5.0,
                rate_limit_per_min: 30,
                require_tee: false,
            },
        }
    }

    async fn agent_chat(&self) -> (StatusCode, Value) {
        call(
            &self.app,
            "POST",
            "/v1/chat/completions",
            &self.agent,
            Some(chat_body("hello")),
        )
        .await
    }

    /// An agent request past the front door, straight into the gateway.
    #[allow(clippy::result_large_err)]
    async fn inside(&self) -> Result<(), nymirad::openai::ApiError> {
        let opts = ChatOptions {
            scope: self.scope(),
            ..ChatOptions::default()
        };
        self.s
            .gateway
            .chat(chat_body("inside"), opts)
            .await
            .map(|_| ())
    }

    async fn view(&self) -> Value {
        let (status, rows) = call(&self.app, "GET", AGENTS, &self.owner, None).await;
        assert_eq!(status, StatusCode::OK, "{rows}");
        rows[0].clone()
    }

    /// Nothing new reached the operator or the model since `(records, chats)`.
    fn quiet_since(&self, before: (usize, usize)) {
        assert_eq!(
            (
                self.s.gateway.records().len(),
                self.s.fake.handle.chats().len()
            ),
            before,
            "an agent request reached the operator"
        );
    }

    fn mark(&self) -> (usize, usize) {
        (
            self.s.gateway.records().len(),
            self.s.fake.handle.chats().len(),
        )
    }
}

async fn settle(s: &Stack) {
    s.gateway.reap(true).await;
    for _ in 0..3 {
        s.op.tick().await;
        s.clock.advance_secs(11);
    }
    s.op.tick().await;
    s.op.tick().await;
    s.gateway.settle().await;
}

fn quick() -> RevokeOptions {
    RevokeOptions {
        close: no_delay(),
        settle_polls: 400,
        settle_every: Duration::from_millis(50),
    }
}

// ---- the owner's view -------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_sees_spend_balance_expiry_and_policy_and_the_agent_token_does_not() {
    let rig = Rig::new(Net::Direct, 30).await;
    submitter(&rig.s, "personal", NearToken::from_near(5)).await;
    let note = entry(&rig.s);
    let deposited: u128 = note.deposit_amount.as_deref().unwrap().parse().unwrap();

    let row = rig.view().await;
    assert_eq!(row["agent"], "a");
    assert_eq!(row["token"], "active");
    assert_eq!(row["note_id"], 0);
    assert_eq!(row["note_status"], "Accepted");
    assert_eq!(row["spent_today_micro"], "0");
    assert_eq!(row["balance_micro"], deposited.to_string());
    assert_eq!(row["expires_at"], note.expiry_ns.unwrap());
    assert_eq!(row["policy_version"], 1);
    assert_eq!(row["policy"]["max_daily_usd"], 2.0);
    assert_eq!(row["policy"]["allowed_models"], json!([MODEL]));

    let (status, reply) = rig.agent_chat().await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    settle(&rig.s).await;
    let charge: u128 = rig.s.gateway.records()[0]
        .charge
        .as_deref()
        .unwrap()
        .parse()
        .unwrap();
    assert!(charge > 0);
    let row = rig.view().await;
    assert_eq!(row["spent_today_micro"], charge.to_string());
    assert_eq!(row["balance_micro"], (deposited - charge).to_string());

    // The view holds no token and no secret.
    let text = row.to_string();
    assert!(!text.contains("nyma_") && !text.contains(&note.secret));
    // The agent token and a missing token get nothing.
    let (status, reply) = call(&rig.app, "GET", AGENTS, &rig.agent, None).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::FORBIDDEN, "agent_route_forbidden")
    );
    let (status, _) = call(&rig.app, "GET", AGENTS, "", None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---- revocation -------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn revoking_with_an_open_lease_retires_it_closes_the_note_and_pays_the_owner() {
    let rig = Rig::new(Net::Direct, 30).await;
    submitter(&rig.s, "personal", NearToken::from_near(5)).await;
    let (status, reply) = rig.agent_chat().await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    assert_eq!(rig.s.gateway.records().len(), 1);
    let commitment = rig.note.clone();
    let deposited: u128 = entry(&rig.s)
        .deposit_amount
        .as_deref()
        .unwrap()
        .parse()
        .unwrap();
    let before = rig.s.usdc.balance_of(rig.s.alice.id()).await;
    let seen = rig.mark();

    // While the revocation runs, the operator is ticked so the lease settles, and the agent
    // keeps asking: at the door (token disabled) and inside (the note is shut).
    let done = AtomicBool::new(false);
    let probes = Mutex::new(Vec::new());
    let revoke = async {
        let out = rig.s.gateway.revoke_agent("a", quick(), None).await;
        done.store(true, Ordering::SeqCst);
        out
    };
    let driver = async {
        while !done.load(Ordering::SeqCst) {
            rig.s.op.tick().await;
            rig.s.clock.advance_secs(11);
            let (door, body) = rig.agent_chat().await;
            let inside = rig.inside().await.unwrap_err();
            probes.lock().unwrap().push((
                door,
                code(&body).to_string(),
                inside.status,
                inside.code,
            ));
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    };
    let (out, ()) = tokio::join!(revoke, driver);
    let Revoked::Closed(closed) = out.expect("the revocation closes the note") else {
        panic!("the operator gave a clearance, so no exit is expected");
    };
    assert!(closed.confirmed);

    let probes = probes.into_inner().unwrap();
    assert!(!probes.is_empty());
    for (door, door_code, status, inside) in probes {
        assert_eq!(
            (door, door_code.as_str()),
            (StatusCode::UNAUTHORIZED, "token_disabled")
        );
        assert_eq!(
            (status, inside.as_str()),
            (StatusCode::PAYMENT_REQUIRED, "note_expiring")
        );
    }
    // The lease was retired and settled, the note closed to the payout account, the token is off.
    let record = &rig.s.gateway.records()[0];
    assert!(record.closed_at.is_some());
    let charge: u128 = record.charge.as_deref().unwrap().parse().unwrap();
    assert_eq!(status_of(&rig.s, &commitment).await, "Closed");
    assert_eq!(entry(&rig.s).status, NoteStatus::Closed);
    assert_eq!(
        rig.s.usdc.balance_of(rig.s.alice.id()).await,
        before + deposited - charge
    );
    assert!(rig.registry.load().unwrap().agents[0].disabled);
    assert!(rig.s.closes.sent().is_empty(), "the operator was not used");
    // After the close nothing is served either, and nothing new reached the operator.
    assert_eq!(rig.inside().await.unwrap_err().code, "note_expiring");
    let (door, body) = rig.agent_chat().await;
    assert_eq!(
        (door, code(&body)),
        (StatusCode::UNAUTHORIZED, "token_disabled")
    );
    assert_eq!(rig.s.gateway.records().len(), seen.0);
    assert_eq!(rig.s.fake.handle.chats().len(), seen.1);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_clearance_revocation_files_an_exit_and_finalize_is_sent_after_the_window() {
    let rig = Rig::new(Net::Direct, 30).await;
    submitter(&rig.s, "personal", NearToken::from_near(5)).await;
    let commitment = rig.note.clone();
    rig.s
        .op
        .set_emergency("the clearance key may be exposed")
        .await
        .unwrap();
    let seen = rig.mark();

    let out = rig
        .s
        .gateway
        .revoke_agent("a", quick(), None)
        .await
        .expect("the revocation falls back to an exit");
    let Revoked::Exiting(exit, _) = out else {
        panic!("a refused clearance must end in an exit");
    };
    assert_eq!(exit.conceded, 0);
    assert_eq!(status_of(&rig.s, &commitment).await, "PendingExit");
    assert_eq!(entry(&rig.s).status, NoteStatus::Exiting);
    assert!(rig.registry.load().unwrap().agents[0].disabled);
    // No request is served once the exit proof has left.
    assert_eq!(rig.inside().await.unwrap_err().code, "note_expiring");
    rig.quiet_since(seen);

    // After the window `finalize_exit` goes out on its own. The sandbox cannot wait seven days,
    // so the vault still refuses; the payout is the vault's own test.
    let now = rig.s.clock.now_secs();
    rig.s
        .clock
        .advance_secs((exit.deadline_secs + 10).saturating_sub(now));
    let tried = rig.s.gateway.finalize_due().await;
    assert_eq!(tried.len(), 1);
    let err = tried[0].1.as_ref().expect_err("the vault's window is open");
    assert!(
        err.to_string().contains("exit window is still open"),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_submitter_without_near_hands_the_close_to_the_operator_and_a_rerun_picks_up() {
    let rig = Rig::new(Net::Direct, 30).await;
    submitter(&rig.s, "personal", NearToken::from_millinear(2)).await;

    let err = rig
        .s
        .gateway
        .revoke_agent("a", quick(), None)
        .await
        .expect_err("no NEAR for gas");
    assert!(matches!(err, LeaveError::NoNear { .. }), "{err}");
    // The agent stays shut out while the owner fixes it.
    assert!(rig.registry.load().unwrap().agents[0].disabled);
    assert_eq!(rig.inside().await.unwrap_err().code, "note_expiring");
    assert!(rig.s.closes.sent().is_empty());

    let mut opts = quick();
    opts.close.via_operator = true;
    let out = rig
        .s
        .gateway
        .revoke_agent("a", opts, None)
        .await
        .expect("the operator takes the close proof");
    // The operator's closer is a stub here, so the vault does not show the note closed yet.
    assert!(
        matches!(out, Revoked::Closed(ref c) if !c.confirmed),
        "{out:?}"
    );
    assert_eq!(rig.s.closes.sent().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dead_tor_sends_nothing_and_never_asks_during_a_revocation() {
    let (tor, _state) = nymirad::tor::Tor::for_tests(
        "127.0.0.1:9".parse().unwrap(),
        nymirad::tor::TorState::Failed("test: no circuit".into()),
    );
    let rig = Rig::new(Net::Tor(Arc::new(tor)), 30).await;
    submitter(&rig.s, "personal", NearToken::from_near(5)).await;
    let no = Scripted {
        yes: false,
        asked: Mutex::new(Vec::new()),
    };

    let err = rig
        .s
        .gateway
        .revoke_agent("a", quick(), Some(&no))
        .await
        .expect_err("Tor is down");
    assert!(matches!(err, LeaveError::TorDown(_)), "{err}");
    // The clearance and the close never go direct, so there is nothing to confirm.
    assert!(no.asked.lock().unwrap().is_empty());
    assert_eq!(status_of(&rig.s, &rig.note).await, "Active");
    assert!(rig.registry.load().unwrap().agents[0].disabled);
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_an_unknown_agent_changes_nothing() {
    let rig = Rig::new(Net::Direct, 30).await;
    let err = rig
        .s
        .gateway
        .revoke_agent("nobody", quick(), None)
        .await
        .expect_err("no such agent");
    assert!(err.to_string().contains("no agent named nobody"), "{err}");
    assert!(!rig.registry.load().unwrap().agents[0].disabled);
}

// ---- the agent token never reaches an owner action -----------------------------------------

/// One test per action: the agent token is refused before any handler runs, on the TCP listener.
macro_rules! refused_for_the_agent {
    ($name:ident, $method:expr, $path:expr) => {
        #[tokio::test]
        async fn $name() {
            let home = tempfile::tempdir().unwrap();
            let registry = Registry::at(home.path());
            register(&registry, home.path(), "a", "0x01");
            let token =
                std::fs::read_to_string(home.path().join("tokens").join("a.token")).unwrap();
            let owner = Arc::new(OwnerToken::load_or_create(&home.path().join("o.token")).unwrap());
            let app = router_for(owner, None, registry, Side::Tcp);
            let (status, reply) = call(&app, $method, $path, &token, Some(json!({}))).await;
            assert_eq!(
                (status, code(&reply)),
                (StatusCode::FORBIDDEN, "agent_route_forbidden"),
                "{reply}"
            );
        }
    };
}

refused_for_the_agent!(the_agent_token_cannot_close, "POST", "/v1/close");
refused_for_the_agent!(the_agent_token_cannot_exit, "POST", "/v1/exit");
refused_for_the_agent!(
    the_agent_token_cannot_claim_a_payout,
    "POST",
    "/v1/claim-payout"
);
refused_for_the_agent!(
    the_agent_token_cannot_change_policy,
    "POST",
    "/v1/nymira/policy"
);
refused_for_the_agent!(
    the_agent_token_cannot_approve,
    "POST",
    nymirad::approvals::PATH
);
refused_for_the_agent!(
    the_agent_token_cannot_list_approvals,
    "GET",
    nymirad::approvals::PATH
);
refused_for_the_agent!(
    the_agent_token_cannot_make_or_change_tokens,
    "POST",
    "/v1/nymira/tokens"
);
refused_for_the_agent!(
    the_agent_token_cannot_export_secrets,
    "GET",
    "/v1/nymira/export"
);
refused_for_the_agent!(
    the_agent_token_cannot_back_up_or_restore,
    "POST",
    "/v1/backup"
);
refused_for_the_agent!(the_agent_token_cannot_fund, "POST", "/v1/deposit");
refused_for_the_agent!(the_agent_token_cannot_revoke, "POST", "/v1/nymira/revoke");

#[tokio::test]
async fn the_owner_token_cannot_revoke_over_http_either() {
    let home = tempfile::tempdir().unwrap();
    let registry = Registry::at(home.path());
    register(&registry, home.path(), "a", "0x01");
    let owner = Arc::new(OwnerToken::load_or_create(&home.path().join("o.token")).unwrap());
    let text = std::fs::read_to_string(home.path().join("o.token")).unwrap();
    let app = router_for(owner, None, registry, Side::Tcp);
    let (status, reply) = call(&app, "POST", "/v1/nymira/revoke", &text, Some(json!({}))).await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::FORBIDDEN, "owner_route_forbidden")
    );
}

// ---- the expiry guard on a budget note ----------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn in_the_last_five_days_agent_requests_get_note_expiring_and_the_guard_closes_the_note() {
    // The vault's notes live four days: the budget note is inside the window from the start.
    let rig = Rig::new(Net::Direct, 4).await;
    submitter(&rig.s, "personal", NearToken::from_near(5)).await;
    let seen = rig.mark();

    let (status, reply) = rig.agent_chat().await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::PAYMENT_REQUIRED, "note_expiring"),
        "{reply}"
    );
    rig.quiet_since(seen);

    // The owner gets a notice, then the usual close follows and pays the payout account.
    let before = rig.s.usdc.balance_of(rig.s.alice.id()).await;
    let events = rig.s.gateway.guard_tick().await;
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Notice(t) if t.contains("note 0 expires"))),
        "{events:?}"
    );
    assert!(events.contains(&Event::Closed(0)), "{events:?}");
    assert_eq!(rig.s.usdc.balance_of(rig.s.alice.id()).await, before + ONE);
    // A closed budget note serves nothing.
    let (status, reply) = rig.agent_chat().await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::PAYMENT_REQUIRED, "note_expiring")
    );
    rig.quiet_since(seen);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_budget_note_whose_close_fails_gets_an_exit_three_days_before_expiry() {
    let rig = Rig::new(Net::Direct, 4).await;
    submitter(&rig.s, "personal", NearToken::from_near(5)).await;
    let expiry = entry(&rig.s).expiry_ns.unwrap();
    rig.s
        .op
        .set_emergency("the clearance key may be exposed")
        .await
        .unwrap();

    let events = rig.s.gateway.guard_tick().await;
    assert!(
        events.iter().any(|e| matches!(e, Event::CloseFailed(0, _))),
        "{events:?}"
    );
    let (status, reply) = rig.agent_chat().await;
    assert_eq!(
        (status, code(&reply)),
        (StatusCode::PAYMENT_REQUIRED, "note_expiring"),
        "{reply}"
    );
    let events = rig
        .s
        .gateway
        .guard_tick_at(expiry - 2 * DAY - DAY / 2)
        .await;
    assert!(events.contains(&Event::ExitFiled(0)), "{events:?}");
    assert_eq!(status_of(&rig.s, &rig.note).await, "PendingExit");
    assert_eq!(rig.inside().await.unwrap_err().code, "note_expiring");
}
