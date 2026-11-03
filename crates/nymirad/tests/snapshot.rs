//! The snapshot against a real sandbox vault with deposits.

use near_workspaces::{Account, Contract};
use nymira_testkit::mock_usdc::MockUsdc;
use nymira_testkit::vault::DevVault;
use nymirad::rpc::{Rpc, Transport};
use nymirad::snapshot;
use serde_json::json;

const ONE: u128 = 1_000_000;
const FEE: u128 = 50_000;

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
    let vault = DevVault {
        note_ttl_days: 30,
        bound_classes: &["100000"],
    }
    .deploy(&worker, &usdc, &operator, &treasury)
    .await;
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
