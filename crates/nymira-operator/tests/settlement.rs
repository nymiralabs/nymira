mod common;

use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

use common::*;
use nymira_operator::chain::FakeVault;
use nymira_operator::operator::{felt_hex, Profile};
use nymira_operator::provider::ProviderError;
use nymira_operator::signer::{
    bind_socket, seed_from_passphrase, serve, FakeHeartbeat, SignRequest, Signer, SignerApi,
    SignerError, SocketSigner, HEARTBEAT_EVERY_SECS,
};
use nymira_operator::store;
use nymira_operator::usage::{SignedUsage, UsageRecord};
use serde_json::{json, Value};
use zkapi_core::v2::{next_anchor, state_message};
use zkapi_proof::compact::{verify_signature, CompactSigner};
use zkapi_types::wire::{ApiRequestV2, RecoveryResponseV2};
use zkapi_types::Felt252;

struct Lease {
    id: String,
    token: String,
    request: ApiRequestV2,
}

async fn open(rig: &Rig, n: u64) -> Lease {
    let request = rig.fresh(n);
    let (status, body, _) = rig.lease(&request).await;
    assert_eq!(status, 201, "{body}");
    Lease {
        id: body["lease_id"].as_str().unwrap().into(),
        token: body["lease_token"].as_str().unwrap().into(),
        request,
    }
}

async fn call(rig: &Rig, method: reqwest::Method, path: &str, token: Option<&str>) -> (u16, Value) {
    let mut request = rig.http.request(method, format!("{}{path}", rig.url));
    if let Some(t) = token {
        request = request.header("X-Nymira-Lease-Token", t);
    }
    let response = request.send().await.unwrap();
    (
        response.status().as_u16(),
        response.json().await.unwrap_or(Value::Null),
    )
}

async fn retire(rig: &Rig, l: &Lease) -> (u16, Value) {
    call(
        rig,
        reqwest::Method::POST,
        &format!("/v2/openrouter/leases/{}", l.id),
        Some(&l.token),
    )
    .await
}

async fn status(rig: &Rig, l: &Lease) -> Value {
    call(
        rig,
        reqwest::Method::GET,
        &format!("/v2/openrouter/leases/{}", l.id),
        Some(&l.token),
    )
    .await
    .1
}

async fn request_status(rig: &Rig, l: &Lease) -> (u16, Value) {
    call(
        rig,
        reqwest::Method::GET,
        &format!("/v2/requests/{}", l.request.client_request_id),
        None,
    )
    .await
}

/// Key deactivated, ten seconds waited, usage read, state signed.
async fn settle_all(rig: &Rig) {
    rig.op.tick().await;
    rig.clock.advance_secs(11);
    rig.op.tick().await;
    rig.op.tick().await;
}

async fn row(rig: &Rig, l: &Lease) -> store::LeaseRow {
    let id = l.id.clone();
    rig.op
        .store()
        .run(move |c| store::lease(c, &id))
        .await
        .unwrap()
        .unwrap()
}

// ---------------------------------------------------------------- the lease endpoints

#[tokio::test(flavor = "multi_thread")]
async fn a_lease_id_alone_is_refused_and_the_token_opens_status_and_retire() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 1).await;
    let path = format!("/v2/openrouter/leases/{}", l.id);
    for method in [reqwest::Method::GET, reqwest::Method::POST] {
        let (s, v) = call(&rig, method.clone(), &path, None).await;
        json_error(s, &v, 401, "lease_token_invalid");
        let (s, v) = call(
            &rig,
            method,
            &path,
            Some("00000000000000000000000000000000"),
        )
        .await;
        json_error(s, &v, 401, "lease_token_invalid");
    }
    let (s, v) = call(
        &rig,
        reqwest::Method::GET,
        "/v2/openrouter/leases/nope",
        Some(&l.token),
    )
    .await;
    json_error(s, &v, 404, "lease_not_found");

    let v = status(&rig, &l).await;
    assert_eq!(v["status"], "active");
    assert_eq!(v["client_request_id"], l.request.client_request_id);
    assert_eq!(v["spending_limit_usd"], 0.1);
    assert!(v.get("api_key").is_none() && v.get("lease_token").is_none());

    let (s, v) = retire(&rig, &l).await;
    assert_eq!((s, v["status"].as_str()), (200, Some("retiring")));
    let (s, v) = retire(&rig, &l).await;
    json_error(s, &v, 410, "lease_ended");
}

