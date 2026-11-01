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

/// `NYMIRA_VAULT_WASM` points a test at a prebuilt wasm (the reproducible build) for the plain
/// `dev-setup` build; a build with other features is always compiled.
fn vault_wasm(features: &[&str]) -> Vec<u8> {
    if let (Ok(path), ["dev-setup"]) = (std::env::var("NYMIRA_VAULT_WASM"), features) {
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

fn new_args(token: &AccountId, tvl_cap: u128, operator: &str, treasury: &str) -> Value {
    json!({
        "operator_pk": point(1),
        "clearance_pk": point(2),
        "token": token,
        "denominations": ["1000000", "5000000"],
        "tvl_cap": tvl_cap.to_string(),
        "request_vk": b64(&canonical_verifying_key(Kind::Request)),
        "withdrawal_vk": b64(&canonical_verifying_key(Kind::Withdrawal)),
        "treasury": treasury,
        "note_fee": NOTE_FEE.to_string(),
        "exit_delay_secs": 7 * 86_400,
        "note_ttl": 30 * 86_400,
        "bound_classes": ["100000", "500000"],
        "charge_step": "1000",
        "min_charge": "2000",
        "slot_rate": 1000,
        "owner": "owner.test",
        "operator_account": operator,
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
    operator: Account,
    treasury: Account,
}

async fn setup(tvl_cap: u128) -> Result<Env> {
    setup_with(tvl_cap, &["dev-setup"]).await
}

async fn setup_with(tvl_cap: u128, features: &[&str]) -> Result<Env> {
    let worker = near_workspaces::sandbox().await?;
    let usdc = MockUsdc::deploy(&worker).await?;
    let operator = worker.dev_create_account().await?;
    let treasury = worker.dev_create_account().await?;
    // A fixed account id, so proofs made for it can be cached across tests.
    let vault = worker
        .root_account()?
        .create_subaccount("vault")
        .initial_balance(NearToken::from_near(50))
        .transact()
        .await?
        .into_result()?
        .deploy(&vault_wasm(features))
        .await?
        .into_result()?;
    let init = vault
        .call("new")
        .args_json(new_args(
            usdc.contract.id(),
            tvl_cap,
            operator.id().as_str(),
            treasury.id().as_str(),
        ))
        .max_gas()
        .transact()
        .await?;
    report("new", &init);
    init.into_result()?;
    usdc.register_id(vault.id()).await;
    usdc.register(&treasury).await;
    let alice = worker.dev_create_account().await?;
    usdc.register(&alice).await;
    usdc.mint(alice.id(), 1_000_000 * ONE).await;
    Ok(Env {
        worker,
        usdc,
        vault,
        alice,
        operator,
        treasury,
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
        .args_json(new_args(
            usdc.contract.id(),
            100 * ONE,
            "operator.test",
            "treasury.test",
        ))
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

// Mutual close, payouts, claims

mod close {
    use std::sync::OnceLock;

    use near_workspaces::types::Gas;
    use nymira_near_codec::Network;
    use nymira_testkit::proofs::{Bundle, ProofGen, WithdrawalKindId};
    use zkapi_proof::dev::{DevNote, DevRequest, DevState};

    use super::*;

    const CHARGE: u128 = 20_000;
    const CLOSE_FEE: u128 = 30_000;

    fn gen(vault: &AccountId) -> &'static ProofGen {
        static GEN: OnceLock<ProofGen> = OnceLock::new();
        GEN.get_or_init(|| ProofGen::new(Network::Testnet, vault.as_str()).unwrap())
    }

    /// Alice deposits 1 USDC for a note whose registration is `secret`'s; returns the note the
    /// vault stored (its id and expiry).
    async fn deposit_note(env: &Env, secret: u64) -> Result<DevNote> {
        let before: Vec<Value> =
            view(&env.vault, "note_records", json!({"from": 0, "limit": 100})).await;
        let note = DevNote {
            secret: Felt252::from_u64(secret),
            note_id: before.len() as u32,
            deposit_amount: ONE,
            expiry: 0,
        };
        let msg = json!({ "commitment": format!("0x{}", hex::encode(note.registration().0)) });
        transfer_call(
            &env.usdc.contract,
            &env.alice,
            env.vault.id(),
            ONE + NOTE_FEE,
            &msg.to_string(),
        )
        .await?
        .into_result()?;
        let after: Vec<Value> =
            view(&env.vault, "note_records", json!({"from": 0, "limit": 100})).await;
        let expiry = after[note.note_id as usize]["expiry"].as_u64().unwrap();
        Ok(DevNote { expiry, ..note })
    }

    fn state_after_spending(env: &Env, note: &DevNote) -> DevState {
        let s0 = note.genesis(Felt252::from_u64(7));
        gen(env.vault.id())
            .next_state(
                &s0,
                &DevRequest {
                    request_time: 2_000_000_000,
                    solvency_bound: 100_000,
                    request_context: Felt252::from_u64(99),
                    rerandomization: Felt252::from_u64(9),
                },
                CHARGE,
                &Felt252::from_u64(17),
            )
            .unwrap()
    }

    async fn close_bundle(env: &Env, note: &DevNote, payout: &str) -> Result<Bundle> {
        let valid_until = now_seconds(&env.worker).await + 3_600;
        Ok(gen(env.vault.id()).withdrawal(
            &state_after_spending(env, note),
            WithdrawalKindId::MutualClose,
            payout,
            0,
            valid_until,
        )?)
    }

    async fn mutual_close(
        env: &Env,
        caller: &Account,
        bundle: &Bundle,
        gas: Gas,
        extra: Value,
    ) -> Result<ExecutionFinalResult> {
        let mut args = json!({
            "proof": bundle.proof_base64(),
            "inputs": bundle.inputs_base64(),
        });
        if let (Some(args), Some(extra)) = (args.as_object_mut(), extra.as_object()) {
            args.extend(extra.clone());
        }
        Ok(caller
            .call(env.vault.id(), "mutual_close")
            .args_json(args)
            .gas(gas)
            .transact()
            .await?)
    }

    fn tgas(n: u64) -> Gas {
        Gas::from_tgas(n)
    }

    async fn pending(env: &Env, account: &AccountId) -> String {
        view(
            &env.vault,
            "pending_payout",
            json!({ "account_id": account }),
        )
        .await
    }

    async fn settled(env: &Env) {
        env.worker.fast_forward(5).await.unwrap();
    }

    #[tokio::test]
    async fn a_user_close_pays_the_payout_account_and_ignores_a_payout_argument() -> Result<()> {
        let env = setup(100 * ONE).await?;
        let mallory = env.worker.dev_create_account().await?;
        env.usdc.register(&mallory).await;
        let note = deposit_note(&env, 42).await?;
        let bundle = close_bundle(&env, &note, env.alice.id().as_str()).await?;
        let before = env.usdc.balance_of(env.alice.id()).await;
        // A payout account passed as an argument changes nothing.
        let outcome = mutual_close(
            &env,
            &env.alice,
            &bundle,
            tgas(300),
            json!({ "withdraw_to": mallory.id(), "payout_account": mallory.id() }),
        )
        .await?;
        report("mutual_close, user, 300 Tgas", &outcome);
        println!(
            "gas mutual_close on the vault: {:.2} Tgas",
            gas_on(&outcome, env.vault.id()) as f64 / TGAS as f64
        );
        let logs = outcome.logs().join("\n");
        outcome.into_result()?;
        settled(&env).await;
        assert!(logs.contains(r#""event":"mutual_close""#));
        assert_eq!(
            env.usdc.balance_of(env.alice.id()).await,
            before + ONE - CHARGE
        );
        assert_eq!(env.usdc.balance_of(mallory.id()).await, 0);
        assert_eq!(pending(&env, env.alice.id()).await, "0");
        let treasury: String = view(&env.vault, "treasury_balance", json!({})).await;
        assert_eq!(treasury, (NOTE_FEE + CHARGE).to_string());
        let status: Value = view(&env.vault, "note_records", json!({"from": 0, "limit": 1})).await;
        assert_eq!(status[0]["status"], "Closed");
        let used: bool = view(
            &env.vault,
            "is_nullifier_used",
            json!({ "nullifier": format!("0x{}", hex::encode(bundle.inputs_wire[11])) }),
        )
        .await;
        assert!(used);
        // The vault holds what the vault owes: the treasury balance.
        assert_eq!(env.usdc.balance_of(env.vault.id()).await, NOTE_FEE + CHARGE);
        Ok(())
    }

    #[tokio::test]
    async fn the_operator_close_takes_the_close_fee() -> Result<()> {
        let env = setup(100 * ONE).await?;
        let note = deposit_note(&env, 42).await?;
        let bundle = close_bundle(&env, &note, env.alice.id().as_str()).await?;
        let before = env.usdc.balance_of(env.alice.id()).await;
        let outcome = mutual_close(&env, &env.operator, &bundle, tgas(300), json!({})).await?;
        report("mutual_close, operator", &outcome);
        outcome.into_result()?;
        settled(&env).await;
        assert_eq!(
            env.usdc.balance_of(env.alice.id()).await,
            before + ONE - CHARGE - CLOSE_FEE
        );
        let treasury: String = view(&env.vault, "treasury_balance", json!({})).await;
        assert_eq!(treasury, (NOTE_FEE + CHARGE + CLOSE_FEE).to_string());
        Ok(())
    }

    #[tokio::test]
    async fn a_payout_to_an_unregistered_account_registers_it_first() -> Result<()> {
        let env = setup(100 * ONE).await?;
        let fresh = env.worker.dev_create_account().await?;
        assert!(!env.usdc.is_registered(fresh.id()).await);
        // The payout account is the one the deposit names.
        let note = {
            let before: Vec<Value> =
                view(&env.vault, "note_records", json!({"from": 0, "limit": 100})).await;
            let n = DevNote {
                secret: Felt252::from_u64(77),
                note_id: before.len() as u32,
                deposit_amount: ONE,
                expiry: 0,
            };
            let msg = json!({
                "commitment": format!("0x{}", hex::encode(n.registration().0)),
                "withdraw_to": fresh.id(),
            });
            transfer_call(
                &env.usdc.contract,
                &env.alice,
                env.vault.id(),
                ONE + NOTE_FEE,
                &msg.to_string(),
            )
            .await?
            .into_result()?;
            let after: Vec<Value> =
                view(&env.vault, "note_records", json!({"from": 0, "limit": 100})).await;
            DevNote {
                expiry: after[n.note_id as usize]["expiry"].as_u64().unwrap(),
                ..n
            }
        };
        let bundle = close_bundle(&env, &note, fresh.id().as_str()).await?;
        let outcome = mutual_close(&env, &env.alice, &bundle, tgas(300), json!({})).await?;
        report("mutual_close to an unregistered account", &outcome);
        outcome.into_result()?;
        settled(&env).await;
        assert!(env.usdc.is_registered(fresh.id()).await);
        assert_eq!(env.usdc.balance_of(fresh.id()).await, ONE - CHARGE);
        Ok(())
    }

    #[tokio::test]
    async fn the_registration_deposit_comes_back_when_the_account_is_already_registered(
    ) -> Result<()> {
        let env = setup(100 * ONE).await?;
        let note = deposit_note(&env, 42).await?;
        let bundle = close_bundle(&env, &note, env.alice.id().as_str()).await?;
        assert!(env.usdc.is_registered(env.alice.id()).await);
        settled(&env).await;
        let before = env.vault.view_account().await?;
        mutual_close(&env, &env.alice, &bundle, tgas(300), json!({}))
            .await?
            .into_result()?;
        settled(&env).await;
        let after = env.vault.view_account().await?;
        // The vault may even gain (it earns a share of the gas it burns), but the 0.00125 NEAR
        // sent with `storage_deposit` must not be lost.
        assert!(
            after.balance.as_yoctonear() + 10u128.pow(20) >= before.balance.as_yoctonear(),
            "{} -> {}",
            before.balance.as_yoctonear(),
            after.balance.as_yoctonear()
        );
        Ok(())
    }

    #[tokio::test]
    async fn junk_bytes_from_a_successful_transfer_pay_once_and_create_no_pending_payout(
    ) -> Result<()> {
        let env = setup(100 * ONE).await?;
        env.usdc.set_junk_return(true).await;
        let note = deposit_note(&env, 42).await?;
        let bundle = close_bundle(&env, &note, env.alice.id().as_str()).await?;
        let before = env.usdc.balance_of(env.alice.id()).await;
        let outcome = mutual_close(&env, &env.alice, &bundle, tgas(300), json!({})).await?;
        report("mutual_close, token returns junk", &outcome);
        outcome.into_result()?;
        settled(&env).await;
        assert_eq!(
            env.usdc.balance_of(env.alice.id()).await,
            before + ONE - CHARGE
        );
        assert_eq!(pending(&env, env.alice.id()).await, "0");
        Ok(())
    }

    #[tokio::test]
    async fn a_failed_transfer_creates_one_pending_payout_and_anyone_can_claim_it() -> Result<()> {
        let env = setup(100 * ONE).await?;
        let note = deposit_note(&env, 42).await?;
        let bundle = close_bundle(&env, &note, env.alice.id().as_str()).await?;
        env.usdc.set_panic_in_transfer(true).await;
        let before = env.usdc.balance_of(env.alice.id()).await;
        let outcome = mutual_close(&env, &env.alice, &bundle, tgas(300), json!({})).await?;
        report("mutual_close, transfer fails", &outcome);
        let logs = outcome.logs().join("\n");
        outcome.into_result()?;
        settled(&env).await;
        assert!(logs.contains(r#""event":"payout_pending""#), "{logs}");
        assert_eq!(env.usdc.balance_of(env.alice.id()).await, before);
        assert_eq!(
            pending(&env, env.alice.id()).await,
            (ONE - CHARGE).to_string()
        );

        // The token recovers; a stranger triggers the claim; only alice is paid.
        env.usdc.set_panic_in_transfer(false).await;
        let stranger = env.worker.dev_create_account().await?;
        let claim = stranger
            .call(env.vault.id(), "claim_payout")
            .args_json(json!({ "account_id": env.alice.id() }))
            .gas(tgas(300))
            .transact()
            .await?;
        report("claim_payout", &claim);
        let logs = claim.logs().join("\n");
        claim.into_result()?;
        settled(&env).await;
        assert!(logs.contains(r#""event":"payout_claimed""#), "{logs}");
        assert_eq!(
            env.usdc.balance_of(env.alice.id()).await,
            before + ONE - CHARGE
        );
        assert_eq!(env.usdc.balance_of(stranger.id()).await, 0);
        assert_eq!(pending(&env, env.alice.id()).await, "0");
        Ok(())
    }

    #[tokio::test]
    async fn a_paused_token_leaves_a_pending_payout_and_a_failed_claim_restores_it() -> Result<()> {
        let env = setup(100 * ONE).await?;
        let note = deposit_note(&env, 42).await?;
        let bundle = close_bundle(&env, &note, env.alice.id().as_str()).await?;
        env.usdc.set_paused(true).await;
        mutual_close(&env, &env.alice, &bundle, tgas(300), json!({}))
            .await?
            .into_result()?;
        settled(&env).await;
        assert_eq!(
            pending(&env, env.alice.id()).await,
            (ONE - CHARGE).to_string()
        );
        // Still paused: the claim fails and puts the amount back.
        let claim = env
            .alice
            .call(env.vault.id(), "claim_payout")
            .args_json(json!({ "account_id": env.alice.id() }))
            .gas(tgas(300))
            .transact()
            .await?;
        report("claim_payout, token paused", &claim);
        claim.into_result()?;
        settled(&env).await;
        assert_eq!(
            pending(&env, env.alice.id()).await,
            (ONE - CHARGE).to_string()
        );
        env.usdc.set_paused(false).await;
        env.alice
            .call(env.vault.id(), "claim_payout")
            .args_json(json!({ "account_id": env.alice.id() }))
            .gas(tgas(300))
            .transact()
            .await?
            .into_result()?;
        settled(&env).await;
        assert_eq!(pending(&env, env.alice.id()).await, "0");
        Ok(())
    }

    #[tokio::test]
    async fn claim_treasury_pays_the_treasury_account_only_and_restores_on_failure() -> Result<()> {
        let env = setup(100 * ONE).await?;
        deposit_note(&env, 42).await?;
        let stranger = env.worker.dev_create_account().await?;
        env.usdc.set_panic_in_transfer(true).await;
        let failed = stranger
            .call(env.vault.id(), "claim_treasury")
            .gas(tgas(300))
            .transact()
            .await?;
        report("claim_treasury, transfer fails", &failed);
        failed.into_result()?;
        settled(&env).await;
        let balance: String = view(&env.vault, "treasury_balance", json!({})).await;
        assert_eq!(balance, NOTE_FEE.to_string());

        env.usdc.set_panic_in_transfer(false).await;
        let paid = stranger
            .call(env.vault.id(), "claim_treasury")
            .gas(tgas(300))
            .transact()
            .await?;
        report("claim_treasury", &paid);
        let logs = paid.logs().join("\n");
        paid.into_result()?;
        settled(&env).await;
        assert!(logs.contains(r#""event":"treasury_claimed""#), "{logs}");
        assert_eq!(env.usdc.balance_of(env.treasury.id()).await, NOTE_FEE);
        assert_eq!(env.usdc.balance_of(stranger.id()).await, 0);
        let balance: String = view(&env.vault, "treasury_balance", json!({})).await;
        assert_eq!(balance, "0");
        Ok(())
    }

    #[tokio::test]
    async fn the_least_gas_that_passes_pays_in_full_and_less_reverts_whole() -> Result<()> {
        let env = setup(100 * ONE).await?;
        let note = deposit_note(&env, 42).await?;
        let bundle = close_bundle(&env, &note, env.alice.id().as_str()).await?;
        // One Tgas short of the minimum: the method panics before any write.
        let short = mutual_close(&env, &env.alice, &bundle, tgas(109), json!({})).await?;
        assert!(short.is_failure(), "109 Tgas must be refused");
        let status: Value = view(&env.vault, "note_records", json!({"from": 0, "limit": 1})).await;
        assert_eq!(status[0]["status"], "Active");
        // At the minimum the payout is the same as with 300 Tgas.
        let before = env.usdc.balance_of(env.alice.id()).await;
        let exact = mutual_close(&env, &env.alice, &bundle, tgas(110), json!({})).await?;
        report("mutual_close, least gas (110 Tgas)", &exact);
        exact.into_result()?;
        settled(&env).await;
        assert_eq!(
            env.usdc.balance_of(env.alice.id()).await,
            before + ONE - CHARGE
        );
        Ok(())
    }

    #[tokio::test]
    async fn claims_with_the_least_gas_pay_in_full_and_less_reverts_whole() -> Result<()> {
        let env = setup(100 * ONE).await?;
        deposit_note(&env, 42).await?;
        let short = env
            .alice
            .call(env.vault.id(), "claim_treasury")
            .gas(tgas(49))
            .transact()
            .await?;
        assert!(short.is_failure());
        let balance: String = view(&env.vault, "treasury_balance", json!({})).await;
        assert_eq!(balance, NOTE_FEE.to_string());
        let exact = env
            .alice
            .call(env.vault.id(), "claim_treasury")
            .gas(tgas(50))
            .transact()
            .await?;
        report("claim_treasury, least gas (50 Tgas)", &exact);
        exact.into_result()?;
        settled(&env).await;
        assert_eq!(env.usdc.balance_of(env.treasury.id()).await, NOTE_FEE);
        Ok(())
    }

    #[tokio::test]
    async fn callbacks_cannot_be_called_from_outside_or_starved() -> Result<()> {
        let env = setup(100 * ONE).await?;
        let args = json!({
            "account": env.alice.id(),
            "amount": "900000",
            "note_id": 0,
            "commitment": commitment(1),
        });
        for caller in [&env.alice, &env.operator] {
            let outcome = caller
                .call(env.vault.id(), "on_close_payout")
                .args_json(args.clone())
                .max_gas()
                .transact()
                .await?;
            assert!(outcome.is_failure());
            assert!(format!("{:?}", outcome.failures()).contains("private"));
        }
        for (method, args) in [
            (
                "on_claim_payout",
                json!({"account": env.alice.id(), "amount": "1"}),
            ),
            ("on_claim_treasury", json!({"amount": "1"})),
        ] {
            let outcome = env
                .alice
                .call(env.vault.id(), method)
                .args_json(args)
                .max_gas()
                .transact()
                .await?;
            assert!(outcome.is_failure(), "{method}");
        }
        // The vault itself, with too little gas for the callback: nothing is written.
        let starved = env
            .vault
            .as_account()
            .call(env.vault.id(), "on_close_payout")
            .args_json(args)
            .gas(tgas(2))
            .transact()
            .await?;
        assert!(starved.is_failure());
        assert_eq!(pending(&env, env.alice.id()).await, "0");
        Ok(())
    }

    /// `extra` more notes join note 0 in the tree, note 0 closes, then anyone retires note 0 with the
    /// siblings a client reads off its rebuild. Returns the env and the retirement's outcome.
    async fn retire_note_zero(extra: u64) -> Result<(Env, ExecutionFinalResult)> {
        let env = setup(1_000 * ONE).await?;
        let note = deposit_note(&env, 42).await?;
        for n in 0..extra {
            transfer_call(
                &env.usdc.contract,
                &env.alice,
                env.vault.id(),
                ONE + NOTE_FEE,
                &deposit_msg(1_000 + n),
            )
            .await?
            .into_result()?;
        }
        sync_root(&env, 80).await?.into_result()?;
        let bundle = close_bundle(&env, &note, env.alice.id().as_str()).await?;
        mutual_close(&env, &env.alice, &bundle, tgas(300), json!({}))
            .await?
            .into_result()?;
        let leaves = all_leaves(&env.vault).await;
        let level = (31 - (leaves.len() as u32).leading_zeros()) as usize;
        let siblings: Vec<String> = MerkleTree::from_leaves(&leaves).get_siblings(0)[..level]
            .iter()
            .map(|s| format!("0x{}", hex::encode(s.0)))
            .collect();
        let outcome = env
            .alice
            .call(env.vault.id(), "retire_closed_leaf")
            .args_json(json!({ "note_id": 0, "siblings": siblings }))
            .max_gas()
            .transact()
            .await?;
        Ok((env, outcome))
    }

    #[tokio::test]
    async fn retiring_a_closed_leaf_matches_the_client_rebuild_within_gas() -> Result<()> {
        // 41 leaves: note 0 sits under frontier slot 5, so 10 path hashes plus the root's up to 32.
        let (env, outcome) = retire_note_zero(40).await?;
        report("retire_closed_leaf, 41 leaves", &outcome);
        let burnt = outcome.total_gas_burnt.as_gas();
        let logs = outcome.logs().join("\n");
        outcome.into_result()?;
        assert!(logs.contains(r#""event":"retire_closed_leaf""#));
        // 94 hashes is the most any tree can need (2 * 31 path hashes and 32 for the root); at
        // 1.8 Tgas each and the 1.2 margin that is 203 Tgas, so the call never needs splitting.
        assert!(burnt * 12 / 10 < 300 * TGAS, "burnt {burnt}");
        let leaves = all_leaves(&env.vault).await;
        assert_eq!(leaves[0], Felt252::ZERO);
        let root: String = view(&env.vault, "current_root", json!({})).await;
        assert_eq!(
            format!(
                "0x{}",
                hex::encode(MerkleTree::from_leaves(&leaves).root().0)
            ),
            root
        );
        let epoch: String = view(&env.vault, "retirement_epoch", json!({})).await;
        assert_eq!(epoch, "1");
        Ok(())
    }

    async fn call_with_bundle(
        env: &Env,
        method: &str,
        bundle: &Bundle,
        extra: Value,
    ) -> Result<ExecutionFinalResult> {
        let mut args = json!({
            "proof": bundle.proof_base64(),
            "inputs": bundle.inputs_base64(),
        });
        args.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        Ok(env
            .alice
            .call(env.vault.id(), method)
            .args_json(args)
            .max_gas()
            .transact()
            .await?)
    }

    /// The gas bound of one call: 300 Tgas with the 1.2 margin the spec asks for.
    fn assert_in_gas(label: &str, outcome: &ExecutionFinalResult) {
        report(label, outcome);
        let burnt = outcome.total_gas_burnt.as_gas();
        assert!(burnt * 12 / 10 < 300 * TGAS, "{label} burnt {burnt}");
    }

    #[tokio::test]
    async fn the_exit_calls_work_with_usdc_paused_and_stay_in_gas() -> Result<()> {
        use zkapi_proof::dev::tree_with;
        let env = setup(100 * ONE).await?;
        let note = deposit_note(&env, 42).await?;
        let valid_until = now_seconds(&env.worker).await + 3_600;
        let g = gen(env.vault.id());
        let s1 = state_after_spending(&env, &note);
        let now = now_seconds(&env.worker).await;
        let request = |bound| DevRequest {
            request_time: now,
            solvency_bound: bound,
            request_context: Felt252::from_u64(99),
            rerandomization: Felt252::from_u64(9),
        };
        let s2 = g.next_state(&s1, &request(100_000), 30_000, &Felt252::from_u64(18))?;
        let tree = tree_with(&[&note]);
        let escape = g.withdrawal(
            &s1,
            WithdrawalKindId::Escape,
            env.alice.id().as_str(),
            0,
            valid_until,
        )?;
        let raise = g.request(&s1, &tree, &request(50_000))?;
        let defeat = g.request(&s2, &tree, &request(100_000))?;

        // No USDC call anywhere in the filing: it works with the token paused.
        env.usdc.set_paused(true).await;
        let filed = call_with_bundle(&env, "request_exit", &escape, json!({})).await?;
        assert_in_gas("request_exit", &filed);
        assert!(filed
            .logs()
            .join("\n")
            .contains(r#""event":"request_exit""#));
        filed.into_result()?;
        let exits: Vec<Value> = view(
            &env.vault,
            "exit_requests",
            json!({"from_note_id": 0, "limit": 100}),
        )
        .await;
        assert_eq!(exits.len(), 1);
        assert_eq!(exits[0]["final_balance"], (ONE - CHARGE).to_string());
        let records: Vec<Value> =
            view(&env.vault, "note_records", json!({"from": 0, "limit": 1})).await;
        assert_eq!(records[0]["status"], "PendingExit");

        // Anyone answers: raise at the escaped nullifier, then defeat at the successor.
        let raised = call_with_bundle(
            &env,
            "raise_concession",
            &raise,
            json!({"note_id": 0, "amount": "40000"}),
        )
        .await?;
        assert_in_gas("raise_concession", &raised);
        raised.into_result()?;
        let exits: Vec<Value> = view(
            &env.vault,
            "exit_requests",
            json!({"from_note_id": 0, "limit": 100}),
        )
        .await;
        assert_eq!(exits[0]["conceded"], "40000");
        assert_eq!(
            exits[0]["final_balance"],
            (ONE - CHARGE - 40_000).to_string()
        );
        let defeated =
            call_with_bundle(&env, "defeat_exit", &defeat, json!({"note_id": 0})).await?;
        assert_in_gas("defeat_exit", &defeated);
        defeated.into_result()?;
        let records: Vec<Value> =
            view(&env.vault, "note_records", json!({"from": 0, "limit": 1})).await;
        assert_eq!(records[0]["status"], "Active");
        let exits: Vec<Value> = view(
            &env.vault,
            "exit_requests",
            json!({"from_note_id": 0, "limit": 100}),
        )
        .await;
        assert!(exits.is_empty());
        // The escaped state is dead for good.
        let again = call_with_bundle(&env, "request_exit", &escape, json!({})).await?;
        assert!(again.is_failure());
        Ok(())
    }
}

// The Intents branch against the open-source Verifier v0.4.2

/// Gas budget of the vault's part of the Intents path.
const INTENTS_GAS_BUDGET: u64 = 20 * TGAS;

#[tokio::test]
async fn the_intents_branch_runs_against_the_open_source_verifier() -> Result<()> {
    use nymira_testkit::intents::deploy_verifier;

    // The test-only Intents account names the fixture's account, `intents.test.near`; the
    // prebuilt-wasm override does not apply, it has no such feature.
    let env = setup_with(1_000 * ONE, &["dev-setup", "test-intents-account"]).await?;
    let verifier = deploy_verifier(&env.worker, "intents", "wrap.test.near").await?;
    assert_eq!(verifier.id().as_str(), "intents.test.near");
    env.usdc.register_id(verifier.id()).await;
    let bob = env.worker.dev_create_account().await?;
    env.usdc.register(&bob).await;

    // Deposit into the Verifier, credited to alice.
    transfer_call(
        &env.usdc.contract,
        &env.alice,
        verifier.id(),
        20 * ONE,
        &json!({ "receiver_id": env.alice.id() }).to_string(),
    )
    .await?
    .into_result()?;

    let withdraw = |amount: u128, msg: String| {
        let alice = env.alice.clone();
        let verifier = verifier.id().clone();
        let usdc = env.usdc.contract.id().clone();
        let vault = env.vault.id().clone();
        async move {
            alice
                .call(&verifier, "ft_withdraw")
                .args_json(json!({
                    "token": usdc,
                    "receiver_id": vault,
                    "amount": amount.to_string(),
                    "msg": msg,
                }))
                .deposit(NearToken::from_yoctonear(1))
                .max_gas()
                .transact()
                .await
        }
    };

    // A valid message with an excess: a note for bob and a pending payout for the rest.
    let msg = json!({ "commitment": commitment(1), "withdraw_to": bob.id() }).to_string();
    let outcome = withdraw(ONE + NOTE_FEE + 70_000, msg).await?;
    report("intents deposit, note and excess", &outcome);
    let on_vault = gas_on(&outcome, env.vault.id());
    println!(
        "gas on the vault: {:.2} Tgas",
        on_vault as f64 / TGAS as f64
    );
    assert!(on_vault < INTENTS_GAS_BUDGET, "{on_vault}");
    let logs = outcome.logs().join("\n");
    outcome.into_result()?;
    assert!(logs.contains(r#""event":"deposit""#), "{logs}");
    assert!(logs.contains(r#""reason":"intents_excess""#), "{logs}");
    let notes: Vec<Value> = view(&env.vault, "note_records", json!({"from": 0, "limit": 10})).await;
    assert_eq!(notes.len(), 1);
    let owed: String = view(
        &env.vault,
        "pending_payout",
        json!({"account_id": bob.id()}),
    )
    .await;
    assert_eq!(owed, "70000");
    // The Verifier saw nothing come back: the whole amount was used.
    assert_eq!(
        env.usdc.balance_of(verifier.id()).await,
        20 * ONE - (ONE + NOTE_FEE + 70_000)
    );

    // A bad message: no refund to the Verifier, the amount is unclaimed treasury.
    let outcome = withdraw(2 * ONE, "not json".to_owned()).await?;
    report("intents deposit, bad message", &outcome);
    let on_vault = gas_on(&outcome, env.vault.id());
    assert!(on_vault < INTENTS_GAS_BUDGET, "{on_vault}");
    let logs = outcome.logs().join("\n");
    outcome.into_result()?;
    assert!(logs.contains(r#""event":"unclaimed""#), "{logs}");
    assert!(!logs.contains("not json"));
    let treasury: String = view(&env.vault, "treasury_balance", json!({})).await;
    assert_eq!(treasury, (NOTE_FEE + 2 * ONE).to_string());
    assert_eq!(
        env.usdc.balance_of(verifier.id()).await,
        20 * ONE - (ONE + NOTE_FEE + 70_000) - 2 * ONE
    );

    // Bob collects what is owed.
    let claimed = bob
        .call(env.vault.id(), "claim_payout")
        .args_json(json!({ "account_id": bob.id() }))
        .max_gas()
        .transact()
        .await?;
    report("claim_payout of an Intents excess", &claimed);
    claimed.into_result()?;
    env.worker.fast_forward(5).await?;
    assert_eq!(env.usdc.balance_of(bob.id()).await, 70_000);
    let owed: String = view(
        &env.vault,
        "pending_payout",
        json!({"account_id": bob.id()}),
    )
    .await;
    assert_eq!(owed, "0");
    Ok(())
}
