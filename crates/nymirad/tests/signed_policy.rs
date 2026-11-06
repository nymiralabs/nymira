//! The signed policy: what loads, what counts as no policy, and the files it leaves behind.
//! The key check runs against the fake NEAR RPC; the model allowlist on a live lease is in
//! `agents.rs`, which has the mock stack.

#[path = "support/policy.rs"]
mod sp;

use std::net::SocketAddr;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Mutex;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::{Request, StatusCode};
use ed25519_dalek::SigningKey;
use nymira_fake_near_rpc::{AccessKeySpec, AccountSpec, FakeRpc};
use nymirad::agents::{Registry, Spec, Transport};
use nymirad::guard::{router_for, Side};
use nymirad::policy;
use nymirad::rpc::{Rpc, Transport as Net};
use nymirad::token::OwnerToken;
use serde_json::{json, Value};
use sp::*;
use tower::ServiceExt;

const MODEL: &str = "z-ai/glm-5.3-flash";

struct Rig {
    home: tempfile::TempDir,
    registry: Registry,
    router: axum::Router,
    token: String,
    rpc: FakeRpc,
    owner_key: SigningKey,
    call_key: SigningKey,
}

impl Rig {
    async fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let registry = Registry::at(home.path());
        Spec {
            name: "cursor".into(),
            transport: Transport::Tcp,
            uid: None,
            group: None,
            gid: None,
            token_file: home.path().join("cursor.token"),
        }
        .register(&registry, "0xabc", OWNER, 1)
        .unwrap();
        let token = std::fs::read_to_string(home.path().join("cursor.token")).unwrap();
        Spec {
            name: "other".into(),
            transport: Transport::Tcp,
            uid: None,
            group: None,
            gid: None,
            token_file: home.path().join("other.token"),
        }
        .register(&registry, "0xdef", OWNER, 1)
        .unwrap();
        let owner = std::sync::Arc::new(
            OwnerToken::load_or_create(&home.path().join("owner.token")).unwrap(),
        );
        let router = router_for(owner, None, registry.clone(), Side::Tcp);
        let (owner_key, call_key) = (
            SigningKey::from_bytes(&[1u8; 32]),
            SigningKey::from_bytes(&[2u8; 32]),
        );
        let rpc = FakeRpc::start().await;
        rpc.set_account(OWNER, AccountSpec::default());
        rpc.set_access_keys(
            OWNER,
            vec![
                AccessKeySpec::full_access(&public_key(&owner_key), 0),
                AccessKeySpec::function_call(&public_key(&call_key), 0, "x.test", &["m"]),
            ],
        );
        Self {
            home,
            registry,
            router,
            token,
            rpc,
            owner_key,
            call_key,
        }
    }

    fn rpcs(&self) -> Vec<Rpc> {
        vec![Rpc::new(self.rpc.url(), &Net::Direct).unwrap()]
    }

    fn signed(&self, key: &SigningKey, version: u32, text: &str) -> Vec<u8> {
        signed(&self.registry, "cursor", key, version, text)
    }

    async fn load(&self, bytes: &[u8]) -> Result<policy::Loaded, String> {
        policy::load(
            &self.registry,
            "cursor",
            bytes,
            &ctx(),
            &self.rpcs(),
            &|_| true,
        )
        .await
    }

    async fn ask(&self, method: &str, path: &str, body: Value) -> (StatusCode, Value) {
        let mut request = Request::builder()
            .method(method)
            .uri(path)
            .header("host", "localhost:8788")
            .header("authorization", format!("Bearer {}", self.token))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        request.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:50000".parse::<SocketAddr>().unwrap(),
        ));
        let reply = self.router.clone().oneshot(request).await.unwrap();
        let status = reply.status();
        let bytes = axum::body::to_bytes(reply.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    /// Every agent route is refused with `policy_missing`.
    async fn refused(&self, why: &str) {
        for (m, p) in [("POST", "/v1/chat/completions"), ("GET", "/v1/models")] {
            let (status, reply) = self
                .ask(m, p, json!({"model": MODEL, "messages": []}))
                .await;
            assert_eq!(
                (status, reply["error"]["code"].as_str()),
                (StatusCode::PAYMENT_REQUIRED, Some("policy_missing")),
                "{why}: {m} {p}"
            );
        }
    }

    fn version(&self) -> Option<u32> {
        self.registry.load().unwrap().agents[0].policy_version
    }
}

