//! The two-step Intents path: 1Click pays the user's own NEAR account, then `nymirad` sends the
//! USDC on to the vault from that account.
#[path = "support/stack.rs"]
mod stack;

use near_workspaces::types::{AccessKey, NearToken, PublicKey};
use near_workspaces::{Account, Contract};
use nymirad::{
    backup,
    config::Config,
    intents::{Funding, Phase},
    mode::Mode,
    oneclick::{validate_two_step, FundingClient},
    rpc::{Rpc, Transport},
    secrets::{NoteStatus, NoteStore, ROUNDS},
};
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::process::{Command, Stdio};
use std::sync::Arc;

const SEED: [u8; 32] = [21; 32];

/// What 1Click does for a two-step quote: a plain `ft_withdraw` to the recipient, no message.
struct Deliver {
    owner: Account,
    verifier: Contract,
    token: String,
}
#[async_trait::async_trait]
impl nymirad::fake_oneclick::Delivery for Deliver {
    async fn deliver(&self, request: &Value) -> Result<(String, String), String> {
        let result = self
            .owner
            .call(self.verifier.id(), "ft_withdraw")
            .args_json(json!({"token": self.token, "receiver_id": request["recipient"], "amount": request["amount"]}))
            .deposit(NearToken::from_yoctonear(1))
            .max_gas()
            .transact()
            .await
            .map_err(|e| e.to_string())?;
        let hash = result.outcome().transaction_hash.to_string();
        result.into_result().map_err(|e| e.to_string())?;
        Ok((hash, self.owner.id().to_string()))
    }
}

fn cli(home: &std::path::Path, args: &[String], input: &str) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nymirad"));
    cmd.env("NYMIRA_MODE", "mainnet")
        .arg("--home")
        .arg(home)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: setsid is async-signal-safe and has no preconditions here.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

struct World {
    s: stack::Stack,
    fake: nymirad::fake_oneclick::Running,
    home: tempfile::TempDir,
    credentials: tempfile::TempDir,
    backup_place: tempfile::TempDir,
    identity: String,
}

