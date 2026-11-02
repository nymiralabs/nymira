//! Durability at the operator: what waits for the replica, what happens when it stays away for
//! six hours, and what a restoring operator refuses.

mod common;

use common::*;
use nymira_operator::lifecycle::UNREAD_SETTLES_AFTER_SECS;
use nymira_operator::replog::kinds;
use nymira_operator::store;
use serde_json::Value;

struct Lease {
    id: String,
    token: String,
    nullifier: String,
}

async fn open(rig: &Rig, n: u64) -> Lease {
    let (status, body, _) = rig.lease(&rig.fresh(n)).await;
    assert_eq!(status, 201, "{body}");
    Lease {
        id: body["lease_id"].as_str().unwrap().into(),
        token: body["lease_token"].as_str().unwrap().into(),
        nullifier: nullifier_hex(n),
    }
}

async fn retire(rig: &Rig, l: &Lease) {
    let response = rig
        .http
        .post(format!("{}/v2/openrouter/leases/{}", rig.url, l.id))
        .header("x-nymira-lease-token", &l.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
}

async fn nullifier(rig: &Rig, l: &Lease) -> Value {
    rig.get_json(&format!("/v2/nullifiers/{}", l.nullifier))
        .await
        .1
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signed_state_is_on_the_replica_before_a_client_can_read_it() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 1).await;
    rig.provider.set_usage(&l.id, 40_000_000);
    retire(&rig, &l).await;
    rig.replica.set_down(true);
    rig.op.tick().await;
    rig.clock.advance_secs(11);
    for _ in 0..3 {
        rig.op.tick().await;
    }
    // The replica is away: no state exists for the client to read.
    let v = nullifier(&rig, &l).await;
    assert_eq!(v["nullifier_status"], "reserved");
    assert!(v.get("request_response").is_none());
    assert!(!rig.replica.records().iter().any(|r| r.0 == kinds::SIGNED));
    rig.replica.set_down(false);
    for _ in 0..3 {
        rig.op.tick().await;
    }
    let v = nullifier(&rig, &l).await;
    assert_eq!(v["nullifier_status"], "finalized");
    assert!(v["request_response"].is_object());
    let held = rig.replica.records();
    assert!(
        held.contains(&(kinds::SIGNED.into(), l.id.clone())),
        "{held:?}"
    );
    assert!(
        held.contains(&(kinds::SETTLED.into(), l.id.clone())),
        "{held:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn after_six_hours_without_the_replica_a_lease_settles_at_its_bound_and_no_state_is_signed() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 1).await;
    rig.provider.set_usage(&l.id, 40_000_000);
    retire(&rig, &l).await;
    rig.op.tick().await;
    rig.replica.set_down(true);
    rig.clock.advance_secs(11);
    rig.op.tick().await;
    rig.clock.advance_secs(UNREAD_SETTLES_AFTER_SECS - 12);
    for _ in 0..3 {
        rig.op.tick().await;
    }
    assert_eq!(nullifier(&rig, &l).await["nullifier_status"], "reserved");
    rig.clock.advance_secs(30);
    for _ in 0..3 {
        rig.op.tick().await;
    }
    let id = l.id.clone();
    let row = rig
        .op
        .store()
        .run(move |c| store::lease(c, &id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.status, "settled");
    assert_eq!(row.charge, Some(100_000), "the bound");
    assert_eq!(row.charge_basis.as_deref(), Some("link_down"));
    let id = l.id.clone();
    let signed = rig
        .op
        .store()
        .run(move |c| store::signed_state(c, &id))
        .await
        .unwrap();
    assert!(signed.is_none(), "no successor state was signed");
    // The client is told the nullifier is final and that there is no next state: it exits.
    let v = nullifier(&rig, &l).await;
    assert_eq!(v["nullifier_status"], "finalized");
    assert!(v.get("request_response").is_none() || v["request_response"].is_null());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reservation_that_is_undone_is_recorded_as_rolled_back() {
    let rig = Rig::new(Setup::default()).await;
    rig.provider
        .set_mint_error(Some(nymira_operator::provider::ProviderError::Unavailable(
            "down".into(),
        )));
    let (s, v, _) = rig.lease(&rig.fresh(1)).await;
    json_error(s, &v, 502, "provider_error");
    let held = rig.replica.records();
    assert_eq!(held.len(), 1, "{held:?}");
    assert_eq!(held[0].0, kinds::ROLLED_BACK);
}

#[tokio::test(flavor = "multi_thread")]
async fn only_the_first_delivery_is_recorded() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 1).await;
    rig.op.mark_delivered(&l.id).await.unwrap();
    rig.op.mark_delivered(&l.id).await.unwrap();
    let delivered: Vec<_> = rig
        .replica
        .records()
        .into_iter()
        .filter(|r| r.0 == kinds::DELIVERED)
        .collect();
    assert_eq!(delivered, [(kinds::DELIVERED.to_string(), l.id)]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clearance_is_on_the_replica_before_it_is_signed_and_refused_while_it_is_away() {
    let rig = Rig::new(Setup::default()).await;
    rig.replica.set_down(true);
    let (status, v) = rig.clearance(&rig.ownership(5)).await;
    json_error(status, &v, 503, "replica_unreachable");
    assert!(rig.replica.records().is_empty());
    rig.replica.set_down(false);
    let (status, v) = rig.clearance(&rig.ownership(5)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(
        rig.replica.records(),
        [(kinds::CLEARANCE.to_string(), nullifier_hex(5))]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_restoring_operator_signs_no_clearance_no_lease_and_says_so_but_still_serves_close() {
    let rig = Rig::new(Setup::default()).await;
    rig.vault.edit(|v| {
        v.notes.insert(0, "Active".into());
    });
    rig.replica.set_recovery(true);
    let (status, v) = rig.clearance(&rig.ownership(5)).await;
    json_error(status, &v, 503, "recovery_mode");
    let (status, v, _) = rig.lease(&rig.fresh(1)).await;
    json_error(status, &v, 503, "recovery_mode");
    assert_eq!(rig.status().await["recovery_mode"], true);
    // An operator-sent close signs nothing of the operator's: it is still served.
    let public = withdrawal_statement(1, 1, rig.clock_now() + 3_000);
    let (status, v) = rig.post_json("/v1/close", stub_close_body(&public)).await;
    assert_eq!(status, 200, "{v}");
    rig.replica.set_recovery(false);
    assert_eq!(rig.status().await["recovery_mode"], false);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_lease_is_recorded_as_rolled_back() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 1).await;
    let response = rig
        .http
        .post(format!("{}/v2/openrouter/leases/{}/expire", rig.url, l.id))
        .header("x-nymira-lease-token", &l.token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 200);
    assert!(rig
        .replica
        .records()
        .contains(&(kinds::ROLLED_BACK.into(), l.id)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_signature_asked_for_again_is_the_state_the_replica_already_holds() {
    use nymira_operator::signer::{SignRequest, SignerApi};
    use nymira_operator::usage::SignedUsage;
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 1).await;
    rig.provider.set_usage(&l.id, 40_000_000);
    retire(&rig, &l).await;
    rig.op.tick().await;
    rig.clock.advance_secs(11);
    rig.op.tick().await;
    let id = l.id.clone();
    let usage: SignedUsage = serde_json::from_str(
        &rig.op
            .store()
            .run(move |c| store::lease(c, &id))
            .await
            .unwrap()
            .unwrap()
            .usage_json
            .unwrap(),
    )
    .unwrap();
    let request = SignRequest {
        lease_id: l.id.clone(),
        charge: "42000".into(),
        usage,
    };
    let first = rig.signer.sign_next(request.clone()).await.unwrap();
    // The write to the local disk is lost (a crash or a timeout after the replica answered).
    rig.op
        .store()
        .run(|c| c.execute("DELETE FROM signed_states", []).map(|_| ()))
        .await
        .unwrap();
    let second = rig.signer.sign_next(request).await.unwrap();
    assert_eq!(
        serde_json::to_string(&first).unwrap(),
        serde_json::to_string(&second).unwrap(),
        "the same blind, the same signature"
    );
}
