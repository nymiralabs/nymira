//! Opt-in mode: a budget note moved to the agent's own machine. Against the mock stack: a
//! sandbox vault, the real operator behind the fake NEAR AI gateway, and two `nymirad` gateways,
//! the owner's and the agent machine's, each over its own state directory.

#[path = "support/policy.rs"]
mod policy;
#[path = "support/stack.rs"]
mod stack;

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use near_workspaces::types::{AccessKey, NearToken, PublicKey};
use nymira_near_codec::{chain_id, contract_address, Network};
use nymirad::agents::{Registry, Scope, Spec, Transport};
use nymirad::backup;
use nymirad::gateway::{ChatOptions, Gateway};
use nymirad::guard::{router_for, Side};
use nymirad::optin::{self, MoveRequest, OperatorSpent, Verify};
use nymirad::pins::read_live;
use nymirad::policy::Policy;
use nymirad::rpc::{Rpc, Transport as Net};
use nymirad::token::OwnerToken;
use serde_json::{json, Value};
use stack::*;
use tower::ServiceExt;
use zkapi_types::wire::CurvePointWire;
use zkapi_types::Felt252;

fn chat_body(text: &str) -> Value {
    json!({"model": MODEL, "stream": false, "max_tokens": 20,
        "messages": [{"role": "user", "content": text}]})
}

fn scope(note: &str) -> Scope {
    Scope::Agent {
        name: "a".into(),
        note: note.into(),
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

async fn http(
    app: &axum::Router,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost:8788")
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.map(|v| v.to_string()).unwrap_or_default()))
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:50000".parse::<SocketAddr>().unwrap(),
    ));
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

/// Retires the open lease on `gw` and lets the operator settle it.
async fn settle(s: &Stack, gw: &Gateway) {
    gw.reap(true).await;
    for _ in 0..3 {
        s.op.tick().await;
        s.clock.advance_secs(11);
    }
    s.op.tick().await;
    s.op.tick().await;
    gw.settle().await;
}

struct Moved {
    s: Stack,
    owner_registry: Registry,
    owner_app: axum::Router,
    owner_token: String,
    agent_token: String,
    note: String,
    owner_priv: String,
    owner_pub: String,
    agent_priv: String,
    agent_pub: String,
    states: tempfile::TempDir,
    package: PathBuf,
}

impl Moved {
    /// The owner's stack with one budget note and a policy, not yet moved.
    async fn before_the_move() -> Self {
        let s = build(Net::Direct, true).await;
        let home = s.home.path().to_path_buf();
        let note = entry(&s).commitment.clone();
        let registry = Registry::at(&home);
        Spec {
            name: "a".into(),
            transport: Transport::Tcp,
            uid: None,
            group: None,
            gid: None,
            token_file: home.join("tokens").join("a.token"),
        }
        .register(&registry, &note, "owner.test", 1)
        .unwrap();
        policy::give_policy_text(
            &registry,
            "a",
            1,
            &policy::yaml_limits(&[MODEL], "0.5", "0.05", "2", 30),
        );
        let token_path = home.join("owner.token");
        let owner = Arc::new(OwnerToken::load_or_create(&token_path).unwrap());
        let owner_app = router_for(owner, Some(s.gateway.clone()), registry.clone(), Side::Tcp);
        let (owner_priv, owner_pub) = backup::generate_identity();
        let (agent_priv, agent_pub) = backup::generate_identity();
        Self {
            owner_token: std::fs::read_to_string(&token_path).unwrap(),
            agent_token: std::fs::read_to_string(home.join("tokens").join("a.token")).unwrap(),
            owner_registry: registry,
            owner_app,
            note,
            owner_priv: owner_priv.to_string(),
            owner_pub,
            agent_priv: agent_priv.to_string(),
            agent_pub,
            states: tempfile::tempdir().unwrap(),
            package: home.join("move.age"),
            s,
        }
    }

    fn request(&self) -> MoveRequest {
        MoveRequest {
            agent: "a".into(),
            agent_recipient: self.agent_pub.clone(),
            owner_recipient: self.owner_pub.clone(),
            places: vec![self.states.path().into()],
            out: self.package.clone(),
        }
    }

    fn do_move(&self) -> Result<String, String> {
        optin::move_note(
            self.s.home.path(),
            PASS,
            10,
            &self.request(),
            1,
            &mut |_| true,
        )
    }

