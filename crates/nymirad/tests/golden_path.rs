//! The golden path, start to finish, in the mock stack: a 1 USDC note is deposited through the
//! wallet, used for a chat through the operator, settled, closed with a clearance, and its
//! leaf retired. Every step is asserted in order, so a break in any module stops the walk at
//! that step. The same walk is done by hand on testnet with real NEAR AI (issue 35).

#[path = "support/stack.rs"]
mod stack;

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use near_workspaces::types::{AccessKey, NearToken, PublicKey};
use nymira_operator::store::Store;
use nymira_operator::tx::{NearTx, TxConfig};
use nymirad::attest::Trust;
use nymirad::backup;
use nymirad::deposit::{Confirm, Ctx, Summary};
use nymirad::gateway::{ChatOptions, Outcome};
use nymirad::mode::Mode;
use nymirad::pins::{read_live, Tripwire};
use nymirad::rpc::{Rpc, Transport};
use nymirad::secrets::{NoteStatus, NoteStore};
use serde_json::{json, Value};
use stack::*;
use zkapi_core::MerkleTree;
use zkapi_types::Felt252;

struct Yes;

impl Confirm for Yes {
    fn confirm(&self, _: &Summary) -> bool {
        true
    }
}

async fn view(s: &Stack, method: &str, args: Value) -> Value {
    s.vault
        .view(method)
        .args_json(args)
        .await
        .unwrap()
        .json()
        .unwrap()
}

async fn leaves(s: &Stack) -> Vec<String> {
    serde_json::from_value(view(s, "get_leaves", json!({"from": 0, "limit": 10})).await).unwrap()
}

fn amount(v: &Value) -> u128 {
    v.as_str().unwrap().parse().unwrap()
}

/// Gas burnt by every receipt of a transaction, in Tgas.
async fn tgas_of(s: &Stack, tx_hash: &str, sender: &str) -> f64 {
    let rpc = Rpc::new(s.worker.rpc_addr(), &Transport::Direct).unwrap();
    let status = rpc
        .call(
            "EXPERIMENTAL_tx_status",
            json!({ "tx_hash": tx_hash, "sender_account_id": sender, "wait_until": "FINAL" }),
        )
        .await
        .unwrap();
    let mut burnt = status["transaction_outcome"]["outcome"]["gas_burnt"]
        .as_u64()
        .unwrap_or(0);
    for r in status["receipts_outcome"].as_array().into_iter().flatten() {
        burnt += r["outcome"]["gas_burnt"].as_u64().unwrap_or(0);
    }
    burnt as f64 / 1e12
}

/// The wallet's deposit context: alice signs the token transfer with a full-access key of her own.
async fn deposit_ctx(s: &Stack) -> Ctx {
    let key = SigningKey::from_bytes(&[21u8; 32]);
    let public: PublicKey = format!(
        "ed25519:{}",
        bs58::encode(key.verifying_key().as_bytes()).into_string()
    )
    .parse()
    .unwrap();
    s.alice
        .batch(s.alice.id())
        .add_key(public, AccessKey::full_access())
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    // The new key shows in a final block only a few blocks later.
    s.worker.fast_forward(5).await.unwrap();
    let rpc = Rpc::new(s.worker.rpc_addr(), &Transport::Direct).unwrap();
    let pins = read_live(&rpc, s.vault.id().as_str()).await.unwrap();
    let tx_store = Store::open(&s.home.path().join("deposit-tx.db")).unwrap();
    let tx = NearTx::new(
        s.worker.rpc_addr(),
        s.alice.id().as_str(),
        s.usdc.contract.id().as_str(),
        key,
        tx_store.clone(),
        s.clock.clone(),
    )
    .with_deposit(1)
    .with_config(TxConfig {
        send_timeout: Duration::from_secs(60),
        retry_delay: Duration::from_millis(200),
        max_attempts: 5,
    });
    let (_, recipient) = backup::generate_identity();
    Ctx {
        // The mainnet profile: the testnet profile pins Circle's USDC, which the sandbox lacks.
        mode: Mode::Mainnet,
        rpc,
        pins,
        tripwire: Arc::new(Tripwire::default()),
        notes: NoteStore::open(&s.home.path().join("notes.sealed"), PASS, 10).unwrap(),
        tx,
        tx_store,
        sender: s.alice.id().to_string(),
        clock: s.clock.clone(),
        backup: Some((
            backup::parse_recipient(&recipient).unwrap(),
            vec![s.home.path().join("backup")],
        )),
        calendar_dir: s.home.path().join("cal"),
    }
}

fn request(text: &str) -> Value {
    json!({"model": MODEL, "stream": false, "messages": [{"role": "user", "content": text}]})
}

