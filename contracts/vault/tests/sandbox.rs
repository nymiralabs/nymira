//! The vault deployed on a NEAR sandbox with the mock USDC token: deposits, `sync_root`, gas.
//! Unit tests for every refusal and boundary are in `src/tests.rs`.

use near_workspaces::network::Sandbox;
use near_workspaces::result::ExecutionFinalResult;
use near_workspaces::types::NearToken;
use near_workspaces::{Account, AccountId, Contract, Worker};
use nymira_testkit::build::contract_wasm_with_features;
use nymira_testkit::clock::{advance_to, now_seconds};
use nymira_testkit::mock_usdc::MockUsdc;
use nymira_testkit::proofs::{canonical_verifying_key, signing_key, Kind};
use serde_json::{json, Value};
use zkapi_core::{Felt252, MerkleTree};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

const ONE: u128 = 1_000_000;
const NOTE_FEE: u128 = 50_000;
const TGAS: u64 = 1_000_000_000_000;

/// `NYMIRA_VAULT_WASM` points a test at a prebuilt wasm (the reproducible build).
fn vault_wasm(features: &[&str]) -> Vec<u8> {
    if let Ok(path) = std::env::var("NYMIRA_VAULT_WASM") {
        return std::fs::read(path).expect("NYMIRA_VAULT_WASM is readable");
    }
    contract_wasm_with_features("contracts/vault", features)
}

fn point(seed: u64) -> Value {
    let key = signing_key(seed);
    json!({ "x": format!("0x{}", hex::encode(key.x.0)), "y": format!("0x{}", hex::encode(key.y.0)) })
}

