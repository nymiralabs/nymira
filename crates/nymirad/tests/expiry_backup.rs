//! The expiry guard and the backup, against a sandbox vault whose notes live four days (so every
//! note is inside the 5-day window from the start) or thirty (outside it), the real operator over
//! it, the fake NEAR AI gateway behind that, and `nymirad`'s gateway in front.
//!
//! The guard takes the time it decides by as an argument (`guard_tick_at`), so a note's expiry can
//! be brought closer without moving the clocks the proofs depend on. The transactions it sends
//! are real.

mod support;

use std::sync::Arc;
use std::time::Duration;

use near_workspaces::types::{AccessKey, NearToken, PublicKey};
use near_workspaces::{Account, Contract};
use nymira_fake_near_ai::{Running, TestPolicy};
use nymira_near_codec::{chain_id, contract_address, Network};
use nymira_operator::alarm::Alarms;
use nymira_operator::chain::RpcVault;
use nymira_operator::clock::{Clock, ManualClock};
use nymira_operator::closer::{Closer, FakeCloseSender};
use nymira_operator::nearai::{NearAiAdapter, NearAiConfig};
use nymira_operator::operator::{Config as OpConfig, Operator, Parts};
use nymira_operator::proxy::ProxyConfig;
use nymira_operator::replica::NoReplica;
use nymira_operator::sealed::SealedFile;
use nymira_operator::signer::{FakeHeartbeat, LocalSigner, Signer};
use nymira_operator::store::Store;
use nymira_operator::tx::TxConfig;
use nymira_operator::usage::Minter;
use nymira_operator::verify::{GrothVerifier, GrothWithdrawalVerifier};
use nymira_testkit::mock_usdc::MockUsdc;
use nymira_testkit::vault::DevVault;
use nymirad::attest::{Policy, TestEvidence};
use nymirad::backup::{self, StateBackup, Target};
use nymirad::expiry::{Event, GuardConfig, DAY};
use nymirad::gateway::close::CloseOptions;
use nymirad::gateway::{ChatOptions, Gateway, GatewayConfig, Outcome};
use nymirad::pins::read_live;
use nymirad::rpc::{Rpc, Transport};
use nymirad::secrets::{new_secret, NoteEntry, NoteStatus, NoteStore};
use nymirad::submitter::METHODS;
use serde_json::{json, Value};
use zkapi_proof::compact::CompactSigner;
use zkapi_types::Felt252;

const SETUP_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/zkapi/protocol/setup/v2-concession-dev"
);
const ONE: u128 = 1_000_000;
const FEE: u128 = 50_000;
const MODEL: &str = "z-ai/glm-5.3-flash";
const PASS: &str = "test-passphrase";

/// What it takes to build a gateway over some state directory.
struct Wiring {
    operator_url: String,
    gateway_url: String,
    rpc_url: String,
    vault_id: String,
}

struct Stack {
    worker: near_workspaces::Worker<near_workspaces::network::Sandbox>,
    vault: Contract,
    usdc: MockUsdc,
    alice: Account,
    _fake: Running,
    clock: Arc<ManualClock>,
    op: Arc<Operator>,
    gateway: Arc<Gateway>,
    home: tempfile::TempDir,
    secrets: Vec<String>,
    wiring: Wiring,
}

struct Opts {
    /// The vault's note lifetime in days.
    ttl_days: u64,
    guard: GuardConfig,
    backup: Option<Target>,
}

fn guard(tune: impl FnOnce(&mut GuardConfig)) -> GuardConfig {
    let mut g = GuardConfig {
        close: CloseOptions {
            delay: Duration::ZERO,
            polls: 2,
            poll_every: Duration::from_millis(20),
            ..CloseOptions::default()
        },
        retry_secs: 0,
        end_wait_secs: 0,
        ..GuardConfig::default()
    };
    tune(&mut g);
    g
}

fn tx_config() -> TxConfig {
    TxConfig {
        send_timeout: Duration::from_secs(60),
        retry_delay: Duration::from_millis(200),
        max_attempts: 5,
    }
}

