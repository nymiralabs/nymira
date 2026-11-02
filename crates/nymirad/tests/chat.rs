//! The whole path: a sandbox vault with deposited notes, the operator over it, the fake NEAR AI
//! gateway behind the operator, and `nymirad`'s gateway in front. A chat goes in as OpenAI JSON;
//! the lease, the encrypted exchange, the settlement and the checks happen in between.

use std::sync::Arc;

use near_workspaces::types::NearToken;
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
use nymira_operator::usage::Minter;
use nymira_operator::verify::{GrothVerifier, GrothWithdrawalVerifier};
use nymira_testkit::build::contract_wasm_with_features;
use nymira_testkit::mock_usdc::MockUsdc;
use nymira_testkit::proofs::{canonical_verifying_key, signing_key, Kind};
use nymirad::attest::{Policy, TestEvidence, Trust};
use nymirad::gateway::{ChatOptions, Gateway, GatewayConfig, Outcome};
use nymirad::pins::read_live;
use nymirad::rpc::{Rpc, Transport};
use nymirad::secrets::{new_secret, NoteEntry, NoteStatus, NoteStore};
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
    _worker: near_workspaces::Worker<near_workspaces::network::Sandbox>,
    vault: Contract,
    fake: Running,
    clock: Arc<ManualClock>,
    op: Arc<Operator>,
    gateway: Arc<Gateway>,
    home: tempfile::TempDir,
    secrets: Vec<String>,
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

async fn stack(note_count: u64, tune: impl FnOnce(&mut GatewayConfig)) -> Stack {
    stack_with(note_count, tune, |_| {}).await
}

