//! Deposit against a real NEAR sandbox node, the real vault and the mock USDC.

use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use near_workspaces::types::{AccessKey, NearToken, PublicKey};
use near_workspaces::{Account, Contract};
use nymira_operator::clock::{Clock, ManualClock};
use nymira_operator::store::Store;
use nymira_operator::tx::{NearTx, TxConfig, TxRequest};
use nymira_testkit::build::contract_wasm_with_features;
use nymira_testkit::mock_usdc::MockUsdc;
use nymira_testkit::proofs::{canonical_verifying_key, signing_key, Kind};
use nymirad::backup;
use nymirad::deposit::{short_hint, Confirm, Ctx, DepositError, Summary, GAS_TRANSFER_CALL};
use nymirad::mode::Mode;
use nymirad::pins::{read_live, Pins, Tripwire};
use nymirad::rpc::{Rpc, Transport};
use nymirad::secrets::{new_secret, NoteEntry, NoteStatus, NoteStore};
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

struct Answer(bool);

impl Confirm for Answer {
    fn confirm(&self, _: &Summary) -> bool {
        self.0
    }
}

struct Env {
    _worker: near_workspaces::Worker<near_workspaces::network::Sandbox>,
    usdc: MockUsdc,
    vault: Contract,
    alice: Account,
    dir: tempfile::TempDir,
    key: SigningKey,
    rpc_url: String,
    clock: Arc<ManualClock>,
}

async fn env() -> Env {
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

    let key = SigningKey::from_bytes(&[21u8; 32]);
    let public: PublicKey = format!(
        "ed25519:{}",
        bs58::encode(key.verifying_key().as_bytes()).into_string()
    )
    .parse()
    .unwrap();
    alice
        .batch(alice.id())
        .add_key(public, AccessKey::full_access())
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    // The new key shows in a final block only a few blocks later.
    worker.fast_forward(5).await.unwrap();
    Env {
        rpc_url: worker.rpc_addr(),
        _worker: worker,
        usdc,
        vault,
        alice,
        dir: tempfile::tempdir().unwrap(),
        key,
        clock: Arc::new(ManualClock::new(1_900_000_000_000)),
    }
}

struct Built {
    ctx: Ctx,
    identity: String,
}

async fn build(e: &Env, mode: Mode, pins: Option<Pins>) -> Built {
    let rpc = Rpc::new(e.rpc_url.clone(), &Transport::Direct).unwrap();
    let pins = match pins {
        Some(p) => p,
        None => read_live(&rpc, e.vault.id().as_str()).await.unwrap(),
    };
    let tx_store = Store::open(&e.dir.path().join("tx.db")).unwrap();
    let tx = NearTx::new(
        e.rpc_url.clone(),
        e.alice.id().as_str(),
        e.usdc.contract.id().as_str(),
        e.key.clone(),
        tx_store.clone(),
        e.clock.clone(),
    )
    .with_deposit(1)
    .with_config(TxConfig {
        send_timeout: Duration::from_secs(60),
        retry_delay: Duration::from_millis(200),
        max_attempts: 5,
    });
    let (identity, recipient) = backup::generate_identity();
    Built {
        identity: identity.to_string(),
        ctx: Ctx {
            mode,
            rpc,
            pins,
            tripwire: Arc::new(Tripwire::default()),
            notes: NoteStore::open(&e.dir.path().join("notes.sealed"), "pass", 10).unwrap(),
            tx,
            tx_store,
            sender: e.alice.id().to_string(),
            clock: e.clock.clone(),
            backup: Some(backup::Target::new(
                backup::parse_recipient(&recipient).unwrap(),
                vec![e.dir.path().join("b1"), e.dir.path().join("b2")],
            )),
            calendar_dir: e.dir.path().join("cal"),
        },
    }
}