#[tokio::test(flavor = "multi_thread")]
async fn the_whole_first_release_path_runs_in_order() {
    let s = build(Transport::Direct, false).await;
    let bob = s.worker.dev_create_account().await.unwrap();
    s.usdc.register(&bob).await;
    let alice_start = s.usdc.balance_of(s.alice.id()).await;

    // 1. Deposit a 1 USDC note. The fee comes from `config()`, the receipt confirms the deposit.
    let config = view(&s, "config", json!({})).await;
    let fee = amount(&config["note_fee"]);
    assert_eq!(fee, FEE, "the vault's own fee, read from config()");
    let ctx = deposit_ctx(&s).await;
    let accepted = ctx
        .deposit(ONE, Some(bob.id().as_str()), &Yes)
        .await
        .expect("the deposit is accepted from its receipt");
    assert_eq!((accepted.note_id, accepted.deposit_amount), (0, ONE));
    assert!(accepted.warnings.is_empty(), "{:?}", accepted.warnings);
    assert_eq!(
        s.usdc.balance_of(s.alice.id()).await,
        alice_start - ONE - fee
    );
    assert_eq!(s.usdc.balance_of(s.vault.id()).await, ONE + fee);
    assert_eq!(amount(&view(&s, "treasury_balance", json!({})).await), fee);
    let notes = ctx.notes.list().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].status, NoteStatus::Accepted);
    assert_eq!(notes[0].payout, bob.id().as_str());
    let commitment = notes[0].commitment.clone();
    assert_eq!(
        view(&s, "note_status", json!({ "commitment": commitment })).await,
        "Active"
    );
    let tgas_deposit = tgas_of(&s, &accepted.tx_hash, s.alice.id().as_str()).await;
    drop(ctx);

    // 2. `sync_root` puts the leaf into the root.
    let root_before = view(&s, "current_root", json!({})).await;
    sync_root(&s.worker, &s.alice, &s.vault).await;
    let root = view(&s, "current_root", json!({})).await;
    assert_ne!(root, root_before, "the leaf is in the root");
    assert_eq!(leaves(&s).await.len(), 1);
    // 3-6. A chat: the tree is built from the vault's leaves, the proof is made, the lease
    // opens, the operator relays the encrypted exchange.
    let answer = match s
        .gateway
        .chat(
            request("What is the capital of France?"),
            ChatOptions::default(),
        )
        .await
        .map_err(|e| e.message)
        .expect("a lease opens on the note and the chat is answered")
    {
        Outcome::Json(v, _) => v,
        Outcome::Stream(..) => panic!("expected JSON"),
    };
    assert_eq!(
        answer["choices"][0]["message"]["content"],
        "fake reply to: What is the capital of France?"
    );
    let seen = s.fake.handle.chats();
    assert_eq!(seen.len(), 1, "one request reached the provider");
    let seen = &seen[0];
    assert!(seen.e2ee && seen.encrypt_all_fields && seen.no_aliasing && seen.include_usage);
    assert!(
        !String::from_utf8_lossy(&seen.body).contains("capital of France"),
        "the provider saw ciphertext only"
    );
    let lease = s.gateway.records();
    assert_eq!(lease.len(), 1);
    assert_eq!(lease[0].note_id, 0);
    assert_eq!(
        lease[0].root,
        root.as_str().unwrap(),
        "the proof is against the vault's current root"
    );
    assert_eq!(lease[0].leaf_count, 1);
    assert!(lease[0].snapshot_block > 0);

    // 7. Retire the lease, the operator settles, the wallet checks the signature and the charge
    // and stores the next state.
    s.gateway.reap(true).await;
    for _ in 0..3 {
        s.op.tick().await;
        s.clock.advance_secs(11);
    }
    s.op.tick().await;
    s.op.tick().await;
    let settled = s.gateway.settle().await;
    assert_eq!(settled.len(), 1, "the lease settled");
    let done = &settled[0];
    // Attestation (before the lease) and signature (here) both checked against the test policy.
    assert_eq!(done.trust, Trust::ModelSigned);
    assert!(done.warnings.is_empty(), "{:?}", done.warnings);
    assert_eq!(
        done.charge, done.expected,
        "the charge is what the signed usage costs"
    );
    assert!(
        (2000..=100_000).contains(&done.charge),
        "inside the bound class: {}",
        done.charge
    );
    let record = &s.gateway.records()[0];
    assert_eq!(
        record.charge.as_deref(),
        Some(done.charge.to_string().as_str())
    );
    assert_eq!((record.requests, record.message_count), (1, 1));
    // The next state is stored: sealed under the wallet directory, nothing readable in the clear.
    let wallets: Vec<_> = std::fs::read_dir(s.home.path().join("wallets"))
        .expect("the wallet is sealed under the home directory")
        .flatten()
        .collect();
    assert_eq!(wallets.len(), 1);
    let sealed = std::fs::read(wallets[0].path()).unwrap();
    assert!(!String::from_utf8_lossy(&sealed).contains("current_balance"));
    let secret = NoteStore::open(&s.home.path().join("notes.sealed"), PASS, 10)
        .unwrap()
        .list()
        .unwrap()[0]
        .secret
        .clone();
    assert!(!String::from_utf8_lossy(&sealed).contains(secret.as_str()));
    // Nothing moved on chain yet: the charge is only settled by the close.
    assert_eq!(s.usdc.balance_of(bob.id()).await, 0);

    // 8-10. Close: the clearance from the operator (checked by the vault inside the proof) and
    // `mutual_close` from the submitter account. The operator did not send the close itself.
    let sub = submitter(&s, "personal", NearToken::from_near(5)).await;
    let closed = s
        .gateway
        .close_note(0, no_delay(), None)
        .await
        .expect("the close lands");
    assert!(closed.confirmed);
    assert_eq!(closed.payout_account, bob.id().as_str());
    assert!(
        s.closes.sent().is_empty(),
        "the close came from the submitter, not the operator"
    );
    assert_eq!(
        view(&s, "note_status", json!({ "commitment": commitment })).await,
        "Closed"
    );
    let tgas_close = tgas_of(&s, &closed.tx_hash, sub.id().as_str()).await;
    // The payout account got the deposit minus the charge, to the base unit.
    assert_eq!(s.usdc.balance_of(bob.id()).await, ONE - done.charge);
    assert_eq!(entry(&s).status, NoteStatus::Closed);
    // The treasury holds the fee and the charge, and the vault holds exactly that.
    let treasury = amount(&view(&s, "treasury_balance", json!({})).await);
    assert_eq!(treasury, fee + done.charge);
    assert_eq!(s.usdc.balance_of(s.vault.id()).await, treasury);
    assert_eq!(s.usdc.balance_of(s.treasury.id()).await, 0);

    // 11. `claim_treasury` pays the treasury account, from any caller.
    let claimed = s
        .alice
        .call(s.vault.id(), "claim_treasury")
        .max_gas()
        .transact()
        .await
        .unwrap();
    let tgas_claim = claimed.total_gas_burnt.as_gas() as f64 / 1e12;
    claimed.into_result().unwrap();
    s.worker.fast_forward(5).await.unwrap();
    assert_eq!(s.usdc.balance_of(s.treasury.id()).await, fee + done.charge);
    assert_eq!(amount(&view(&s, "treasury_balance", json!({})).await), 0);
    assert_eq!(s.usdc.balance_of(s.vault.id()).await, 0);

    // 12. `retire_closed_leaf` with the siblings a client reads off its own rebuild.
    let words: Vec<Felt252> = leaves(&s)
        .await
        .iter()
        .map(|l| {
            let mut w = [0u8; 32];
            w.copy_from_slice(&hex::decode(l.trim_start_matches("0x")).unwrap());
            Felt252(w)
        })
        .collect();
    let level = (31 - (words.len() as u32).leading_zeros()) as usize;
    let siblings: Vec<String> = MerkleTree::from_leaves(&words).get_siblings(0)[..level]
        .iter()
        .map(|w| format!("0x{}", hex::encode(w.0)))
        .collect();
    let epoch_before = view(&s, "retirement_epoch", json!({})).await;
    let retired = s
        .alice
        .call(s.vault.id(), "retire_closed_leaf")
        .args_json(json!({ "note_id": 0, "siblings": siblings }))
        .max_gas()
        .transact()
        .await
        .unwrap();
    let tgas_retire = retired.total_gas_burnt.as_gas() as f64 / 1e12;
    retired.into_result().unwrap();
    s.worker.fast_forward(5).await.unwrap();
    assert_eq!(
        leaves(&s).await[0],
        format!("0x{}", "00".repeat(32)),
        "the closed note's leaf is zero"
    );
    assert_ne!(
        view(&s, "current_root", json!({})).await,
        root,
        "the root moved"
    );
    assert_ne!(view(&s, "retirement_epoch", json!({})).await, epoch_before);

    // 13. A new lease on that note is refused: the wallet has no note to lease on, and the
    // vault will not retire the leaf twice.
    let refused = s
        .gateway
        .chat(request("again"), ChatOptions::default())
        .await
        .err()
        .expect("a closed note opens no lease");
    assert_eq!(refused.status.as_u16(), 402);
    assert!(
        refused.message.contains("no accepted note"),
        "{}",
        refused.message
    );
    assert_eq!(
        s.fake.handle.chats().len(),
        1,
        "nothing more reached the provider"
    );
    let twice = s
        .alice
        .call(s.vault.id(), "retire_closed_leaf")
        .args_json(json!({ "note_id": 0, "siblings": [] }))
        .max_gas()
        .transact()
        .await
        .unwrap();
    assert!(twice.is_failure());

    println!(
        "golden path gas (Tgas): deposit {tgas_deposit:.1}, mutual_close {tgas_close:.1}, claim_treasury {tgas_claim:.1}, retire_closed_leaf {tgas_retire:.1}; charge {} of {} base units",
        done.charge, ONE
    );
}