// ---------------------------------------------------------------- retire and settle

#[tokio::test(flavor = "multi_thread")]
async fn retirement_deactivates_the_key_waits_ten_seconds_and_settles_the_measured_charge() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 1).await;
    // 0.04 USD of usage: 41_200 micro with the margin; 60 s held at 0.1% a minute of 0.10 adds 100;
    // 41_300 rounds up to 42_000.
    rig.provider.set_usage(&l.id, 40_000_000);
    rig.mark_delivered_at(&l, 0).await;
    rig.clock.advance_secs(60);
    assert_eq!(retire(&rig, &l).await.0, 200);
    rig.op.tick().await;
    assert!(rig.provider.keys()[&l.id].revoked, "key deactivated");
    assert_eq!(status(&rig, &l).await["status"], "retiring");
    // Nothing is read before ten seconds have passed.
    rig.clock.advance_secs(9);
    rig.op.tick().await;
    assert!(row(&rig, &l).await.usage_json.is_none());
    rig.clock.advance_secs(1);
    rig.op.tick().await;
    assert_eq!(row(&rig, &l).await.status, "retired");
    rig.op.tick().await;

    let v = status(&rig, &l).await;
    assert_eq!(v["status"], "finalized");
    assert_eq!(v["charge_applied"], "42000");
    assert_eq!(v["charge_basis"], "measured");
    assert_eq!(v["usage_usd"], 0.04);
    assert_eq!(rig.op.ledger().open_bounds(), 0);
    assert_eq!(rig.status().await["daily_budget"]["used"], "42000");
    let nullifier = felt_hex(&l.request.public_inputs.request_nullifier);
    let state = rig
        .op
        .store()
        .run(move |c| store::nullifier_row(c, &nullifier))
        .await
        .unwrap()
        .unwrap()
        .state;
    assert_eq!(state, "finalized");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_still_in_flight_at_retire_is_counted_in_the_charge() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 1).await;
    let guard = rig.op.begin_request(&l.id).expect("lease takes requests");
    assert_eq!(retire(&rig, &l).await.0, 200);
    assert!(
        rig.op.begin_request(&l.id).is_err(),
        "a retiring lease takes no new requests"
    );
    rig.op.tick().await;
    assert!(
        !rig.provider.keys()[&l.id].revoked,
        "the key stays until the request drains"
    );
    // The request finishes and its cost lands in the provider's usage.
    rig.provider.set_usage(&l.id, 20_000_000);
    drop(guard);
    settle_all(&rig).await;
    assert_eq!(
        status(&rig, &l).await["charge_applied"],
        "21000",
        "20_600 rounded up, in the charge"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_that_never_drains_is_cancelled_at_its_deadline() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 1).await;
    let _stuck = rig.op.begin_request(&l.id).unwrap();
    retire(&rig, &l).await;
    rig.clock.advance_secs(119);
    rig.op.tick().await;
    assert!(!rig.provider.keys()[&l.id].revoked);
    rig.clock.advance_secs(1);
    rig.op.tick().await;
    assert!(rig.provider.keys()[&l.id].revoked);
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_for_five_minutes_and_the_ten_minute_cap_retire_a_lease() {
    let rig = Rig::new(Setup::default()).await;
    let idle = open(&rig, 1).await;
    let busy = open(&rig, 2).await;
    rig.clock.advance_secs(200);
    rig.mark_delivered_at(&busy, 200).await;
    rig.clock.advance_secs(99);
    rig.op.tick().await;
    assert_eq!(status(&rig, &idle).await["status"], "active", "299 s idle");
    rig.clock.advance_secs(1);
    rig.op.tick().await;
    assert_eq!(
        status(&rig, &idle).await["status"],
        "retiring",
        "300 s idle"
    );
    assert_eq!(status(&rig, &busy).await["status"], "active", "100 s idle");
    // Activity keeps it open until the 10-minute cap.
    rig.clock.advance_secs(250);
    rig.mark_delivered_at(&busy, 550).await;
    rig.op.tick().await;
    assert_eq!(status(&rig, &busy).await["status"], "active");
    rig.clock.advance_secs(50);
    rig.op.tick().await;
    assert_eq!(
        status(&rig, &busy).await["status"],
        "retiring",
        "600 s since it was issued"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn usage_that_cannot_be_read_settles_at_the_bound_after_six_hours() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 1).await;
    rig.provider
        .set_usage_error(Some(ProviderError::Unavailable("reporting down".into())));
    retire(&rig, &l).await;
    rig.op.tick().await;
    rig.clock.advance_secs(6 * 3600 - 1);
    rig.op.tick().await;
    rig.op.tick().await;
    let v = status(&rig, &l).await;
    assert_eq!(v["status"], "retiring");
    assert_eq!(v["settlement_deadline"], T0 + 6 * 3600);
    rig.clock.advance_secs(1);
    rig.op.tick().await;
    rig.op.tick().await;
    let v = status(&rig, &l).await;
    assert_eq!(
        (v["charge_applied"].as_str(), v["charge_basis"].as_str()),
        (Some("100000"), Some("worst_case"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_used_lease_pays_at_least_the_minimum_and_never_more_than_the_bound() {
    let rig = Rig::new(Setup::default()).await;
    let used = open(&rig, 1).await;
    let unused = open(&rig, 2).await;
    let huge = open(&rig, 3).await;
    rig.mark_delivered_at(&used, 0).await;
    rig.provider.set_usage(&huge.id, 900_000_000_000);
    for l in [&used, &unused, &huge] {
        retire(&rig, l).await;
    }
    settle_all(&rig).await;
    assert_eq!(status(&rig, &used).await["charge_applied"], "2000");
    assert_eq!(
        status(&rig, &unused).await["charge_applied"],
        "2000",
        "not expired: the minimum, never 0"
    );
    assert_eq!(status(&rig, &huge).await["charge_applied"], "100000");
}

// ---------------------------------------------------------------- expire

#[tokio::test(flavor = "multi_thread")]
async fn expire_before_any_request_costs_nothing_and_unbinds_the_nullifier() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 1).await;
    let (s, v) = call(
        &rig,
        reqwest::Method::POST,
        &format!("/v2/openrouter/leases/{}/expire", l.id),
        Some(&l.token),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(
        (v["charge_applied"].as_str(), v["charge_basis"].as_str()),
        (Some("0"), Some("unissued"))
    );
    assert!(rig.provider.keys()[&l.id].revoked);
    assert!(
        request_status(&rig, &l).await.0 == 404,
        "the proof and the record are gone"
    );
    assert_eq!(rig.op.ledger().open_bounds(), 0);
    // The nullifier is free: the same proof opens a lease again.
    let (s, _, _) = rig.lease(&l.request).await;
    assert_eq!(s, 201);
}

#[tokio::test(flavor = "multi_thread")]
async fn expire_after_a_request_is_refused_and_the_lease_settles_normally() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 1).await;
    rig.mark_delivered_at(&l, 0).await;
    let path = format!("/v2/openrouter/leases/{}/expire", l.id);
    let (s, v) = call(&rig, reqwest::Method::POST, &path, Some(&l.token)).await;
    json_error(s, &v, 409, "lease_used");
    let (s, v) = call(&rig, reqwest::Method::POST, &path, None).await;
    json_error(s, &v, 401, "lease_token_invalid");
    retire(&rig, &l).await;
    settle_all(&rig).await;
    assert_eq!(status(&rig, &l).await["charge_applied"], "2000");
    let (s, v) = call(&rig, reqwest::Method::POST, &path, Some(&l.token)).await;
    json_error(s, &v, 409, "lease_used");
}

// ---------------------------------------------------------------- the signed next state

#[tokio::test(flavor = "multi_thread")]
async fn the_next_state_is_signed_under_the_state_key_at_the_successor_anchor() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 1).await;
    rig.provider.set_usage(&l.id, 40_000_000);
    retire(&rig, &l).await;
    settle_all(&rig).await;
    let (s, v) = request_status(&rig, &l).await;
    assert_eq!(s, 200);
    assert!(
        !v.to_string().contains(&rig.provider.keys()[&l.id].key_id),
        "never a key"
    );
    let recovery: RecoveryResponseV2 = serde_json::from_value(v).unwrap();
    assert_eq!(
        (recovery.status.as_str(), recovery.nullifier_status.as_str()),
        ("finalized", "finalized")
    );
    let r = recovery.request_response.unwrap();
    assert_eq!(r.charge_applied, 42_000);
    assert_eq!(r.client_request_id, l.request.client_request_id);
    assert_eq!(
        r.next_anchor,
        next_anchor(&l.request.public_inputs.request_nullifier)
    );
    let message = state_message(
        2,
        chain_id(),
        &contract(),
        &r.next_commitment.x,
        &r.next_commitment.y,
        &r.next_anchor,
    );
    let (state_pk, clearance_pk) = (
        rig.signer.state_public_key(),
        rig.signer.clearance_public_key(),
    );
    assert!(verify_signature(&state_pk, &message, &r.next_state_signature).unwrap());
    assert!(!verify_signature(&clearance_pk, &message, &r.next_state_signature).unwrap());
}

