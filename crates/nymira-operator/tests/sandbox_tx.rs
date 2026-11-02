//! The operator's own transactions against a real NEAR sandbox node and the real vault: the
//! Borsh bytes, the function-call key's limits and the receipts of a failed call.

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use near_workspaces::types::{AccessKey, NearToken, PublicKey};
use nymira_operator::clock::ManualClock;
use nymira_operator::signer::HeartbeatSender;
use nymira_operator::store::Store;
use nymira_operator::tx::{NearHeartbeat, NearTx, TxConfig, TxRequest, HEARTBEAT_GAS};
use nymira_testkit::build::contract_wasm_with_features;
use nymira_testkit::mock_usdc::MockUsdc;
use nymira_testkit::proofs::{canonical_verifying_key, signing_key, Kind};
use serde_json::{json, Value};

fn point(seed: u64) -> Value {
    let key = signing_key(seed);
    json!({ "x": format!("0x{}", hex::encode(key.x.0)), "y": format!("0x{}", hex::encode(key.y.0)) })
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_heartbeat_and_a_failing_close_run_on_a_real_node_with_a_function_call_key() {
    let worker = near_workspaces::sandbox().await.unwrap();
    let usdc = MockUsdc::deploy(&worker).await.unwrap();
    let operator = worker.dev_create_account().await.unwrap();
    let treasury = worker.dev_create_account().await.unwrap();
    let vault = worker
        .root_account()
        .unwrap()
        .create_subaccount("vault")
        .initial_balance(NearToken::from_near(50))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap()
        .deploy(&contract_wasm_with_features(
            "contracts/vault",
            &["dev-setup"],
        ))
        .await
        .unwrap()
        .into_result()
        .unwrap();
    vault
        .call("new")
        .args_json(json!({
            "operator_pk": point(1), "clearance_pk": point(2), "token": usdc.contract.id(),
            "denominations": ["1000000"], "tvl_cap": "100000000",
            "request_vk": b64(&canonical_verifying_key(Kind::Request)),
            "withdrawal_vk": b64(&canonical_verifying_key(Kind::Withdrawal)),
            "treasury": treasury.id(), "note_fee": "50000", "exit_delay_secs": 7 * 86_400,
            "note_ttl": 30 * 86_400, "bound_classes": ["100000"], "charge_step": "1000",
            "min_charge": "2000", "slot_rate": 1000, "owner": "owner.test",
            "operator_account": operator.id(),
        }))
        .max_gas()
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();

    // A function-call key bound to the vault, with the two methods listed.
    let key = SigningKey::from_bytes(&[11u8; 32]);
    let public: PublicKey = format!(
        "ed25519:{}",
        bs58::encode(key.verifying_key().as_bytes()).into_string()
    )
    .parse()
    .unwrap();
    operator
        .batch(operator.id())
        .add_key(
            public,
            AccessKey::function_call_access(vault.id(), &["heartbeat", "mutual_close"], None),
        )
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();

    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(&dir.path().join("db")).unwrap();
    let tx = Arc::new(
        NearTx::new(
            worker.rpc_addr(),
            operator.id().as_str(),
            vault.id().as_str(),
            key.clone(),
            store.clone(),
            Arc::new(ManualClock::new(1_900_000_000_000)),
        )
        .with_config(TxConfig {
            send_timeout: Duration::from_secs(60),
            retry_delay: Duration::from_millis(200),
            max_attempts: 5,
        }),
    );

    // The heartbeat: accepted by the node and by the vault (only the operator account may call).
    // A final block that does not hold the new key yet is a refusal to sign, not a guess.
    let mut landed = Err(String::new());
    for _ in 0..40 {
        landed = NearHeartbeat(tx.clone()).send().await;
        if landed.is_ok() {
            break;
        }
        assert!(
            landed.as_ref().unwrap_err().contains("not readable"),
            "{landed:?}"
        );
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    landed.expect("heartbeat lands");
    NearHeartbeat(tx.clone())
        .send()
        .await
        .expect("and again, with the next nonce");

    // A close with a proof that is not one: the node takes the transaction, the vault refuses it,
    // and the failed receipt is read.
    let failed = tx
        .send(TxRequest {
            kind: "close",
            method: "mutual_close",
            args: json!({ "proof": b64(&[0u8; 256]), "inputs": Vec::<String>::new() })
                .to_string()
                .into_bytes(),
            gas: 100_000_000_000_000,
            idem_key: Some("0x1".into()),
        })
        .await
        .unwrap();
    assert!(!failed.success, "{failed:?}");
    assert!(
        failed.detail.contains("wrong number of public inputs"),
        "the vault's own panic message: {}",
        failed.detail
    );

    // A method the key does not list is refused by the node before anything runs.
    let refused = tx
        .send(TxRequest {
            kind: "heartbeat",
            method: "sync_root",
            args: b"{}".to_vec(),
            gas: HEARTBEAT_GAS,
            idem_key: None,
        })
        .await
        .unwrap();
    assert!(!refused.success, "{refused:?}");
    assert!(
        refused.detail.contains("MethodNameMismatch"),
        "{}",
        refused.detail
    );
}
