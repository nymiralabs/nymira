//! The snapshot against a real sandbox vault with deposits.

use near_workspaces::types::NearToken;
use near_workspaces::{Account, Contract};
use nymira_testkit::build::contract_wasm_with_features;
use nymira_testkit::mock_usdc::MockUsdc;
use nymira_testkit::proofs::{canonical_verifying_key, signing_key, Kind};
use nymirad::rpc::{Rpc, Transport};
use nymirad::snapshot;
use serde_json::{json, Value};

const ONE: u128 = 1_000_000;
const FEE: u128 = 50_000;

fn point(seed: u64) -> Value {
    let key = signing_key(seed);
    json!({ "x": format!("0x{}", hex::encode(key.x.0)), "y": format!("0x{}", hex::encode(key.y.0)) })
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

async fn vault_with_notes(
    count: u64,
) -> (
    near_workspaces::Worker<near_workspaces::network::Sandbox>,
    Contract,
) {
    let worker = near_workspaces::sandbox().await.unwrap();
    let usdc = MockUsdc::deploy(&worker).await.unwrap();
    let operator = worker.dev_create_account().await.unwrap();
    let treasury = worker.dev_create_account().await.unwrap();
    let alice = worker.dev_create_account().await.unwrap();
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
            "denominations": [ONE.to_string()], "tvl_cap": "100000000",
            "request_vk": b64(&canonical_verifying_key(Kind::Request)),
            "withdrawal_vk": b64(&canonical_verifying_key(Kind::Withdrawal)),
            "treasury": treasury.id(), "note_fee": FEE.to_string(),
            "exit_delay_secs": 7 * 86_400, "note_ttl": 30 * 86_400,
            "bound_classes": ["100000"], "charge_step": "1000", "min_charge": "2000",
            "slot_rate": 1000, "owner": worker.root_account().unwrap().id(),
            "operator_account": operator.id(),
        }))
        .max_gas()
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    usdc.register_id(vault.id()).await;
    usdc.register(&treasury).await;
    usdc.register(&alice).await;
    usdc.mint(alice.id(), 10 * ONE).await;
    for n in 0..count {
        let msg = json!({ "commitment": format!("0x{:064x}", 1000 + n) }).to_string();
        usdc.ft_transfer_call(&alice, vault.id(), ONE + FEE, &msg)
            .await
            .unwrap();
    }
    if count > 0 {
        // sync_root waits 30 s after the last insert unless 16 leaves are waiting.
        // The sandbox keeps producing blocks, so a target can already be behind: ask again.
        for _ in 0..5 {
            let at = nymira_testkit::clock::now_seconds(&worker).await + 40;
            if nymira_testkit::clock::advance_to(&worker, at).await.is_ok() {
                break;
            }
        }
        sync(&alice, &vault).await;
    }
    (worker, vault)
}

async fn sync(caller: &Account, vault: &Contract) {
    caller
        .call(vault.id(), "sync_root")
        .args_json(json!({ "max": 100 }))
        .max_gas()
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
}

#[tokio::test]
async fn the_tree_from_get_leaves_matches_the_vaults_root() {
    let (worker, vault) = vault_with_notes(3).await;
    worker.fast_forward(5).await.unwrap();
    let rpc = Rpc::new(worker.rpc_addr(), &Transport::Direct).unwrap();
    let snap = snapshot::take(&rpc, vault.id().as_str()).await.unwrap();
    let open: u32 = vault.view("open_notes").await.unwrap().json().unwrap();
    assert_eq!(open, 3, "deposits were accepted");
    assert_eq!(snap.leaf_count, 3);
    let on_chain: String = vault.view("current_root").await.unwrap().json().unwrap();
    assert_eq!(format!("0x{}", hex::encode(snap.root.0)), on_chain);
    assert_eq!(snap.tree.root(), snap.root);
    assert_eq!(snap.config["bound_classes"], json!(["100000"]));
    assert!(snap.block_height > 0);
}

#[tokio::test]
async fn an_empty_vault_gives_an_empty_snapshot() {
    let (worker, vault) = vault_with_notes(0).await;
    worker.fast_forward(5).await.unwrap();
    let rpc = Rpc::new(worker.rpc_addr(), &Transport::Direct).unwrap();
    let snap = snapshot::take(&rpc, vault.id().as_str()).await.unwrap();
    assert_eq!(snap.leaf_count, 0);
    assert_eq!(snap.tree.root(), snap.root);
}