async fn stack_with(
    note_count: u64,
    tune: impl FnOnce(&mut GatewayConfig),
    tune_pins: impl FnOnce(&mut nymirad::pins::Pins),
) -> Stack {
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
    let mut secrets = Vec::new();
    for n in 0..note_count {
        secrets.push(deposit(&usdc, &alice, &vault, &notes, n, &clock).await);
    }
    if note_count > 0 {
        // The sandbox keeps producing blocks, so a target can already be behind: ask again.
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
    }
    worker.fast_forward(5).await.unwrap();

    // The operator over the vault, with the fake gateway behind it.
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
    // The fake gateway lists every model at one price; pin the one model whose price it matches.
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

    // nymirad's gateway.
    let rpc = Rpc::new(worker.rpc_addr(), &Transport::Direct).unwrap();
    let mut pins = read_live(&rpc, vault.id().as_str()).await.unwrap();
    tune_pins(&mut pins);
    let signer_key = ed25519_dalek::SigningKey::from_bytes(&[42; 32]);
    let test = TestPolicy::fake();
    let policy = Policy {
        version: 1,
        attestation_root_pk: test.attestation_root_pk,
        accepted_measurements: test.accepted_measurements,
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
    use sha2::{Digest, Sha256};
    let mut config = GatewayConfig {
        home: home.path().to_path_buf(),
        operator_url,
        gateway_url: fake.url(),
        vault: vault.id().to_string(),
        network,
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
        backup: None,
    };
    tune(&mut config);
    let gateway = Arc::new(
        Gateway::new(
            config,
            reqwest::Client::new(),
            rpc,
            pins,
            NoteStore::open(&home.path().join("notes.sealed"), PASS, 10).unwrap(),
            PASS,
            Arc::new(TestEvidence { policy }),
            dyn_clock,
        )
        .unwrap(),
    );
    // Building the stack takes a while; the wallet stamps requests with the real clock, the
    // operator reads the manual one. Bring them together before the first request.
    let real = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    clock.advance_secs(real.saturating_sub(clock.now_secs()));
    Stack {
        _worker: worker,
        vault,
        fake,
        clock,
        op,
        gateway,
        home,
        secrets,
    }
}

fn request(text: &str, stream: bool) -> Value {
    json!({"model": MODEL, "stream": stream,
           "messages": [{"role": "user", "content": text}]})
}

async fn json_chat(s: &Stack, text: &str) -> Value {
    match s
        .gateway
        .chat(request(text, false), ChatOptions::default())
        .await
        .map_err(|e| e.message)
        .unwrap()
    {
        Outcome::Json(v, _) => v,
        Outcome::Stream(..) => panic!("expected JSON"),
    }
}

/// Retires, lets the operator settle (it waits 10 s after revoking), and settles on our side.
async fn settle(s: &Stack) -> Vec<nymirad::gateway::Settled> {
    s.gateway.reap(true).await;
    for _ in 0..3 {
        s.op.tick().await;
        s.clock.advance_secs(11);
    }
    s.op.tick().await;
    s.op.tick().await;
    s.gateway.settle().await
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chat_goes_through_the_lease_encrypted_and_settles_with_checks() {
    let s = stack(1, |_| {}).await;
    let answer = json_chat(&s, "What is the capital of France?").await;
    assert_eq!(
        answer["choices"][0]["message"]["content"],
        "fake reply to: What is the capital of France?"
    );
    // The gateway saw ciphertext only.
    let seen = s.fake.handle.chats().pop().unwrap();
    assert!(seen.e2ee && seen.encrypt_all_fields && seen.no_aliasing && seen.include_usage);

    let settled = settle(&s).await;
    assert_eq!(settled.len(), 1);
    let done = &settled[0];
    assert_eq!(done.trust, Trust::ModelSigned);
    assert!(done.warnings.is_empty(), "{:?}", done.warnings);
    assert_eq!(done.charge, done.expected);

    // Records: metadata only.
    let records = s.gateway.records();
    assert_eq!(records.len(), 1);
    let r = &records[0];
    assert_eq!(
        (r.requests, r.message_count, r.model.as_str()),
        (1, 1, MODEL)
    );
    assert!(r.root.starts_with("0x") && r.leaf_count == 1 && r.snapshot_block > 0);
    assert_eq!(r.charge.as_deref(), Some(done.charge.to_string().as_str()));
    let file = std::fs::read_to_string(s.home.path().join("lease-records.jsonl")).unwrap();
    assert!(
        !file.contains("capital of France"),
        "no content in the records"
    );

    // Nothing in the clear on disk: no note secret, no wallet state, no work directory.
    assert!(!s.home.path().join("work").join("x").exists());
    let mut leaked = Vec::new();
    for entry in walk(s.home.path()) {
        let bytes = std::fs::read(&entry).unwrap_or_default();
        let text = String::from_utf8_lossy(&bytes);
        for secret in &s.secrets {
            if text.contains(secret.as_str()) {
                leaked.push(entry.clone());
            }
        }
        if text.contains("current_balance") {
            leaked.push(entry.clone());
        }
    }
    assert!(leaked.is_empty(), "plain state on disk: {leaked:?}");
    let _ = &s.vault;
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

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_is_decrypted_as_it_arrives() {
    let s = stack(1, |_| {}).await;
    let Outcome::Stream(body, lease) = s
        .gateway
        .chat(request("stream me", true), ChatOptions::default())
        .await
        .map_err(|e| e.message)
        .unwrap()
    else {
        panic!("expected a stream");
    };
    assert!(!lease.is_empty());
    let bytes = axum::body::to_bytes(body, usize::MAX).await.unwrap();
    let text = String::from_utf8(bytes.to_vec()).unwrap();
    let content: String = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| serde_json::from_str::<Value>(d).ok())
        .filter_map(|v| {
            v["choices"][0]["delta"]["content"]
                .as_str()
                .map(String::from)
        })
        .collect();
    assert_eq!(content, "fake reply to: stream me");
    assert!(text.contains("[DONE]"));
    // The gateway signs streams itself (the fake follows the live behaviour), so a stream is
    // reported as gateway-signed, not model-signed.
    let settled = settle(&s).await;
    assert_eq!(settled[0].trust, Trust::GatewaySigned);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_note_gives_402_with_the_two_notes_hint_and_a_second_note_serves() {
    let s = stack(1, |_| {}).await;
    json_chat(&s, "one").await;
    s.gateway.reap(true).await; // retiring: the note still has its lease
    let err = s
        .gateway
        .chat(request("two", false), ChatOptions::default())
        .await
        .err()
        .expect("refused");
    assert_eq!(err.status.as_u16(), 402);
    assert!(err.message.contains("keep two notes"), "{}", err.message);

    let s2 = stack(2, |_| {}).await;
    json_chat(&s2, "first").await;
    s2.gateway.reap(true).await;
    json_chat(&s2, "second, on the other note").await;
    assert_eq!(s2.gateway.records().len(), 2);
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_note_the_answer_is_402() {
    let s = stack(0, |_| {}).await;
    let err = s
        .gateway
        .chat(request("hi", false), ChatOptions::default())
        .await
        .err()
        .expect("refused");
    assert_eq!(err.status.as_u16(), 402);
    assert!(err.message.contains("no accepted note"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_attestation_stops_the_request_before_anything_is_encrypted() {
    let s = stack(1, |_| {}).await;
    s.fake.handle.update(|sw| sw.measurement = "ff".repeat(32));
    let err = s
        .gateway
        .chat(request("secret", false), ChatOptions::default())
        .await
        .err()
        .expect("refused");
    assert_eq!(err.code, "attestation_failed");
    assert!(
        s.fake.handle.chats().is_empty(),
        "nothing reached the gateway"
    );

    s.fake.handle.update(|sw| {
        sw.measurement = nymira_fake_near_ai::policy::default_measurement();
        sw.tcb_status = "OutOfDate".into();
    });
    let err = s
        .gateway
        .chat(request("secret", false), ChatOptions::default())
        .await
        .err()
        .expect("refused");
    assert!(err.message.contains("TCB"), "{}", err.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_bad_signature_is_reported_as_not_verified() {
    let s = stack(1, |_| {}).await;
    s.fake.handle.update(|sw| sw.bad_signature = true);
    json_chat(&s, "hello").await;
    let settled = settle(&s).await;
    assert!(matches!(settled[0].trust, Trust::NotVerified(_)));
    assert!(settled[0]
        .warnings
        .iter()
        .any(|w| w.contains("not verified")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_charge_above_the_rules_warns() {
    // This client believes the model costs 1 nano-USD a token; the operator and the provider
    // charge the real 80 and 500, so the charge sits far above what the client recomputes.
    let s = stack(1, |c| {
        for m in &mut c.models {
            m.input_nano_per_token = 1;
            m.output_nano_per_token = 1;
        }
    })
    .await;
    s.fake.handle.update(|sw| {
        sw.usage = Some(nymira_fake_near_ai::Usage {
            prompt_tokens: 100,
            completion_tokens: 100_000,
        })
    });
    let mut req = request("hello", false);
    req["max_tokens"] = json!(50);
    match s.gateway.chat(req, ChatOptions::default()).await {
        Ok(_) => {}
        Err(e) => panic!("{}", e.message),
    }
    let settled = settle(&s).await;
    assert!(
        settled[0].warnings.iter().any(|w| w.contains("charged")),
        "{:?}",
        settled[0]
    );
    assert!(settled[0].charge > settled[0].expected);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_changed_vault_or_changed_lease_parameters_refuse_a_lease() {
    // The binary pins a different code hash than the vault has.
    let s = stack_with(1, |_| {}, |p| p.code_hash = "different".into()).await;
    let err = s
        .gateway
        .chat(request("hi", false), ChatOptions::default())
        .await
        .err()
        .expect("refused");
    assert_eq!(err.code, "vault_pins_changed");
    assert!(err.message.contains("code hash"), "{}", err.message);
    assert!(s.fake.handle.chats().is_empty());

    // The binary pins other lease parameters than the vault (and so the operator) have.
    let s = stack_with(
        1,
        |_| {},
        |p| {
            p.config["min_charge"] = json!("1");
        },
    )
    .await;
    let err = s
        .gateway
        .chat(request("hi", false), ChatOptions::default())
        .await
        .err()
        .expect("refused");
    assert!(
        err.message.contains("update") || err.message.contains("params_applied"),
        "{}",
        err.message
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_swapped_proving_key_is_refused_before_use() {
    assert!(nymirad::gateway::check_proving_key(SETUP_DIR, &"00".repeat(32)).is_err());
    use sha2::{Digest, Sha256};
    let good = hex::encode(Sha256::digest(
        std::fs::read(format!("{SETUP_DIR}/request.pk")).unwrap(),
    ));
    assert!(nymirad::gateway::check_proving_key(SETUP_DIR, &good).is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_live_model_list_is_compared_at_start() {
    let s = stack(0, |_| {}).await;
    assert!(s.gateway.check_models().await.is_empty());
    s.fake
        .handle
        .update(|sw| sw.input_price_nano_per_token = 99);
    let warnings = s.gateway.check_models().await;
    assert!(
        warnings.iter().any(|w| w.contains("price changed")),
        "{warnings:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_backup_carries_the_lease_record_and_the_sealed_state() {
    let places = tempfile::tempdir().unwrap();
    let (private, public) = nymirad::backup::generate_identity();
    let recipient = nymirad::backup::parse_recipient(&public).unwrap();
    let place = places.path().to_path_buf();
    let s = stack(1, move |c| c.backup = Some((recipient, vec![place]))).await;
    json_chat(&s, "backed up?").await;
    settle(&s).await;
    let sealed = std::fs::read(places.path().join("state.age")).unwrap();
    let plain = nymirad::backup::decrypt(private.as_str(), &sealed).unwrap();
    let files: std::collections::BTreeMap<String, String> = serde_json::from_slice(&plain).unwrap();
    assert!(files.contains_key("lease-records.jsonl"));
    assert!(files.contains_key("notes.sealed"));
    assert!(files.keys().any(|k| k.starts_with("wallets/")));
    // Restoring needs only the private key: the sealed files inside open with the passphrase.
    use base64::Engine;
    let record = base64::engine::general_purpose::STANDARD
        .decode(&files["lease-records.jsonl"])
        .unwrap();
    assert!(String::from_utf8(record).unwrap().contains("\"charge\""));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_tries_openai_shapes_over_http_and_errors_keep_their_shape() {
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use tower::ServiceExt;

    let s = stack(1, |_| {}).await;
    let token_path = s.home.path().join("owner.token");
    let token = nymirad::token::OwnerToken::load_or_create(&token_path).unwrap();
    let text = std::fs::read_to_string(&token_path).unwrap();
    let app = nymirad::guard::router_with(Arc::new(token), Some(s.gateway.clone()));
    let call = |body: Value| {
        let mut r = Request::builder()
            .method("POST")
            .uri("/v1/chat/completions")
            .header("host", "localhost:8788")
            .header("authorization", format!("Bearer {text}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap();
        r.extensions_mut().insert(ConnectInfo(
            "127.0.0.1:5000".parse::<std::net::SocketAddr>().unwrap(),
        ));
        app.clone().oneshot(r)
    };

    // A tool definition and a tool-result turn go through encrypted; the operator accepts them.
    let tools = json!({"model": MODEL, "messages": [{"role": "user", "content": "weather?"}],
        "tools": [{"type": "function", "function": {"name": "get_weather",
            "description": "Weather", "parameters": {"type": "object"}}}]});
    let r = call(tools).await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers().contains_key("x-nymira-lease-id"));
    let body: Value = serde_json::from_slice(
        &axum::body::to_bytes(r.into_body(), usize::MAX)
            .await
            .unwrap(),
    )
    .unwrap();
    assert_eq!(body["choices"][0]["message"]["role"], "assistant");
    assert!(body["usage"]["prompt_tokens"].is_u64());

    // OpenAI's error shape for an unserved model and for malformed input.
    for bad in [
        json!({"model": "gpt-4", "messages": [{"role": "user", "content": "x"}]}),
        json!({"model": MODEL, "messages": [{"role": "user", "content": "x"}], "logit_bias": {}}),
    ] {
        let r = call(bad).await.unwrap();
        assert_eq!(r.status(), 400);
        let e: Value = serde_json::from_slice(
            &axum::body::to_bytes(r.into_body(), usize::MAX)
                .await
                .unwrap(),
        )
        .unwrap();
        assert!(e["error"]["message"].is_string() && e["error"]["type"].is_string());
    }
}
