//! The recovery routes: what a client reads after a crash. `GET /v2/nullifiers/{n}`, its
//! evidence, `GET /v2/requests/{id}`, and what `expire` leaves behind.

mod common;

use common::*;
use nymira_operator::store;
use serde_json::Value;
use zkapi_types::wire::RecoveryResponseV2;

struct Lease {
    id: String,
    token: String,
    nullifier: String,
    client_request_id: String,
}

async fn open(rig: &Rig, n: u64) -> Lease {
    let request = rig.fresh(n);
    let (status, body, _) = rig.lease(&request).await;
    assert_eq!(status, 201, "{body}");
    Lease {
        id: body["lease_id"].as_str().unwrap().into(),
        token: body["lease_token"].as_str().unwrap().into(),
        nullifier: nullifier_hex(n),
        client_request_id: request.client_request_id,
    }
}

async fn post_lease_path(rig: &Rig, l: &Lease, suffix: &str) -> (u16, Value) {
    let response = rig
        .http
        .post(format!("{}/v2/openrouter/leases/{}{suffix}", rig.url, l.id))
        .header("x-nymira-lease-token", &l.token)
        .send()
        .await
        .unwrap();
    (
        response.status().as_u16(),
        response.json().await.unwrap_or(Value::Null),
    )
}

async fn settle(rig: &Rig, l: &Lease) {
    assert_eq!(post_lease_path(rig, l, "").await.0, 200);
    rig.op.tick().await;
    rig.clock.advance_secs(11);
    rig.op.tick().await;
    rig.op.tick().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_nullifier_nobody_has_seen_is_unknown() {
    let rig = Rig::new(Setup::default()).await;
    let (s, v) = rig
        .get_json(&format!("/v2/nullifiers/{}", nullifier_hex(1)))
        .await;
    assert_eq!(s, 200);
    assert_eq!(v["nullifier_status"], "unknown");
    assert_eq!(v["status"], "unknown");
    let (s, v) = rig
        .get_json(&format!("/v2/nullifiers/{}/evidence", nullifier_hex(1)))
        .await;
    assert_eq!(s, 200);
    assert_eq!(v["nullifier_status"], "unknown");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_nullifier_is_reserved_then_finalized_with_the_signed_state() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 2).await;
    let (_, v) = rig
        .get_json(&format!("/v2/nullifiers/{}", l.nullifier))
        .await;
    assert_eq!(v["nullifier_status"], "reserved");
    assert_eq!(v["status"], "pending");
    assert!(v["settlement_deadline"].is_u64());
    assert!(v.get("request_response").is_none());

    settle(&rig, &l).await;
    let (_, v) = rig
        .get_json(&format!("/v2/nullifiers/{}", l.nullifier))
        .await;
    let recovery: RecoveryResponseV2 = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(recovery.nullifier_status, "finalized");
    let response = recovery.request_response.expect("the signed state");
    assert_eq!(response.client_request_id, l.client_request_id);

    let (_, evidence) = rig
        .get_json(&format!("/v2/nullifiers/{}/evidence", l.nullifier))
        .await;
    assert_eq!(evidence["nullifier_status"], "finalized");
    assert_eq!(evidence["request_response"], v["request_response"]);
    assert_eq!(
        evidence["charge_applied"],
        response.charge_applied.to_string()
    );
    // The same answer by request id, and no key or token in any of them.
    let (s, by_request) = rig
        .get_json(&format!("/v2/requests/{}", l.client_request_id))
        .await;
    assert_eq!(s, 200);
    assert_eq!(by_request["request_response"], v["request_response"]);
    let provider_key = rig.provider.keys()[&l.id].key_id.clone();
    for body in [&v, &evidence, &by_request] {
        let text = body.to_string();
        assert!(!text.contains(&l.token), "no lease token");
        assert!(!text.contains(&provider_key), "no provider key");
        assert!(!text.contains(&l.id), "no lease id");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn clearance_and_exit_show_as_their_own_states() {
    let rig = Rig::new(Setup::default()).await;
    assert_eq!(rig.clearance(&rig.ownership(3)).await.0, 200);
    let (_, v) = rig
        .get_json(&format!("/v2/nullifiers/{}", nullifier_hex(3)))
        .await;
    assert_eq!(v["nullifier_status"], "clearance_reserved");
    let (_, v) = rig
        .get_json(&format!("/v2/nullifiers/{}/evidence", nullifier_hex(3)))
        .await;
    assert_eq!(v["nullifier_status"], "clearance_reserved");

    // An exit spends the nullifier for good: a wallet reads it as finalized, never as free.
    rig.op.import_exit(&nullifier_hex(4)).await.unwrap();
    let (_, v) = rig
        .get_json(&format!("/v2/nullifiers/{}", nullifier_hex(4)))
        .await;
    assert_eq!(v["nullifier_status"], "finalized");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_nullifier_is_read_in_either_case_and_anything_else_is_malformed() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 5).await;
    let upper = format!("0x{}", l.nullifier[2..].to_ascii_uppercase());
    let (s, v) = rig.get_json(&format!("/v2/nullifiers/{upper}")).await;
    assert_eq!(s, 200);
    assert_eq!(v["nullifier_status"], "reserved");
    for bad in [
        "1234",
        "0x12",
        &format!("0x{}", "g".repeat(64)),
        &"a".repeat(66),
    ] {
        let (s, v) = rig.get_json(&format!("/v2/nullifiers/{bad}")).await;
        json_error(s, &v, 400, "malformed_request");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_expired_lease_leaves_neither_its_proof_nor_its_nullifier() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 6).await;
    let id = l.id.clone();
    assert!(rig
        .op
        .store()
        .run(move |c| store::request_json(c, &id))
        .await
        .unwrap()
        .is_some());
    let (s, v) = post_lease_path(&rig, &l, "/expire").await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["charge_applied"], "0");
    let id = l.id.clone();
    assert!(
        rig.op
            .store()
            .run(move |c| store::request_json(c, &id))
            .await
            .unwrap()
            .is_none(),
        "the proof stored for the attempt is deleted at once"
    );
    let (_, v) = rig
        .get_json(&format!("/v2/nullifiers/{}", l.nullifier))
        .await;
    assert_eq!(v["nullifier_status"], "unknown");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_expire_keeps_the_proof_an_exit_answer_needs() {
    let rig = Rig::new(Setup::default()).await;
    let l = open(&rig, 7).await;
    rig.op.mark_delivered(&l.id).await.unwrap();
    let (s, v) = post_lease_path(&rig, &l, "/expire").await;
    json_error(s, &v, 409, "lease_used");
    let id = l.id.clone();
    let (row, proof) = rig
        .op
        .store()
        .run(move |c| Ok((store::lease(c, &id)?, store::request_json(c, &id)?)))
        .await
        .unwrap();
    assert!(row.unwrap().delivered, "the lease is delivered");
    assert!(
        proof.is_some(),
        "its request proof stays for `raise_concession`"
    );
    let (_, v) = rig
        .get_json(&format!("/v2/nullifiers/{}", l.nullifier))
        .await;
    assert_eq!(v["nullifier_status"], "reserved");
}
