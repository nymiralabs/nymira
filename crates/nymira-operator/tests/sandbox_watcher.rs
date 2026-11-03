//! The watcher against a real NEAR sandbox node and the real vault: defeat and raise work for
//! an account that is not the operator's, and a closed leaf is retired with the siblings the
//! watcher computes.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::{inputs_from, payload};
use ed25519_dalek::SigningKey;
use near_workspaces::types::{AccessKey, NearToken, PublicKey};
use near_workspaces::{Account, AccountId, Contract};
use nymira_near_codec::Network;
use nymira_operator::alarm::Alarms;
use nymira_operator::chain::{RpcVault, VaultView};
use nymira_operator::clock::ManualClock;
use nymira_operator::operator::felt_hex;
use nymira_operator::store::{self, NewLease, Store};
use nymira_operator::tx::{NearTx, TxConfig};
use nymira_operator::watcher::{NearCalls, Watcher, WatcherConfig};
use nymira_testkit::build::contract_wasm_with_features;
use nymira_testkit::clock::now_seconds;
use nymira_testkit::mock_usdc::MockUsdc;
use nymira_testkit::proofs::{
    canonical_verifying_key, signing_key, Bundle, Kind, ProofGen, WithdrawalKindId,
};
use serde_json::{json, Value};
use zkapi_core::MerkleTree;
use zkapi_proof::dev::{tree_with, DevNote, DevRequest};
use zkapi_types::wire::{ApiRequestV2, Groth16ProofWire, ProofBackendWire};
use zkapi_types::{canonical_payload_hash, Felt252};

const ONE: u128 = 1_000_000;
const NOTE_FEE: u128 = 50_000;

