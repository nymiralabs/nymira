//! The CLI funds a note through the open-source Verifier, release v0.4.2.
#[path = "support/stack.rs"]
mod stack;

use near_workspaces::types::NearToken;
use near_workspaces::{Account, Contract};
use nymirad::{
    backup,
    config::Config,
    rpc::{Rpc, Transport},
    secrets::{NoteStatus, NoteStore, ROUNDS},
};
use serde_json::{json, Value};
use std::io::Write;
use std::os::unix::{fs::PermissionsExt, process::CommandExt};
use std::process::{Command, Stdio};
use std::sync::Arc;

struct Deliver {
    owner: Account,
    verifier: Contract,
    token: String,
    vault: String,
    no_deposit: bool,
}
#[async_trait::async_trait]
impl nymirad::fake_oneclick::Delivery for Deliver {
    async fn deliver(&self, request: &Value) -> Result<(String, String), String> {
        if self.no_deposit {
            let result = self
                .owner
                .call(&self.vault.parse().unwrap(), "sync_root")
                .args_json(json!({"max": 0}))
                .max_gas()
                .transact()
                .await
                .map_err(|e| e.to_string())?;
            let hash = result.outcome().transaction_hash.to_string();
            result.into_result().map_err(|e| e.to_string())?;
            return Ok((hash, self.owner.id().to_string()));
        }
        let result = self.owner.call(self.verifier.id(), "ft_withdraw")
            .args_json(json!({"token": self.token, "receiver_id": self.vault, "amount": request["amount"], "msg": request["customRecipientMsg"]}))
            .deposit(NearToken::from_yoctonear(1)).max_gas().transact().await.map_err(|e| e.to_string())?;
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

async fn walk(real_verifier: bool, no_deposit: bool) {
    static SANDBOX: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _sandbox = SANDBOX.lock().await;
    let s = stack::build(Transport::Direct, false).await;
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
    let verifier = if real_verifier {
        nymira_testkit::intents::deploy_verifier(&s.worker, "intents", "wrap.test.near")
            .await
            .unwrap()
    } else {
        nymira_testkit::intents::deploy_standin(&s.worker)
            .await
            .unwrap()
    };
    let deliverer = if real_verifier {
        s.alice.clone()
    } else {
        verifier.as_account().clone()
    };
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
    let fake = nymirad::fake_oneclick::start(Arc::new(Deliver {
        owner: deliverer,
        verifier,
        token: s.usdc.contract.id().to_string(),
        vault: s.vault.id().to_string(),
        no_deposit,
    }))
    .await
    .unwrap();
    fake.refuse_asset(nymirad::intents::SOURCES[1].asset);
    let home = tempfile::tempdir().unwrap();
    let (identity, recipient) = backup::generate_identity();
    let config = Config {
        rpc_url: s.worker.rpc_addr(),
        account: s.alice.id().to_string(),
        direct: true,
        oneclick_base: Some(fake.url()),
        backup_recipient: Some(recipient),
        backup_places: vec![home.path().join("backup")],
        ..Default::default()
    };
    config.save(home.path()).unwrap();
    let old = backup::StateBackup {
        version: backup::VERSION,
        taken_at: 1,
        ..Default::default()
    };
    backup::write_backups(
        &config.backup_target().unwrap(),
        &old.to_bytes().unwrap(),
        backup::STATE_FILE,
    )
    .unwrap();
    for (name, value) in [("pass", stack::PASS), ("backup-key", identity.as_str())] {
        std::fs::write(home.path().join(name), value).unwrap();
        std::fs::set_permissions(
            home.path().join(name),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let rpc = Rpc::new(s.worker.rpc_addr(), &Transport::Direct).unwrap();
    let empty_root = rpc
        .view(s.vault.id().as_str(), "current_root", json!({}))
        .await
        .unwrap();
    let pins = nymirad::pins::read_live(&rpc, s.vault.id().as_str())
        .await
        .unwrap();
    std::fs::write(
        home.path().join("demo-pins.json"),
        serde_json::to_vec(&pins).unwrap(),
    )
    .unwrap();
    let args = vec![
        "--passphrase-file".into(),
        home.path().join("pass").display().to_string(),
        "deposit".into(),
        "--source".into(),
        "ethereum-usdc".into(),
        "--refund".into(),
        "0x2222222222222222222222222222222222222222".into(),
        "--amount".into(),
        "1000000".into(),
        "--payout".into(),
        s.alice.id().to_string(),
        "--backup-key-file".into(),
        home.path().join("backup-key").display().to_string(),
        "--mock-pay".into(),
    ];
    let dir = home.path().to_path_buf();
    let decline_args = args.clone();
    let out = tokio::task::spawn_blocking(move || cli(&dir, &decline_args, "no\n"))
        .await
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("1Click calls do not go through Tor, because 1Click's terms forbid using Tor to get around its restrictions. 1Click will see your IP address, source wallet, commitment and payout account."));
    assert!(
        fake.requests().is_empty(),
        "no quote or status before confirmation"
    );
    let dir = home.path().to_path_buf();
    let out = tokio::task::spawn_blocking(move || cli(&dir, &args, "yes\nyes\nno\n"))
        .await
        .unwrap();
    assert!(!out.status.success());
    let shown = String::from_utf8_lossy(&out.stdout);
    assert!(shown.contains("On this path 1Click and the Intents explorer pair your source wallet with this deposit. The proofs are designed to keep the vault and the operator from tying your spending to this deposit, but while few notes are open, and without Tor, your spending can still be matched to it by time and network address. Wait a while between depositing and your first chat."));
    assert!(shown.contains(&format!("Payout account {} (fingerprint ", s.alice.id())));
    assert!(shown.contains("non-refundable note fee"));
    assert!(String::from_utf8_lossy(&out.stdout).contains("Deposit address: fake-deposit-"));
    assert!(String::from_utf8_lossy(&out.stdout)
        .contains("Pay 1.065000 source tokens (1065000 source base units)"));
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("Available sources: ethereum-usdc\n"),
        "failed Base and high-minimum BSC must be omitted"
    );
    assert!(!home.path().join("calendar").exists());
    let saved_pending = backup::load(&[home.path().join("backup")], &identity).unwrap();
    assert_eq!(saved_pending.notes[0].status, NoteStatus::Pending);
    let resumed = tempfile::tempdir().unwrap();
    backup::restore_into(resumed.path(), saved_pending, stack::PASS, ROUNDS).unwrap();
    std::fs::write(
        resumed.path().join("demo-pins.json"),
        serde_json::to_vec(&pins).unwrap(),
    )
    .unwrap();
    let attempt = NoteStore::open(&resumed.path().join("notes.sealed"), stack::PASS, ROUNDS)
        .unwrap()
        .funding_attempts()
        .unwrap()
        .pop()
        .unwrap();
    reqwest::Client::new()
        .post(format!("{}/mock/pay", fake.url()))
        .json(&json!({"depositAddress": attempt.quote["quote"]["depositAddress"]}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let resume_args = vec![
        "--passphrase-file".into(),
        home.path().join("pass").display().to_string(),
        "recover".into(),
        "--intents".into(),
        attempt.commitment.clone(),
    ];
    let before = fake.requests().len();
    let dir = resumed.path().to_path_buf();
    let no_args = resume_args.clone();
    let out = tokio::task::spawn_blocking(move || cli(&dir, &no_args, "no\n"))
        .await
        .unwrap();
    assert!(!out.status.success());
    assert_eq!(
        fake.requests().len(),
        before,
        "restart asks again before status"
    );
    let dir = resumed.path().to_path_buf();
    let out = tokio::task::spawn_blocking(move || cli(&dir, &resume_args, "yes\n"))
        .await
        .unwrap();
    if no_deposit {
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr)
            .contains("no unique matching vault deposit receipt"));
        assert_eq!(
            NoteStore::open(&resumed.path().join("notes.sealed"), stack::PASS, ROUNDS)
                .unwrap()
                .list()
                .unwrap()[0]
                .status,
            NoteStatus::Pending
        );
        assert!(!resumed.path().join("calendar").exists());
        return;
    }
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("accepted") && said.contains("Read the payout account"),
        "{said}"
    );
    let seen = fake.requests();
    assert!(
        seen.iter()
            .all(|r| r["originAsset"] != nymirad::intents::SOURCES[2].asset),
        "BSC minimum is above this payment"
    );
    assert!(
        seen.iter().any(|r| r["dry"] == true),
        "sources must be discovered with dry quotes"
    );
    for r in seen.iter().filter(|r| r["dry"] == true) {
        assert_eq!(r["recipient"], "nymira.near");
        assert_eq!(r["refundTo"], "0x1111111111111111111111111111111111111111");
        assert!(!r.to_string().contains(s.alice.id().as_str()));
    }
    let notes = NoteStore::open(&resumed.path().join("notes.sealed"), stack::PASS, ROUNDS)
        .unwrap()
        .list()
        .unwrap();
    assert_eq!(notes[0].status, NoteStatus::Accepted);
    let saved = backup::load(&[home.path().join("backup")], &identity).unwrap();
    assert_eq!(saved.notes[0].commitment, notes[0].commitment);
    let restored = tempfile::tempdir().unwrap();
    backup::restore_into(restored.path(), saved, stack::PASS, 10).unwrap();
    let mut root = empty_root.clone();
    for _ in 0..5 {
        stack::sync_root(&s.worker, &s.alice, &s.vault).await;
        root = rpc
            .view(s.vault.id().as_str(), "current_root", json!({}))
            .await
            .unwrap();
        if root != empty_root {
            break;
        }
    }
    assert_ne!(
        root, empty_root,
        "the deposited leaf must reach a final root before opening a lease"
    );
    let gateway = stack::make_gateway(
        restored.path(),
        &s.parts,
        Transport::Direct,
        s.clock.clone(),
    )
    .await;
    let result = gateway.chat(json!({"model": stack::MODEL, "stream": false, "messages": [{"role": "user", "content": "Hello"}]}), nymirad::gateway::ChatOptions::default()).await;
    if let Err(e) = result {
        panic!("lease failed: {e:?}");
    }
    let ics = std::fs::read_dir(resumed.path().join("calendar"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let text = std::fs::read_to_string(ics).unwrap();
    assert!(text.contains("SUMMARY:Nymira note expires"));
    assert!(!text.contains(&notes[0].commitment) && !text.contains(s.alice.id().as_str()));
}

#[tokio::test(flavor = "multi_thread")]
async fn cli_quote_pay_status_receipt_backup_restore_then_a_lease() {
    walk(true, false).await;
}
#[tokio::test(flavor = "multi_thread")]
async fn the_demo_standin_also_delivers_a_usable_note() {
    walk(false, false).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn success_without_a_matching_deposit_receipt_never_accepts_the_note() {
    walk(true, true).await;
}
