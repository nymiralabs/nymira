//! Opening and retiring a lease against the operator's own harness, and against a scripted
//! operator for the refusals the real one only gives under load.

#[path = "../../nymira-operator/tests/common/mod.rs"]
mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use common::*;
use nymira_operator::status::status_message;
use nymirad::lease::{open_lease, Attempt, LeaseClient, LeaseError, Pinned};
use serde_json::{json, Value};
use zkapi_proof::compact::CompactSigner;
use zkapi_types::Felt252;

fn pk_json(seed: u64) -> Value {
    let p = CompactSigner::from_seed(&Felt252::from_u64(seed)).public_key();
    json!({"x": p.x.to_string(), "y": p.y.to_string()})
}

fn params() -> Value {
    json!({"bound_classes": ["100000", "500000"], "charge_step": "1000",
           "min_charge": "2000", "slot_rate": 1000})
}

fn client(url: &str) -> LeaseClient {
    LeaseClient::new(reqwest::Client::new(), url)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lease_opens_and_only_its_token_retires_it() {
    let rig = Rig::new(Setup::default()).await;
    let (pk, cfg) = (pk_json(1), params());
    let pinned = Pinned {
        operator_pk: &pk,
        config: &cfg,
    };
    let c = client(&rig.url);
    let now = T0;
    let lease = open_lease(&c, &pinned, || now, |_, _| async { Ok(rig.fresh(1)) })
        .await
        .unwrap();
    assert_eq!(lease.bound, 100_000);
    assert!(!format!("{lease:?}").contains(lease.token.expose()));

    // The id alone is no credential.
    let bare = rig
        .http
        .post(format!("{}/v2/openrouter/leases/{}", rig.url, lease.id))
        .send()
        .await
        .unwrap();
    assert_eq!(bare.status(), 401);
    c.retire(&lease).await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_status_signed_by_another_key_builds_no_proof() {
    let rig = Rig::new(Setup::default()).await;
    let (pk, cfg) = (pk_json(99), params());
    let built = AtomicUsize::new(0);
    let err = open_lease(
        &client(&rig.url),
        &Pinned {
            operator_pk: &pk,
            config: &cfg,
        },
        || T0,
        |_, _| async {
            built.fetch_add(1, Ordering::SeqCst);
            Ok(rig.fresh(1))
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(err, LeaseError::Gate(_)), "{err}");
    assert_eq!(
        built.load(Ordering::SeqCst),
        0,
        "no proof before the nonce verifies"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn different_lease_parameters_build_no_proof() {
    let rig = Rig::new(Setup::default()).await;
    let pk = pk_json(1);
    let mut cfg = params();
    cfg["min_charge"] = json!("1");
    let built = AtomicUsize::new(0);
    let err = open_lease(
        &client(&rig.url),
        &Pinned {
            operator_pk: &pk,
            config: &cfg,
        },
        || T0,
        |_, _| async {
            built.fetch_add(1, Ordering::SeqCst);
            Ok(rig.fresh(1))
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("min_charge"), "{err}");
    assert_eq!(built.load(Ordering::SeqCst), 0);
}

/// An operator that signs its status like the real one and answers leases from a script.
#[derive(Clone)]
struct Script {
    answers: Arc<Mutex<Vec<(u16, Value)>>>,
    puzzles: Arc<Mutex<Vec<Option<String>>>>,
    tokens: Arc<Mutex<Vec<Option<String>>>>,
    difficulty: u64,
}

async fn script_status(State(s): State<Script>) -> Json<Value> {
    let signer = CompactSigner::from_seed(&Felt252::from_u64(1));
    let ts = T0 / 5 * 5;
    let sig = signer.sign(&status_message("aabbccdd", "operator.test", ts));
    let mut doc = json!({
        "nonce": {"nonce": "aabbccdd", "host": "operator.test", "ts": ts},
        "nonce_sig": sig,
        "admission": {"puzzle_difficulty": s.difficulty},
    });
    doc.as_object_mut()
        .unwrap()
        .extend(params().as_object().unwrap().clone());
    Json(doc)
}

async fn script_lease(State(s): State<Script>, headers: HeaderMap) -> (StatusCode, Json<Value>) {
    let header = |n: &str| {
        headers
            .get(n)
            .and_then(|v| v.to_str().ok())
            .map(String::from)
    };
    s.puzzles.lock().unwrap().push(header("x-nymira-puzzle"));
    s.tokens
        .lock()
        .unwrap()
        .push(header("x-nymira-lease-token"));
    let (code, body) = s.answers.lock().unwrap().remove(0);
    (StatusCode::from_u16(code).unwrap(), Json(body))
}

fn error(code: &str) -> Value {
    json!({"error": {"code": code, "message": "scripted"}})
}

fn created() -> Value {
    json!({"status": "active", "client_request_id": "c", "issued_at": T0, "expires_at": T0 + 900,
           "lease_id": "lease-1", "lease_token": "tok", "bound": "100000"})
}

async fn scripted(answers: Vec<(u16, Value)>, difficulty: u64) -> (String, Script) {
    let s = Script {
        answers: Arc::new(Mutex::new(answers)),
        puzzles: Arc::default(),
        tokens: Arc::default(),
        difficulty,
    };
    let app = Router::new()
        .route("/v1/status", get(script_status))
        .route("/v2/openrouter/leases", post(script_lease))
        .with_state(s.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, s)
}

async fn run(url: &str) -> (Result<nymirad::lease::Lease, LeaseError>, Vec<Attempt>) {
    let (pk, cfg) = (pk_json(1), params());
    let attempts = Mutex::new(Vec::new());
    let times = Mutex::new(Vec::new());
    let result = open_lease(
        &client(url),
        &Pinned {
            operator_pk: &pk,
            config: &cfg,
        },
        || T0,
        |a, request_time| {
            times.lock().unwrap().push(request_time);
            attempts.lock().unwrap().push(a);
            let r = variant(1, 100_000, T0, &root_hex());
            async move { Ok(r) }
        },
    )
    .await;
    // Every proof is stamped with the operator's time on the grid, not the client's clock.
    for t in times.into_inner().unwrap() {
        assert_eq!(
            t,
            T0 / nymirad::gate::TIME_GRID_SECS * nymirad::gate::TIME_GRID_SECS
        );
    }
    (result, attempts.into_inner().unwrap())
}

#[tokio::test(flavor = "multi_thread")]
async fn root_stale_rebuilds_once_and_a_second_one_is_reported() {
    let (url, _) = scripted(vec![(409, error("root_stale")), (201, created())], 0).await;
    let (r, attempts) = run(&url).await;
    r.unwrap();
    assert_eq!(attempts, vec![Attempt::First, Attempt::Rebuild]);

    let (url, _) = scripted(
        vec![(409, error("root_stale")), (409, error("root_stale"))],
        0,
    )
    .await;
    let (r, attempts) = run(&url).await;
    assert!(matches!(r, Err(LeaseError::Operator(e)) if e.code == "root_stale"));
    assert_eq!(attempts.len(), 2, "built twice, never looped");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_puzzle_is_sent_when_advertised_and_retried_once_when_required() {
    let (url, s) = scripted(vec![(201, created())], 8).await;
    run(&url).await.0.unwrap();
    let sent = s.puzzles.lock().unwrap()[0].clone().expect("puzzle header");
    assert!(nymira_operator::puzzle::check(
        &sent,
        8,
        &["aabbccdd".to_string()]
    ));

    // Not advertised, then required: one retry carrying a solution.
    let (url, s) = scripted(vec![(429, error("puzzle_required")), (201, created())], 0).await;
    let (r, _) = run(&url).await;
    r.unwrap();
    assert_eq!(s.puzzles.lock().unwrap().len(), 2);

    let (url, s) = scripted(
        vec![
            (429, error("puzzle_required")),
            (429, error("puzzle_required")),
        ],
        0,
    )
    .await;
    assert!(run(&url).await.0.is_err());
    assert_eq!(s.puzzles.lock().unwrap().len(), 2, "one retry only");
}

#[tokio::test(flavor = "multi_thread")]
async fn other_errors_come_back_by_code_with_their_hint() {
    let busy =
        json!({"error": {"code": "operator_capacity", "message": "full", "retry_after_secs": 7}});
    let (url, _) = scripted(vec![(429, busy)], 0).await;
    match run(&url).await.0 {
        Err(LeaseError::Operator(e)) => {
            assert_eq!(
                (e.status, e.code.as_str(), e.retry_after_secs),
                (429, "operator_capacity", Some(7))
            )
        }
        other => panic!("{other:?}"),
    }
    let (url, _) = scripted(vec![(418, error("teapot"))], 0).await;
    assert!(matches!(run(&url).await.0, Err(LeaseError::Operator(e)) if e.status == 418));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wallet_proves_the_lease_on_the_operators_time_and_installs_the_next_state() {
    use nymira_near_codec::chain_id;
    use zkapi_client::config::ClientConfig;
    use zkapi_client::wallet::Wallet;
    use zkapi_core::v2 as core;
    use zkapi_types::MERKLE_DEPTH;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let rig = Rig::new(Setup {
        verifier: Verifier::Real,
        clock_secs: now,
        ..Setup::default()
    })
    .await;
    let dir = tempfile::tempdir().unwrap();
    let mut wallet = Wallet::new(ClientConfig {
        protocol_version: 2,
        chain_id: chain_id(network()),
        contract_address: contract(),
        request_charge_cap: 100_000,
        policy_charge_cap: 100_000,
        policy_enabled: false,
        server_url: rig.url.clone(),
        state_dir: dir.path().to_string_lossy().into_owned(),
        proof_mode: zkapi_client::config::ClientProofMode::Groth16 {
            setup_dir: SETUP_DIR.into(),
        },
        state_signing_key: CompactSigner::from_seed(&Felt252::from_u64(1)).public_key(),
        clearance_signing_key: CompactSigner::from_seed(&Felt252::from_u64(2)).public_key(),
        settlement_window_seconds: 21_905,
    })
    .unwrap();
    wallet
        .confirm_deposit(Felt252::from_u64(42), 0, 2_000_000, EXPIRY)
        .unwrap();

    let (pk, cfg) = (pk_json(1), params());
    let c = client(&rig.url);
    let siblings = core::zero_hashes()[..MERKLE_DEPTH].to_vec();
    let wallet = Mutex::new(wallet);
    let lease = open_lease(
        &c,
        &Pinned {
            operator_pk: &pk,
            config: &cfg,
        },
        || now,
        |attempt, request_time| {
            assert_eq!(request_time % nymirad::gate::TIME_GRID_SECS, 0);
            let mut w = wallet.lock().unwrap();
            if attempt == Attempt::Rebuild {
                nymirad::lease::set_aside_stale(&w).unwrap();
            }
            let r = nymirad::lease::prepare(
                &mut w,
                tree().root(),
                siblings.clone(),
                100_000,
                request_time,
            );
            async move { r }
        },
    )
    .await
    .unwrap();

    // Retire, settle, and the wallet installs the signed next state.
    rig.provider.set_usage(&lease.id, 40_000_000);
    c.retire(&lease).await.unwrap();
    rig.op.tick().await;
    rig.clock.advance_secs(11);
    rig.op.tick().await;
    rig.op.tick().await;
    let mut w = wallet.into_inner().unwrap();
    let response = w
        .recover_pending()
        .await
        .unwrap()
        .expect("finalized response");
    assert_eq!(response.charge_applied, 42_000);
    assert_eq!(w.state().unwrap().current_balance, 2_000_000 - 42_000);
    // The wallet's state holds the bound it proved at.
    assert_eq!(
        w.state().unwrap().bound_at_state,
        0,
        "a new state, no bound yet"
    );
}