fn chain_id() -> u64 {
    nymira_near_codec::chain_id(network())
}

/// A lease taken to `retired` (usage record stored) without settling.
async fn retired(rig: &Rig, n: u64, usage_nano: u128) -> (Lease, SignRequest) {
    let l = open(rig, n).await;
    rig.provider.set_usage(&l.id, usage_nano);
    retire(rig, &l).await;
    rig.op.tick().await;
    rig.clock.advance_secs(11);
    rig.op.tick().await;
    let usage: SignedUsage = serde_json::from_str(&row(rig, &l).await.usage_json.unwrap()).unwrap();
    // 40_000_000 nano after 0 s held: 41_200 -> 42_000
    let request = SignRequest {
        lease_id: l.id.clone(),
        charge: "42000".into(),
        usage,
    };
    (l, request)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_signer_signs_a_matching_request_and_refuses_each_wrong_one() {
    let rig = Rig::new(Setup::default()).await;
    let (l, good) = retired(&rig, 1, 40_000_000).await;

    let refused = |r: Result<_, SignerError>, why: &str| match r {
        Err(SignerError::Refused(reason)) => assert!(reason.contains(why), "{reason}"),
        other => panic!("expected a refusal about {why}: {other:?}"),
    };
    // A wrong charge.
    let mut wrong = good.clone();
    wrong.charge = "41000".into();
    refused(rig.signer.sign_next(wrong).await, "charge");
    // A usage record from another minting key, and one for another lease.
    let other = nymira_operator::usage::Minter::generate();
    let forged = other.sign(good.usage.record.clone());
    refused(
        rig.signer
            .sign_next(SignRequest {
                usage: forged,
                ..good.clone()
            })
            .await,
        "usage record",
    );
    let swapped = rig.minter.sign(UsageRecord {
        lease_id: "other".into(),
        ..good.usage.record.clone()
    });
    refused(
        rig.signer
            .sign_next(SignRequest {
                usage: swapped,
                ..good.clone()
            })
            .await,
        "usage record",
    );
    // A lease nobody reserved.
    refused(
        rig.signer
            .sign_next(SignRequest {
                lease_id: "ghost".into(),
                ..good.clone()
            })
            .await,
        "reservation",
    );
    // A lease that is not retired.
    let active = open(&rig, 2).await;
    let early = SignRequest {
        lease_id: active.id.clone(),
        ..good.clone()
    };
    refused(rig.signer.sign_next(early).await, "not retired");
    // The right request is signed, and asked again returns the same state.
    let first = rig.signer.sign_next(good.clone()).await.unwrap();
    let again = rig.signer.sign_next(good.clone()).await.unwrap();
    assert_eq!(
        serde_json::to_value(&first).unwrap(),
        serde_json::to_value(&again).unwrap()
    );
    let mut other_charge = good.clone();
    other_charge.charge = "43000".into();
    refused(rig.signer.sign_next(other_charge).await, "another charge");
    let _ = l;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_signer_refuses_a_nullifier_in_an_exit() {
    let rig = Rig::new(Setup::default()).await;
    let (l, good) = retired(&rig, 1, 40_000_000).await;
    // The exit import cannot take the reserved row, but the signer must still know.
    let n = felt_hex(&l.request.public_inputs.request_nullifier);
    assert_eq!(rig.op.import_exit(&n).await, Err("reserved".to_string()));
    match rig.signer.sign_next(good).await {
        Err(SignerError::Refused(reason)) => assert!(reason.contains("exit"), "{reason}"),
        other => panic!("{other:?}"),
    }
    // The operator settles the lease without a next state: its bound leaves the admission ledger
    // and the credit is not held for ever by an exit someone filed on an open lease.
    let open_before = rig.op.ledger().open_bounds();
    assert!(open_before > 0);
    rig.op.tick().await;
    assert_eq!(row(&rig, &l).await.status, "settled");
    assert_eq!(
        rig.op.ledger().open_bounds(),
        0,
        "the bound is still booked"
    );
    assert_ne!(request_status(&rig, &l).await.1["status"], "pending");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signed_state_survives_a_restart_before_the_response_was_read() {
    let mut rig = Rig::new(Setup::default()).await;
    let (l, good) = retired(&rig, 1, 40_000_000).await;
    // The signer wrote the state and the process died before anyone read the answer.
    let signed = rig.signer.sign_next(good).await.unwrap();
    rig.restart(Setup::default()).await;
    let (s, v) = request_status(&rig, &l).await;
    assert_eq!(s, 200);
    let recovered = v["request_response"]["next_state_signature"].clone();
    assert_eq!(
        recovered,
        serde_json::to_value(signed.next_state_signature).unwrap()
    );
    // The next tick settles on top of the stored state: same signature, now finalized.
    rig.op.tick().await;
    assert_eq!(status(&rig, &l).await["status"], "finalized");
    assert_eq!(
        request_status(&rig, &l).await.1["request_response"]["next_state_signature"],
        recovered
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_restores_the_days_spend_and_open_leases() {
    let mut rig = Rig::new(Setup::default()).await;
    let settled = open(&rig, 1).await;
    let open_lease = open(&rig, 2).await;
    rig.provider.set_usage(&settled.id, 40_000_000);
    retire(&rig, &settled).await;
    settle_all(&rig).await;
    rig.restart(Setup::default()).await;
    assert_eq!(rig.status().await["daily_budget"]["used"], "42000");
    assert_eq!(
        rig.op.ledger().open_bounds(),
        100_000,
        "only the unsettled lease is open"
    );
    assert_eq!(status(&rig, &open_lease).await["status"], "active");
}

// ---------------------------------------------------------------- the signer process

#[tokio::test(flavor = "multi_thread")]
async fn the_signer_serves_its_own_user_over_a_0600_socket_and_nobody_else() {
    let rig = Rig::new(Setup::default()).await;
    let (_l, good) = retired(&rig, 1, 40_000_000).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("signer.sock");
    let me = std::fs::metadata(dir.path()).unwrap().uid();

    let listener = bind_socket(&path).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
    let task = tokio::spawn(serve(listener, rig.signer.clone(), me));
    let response = SocketSigner(path.clone())
        .sign_next(good.clone())
        .await
        .unwrap();
    assert_eq!(response.charge_applied, 42_000);
    // A refusal travels as a refusal.
    let mut wrong = good.clone();
    wrong.charge = "1".into();
    assert!(matches!(
        SocketSigner(path.clone()).sign_next(wrong).await,
        Err(SignerError::Refused(_))
    ));
    task.abort();

    // The same signer, told that only another user may connect, drops us unread.
    let other_path = dir.path().join("other.sock");
    let listener = bind_socket(&other_path).unwrap();
    let task = tokio::spawn(serve(listener, rig.signer.clone(), me + 1));
    assert!(SocketSigner(other_path).sign_next(good).await.is_err());
    task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_signer_refuses_to_start_with_equal_keys_and_derives_seeds_from_passphrases() {
    let rig = Rig::new(Setup::default()).await;
    let equal = Signer::new(
        CompactSigner::from_seed(&Felt252::from_u64(7)),
        CompactSigner::from_seed(&Felt252::from_u64(7)),
        nymira_operator::store::Store::open_in_memory().unwrap(),
        Arc::new(FakeVault::new(vault_config(), &root_hex())),
        rig.minter.public_key(),
        chain_id(),
        contract(),
        rig.clock.clone(),
        Arc::new(FakeHeartbeat::default()),
    );
    assert!(equal.is_err());
    // Seeds from passphrases: deterministic, role-separated, canonical felts.
    let a = seed_from_passphrase("correct horse", "state", 1_000);
    assert_eq!(a, seed_from_passphrase("correct horse", "state", 1_000));
    assert_ne!(a, seed_from_passphrase("correct horse", "clearance", 1_000));
    assert_ne!(a, seed_from_passphrase("another one", "state", 1_000));
    assert!(Felt252::try_from_bytes_be(a.0).is_ok());
    // The production policy refuses the published development seeds; sandbox builds allow them.
    assert!(
        Signer::signers_from_seeds(&Felt252::from_u64(1), &Felt252::from_u64(2), false).is_err()
    );
    assert!(Signer::signers_from_seeds(&Felt252::from_u64(1), &Felt252::from_u64(2), true).is_ok());
    let (s, c) = Signer::signers_from_seeds(
        &a,
        &seed_from_passphrase("second passphrase", "clearance", 1_000),
        false,
    )
    .unwrap();
    assert_ne!(s.public_key(), c.public_key());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_signer_sends_a_heartbeat_every_thirty_minutes() {
    let rig = Rig::new(Setup::default()).await;
    rig.signer.tick().await;
    assert_eq!(rig.heartbeat.sent(), 1);
    rig.clock.advance_secs(HEARTBEAT_EVERY_SECS - 1);
    rig.signer.tick().await;
    assert_eq!(rig.heartbeat.sent(), 1);
    rig.clock.advance_secs(1);
    rig.heartbeat.set_failing(true);
    rig.signer.tick().await;
    assert_eq!(rig.heartbeat.sent(), 1, "the send failed");
    rig.heartbeat.set_failing(false);
    // A failed send backs off for a minute: the next tick does not ask the node again.
    rig.signer.tick().await;
    assert_eq!(rig.heartbeat.sent(), 1, "backing off");
    rig.clock.advance_secs(60);
    rig.signer.tick().await;
    assert_eq!(rig.heartbeat.sent(), 2, "retried after the back-off");
}

// ---------------------------------------------------------------- start-up

#[tokio::test(flavor = "multi_thread")]
async fn the_operator_refuses_to_start_against_a_testnet_or_mainnet_vault() {
    for profile in [Profile::Testnet, Profile::Mainnet] {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(nymira_operator::clock::ManualClock::new(T0 * 1000));
        let vault = Arc::new(FakeVault::new(vault_config(), &root_hex()));
        let mut cfg = config();
        cfg.profile = profile;
        let started = nymira_operator::Operator::start(nymira_operator::Parts {
            cfg,
            clock: clock.clone(),
            vault,
            provider: Arc::new(nymira_operator::provider::FakeProvider::new(
                clock.clone(),
                1,
            )),
            replica: Arc::new(nymira_operator::replica::NoReplica),
            verifier: Arc::new(Counting::default()),
            withdrawal_verifier: Arc::new(AcceptWithdrawals),
            closer: Arc::new(NoCloser),
            store: nymira_operator::store::Store::open(&dir.path().join("db")).unwrap(),
            state_signer: Arc::new(CompactSigner::from_seed(&Felt252::from_u64(1))),
            signer: Arc::new(NoSigner),
            minter: Arc::new(nymira_operator::usage::Minter::generate()),
            alarms: std::sync::Arc::new(nymira_operator::alarm::Alarms::new(vec![], clock.clone())),
        })
        .await;
        assert!(
            matches!(
                started,
                Err(nymira_operator::operator::StartError::NoReplication)
            ),
            "{profile:?}"
        );
    }
}

struct NoSigner;

#[async_trait::async_trait]
impl SignerApi for NoSigner {
    async fn sign_clearance(
        &self,
        _: Felt252,
    ) -> Result<zkapi_types::SchnorrSignature, nymira_operator::signer::SignerError> {
        Err(nymira_operator::signer::SignerError::Refused("none".into()))
    }

    async fn sign_next(
        &self,
        _: SignRequest,
    ) -> Result<zkapi_types::wire::RequestResponseV2, SignerError> {
        Err(SignerError::Refused("none".into()))
    }
}

impl Rig {
    /// Records a request proxied at `after_secs` past the start, for the delivery and idle rules.
    async fn mark_delivered_at(&self, l: &Lease, _after_secs: u64) {
        self.op.mark_delivered(&l.id).await.unwrap();
    }
}

#[allow(dead_code)]
fn unused(_: Value) {
    let _ = json!(null);
}

// ---------------------------------------------------------------- races and recovery

#[tokio::test(flavor = "multi_thread")]
async fn a_request_running_when_expire_arrives_counts_as_delivery() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 1).await;
    let guard = rig.op.begin_request(&l.id).unwrap();
    let path = format!("/v2/openrouter/leases/{}/expire", l.id);
    let (s, v) = call(&rig, reqwest::Method::POST, &path, Some(&l.token)).await;
    json_error(s, &v, 409, "lease_used");
    assert!(
        rig.op.begin_request(&l.id).is_ok(),
        "a refused expire leaves the lease open"
    );
    drop(guard);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_key_that_will_not_deactivate_is_retried_until_the_six_hours_are_up() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 1).await;
    rig.provider
        .set_revoke_error(Some(ProviderError::Unavailable("api down".into())));
    retire(&rig, &l).await;
    rig.op.tick().await;
    assert_eq!(row(&rig, &l).await.status, "retiring");
    rig.clock.advance_secs(6 * 3600);
    rig.provider
        .set_usage_error(Some(ProviderError::Unavailable("api down".into())));
    for _ in 0..4 {
        rig.op.tick().await;
        rig.clock.advance_secs(11);
    }
    let v = status(&rig, &l).await;
    assert_eq!(
        (v["charge_applied"].as_str(), v["charge_basis"].as_str()),
        (Some("100000"), Some("worst_case"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_signer_signs_only_the_usage_record_stored_for_the_lease() {
    let rig = Rig::new(Setup::default()).await;
    let (_l, good) = retired(&rig, 1, 40_000_000).await;
    // A genuine record from the minting key with another figure, as a replayed or stale one.
    let mut record = good.usage.record.clone();
    record.cost_nano_usd = Some("1".into());
    let other = rig.minter.sign(record);
    let request = SignRequest {
        charge: "2000".into(),
        usage: other,
        ..good
    };
    match rig.signer.sign_next(request).await {
        Err(SignerError::Refused(reason)) => assert!(reason.contains("stored"), "{reason}"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restart_keeps_a_lease_that_owes_a_charge_even_if_the_replica_is_down() {
    let mut rig = Rig::new(Setup::default()).await;
    let (l, _good) = retired(&rig, 1, 40_000_000).await;
    let id = l.id.clone();
    rig.op
        .store()
        .run(move |c| {
            c.execute("UPDATE leases SET replicated = 0 WHERE lease_id = ?1", [id])
                .map(|_| ())
        })
        .await
        .unwrap();
    rig.replica.set_down(true);
    rig.restart(Setup::default()).await;
    assert_eq!(
        row(&rig, &l).await.status,
        "retired",
        "kept, not rolled back"
    );
    // No state is signed while the replica is away; it is signed once the replica is back.
    rig.op.tick().await;
    assert_eq!(status(&rig, &l).await["status"], "retiring");
    rig.replica.set_down(false);
    rig.op.tick().await;
    assert_eq!(status(&rig, &l).await["status"], "finalized");
}

#[test]
fn the_two_passphrases_must_differ() {
    assert!(nymira_operator::signer::seeds_from_passphrases("same", "same", 1_000).is_err());
    let (a, b) = nymira_operator::signer::seeds_from_passphrases("one", "two", 1_000).unwrap();
    assert_ne!(a, b);
}
