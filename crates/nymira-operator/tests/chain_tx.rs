//! The operator's own transactions against the fake NEAR RPC: written before they are sent,
//! resent unchanged, awaited to FINAL, receipts read.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use ed25519_dalek::SigningKey;
use nymira_fake_near_rpc::{
    AccessKeySpec, AccountSpec, Failure, FakeRpc, OutcomeSpec, ReceiptSpec, TxStatusSpec,
};
use nymira_operator::alarm::{Alarms, FakeSink};
use nymira_operator::clock::ManualClock;
use nymira_operator::closer::{CloseSender, NearCloseSender};
use nymira_operator::signer::HeartbeatSender;
use nymira_operator::store::{self, Store};
use nymira_operator::tx::{
    sign_transaction, transaction_bytes, NearHeartbeat, NearTx, TxConfig, TxRequest,
};
use serde_json::json;

const OPERATOR: &str = "operator.nymira.testnet";
const VAULT: &str = "vault.nymira.testnet";

struct Env {
    rpc: FakeRpc,
    store: Store,
    clock: Arc<ManualClock>,
    key: SigningKey,
    _dir: tempfile::TempDir,
}

async fn env(on_chain_nonce: u64) -> Env {
    let rpc = FakeRpc::start().await;
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let public = format!(
        "ed25519:{}",
        bs58::encode(key.verifying_key().as_bytes()).into_string()
    );
    rpc.set_account(OPERATOR, AccountSpec::default());
    rpc.set_access_keys(
        OPERATOR,
        vec![AccessKeySpec {
            public_key: public,
            nonce: on_chain_nonce,
            permission: json!({"FunctionCall": {
                "allowance": null, "receiver_id": VAULT, "method_names": ["heartbeat", "mutual_close"]
            }}),
        }],
    );
    let dir = tempfile::tempdir().unwrap();
    Env {
        store: Store::open(&dir.path().join("db")).unwrap(),
        clock: Arc::new(ManualClock::new(1_900_000_000_000)),
        rpc,
        key,
        _dir: dir,
    }
}

fn near_tx(e: &Env) -> NearTx {
    NearTx::new(
        e.rpc.url(),
        OPERATOR,
        VAULT,
        e.key.clone(),
        e.store.clone(),
        e.clock.clone(),
    )
    .with_config(TxConfig {
        send_timeout: Duration::from_millis(400),
        retry_delay: Duration::from_millis(50),
        max_attempts: 5,
    })
}

fn request(idem: Option<&str>) -> TxRequest {
    TxRequest {
        kind: "close",
        method: "mutual_close",
        args: br#"{"proof":"AA==","inputs":[]}"#.to_vec(),
        gas: 100_000_000_000_000,
        idem_key: idem.map(String::from),
    }
}