    /// The agent machine: a fresh state directory restored from the package, with its own gateway.
    async fn agent_machine(&self) -> (tempfile::TempDir, Arc<Gateway>) {
        let dir = tempfile::tempdir().unwrap();
        let state = backup::load(&[self.package.clone()], &self.agent_priv).unwrap();
        backup::restore_into(dir.path(), state, PASS, 10).unwrap();
        let gw = make_gateway(dir.path(), &self.s.parts, Net::Direct, self.s.clock.clone()).await;
        (dir, gw)
    }

    async fn verify(&self) -> Verify {
        let rpc = Rpc::new(self.s.parts.rpc_url.clone(), &Net::Direct).unwrap();
        let pins = read_live(&rpc, &self.s.parts.vault_id).await.unwrap();
        let point =
            |f: &str| Felt252::from_hex(pins.config["operator_pk"][f].as_str().unwrap()).unwrap();
        Verify {
            operator_key: CurvePointWire {
                x: point("x"),
                y: point("y"),
            },
            chain_id: chain_id(Network::Testnet),
            contract: Felt252(contract_address(&self.s.parts.vault_id)),
        }
    }

    fn spent_check(&self) -> OperatorSpent {
        OperatorSpent {
            base: self.s.parts.operator_url.clone(),
            http: reqwest::Client::new(),
        }
    }

