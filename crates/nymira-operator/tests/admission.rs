mod common;

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use common::*;
use nymira_operator::chain::ExitEntry;
use nymira_operator::operator::felt_hex;
use nymira_operator::provider::{key_cap_nano_usd, ProviderError};
use nymira_operator::store;
use nymira_operator::token;
use serde_json::{json, Value};
use zkapi_types::Felt252;

fn counting() -> (Arc<Counting>, Setup) {
    let c = Arc::new(Counting::default());
    (
        c.clone(),
        Setup {
            verifier: Verifier::Stub(c),
            ..Setup::default()
        },
    )
}

fn retry_after(headers: &reqwest::header::HeaderMap) -> u64 {
    headers["retry-after"].to_str().unwrap().parse().unwrap()
}

// ---------------------------------------------------------------- lease issue

#[tokio::test(flavor = "multi_thread")]
async fn a_valid_request_gets_a_lease_without_the_key() {
    let rig = Rig::new(Setup::default()).await;
    let (status, v, _) = rig.lease(&rig.fresh(1)).await;
    assert_eq!(status, 201, "{v}");
    assert_eq!(v["status"], "active");
    assert_eq!(v["client_request_id"], "req-1");
    assert_eq!(v["bound"], "100000");
    assert_eq!(v["expires_at"], T0 + 600);
    assert_eq!(v["settle_after"], T0 + 610);
    let lease_id = v["lease_id"].as_str().unwrap();
    let lease_token = v["lease_token"].as_str().unwrap();
    assert_eq!(lease_token.len(), 32);
    assert_ne!(lease_id, lease_token);

    let key = &rig.provider.keys()[lease_id];
    assert!(!key.revoked);
    assert_eq!(key.cap_nano_usd, key_cap_nano_usd(100_000));
    assert_eq!(
        key.cap_nano_usd, 97_087_378,
        "0.10 USDC / 1.03 in nano-USD, rounded down"
    );
    assert!(key.expires_at > T0);
    // The key name is random: unrelated to the lease id and the token.
    assert!(!key.key_id.contains(lease_id) && !key.key_id.contains(lease_token));
    // The response never carries the key or its name.
    let text = v.to_string();
    assert!(!text.contains(&key.key_id) && v.get("api_key").is_none());
    // The record holds the token hashed, never the token.
    let id = lease_id.to_string();
    let row = rig
        .op
        .store()
        .run(move |c| store::lease(c, &id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.token_hash, token::hash(lease_token));
    assert_ne!(row.token_hash, lease_token.as_bytes());
    assert_eq!(row.key_id.as_deref(), Some(key.key_id.as_str()));
    assert_eq!((row.status.as_str(), row.replicated), ("active", true));
    assert_eq!(rig.replica.entries().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_database_is_opened_with_full_synchronous_and_fullfsync() {
    let rig = Rig::new(Setup::default()).await;
    assert_eq!(rig.op.store().durability().unwrap(), (2, 1));
}

// ---------------------------------------------------------------- the error table

#[tokio::test(flavor = "multi_thread")]
async fn malformed_requests_are_refused() {
    let (calls, setup) = counting();
    let rig = Rig::new(setup).await;
    let good = serde_json::to_value(rig.fresh(1)).unwrap();
    let mut cases: Vec<Vec<u8>> = vec![b"not json".to_vec(), b"[]".to_vec(), b"{}".to_vec()];
    let mut extra = good.clone();
    extra["prompt"] = "hello".into();
    cases.push(extra.to_string().into_bytes());
    let mut extra_input = good.clone();
    extra_input["public_inputs"]["surprise"] = 1.into();
    cases.push(extra_input.to_string().into_bytes());
    let mut wrong_type = good.clone();
    wrong_type["public_inputs"]["request_time"] = "soon".into();
    cases.push(wrong_type.to_string().into_bytes());
    // A duplicate key anywhere.
    let text = good.to_string();
    cases.push(
        text.replacen("\"payload\"", "\"payload\":\"{}\",\"payload\"", 1)
            .into_bytes(),
    );
    // Field lengths: id of 37 characters, payload over 2 KiB, wrong payload hash, extra payload key.
    let mut long_id = good.clone();
    long_id["client_request_id"] = "x".repeat(37).into();
    cases.push(long_id.to_string().into_bytes());
    let mut empty_id = good.clone();
    empty_id["client_request_id"] = "".into();
    cases.push(empty_id.to_string().into_bytes());
    let mut big_payload = good.clone();
    big_payload["payload"] = format!(
        "{{\"mode\":\"openrouter_ephemeral_lease\",\"version\":1,\"pad\":\"{}\"}}",
        "a".repeat(2100)
    )
    .into();
    cases.push(big_payload.to_string().into_bytes());
    let mut bad_hash = good.clone();
    bad_hash["payload_hash"] = "0x1234".into();
    cases.push(bad_hash.to_string().into_bytes());
    let mut prompt_payload = good.clone();
    prompt_payload["payload"] =
        r#"{"mode":"openrouter_ephemeral_lease","version":1,"prompt":"hi"}"#.into();
    cases.push(prompt_payload.to_string().into_bytes());
    for (i, case) in cases.into_iter().enumerate() {
        let (status, v, _) = rig.post_lease(case).await;
        assert_eq!(
            (status, code(&v)),
            (400, "malformed_request"),
            "case {i}: {v}"
        );
        assert!(v["error"].get("retry_after_secs").is_none());
    }
    assert_eq!(calls.0.load(Ordering::SeqCst), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_oversize_body_is_refused_before_anything_else() {
    let (calls, setup) = counting();
    let rig = Rig::new(setup).await;
    // The server may answer 413 and close before the client has sent the whole body; on a slow
    // runner the client then sees a reset instead of the answer.
    let refused = rig
        .http
        .post(format!("{}/v2/openrouter/leases", rig.url))
        .header("content-type", "application/json")
        .body(vec![b' '; 64 * 1024 + 1])
        .send()
        .await;
    match refused {
        Ok(response) => {
            let status = response.status().as_u16();
            let v: Value = response.json().await.unwrap();
            json_error(status, &v, 413, "body_too_large");
        }
        Err(e) => assert!(!e.is_timeout(), "a reset is fine, a hang is not: {e}"),
    }
    assert_eq!(calls.0.load(Ordering::SeqCst), 0);
    assert_eq!(rig.op.attempt_entries(), 0);
    // Just at the limit it is parsed (and fails as malformed). A fresh client: the server may have
    // closed the pooled connection after the 413, and a reused one then fails with
    // IncompleteMessage.
    let response = reqwest::Client::new()
        .post(format!("{}/v2/openrouter/leases", rig.url))
        .header("content-type", "application/json")
        .body(vec![b' '; 64 * 1024])
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let v: Value = response.json().await.unwrap_or(Value::Null);
    json_error(status, &v, 400, "malformed_request");
}

#[tokio::test(flavor = "multi_thread")]
async fn version_bound_class_and_time_are_refused_before_groth16() {
    let (calls, setup) = counting();
    let rig = Rig::new(setup).await;
    let now = rig.clock_now();

    let mut r = rig.fresh(1);
    r.public_inputs.protocol_version = 3;
    let (s, v, _) = rig.lease(&r).await;
    json_error(s, &v, 400, "unsupported_version");

    let (s, v, _) = rig.lease(&variant(2, 123_456, now, &root_hex())).await;
    json_error(s, &v, 400, "bound_class_invalid");

    // 300 s of life plus 30 s of skew: at the edge it passes, one second more does not.
    let (s, _, _) = rig
        .lease(&variant(3, 100_000, now - 330, &root_hex()))
        .await;
    assert_eq!(s, 201);
    let (s, v, _) = rig
        .lease(&variant(4, 100_000, now - 331, &root_hex()))
        .await;
    json_error(s, &v, 400, "proof_expired");
    let (s, _, _) = rig.lease(&variant(5, 100_000, now + 30, &root_hex())).await;
    assert_eq!(s, 201);
    let (s, v, _) = rig.lease(&variant(6, 100_000, now + 31, &root_hex())).await;
    json_error(s, &v, 400, "proof_expired");

    // Another deployment's chain, contract or key is a public-input failure.
    let mut wrong = rig.fresh(7);
    wrong.public_inputs.chain_id += 1;
    let (s, v, _) = rig.lease(&wrong).await;
    json_error(s, &v, 422, "proof_invalid");

    let (s, v, _) = rig
        .lease(&variant(8, 100_000, now, &felt_hex(&Felt252::from_u64(5))))
        .await;
    json_error(s, &v, 409, "root_stale");

    // A used nullifier is refused without verifying again.
    let before = calls.0.load(Ordering::SeqCst);
    let (s, v, _) = rig.lease(&variant(3, 100_000, now, &root_hex())).await;
    json_error(s, &v, 409, "nullifier_seen");
    assert_eq!(calls.0.load(Ordering::SeqCst), before);
    // Only the two admitted leases (and the three admitted before) ever reached Groth16.
    assert_eq!(calls.0.load(Ordering::SeqCst), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_point_off_the_curve_is_refused_and_not_counted() {
    let rig = Rig::new(Setup::default()).await;
    let mut r = rig.fresh(1);
    r.public_inputs.anonymous_commitment_y = Felt252::from_u64(3);
    let (s, v, _) = rig.lease(&r).await;
    json_error(s, &v, 422, "proof_invalid");
    assert_eq!(
        rig.op.attempt_entries(),
        0,
        "failed point checks never reach the counter"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pending_exit_blocks_the_nullifier_and_its_successor() {
    let rig = Rig::new(Setup::default()).await;
    let a = felt_hex(&Felt252::from_u64(1_000 + 1));
    let b = felt_hex(&Felt252::from_u64(1_000 + 2));
    rig.vault.edit(|v| {
        v.exits = vec![ExitEntry {
            note_id: 0,
            nullifier: a.clone(),
            next_nullifier: b.clone(),
            ..Default::default()
        }]
    });
    rig.clock.advance_secs(2);
    for n in [1, 2] {
        let (s, v, _) = rig.lease(&rig.fresh(n)).await;
        json_error(s, &v, 409, "nullifier_in_exit");
    }
    let (s, _, _) = rig.lease(&rig.fresh(3)).await;
    assert_eq!(s, 201);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_real_verifier_refuses_a_forged_proof() {
    let rig = Rig::new(Setup {
        verifier: Verifier::Real,
        ..Setup::default()
    })
    .await;
    let mut forged = real_request().clone();
    forged.public_inputs.request_nullifier = Felt252::from_u64(77);
    let (s, v, _) = rig.lease(&forged).await;
    json_error(s, &v, 422, "proof_invalid");
    let mut tampered = real_request().clone();
    tampered.public_inputs.solvency_bound = 500_000;
    let (s, v, _) = rig.lease(&tampered).await;
    json_error(s, &v, 422, "proof_invalid");
    // The genuine proof passes and the lease opens.
    let (s, v, _) = rig.lease(real_request()).await;
    assert_eq!(s, 201, "{v}");
}

// ---------------------------------------------------------------- the root window

#[tokio::test(flavor = "multi_thread")]
async fn the_current_root_is_accepted_after_an_hour_and_a_replaced_root_for_sixty_seconds() {
    let rig = Rig::new(Setup::default()).await;
    rig.clock.advance_secs(3_600);
    let (s, _, _) = rig.lease(&rig.fresh(1)).await;
    assert_eq!(s, 201, "the current root never expires");

    let old = root_hex();
    let new = felt_hex(&Felt252::from_u64(0xabcdef));
    rig.vault.edit(|v| v.root = new.clone());
    rig.clock.advance_secs(2);
    // The first request after the deposit sees the replacement.
    let (s, _, _) = rig.lease(&variant(2, 100_000, rig.clock_now(), &new)).await;
    assert_eq!(s, 201);
    rig.clock.advance_secs(59);
    let (s, _, _) = rig.lease(&variant(3, 100_000, rig.clock_now(), &old)).await;
    assert_eq!(s, 201, "replaced 59 seconds ago");
    rig.clock.advance_secs(2);
    let (s, v, _) = rig.lease(&variant(4, 100_000, rig.clock_now(), &old)).await;
    json_error(s, &v, 409, "root_stale");
    let (s, _, _) = rig.lease(&variant(5, 100_000, rig.clock_now(), &new)).await;
    assert_eq!(s, 201);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_retirement_epoch_drops_every_stale_root() {
    let rig = Rig::new(Setup::default()).await;
    let old = root_hex();
    let new = felt_hex(&Felt252::from_u64(0x1234));
    rig.vault.edit(|v| v.root = new.clone());
    rig.clock.advance_secs(2);
    let (s, _, _) = rig.lease(&variant(1, 100_000, rig.clock_now(), &new)).await;
    assert_eq!(s, 201);
    let (s, _, _) = rig.lease(&variant(2, 100_000, rig.clock_now(), &old)).await;
    assert_eq!(s, 201, "inside the grace window");
    rig.vault.edit(|v| v.epoch = 1);
    rig.clock.advance_secs(2);
    let (s, v, _) = rig.lease(&variant(3, 100_000, rig.clock_now(), &old)).await;
    json_error(s, &v, 409, "root_stale");
}

// ---------------------------------------------------------------- admission: credit and budget

#[tokio::test(flavor = "multi_thread")]
async fn credit_below_open_bounds_plus_reserve_refuses_and_leaves_the_nullifier_unbound() {
    // 1.00 reserve + 0.15 free: one 0.10 lease fits, a second does not.
    let rig = Rig::new(Setup {
        credit: 1_150_000,
        ..Setup::default()
    })
    .await;
    let (s, v1, _) = rig.lease(&rig.fresh(1)).await;
    assert_eq!(s, 201);
    let second = rig.fresh(2);
    let (s, v, h) = rig.lease(&second).await;
    json_error(s, &v, 429, "operator_capacity");
    let wait = retry_after(&h);
    assert!(
        (1..300).contains(&wait),
        "shorter than the proof's life: {wait}"
    );
    assert_eq!(v["error"]["retry_after_secs"], wait);
    // The nullifier is unbound: the same proof works once a reservation is released.
    let id = v1["lease_id"].as_str().unwrap();
    rig.op.ledger().release(id);
    let (s, _, _) = rig.lease(&second).await;
    assert_eq!(s, 201);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_half_dollar_class_is_capped_at_a_fifth_of_live_credit() {
    let rig = Rig::new(Setup {
        credit: 2_400_000,
        ..Setup::default()
    })
    .await;
    let big = variant(1, 500_000, rig.clock_now(), &root_hex());
    let (s, v, _) = rig.lease(&big).await;
    json_error(s, &v, 429, "operator_capacity");
    rig.provider.set_balance(2_500_000);
    rig.clock.advance_secs(301); // the balance is read again every 5 minutes
    let big = variant(1, 500_000, rig.clock_now(), &root_hex());
    let (s, _, _) = rig.lease(&big).await;
    assert_eq!(s, 201);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_daily_budget_refuses_until_midnight_and_a_half_dollar_lease_opens_under_it() {
    let rig = Rig::new(Setup::default()).await;
    // A 0.50 lease opens under a 0.25 budget: bounds are not counted.
    let (s, _, _) = rig
        .lease(&variant(1, 500_000, rig.clock_now(), &root_hex()))
        .await;
    assert_eq!(s, 201);
    assert_eq!(rig.status().await["daily_budget"]["used"], "0");

    // Request cost reserved by in-flight requests counts, and shows in the status.
    rig.op.ledger().set_inflight("some-lease", 250_000);
    assert_eq!(rig.status().await["daily_budget"]["used"], "250000");
    let blocked = rig.fresh(2);
    let (s, v, h) = rig.lease(&blocked).await;
    json_error(s, &v, 429, "operator_capacity");
    let to_midnight = 86_400 - T0 % 86_400;
    assert_eq!(
        retry_after(&h),
        to_midnight,
        "the one wait longer than the proof's life"
    );
    assert!(to_midnight > 300);

    // The nullifier stayed unbound: the same proof opens a lease once the day rolls over.
    rig.clock.advance_secs(to_midnight);
    let fresh = variant(2, 100_000, rig.clock_now(), &root_hex());
    rig.op.ledger().set_inflight("some-lease", 0);
    let (s, _, _) = rig.lease(&fresh).await;
    assert_eq!(s, 201);
    let s = rig.status().await;
    assert_eq!(s["daily_budget"]["resets_ts"], (T0 / 86_400 + 2) * 86_400);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_used_up_budget_resets_at_utc_midnight_without_any_release() {
    let rig = Rig::new(Setup::default()).await;
    rig.op
        .ledger()
        .settle("old-lease", 260_000, rig.clock_now());
    let (s, v, _) = rig.lease(&rig.fresh(1)).await;
    json_error(s, &v, 429, "operator_capacity");
    rig.clock.advance_secs(86_400 - T0 % 86_400);
    let (s, _, _) = rig.lease(&rig.fresh(1)).await;
    assert_eq!(s, 201);
}

#[tokio::test(flavor = "multi_thread")]
async fn caps_are_per_note_and_global_never_per_account() {
    // There is no account in a request: two different notes (nullifiers) are limited by the same
    // global budget, and one note by its own attempt counter.
    let rig = Rig::new(Setup::default()).await;
    rig.op.ledger().settle("x", 250_000, rig.clock_now());
    for n in [1, 2] {
        let (s, v, _) = rig.lease(&rig.fresh(n)).await;
        json_error(s, &v, 429, "operator_capacity");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn key_minting_is_rate_limited_by_a_token_bucket() {
    let rig = Rig::new(Setup {
        tune: Box::new(|c| {
            c.budget.bucket_capacity = 2;
            c.budget.bucket_refill_per_sec = 0.5;
        }),
        ..Setup::default()
    })
    .await;
    for n in [1, 2] {
        assert_eq!(rig.lease(&rig.fresh(n)).await.0, 201);
    }
    let third = rig.fresh(3);
    let (s, v, h) = rig.lease(&third).await;
    json_error(s, &v, 429, "operator_capacity");
    assert!(retry_after(&h) <= 5);
    rig.clock.advance_secs(2);
    let third = variant(3, 100_000, rig.clock_now(), &root_hex());
    assert_eq!(rig.lease(&third).await.0, 201);
}

// ---------------------------------------------------------------- the attempt counter and the pool

#[tokio::test(flavor = "multi_thread")]
async fn a_fourth_attempt_on_one_nullifier_in_300_seconds_is_rate_limited() {
    let rig = Rig::new(Setup::default()).await;
    let base = rig.fresh(1);
    assert_eq!(rig.lease(&base).await.0, 201);
    for i in 0..2 {
        let mut again = base.clone();
        again.client_request_id = format!("other-{i}");
        let (s, v, _) = rig.lease(&again).await;
        json_error(s, &v, 409, "nullifier_seen");
    }
    let mut again = base.clone();
    again.client_request_id = "other-3".into();
    let (s, v, h) = rig.lease(&again).await;
    json_error(s, &v, 429, "nullifier_rate_limited");
    assert!(retry_after(&h) <= 300);
    rig.clock.advance_secs(300);
    let mut later = variant(1, 100_000, rig.clock_now(), &root_hex());
    later.client_request_id = "other-4".into();
    let (s, v, _) = rig.lease(&later).await;
    json_error(s, &v, 409, "nullifier_seen");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_flood_of_garbage_nullifiers_leaves_real_leases_getting_through() {
    let rig = Rig::new(Setup {
        verifier: Verifier::Real,
        // The puzzle would stop the flood earlier; this test is about the counter.
        tune: Box::new(|c| {
            c.attempts_cap = 100;
            c.puzzle_load_pct = 100;
        }),
        ..Setup::default()
    })
    .await;
    let mut tasks = Vec::new();
    for n in 0..300u64 {
        let (http, url) = (rig.http.clone(), rig.url.clone());
        let garbage = body(&variant(n + 10, 100_000, T0, &root_hex()));
        tasks.push(tokio::spawn(async move {
            let r = http
                .post(format!("{url}/v2/openrouter/leases"))
                .body(garbage)
                .send()
                .await
                .unwrap();
            r.status().as_u16()
        }));
    }
    for t in tasks {
        let status = t.await.unwrap();
        assert!(
            status == 422 || status == 429,
            "garbage never gets a lease: {status}"
        );
    }
    assert_eq!(
        rig.op.attempt_entries(),
        100,
        "the counter sits at its size cap"
    );
    // A real proof still gets through.
    let (s, v, _) = rig.lease(real_request()).await;
    assert_eq!(s, 201, "{v}");
    assert_eq!(rig.op.attempt_entries(), 100);
}

/// Opens the gate on drop: a failed assertion must not leave verifier threads blocked.
struct OpenOnDrop(Arc<Gate>);

impl Drop for OpenOnDrop {
    fn drop(&mut self) {
        self.0.open();
    }
}

/// Starts `count` leases that block inside verification.
async fn hold(
    rig: &Arc<Rig>,
    gate: &Arc<Gate>,
    count: u64,
) -> Vec<tokio::task::JoinHandle<(u16, Value, reqwest::header::HeaderMap)>> {
    let mut held = Vec::new();
    for n in 0..count {
        let rig = rig.clone();
        held.push(tokio::spawn(
            async move { rig.lease(&rig.fresh(n + 1)).await },
        ));
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while gate.entered() < count as usize {
        assert!(
            std::time::Instant::now() < deadline,
            "only {} verifications started",
            gate.entered()
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    held
}

#[tokio::test(flavor = "multi_thread")]
async fn the_fifth_concurrent_verification_gets_an_immediate_429() {
    let gate = Arc::new(Gate::default());
    let _open = OpenOnDrop(gate.clone());
    let rig = Arc::new(
        Rig::new(Setup {
            verifier: Verifier::Stub(gate.clone()),
            // The puzzle would answer first at this load; this test is about the pool.
            tune: Box::new(|c| c.puzzle_load_pct = 100),
            ..Setup::default()
        })
        .await,
    );
    let held = hold(&rig, &gate, 4).await;
    let started = std::time::Instant::now();
    let (s, v, h) = rig.lease(&rig.fresh(5)).await;
    json_error(s, &v, 429, "operator_capacity");
    assert!(started.elapsed() < Duration::from_secs(1), "no queue");
    assert!(retry_after(&h) >= 1);
    gate.open();
    for h in held {
        assert_eq!(h.await.unwrap().0, 201);
    }
    // The refused request left its nullifier unbound: it goes through once the pool is free.
    assert_eq!(rig.lease(&rig.fresh(5)).await.0, 201);
}

#[tokio::test(flavor = "multi_thread")]
async fn above_half_verification_load_a_puzzle_is_advertised_and_required() {
    let gate = Arc::new(Gate::default());
    let _open = OpenOnDrop(gate.clone());
    let rig = Arc::new(
        Rig::new(Setup {
            verifier: Verifier::Stub(gate.clone()),
            ..Setup::default()
        })
        .await,
    );
    let held = hold(&rig, &gate, 3).await; // 75% of a pool of four
    let status = rig.status().await;
    assert_eq!(status["admission"]["load_pct"], 75);
    let difficulty = status["admission"]["puzzle_difficulty"].as_u64().unwrap();
    assert!(difficulty > 0);
    let (s, v, h) = rig.lease(&rig.fresh(6)).await;
    json_error(s, &v, 429, "puzzle_required");
    assert!(retry_after(&h) >= 1);
    // Bound to another slot's nonce, or too weak, is refused too.
    let (s, v, _) = rig.post_lease_with(body(&rig.fresh(6)), Some("00:1")).await;
    json_error(s, &v, 429, "puzzle_required");
    let nonce = status["nonce"]["nonce"].as_str().unwrap();
    let solved = nymira_operator::puzzle::solve(nonce, difficulty as u32);
    gate.open();
    for h in held {
        assert_eq!(h.await.unwrap().0, 201);
    }
    // Load is the busiest moment of the last two slots, so the puzzle is still on; solved it passes.
    let (s, v, _) = rig
        .post_lease_with(body(&rig.fresh(6)), Some(&solved))
        .await;
    assert_eq!(s, 201, "{v}");
    // A solution for a slot older than 30 seconds is refused while load stays high.
    // Two slots later the load is gone and no puzzle is needed.
    rig.clock.advance_secs(10);
    assert_eq!(rig.status().await["admission"]["puzzle_difficulty"], 0);
    assert_eq!(rig.lease(&rig.fresh(7)).await.0, 201);
}

// ---------------------------------------------------------------- one winner per nullifier

#[tokio::test(flavor = "multi_thread")]
async fn a_duplicate_post_returns_the_same_lease() {
    let rig = Rig::new(Setup::default()).await;
    rig.provider.set_mint_delay(Duration::from_millis(100));
    let r = rig.fresh(1);
    let (a, b) = tokio::join!(rig.lease(&r), rig.lease(&r));
    assert_eq!((a.0, b.0), (201, 201), "{} {}", a.1, b.1);
    assert_eq!(a.1["lease_id"], b.1["lease_id"]);
    assert_eq!(a.1["lease_token"], b.1["lease_token"]);
    assert_eq!(rig.provider.mint_calls(), 1);
    assert_eq!(
        rig.op
            .store()
            .run(|c| store::count_nullifiers(c))
            .await
            .unwrap(),
        1
    );
    // A repeat after the fact returns it again; a different body for the same nullifier does not.
    rig.clock.advance_secs(300); // a fresh attempt window
    let (s, c, _) = rig.lease(&r).await;
    assert_eq!((s, &c["lease_id"]), (201, &a.1["lease_id"]));
    let mut other = r.clone();
    other.client_request_id = "different".into();
    let (s, v, _) = rig.lease(&other).await;
    json_error(s, &v, 409, "nullifier_seen");
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_lease_and_clearance_leave_exactly_one_winner() {
    for n in 0..8u64 {
        let rig = Rig::new(Setup::default()).await;
        let r = rig.fresh(n + 1);
        let nullifier = felt_hex(&r.public_inputs.request_nullifier);
        let (lease, clearance) = tokio::join!(rig.lease(&r), rig.op.reserve_clearance(&nullifier));
        match (lease.0, clearance) {
            (201, Err(state)) => assert_eq!(state, "reserved"),
            (409, Ok(())) => assert_eq!(code(&lease.1), "clearance_signed", "{}", lease.1),
            other => panic!("two winners or none: {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn concurrent_lease_and_exit_import_leave_exactly_one_winner() {
    for n in 0..8u64 {
        let rig = Rig::new(Setup::default()).await;
        let r = rig.fresh(n + 1);
        let nullifier = felt_hex(&r.public_inputs.request_nullifier);
        let (lease, import) = tokio::join!(rig.lease(&r), rig.op.import_exit(&nullifier));
        match (lease.0, import) {
            (201, Err(state)) => assert_eq!(state, "reserved"),
            (409, Ok(())) => assert_eq!(code(&lease.1), "nullifier_in_exit", "{}", lease.1),
            other => panic!("two winners or none: {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signed_clearance_blocks_a_lease_for_good() {
    let rig = Rig::new(Setup::default()).await;
    let r = rig.fresh(1);
    rig.op
        .reserve_clearance(&felt_hex(&r.public_inputs.request_nullifier))
        .await
        .unwrap();
    for _ in 0..2 {
        let (s, v, _) = rig.lease(&r).await;
        json_error(s, &v, 409, "clearance_signed");
    }
}

// ---------------------------------------------------------------- reservation, replica, start-up

#[tokio::test(flavor = "multi_thread")]
async fn without_a_replica_acknowledgement_no_key_is_minted_and_the_nullifier_stays_unbound() {
    let rig = Rig::new(Setup::default()).await;
    rig.replica.set_down(true);
    let r = rig.fresh(1);
    let (s, v, h) = rig.lease(&r).await;
    json_error(s, &v, 503, "replica_unreachable");
    assert!(retry_after(&h) >= 1);
    assert_eq!(
        rig.provider.mint_calls(),
        0,
        "the reservation is replicated before a key is minted"
    );
    assert_eq!(
        rig.op
            .store()
            .run(|c| store::count_nullifiers(c))
            .await
            .unwrap(),
        0
    );
    rig.replica.set_down(false);
    assert_eq!(rig.lease(&r).await.0, 201);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_mint_rolls_the_reservation_back() {
    let rig = Rig::new(Setup::default()).await;
    rig.provider
        .set_mint_error(Some(ProviderError::Unavailable("down".into())));
    let r = rig.fresh(1);
    let (s, v, _) = rig.lease(&r).await;
    json_error(s, &v, 502, "provider_error");
    assert_eq!(
        rig.op
            .store()
            .run(|c| store::count_nullifiers(c))
            .await
            .unwrap(),
        0
    );
    assert_eq!(rig.op.ledger().open_bounds(), 0);
    rig.provider.set_mint_error(None);
    assert_eq!(rig.lease(&r).await.0, 201);
}

#[tokio::test(flavor = "multi_thread")]
async fn start_up_revokes_keys_without_a_record_and_keeps_recorded_ones() {
    let mut rig = Rig::new(Setup::default()).await;
    let (s, v, _) = rig.lease(&rig.fresh(1)).await;
    assert_eq!(s, 201);
    let recorded = v["lease_id"].as_str().unwrap().to_string();
    rig.provider.orphan_key("no-record", 1_000);
    rig.restart(Setup::default()).await;
    let keys = rig.provider.keys();
    assert!(
        keys["no-record"].revoked,
        "a paid key nobody tracks is revoked"
    );
    assert!(!keys[&recorded].revoked);
    assert_eq!(
        rig.op.ledger().open_bounds(),
        100_000,
        "open bounds are restored"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn start_up_rolls_back_a_reservation_whose_key_was_never_recorded() {
    let mut rig = Rig::new(Setup::default()).await;
    let r = rig.fresh(1);
    // A crash between reserving and recording the key: the row exists, provisioning, and the
    // provider may hold a key.
    let nullifier = felt_hex(&r.public_inputs.request_nullifier);
    let new = store::NewLease {
        nullifier: nullifier.clone(),
        client_request_id: r.client_request_id.clone(),
        lease_id: "crashed".into(),
        token_hash: token::hash("t"),
        bound: 100_000,
        request_json: "{}".into(),
        reserved_at: T0,
    };
    rig.op
        .store()
        .run(move |c| store::reserve_lease(c, &new))
        .await
        .unwrap();
    rig.provider.orphan_key("crashed", 1);
    rig.restart(Setup::default()).await;
    assert!(rig.provider.keys()["crashed"].revoked);
    assert_eq!(
        rig.op
            .store()
            .run(|c| store::count_nullifiers(c))
            .await
            .unwrap(),
        0
    );
    assert_eq!(rig.lease(&r).await.0, 201, "the nullifier is unbound again");
}

#[tokio::test(flavor = "multi_thread")]
async fn start_up_resends_or_rolls_back_an_unacknowledged_reservation() {
    for replica_up in [true, false] {
        let mut rig = Rig::new(Setup::default()).await;
        let r = rig.fresh(1);
        let new = store::NewLease {
            nullifier: felt_hex(&r.public_inputs.request_nullifier),
            client_request_id: r.client_request_id.clone(),
            lease_id: "unacked".into(),
            token_hash: token::hash("t"),
            bound: 100_000,
            request_json: serde_json::to_string(&r).unwrap(),
            reserved_at: T0,
        };
        rig.op
            .store()
            .run(move |c| {
                store::reserve_lease(c, &new)?;
                store::activate_lease(c, "unacked", "k-1", T0, T0 + 600)
            })
            .await
            .unwrap();
        rig.provider.orphan_key("unacked", 1);
        rig.replica.set_down(!replica_up);
        rig.restart(Setup::default()).await;
        let rows = rig.op.store().run(|c| store::all_leases(c)).await.unwrap();
        if replica_up {
            assert_eq!(rows.len(), 1);
            assert!(rows[0].replicated);
            assert_eq!(rig.replica.entries().len(), 1, "resent");
            assert!(!rig.provider.keys()["unacked"].revoked);
        } else {
            assert!(rows.is_empty(), "rolled back");
            assert!(rig.provider.keys()["unacked"].revoked);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_operator_refuses_to_start_against_another_operators_key() {
    let vault = Arc::new(nymira_operator::chain::FakeVault::new(
        vault_config(),
        &root_hex(),
    ));
    vault.edit(|v| v.config.operator_pk.x = felt_hex(&Felt252::from_u64(1)));
    let clock = Arc::new(nymira_operator::clock::ManualClock::new(T0 * 1000));
    let started = nymira_operator::Operator::start(nymira_operator::Parts {
        cfg: config(),
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
        store: nymira_operator::store::Store::open_in_memory().unwrap(),
        state_signer: Arc::new(zkapi_proof::compact::CompactSigner::from_seed(
            &Felt252::from_u64(1),
        )),
        signer: Arc::new(NoSigner),
        minter: Arc::new(nymira_operator::usage::Minter::generate()),
        alarms: std::sync::Arc::new(nymira_operator::alarm::Alarms::new(vec![], clock.clone())),
    })
    .await;
    assert!(matches!(
        started,
        Err(nymira_operator::operator::StartError::KeyMismatch)
    ));
}

// ---------------------------------------------------------------- config() follows the vault

#[tokio::test(flavor = "multi_thread")]
async fn the_operator_rereads_config_every_five_minutes() {
    let rig = Rig::new(Setup::default()).await;
    rig.vault.edit(|v| v.config.bound_classes = vec![200_000]);
    rig.clock.advance_secs(10);
    // Not read yet: the old classes still apply.
    assert_eq!(
        rig.lease(&variant(1, 100_000, rig.clock_now(), &root_hex()))
            .await
            .0,
        201
    );
    rig.clock.advance_secs(300);
    let (s, v, _) = rig
        .lease(&variant(2, 100_000, rig.clock_now(), &root_hex()))
        .await;
    json_error(s, &v, 400, "bound_class_invalid");
    assert_eq!(
        rig.lease(&variant(2, 200_000, rig.clock_now(), &root_hex()))
            .await
            .0,
        201
    );
    assert_eq!(rig.status().await["bound_classes"], json!(["200000"]));
}

#[tokio::test(flavor = "multi_thread")]
async fn leases_are_refused_while_params_applied_has_not_been_read() {
    let rig = Rig::new(Setup::default()).await;
    rig.vault.edit(|v| {
        v.config.charge_step = 500;
        v.failing = true;
    });
    rig.op.params_applied().await;
    rig.clock.advance_secs(2);
    let (s, v, h) = rig.lease(&rig.fresh(1)).await;
    json_error(s, &v, 429, "operator_capacity");
    assert!(retry_after(&h) >= 1);
    rig.vault.edit(|v| v.failing = false);
    let (s, _, _) = rig.lease(&rig.fresh(1)).await;
    assert_eq!(s, 201);
    assert_eq!(rig.status().await["charge_step"], "500");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_vault_that_cannot_be_read_stops_new_leases() {
    let rig = Rig::new(Setup::default()).await;
    rig.vault.edit(|v| v.failing = true);
    rig.clock.advance_secs(2);
    let (s, v, _) = rig.lease(&rig.fresh(1)).await;
    json_error(s, &v, 429, "operator_capacity");
    let _ = Value::Null;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_root_and_epoch_change_in_one_poll_gives_the_old_root_no_grace() {
    let rig = Rig::new(Setup::default()).await;
    let old = root_hex();
    let new = felt_hex(&Felt252::from_u64(0x77));
    rig.vault.edit(|v| {
        v.root = new.clone();
        v.epoch = 1;
    });
    rig.clock.advance_secs(2);
    let (s, v, _) = rig.lease(&variant(1, 100_000, rig.clock_now(), &old)).await;
    json_error(s, &v, 409, "root_stale");
    assert_eq!(
        rig.lease(&variant(1, 100_000, rig.clock_now(), &new))
            .await
            .0,
        201
    );
}

struct NoSigner;

#[async_trait::async_trait]
impl nymira_operator::signer::SignerApi for NoSigner {
    async fn sign_clearance(
        &self,
        _: Felt252,
    ) -> Result<zkapi_types::SchnorrSignature, nymira_operator::signer::SignerError> {
        Err(nymira_operator::signer::SignerError::Refused("none".into()))
    }

    async fn sign_next(
        &self,
        _: nymira_operator::signer::SignRequest,
    ) -> Result<zkapi_types::wire::RequestResponseV2, nymira_operator::signer::SignerError> {
        Err(nymira_operator::signer::SignerError::Refused(
            "no signer in this test".into(),
        ))
    }
}