fn files_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(files_under(&path));
        } else {
            out.push(path);
        }
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deposit_is_confirmed_from_its_own_receipt_and_leaves_no_plain_secret() {
    let e = env().await;
    let b = build(&e, Mode::Mainnet, None).await;
    let accepted = b.ctx.deposit(ONE, None, &Answer(true)).await.unwrap();
    assert_eq!(accepted.note_id, 0);
    assert_eq!(accepted.deposit_amount, ONE);
    // The vault holds the denomination plus the fee; alice paid exactly that.
    assert_eq!(e.usdc.balance_of(e.alice.id()).await, 10 * ONE - ONE - FEE);

    let notes = b.ctx.notes.list().unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].status, NoteStatus::Accepted);
    assert_eq!(
        notes[0].payout,
        e.alice.id().to_string(),
        "payout defaults to the sender"
    );
    let secret = notes[0].secret.clone();
    assert_eq!(notes[0].expiry_ns, Some(accepted.expiry_secs));
    // What the CLI prints: the note, its expiry, the calendar file and the wait hint, in that order.
    let report = accepted.report();
    let lines: Vec<&str> = report.lines().collect();
    assert_eq!(lines.len(), 3, "{report}");
    assert_eq!(
        lines[0],
        format!(
            "Note 0 accepted. Expires at Unix time {}.",
            accepted.expiry_secs
        )
    );
    assert_eq!(
        lines[1],
        format!("Calendar file: {}", accepted.calendar.display())
    );
    assert_eq!(lines[2], nymirad::deposit::WAIT_HINT);

    // The calendar file carries the vault's expiry.
    let ics = std::fs::read_to_string(&accepted.calendar).unwrap();
    assert!(ics.contains("BEGIN:VEVENT") && ics.contains("SUMMARY:Nymira note 0 expires"));

    // Both backup copies open with the private key, and only with it.
    for place in ["b1", "b2"] {
        let bytes = std::fs::read(e.dir.path().join(place).join("notes.age")).unwrap();
        let plain = backup::decrypt(&b.identity, &bytes).unwrap();
        assert!(String::from_utf8_lossy(&plain).contains(&notes[0].commitment));
        let (other, _) = backup::generate_identity();
        assert!(backup::decrypt(&other, &bytes).is_err());
    }

    // No file in the state directory holds the secret in the clear, and neither does an error
    // or a debug print.
    let needle = secret.as_bytes();
    for file in files_under(e.dir.path()) {
        let bytes = std::fs::read(&file).unwrap();
        if file.ends_with("notes.age") || bytes.len() < needle.len() {
            continue;
        }
        assert!(
            !bytes.windows(needle.len()).any(|w| w == needle),
            "{} holds the secret",
            file.display()
        );
    }
    assert!(!format!("{:?}", notes[0]).contains(&secret));
    let refused = b
        .ctx
        .deposit(ONE + 1, None, &Answer(true))
        .await
        .unwrap_err();
    assert!(!refused.to_string().contains(&secret));
    assert!(NoteStore::open(&e.dir.path().join("notes.sealed"), "wrong", 10).is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_named_payout_account_is_shown_as_a_fingerprint_and_recorded() {
    let e = env().await;
    let b = build(&e, Mode::Mainnet, None).await;
    struct Check;
    impl Confirm for Check {
        fn confirm(&self, s: &Summary) -> bool {
            assert_eq!(s.payout, "bob.test");
            assert_eq!(
                s.payout_fingerprint,
                nymirad::deposit::fingerprint("bob.test")
            );
            assert_eq!(s.total, ONE + FEE);
            true
        }
    }
    b.ctx.deposit(ONE, Some("bob.test"), &Check).await.unwrap();
    assert_eq!(b.ctx.notes.list().unwrap()[0].payout, "bob.test");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_vault_stops_the_deposit_before_anything_is_sent() {
    let e = env().await;
    let rpc = Rpc::new(e.rpc_url.clone(), &Transport::Direct).unwrap();
    let mut pins = read_live(&rpc, e.vault.id().as_str()).await.unwrap();
    for (what, change) in [
        (
            "config.note_fee",
            Box::new(|p: &mut Pins| p.config["note_fee"] = json!("1")) as Box<dyn Fn(&mut Pins)>,
        ),
        (
            "code hash",
            Box::new(|p: &mut Pins| p.code_hash = "x".into()),
        ),
        (
            "verifying key hashes",
            Box::new(|p: &mut Pins| p.vk_hashes["request"] = json!("0x00")),
        ),
        (
            "access keys",
            Box::new(|p: &mut Pins| p.access_keys.push(json!({"public_key": "ed25519:x"}))),
        ),
        (
            "config.owner",
            Box::new(|p: &mut Pins| p.config["owner"] = json!("evil.near")),
        ),
    ] {
        let original = pins.clone();
        change(&mut pins);
        let b = build(&e, Mode::Mainnet, Some(pins.clone())).await;
        let err = b.ctx.deposit(ONE, None, &Answer(true)).await.unwrap_err();
        match err {
            DepositError::Tripwire(diffs) => {
                assert!(diffs.contains(&what.to_string()), "{what}: {diffs:?}")
            }
            other => panic!("{what}: {other}"),
        }
        assert!(b.ctx.tripwire.tripped().is_some());
        assert!(b.ctx.notes.list().unwrap().is_empty(), "no secret was made");
        assert_eq!(
            e.usdc.balance_of(e.alice.id()).await,
            10 * ONE,
            "nothing was sent"
        );
        pins = original;
        std::fs::remove_file(e.dir.path().join("notes.sealed")).ok();
    }
    // No vault is pinned in this build for the testnet profile yet.
    let tripwire = Tripwire::default();
    let diffs = tripwire
        .check(Mode::Testnet, &rpc, e.vault.id().as_str())
        .await
        .unwrap_err();
    assert!(diffs[0].contains("no vault is pinned"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_testnet_profile_takes_only_circles_usdc_and_a_short_balance_points_to_the_faucet() {
    let e = env().await;
    let b = build(&e, Mode::Testnet, None).await;
    let err = b.ctx.deposit(ONE, None, &Answer(true)).await.unwrap_err();
    assert!(matches!(err, DepositError::WrongToken { .. }), "{err}");
    assert!(short_hint(Mode::Testnet).contains("faucet.circle.com"));
    assert!(short_hint(Mode::Mainnet).is_empty());

    // Short balance, on a profile that does not fix the token: registered, but empty.
    let mut poor = build(&e, Mode::Mainnet, None).await.ctx;
    let broke = e._worker.dev_create_account().await.unwrap();
    e.usdc.register(&broke).await;
    e._worker.fast_forward(5).await.unwrap();
    poor.sender = broke.id().to_string();
    let err = poor.deposit(ONE, None, &Answer(true)).await.unwrap_err();
    assert!(
        matches!(err, DepositError::ShortBalance { have: 0, need, .. } if need == ONE + FEE),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_without_storage_deposit_is_reported_before_the_transfer() {
    let e = env().await;
    let b = build(&e, Mode::Mainnet, None).await;
    let mut ctx = b.ctx;
    ctx.sender = "nobody.test".into();
    let err = ctx.deposit(ONE, None, &Answer(true)).await.unwrap_err();
    assert!(
        matches!(err, DepositError::NotRegistered(ref a) if a == "nobody.test"),
        "{err}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_declined_confirmation_stores_nothing() {
    let e = env().await;
    let b = build(&e, Mode::Mainnet, None).await;
    let err = b.ctx.deposit(ONE, None, &Answer(false)).await.unwrap_err();
    assert!(matches!(err, DepositError::Declined));
    assert!(b.ctx.notes.list().unwrap().is_empty());
    assert_eq!(e.usdc.balance_of(e.alice.id()).await, 10 * ONE);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_transfer_the_vault_refunds_leaves_a_failed_note_and_no_money_spent() {
    let e = env().await;
    let b = build(&e, Mode::Mainnet, None).await;
    // The vault refuses itself as payout and returns the amount.
    let err = b
        .ctx
        .deposit(ONE, Some(e.vault.id().as_str()), &Answer(true))
        .await
        .unwrap_err();
    assert!(matches!(err, DepositError::Refunded), "{err}");
    assert_eq!(b.ctx.notes.list().unwrap()[0].status, NoteStatus::Failed);
    assert_eq!(e.usdc.balance_of(e.alice.id()).await, 10 * ONE);
}

#[tokio::test(flavor = "multi_thread")]
async fn after_a_crash_the_note_is_found_again_from_its_deposit_receipt() {
    let e = env().await;
    let b = build(&e, Mode::Mainnet, None).await;
    // The process died after the secret was sealed and the transfer signed, before it was sent.
    let (secret, commitment) = new_secret(Mode::Mainnet.chain_label());
    let commitment = format!("0x{}", hex::encode(commitment.0));
    b.ctx
        .notes
        .add(NoteEntry {
            chain_label: Mode::Mainnet.chain_label().into(),
            secret: hex::encode(secret.0),
            commitment: commitment.clone(),
            denomination: ONE.to_string(),
            payout: e.alice.id().to_string(),
            status: NoteStatus::Pending,
            note_id: None,
            expiry_ns: None,
            deposit_amount: None,
            tx_hash: None,
            created_at: e.clock.now_secs(),
        })
        .unwrap();
    // And a second pending note whose transfer was never signed.
    let (lost, lost_commitment) = new_secret(Mode::Mainnet.chain_label());
    let lost_commitment = format!("0x{}", hex::encode(lost_commitment.0));
    b.ctx
        .notes
        .add(NoteEntry {
            chain_label: Mode::Mainnet.chain_label().into(),
            secret: hex::encode(lost.0),
            commitment: lost_commitment.clone(),
            denomination: ONE.to_string(),
            payout: e.alice.id().to_string(),
            status: NoteStatus::Pending,
            note_id: None,
            expiry_ns: None,
            deposit_amount: None,
            tx_hash: None,
            created_at: e.clock.now_secs(),
        })
        .unwrap();
    let msg = json!({ "commitment": commitment, "withdraw_to": e.alice.id() }).to_string();
    b.ctx
        .tx
        .submit(TxRequest {
            kind: "deposit",
            method: "ft_transfer_call",
            args: json!({ "receiver_id": e.vault.id(), "amount": (ONE + FEE).to_string(), "msg": msg })
                .to_string()
                .into_bytes(),
            gas: GAS_TRANSFER_CALL,
            idem_key: Some(commitment.clone()),
        })
        .await
        .unwrap();
    assert_eq!(
        e.usdc.balance_of(e.alice.id()).await,
        10 * ONE,
        "not sent yet"
    );

    let results = b.ctx.recover().await.unwrap();
    assert_eq!(results.len(), 2);
    let ok = results.iter().find(|(k, _)| *k == commitment).unwrap();
    assert_eq!(ok.1.as_ref().unwrap().note_id, 0);
    let lost = results.iter().find(|(k, _)| *k == lost_commitment).unwrap();
    assert!(lost.1.is_err());
    let notes = b.ctx.notes.list().unwrap();
    assert_eq!(
        notes
            .iter()
            .filter(|n| n.status == NoteStatus::Accepted)
            .count(),
        1
    );
    assert_eq!(
        notes
            .iter()
            .filter(|n| n.status == NoteStatus::Failed)
            .count(),
        1
    );
    assert_eq!(e.usdc.balance_of(e.alice.id()).await, 10 * ONE - ONE - FEE);
}