async fn deposit(
    usdc: &MockUsdc,
    alice: &Account,
    vault: &Contract,
    notes: &NoteStore,
    n: u64,
    clock: &ManualClock,
) -> String {
    let (secret, commitment) = new_secret("near-testnet");
    let commitment_hex = format!("0x{}", hex::encode(commitment.0));
    let msg = json!({ "commitment": commitment_hex }).to_string();
    usdc.ft_transfer_call(alice, vault.id(), ONE + FEE, &msg)
        .await
        .unwrap();
    let records: Value = vault
        .view("note_records")
        .args_json(json!({ "from": n, "limit": 1 }))
        .await
        .unwrap()
        .json()
        .unwrap();
    let rec = &records[0];
    let secret_hex = hex::encode(secret.0);
    notes
        .add(NoteEntry {
            chain_label: "near-testnet".into(),
            secret: secret_hex.clone(),
            commitment: commitment_hex,
            denomination: ONE.to_string(),
            payout: alice.id().to_string(),
            status: NoteStatus::Accepted,
            note_id: Some(rec["note_id"].as_u64().unwrap() as u32),
            expiry_ns: rec["expiry"].as_u64(),
            deposit_amount: rec["deposit_amount"].as_str().map(String::from),
            tx_hash: None,
            created_at: clock.now_secs(),
        })
        .unwrap();
    secret_hex
}

/// A gateway over `home`, sealed under `passphrase`.
async fn make_gateway(
    home: &std::path::Path,
    passphrase: &str,
    wiring: &Wiring,
    clock: Arc<ManualClock>,
    opts: Opts,
) -> Arc<Gateway> {
    use sha2::{Digest, Sha256};
    let rpc = Rpc::new(wiring.rpc_url.clone(), &Transport::Direct).unwrap();
    let pins = read_live(&rpc, &wiring.vault_id).await.unwrap();
    let signer_key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let test = TestPolicy::fake();
    let policy = Policy {
        version: 1,
        attestation_root_pk: test.attestation_root_pk,
        accepted_measurements: test.accepted_measurements,
        deployments: vec![],
        accepted_tcb_statuses: test.accepted_tcb_statuses,
        signature: String::new(),
    }
    .sign(&signer_key);
    let policy = Policy::load(
        &serde_json::to_vec(&policy).unwrap(),
        &signer_key.verifying_key().to_bytes(),
        1,
    )
    .unwrap();
    let config = GatewayConfig {
        home: home.to_path_buf(),
        operator_url: wiring.operator_url.clone(),
        gateway_url: wiring.gateway_url.clone(),
        vault: wiring.vault_id.clone(),
        network: Network::Testnet,
        setup_dir: SETUP_DIR.into(),
        proving_key_sha256: hex::encode(Sha256::digest(
            std::fs::read(format!("{SETUP_DIR}/request.pk")).unwrap(),
        )),
        models: ProxyConfig::default()
            .models
            .into_iter()
            .filter(|m| m.id == MODEL)
            .collect(),
        rounds: 10,
        idle_secs: 60,
        share_pct: 90,
        prewarm_max_secs: 0,
        backup: opts.backup,
    };
    let dyn_clock: Arc<dyn Clock> = clock;
    Arc::new(
        Gateway::new(
            config,
            Transport::Direct,
            rpc,
            pins,
            NoteStore::open(&home.join("notes.sealed"), passphrase, 10).unwrap(),
            passphrase,
            Arc::new(TestEvidence { policy }),
            dyn_clock,
        )
        .unwrap()
        .with_tx_config(tx_config())
        .with_guard(opts.guard),
    )
}

