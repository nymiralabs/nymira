//! `RpcVault` against the fake NEAR RPC: the views the operator reads, as the vault spells them.

use nymira_fake_near_rpc::{ArgsMatch, FakeRpc, ViewResult};
use nymira_operator::chain::{RpcVault, VaultView};
use serde_json::json;

const VAULT: &str = "vault.nymira.testnet";

fn word(n: u8) -> String {
    format!("0x{}", hex::encode([n; 32]))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_views_parse_as_the_vault_spells_them() {
    let rpc = FakeRpc::start().await;
    let view = |method: &str, args: ArgsMatch, result: serde_json::Value| {
        rpc.set_view(VAULT, method, args, ViewResult::json(result));
    };
    view(
        "config",
        ArgsMatch::Any,
        json!({
            "operator_pk": {"x": word(1), "y": word(2)}, "clearance_pk": {"x": word(3), "y": word(4)},
            "operator_account": "operator.nymira.testnet", "token": "usdc.testnet",
            "treasury": "t.testnet", "owner": "o.testnet", "chain_id": "397",
            "denominations": ["1000000"], "note_fee": "10000", "close_fee": "10000",
            "bound_classes": ["100000", "500000"], "charge_step": "1000", "min_charge": "2000",
            "slot_rate": 1000, "tvl_cap": "100000000", "current_tvl": "0",
            "exit_delay_secs": 604800, "note_ttl": 86400
        }),
    );
    view("current_root", ArgsMatch::Any, json!(word(9)));
    view("retirement_epoch", ArgsMatch::Any, json!("3"));
    view("open_notes", ArgsMatch::Any, json!(7));
    view(
        "liquid_near",
        ArgsMatch::Any,
        json!("2500000000000000000000000"),
    );
    let exit = |id: u32| {
        json!({"note_id": id, "nullifier": word(id as u8), "next_nullifier": word(id as u8 + 50),
               "final_balance": "1", "conceded": "0", "deadline": "5"})
    };
    view(
        "exit_requests",
        ArgsMatch::json(json!({"from_note_id": 0, "limit": 100})),
        json!([exit(4), exit(8)]),
    );
    view(
        "note_records",
        ArgsMatch::json(json!({"from": 2, "limit": 1})),
        json!([{"note_id": 2, "deposit_amount": "1", "expiry": 1, "status": "Active", "leaf_retired": false}]),
    );
    view(
        "note_records",
        ArgsMatch::json(json!({"from": 3, "limit": 1})),
        json!([{"note_id": 3, "deposit_amount": "1", "expiry": 1, "status": "Closed", "leaf_retired": true}]),
    );

    let vault = RpcVault::new(rpc.url(), VAULT);
    let config = vault.config().await.unwrap();
    assert_eq!(config.chain_id, 397);
    assert_eq!(config.bound_classes, vec![100_000, 500_000]);
    assert_eq!(
        (config.charge_step, config.min_charge, config.slot_rate),
        (1_000, 2_000, 1_000)
    );
    assert_eq!(config.operator_pk.x, word(1));
    assert_eq!(vault.current_root().await.unwrap(), word(9));
    assert_eq!(vault.retirement_epoch().await.unwrap(), 3);
    assert_eq!(vault.open_notes().await.unwrap(), 7);
    assert_eq!(
        vault.liquid_near().await.unwrap(),
        2_500_000_000_000_000_000_000_000
    );
    let exits = vault.exits().await.unwrap();
    assert_eq!(
        exits.iter().map(|e| e.note_id).collect::<Vec<_>>(),
        vec![4, 8]
    );
    assert_eq!(exits[0].next_nullifier, word(54));
    assert_eq!(vault.open_among(&[2, 3]).await.unwrap(), 1);

    // The closer's two reads: a note's status word and whether a nullifier is spent.
    rpc.set_view(
        VAULT,
        "note_records",
        ArgsMatch::json(json!({"from": 77, "limit": 1})),
        ViewResult::json(json!([])),
    );
    rpc.set_view(
        VAULT,
        "is_nullifier_used",
        ArgsMatch::json(json!({"nullifier": word(5)})),
        ViewResult::json(json!(true)),
    );
    rpc.set_view(
        VAULT,
        "is_nullifier_used",
        ArgsMatch::json(json!({"nullifier": word(6)})),
        ViewResult::json(json!(false)),
    );
    assert_eq!(vault.note(2).await.unwrap().unwrap().status, "Active");
    assert_eq!(vault.note(3).await.unwrap().unwrap().status, "Closed");
    assert!(vault.note(77).await.unwrap().is_none());
    assert!(vault.nullifier_used(&word(5)).await.unwrap());
    assert!(!vault.nullifier_used(&word(6)).await.unwrap());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_view_is_an_error() {
    let rpc = FakeRpc::start().await;
    rpc.set_view(
        VAULT,
        "current_root",
        ArgsMatch::Any,
        ViewResult::error("contract panicked"),
    );
    let vault = RpcVault::new(rpc.url(), VAULT);
    assert!(vault.current_root().await.is_err());
    assert!(RpcVault::new("http://127.0.0.1:1", VAULT)
        .current_root()
        .await
        .is_err());
}