#[tokio::test]
async fn each_bad_policy_means_every_agent_request_is_refused() {
    let r = Rig::new().await;
    let good = yaml(&[MODEL]);
    r.refused("no policy at all").await;

    let mut cases: Vec<(&str, Vec<u8>)> = Vec::new();
    // Unsigned: a well-formed file whose signature is zeros.
    let mut unsigned = sign_for(
        &r.owner_key,
        OWNER,
        &policy::recipient("cursor", "0xabc"),
        1,
        &good,
    );
    unsigned["signature"] = json!(base64_zeros());
    cases.push(("unsigned", serde_json::to_vec(&unsigned).unwrap()));
    // Changed after signing: a looser limit in the signed message.
    let mut changed = sign_for(
        &r.owner_key,
        OWNER,
        &policy::recipient("cursor", "0xabc"),
        1,
        &good,
    );
    let looser = json!({"kind": "nymira-policy", "version": 1, "policy": good.replace("max_daily_usd: 2", "max_daily_usd: 2000")});
    changed["message"] = json!(looser.to_string());
    cases.push((
        "changed after signing",
        serde_json::to_vec(&changed).unwrap(),
    ));
    // Another account's key and name.
    let mallory = SigningKey::from_bytes(&[3u8; 32]);
    cases.push((
        "signed by another account",
        serde_json::to_vec(&sign_for(
            &mallory,
            "mallory.test",
            &policy::recipient("cursor", "0xabc"),
            1,
            &good,
        ))
        .unwrap(),
    ));
    // The owner's name on a key that is not one of the owner's keys.
    cases.push((
        "a key the owner does not have",
        r.signed(&mallory, 1, &good),
    ));
    // A function-call key of the owner.
    cases.push(("function-call key", r.signed(&r.call_key, 1, &good)));
    // Another agent's recipient tag, signed with the owner's real key.
    cases.push((
        "signed for another agent",
        serde_json::to_vec(&sign_for(
            &r.owner_key,
            OWNER,
            &policy::recipient("other", "0xdef"),
            1,
            &good,
        ))
        .unwrap(),
    ));
    // The right agent name on the wrong note.
    cases.push((
        "signed for another note",
        serde_json::to_vec(&sign_for(
            &r.owner_key,
            OWNER,
            &policy::recipient("cursor", "0xdef"),
            1,
            &good,
        ))
        .unwrap(),
    ));
    cases.push((
        "missing a limit key",
        r.signed(
            &r.owner_key,
            1,
            &good.replace("rate_limit_per_min: 30\n", ""),
        ),
    ));
    cases.push((
        "an unknown key",
        r.signed(&r.owner_key, 1, &format!("{good}spend_everything: yes\n")),
    ));
    cases.push((
        "a repeated key",
        r.signed(&r.owner_key, 1, &format!("{good}max_call_usd: 9\n")),
    ));
    cases.push((
        "paying sellers before they exist",
        r.signed(
            &r.owner_key,
            1,
            &format!("{good}allowed_origins:\n  - shop.example\n"),
        ),
    ));
    cases.push((
        "an anchor",
        r.signed(
            &r.owner_key,
            1,
            &good.replace("max_call_usd: 0.05", "max_call_usd: &a 0.05"),
        ),
    ));
    cases.push((
        "an unpinned model",
        r.signed(&r.owner_key, 1, &yaml(&["evil/model"])),
    ));
    cases.push(("not json", b"allowed_models: [x]".to_vec()));
    for (why, bytes) in &cases {
        assert!(r.load(bytes).await.is_err(), "{why} must not load");
        assert_eq!(r.version(), None, "{why}");
        assert!(
            !r.home.path().join(policy::file_name("cursor")).exists(),
            "{why}"
        );
        r.refused(why).await;
    }

    // A lower or equal version after a good one.
    r.load(&r.signed(&r.owner_key, 2, &good)).await.unwrap();
    assert_eq!(r.version(), Some(2));
    let (status, _) = r.ask("GET", "/v1/models", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    for v in [1, 2] {
        let err = r.load(&r.signed(&r.owner_key, v, &good)).await.unwrap_err();
        assert!(err.contains("does not rise"), "{err}");
        assert_eq!(r.version(), Some(2));
    }
    let (status, _) = r.ask("GET", "/v1/models", json!({})).await;
    assert_eq!(status, StatusCode::OK, "the refused loads left v2 in force");
}

fn base64_zeros() -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode([0u8; 64])
}