    fn state_files(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(self.states.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".age"))
            .collect();
        names.sort();
        names
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn after_the_move_the_owners_copy_refuses_to_spend_and_only_the_owner_cli_moves() {
    let m = Moved::before_the_move().await;
    // Before the move the owner's copy serves the agent.
    let (status, reply) = http(
        &m.owner_app,
        "POST",
        "/v1/chat/completions",
        &m.agent_token,
        Some(chat_body("one")),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{reply}");
    settle(&m.s, &m.s.gateway).await;
    assert_eq!(m.s.gateway.records().len(), 1);

    m.do_move().unwrap();
    assert!(m.owner_registry.load().unwrap().agents[0].moved);

    let seen = (m.s.gateway.records().len(), m.s.fake.handle.chats().len());
    let (status, reply) = http(
        &m.owner_app,
        "POST",
        "/v1/chat/completions",
        &m.agent_token,
        Some(chat_body("two")),
    )
    .await;
    assert_eq!(
        (status, reply["error"]["code"].as_str()),
        (StatusCode::PAYMENT_REQUIRED, Some("note_moved")),
        "{reply}"
    );
    let inside =
        m.s.gateway
            .chat(
                chat_body("inside"),
                ChatOptions {
                    scope: scope(&m.note),
                    ..Default::default()
                },
            )
            .await
            .map(|_| ())
            .unwrap_err();
    assert_eq!(inside.code, "note_moved");
    // The owner's own token never selects a budget note either.
    let (status, _) = http(
        &m.owner_app,
        "POST",
        "/v1/chat/completions",
        &m.owner_token,
        Some(chat_body("owner")),
    )
    .await;
    assert_ne!(status, StatusCode::OK);
    assert_eq!(
        (m.s.gateway.records().len(), m.s.fake.handle.chats().len()),
        seen,
        "a request reached the operator after the move"
    );

    // One copy only: a second move is refused, and so is a note that is not settled.
    assert!(m
        .do_move()
        .unwrap_err()
        .contains("already on the agent's machine"));

    // No route moves a note: not for the agent token, not for the owner token over HTTP.
    assert!(nymirad::agents::AGENT_ROUTES
        .iter()
        .all(|(_, p)| !p.contains("move")));
    for token in [&m.agent_token, &m.owner_token] {
        for path in ["/v1/nymira/move", "/v1/nymira/agents/a/move"] {
            let (status, _) = http(&m.owner_app, "POST", path, token, Some(json!({}))).await;
            assert!(!status.is_success(), "{path} answered {status}");
        }
    }
    assert!(m.owner_registry.load().unwrap().agents[0].moved);
}

#[tokio::test(flavor = "multi_thread")]
async fn each_retire_on_the_agent_machine_writes_a_numbered_state_and_the_owner_imports_the_latest()
{
    let m = Moved::before_the_move().await;
    m.do_move().unwrap();
    let (agent_home, agent) = m.agent_machine().await;
    assert!(m.state_files().is_empty());

    for round in 1..=2u64 {
        agent
            .chat(
                chat_body("work"),
                ChatOptions {
                    scope: scope(&m.note),
                    ..Default::default()
                },
            )
            .await
            .unwrap_or_else(|e| panic!("{}: {}", e.code, e.message));
        settle(&m.s, &agent).await;
        let names = m.state_files();
        assert_eq!(names.len() as u64, round, "{names:?}");
        assert!(
            names.last().unwrap().ends_with(&format!("{round:010}.age")),
            "{names:?}"
        );
    }
    // Nothing on the agent machine's disk or in the files is readable without the owner's key.
    for name in m.state_files() {
        let bytes = std::fs::read(m.states.path().join(&name)).unwrap();
        assert!(bytes.starts_with(b"age-encryption.org/v1"));
        assert!(backup::decrypt(&m.agent_priv, &bytes).is_err());
    }

    let verify = m.verify().await;
    let report = optin::import_best(
        m.s.home.path(),
        PASS,
        10,
        "a",
        &[m.states.path().into()],
        &m.owner_priv,
        &verify,
        &m.spent_check(),
    )
    .await
    .unwrap();
    let best = report.best.as_ref().unwrap();
    assert_eq!((best.position, best.seq), (2, 2), "{:?}", report.lines());
    assert_eq!(report.stale.len(), 1, "{:?}", report.lines());
    assert!(report.installed);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);

    // The operator has spent the first state's nullifier: with only that file the import warns.
    std::fs::remove_file(m.states.path().join(m.state_files().pop().unwrap())).unwrap();
    let behind = optin::import_best(
        m.s.home.path(),
        PASS,
        10,
        "a",
        &[m.states.path().into()],
        &m.owner_priv,
        &verify,
        &m.spent_check(),
    )
    .await
    .unwrap();
    assert_eq!(behind.best.as_ref().unwrap().position, 1);
    assert!(
        behind
            .warnings
            .iter()
            .any(|w| w.contains("already counts the nullifier")),
        "{:?}",
        behind.warnings
    );
    drop(agent_home);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_holding_the_secret_that_closes_the_note_pays_only_the_owners_payout_account() {
    let m = Moved::before_the_move().await;
    m.do_move().unwrap();
    let (_agent_home, agent) = m.agent_machine().await;
    agent
        .chat(
            chat_body("spend a little"),
            ChatOptions {
                scope: scope(&m.note),
                ..Default::default()
            },
        )
        .await
        .unwrap();
    settle(&m.s, &agent).await;
    let charge: u128 = agent.records()[0]
        .charge
        .as_deref()
        .unwrap()
        .parse()
        .unwrap();
    assert!(charge > 0);

    // The agent's machine has its own submitter account, which is not the owner.
    let sub = {
        let account =
            m.s.worker
                .root_account()
                .unwrap()
                .create_subaccount("subagentfam")
                .initial_balance(NearToken::from_near(5))
                .transact()
                .await
                .unwrap()
                .into_result()
                .unwrap();
        let made = agent
            .submitters()
            .add("personal", account.id().as_str())
            .unwrap();
        let public: PublicKey = made.public_key().parse().unwrap();
        account
            .batch(account.id())
            .add_key(
                public,
                AccessKey::function_call_access(m.s.vault.id(), &nymirad::submitter::METHODS, None),
            )
            .transact()
            .await
            .unwrap()
            .into_result()
            .unwrap();
        m.s.worker.fast_forward(5).await.unwrap();
        account
    };
    let deposited: u128 = entry(&m.s)
        .deposit_amount
        .as_deref()
        .unwrap()
        .parse()
        .unwrap();
    let owner_before = m.s.usdc.balance_of(m.s.alice.id()).await;
    let sub_before = m.s.usdc.balance_of(sub.id()).await;
    let treasury_before = m.s.usdc.balance_of(m.s.treasury.id()).await;

    let id = entry(&m.s).note_id.unwrap();
    let closed = agent.close_note(id, no_delay(), None).await.unwrap();
    assert!(closed.confirmed);

    assert_eq!(status_of(&m.s, &m.note).await, "Closed");
    assert_eq!(
        m.s.usdc.balance_of(m.s.alice.id()).await,
        owner_before + deposited - charge,
        "the payout account gets what is left"
    );
    assert_eq!(
        m.s.usdc.balance_of(sub.id()).await,
        sub_before,
        "the sender gets nothing"
    );
    assert_eq!(
        m.s.usdc.balance_of(m.s.treasury.id()).await,
        treasury_before
    );
}
