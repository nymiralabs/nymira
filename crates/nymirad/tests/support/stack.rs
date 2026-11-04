//! The sandbox stack shared by the tests that run the gateway against a real vault: a sandbox
//! vault with the mock USDC, the real operator (closer stubbed) behind the fake NEAR AI
//! gateway, and `nymirad`'s gateway in front. Included by path (`#[path = "support/stack.rs"]`)
//! so only the binaries that use it compile it.
#![allow(dead_code)]

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
use nymira_testkit::proofs::signing_key;
use nymira_testkit::vault::DevVault;
use nymirad::attest::{Policy, TestEvidence};
use nymirad::gateway::close::CloseOptions;
use nymirad::gateway::{Gateway, GatewayConfig};
use nymirad::pins::read_live;
use nymirad::rpc::{Rpc, Transport};
use nymirad::secrets::{new_secret, NoteEntry, NoteStatus, NoteStore};
use nymirad::submitter::METHODS;
use serde_json::{json, Value};
use zkapi_proof::compact::CompactSigner;
use zkapi_types::Felt252;

pub const SETUP_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/zkapi/protocol/setup/v2-concession-dev"
);
pub const ONE: u128 = 1_000_000;
pub const FEE: u128 = 50_000;
pub const MODEL: &str = "z-ai/glm-5.3-flash";
pub const PASS: &str = "test-passphrase";

pub fn point(seed: u64) -> Value {
    let key = signing_key(seed);
    json!({ "x": format!("0x{}", hex::encode(key.x.0)), "y": format!("0x{}", hex::encode(key.y.0)) })
}

pub fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub struct Stack {
    pub worker: near_workspaces::Worker<near_workspaces::network::Sandbox>,
    pub vault: Contract,
    pub usdc: MockUsdc,
    pub alice: Account,
    pub fake: Running,
    pub treasury: Account,
    pub clock: Arc<ManualClock>,
    pub op: Arc<Operator>,
    pub closes: Arc<FakeCloseSender>,
    pub gateway: Arc<Gateway>,
    pub home: tempfile::TempDir,
    /// A second gateway over the same home, for the crash test.
    pub parts: GatewayParts,
}

/// What it takes to build another gateway over the same state.
pub struct GatewayParts {
    pub operator_url: String,
    pub gateway_url: String,
    pub rpc_url: String,
    pub vault_id: String,
}

pub async fn deposit(
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

pub fn tx_config() -> TxConfig {
    TxConfig {
        send_timeout: Duration::from_secs(60),
        retry_delay: Duration::from_millis(200),
        max_attempts: 5,
    }
}

pub fn gateway_config(home: &std::path::Path, parts: &GatewayParts) -> GatewayConfig {
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
        withdrawal_key_sha256: hex::encode(Sha256::digest(
            std::fs::read(format!("{SETUP_DIR}/withdrawal.pk")).unwrap(),
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

pub async fn make_gateway(
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

/// The stack with one 1 USDC note already deposited (note 0) and its leaf in the vault's root.
pub async fn stack(transport: Transport) -> Stack {
    build(transport, true).await
}

/// Lets the vault's minimum root age pass on the sandbox clock, then `sync_root`s every note.
pub async fn sync_root(
    worker: &near_workspaces::Worker<near_workspaces::network::Sandbox>,
    caller: &Account,
    vault: &Contract,
) {
    for _ in 0..5 {
        let at = nymira_testkit::clock::now_seconds(worker).await + 40;
        if nymira_testkit::clock::advance_to(worker, at).await.is_ok() {
            break;
        }
    }
    caller
        .call(vault.id(), "sync_root")
        .args_json(json!({ "max": 100 }))
        .max_gas()
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    worker.fast_forward(5).await.unwrap();
}

/// `predeposit` false leaves the vault empty: the caller deposits through the wallet itself.
pub async fn build(transport: Transport, predeposit: bool) -> Stack {
    let worker = near_workspaces::sandbox().await.unwrap();
    let usdc = MockUsdc::deploy(&worker).await.unwrap();
    let operator_account = worker.dev_create_account().await.unwrap();
    let treasury = worker.dev_create_account().await.unwrap();
    let alice = worker.dev_create_account().await.unwrap();
    let vault = DevVault::default()
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
    if predeposit {
        let notes = NoteStore::open(&home.path().join("notes.sealed"), PASS, 10).unwrap();
        deposit(&usdc, &alice, &vault, &notes, 0, &clock).await;
        drop(notes);
        sync_root(&worker, &alice, &vault).await;
    }

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
        fake,
        treasury,
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
pub async fn submitter(s: &Stack, family: &str, near: NearToken) -> Account {
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

pub fn no_delay() -> CloseOptions {
    CloseOptions {
        delay: Duration::ZERO,
        polls: 2,
        poll_every: Duration::from_millis(20),
        ..CloseOptions::default()
    }
}

pub async fn status_of(s: &Stack, commitment: &str) -> Value {
    s.vault
        .view("note_status")
        .args_json(json!({ "commitment": commitment }))
        .await
        .unwrap()
        .json()
        .unwrap()
}

pub fn entry(s: &Stack) -> NoteEntry {
    NoteStore::open(&s.home.path().join("notes.sealed"), PASS, 10)
        .unwrap()
        .list()
        .unwrap()
        .remove(0)
}