#[tokio::test]
async fn a_policy_that_is_altered_on_disk_or_swapped_for_an_old_one_stops_counting() {
    let r = Rig::new().await;
    let file = r.home.path().join(policy::file_name("cursor"));
    let v1 = r.signed(&r.owner_key, 1, &yaml(&[MODEL]));
    let v2 = r.signed(&r.owner_key, 2, &yaml(&[MODEL]));
    r.load(&v1).await.unwrap();
    r.load(&v2).await.unwrap();
    let (status, _) = r.ask("GET", "/v1/models", json!({})).await;
    assert_eq!(status, StatusCode::OK);

    // Someone edits the stored file: the signature no longer matches.
    let text = String::from_utf8(std::fs::read(&file).unwrap()).unwrap();
    std::fs::write(&file, text.replace("max_daily_usd: 2", "max_daily_usd: 9")).unwrap();
    r.refused("edited file").await;

    // A genuine older policy put back does not match the confirmed version.
    std::fs::write(&file, &v1).unwrap();
    r.refused("older signed policy").await;

    // The confirmed one put back works again.
    std::fs::write(&file, &v2).unwrap();
    let (status, _) = r.ask("GET", "/v1/models", json!({})).await;
    assert_eq!(status, StatusCode::OK);

    // A file nobody confirmed is not in force, even if its signature is good.
    r.registry
        .update(|b| {
            b.agents[0].policy_version = None;
            Ok(())
        })
        .unwrap();
    r.refused("unconfirmed file").await;
}