/// A sandbox with the real Verifier, a fake 1Click that pays through it, and alice's own key.
async fn world(poor_account: bool) -> World {
    let s = stack::build(Transport::Direct, false).await;
    // An account with too little NEAR for the second step's gas.
    let poor = s
        .worker
        .root_account()
        .unwrap()
        .create_subaccount("poor")
        .initial_balance(NearToken::from_millinear(5))
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let account = if poor_account { &poor } else { &s.alice };
    s.vault
        .as_account()
        .deploy(&nymira_testkit::build::contract_wasm_with_features(
            "contracts/vault",
            &["dev-setup", "test-intents-account"],
        ))
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let verifier = nymira_testkit::intents::deploy_verifier(&s.worker, "intents", "wrap.test.near")
        .await
        .unwrap();
    s.usdc.register_id(verifier.id()).await;
    s.usdc
        .ft_transfer_call(
            &s.alice,
            verifier.id(),
            5_000_000,
            &json!({"receiver_id": s.alice.id()}).to_string(),
        )
        .await
        .unwrap();
    let key = ed25519_dalek::SigningKey::from_bytes(&SEED);
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
    s.worker.fast_forward(5).await.unwrap();
    let fake = nymirad::fake_oneclick::start(Arc::new(Deliver {
        owner: s.alice.clone(),
        verifier,
        token: s.usdc.contract.id().to_string(),
    }))
    .await
    .unwrap();
    fake.refuse_asset(nymirad::intents::SOURCES[1].asset);
    let home = tempfile::tempdir().unwrap();
    let credentials = tempfile::tempdir().unwrap();
    let backup_place = tempfile::tempdir().unwrap();
    let (identity, recipient) = backup::generate_identity();
    Config {
        rpc_url: s.worker.rpc_addr(),
        account: account.id().to_string(),
        direct: true,
        oneclick_base: Some(fake.url()),
        backup_recipient: Some(recipient),
        backup_places: vec![backup_place.path().into()],
        intents_two_step: true,
        ..Default::default()
    }
    .save(home.path())
    .unwrap();
    for (name, value) in [
        ("pass", stack::PASS.to_string()),
        ("backup-key", identity.to_string()),
        ("key", hex::encode(SEED)),
    ] {
        let path = credentials.path().join(name);
        std::fs::write(&path, value).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let rpc = Rpc::new(s.worker.rpc_addr(), &Transport::Direct).unwrap();
    let pins = nymirad::pins::read_live(&rpc, s.vault.id().as_str())
        .await
        .unwrap();
    std::fs::write(
        home.path().join("demo-pins.json"),
        serde_json::to_vec(&pins).unwrap(),
    )
    .unwrap();
    World {
        s,
        fake,
        home,
        credentials,
        backup_place,
        identity: identity.to_string(),
    }
}

fn deposit_args(w: &World) -> Vec<String> {
    let c = |n: &str| w.credentials.path().join(n).display().to_string();
    vec![
        "--passphrase-file".into(),
        c("pass"),
        "deposit".into(),
        "--source".into(),
        "ethereum-usdc".into(),
        "--refund".into(),
        "0x2222222222222222222222222222222222222222".into(),
        "--amount".into(),
        "1000000".into(),
        "--payout".into(),
        w.s.alice.id().to_string(),
        "--backup-key-file".into(),
        c("backup-key"),
        "--key-file".into(),
        c("key"),
        "--mock-pay".into(),
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn the_cli_pays_the_account_then_the_vault_and_a_lease_opens() {
    static SANDBOX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _sandbox = SANDBOX.lock().await;
    let w = world(false).await;
    let args = deposit_args(&w);
    // Declining the 1Click question stops everything: the fixed text is shown and 1Click is
    // not contacted, as on the direct path.
    let (dir, declined) = (w.home.path().to_path_buf(), args.clone());
    let out = tokio::task::spawn_blocking(move || cli(&dir, &declined, "no\n"))
        .await
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("1Click calls do not go through Tor, because 1Click's terms forbid using Tor to get around its restrictions. 1Click will see your IP address, source wallet, commitment and payout account."));
    assert!(
        w.fake.requests().is_empty(),
        "no quote or status before confirmation"
    );
    let dir = w.home.path().to_path_buf();
    let out = tokio::task::spawn_blocking(move || cli(&dir, &args, "yes\nyes\nyes\n"))
        .await
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{said}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(said.contains("accepted") && said.contains("Read the payout account"));
    // Notices before paying.
    assert!(said.contains(nymirad::intents::PRIVACY));
    assert!(said.contains("Your NEAR account appears on chain next to the deposit."));
    assert!(said.contains(
        "needs a little NEAR for gas on your account and an account registered for USDC"
    ));
    // The quote pays the user's account and carries no message.
    let quotes: Vec<_> = w
        .fake
        .requests()
        .into_iter()
        .filter(|r| r["dry"] == false)
        .collect();
    assert_eq!(quotes.len(), 1);
    assert_eq!(quotes[0]["recipient"], w.s.alice.id().to_string());
    assert!(quotes[0].get("customRecipientMsg").is_none());
    // The second step names no withdraw_to: the payout account is the sending account.
    let store = nymira_operator::store::Store::open(&w.home.path().join("tx.db")).unwrap();
    let row = store
        .run(|c| nymira_operator::store::tx(c, 1))
        .await
        .unwrap()
        .unwrap();
    let call: Value = serde_json::from_slice(&row.args).unwrap();
    let msg: Value = serde_json::from_str(call["msg"].as_str().unwrap()).unwrap();
    assert_eq!(call["receiver_id"], w.s.vault.id().to_string());
    assert!(msg.get("withdraw_to").is_none() && msg.get("commitment").is_some());
    // The note is accepted and opens a lease.
    let notes = NoteStore::open(&w.home.path().join("notes.sealed"), stack::PASS, ROUNDS)
        .unwrap()
        .list()
        .unwrap();
    assert_eq!(notes[0].status, NoteStatus::Accepted);
    for _ in 0..5 {
        stack::sync_root(&w.s.worker, &w.s.alice, &w.s.vault).await;
    }
    let saved = backup::load(&[w.backup_place.path().to_path_buf()], &w.identity).unwrap();
    let restored = tempfile::tempdir().unwrap();
    backup::restore_into(restored.path(), saved, stack::PASS, 10).unwrap();
    let gateway = stack::make_gateway(
        restored.path(),
        &w.s.parts,
        Transport::Direct,
        w.s.clock.clone(),
    )
    .await;
    gateway
        .chat(
            json!({"model": stack::MODEL, "stream": false, "messages": [{"role": "user", "content": "Hello"}]}),
            nymirad::gateway::ChatOptions::default(),
        )
        .await
        .unwrap_or_else(|e| panic!("lease failed: {e:?}"));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_account_without_gas_is_not_offered_the_path() {
    static SANDBOX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _sandbox = SANDBOX.lock().await;
    let w = world(true).await;
    let args = deposit_args(&w);
    let dir = w.home.path().to_path_buf();
    let out = tokio::task::spawn_blocking(move || cli(&dir, &args, "yes\nyes\nyes\n"))
        .await
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("not offered"));
    assert!(
        w.fake.requests().is_empty(),
        "no source is listed and no quote asked"
    );
}

#[test]
fn the_quote_must_pay_the_users_own_account_with_no_message() {
    let quote = |recipient: &str, msg: Option<&str>| {
        let mut r = json!({"recipient": recipient});
        if let Some(m) = msg {
            r["customRecipientMsg"] = m.into();
        }
        json!({"quoteRequest": r})
    };
    assert!(validate_two_step(&quote("alice.near", None), "alice.near", "vault.near").is_ok());
    assert!(validate_two_step(&quote("alice.near", Some("")), "alice.near", "vault.near").is_ok());
    let vault = validate_two_step(&quote("vault.near", None), "alice.near", "vault.near");
    assert!(vault.unwrap_err().contains("vault"));
    assert!(validate_two_step(&quote("bob.near", None), "alice.near", "vault.near").is_err());
    assert!(
        validate_two_step(&quote("alice.near", Some("{}")), "alice.near", "vault.near").is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_second_step_leaves_the_usdc_in_the_account_and_offers_the_deposit_again() {
    static SANDBOX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _sandbox = SANDBOX.lock().await;
    let w = world(false).await;
    let s = &w.s;
    let rpc = Rpc::new(s.worker.rpc_addr(), &Transport::Direct).unwrap();
    let funding = Funding {
        mode: Mode::Mainnet,
        home: w.home.path().into(),
        pins: nymirad::pins::read_live(&rpc, s.vault.id().as_str())
            .await
            .unwrap(),
        rpc,
        archival: None,
        notes: NoteStore::open(&w.home.path().join("notes.sealed"), stack::PASS, 10).unwrap(),
        backup: Config::load(w.home.path())
            .unwrap()
            .backup_target()
            .unwrap(),
        clock: s.clock.clone(),
    };
    let client = FundingClient::mock(&w.fake.url(), Mode::Mainnet, true).unwrap();
    let tx = nymirad::deposit::funding_tx(
        &s.worker.rpc_addr(),
        s.alice.id().as_str(),
        s.usdc.contract.id().to_string(),
        ed25519_dalek::SigningKey::from_bytes(&SEED),
        nymira_operator::store::Store::open(&w.home.path().join("tx.db")).unwrap(),
        s.clock.clone(),
        &Transport::Direct,
    )
    .unwrap();
    let mut attempt = funding
        .prepare_two_step(
            &client,
            &tx,
            1_000_000,
            nymirad::intents::SOURCES[0].asset,
            "0x2222222222222222222222222222222222222222",
            s.alice.id().as_str(),
            &w.identity,
        )
        .await
        .unwrap();
    // Someone else takes the commitment first, so the vault refunds the second step.
    s.usdc
        .ft_transfer_call(
            &s.alice,
            s.vault.id(),
            1_050_000,
            &json!({"commitment": attempt.commitment}).to_string(),
        )
        .await
        .unwrap();
    client
        .mock_pay(attempt.quote["quote"]["depositAddress"].as_str().unwrap())
        .await
        .unwrap();
    let before = s.usdc.balance_of(s.alice.id()).await;
    let said = funding
        .progress_two_step(&client, &tx, &mut attempt)
        .await
        .unwrap_err();
    assert!(
        said.contains("refunded")
            && said.contains(&format!("still in {}", s.alice.id()))
            && said.contains(&format!("balance {before} base units"))
            && said.contains(&format!("recover --intents {}", attempt.commitment)),
        "{said}"
    );
    assert_eq!(
        attempt.phase,
        Phase::ReceiptPending,
        "the deposit can be sent again"
    );
    assert_eq!(s.usdc.balance_of(s.alice.id()).await, before);
}
