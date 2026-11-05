//! `make demo`: a local stack with no keys and no money. A NEAR sandbox with the real vault and
//! a mock USDC, the operator over that vault, the fake NEAR AI gateway and a funded demo
//! account. It writes a state directory for `nymirad` and prints the commands to run.

use std::sync::Arc;

use ed25519_dalek::SigningKey;
use near_workspaces::types::{AccessKey, NearToken, PublicKey};
use nymira_near_codec::{chain_id, contract_address, Network};
use nymira_operator::alarm::Alarms;
use nymira_operator::chain::RpcVault;
use nymira_operator::clock::{Clock, SystemClock};
use nymira_operator::closer::{Closer, FakeCloseSender};
use nymira_operator::nearai::{NearAiAdapter, NearAiConfig};
use nymira_operator::operator::{Config, Operator, Parts};
use nymira_operator::replica::NoReplica;
use nymira_operator::sealed::SealedFile;
use nymira_operator::signer::{FakeHeartbeat, LocalSigner, Signer};
use nymira_operator::store::Store;
use nymira_operator::usage::Minter;
use nymira_operator::verify::{GrothVerifier, GrothWithdrawalVerifier};
use nymira_testkit::build::contract_wasm_with_features;
use nymira_testkit::mock_usdc::MockUsdc;
use nymira_testkit::proofs::{canonical_verifying_key, signing_key, Kind};
use nymirad::pins::read_live;
use nymirad::rpc::{Rpc, Transport};
use serde_json::{json, Value};
use zkapi_proof::compact::CompactSigner;
use zkapi_types::Felt252;

const SETUP_DIR: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/zkapi/protocol/setup/v2-concession-dev"
);

fn point(seed: u64) -> Value {
    let key = signing_key(seed);
    json!({ "x": format!("0x{}", hex::encode(key.x.0)), "y": format!("0x{}", hex::encode(key.y.0)) })
}

fn b64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn write_private(path: &std::path::Path, text: &str) {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    f.write_all(text.as_bytes()).unwrap();
}