#[tokio::test]
async fn loading_asks_first_shows_the_policy_and_leaves_a_file_only_the_owner_can_write() {
    let r = Rig::new().await;
    let bytes = r.signed(&r.owner_key, 1, &yaml(&[MODEL]));
    let shown = Mutex::new(String::new());

    // Declined: nothing is stored and nothing changes.
    let err = policy::load(&r.registry, "cursor", &bytes, &ctx(), &r.rpcs(), &|s| {
        *shown.lock().unwrap() = s.to_string();
        false
    })
    .await
    .unwrap_err();
    assert!(err.contains("not confirmed"), "{err}");
    assert_eq!(r.version(), None);
    assert!(!r.home.path().join(policy::file_name("cursor")).exists());
    r.refused("declined").await;

    let summary = shown.lock().unwrap().clone();
    for part in [
        MODEL,
        "Policy v1",
        "agent cursor",
        OWNER,
        "0.5 USD",
        "0.05 USD",
        "2 USD",
        "30 requests",
        "sellers it may pay: none",
    ] {
        assert!(summary.contains(part), "{part} in {summary}");
    }

    // Confirmed: it applies to the very next request.
    r.load(&bytes).await.unwrap();
    assert_eq!(r.version(), Some(1));
    let (status, _) = r.ask("GET", "/v1/models", json!({})).await;
    assert_eq!(status, StatusCode::OK);
    let file = r.home.path().join(policy::file_name("cursor"));
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    // SAFETY: geteuid has no preconditions.
    assert_eq!(
        std::os::unix::fs::MetadataExt::uid(&std::fs::metadata(&file).unwrap()),
        unsafe { libc::geteuid() }
    );
    assert_eq!(
        std::fs::metadata(r.home.path())
            .unwrap()
            .permissions()
            .mode()
            & 0o022,
        0
    );

    // A state directory open to others is refused.
    let open = tempfile::tempdir().unwrap();
    std::fs::set_permissions(open.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
    let reg = Registry::at(open.path());
    Spec {
        name: "a".into(),
        transport: Transport::Tcp,
        uid: None,
        group: None,
        gid: None,
        token_file: open.path().join("a.token"),
    }
    .register(&reg, "0x01", OWNER, 1)
    .unwrap();
    let b = signed(&reg, "a", &r.owner_key, 1, &yaml(&[MODEL]));
    assert!(policy::activate(&reg, "a", &b, &ctx())
        .unwrap_err()
        .contains("writable by others"));
}

#[tokio::test]
async fn the_signature_is_checked_before_the_policy_text_is_read() {
    let r = Rig::new().await;
    // Anchors and tags in the text, and a bad signature: the refusal is about the signature.
    let mut bad = sign_for(
        &r.owner_key,
        OWNER,
        &policy::recipient("cursor", "0xabc"),
        1,
        "x: &a !!map {}\n<<: *a\n",
    );
    bad["signature"] = json!(base64_zeros());
    let err = r
        .load(&serde_json::to_vec(&bad).unwrap())
        .await
        .unwrap_err();
    assert!(err.contains("signature"), "{err}");
    // With a good signature the same text is refused by the restricted format.
    let err = r
        .load(&r.signed(&r.owner_key, 1, "x: &a !!map {}\n<<: *a\n"))
        .await
        .unwrap_err();
    assert!(
        err.contains("unknown key") || err.contains("plain word"),
        "{err}"
    );
}

#[tokio::test]
async fn a_key_the_endpoints_disagree_about_is_refused() {
    let r = Rig::new().await;
    // A second endpoint where the owner's key is only a function-call key.
    let other = FakeRpc::start().await;
    other.set_account(OWNER, AccountSpec::default());
    other.set_access_keys(
        OWNER,
        vec![AccessKeySpec::function_call(
            &public_key(&r.owner_key),
            0,
            "x.test",
            &["m"],
        )],
    );
    let rpcs = vec![
        Rpc::new(r.rpc.url(), &Net::Direct).unwrap(),
        Rpc::new(other.url(), &Net::Direct).unwrap(),
    ];
    let bytes = r.signed(&r.owner_key, 1, &yaml(&[MODEL]));
    let err = policy::load(&r.registry, "cursor", &bytes, &ctx(), &rpcs, &|_| true)
        .await
        .unwrap_err();
    assert!(err.contains("function-call"), "{err}");
    assert!(
        policy::load(&r.registry, "cursor", &bytes, &ctx(), &[], &|_| true)
            .await
            .is_err()
    );
    r.refused("endpoints disagree").await;
}

/// A second endpoint that holds the same keys as the rig's own, at the same height.
async fn twin(r: &Rig) -> FakeRpc {
    let twin = FakeRpc::start().await;
    twin.set_account(OWNER, AccountSpec::default());
    twin.set_access_keys(
        OWNER,
        vec![AccessKeySpec::full_access(&public_key(&r.owner_key), 0)],
    );
    r.rpc.advance_blocks(5, 1.0);
    twin.advance_blocks(5, 1.0);
    twin
}

fn views_of(rpc: &FakeRpc) -> Vec<Value> {
    rpc.requests()
        .into_iter()
        .filter(|q| q.params["request_type"] == "view_access_key_list")
        .map(|q| q.params)
        .collect()
}

#[tokio::test]
async fn every_endpoint_is_read_at_the_one_final_block_the_first_names() {
    let r = Rig::new().await;
    let other = twin(&r).await;
    let rpcs = vec![
        Rpc::new(r.rpc.url(), &Net::Direct).unwrap(),
        Rpc::new(other.url(), &Net::Direct).unwrap(),
    ];
    let key = public_key(&r.owner_key);
    policy::check_key(&rpcs, OWNER, &key).await.unwrap();

    let (first, second) = (views_of(&r.rpc), views_of(&other));
    assert_eq!((first.len(), second.len()), (1, 1));
    let hash = first[0]["block_id"].as_str().expect("pinned by hash");
    assert_eq!(second[0]["block_id"], hash, "both read the same block");
    assert!(first[0].get("finality").is_none() && second[0].get("finality").is_none());
    // Only the first endpoint was asked which block is final.
    let blocks = |f: &FakeRpc| f.requests().iter().filter(|q| q.method == "block").count();
    assert_eq!((blocks(&r.rpc), blocks(&other)), (1, 0));
}

#[tokio::test]
async fn an_endpoint_that_lags_or_sits_on_another_fork_refuses_the_key() {
    let r = Rig::new().await;
    let key = public_key(&r.owner_key);
    let rpcs = |other: &FakeRpc| {
        vec![
            Rpc::new(r.rpc.url(), &Net::Direct).unwrap(),
            Rpc::new(other.url(), &Net::Direct).unwrap(),
        ]
    };

    // Lagging: the second endpoint has not reached the block the first calls final.
    let other = twin(&r).await;
    policy::check_key(&rpcs(&other), OWNER, &key).await.unwrap();
    r.rpc.advance_blocks(3, 1.0);
    let err = policy::check_key(&rpcs(&other), OWNER, &key)
        .await
        .unwrap_err();
    assert!(err.contains("access keys of owner.test at block"), "{err}");
    // It catches up and the same check passes.
    other.advance_blocks(3, 1.0);
    policy::check_key(&rpcs(&other), OWNER, &key).await.unwrap();

    // Forked: same height, another block hash.
    other.set_fork(Some(nymira_fake_near_rpc::ForkSpec {
        from_height: 1,
        tag: "other-fork".into(),
    }));
    let err = policy::check_key(&rpcs(&other), OWNER, &key)
        .await
        .unwrap_err();
    assert!(err.contains("access keys of owner.test at block"), "{err}");

    // The same refusal reaches a signed policy: nothing is loaded.
    let bytes = r.signed(&r.owner_key, 1, &yaml(&[MODEL]));
    assert!(policy::load(
        &r.registry,
        "cursor",
        &bytes,
        &ctx(),
        &rpcs(&other),
        &|_| true
    )
    .await
    .is_err());
    r.refused("an endpoint on another fork").await;
}

#[tokio::test]
async fn after_a_restore_the_higher_of_the_backup_and_the_last_seen_version_counts() {
    let r = Rig::new().await;
    let good = yaml(&[MODEL]);
    r.load(&r.signed(&r.owner_key, 3, &good)).await.unwrap();
    let seen = || r.registry.load().unwrap().agents[0].policy_seen;
    assert_eq!(seen(), 3);
    // The owner last saw 5 elsewhere: that is the floor now.
    r.registry.raise_policy_seen("cursor", 5).unwrap();
    assert_eq!(seen(), 5);
    // A lower last-seen value never lowers it.
    r.registry.raise_policy_seen("cursor", 2).unwrap();
    assert_eq!(seen(), 5);
    let err = r.load(&r.signed(&r.owner_key, 4, &good)).await.unwrap_err();
    assert!(err.contains("does not rise"), "{err}");
    r.load(&r.signed(&r.owner_key, 6, &good)).await.unwrap();
    assert_eq!((seen(), r.version()), (6, Some(6)));
}

#[tokio::test]
async fn a_restored_policy_file_and_registry_come_back_together() {
    // The backup carries agents.json and the policy file by name; a restore writes both flat
    // into an empty directory, and the policy is in force again.
    let r = Rig::new().await;
    r.load(&r.signed(&r.owner_key, 1, &yaml(&[MODEL])))
        .await
        .unwrap();
    let copy = tempfile::tempdir().unwrap();
    for name in [
        nymirad::agents::FILE.to_string(),
        policy::file_name("cursor"),
        "owner.token".to_string(),
    ] {
        copy_private(r.home.path(), copy.path(), &name);
    }
    let owner =
        std::sync::Arc::new(OwnerToken::load_or_create(&copy.path().join("owner.token")).unwrap());
    let again = Rig {
        router: router_for(owner, None, Registry::at(copy.path()), Side::Tcp),
        registry: Registry::at(copy.path()),
        home: copy,
        ..r
    };
    let (status, _) = again.ask("GET", "/v1/models", json!({})).await;
    assert_eq!(status, StatusCode::OK);
}

fn copy_private(from: &Path, to: &Path, name: &str) {
    std::fs::copy(from.join(name), to.join(name)).unwrap();
    std::fs::set_permissions(to.join(name), std::fs::Permissions::from_mode(0o600)).unwrap();
}
