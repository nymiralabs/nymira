//! Close, exit, finalize and claim-payout against a sandbox vault, the real operator (with its
//! closer stubbed) and `nymirad`'s gateway. Chain transactions come from a submitter account
//! whose access key lists the four vault methods and nothing else.

mod support;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
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
use nymira_testkit::build::contract_wasm_with_features;
use nymira_testkit::mock_usdc::MockUsdc;
use nymira_testkit::proofs::{canonical_verifying_key, signing_key, Kind};
use nymirad::attest::{Policy, TestEvidence};
use nymirad::gateway::close::{direct_text, Ask, CloseOptions, LeaveError};
use nymirad::gateway::{ChatOptions, Gateway, GatewayConfig};
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

fn point(seed: u64) -> Value {
    let key = signing_key(seed);
    json!({ "x": format!("0x{}", hex::encode(key.x.0)), "y": format!("0x{}", hex::encode(key.y.0)) })
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

struct Stack {
    worker: near_workspaces::Worker<near_workspaces::network::Sandbox>,
    vault: Contract,
    usdc: MockUsdc,
    alice: Account,
    _fake: Running,
    clock: Arc<ManualClock>,
    op: Arc<Operator>,
    closes: Arc<FakeCloseSender>,
    gateway: Arc<Gateway>,
    home: tempfile::TempDir,
    /// A second gateway over the same home, for the crash test.
    parts: GatewayParts,
}

/// What it takes to build another gateway over the same state.
struct GatewayParts {
    operator_url: String,
    gateway_url: String,
    rpc_url: String,
    vault_id: String,
}

/// Answers every direct-send question the same way and keeps what was asked.
struct Scripted {
    yes: bool,
    asked: Mutex<Vec<String>>,
}

impl Scripted {
    fn new(yes: bool) -> Self {
        Self {
            yes,
            asked: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl Ask for Scripted {
    async fn confirm_direct(&self, text: &str) -> bool {
        self.asked.lock().unwrap().push(text.into());
        self.yes
    }
}

async fn deposit(
    usdc: &MockUsdc,
    alice: &Account,
    vault: &Contract,
    notes: &NoteStore,
    n: u64,
    clock: &ManualClock,
) {
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
    notes
        .add(NoteEntry {
            chain_label: "near-testnet".into(),
            secret: hex::encode(secret.0),
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
}

fn tx_config() -> TxConfig {
    TxConfig {
        send_timeout: Duration::from_secs(60),
        retry_delay: Duration::from_millis(200),
        max_attempts: 5,
    }
}

fn gateway_config(home: &std::path::Path, parts: &GatewayParts) -> GatewayConfig {
    use sha2::{Digest, Sha256};
    GatewayConfig {
        home: home.to_path_buf(),
        operator_url: parts.operator_url.clone(),
        gateway_url: parts.gateway_url.clone(),
        vault: parts.vault_id.clone(),
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
        backup: None,
    }
}

async fn make_gateway(
    home: &std::path::Path,
    parts: &GatewayParts,
    transport: Transport,
    clock: Arc<ManualClock>,
) -> Arc<Gateway> {
    let rpc = Rpc::new(parts.rpc_url.clone(), &Transport::Direct).unwrap();
    let pins = read_live(&rpc, &parts.vault_id).await.unwrap();
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
    let dyn_clock: Arc<dyn Clock> = clock;
    Arc::new(
        Gateway::new(
            gateway_config(home, parts),
            transport,
            rpc,
            pins,
            NoteStore::open(&home.join("notes.sealed"), PASS, 10).unwrap(),
            PASS,
            Arc::new(TestEvidence { policy }),
            dyn_clock,
        )
        .unwrap()
        .with_tx_config(tx_config()),
    )
}

async fn stack(transport: Transport) -> Stack {
    let worker = near_workspaces::sandbox().await.unwrap();
    let usdc = MockUsdc::deploy(&worker).await.unwrap();
    let operator_account = worker.dev_create_account().await.unwrap();
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
            "bound_classes": ["100000", "500000"], "charge_step": "1000", "min_charge": "2000",
            "slot_rate": 1000, "owner": worker.root_account().unwrap().id(),
            "operator_account": operator_account.id(),
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
    usdc.mint(alice.id(), 20 * ONE).await;

    let home = tempfile::tempdir().unwrap();
    let wall = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let clock = Arc::new(ManualClock::new(wall * 1000));
    let notes = NoteStore::open(&home.path().join("notes.sealed"), PASS, 10).unwrap();
    deposit(&usdc, &alice, &vault, &notes, 0, &clock).await;
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
    let closes = Arc::new(FakeCloseSender::with_clock(dyn_clock.clone()));
    let closer = Arc::new(Closer::new(
        vault_view.clone(),
        withdrawal.clone(),
        closes.clone(),
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

    let parts = GatewayParts {
        operator_url,
        gateway_url: fake.url(),
        rpc_url: worker.rpc_addr(),
        vault_id: vault.id().to_string(),
    };
    let gateway = make_gateway(home.path(), &parts, transport, clock.clone()).await;
    let real = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    // The operator limits an ownership proof to 3,300 s from its own clock, with no slack, so the
    // test clock runs a little ahead of the wallet's.
    clock.advance_secs(real.saturating_sub(clock.now_secs()) + 30);
    Stack {
        worker,
        vault,
        usdc,
        alice,
        _fake: fake,
        clock,
        op,
        closes,
        gateway,
        home,
        parts,
    }
}

/// A submitter account for `family`: its key lists the four methods on the vault. `near` is the
/// account's balance.
async fn submitter(s: &Stack, family: &str, near: NearToken) -> Account {
    let account = s
        .worker
        .root_account()
        .unwrap()
        .create_subaccount(&format!("sub{}", family.replace('_', "")))
        .initial_balance(near)
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let made = s
        .gateway
        .submitters()
        .add(family, account.id().as_str())
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
    // The new key is readable at a final block a few blocks later.
    s.worker.fast_forward(5).await.unwrap();
    account
}

fn no_delay() -> CloseOptions {
    CloseOptions {
        delay: Duration::ZERO,
        polls: 2,
        poll_every: Duration::from_millis(20),
        ..CloseOptions::default()
    }
}

async fn status_of(s: &Stack, commitment: &str) -> Value {
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

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

#[test]
fn the_direct_send_question_has_fixed_words() {
    assert_eq!(
        direct_text(7, "250000"),
        "Tor is down. Sending this transaction directly shows your IP address to the RPC endpoint, together with this note's ID (7) and its total spend (250000 base units of the token). Type yes to send directly, or anything else to stop."
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_close_goes_out_from_the_submitter_and_pays_the_payout_account() {
    let s = stack(Transport::Direct).await;
    let sub = submitter(&s, "personal", NearToken::from_near(5)).await;
    let before = s.usdc.balance_of(s.alice.id()).await;
    let started = now();

    let closed = s
        .gateway
        .close_note(0, no_delay(), None)
        .await
        .expect("the close lands");

    assert!(closed.confirmed);
    assert_eq!(closed.payout_account, s.alice.id().as_str());
    // A close proof is good for 3 hours.
    assert!(
        (started + 10_800..=now() + 10_800).contains(&closed.valid_until),
        "valid_until {} for a proof made at {started}",
        closed.valid_until
    );
    let commitment = entry(&s).commitment.clone();
    assert_eq!(status_of(&s, &commitment).await, "Closed");
    assert_eq!(s.usdc.balance_of(s.alice.id()).await, before + ONE);
    assert_eq!(entry(&s).status, NoteStatus::Closed);
    // The submitter key signed it: its nonce moved and the funding account's did not matter.
    let keys = sub.view_access_keys().await.unwrap();
    assert!(
        keys.iter().any(|k| k.access_key.nonce > 0),
        "the submitter's key signed a transaction"
    );
    // The wallet is archived: another close finds no note to close.
    assert!(matches!(
        s.gateway.close_note(0, no_delay(), None).await,
        Err(LeaveError::NoNote(0))
    ));
    assert!(s.closes.sent().is_empty(), "the operator was not used");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_submitter_key_can_call_nothing_else_on_the_vault() {
    let s = stack(Transport::Direct).await;
    let sub = submitter(&s, "personal", NearToken::from_near(5)).await;
    let made = s.gateway.submitters().get("personal").unwrap().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let tx = nymira_operator::tx::NearTx::new(
        s.worker.rpc_addr(),
        sub.id().as_str(),
        s.vault.id().as_str(),
        made.key.clone(),
        Store::open(&dir.path().join("tx.db")).unwrap(),
        s.clock.clone(),
    )
    .with_config(tx_config());
    let end = tx
        .send(nymira_operator::tx::TxRequest {
            kind: "probe",
            method: "heartbeat",
            args: b"{}".to_vec(),
            gas: 30_000_000_000_000,
            idem_key: None,
        })
        .await;
    // The node refuses a method outside the key's list before the vault sees it.
    let refused = match end {
        Ok(e) => !e.success,
        Err(_) => true,
    };
    assert!(refused, "heartbeat is outside the key's methods");
    let text = nymirad::submitter::instructions(&made, s.vault.id().as_str());
    for m in METHODS {
        assert!(text.contains(m), "{m} is named in the instructions");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unused_clearance_still_allows_a_later_close_with_a_new_proof() {
    let s = stack(Transport::Direct).await;
    // A submitter with almost no NEAR cannot pay for gas.
    submitter(&s, "personal", NearToken::from_millinear(2)).await;

    let err = s
        .gateway
        .close_note(0, no_delay(), None)
        .await
        .expect_err("no NEAR for gas");
    assert!(matches!(err, LeaveError::NoNear { .. }), "{err}");
    let commitment = entry(&s).commitment.clone();
    assert_eq!(status_of(&s, &commitment).await, "Active");

    // The operator's close takes it: the operator's closer is a stub here, so the vault does not
    // show the note closed and the answer says so.
    let sent = s
        .gateway
        .close_note(
            0,
            CloseOptions {
                via_operator: true,
                ..no_delay()
            },
            None,
        )
        .await
        .expect("the operator takes the close proof");
    assert!(!sent.confirmed);
    assert!(sent.tx_hash.starts_with("fakehash"));
    let handed = s.closes.sent();
    assert_eq!(handed.len(), 1);
    assert_eq!(handed[0].1.len(), 256, "the proof goes out as made");

    // A new close proof from a funded submitter still works with the same state.
    s.gateway
        .submitters()
        .assign(&commitment, "second")
        .unwrap();
    submitter(&s, "second", NearToken::from_near(5)).await;
    let closed = s
        .gateway
        .close_note(0, no_delay(), None)
        .await
        .expect("a new close proof after an unused clearance");
    assert!(closed.confirmed);
    assert_eq!(status_of(&s, &commitment).await, "Closed");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exit_concedes_nothing_when_no_request_left_and_blocks_new_requests() {
    let s = stack(Transport::Direct).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    let commitment = entry(&s).commitment.clone();

    let exit = s
        .gateway
        .exit_note(0, None, None)
        .await
        .expect("exit filed");
    assert_eq!(exit.conceded, 0);
    assert_eq!(status_of(&s, &commitment).await, "PendingExit");
    let rows: Value = s
        .vault
        .view("exit_requests")
        .args_json(json!({ "from_note_id": 0, "limit": 10 }))
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(rows[0]["conceded"], "0");
    // The deadline is shown, seven days on.
    let chain_now = nymira_testkit::clock::now_seconds(&s.worker).await;
    assert!(exit.deadline_secs.abs_diff(chain_now + 7 * 86_400) < 60);
    assert_eq!(entry(&s).status, NoteStatus::Exiting);

    // No request on the note after the exit proof left.
    let refused = s
        .gateway
        .chat(
            json!({"model": MODEL, "messages": [{"role": "user", "content": "hi"}]}),
            ChatOptions::default(),
        )
        .await
        .err()
        .expect("a chat has no note to use");
    assert!(
        refused.message.contains("no accepted note"),
        "{}",
        refused.message
    );

    // A second `exit` reports the filing instead of filing again.
    let again = s.gateway.exit_note(0, None, None).await.unwrap();
    assert_eq!(again.deadline_secs, exit.deadline_secs);

    // Finalize waits for the deadline, on the local clock first.
    assert!(matches!(
        s.gateway.finalize_note(0, None, None).await,
        Err(LeaveError::NotYet { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn finalize_is_sent_by_the_submitter_after_the_deadline_and_the_vault_decides() {
    let s = stack(Transport::Direct).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    let exit = s.gateway.exit_note(0, None, None).await.unwrap();
    // The local clock is past the deadline, the sandbox's is not: the call is sent, and the vault
    // refuses it. (The sandbox cannot reach seven days; the vault's own tests cover the payout.)
    s.clock
        .advance_secs((exit.deadline_secs + 10).saturating_sub(s.clock.now_secs()));
    let err = s
        .gateway
        .finalize_note(0, None, None)
        .await
        .expect_err("the vault's window is still open");
    assert!(
        err.to_string().contains("exit window is still open"),
        "{err}"
    );
    // While `nymirad` runs the same call is tried without asking, once a minute.
    let first = s.gateway.finalize_due().await;
    assert_eq!(first.len(), 1);
    assert!(first[0].1.is_err());
    assert!(
        s.gateway.finalize_due().await.is_empty(),
        "not again within a minute"
    );
    s.clock.advance_secs(61);
    assert_eq!(s.gateway.finalize_due().await.len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_exit_concedes_the_stored_bound_when_a_request_left_the_state() {
    let s = stack(Transport::Direct).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    // A lease opens (a request proof leaves the device), then the process dies before it settles.
    let answer = s
        .gateway
        .chat(
            json!({"model": MODEL, "messages": [{"role": "user", "content": "hi"}]}),
            ChatOptions::default(),
        )
        .await
        .map_err(|e| e.message)
        .unwrap();
    drop(answer);
    let restarted = make_gateway(s.home.path(), &s.parts, Transport::Direct, s.clock.clone()).await;
    let exit = restarted
        .exit_note(0, None, None)
        .await
        .expect("exit filed");
    assert_eq!(exit.conceded, 100_000, "the 0.10 USDC bound class");
    let rows: Value = s
        .vault
        .view("exit_requests")
        .args_json(json!({ "from_note_id": 0, "limit": 10 }))
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(rows[0]["conceded"], "100000");
    let _ = &s.op;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dead_tor_sends_nothing_unless_the_user_says_yes() {
    let socks = "127.0.0.1:9".parse().unwrap();
    let (tor, _state) = nymirad::tor::Tor::for_tests(
        socks,
        nymirad::tor::TorState::Failed("test: no circuit".into()),
    );
    let s = stack(Transport::Tor(Arc::new(tor))).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    let commitment = entry(&s).commitment.clone();

    // Unattended: it waits, and asks nothing.
    assert!(matches!(
        s.gateway.exit_note(0, None, None).await,
        Err(LeaveError::TorDown(_))
    ));
    // Declined: nothing is sent, and the words are the fixed ones.
    let no = Scripted::new(false);
    assert!(matches!(
        s.gateway.exit_note(0, None, Some(&no)).await,
        Err(LeaveError::Declined)
    ));
    assert_eq!(*no.asked.lock().unwrap(), vec![direct_text(0, "0")]);
    assert_eq!(status_of(&s, &commitment).await, "Active");
    // Confirmed: the direct send goes out.
    let yes = Scripted::new(true);
    let exit = s
        .gateway
        .exit_note(0, None, Some(&yes))
        .await
        .expect("a confirmed direct send");
    assert_eq!(exit.conceded, 0);
    assert_eq!(yes.asked.lock().unwrap().len(), 1);
    assert_eq!(status_of(&s, &commitment).await, "PendingExit");
    // The clearance and the operator's close never go direct, whatever the user says.
    let close = s.gateway.close_note(0, no_delay(), Some(&yes)).await;
    assert!(
        close.is_err(),
        "a note that already left cannot be closed: {close:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_payout_is_collected_with_claim_payout() {
    let s = stack(Transport::Direct).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    let before = s.usdc.balance_of(s.alice.id()).await;
    // The token refuses transfers, so the close pays into the vault's pending payouts.
    s.usdc.set_paused(true).await;
    s.gateway
        .close_note(0, no_delay(), None)
        .await
        .expect("the close lands even when the payout does not");
    assert_eq!(s.usdc.balance_of(s.alice.id()).await, before);
    let pending: Value = s
        .vault
        .view("pending_payout")
        .args_json(json!({ "account_id": s.alice.id() }))
        .await
        .unwrap()
        .json()
        .unwrap();
    assert_eq!(pending, ONE.to_string());

    s.usdc.set_paused(false).await;
    s.gateway
        .claim_payout(0, None, None)
        .await
        .expect("the payout is claimed");
    assert_eq!(s.usdc.balance_of(s.alice.id()).await, before + ONE);
    assert!(matches!(
        s.gateway.claim_payout(0, None, None).await,
        Err(LeaveError::NothingPending(_))
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_reaches_close_over_the_owner_socket_and_not_over_http() {
    use nymirad::ownersock::{bind, request, serve, socket_path, Command};
    use std::os::unix::fs::PermissionsExt;

    let s = stack(Transport::Direct).await;
    submitter(&s, "personal", NearToken::from_near(5)).await;
    let listener = bind(s.home.path()).unwrap();
    let dir = socket_path(s.home.path()).parent().unwrap().to_path_buf();
    assert_eq!(
        std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        std::fs::metadata(socket_path(s.home.path()))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    tokio::spawn(serve(listener, s.gateway.clone()));

    let mut said = Vec::new();
    let text = request(
        s.home.path(),
        &Command::Close {
            note: 0,
            delay_secs: Some(0),
            family: None,
            via_operator: false,
        },
        |l| said.push(l.to_string()),
        |_| false,
    )
    .await
    .expect("the close runs over the socket");
    assert!(text.contains("is closed"), "{text}");
    assert!(said.iter().any(|l| l.contains("on its own Tor circuit")));

    // A group- or world-readable directory is refused.
    let loose = tempfile::tempdir().unwrap();
    std::fs::create_dir(loose.path().join("run")).unwrap();
    std::fs::set_permissions(
        loose.path().join("run"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    assert!(bind(loose.path()).is_err());

    // None of the owner commands is on the HTTP route table.
    for route in ["/v1/close", "/v1/exit", "/v1/finalize", "/v1/claim-payout"] {
        assert!(
            !nymirad::guard::OWNER_ROUTES
                .iter()
                .any(|(_, p)| *p == route),
            "{route}"
        );
    }
}
