//! `POST /v1/close`: the closer repeats the contract's checks, in order and before any gas is
//! spent, and sends `mutual_close` from the operator account.

mod common;

use std::os::unix::fs::MetadataExt;
use std::sync::Arc;

use common::*;
use nymira_operator::closer::{
    bind_socket, serve, CloseApi, Closer, SocketCloser, VERIFIES_PER_MINUTE,
};
use nymira_operator::verify::WithdrawalProofVerifier;
use nymira_testkit::proofs::WithdrawalKindId;
use serde_json::Value;
use zkapi_types::wire::Groth16ProofWire;
use zkapi_types::{Felt252, WithdrawalPublicInputsV2C};

async fn rig() -> Rig {
    let rig = Rig::new(Setup::default()).await;
    rig.vault.edit(|v| {
        v.notes.insert(0, "Active".into());
    });
    rig
}

fn statement(rig: &Rig, n: u64) -> WithdrawalPublicInputsV2C {
    withdrawal_statement(n, 1, rig.clock_now() + 3_000)
}

async fn close(rig: &Rig, public: &WithdrawalPublicInputsV2C) -> (u16, Value) {
    rig.post_json("/v1/close", stub_close_body(public)).await
}

fn words(public: &WithdrawalPublicInputsV2C) -> Vec<[u8; 32]> {
    public
        .to_field_elements()
        .iter()
        .map(|f| *f.as_bytes())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_real_close_proof_is_sent_and_a_forged_one_is_not() {
    let rig = Rig::new(Setup {
        verifier: Verifier::Real,
        ..Setup::default()
    })
    .await;
    rig.vault.edit(|v| {
        v.notes.insert(0, "Active".into());
    });
    let state = note().genesis(Felt252::from_u64(7));
    let bundle = gen()
        .withdrawal(
            &state,
            WithdrawalKindId::MutualClose,
            "alice.testnet",
            0,
            rig.clock_now() + 3_000,
        )
        .unwrap();
    let (status, v) = rig
        .post_json(
            "/v1/close",
            close_body(&bundle.inputs_wire, &bundle.proof_wire),
        )
        .await;
    assert_eq!(status, 200, "{v}");
    assert!(v["tx_hash"].as_str().is_some_and(|h| !h.is_empty()));
    let sent = rig.sender.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(
        sent[0].1, bundle.proof_wire,
        "the proof goes out as it came in"
    );
    assert_eq!(
        sent[0].2,
        bundle
            .inputs_wire
            .iter()
            .map(|w| w.to_vec())
            .collect::<Vec<_>>()
    );

    let mut one = [0u8; 32];
    one[31] = 1;
    let forged = bundle.with_input(9, one);
    // A forgery for another nullifier-free copy: the repeat window would hide it otherwise.
    let mut other = forged.clone();
    other.inputs_wire[11][31] ^= 1;
    let (status, v) = rig
        .post_json(
            "/v1/close",
            close_body(&other.inputs_wire, &other.proof_wire),
        )
        .await;
    json_error(status, &v, 422, "proof_invalid");
    assert_eq!(rig.sender.sent().len(), 1, "nothing more was sent");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_body_is_decoded_strictly_before_anything_else() {
    let rig = rig().await;
    let public = statement(&rig, 1);
    let good = stub_close_body(&public);
    let text = String::from_utf8(good.clone()).unwrap();
    let mut bad_word = words(&public);
    bad_word[12][0] = 1; // a kind with bits far above its width
    let mut short_word = serde_json::from_slice::<Value>(&good).unwrap();
    short_word["inputs"][3] = base64_of(&[0u8; 31]).into();
    let mut short_inputs = serde_json::from_slice::<Value>(&good).unwrap();
    short_inputs["inputs"].as_array_mut().unwrap().pop();
    let mut short_proof = serde_json::from_slice::<Value>(&good).unwrap();
    short_proof["proof"] = base64_of(&[0u8; 255]).into();
    let mut not_base64 = serde_json::from_slice::<Value>(&good).unwrap();
    not_base64["proof"] = "!!!".into();
    for (why, body) in [
        ("not json", b"nope".to_vec()),
        (
            "an extra field",
            text.replacen('{', r#"{"note":1,"#, 1).into_bytes(),
        ),
        (
            "a duplicate key",
            text.replacen('{', r#"{"proof":"AA==","#, 1).into_bytes(),
        ),
        ("a short proof", serde_json::to_vec(&short_proof).unwrap()),
        ("15 inputs", serde_json::to_vec(&short_inputs).unwrap()),
        ("a short input", serde_json::to_vec(&short_word).unwrap()),
        (
            "a proof that is not base64",
            serde_json::to_vec(&not_base64).unwrap(),
        ),
        (
            "a non-canonical kind word",
            close_body(&bad_word, &[0u8; 256]),
        ),
    ] {
        let (status, v) = rig.post_json("/v1/close", body).await;
        assert_eq!((status, code(&v)), (400, "malformed_request"), "{why}: {v}");
    }
    let (status, v) = rig.post_json("/v1/close", vec![b' '; 70 * 1024]).await;
    json_error(status, &v, 413, "body_too_large");
    assert!(rig.sender.sent().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_public_inputs_must_name_this_vault_chain_and_both_keys() {
    let rig = rig().await;
    let now = rig.clock_now();
    let base = |n| withdrawal_statement(n, 1, now + 3_000);
    for (why, public) in [
        ("another chain", {
            let mut p = base(10);
            p.chain_id += 1;
            p
        }),
        ("another vault", {
            let mut p = base(11);
            p.contract_address = Felt252::from_u64(9);
            p
        }),
        ("another protocol version", {
            let mut p = base(12);
            p.protocol_version += 1;
            p
        }),
        ("another state key", {
            let mut p = base(13);
            p.state_signing_key_x = Felt252::from_u64(9);
            p
        }),
        ("another clearance key", {
            let mut p = base(14);
            p.clearance_signing_key_y = Felt252::from_u64(9);
            p
        }),
        (
            "an ownership proof",
            withdrawal_statement(15, 2, now + 3_000),
        ),
        ("an escape", withdrawal_statement(16, 0, now + 3_000)),
    ] {
        let (status, v) = close(&rig, &public).await;
        assert_eq!((status, code(&v)), (422, "proof_invalid"), "{why}: {v}");
    }
    assert!(rig.sender.sent().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_proof_must_stay_valid_for_ten_more_minutes() {
    let rig = rig().await;
    let now = rig.clock_now();
    let (status, v) = close(&rig, &withdrawal_statement(20, 1, now + 599)).await;
    json_error(status, &v, 400, "proof_expired");
    let (status, v) = close(&rig, &withdrawal_statement(21, 1, now)).await;
    json_error(status, &v, 400, "proof_expired");
    let (status, v) = close(&rig, &withdrawal_statement(22, 1, now + 600)).await;
    assert_eq!(status, 200, "ten minutes exactly is enough: {v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_note_must_be_active_and_its_nullifier_unspent_with_no_exit_pending() {
    let rig = rig().await;
    rig.vault.edit(|v| {
        v.notes.insert(1, "Closed".into());
        v.notes.insert(2, "PendingExit".into());
        v.notes.insert(3, "Active".into());
        v.notes.insert(4, "Active".into());
        v.used.insert(nullifier_hex(33));
        v.exits.push(nymira_operator::chain::ExitEntry {
            note_id: 4,
            nullifier: nullifier_hex(900),
            next_nullifier: nullifier_hex(901),
        });
    });
    let on = |n, note_id| {
        let mut p = statement(&rig, n);
        p.note_id = note_id;
        p
    };
    for (why, public, status, expect) in [
        ("a closed note", on(30, 1), 409, "note_not_active"),
        (
            "a note the vault never issued",
            on(31, 77),
            409,
            "note_not_active",
        ),
        ("a note in an exit", on(32, 2), 409, "nullifier_in_exit"),
        ("a spent nullifier", on(33, 3), 409, "nullifier_in_exit"),
        (
            "an exit listed for the note",
            on(34, 4),
            409,
            "nullifier_in_exit",
        ),
    ] {
        let (s, v) = close(&rig, &public).await;
        assert_eq!((s, code(&v)), (status, expect), "{why}: {v}");
    }
    assert!(rig.sender.sent().is_empty());

    rig.vault.edit(|v| v.failing = true);
    let (s, v) = close(&rig, &statement(&rig, 35)).await;
    json_error(s, &v, 429, "operator_capacity");
    assert!(v["error"]["retry_after_secs"].is_u64());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_proof_the_verifier_rejects_is_never_sent() {
    let rig = Rig::new(Setup {
        withdrawal: Some(Arc::new(Reject)),
        ..Setup::default()
    })
    .await;
    rig.vault.edit(|v| {
        v.notes.insert(0, "Active".into());
    });
    let (s, v) = close(&rig, &statement(&rig, 40)).await;
    json_error(s, &v, 422, "proof_invalid");
    assert!(rig.sender.sent().is_empty());
}

struct Reject;

impl WithdrawalProofVerifier for Reject {
    fn verify(&self, _: &WithdrawalPublicInputsV2C, _: &Groth16ProofWire) -> bool {
        false
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repeat_inside_ten_minutes_gets_the_same_hash_and_sends_once() {
    let rig = rig().await;
    let public = statement(&rig, 50);
    let (s, first) = close(&rig, &public).await;
    assert_eq!(s, 200);
    // The vault has not caught up, and later the note is closed: the repeat is still answered.
    rig.clock.advance_secs(599);
    let (s, again) = close(&rig, &public).await;
    assert_eq!(s, 200, "{again}");
    assert_eq!(first["tx_hash"], again["tx_hash"]);
    assert_eq!(rig.sender.sent().len(), 1);
    rig.vault.edit(|v| {
        v.notes.insert(0, "Closed".into());
    });
    let (s, again) = close(&rig, &public).await;
    assert_eq!(s, 200, "{again}");
    assert_eq!(first["tx_hash"], again["tx_hash"]);

    // After ten minutes the answer is the vault's.
    rig.clock.advance_secs(2);
    let mut later = public.clone();
    later.valid_until += 700;
    let (s, v) = close(&rig, &later).await;
    json_error(s, &v, 409, "note_not_active");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_send_is_a_provider_error() {
    let rig = rig().await;
    rig.sender.set_failing(true);
    let (s, v) = close(&rig, &statement(&rig, 60)).await;
    json_error(s, &v, 502, "provider_error");
}

// ---------------------------------------------------------------- limits

/// Holds every verification until opened.
#[derive(Default)]
struct Held {
    entered: std::sync::atomic::AtomicUsize,
    open: std::sync::atomic::AtomicBool,
}

impl WithdrawalProofVerifier for Held {
    fn verify(&self, _: &WithdrawalPublicInputsV2C, _: &Groth16ProofWire) -> bool {
        self.entered
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        while !self.open.load(std::sync::atomic::Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        true
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_closer_verifies_two_proofs_at_once_and_a_third_is_refused() {
    let held = Arc::new(Held::default());
    let rig = Rig::new(Setup {
        withdrawal: Some(held.clone()),
        ..Setup::default()
    })
    .await;
    rig.vault.edit(|v| {
        v.notes.insert(0, "Active".into());
    });
    let (a, b) = (statement(&rig, 70), statement(&rig, 71));
    let first = tokio::spawn({
        let (http, url, body) = (rig.http.clone(), rig.url.clone(), stub_close_body(&a));
        async move {
            http.post(format!("{url}/v1/close"))
                .body(body)
                .send()
                .await
                .unwrap()
                .status()
        }
    });
    let second = tokio::spawn({
        let (http, url, body) = (rig.http.clone(), rig.url.clone(), stub_close_body(&b));
        async move {
            http.post(format!("{url}/v1/close"))
                .body(body)
                .send()
                .await
                .unwrap()
                .status()
        }
    });
    while held.entered.load(std::sync::atomic::Ordering::SeqCst) < 2 {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let (s, v) = close(&rig, &statement(&rig, 72)).await;
    json_error(s, &v, 429, "operator_capacity");
    held.open.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(first.await.unwrap(), 200);
    assert_eq!(second.await.unwrap(), 200);
}

#[tokio::test(flavor = "multi_thread")]
async fn at_most_twenty_verifications_run_in_a_minute() {
    let rig = rig().await;
    for n in 0..VERIFIES_PER_MINUTE as u64 {
        let (s, v) = close(&rig, &statement(&rig, 100 + n)).await;
        assert_eq!(s, 200, "{n}: {v}");
    }
    let (s, v) = close(&rig, &statement(&rig, 150)).await;
    json_error(s, &v, 429, "close_rate_limited");
    assert!(v["error"]["retry_after_secs"].as_u64().unwrap() <= 60);
    assert_eq!(rig.sender.sent().len(), 20, "the 21st sent nothing");
    rig.clock.advance_secs(61);
    let (s, v) = close(&rig, &statement(&rig, 150)).await;
    assert_eq!(s, 200, "a minute later: {v}");
}

#[tokio::test(flavor = "multi_thread")]
async fn at_most_fifty_closes_are_sent_in_a_day_and_the_thirtieth_raises_an_alarm() {
    let rig = rig().await;
    let mut n = 200;
    while rig.sender.sent().len() < 50 {
        for _ in 0..VERIFIES_PER_MINUTE {
            if rig.sender.sent().len() == 50 {
                break;
            }
            let (s, v) = close(&rig, &statement(&rig, n)).await;
            assert_eq!(s, 200, "{n}: {v}");
            n += 1;
            if rig.sender.sent().len() == 29 {
                assert!(rig.sink.sent().is_empty(), "no alarm before the 30th");
            }
            if rig.sender.sent().len() == 30 {
                assert_eq!(rig.sink.sent().len(), 1, "the 30th raises it once");
            }
        }
        rig.clock.advance_secs(61);
    }
    let (s, v) = close(&rig, &statement(&rig, n)).await;
    json_error(s, &v, 429, "close_rate_limited");
    assert_eq!(rig.sender.sent().len(), 50);
    assert_eq!(rig.sink.sent().len(), 1, "one alarm, not one per close");
    // The day ends at UTC midnight.
    rig.clock.advance_secs(86_400);
    let (s, v) = close(&rig, &statement(&rig, n)).await;
    assert_eq!(s, 200, "the next day: {v}");
}

// ---------------------------------------------------------------- the process boundary

#[tokio::test(flavor = "multi_thread")]
async fn the_closer_answers_over_its_socket_and_only_to_its_user() {
    let rig = rig().await;
    let alarms = Arc::new(nymira_operator::alarm::Alarms::new(
        vec![],
        rig.clock.clone(),
    ));
    let closer: Arc<dyn CloseApi> = Arc::new(Closer::new(
        rig.vault.clone(),
        Arc::new(AcceptWithdrawals),
        rig.sender.clone(),
        rig.clock.clone(),
        alarms,
        nymira_near_codec::chain_id(network()),
        contract(),
    ));
    let uid = std::fs::metadata(rig.dir.path()).unwrap().uid();
    let path = rig.dir.path().join("closer.sock");
    tokio::spawn(serve(bind_socket(&path).unwrap(), closer.clone(), uid));
    let over_socket = SocketCloser(path);
    let hash = over_socket
        .close(&stub_close_body(&statement(&rig, 300)))
        .await
        .expect("sent");
    assert!(!hash.is_empty());
    let refused = over_socket
        .close(&stub_close_body(&withdrawal_statement(
            301,
            1,
            rig.clock_now(),
        )))
        .await
        .unwrap_err();
    assert_eq!(refused.code, "proof_expired", "the code crosses the socket");
    assert!(over_socket.close(b"not json").await.is_err());

    let other = rig.dir.path().join("other.sock");
    tokio::spawn(serve(bind_socket(&other).unwrap(), closer, uid + 1));
    assert!(SocketCloser(other)
        .close(&stub_close_body(&statement(&rig, 302)))
        .await
        .is_err());
    assert_eq!(rig.sender.sent().len(), 1);
}