async fn stack(opts: Opts) -> Stack {
    let worker = near_workspaces::sandbox().await.unwrap();
    let usdc = MockUsdc::deploy(&worker).await.unwrap();
    let operator_account = worker.dev_create_account().await.unwrap();
    let treasury = worker.dev_create_account().await.unwrap();
    let alice = worker.dev_create_account().await.unwrap();
    let vault = DevVault {
        note_ttl_days: opts.ttl_days,
        bound_classes: &["100000", "500000"],
    }
    .deploy(&worker, &usdc, &operator_account, &treasury)
    .await;
    usdc.register(&alice).await;
    usdc.mint(alice.id(), 20 * ONE).await;

    let home = tempfile::tempdir().unwrap();
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let clock = Arc::new(ManualClock::new(wall * 1000));
    let notes = NoteStore::open(&home.path().join("notes.sealed"), PASS, 10).unwrap();
    let secrets = vec![deposit(&usdc, &alice, &vault, &notes, 0, &clock).await];
    drop(notes);
    for _ in 0..5 {
        let at = nymira_testkit::clock::now_seconds(&worker).await + 40;
        if nymira_testkit::clock::advance_to(&worker, at).await.is_ok() {
            break;
        }
    }
    alice
        .call(vault.id(), "sync_root")
        .args_json(json!({ "max": 100 }))
        .max_gas()
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    worker.fast_forward(5).await.unwrap();

    let fake = nymira_fake_near_ai::start("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let dyn_clock: Arc<dyn Clock> = clock.clone();
    let alarms = Arc::new(Alarms::new(vec![], dyn_clock.clone()));
    let sealed = |name: &str| SealedFile::open(&home.path().join(name), PASS, 10).unwrap();
    let provider = Arc::new(
        NearAiAdapter::new(
            NearAiConfig {
                user_agent: "test".into(),
                base_url: fake.url(),
                org_id: "org".into(),
                workspace_id: "ws".into(),
            },
            dyn_clock.clone(),
            alarms.clone(),
            sealed("operator-login.enc"),
            sealed("operator-keys.enc"),
            Some(nymira_fake_near_ai::FIRST_REFRESH_TOKEN),
        )
        .unwrap(),
    );
    let vault_view = Arc::new(RpcVault::new(worker.rpc_addr(), vault.id().to_string()));
    let network = Network::Testnet;
    let contract = Felt252(contract_address(vault.id().as_str()));
    let withdrawal =
        Arc::new(GrothWithdrawalVerifier::load(std::path::Path::new(SETUP_DIR)).unwrap());
    let closer = Arc::new(Closer::new(
        vault_view.clone(),
        withdrawal.clone(),
        Arc::new(FakeCloseSender::with_clock(dyn_clock.clone())),
        dyn_clock.clone(),
        alarms.clone(),
        chain_id(network),
        contract,
    ));
    let store = Store::open(&home.path().join("operator.db")).unwrap();
    let minter = Minter::generate();
    let signer = Arc::new(
        Signer::new(
            CompactSigner::from_seed(&Felt252::from_u64(1)),
            CompactSigner::from_seed(&Felt252::from_u64(2)),
            Store::open(&home.path().join("operator.db")).unwrap(),
            vault_view.clone(),
            minter.public_key(),
            chain_id(network),
            contract,
            dyn_clock.clone(),
            Arc::new(FakeHeartbeat::default()),
        )
        .unwrap(),
    );
    signer.spawn_background();
    let mut cfg = OpConfig::new(
        vault.id().as_str(),
        chain_id(network),
        contract,
        "http://localhost",
    );
    cfg.host = "localhost".into();
    cfg.proxy.models.retain(|m| m.id == MODEL);
    let op = Operator::start(Parts {
        cfg,
        clock: dyn_clock.clone(),
        vault: vault_view,
        provider,
        replica: Arc::new(NoReplica),
        verifier: Arc::new(GrothVerifier::load(std::path::Path::new(SETUP_DIR)).unwrap()),
        withdrawal_verifier: withdrawal,
        closer,
        store,
        state_signer: Arc::new(CompactSigner::from_seed(&Felt252::from_u64(1))),
        signer: Arc::new(LocalSigner(signer)),
        minter: Arc::new(minter),
        alarms,
    })
    .await
    .unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let operator_url = format!("http://{}", listener.local_addr().unwrap());
    let app = nymira_operator::http::router(op.clone());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let wiring = Wiring {
        operator_url,
        gateway_url: fake.url(),
        rpc_url: worker.rpc_addr(),
        vault_id: vault.id().to_string(),
    };
    let gateway = make_gateway(home.path(), PASS, &wiring, clock.clone(), opts).await;
    // The operator limits an ownership proof to 3,300 s from its own clock, with no slack, so the
    // test clock runs a little ahead of the wallet's.
    let real = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    clock.advance_secs(real.saturating_sub(clock.now_secs()) + 30);
    Stack {
        worker,
        vault,
        usdc,
        alice,
        _fake: fake,
        clock,
        op,
        gateway,
        home,
        secrets,
        wiring,
    }
}

/// A submitter account for the family `personal`, funded with `near`: its key lists the four
/// vault methods.
async fn submitter(s: &Stack, gw: &Gateway, name: &str, near: NearToken) -> Account {
    let account = s
        .worker
        .root_account()
        .unwrap()
        .create_subaccount(name)
        .initial_balance(near)
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let made = gw
        .submitters()
        .add("personal", account.id().as_str())
        .unwrap();
    let public: PublicKey = made.public_key().parse().unwrap();
    account
        .batch(account.id())
        .add_key(
            public,
            AccessKey::function_call_access(s.vault.id(), &METHODS, None),
        )
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    s.worker.fast_forward(5).await.unwrap();
    account
}

fn request(text: &str) -> Value {
    json!({"model": MODEL, "stream": false, "messages": [{"role": "user", "content": text}]})
}

async fn chat_on(gw: &Arc<Gateway>, text: &str) -> Result<Value, nymirad::openai::ApiError> {
    match gw.chat(request(text), ChatOptions::default()).await? {
        Outcome::Json(v, _) => Ok(v),
        Outcome::Stream(..) => panic!("expected JSON"),
    }
}

/// Retires, lets the operator settle (it waits 10 s after revoking), and settles on our side.
async fn settle_on(s: &Stack, gw: &Arc<Gateway>) -> Vec<nymirad::gateway::Settled> {
    gw.reap(true).await;
    for _ in 0..3 {
        s.op.tick().await;
        s.clock.advance_secs(11);
    }
    s.op.tick().await;
    s.op.tick().await;
    gw.settle().await
}

async fn note_status(s: &Stack) -> Value {
    let commitment = entry(s).commitment.clone();
    s.vault
        .view("note_status")
        .args_json(json!({ "commitment": commitment }))
        .await
        .unwrap()
        .json()
        .unwrap()
}

fn entry(s: &Stack) -> NoteEntry {
    NoteStore::open(&s.home.path().join("notes.sealed"), PASS, 10)
        .unwrap()
        .list()
        .unwrap()
        .remove(0)
}

async fn exit_requests(s: &Stack) -> Vec<Value> {
    s.vault
        .view("exit_requests")
        .args_json(json!({ "from_note_id": 0, "limit": 10 }))
        .await
        .unwrap()
        .json()
        .unwrap()
}

// ---- the expiry guard ---------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn a_four_day_note_takes_no_lease_is_closed_by_the_guard_and_paid_out() {
    let s = stack(Opts {
        ttl_days: 4,
        guard: guard(|_| {}),
        backup: None,
    })
    .await;
    submitter(&s, &s.gateway, "subclose", NearToken::from_near(5)).await;
    let expiry = entry(&s).expiry_ns.unwrap();
    let left = expiry - s.clock.now_secs();
    assert!(
        (4 * DAY - 120..5 * DAY).contains(&left),
        "the sandbox vault's note lives {left} s, not four to five days"
    );

    // No new lease: the vault's own short TTL puts the note inside the 5-day window.
    let refused = chat_on(&s.gateway, "hello").await.unwrap_err();
    assert_eq!(refused.status.as_u16(), 402);
    assert!(
        refused.message.contains("expires within 5 days"),
        "{}",
        refused.message
    );

    // The guard closes it: the payout account gets the denomination, the vault shows it closed.
    let before = s.usdc.balance_of(s.alice.id()).await;
    let events = s.gateway.guard_tick().await;
    assert!(events.contains(&Event::Closed(0)), "{events:?}");
    assert_eq!(s.usdc.balance_of(s.alice.id()).await, before + ONE);
    assert_eq!(note_status(&s).await, "Closed");
    assert_eq!(entry(&s).status, NoteStatus::Closed);
    // Nothing further to do on the next pass.
    assert!(s.gateway.guard_tick().await.is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_close_becomes_a_request_exit_at_three_days_and_the_emergency_stops_leases() {
    let s = stack(Opts {
        ttl_days: 4,
        guard: guard(|_| {}),
        backup: None,
    })
    .await;
    submitter(&s, &s.gateway, "subexit", NearToken::from_near(5)).await;
    let expiry = entry(&s).expiry_ns.unwrap();

    // The operator refuses clearances while in emergency, so the close cannot go through.
    s.op.set_emergency("the clearance key may be exposed")
        .await
        .unwrap();
    let events = s.gateway.guard_tick().await;
    assert!(
        events.iter().any(|e| matches!(e, Event::Emergency(t) if t.contains("nymirad exit --note ID") && t.contains("IDs: 0"))),
        "{events:?}"
    );
    assert!(
        events.iter().any(|e| matches!(e, Event::CloseFailed(0, _))),
        "close attempted at 5 days: {events:?}"
    );
    // More than three days are left: no exit yet.
    assert_eq!(note_status(&s).await, "Active");
    assert!(exit_requests(&s).await.is_empty());
    assert!(!events.iter().any(|e| matches!(e, Event::ExitFiled(_))));

    // New leases stop on the notice (the daemon read it above) and say what to do.
    assert!(s.gateway.emergency().is_some());
    let refused = chat_on(&s.gateway, "hello").await.unwrap_err();
    assert_eq!(refused.code, "operator_emergency");
    assert!(
        refused.message.contains("nymirad exit --note ID"),
        "{}",
        refused.message
    );

    // Three days before expiry the failed close turns into request_exit.
    let events = s.gateway.guard_tick_at(expiry - 2 * DAY - DAY / 2).await;
    assert!(events.contains(&Event::ExitFiled(0)), "{events:?}");
    assert_eq!(note_status(&s).await, "PendingExit");
    let exits = exit_requests(&s).await;
    assert_eq!(exits.len(), 1);
    assert_eq!(entry(&s).status, NoteStatus::Exiting);
    // The exit is filed once.
    assert!(s
        .gateway
        .guard_tick_at(expiry - 2 * DAY)
        .await
        .iter()
        .all(|e| !matches!(e, Event::ExitFiled(_) | Event::ExitFailed(..))));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_emergency_stops_new_leases_before_a_proof_is_sent_and_clears_with_it() {
    let s = stack(Opts {
        ttl_days: 30,
        guard: guard(|_| {}),
        backup: None,
    })
    .await;
    // The daemon has not polled yet: the lease path itself sees the notice in the status.
    s.op.set_emergency("the state key may be exposed")
        .await
        .unwrap();
    let refused = chat_on(&s.gateway, "hello").await.unwrap_err();
    assert_eq!(refused.code, "operator_emergency");
    // No proof left, so the note can still prove: the next lease, once the notice is gone, works.
    s.op.clear_emergency().await.unwrap();
    assert_eq!(s.gateway.refresh_emergency().await, None);
    let answer = chat_on(&s.gateway, "after the all-clear").await.unwrap();
    assert!(answer["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .contains("after the all-clear"));
}

#[tokio::test(flavor = "multi_thread")]
async fn notices_come_at_14_7_and_2_days_once_each_and_a_session_end_close_is_a_switch() {
    let s = stack(Opts {
        ttl_days: 30,
        guard: guard(|g| g.close_at_session_end = true),
        backup: None,
    })
    .await;
    submitter(&s, &s.gateway, "subnotice", NearToken::from_near(5)).await;
    let expiry = entry(&s).expiry_ns.unwrap();
    let notices = |events: &[Event]| -> Vec<String> {
        events
            .iter()
            .filter_map(|e| match e {
                Event::Notice(t) => Some(t.clone()),
                _ => None,
            })
            .collect()
    };

    // Outside the window, nothing is closed, and a note with 30 days left has no notice.
    assert!(s.gateway.guard_tick().await.is_empty());
    let at14 = notices(&s.gateway.guard_tick_at(expiry - 13 * DAY).await);
    assert_eq!(at14.len(), 1);
    assert!(at14[0].contains("14-day notice"), "{at14:?}");
    assert!(notices(&s.gateway.guard_tick_at(expiry - 12 * DAY).await).is_empty());
    let at7 = notices(&s.gateway.guard_tick_at(expiry - 6 * DAY).await);
    assert!(at7.len() == 1 && at7[0].contains("7-day notice"), "{at7:?}");
    // 2 days left: the notice, and the guard acts too (the close is tried; the sandbox clock is
    // not moved, so it lands).
    let at2 = s.gateway.guard_tick_at(expiry - DAY - DAY / 2).await;
    assert!(
        notices(&at2).iter().any(|n| n.contains("2-day notice")),
        "{at2:?}"
    );
    assert!(at2.contains(&Event::Closed(0)), "{at2:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_at_the_end_of_a_session_is_off_by_default_and_on_when_asked() {
    // Default: the session ends and the note stays open.
    let off = stack(Opts {
        ttl_days: 30,
        guard: guard(|_| {}),
        backup: None,
    })
    .await;
    submitter(&off, &off.gateway, "suboff", NearToken::from_near(5)).await;
    assert!(!GuardConfig::default().close_at_session_end);
    assert!(off.gateway.end_session().await.is_empty());
    assert_eq!(note_status(&off).await, "Active");
    assert_eq!(entry(&off).status, NoteStatus::Accepted);

    // Asked for: the same end closes the note and pays it out.
    let on = stack(Opts {
        ttl_days: 30,
        guard: guard(|g| g.close_at_session_end = true),
        backup: None,
    })
    .await;
    submitter(&on, &on.gateway, "subon", NearToken::from_near(5)).await;
    let before = on.usdc.balance_of(on.alice.id()).await;
    let events = on.gateway.end_session().await;
    assert_eq!(events, vec![Event::Closed(0)]);
    assert_eq!(on.usdc.balance_of(on.alice.id()).await, before + ONE);
    assert_eq!(note_status(&on).await, "Closed");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_guard_can_be_switched_off() {
    let s = stack(Opts {
        ttl_days: 4,
        guard: guard(|g| g.enabled = false),
        backup: None,
    })
    .await;
    submitter(&s, &s.gateway, "subfree", NearToken::from_near(5)).await;
    let expiry = entry(&s).expiry_ns.unwrap();
    // Inside the 5-day window a lease still opens, and no pass closes, exits or reminds.
    let answer = chat_on(&s.gateway, "inside the window").await.unwrap();
    assert!(answer["choices"][0]["message"]["content"].is_string());
    assert!(s.gateway.guard_tick_at(expiry - DAY).await.is_empty());
    assert!(s.gateway.guard_tick().await.is_empty());
    assert_eq!(entry(&s).status, NoteStatus::Accepted);
    assert!(exit_requests(&s).await.is_empty());
}

// ---- backup and restore --------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread")]
async fn after_every_retire_the_state_is_backed_up_and_restores_on_a_new_machine_to_keep_spending()
{
    let places = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (private, public) = backup::generate_identity();
    let target = Target::new(
        backup::parse_recipient(&public).unwrap(),
        vec![places.0.path().into(), places.1.path().into()],
    );
    let s = stack(Opts {
        ttl_days: 30,
        guard: guard(|_| {}),
        backup: Some(target),
    })
    .await;
    chat_on(&s.gateway, "first").await.unwrap();
    settle_on(&s, &s.gateway).await;

    // Both places hold the same encrypted file; only the private key opens it.
    let (a, b) = (
        std::fs::read(places.0.path().join("state.age")).unwrap(),
        std::fs::read(places.1.path().join("state.age")).unwrap(),
    );
    assert_eq!(a, b);
    assert!(
        backup::decrypt(PASS, &a).is_err(),
        "the machine's passphrase opens nothing"
    );
    let (other, _) = backup::generate_identity();
    assert!(backup::decrypt(&other, &a).is_err());
    let plain = backup::decrypt(&private, &a).unwrap();
    let state = StateBackup::from_bytes(&plain).unwrap();
    assert_eq!(state.notes[0].secret, s.secrets[0]);
    // The lease settled before the backup: the wallet in it holds the next state, not the open lease.
    assert!(state.files.contains_key("lease-records.jsonl"));
    assert!(!String::from_utf8_lossy(&a).contains(&s.secrets[0]));
    // The private key is nowhere on this machine: not in the state directory, not beside the file.
    for dir in [s.home.path(), places.0.path(), places.1.path()] {
        for file in walk(dir) {
            let bytes = std::fs::read(&file).unwrap_or_default();
            assert!(
                !String::from_utf8_lossy(&bytes).contains(private.as_str()),
                "the private key is in {}",
                file.display()
            );
        }
    }

    // A new machine: an empty state directory, only the private key and one place in hand.
    let fresh = tempfile::tempdir().unwrap();
    let loaded = backup::load(&[places.1.path().into()], &private).unwrap();
    let restored = backup::restore_into(fresh.path(), loaded, "a-new-passphrase", 10).unwrap();
    assert_eq!(restored.notes, vec![(Some(0), NoteStatus::Accepted, true)]);
    let gw2 = make_gateway(
        fresh.path(),
        "a-new-passphrase",
        &s.wiring,
        s.clock.clone(),
        Opts {
            ttl_days: 30,
            guard: guard(|_| {}),
            backup: None,
        },
    )
    .await;
    // It spends on the same note, from the state the backup carried.
    let answer = chat_on(&gw2, "second, on the restored note").await.unwrap();
    assert!(answer["choices"][0]["message"]["content"]
        .as_str()
        .unwrap()
        .contains("second, on the restored note"));
    let settled = settle_on(&s, &gw2).await;
    assert_eq!(settled.len(), 1);
    assert_eq!(settled[0].charge, settled[0].expected);
    let leases = gw2.records();
    assert!(leases.iter().all(|r| r.note_id == 0), "{leases:?}");
    assert_eq!(
        leases.len(),
        2,
        "the first lease's record came with the backup"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_passphrase_backup_restores_from_the_passphrase_and_the_file_alone() {
    let place = tempfile::tempdir().unwrap();
    let pass = "an even longer backup passphrase";
    let (kdf, public) = backup::passphrase_setup(pass).unwrap();
    let target = Target::new(
        backup::parse_recipient(&public).unwrap(),
        vec![place.path().into()],
    )
    .with_kdf(kdf);
    let s = stack(Opts {
        ttl_days: 30,
        guard: guard(|_| {}),
        backup: Some(target),
    })
    .await;
    chat_on(&s.gateway, "first").await.unwrap();
    settle_on(&s, &s.gateway).await;

    let file = std::fs::read(place.path().join("state.age")).unwrap();
    assert!(backup::decrypt("not the passphrase at all", &file).is_err());
    let loaded = backup::load(&[place.path().join("state.age")], pass).unwrap();
    let fresh = tempfile::tempdir().unwrap();
    backup::restore_into(fresh.path(), loaded, "pw-for-the-new-machine", 10).unwrap();
    let gw2 = make_gateway(
        fresh.path(),
        "pw-for-the-new-machine",
        &s.wiring,
        s.clock.clone(),
        Opts {
            ttl_days: 30,
            guard: guard(|_| {}),
            backup: None,
        },
    )
    .await;
    chat_on(&gw2, "continuing").await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_note_secret_alone_allows_an_exit_and_never_a_new_request() {
    let s = stack(Opts {
        ttl_days: 30,
        guard: guard(|_| {}),
        backup: None,
    })
    .await;
    let secret = s.secrets[0].clone();
    let fresh = tempfile::tempdir().unwrap();
    let rpc = Rpc::new(s.wiring.rpc_url.clone(), &Transport::Direct).unwrap();
    let now = s.clock.now_secs();
    let restore = |note: u32, secret: String| {
        let (rpc, fresh, vault) = (&rpc, fresh.path().to_path_buf(), s.wiring.vault_id.clone());
        let payout = s.alice.id().to_string();
        async move {
            nymirad::restore::from_secret(
                &fresh,
                rpc,
                &vault,
                "near-testnet",
                &secret,
                note,
                &payout,
                "pw2",
                10,
                now,
            )
            .await
        }
    };
    // The wrong note ID or the wrong secret is not this note.
    assert!(matches!(
        restore(1, secret.clone()).await,
        Err(nymirad::restore::SecretRestoreError::NoNote(1))
    ));
    assert!(matches!(
        restore(0, "11".repeat(32)).await,
        Err(nymirad::restore::SecretRestoreError::NotThisNote(0))
    ));
    let entry = restore(0, secret.clone()).await.unwrap();
    assert_eq!(entry.status, NoteStatus::Exiting);
    // Restoring it twice is refused.
    assert!(matches!(
        restore(0, secret).await,
        Err(nymirad::restore::SecretRestoreError::Known)
    ));

    let gw2 = make_gateway(
        fresh.path(),
        "pw2",
        &s.wiring,
        s.clock.clone(),
        Opts {
            ttl_days: 30,
            guard: guard(|_| {}),
            backup: None,
        },
    )
    .await;
    // No request is ever made on it.
    let refused = chat_on(&gw2, "spend?").await.unwrap_err();
    assert_eq!(refused.status.as_u16(), 402);
    // An exit works, from the state the note had at deposit, conceding nothing.
    submitter(&s, &gw2, "subsecret", NearToken::from_near(5)).await;
    let exiting = gw2
        .exit_note(0, None, None)
        .await
        .expect("the exit is filed");
    assert_eq!(exiting.conceded, 0);
    assert_eq!(note_status(&s).await, "PendingExit");
    // The user is told what an old-state exit risks.
    assert!(backup::SECRET_ONLY_NOTICE.contains("can show a later state and defeat the exit"));
}

fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        if e.path().is_dir() {
            out.extend(walk(&e.path()));
        } else {
            out.push(e.path());
        }
    }
    out
}
