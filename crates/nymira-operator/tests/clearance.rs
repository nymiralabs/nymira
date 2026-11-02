//! Clearance: an ownership proof for a nullifier with no open lease gets a signature under the
//! clearance key, and the nullifier is spent at this operator for good.

mod common;

use std::os::unix::fs::MetadataExt;

use common::*;
use nymira_operator::signer::{bind_socket, serve, SignerApi, SignerError, SocketSigner};
use nymira_testkit::proofs::WithdrawalKindId;
use zkapi_core::v2::clearance_message;
use zkapi_proof::compact::CompactSigner;
use zkapi_types::wire::ClearanceResponseV2;
use zkapi_types::{Felt252, PROTOCOL_VERSION};

fn verifies_under(seed: u64, nullifier: &Felt252, response: &ClearanceResponseV2) -> bool {
    let message = clearance_message(
        PROTOCOL_VERSION,
        nymira_near_codec::chain_id(network()),
        &contract(),
        nullifier,
    );
    CompactSigner::from_seed(&Felt252::from_u64(seed))
        .verify(&message, &response.signature)
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_ownership_proof_gets_a_signature_under_the_clearance_key_only() {
    let rig = Rig::new(Setup {
        verifier: Verifier::Real,
        ..Setup::default()
    })
    .await;
    let state = note().genesis(Felt252::from_u64(7));
    let bundle = gen()
        .withdrawal(
            &state,
            WithdrawalKindId::Ownership,
            "",
            0,
            rig.clock_now() + 3_000,
        )
        .unwrap();
    let public = statement_of(&bundle);
    let (status, body) = rig
        .post_json(
            "/v2/withdraw/clearance",
            clearance_body(&public, wire_proof(&bundle)),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let response: ClearanceResponseV2 = serde_json::from_value(body).unwrap();
    assert_eq!(response.withdrawal_nullifier, public.withdrawal_nullifier);
    assert!(
        verifies_under(2, &public.withdrawal_nullifier, &response),
        "the clearance key (seed 2) signed it"
    );
    assert!(
        !verifies_under(1, &public.withdrawal_nullifier, &response),
        "and the state key (seed 1) did not"
    );

    // A forged public input does not verify.
    let forged = bundle.with_input(9, [0xff; 32]);
    let mut other = statement_of(&bundle);
    other.final_balance += 1;
    let (status, v) = rig
        .post_json(
            "/v2/withdraw/clearance",
            clearance_body(&other, wire_proof(&forged)),
        )
        .await;
    json_error(status, &v, 422, "proof_invalid");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clearance_is_refused_while_a_lease_is_open() {
    let rig = Rig::new(Setup::default()).await;
    let (s, v, _) = rig.lease(&rig.fresh(1)).await;
    assert_eq!(s, 201, "{v}");
    let (status, v) = rig.clearance(&rig.ownership(1)).await;
    json_error(status, &v, 409, "lease_open");
    let (_, v) = rig
        .get_json(&format!("/v2/nullifiers/{}", nullifier_hex(1)))
        .await;
    assert_eq!(v["nullifier_status"], "reserved", "the lease keeps it");
}

#[tokio::test(flavor = "multi_thread")]
async fn after_a_clearance_a_lease_on_the_nullifier_is_refused_for_good() {
    let rig = Rig::new(Setup::default()).await;
    let (status, v) = rig.clearance(&rig.ownership(2)).await;
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["status"], "signed");

    let (s, v, _) = rig.lease(&rig.fresh(2)).await;
    json_error(s, &v, 409, "clearance_signed");
    // Not for 300 seconds: for good, also across a restart.
    rig.clock.advance_secs(24 * 3_600);
    let (s, v, _) = rig
        .lease(&variant(2, 100_000, rig.clock_now(), &root_hex()))
        .await;
    json_error(s, &v, 409, "clearance_signed");
    let (_, v) = rig
        .get_json(&format!("/v2/nullifiers/{}", nullifier_hex(2)))
        .await;
    assert_eq!(v["nullifier_status"], "clearance_reserved");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clearance_survives_a_restart_and_can_be_asked_for_again() {
    let mut rig = Rig::new(Setup::default()).await;
    let (status, first) = rig.clearance(&rig.ownership(3)).await;
    assert_eq!(status, 200);
    rig.restart(Setup::default()).await;
    let (s, v, _) = rig.lease(&rig.fresh(3)).await;
    json_error(s, &v, 409, "clearance_signed");
    // A client that lost the answer asks again and gets a signature for the same message.
    let (status, again) = rig.clearance(&rig.ownership(3)).await;
    assert_eq!(status, 200, "{again}");
    let response: ClearanceResponseV2 = serde_json::from_value(again).unwrap();
    assert!(verifies_under(2, &response.withdrawal_nullifier, &response));
    let _ = first;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lease_and_a_clearance_racing_for_one_nullifier_leave_one_winner() {
    for n in 10..30 {
        let rig = Rig::new(Setup::default()).await;
        let request = rig.fresh(n);
        let public = rig.ownership(n);
        let (lease, clearance) = tokio::join!(rig.lease(&request), rig.clearance(&public));
        let lease_won = lease.0 == 201;
        let clearance_won = clearance.0 == 200;
        assert!(
            lease_won ^ clearance_won,
            "exactly one wins: lease {} {}, clearance {} {}",
            lease.0,
            lease.1,
            clearance.0,
            clearance.1
        );
        if lease_won {
            assert_eq!(code(&clearance.1), "lease_open");
        } else {
            assert_eq!(code(&lease.1), "clearance_signed");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clearance_for_a_nullifier_in_an_exit_or_a_finished_lease_is_refused() {
    let rig = Rig::new(Setup::default()).await;
    rig.op.import_exit(&nullifier_hex(4)).await.unwrap();
    let (status, v) = rig.clearance(&rig.ownership(4)).await;
    json_error(status, &v, 409, "nullifier_in_exit");

    let (s, lease, _) = rig.lease(&rig.fresh(5)).await;
    assert_eq!(s, 201);
    let expired = rig
        .http
        .post(format!(
            "{}/v2/openrouter/leases/{}/expire",
            rig.url,
            lease["lease_id"].as_str().unwrap()
        ))
        .header(
            "x-nymira-lease-token",
            lease["lease_token"].as_str().unwrap(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(expired.status(), 200);
    let (status, v) = rig.clearance(&rig.ownership(5)).await;
    assert_eq!(status, 200, "an expired lease unbinds the nullifier: {v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_ownership_proof_is_checked_before_a_signature_is_given() {
    let rig = Rig::new(Setup::default()).await;
    let now = rig.clock_now();
    let ok = withdrawal_statement(40, 2, now + 3_300);
    let (status, v) = rig.clearance(&ok).await;
    assert_eq!(status, 200, "valid for exactly 3,300 seconds: {v}");

    for (why, public, expect, expected_status) in [
        (
            "valid for longer than 3,300 seconds",
            withdrawal_statement(41, 2, now + 3_301),
            "proof_expired",
            400,
        ),
        (
            "already over",
            withdrawal_statement(42, 2, now - 1),
            "proof_expired",
            400,
        ),
        (
            "a mutual close, not an ownership proof",
            withdrawal_statement(43, 1, now + 3_000),
            "proof_invalid",
            422,
        ),
        (
            "an escape",
            withdrawal_statement(44, 0, now + 3_000),
            "proof_invalid",
            422,
        ),
        (
            "another chain",
            {
                let mut p = withdrawal_statement(45, 2, now + 3_000);
                p.chain_id += 1;
                p
            },
            "proof_invalid",
            422,
        ),
        (
            "another vault",
            {
                let mut p = withdrawal_statement(46, 2, now + 3_000);
                p.contract_address = Felt252::from_u64(9);
                p
            },
            "proof_invalid",
            422,
        ),
        (
            "another state key",
            {
                let mut p = withdrawal_statement(47, 2, now + 3_000);
                p.state_signing_key_x = Felt252::from_u64(9);
                p
            },
            "proof_invalid",
            422,
        ),
        (
            "another clearance key",
            {
                let mut p = withdrawal_statement(48, 2, now + 3_000);
                p.clearance_signing_key_y = Felt252::from_u64(9);
                p
            },
            "proof_invalid",
            422,
        ),
        (
            "a successor published",
            {
                let mut p = withdrawal_statement(49, 2, now + 3_000);
                p.next_nullifier = Felt252::from_u64(1);
                p
            },
            "proof_invalid",
            422,
        ),
        (
            "a destination set",
            {
                let mut p = withdrawal_statement(50, 2, now + 3_000);
                p.destination = [1; 20];
                p
            },
            "proof_invalid",
            422,
        ),
        (
            "a concession",
            {
                let mut p = withdrawal_statement(51, 2, now + 3_000);
                p.conceded = 1;
                p
            },
            "proof_invalid",
            422,
        ),
        (
            "another protocol version",
            {
                let mut p = withdrawal_statement(52, 2, now + 3_000);
                p.protocol_version += 1;
                p
            },
            "unsupported_version",
            400,
        ),
    ] {
        let (status, v) = rig.clearance(&public).await;
        assert_eq!((status, code(&v)), (expected_status, expect), "{why}: {v}");
        let (_, st) = rig
            .get_json(&format!("/v2/nullifiers/{}", felt_hex_of(&public)))
            .await;
        assert_eq!(
            st["nullifier_status"], "unknown",
            "{why}: nothing is reserved"
        );
    }

    // The nullifier named outside the proof is the proof's.
    let public = rig.ownership(53);
    let mut body: serde_json::Value =
        serde_json::from_slice(&clearance_body(&public, dummy_proof())).unwrap();
    body["withdrawal_nullifier"] = format!("0x{:064x}", 9).into();
    let (status, v) = rig
        .post_json("/v2/withdraw/clearance", serde_json::to_vec(&body).unwrap())
        .await;
    json_error(status, &v, 400, "malformed_request");

    // A proof the verifier rejects reserves nothing.
    let rig = Rig::new(Setup {
        withdrawal: Some(std::sync::Arc::new(Reject)),
        ..Setup::default()
    })
    .await;
    let (status, v) = rig.clearance(&rig.ownership(54)).await;
    json_error(status, &v, 422, "proof_invalid");
    let (_, st) = rig
        .get_json(&format!("/v2/nullifiers/{}", nullifier_hex(54)))
        .await;
    assert_eq!(st["nullifier_status"], "unknown");
}

struct Reject;

impl nymira_operator::verify::WithdrawalProofVerifier for Reject {
    fn verify(
        &self,
        _: &zkapi_types::WithdrawalPublicInputsV2C,
        _: &zkapi_types::wire::Groth16ProofWire,
    ) -> bool {
        false
    }
}

fn felt_hex_of(public: &zkapi_types::WithdrawalPublicInputsV2C) -> String {
    nymira_operator::operator::felt_hex(&public.withdrawal_nullifier)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_clearance_body_is_decoded_strictly() {
    let rig = Rig::new(Setup::default()).await;
    let public = rig.ownership(60);
    let good = clearance_body(&public, dummy_proof());
    let text = String::from_utf8(good.clone()).unwrap();
    for (why, body) in [
        ("not json", b"nope".to_vec()),
        (
            "an extra field",
            text.replacen('{', r#"{"prompt":"hi","#, 1).into_bytes(),
        ),
        (
            "a duplicate key",
            text.replacen('{', &format!(r#"{{"withdrawal_nullifier":"{}","#, "0x1"), 1)
                .into_bytes(),
        ),
        (
            "a missing proof",
            br#"{"withdrawal_nullifier":"0x1"}"#.to_vec(),
        ),
    ] {
        let (status, v) = rig.post_json("/v2/withdraw/clearance", body).await;
        assert_eq!((status, code(&v)), (400, "malformed_request"), "{why}: {v}");
    }
    let (status, v) = rig
        .post_json("/v2/withdraw/clearance", vec![b' '; 70 * 1024])
        .await;
    json_error(status, &v, 413, "body_too_large");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_nullifier_is_limited_to_three_clearance_attempts_in_five_minutes() {
    let rig = Rig::new(Setup {
        withdrawal: Some(std::sync::Arc::new(Reject)),
        ..Setup::default()
    })
    .await;
    let public = rig.ownership(70);
    // Refused by the pairing, which comes after the counter.
    for _ in 0..3 {
        let (status, v) = rig.clearance(&public).await;
        json_error(status, &v, 422, "proof_invalid");
    }
    let (status, v) = rig.clearance(&public).await;
    json_error(status, &v, 429, "nullifier_rate_limited");
}

// ---------------------------------------------------------------- the signer

#[tokio::test(flavor = "multi_thread")]
async fn the_signer_signs_a_clearance_only_for_a_reserved_nullifier() {
    let rig = Rig::new(Setup::default()).await;
    let n = Felt252::from_u64(1_080);
    let err = rig.signer.sign_clearance(n).await.unwrap_err();
    assert!(
        matches!(err, SignerError::Refused(_)),
        "nothing reserved: {err}"
    );

    rig.op
        .reserve_clearance(&nullifier_hex(80))
        .await
        .expect("reserved");
    rig.signer.sign_clearance(n).await.expect("now it signs");

    // A nullifier that holds a lease is refused even if a row says otherwise.
    let (s, _, _) = rig.lease(&rig.fresh(81)).await;
    assert_eq!(s, 201);
    assert!(rig
        .signer
        .sign_clearance(Felt252::from_u64(1_081))
        .await
        .is_err());

    // A nullifier seen in an exit is refused.
    rig.op.reserve_clearance(&nullifier_hex(82)).await.unwrap();
    rig.op.import_exit(&nullifier_hex(82)).await.unwrap_err();
    assert!(rig
        .signer
        .sign_clearance(Felt252::from_u64(1_082))
        .await
        .is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_signer_answers_a_clearance_over_its_socket_and_only_to_its_user() {
    let rig = Rig::new(Setup::default()).await;
    rig.op.reserve_clearance(&nullifier_hex(90)).await.unwrap();
    let path = rig.dir.path().join("signer.sock");
    let uid = std::fs::metadata(rig.dir.path()).unwrap().uid();
    tokio::spawn(serve(bind_socket(&path).unwrap(), rig.signer.clone(), uid));
    let signature = SocketSigner(path.clone())
        .sign_clearance(Felt252::from_u64(1_090))
        .await
        .expect("signed over the socket");
    let response = ClearanceResponseV2 {
        status: "signed".into(),
        withdrawal_nullifier: Felt252::from_u64(1_090),
        signature,
    };
    assert!(verifies_under(2, &response.withdrawal_nullifier, &response));

    let other = rig.dir.path().join("other.sock");
    tokio::spawn(serve(
        bind_socket(&other).unwrap(),
        rig.signer.clone(),
        uid + 1,
    ));
    assert!(SocketSigner(other)
        .sign_clearance(Felt252::from_u64(1_090))
        .await
        .is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_note_that_is_not_active_gets_no_clearance() {
    let rig = Rig::new(Setup::default()).await;
    for (n, status, code) in [
        (60, "Closed", "note_not_active"),
        (61, "PendingExit", "nullifier_in_exit"),
    ] {
        rig.vault.edit(|v| {
            v.notes.insert(0, status.into());
        });
        let (s, v) = rig.clearance(&rig.ownership(n)).await;
        json_error(s, &v, 409, code);
    }
    rig.vault.edit(|v| {
        v.notes.remove(&0);
    });
    let (s, v) = rig.clearance(&rig.ownership(62)).await;
    json_error(s, &v, 409, "note_not_active");
    // Nothing was reserved: once the note is active the same nullifier clears.
    rig.vault.edit(|v| {
        v.notes.insert(0, "Active".into());
    });
    assert_eq!(rig.clearance(&rig.ownership(62)).await.0, 200);
}