fn b64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, b)| n | u32::from(*b) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(TABLE[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn new_args(token: &AccountId, tvl_cap: u128) -> Value {
    json!({
        "operator_pk": point(1),
        "clearance_pk": point(2),
        "token": token,
        "denominations": ["1000000", "5000000"],
        "tvl_cap": tvl_cap.to_string(),
        "request_vk": b64(&canonical_verifying_key(Kind::Request)),
        "withdrawal_vk": b64(&canonical_verifying_key(Kind::Withdrawal)),
        "treasury": "treasury.test",
        "note_fee": NOTE_FEE.to_string(),
        "exit_delay_secs": 7 * 86_400,
        "note_ttl": 30 * 86_400,
        "bound_classes": ["100000", "500000"],
        "charge_step": "1000",
        "min_charge": "2000",
        "slot_rate": 1000,
        "owner": "owner.test",
        "operator_account": "operator.test",
    })
}

fn report(label: &str, outcome: &ExecutionFinalResult) {
    println!(
        "gas {label}: {:.2} Tgas burnt in all receipts",
        outcome.total_gas_burnt.as_gas() as f64 / TGAS as f64
    );
}

/// Gas burnt by the receipts that ran on `account` (the vault).
fn gas_on(outcome: &ExecutionFinalResult, account: &AccountId) -> u64 {
    outcome
        .receipt_outcomes()
        .iter()
        .filter(|receipt| &receipt.executor_id == account)
        .map(|receipt| receipt.gas_burnt.as_gas())
        .sum()
}

struct Env {
    worker: Worker<Sandbox>,
    usdc: MockUsdc,
    vault: Contract,
    alice: Account,
}

async fn setup(tvl_cap: u128) -> Result<Env> {
    let worker = near_workspaces::sandbox().await?;
    let usdc = MockUsdc::deploy(&worker).await?;
    let vault = worker.dev_deploy(&vault_wasm(&["dev-setup"])).await?;
    let init = vault
        .call("new")
        .args_json(new_args(usdc.contract.id(), tvl_cap))
        .max_gas()
        .transact()
        .await?;
    report("new", &init);
    init.into_result()?;
    usdc.register_id(vault.id()).await;
    let alice = worker.dev_create_account().await?;
    usdc.register(&alice).await;
    usdc.mint(alice.id(), 1_000_000 * ONE).await;
    Ok(Env {
        worker,
        usdc,
        vault,
        alice,
    })
}

fn commitment(n: u64) -> String {
    format!("0x{}", hex::encode(Felt252::from_u64(n).0))
}

async fn transfer_call(
    usdc: &Contract,
    from: &Account,
    to: &AccountId,
    amount: u128,
    msg: &str,
) -> Result<ExecutionFinalResult> {
    Ok(from
        .call(usdc.id(), "ft_transfer_call")
        .args_json(json!({ "receiver_id": to, "amount": amount.to_string(), "msg": msg }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await?)
}

fn deposit_msg(n: u64) -> String {
    json!({ "commitment": commitment(n) }).to_string()
}

async fn view<T: near_sdk::serde::de::DeserializeOwned>(
    vault: &Contract,
    method: &str,
    args: Value,
) -> T {
    vault
        .view(method)
        .args_json(args)
        .await
        .unwrap()
        .json()
        .unwrap()
}

async fn sync_root(env: &Env, max: u32) -> Result<ExecutionFinalResult> {
    Ok(env
        .alice
        .call(env.vault.id(), "sync_root")
        .args_json(json!({ "max": max }))
        .max_gas()
        .transact()
        .await?)
}

#[tokio::test]
async fn a_deposit_through_the_token_creates_a_note() -> Result<()> {
    let env = setup(100 * ONE).await?;
    let before = env.usdc.balance_of(env.alice.id()).await;
    let outcome = transfer_call(
        &env.usdc.contract,
        &env.alice,
        env.vault.id(),
        ONE + NOTE_FEE,
        &deposit_msg(7),
    )
    .await?;
    report("ft_transfer_call deposit", &outcome);
    let vault_gas = gas_on(&outcome, env.vault.id());
    println!(
        "gas ft_on_transfer: {:.2} Tgas",
        vault_gas as f64 / TGAS as f64
    );
    assert!(vault_gas < 10 * TGAS, "ft_on_transfer burnt {vault_gas}");
    outcome.into_result()?;

    assert_eq!(
        env.usdc.balance_of(env.alice.id()).await,
        before - (ONE + NOTE_FEE)
    );
    assert_eq!(env.usdc.balance_of(env.vault.id()).await, ONE + NOTE_FEE);
    let records: Vec<Value> =
        view(&env.vault, "note_records", json!({"from": 0, "limit": 10})).await;
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["deposit_amount"], "1000000");
    assert_eq!(records[0]["status"], "Active");
    let status: Value = view(
        &env.vault,
        "note_status",
        json!({"commitment": commitment(7)}),
    )
    .await;
    assert_eq!(status, "Active");
    let treasury: String = view(&env.vault, "treasury_balance", json!({})).await;
    assert_eq!(treasury, NOTE_FEE.to_string());
    let config: Value = view(&env.vault, "config", json!({})).await;
    assert_eq!(config["current_tvl"], (ONE + NOTE_FEE).to_string());
    let expiry = records[0]["expiry"].as_u64().unwrap();
    assert_eq!((expiry + 1) % 86_400, 0, "expiry is the end of a UTC day");
    Ok(())
}

#[tokio::test]
async fn refused_deposits_come_back_to_the_sender() -> Result<()> {
    let env = setup(100 * ONE).await?;
    let start = env.usdc.balance_of(env.alice.id()).await;
    for (amount, msg) in [
        (ONE, deposit_msg(1)),
        (ONE + NOTE_FEE, "not json".to_owned()),
        (3 * ONE + NOTE_FEE, deposit_msg(2)),
    ] {
        let outcome =
            transfer_call(&env.usdc.contract, &env.alice, env.vault.id(), amount, &msg).await?;
        report("ft_transfer_call refused", &outcome);
        outcome.into_result()?;
        assert_eq!(env.usdc.balance_of(env.alice.id()).await, start);
        assert_eq!(env.usdc.balance_of(env.vault.id()).await, 0);
    }
    let count: u32 = view(&env.vault, "open_notes", json!({})).await;
    assert_eq!(count, 0);
    Ok(())
}

#[tokio::test]
async fn a_fake_token_creates_no_note() -> Result<()> {
    let env = setup(100 * ONE).await?;
    let fake = MockUsdc::deploy(&env.worker).await?;
    fake.register_id(env.vault.id()).await;
    fake.register(&env.alice).await;
    fake.mint(env.alice.id(), 10 * ONE).await;
    let outcome = transfer_call(
        &fake.contract,
        &env.alice,
        env.vault.id(),
        ONE + NOTE_FEE,
        &deposit_msg(1),
    )
    .await?;
    report("fake token deposit", &outcome);
    outcome.into_result()?;
    assert_eq!(fake.balance_of(env.alice.id()).await, 10 * ONE);
    let count: u32 = view(&env.vault, "open_notes", json!({})).await;
    assert_eq!(count, 0);
    Ok(())
}

#[tokio::test]
async fn sync_root_follows_the_thirty_second_rule() -> Result<()> {
    let env = setup(100 * ONE).await?;
    let started = now_seconds(&env.worker).await;
    transfer_call(
        &env.usdc.contract,
        &env.alice,
        env.vault.id(),
        ONE + NOTE_FEE,
        &deposit_msg(1),
    )
    .await?
    .into_result()?;
    let early = sync_root(&env, 120).await?;
    report("sync_root too early", &early);
    assert_eq!(early.into_result()?.json::<u32>()?, 0);
    let leaves: u32 = view(&env.vault, "leaf_count", json!({})).await;
    assert_eq!(leaves, 0);

    advance_to(&env.worker, started + 33).await?;
    let sync = sync_root(&env, 120).await?;
    report("sync_root, one leaf", &sync);
    assert_eq!(sync.into_result()?.json::<u32>()?, 1);
    let leaves: u32 = view(&env.vault, "leaf_count", json!({})).await;
    assert_eq!(leaves, 1);
    Ok(())
}

async fn all_leaves(vault: &Contract) -> Vec<Felt252> {
    let mut out = Vec::new();
    loop {
        let page: Vec<String> = view(
            vault,
            "get_leaves",
            json!({"from": out.len(), "limit": 100}),
        )
        .await;
        if page.is_empty() {
            return out;
        }
        for leaf in page {
            let bytes: [u8; 32] = hex::decode(leaf.trim_start_matches("0x"))
                .unwrap()
                .try_into()
                .unwrap();
            out.push(Felt252(bytes));
        }
    }
}

/// Gas bound of one call: 300 Tgas with the 1.2 margin the spec asks for.
const SYNC_GAS_BOUND: u64 = 250 * TGAS;

#[tokio::test]
async fn full_sync_root_batches_stay_in_gas_and_match_a_client_rebuild() -> Result<()> {
    let env = setup(1_000 * ONE).await?;
    let mut next = 1;
    // 80 is the cap; two batches, the second crossing index 127 (a seven-hash append).
    for batch in 0..2u64 {
        for _ in 0..80 {
            transfer_call(
                &env.usdc.contract,
                &env.alice,
                env.vault.id(),
                ONE + NOTE_FEE,
                &deposit_msg(next),
            )
            .await?
            .into_result()?;
            next += 1;
        }
        let sync = sync_root(&env, 120).await?;
        let burnt = sync.total_gas_burnt.as_gas();
        report(
            &format!("sync_root, batch {batch} of 80 (asked for 120)"),
            &sync,
        );
        assert!(burnt < SYNC_GAS_BOUND, "an 80-leaf batch burnt {burnt}");
        let logs = sync.logs();
        assert_eq!(logs.len(), 1, "one event per batch");
        assert!(logs[0].len() < 16 * 1024);
        assert_eq!(sync.into_result()?.json::<u32>()?, 80, "capped at 80");

        let leaves = all_leaves(&env.vault).await;
        assert_eq!(leaves.len() as u64, 80 * (batch + 1));
        let root: String = view(&env.vault, "current_root", json!({})).await;
        assert_eq!(
            format!(
                "0x{}",
                hex::encode(MerkleTree::from_leaves(&leaves).root().0)
            ),
            root
        );
        let known: bool = view(&env.vault, "is_known_root", json!({"root": root})).await;
        assert!(known);
    }
    Ok(())
}

#[tokio::test]
async fn liquid_near_is_balance_minus_locked_storage() -> Result<()> {
    let env = setup(100 * ONE).await?;
    let account = env.vault.view_account().await?;
    let liquid: String = view(&env.vault, "liquid_near", json!({})).await;
    let locked = u128::from(account.storage_usage) * 10u128.pow(19);
    assert_eq!(
        liquid,
        (account.balance.as_yoctonear() - locked).to_string()
    );
    Ok(())
}

#[tokio::test]
async fn a_build_without_dev_setup_refuses_the_development_keys() -> Result<()> {
    let worker = near_workspaces::sandbox().await?;
    let usdc = MockUsdc::deploy(&worker).await?;
    let vault = worker.dev_deploy(&vault_wasm(&[])).await?;
    let init = vault
        .call("new")
        .args_json(new_args(usdc.contract.id(), 100 * ONE))
        .max_gas()
        .transact()
        .await?;
    report(
        "new with development keys on a ceremony-pinned build",
        &init,
    );
    let failure = init.into_result().expect_err("new must panic");
    assert!(
        failure
            .to_string()
            .contains("does not match the compiled hash"),
        "{failure}"
    );
    Ok(())
}