fn point(seed: u64) -> Value {
    let key = signing_key(seed);
    json!({ "x": format!("0x{}", hex::encode(key.x.0)), "y": format!("0x{}", hex::encode(key.y.0)) })
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

struct Env {
    worker: near_workspaces::Worker<near_workspaces::network::Sandbox>,
    usdc: MockUsdc,
    vault: Contract,
    alice: Account,
    dir: tempfile::TempDir,
}

async fn setup() -> Env {
    setup_with(7 * 86_400, 30 * 86_400).await
}

async fn setup_with(exit_delay_secs: u64, note_ttl: u64) -> Env {
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
            "denominations": ["1000000"], "tvl_cap": "100000000",
            "request_vk": b64(&canonical_verifying_key(Kind::Request)),
            "withdrawal_vk": b64(&canonical_verifying_key(Kind::Withdrawal)),
            "treasury": treasury.id(), "note_fee": NOTE_FEE.to_string(),
            "exit_delay_secs": exit_delay_secs, "note_ttl": note_ttl,
            "bound_classes": ["100000"], "charge_step": "1000", "min_charge": "2000",
            "slot_rate": 1000, "owner": alice.id(), "operator_account": operator.id(),
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
    usdc.mint(alice.id(), 1_000 * ONE).await;
    Env {
        worker,
        usdc,
        vault,
        alice,
        dir: tempfile::tempdir().unwrap(),
    }
}

async fn view<T: serde::de::DeserializeOwned>(vault: &Contract, method: &str, args: Value) -> T {
    vault
        .view(method)
        .args_json(args)
        .await
        .unwrap()
        .json()
        .unwrap()
}

async fn deposit(env: &Env, registration: &Felt252) {
    env.alice
        .call(env.usdc.contract.id(), "ft_transfer_call")
        .args_json(json!({
            "receiver_id": env.vault.id(), "amount": (ONE + NOTE_FEE).to_string(),
            "msg": json!({ "commitment": format!("0x{}", hex::encode(registration.0)) }).to_string(),
        }))
        .deposit(NearToken::from_yoctonear(1))
        .max_gas()
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
}

async fn note(env: &Env, secret: u64) -> DevNote {
    let before: Vec<Value> =
        view(&env.vault, "note_records", json!({"from": 0, "limit": 100})).await;
    let note = DevNote {
        secret: Felt252::from_u64(secret),
        note_id: before.len() as u32,
        deposit_amount: ONE,
        expiry: 0,
    };
    deposit(env, &note.registration()).await;
    let after: Vec<Value> =
        view(&env.vault, "note_records", json!({"from": 0, "limit": 100})).await;
    DevNote {
        expiry: after[note.note_id as usize]["expiry"].as_u64().unwrap(),
        ..note
    }
}

/// An account that is not the operator's, with a function-call key that lists its methods.
async fn keyed(env: &Env, seed: u8, methods: &[&str]) -> (Account, Arc<NearTx>, Store) {
    let account = env.worker.dev_create_account().await.unwrap();
    let key = SigningKey::from_bytes(&[seed; 32]);
    let public: PublicKey = format!(
        "ed25519:{}",
        bs58::encode(key.verifying_key().as_bytes()).into_string()
    )
    .parse()
    .unwrap();
    account
        .batch(account.id())
        .add_key(
            public,
            AccessKey::function_call_access(env.vault.id(), methods, None),
        )
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let store = Store::open(&env.dir.path().join(format!("tx{seed}.db"))).unwrap();
    let tx = Arc::new(
        NearTx::new(
            env.worker.rpc_addr(),
            account.id().as_str(),
            env.vault.id().as_str(),
            key,
            store.clone(),
            Arc::new(ManualClock::new(1_000)),
        )
        .with_config(TxConfig {
            send_timeout: Duration::from_secs(60),
            retry_delay: Duration::from_millis(300),
            max_attempts: 40,
        }),
    );
    (account, tx, store)
}

fn stored_request(bundle: &Bundle, id: &str) -> ApiRequestV2 {
    let payload = payload();
    ApiRequestV2 {
        client_request_id: id.into(),
        payload_hash: canonical_payload_hash(payload.as_bytes()),
        payload,
        public_inputs: inputs_from(bundle),
        proof: Groth16ProofWire {
            backend: ProofBackendWire::Groth16Bn254,
            proof: bundle.proof_base64(),
        },
    }
}

async fn lease_for(store: &Store, bundle: &Bundle, id: &str, bound: u128, delivered: bool) {
    let request = stored_request(bundle, id);
    let lease = NewLease {
        nullifier: felt_hex(&request.public_inputs.request_nullifier),
        client_request_id: id.into(),
        lease_id: id.into(),
        token_hash: vec![0; 32],
        bound,
        request_json: serde_json::to_string(&request).unwrap(),
        reserved_at: 1,
    };
    let id = id.to_string();
    store
        .run(move |c| {
            store::reserve_lease(c, &lease)?;
            store::activate_lease(c, &id, "key", 1, 601)?;
            if delivered {
                store::mark_delivered(c, &id, 2)?;
            }
            Ok(())
        })
        .await
        .unwrap();
}

async fn watcher(
    env: &Env,
    answer: Arc<NearTx>,
    upkeep: Arc<NearTx>,
    store: Store,
    clock: Arc<ManualClock>,
) -> Watcher {
    let providers: Vec<Arc<dyn VaultView>> = (0..2)
        .map(|_| {
            Arc::new(RpcVault::new(
                env.worker.rpc_addr(),
                env.vault.id().as_str(),
            )) as Arc<dyn VaultView>
        })
        .collect();
    Watcher::new(
        providers,
        Arc::new(NearCalls { answer, upkeep }),
        store,
        clock.clone(),
        Arc::new(Alarms::new(vec![], clock)),
        WatcherConfig::default(),
    )
    .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_that_is_not_the_operators_raises_and_defeats_for_anyone() {
    let env = setup().await;
    let gen = ProofGen::new(Network::Testnet, env.vault.id().as_str()).unwrap();
    // Note 0 has a delivered lease at the escaped state; note 1 has one past it.
    let (note0, note1) = (note(&env, 42).await, note(&env, 43).await);
    let chain_now = now_seconds(&env.worker).await;
    let request = |bound| DevRequest {
        request_time: chain_now,
        solvency_bound: bound,
        request_context: Felt252::from_u64(99),
        rerandomization: Felt252::from_u64(9),
    };
    let tree = tree_with(&[&note0, &note1]);
    let spend = |note: &DevNote, charge, blinding| {
        let s0 = note.genesis(Felt252::from_u64(7));
        gen.next_state(&s0, &request(100_000), charge, &Felt252::from_u64(blinding))
            .unwrap()
    };
    let (s1, t1) = (spend(&note0, 20_000, 17), spend(&note1, 20_000, 19));
    let t2 = gen
        .next_state(&t1, &request(100_000), 30_000, &Felt252::from_u64(20))
        .unwrap();
    let raise_proof = gen.request(&s1, &tree, &request(50_000)).unwrap();
    let defeat_proof = gen.request(&t2, &tree, &request(100_000)).unwrap();
    for state in [&s1, &t1] {
        let escape = gen
            .withdrawal(
                state,
                WithdrawalKindId::Escape,
                env.alice.id().as_str(),
                0,
                chain_now + 3_600,
            )
            .unwrap();
        env.alice
            .call(env.vault.id(), "request_exit")
            .args_json(json!({ "proof": escape.proof_base64(), "inputs": escape.inputs_base64() }))
            .max_gas()
            .transact()
            .await
            .unwrap()
            .into_result()
            .unwrap();
    }

    let (_, answer, _) = keyed(&env, 21, &["defeat_exit", "raise_concession"]).await;
    let (_, upkeep, store) = keyed(
        &env,
        22,
        &["sync_root", "retire_closed_leaf", "finalize_exit"],
    )
    .await;
    // Let the keys reach a final block.
    tokio::time::sleep(Duration::from_secs(5)).await;
    let clock = Arc::new(ManualClock::new((chain_now + 60) * 1_000));
    let watcher = watcher(&env, answer, upkeep, store.clone(), clock).await;
    // The proofs were stored when the leases were served, before either exit was filed.
    lease_for(&store, &raise_proof, "raise", 50_000, true).await;
    lease_for(&store, &defeat_proof, "defeat", 100_000, true).await;

    // A sandbox makes blocks only when asked; the watcher reads final ones.
    env.worker.fast_forward(10).await.unwrap();
    watcher.poll().await;
    let mut exits: Vec<Value> = Vec::new();
    for _ in 0..10 {
        env.worker.fast_forward(5).await.unwrap();
        exits = view(
            &env.vault,
            "exit_requests",
            json!({"from_note_id": 0, "limit": 100}),
        )
        .await;
        if exits.len() == 1 {
            break;
        }
    }
    assert_eq!(exits.len(), 1, "note 1's exit is defeated: {exits:?}");
    assert_eq!(exits[0]["note_id"], 0);
    assert_eq!(
        exits[0]["conceded"], "50000",
        "note 0's concession rose to the proof's bound"
    );
    let records: Vec<Value> =
        view(&env.vault, "note_records", json!({"from": 0, "limit": 2})).await;
    assert_eq!(records[0]["status"], "PendingExit");
    assert_eq!(records[1]["status"], "Active", "defeated");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_closed_leaf_is_retired_by_the_watcher_with_its_own_siblings() {
    let env = setup().await;
    let gen = ProofGen::new(Network::Testnet, env.vault.id().as_str()).unwrap();
    let note0 = note(&env, 42).await;
    for secret in [43, 44] {
        note(&env, secret).await;
    }
    // The vault inserts a small batch only thirty seconds after the last insert.
    let waited = now_seconds(&env.worker).await + 31;
    nymira_testkit::clock::advance_to(&env.worker, waited)
        .await
        .unwrap();
    env.alice
        .call(env.vault.id(), "sync_root")
        .args_json(json!({ "max": 80 }))
        .max_gas()
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let chain_now = now_seconds(&env.worker).await;
    let s0 = note0.genesis(Felt252::from_u64(7));
    let close = gen
        .withdrawal(
            &s0,
            WithdrawalKindId::MutualClose,
            env.alice.id().as_str(),
            0,
            chain_now + 3_600,
        )
        .unwrap();
    env.alice
        .call(env.vault.id(), "mutual_close")
        .args_json(json!({ "proof": close.proof_base64(), "inputs": close.inputs_base64() }))
        .max_gas()
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();

    let (_, answer, _) = keyed(&env, 31, &["defeat_exit", "raise_concession"]).await;
    let (_, upkeep, store) = keyed(&env, 32, &["sync_root", "retire_closed_leaf"]).await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    let clock = Arc::new(ManualClock::new((chain_now + 60) * 1_000));
    env.worker.fast_forward(10).await.unwrap();
    watcher(&env, answer, upkeep, store, clock)
        .await
        .poll()
        .await;

    let mut records: Vec<Value> = Vec::new();
    for _ in 0..30 {
        records = view(&env.vault, "note_records", json!({"from": 0, "limit": 100})).await;
        if records[0]["leaf_retired"] == true {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert_eq!(records[0]["leaf_retired"], true, "{records:?}");
    assert_eq!(records[1]["leaf_retired"], false);
    let mut leaves = Vec::new();
    let page: Vec<String> = view(&env.vault, "get_leaves", json!({"from": 0, "limit": 100})).await;
    for leaf in page {
        let bytes: [u8; 32] = hex::decode(leaf.trim_start_matches("0x"))
            .unwrap()
            .try_into()
            .unwrap();
        leaves.push(Felt252(bytes));
    }
    assert_eq!(leaves[0], Felt252::ZERO);
    let root: String = view(&env.vault, "current_root", json!({})).await;
    assert_eq!(
        format!(
            "0x{}",
            hex::encode(MerkleTree::from_leaves(&leaves).root().0)
        ),
        root
    );
    let _: Option<AccountId> = None;
}

/// `finalize_exit` and `claim_expired_batch` sent by the watcher's own keys, and the gas of a
/// full batch. Both wait for the vault's real windows: the shortest exit delay `new()` takes is
/// one day, which is about 290,000 sandbox blocks, so this runs for one to two hours and is
/// ignored by default:
/// `cargo test -p nymira-operator --test sandbox_watcher claim_and_finalize -- --ignored --nocapture`.
///
/// The measured gas of 50 swept notes is printed as `measure claim_expired_batch`; the
/// assertion is that the call succeeds with the 299 Tgas the watcher attaches.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "one to two hours of sandbox time"]
async fn claim_and_finalize_are_sent_by_the_watcher_and_a_full_batch_fits_in_its_gas() {
    use nymira_operator::watcher::GAS_CLAIM_BATCH;
    const FULL_BATCH: u32 = 50;
    let env = setup_with(86_400, 1).await;
    let gen = ProofGen::new(Network::Testnet, env.vault.id().as_str()).unwrap();
    // Note 0 leaves by exit; notes 1..=50 are swept by a direct call that measures the gas;
    // notes 51..=53 are swept by the watcher.
    let mut notes = Vec::new();
    for secret in 0..=FULL_BATCH + 3 {
        notes.push(note(&env, 100 + u64::from(secret)).await);
    }
    let chain_now = now_seconds(&env.worker).await;
    let s0 = notes[0].genesis(Felt252::from_u64(7));
    let escape = gen
        .withdrawal(
            &s0,
            WithdrawalKindId::Escape,
            env.alice.id().as_str(),
            0,
            chain_now + 3_600,
        )
        .unwrap();
    env.alice
        .call(env.vault.id(), "request_exit")
        .args_json(json!({ "proof": escape.proof_base64(), "inputs": escape.inputs_base64() }))
        .max_gas()
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let exits: Vec<Value> = view(
        &env.vault,
        "exit_requests",
        json!({"from_note_id": 0, "limit": 1}),
    )
    .await;
    let deadline = exits[0]["deadline"]
        .as_str()
        .unwrap()
        .parse::<u64>()
        .unwrap()
        / 1_000_000_000;
    let records: Vec<Value> =
        view(&env.vault, "note_records", json!({"from": 0, "limit": 100})).await;
    let claimable = records
        .iter()
        .map(|r| r["expiry"].as_u64().unwrap() + 86_400)
        .max()
        .unwrap();
    let target = deadline.max(claimable) + 1;
    eprintln!(
        "advancing the chain {} s to {target}",
        target - now_seconds(&env.worker).await
    );
    nymira_testkit::clock::advance_to(&env.worker, target)
        .await
        .unwrap();

    // The measured call: a full batch with the attached gas the watcher uses.
    let swept = env
        .alice
        .call(env.vault.id(), "claim_expired_batch")
        .args_json(json!({ "from": 1, "limit": FULL_BATCH }))
        .gas(near_workspaces::types::Gas::from_gas(GAS_CLAIM_BATCH))
        .transact()
        .await
        .unwrap();
    println!(
        "measure claim_expired_batch ({FULL_BATCH} notes): {} Tgas burnt of {} Tgas attached",
        swept.total_gas_burnt.as_gas() as f64 / 1e12,
        GAS_CLAIM_BATCH as f64 / 1e12
    );
    let ids: Vec<u32> = swept.into_result().unwrap().json().unwrap();
    assert_eq!(ids, (1..=FULL_BATCH).collect::<Vec<u32>>());

    // The watcher: keys that list `finalize_exit` and `claim_expired_batch` but not the single
    // `claim_expired`, so a fallback to single calls cannot hide a failed batch.
    let (_, answer, _) = keyed(&env, 41, &["defeat_exit", "raise_concession"]).await;
    let (_, upkeep, store) = keyed(
        &env,
        42,
        &["sync_root", "finalize_exit", "claim_expired_batch"],
    )
    .await;
    tokio::time::sleep(Duration::from_secs(5)).await;
    env.worker.fast_forward(10).await.unwrap();
    let clock = Arc::new(ManualClock::new(
        (now_seconds(&env.worker).await + 60) * 1_000,
    ));
    let before = env.usdc.balance_of(env.alice.id()).await;
    watcher(&env, answer, upkeep, store, clock)
        .await
        .poll()
        .await;

    let mut records: Vec<Value> = Vec::new();
    for _ in 0..60 {
        records = view(&env.vault, "note_records", json!({"from": 0, "limit": 100})).await;
        if records.iter().all(|r| r["status"] == "Closed") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(
        records.iter().all(|r| r["status"] == "Closed"),
        "note 0 finalized, 51..=53 swept by the batch: {records:?}"
    );
    let exits: Vec<Value> = view(
        &env.vault,
        "exit_requests",
        json!({"from_note_id": 0, "limit": 100}),
    )
    .await;
    assert!(exits.is_empty(), "{exits:?}");
    assert!(
        env.usdc.balance_of(env.alice.id()).await > before,
        "the exit's payout reached its account"
    );
}