#[tokio::main]
async fn main() {
    let home = std::env::temp_dir().join("nymira-demo");
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();

    println!("starting the sandbox and deploying the vault (the first run builds the wasm)...");
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
            "denominations": ["1000000", "5000000"], "tvl_cap": "100000000",
            "request_vk": b64(&canonical_verifying_key(Kind::Request)),
            "withdrawal_vk": b64(&canonical_verifying_key(Kind::Withdrawal)),
            "treasury": treasury.id(), "note_fee": "50000", "exit_delay_secs": 7 * 86_400,
            "note_ttl": 30 * 86_400, "bound_classes": ["100000", "500000"],
            "charge_step": "1000", "min_charge": "2000", "slot_rate": 1000,
            "owner": worker.root_account().unwrap().id(), "operator_account": operator_account.id(),
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
    usdc.mint(alice.id(), 100_000_000).await;

    // The demo account's key, written for the CLI.
    let key = SigningKey::from_bytes(&rand::random::<[u8; 32]>());
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
    worker.fast_forward(5).await.unwrap();
    write_private(&home.join("funding.key"), &hex::encode(key.to_bytes()));
    write_private(&home.join("passphrase"), "demo-passphrase");

    // The fake gateway and the operator over the sandbox vault.
    let fake = nymira_fake_near_ai::start("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let clock: Arc<dyn Clock> = Arc::new(SystemClock);
    let alarms = Arc::new(Alarms::new(vec![], clock.clone()));
    let sealed = |name: &str| SealedFile::open(&home.join(name), "demo-passphrase", 10).unwrap();
    let provider = Arc::new(
        NearAiAdapter::new(
            NearAiConfig {
                user_agent: "demo".into(),
                base_url: fake.url(),
                org_id: "org".into(),
                workspace_id: "ws".into(),
            },
            clock.clone(),
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
        Arc::new(FakeCloseSender::with_clock(clock.clone())),
        clock.clone(),
        alarms.clone(),
        chain_id(network),
        contract,
    ));
    let store = Store::open(&home.join("operator.db")).unwrap();
    let minter = Minter::generate();
    let signer = Arc::new(
        Signer::new(
            CompactSigner::from_seed(&Felt252::from_u64(1)),
            CompactSigner::from_seed(&Felt252::from_u64(2)),
            Store::open(&home.join("operator.db")).unwrap(),
            vault_view.clone(),
            minter.public_key(),
            chain_id(network),
            contract,
            clock.clone(),
            Arc::new(FakeHeartbeat::default()),
        )
        .unwrap(),
    );
    signer.spawn_background();
    let mut cfg = Config::new(
        vault.id().as_str(),
        chain_id(network),
        contract,
        "http://localhost",
    );
    cfg.host = "localhost".into();
    let op = Operator::start(Parts {
        cfg,
        clock: clock.clone(),
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
    .expect("operator starts");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let operator_url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        axum::serve(listener, nymira_operator::http::router(op))
            .await
            .unwrap();
    });

    // Pins and config for the CLI.
    let rpc = Rpc::new(worker.rpc_addr(), &Transport::Direct).unwrap();
    let pins = read_live(&rpc, vault.id().as_str()).await.unwrap();
    std::fs::write(
        home.join("demo-pins.json"),
        serde_json::to_vec_pretty(&pins).unwrap(),
    )
    .unwrap();
    // The attestation policy for the fake gateway's test evidence, signed with a demo key.
    let policy_signer = SigningKey::from_bytes(&rand::random::<[u8; 32]>());
    let test = nymira_fake_near_ai::TestPolicy::fake();
    let policy = nymirad::attest::Policy {
        version: 1,
        attestation_root_pk: test.attestation_root_pk,
        accepted_measurements: test.accepted_measurements,
        deployments: vec![],
        accepted_tcb_statuses: test.accepted_tcb_statuses,
        nvidia_certificates: vec![],
        signature: String::new(),
    }
    .sign(&policy_signer);
    std::fs::write(
        home.join("attestation-policy.json"),
        serde_json::to_vec_pretty(&policy).unwrap(),
    )
    .unwrap();
    std::fs::write(
        home.join("policy-signer.pub"),
        hex::encode(policy_signer.verifying_key().to_bytes()),
    )
    .unwrap();
    nymirad::config::Config {
        rpc_url: worker.rpc_addr(),
        operator_url: operator_url.clone(),
        account: alice.id().to_string(),
        vault: Some(vault.id().to_string()),
        gateway_url: Some(fake.url()),
        setup_dir: Some(SETUP_DIR.into()),
        proving_key_sha256: Some({
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(
                std::fs::read(format!("{SETUP_DIR}/request.pk")).unwrap(),
            ))
        }),
        withdrawal_key_sha256: Some({
            use sha2::{Digest, Sha256};
            hex::encode(Sha256::digest(
                std::fs::read(format!("{SETUP_DIR}/withdrawal.pk")).unwrap(),
            ))
        }),
        // The sandbox node and the operator are on this machine: no Tor (a `mock` build only).
        direct: true,
        ..Default::default()
    }
    .save(&home)
    .unwrap();

    println!("\nDemo stack is up.");
    println!("  sandbox RPC   {}", worker.rpc_addr());
    println!("  vault         {}", vault.id());
    println!("  operator      {operator_url}");
    println!("  fake NEAR AI  {}", fake.url());
    println!("  demo account  {} (100 mock USDC)", alice.id());
    println!("\nIn another terminal (NYMIRA_MODE=mainnet: the demo token is not Circle's):");
    let run = format!(
        "NYMIRA_MODE=mainnet cargo run -q -p nymirad --features mock -- --home {} --passphrase-file {}/passphrase",
        home.display(),
        home.display()
    );
    println!("  {run} status");
    println!(
        "  {run} deposit --key-file {}/funding.key --amount 1000000",
        home.display()
    );
    println!("\nCtrl-C stops everything.");
    std::future::pending::<()>().await;
}