async fn rows(e: &Env, pk: &str) -> Vec<store::TxRow> {
    let pk = pk.to_string();
    e.store
        .run(move |c| {
            let mut s =
                c.prepare("SELECT id FROM operator_txs WHERE public_key = ?1 ORDER BY id")?;
            let ids: Vec<i64> = s.query_map([pk], |r| r.get(0))?.collect::<Result<_, _>>()?;
            ids.into_iter()
                .map(|id| store::tx(c, id).map(|r| r.unwrap()))
                .collect()
        })
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_transaction_is_signed_stored_then_broadcast_and_awaited_to_final() {
    let e = env(41).await;
    let tx = near_tx(&e);
    let end = tx.send(request(None)).await.unwrap();
    assert!(end.success, "{end:?}");

    let submissions = e.rpc.submissions();
    assert_eq!(submissions.len(), 1);
    let sent = &submissions[0];
    assert_eq!(sent.method, "send_tx");
    assert_eq!(sent.wait_until.as_deref(), Some("FINAL"));
    assert_eq!(
        (sent.signer_id.as_str(), sent.receiver_id.as_str()),
        (OPERATOR, VAULT)
    );
    assert_eq!(sent.nonce, 42, "one above the key's on-chain nonce");
    assert_eq!(sent.tx_hash, end.hash);

    let row = rows(&e, &tx.public_key()).await.remove(0);
    assert_eq!(row.state, "final");
    assert_eq!(row.nonce, Some(42));
    assert_eq!(row.hash.as_deref(), Some(end.hash.as_str()));
    assert_eq!(
        base64::engine::general_purpose::STANDARD.encode(row.signed_bytes.unwrap()),
        sent.tx_base64,
        "the stored bytes are the sent bytes"
    );
    // The node learned the block hash from a final block.
    let asked = e.rpc.requests();
    assert!(asked
        .iter()
        .any(|r| r.method == "block" && r.params["finality"] == "final"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_transaction_is_valid_borsh_signed_over_its_hash() {
    let key = SigningKey::from_bytes(&[3u8; 32]);
    let bytes = transaction_bytes(
        OPERATOR,
        key.verifying_key().as_bytes(),
        9,
        VAULT,
        &[5u8; 32],
        "heartbeat",
        b"{}",
        10_000_000_000_000,
    );
    let (signed, hash) = sign_transaction(&key, &bytes);
    assert_eq!(&signed[..bytes.len()], &bytes[..]);
    assert_eq!(signed.len(), bytes.len() + 65);
    assert_eq!(signed[bytes.len()], 0, "ed25519");
    let digest = {
        use sha2::{Digest, Sha256};
        Sha256::digest(&bytes)
    };
    assert_eq!(hash, bs58::encode(digest).into_string());
    let signature = ed25519_dalek::Signature::from_slice(&signed[bytes.len() + 1..]).unwrap();
    use ed25519_dalek::Verifier;
    key.verifying_key()
        .verify(&digest, &signature)
        .expect("signature over the hash");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_intent_and_the_signed_bytes_are_on_disk_before_the_broadcast() {
    let e = env(0).await;
    e.rpc.set_failure(Failure::Timeout);
    let tx = near_tx(&e);
    // The node answers nothing: the driver gives up, but the row is complete.
    let row = tx.submit(request(None)).await;
    assert!(row.is_err(), "the node never answered the nonce query");
    let intent = rows(&e, &tx.public_key()).await.remove(0);
    assert_eq!(intent.state, "intent");
    assert_eq!(
        intent.args,
        request(None).args,
        "the intent is written first"
    );
    assert!(intent.signed_bytes.is_none());

    e.rpc.set_failure(Failure::None);
    tx.resume().await;
    let done = rows(&e, &tx.public_key()).await.remove(0);
    assert_eq!(
        done.state, "final",
        "a restart finishes what the intent started"
    );
    assert_eq!(e.rpc.submissions().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn after_a_timeout_the_hash_is_asked_for_and_the_same_bytes_go_out_again() {
    let e = env(0).await;
    let tx = Arc::new(near_tx(&e));
    let row = tx.submit(request(None)).await.unwrap();
    let hash = row.hash.clone().unwrap();
    e.rpc.set_failure(Failure::Timeout);
    let driving = tokio::spawn({
        let tx = tx.clone();
        async move { tx.drive(row.id).await }
    });
    tokio::time::sleep(Duration::from_millis(900)).await;
    e.rpc.set_failure(Failure::None);
    let end = driving.await.unwrap().unwrap();
    assert!(end.success, "{end:?}");

    let requests = e.rpc.requests();
    assert!(
        requests
            .iter()
            .any(|r| r.method == "tx" && r.params["tx_hash"] == hash.as_str()),
        "the hash was queried after the timeout"
    );
    let submissions = e.rpc.submissions();
    assert_eq!(submissions.len(), 1, "the node saw it once");
    assert_eq!(submissions[0].tx_hash, hash);
    let stored = rows(&e, &tx.public_key()).await.remove(0);
    assert_eq!(
        base64::engine::general_purpose::STANDARD.encode(stored.signed_bytes.unwrap()),
        submissions[0].tx_base64,
        "unchanged bytes, the nonce allocated once"
    );
    assert_eq!(rows(&e, &tx.public_key()).await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_receipt_is_a_failed_transaction() {
    let e = env(0).await;
    let tx = near_tx(&e);
    let row = tx.submit(request(None)).await.unwrap();
    let hash = row.hash.clone().unwrap();
    let mut spec = TxStatusSpec::success(&hash, OPERATOR, VAULT);
    spec.receipts = vec![
        ReceiptSpec {
            executor_id: VAULT.into(),
            logs: vec![],
            status: OutcomeSpec::Success(String::new()),
        },
        ReceiptSpec {
            executor_id: "usdc.testnet".into(),
            logs: vec![],
            status: OutcomeSpec::Failure("ft_transfer panicked".into()),
        },
    ];
    e.rpc.set_tx_status(spec);
    let end = tx.drive(row.id).await.unwrap();
    assert!(!end.success);
    assert!(
        end.detail.contains("ft_transfer panicked"),
        "{}",
        end.detail
    );
    let stored = rows(&e, &tx.public_key()).await.remove(0);
    assert_eq!(stored.state, "failed");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_repeat_with_the_same_key_inside_ten_minutes_is_the_first_transaction() {
    let e = env(0).await;
    let tx = near_tx(&e);
    let first = tx.send(request(Some("0xabc"))).await.unwrap();
    e.clock.advance_secs(599);
    let again = tx.send(request(Some("0xabc"))).await.unwrap();
    assert_eq!(first.hash, again.hash);
    assert_eq!(e.rpc.submissions().len(), 1);
    let other = tx.send(request(Some("0xdef"))).await.unwrap();
    assert_ne!(first.hash, other.hash);
    e.clock.advance_secs(2);
    let later = tx.send(request(Some("0xabc"))).await.unwrap();
    assert_ne!(first.hash, later.hash, "a new send after the window");
}

#[tokio::test(flavor = "multi_thread")]
async fn nonces_grow_with_each_transaction_even_before_the_node_has_seen_the_last() {
    let e = env(5).await;
    let tx = near_tx(&e);
    let a = tx.submit(request(None)).await.unwrap();
    let b = tx.submit(request(None)).await.unwrap();
    assert_eq!((a.nonce, b.nonce), (Some(6), Some(7)));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_heartbeat_goes_out_as_a_transaction_and_a_failed_send_is_an_error() {
    let e = env(0).await;
    let tx = Arc::new(near_tx(&e));
    let heartbeat = NearHeartbeat(tx.clone());
    heartbeat.send().await.expect("sent");
    let submissions = e.rpc.submissions();
    assert_eq!(submissions.len(), 1);
    let row = rows(&e, &tx.public_key()).await.remove(0);
    assert_eq!(
        (row.kind.as_str(), row.method.as_str()),
        ("heartbeat", "heartbeat")
    );
    assert_eq!(row.args, b"{}");
    assert_eq!(row.state, "final");

    // A node that refuses everything: an error, and the next tick makes a new attempt.
    e.rpc.set_failure(Failure::Http(503));
    assert!(heartbeat.send().await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_node_that_errors_is_retried_and_the_same_row_finishes_later() {
    let e = env(0).await;
    let tx = near_tx(&e);
    let row = tx.submit(request(None)).await.unwrap();
    e.rpc.set_failure(Failure::RpcError {
        code: -32000,
        message: "Server error".into(),
    });
    // A transient error is retried until the attempts run out, then reported.
    assert!(tx.drive(row.id).await.is_err());
    e.rpc.set_failure(Failure::None);
    let end = tx.drive(row.id).await.unwrap();
    assert!(end.success, "the same row finishes once the node answers");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_close_that_fails_after_the_answer_raises_an_alarm() {
    let e = env(0).await;
    let tx = Arc::new(near_tx(&e));
    // The row exists first, so the sender's repeat key finds it and the status can be scripted.
    let row = tx.submit(request(Some("n1"))).await.unwrap();
    let mut spec = TxStatusSpec::success(row.hash.as_deref().unwrap(), OPERATOR, VAULT);
    spec.receipts = vec![ReceiptSpec {
        executor_id: VAULT.into(),
        logs: vec![],
        status: OutcomeSpec::Failure("vault refused".into()),
    }];
    e.rpc.set_tx_status(spec);
    let sink = Arc::new(FakeSink::default());
    let sender = NearCloseSender {
        tx,
        store: e.store.clone(),
        clock: e.clock.clone(),
        alarms: Arc::new(Alarms::new(vec![sink.clone()], e.clock.clone())),
    };
    let answer = sender.send("n1", b"AA==".to_vec(), vec![]).await;
    assert!(answer.is_err(), "{answer:?}");
    let sent = sink.sent();
    assert_eq!(sent.len(), 1, "{sent:?}");
    assert_eq!(sent[0].key, "close_failed:n1");
    assert!(sent[0].body.contains("vault refused"), "{}", sent[0].body);
}
