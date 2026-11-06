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
use std::io::{BufRead, BufReader, Read, Write};
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

fn stop_after_address(home: &std::path::Path, args: &[String]) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nymirad"));
    command
        .env("NYMIRA_MODE", "mainnet")
        .arg("--home")
        .arg(home)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: setsid is async-signal-safe and has no preconditions here.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    let mut input = child.stdin.take().unwrap();
    input.write_all(b"yes\nyes\n").unwrap();
    input.flush().unwrap();
    let mut reader = BufReader::new(child.stdout.take().unwrap());
    let mut shown = Vec::new();
    loop {
        let mut line = String::new();
        assert!(
            reader.read_line(&mut line).unwrap() > 0,
            "process ended before address: {}",
            String::from_utf8_lossy(&shown)
        );
        shown.extend_from_slice(line.as_bytes());
        if line.starts_with("Deposit address:") {
            break;
        }
    }
    child.kill().unwrap();
    reader.read_to_end(&mut shown).unwrap();
    let mut output = child.wait_with_output().unwrap();
    output.stdout = shown;
    output
}

async fn walk(real_verifier: bool, no_deposit: bool, restore_from_backup: bool) {
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
    let credentials = tempfile::tempdir().unwrap();
    let backup_place = tempfile::tempdir().unwrap();
    let (identity, recipient) = backup::generate_identity();
    let config = Config {
        rpc_url: s.worker.rpc_addr(),
        account: s.alice.id().to_string(),
        direct: true,
        oneclick_base: Some(fake.url()),
        backup_recipient: Some(recipient),
        backup_places: vec![backup_place.path().into()],
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
        std::fs::write(credentials.path().join(name), value).unwrap();
        std::fs::set_permissions(
            credentials.path().join(name),
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
        credentials.path().join("pass").display().to_string(),
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
        credentials.path().join("backup-key").display().to_string(),
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
    let out = tokio::task::spawn_blocking(move || stop_after_address(&dir, &args))
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
    let saved_pending = backup::load(&[backup_place.path().to_path_buf()], &identity).unwrap();
    assert_eq!(saved_pending.notes[0].status, NoteStatus::Pending);
    let pending = &saved_pending.funding[0];
    assert_eq!(pending.phase, nymirad::intents::Phase::AddressReady);
    let sealed = std::fs::read(home.path().join("notes.sealed.funding")).unwrap();
    for private in [
        &pending.commitment,
        pending.quote["quote"]["depositAddress"].as_str().unwrap(),
        "0x2222222222222222222222222222222222222222",
        "address_ready",
    ] {
        assert!(
            !String::from_utf8_lossy(&sealed).contains(private),
            "funding state must be encrypted"
        );
    }
    let resumed = tempfile::tempdir().unwrap();
    let resumed_path = if restore_from_backup {
        std::fs::remove_dir_all(home.path()).unwrap();
        backup::restore_into(resumed.path(), saved_pending, stack::PASS, ROUNDS).unwrap();
        resumed.path()
    } else {
        home.path()
    };
    std::fs::write(
        resumed_path.join("demo-pins.json"),
        serde_json::to_vec(&pins).unwrap(),
    )
    .unwrap();
    let attempt = NoteStore::open(&resumed_path.join("notes.sealed"), stack::PASS, ROUNDS)
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
        credentials.path().join("pass").display().to_string(),
        "recover".into(),
        "--intents".into(),
        attempt.commitment.clone(),
    ];
    let before = fake.requests().len();
    let dir = resumed_path.to_path_buf();
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
    let dir = resumed_path.to_path_buf();
    let out = tokio::task::spawn_blocking(move || cli(&dir, &resume_args, "yes\n"))
        .await
        .unwrap();
    if no_deposit {
        assert!(!out.status.success());
        assert!(String::from_utf8_lossy(&out.stderr)
            .contains("no unique matching vault deposit receipt"));
        assert_eq!(
            NoteStore::open(&resumed_path.join("notes.sealed"), stack::PASS, ROUNDS)
                .unwrap()
                .list()
                .unwrap()[0]
                .status,
            NoteStatus::Pending
        );
        assert!(!resumed_path.join("calendar").exists());
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
    let notes = NoteStore::open(&resumed_path.join("notes.sealed"), stack::PASS, ROUNDS)
        .unwrap()
        .list()
        .unwrap();
    assert_eq!(notes[0].status, NoteStatus::Accepted);
    let saved = backup::load(&[backup_place.path().to_path_buf()], &identity).unwrap();
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
    let ics = std::fs::read_dir(resumed_path.join("calendar"))
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
    walk(true, false, false).await;
}
#[tokio::test(flavor = "multi_thread")]
async fn the_demo_standin_also_delivers_a_usable_note() {
    walk(false, false, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn success_without_a_matching_deposit_receipt_never_accepts_the_note() {
    walk(true, true, true).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn refused_verifier_deliveries_are_found_and_claimed_without_a_note_id() {
    use nymira_testkit::proofs::{canonical_verifying_key, Kind};
    use nymirad::intents::{Attempt, Funding, Phase};
    use nymirad::mode::Mode;
    use nymirad::oneclick::FundingClient;
    use nymirad::ownersock::{bind, serve};
    use nymirad::secrets::{new_secret, NoteEntry};
    use std::sync::Mutex;

    let mut s = stack::build(Transport::Direct, false).await;
    let vault = s
        .worker
        .dev_deploy(&nymira_testkit::build::contract_wasm_with_features(
            "contracts/vault",
            &["dev-setup", "test-intents-account"],
        ))
        .await
        .unwrap();
    vault.call("new").args_json(json!({
        "operator_pk": stack::point(1), "clearance_pk": stack::point(2), "token": s.usdc.contract.id(),
        "denominations": ["1000000"], "tvl_cap": "1050000",
        "request_vk": stack::b64(&canonical_verifying_key(Kind::Request)),
        "withdrawal_vk": stack::b64(&canonical_verifying_key(Kind::Withdrawal)),
        "treasury": s.treasury.id(), "note_fee": "50000", "exit_delay_secs": 604800,
        "note_ttl": 2592000, "bound_classes": ["100000", "500000"],
        "charge_step": "1000", "min_charge": "2000", "slot_rate": 1000,
        "owner": s.worker.root_account().unwrap().id(), "operator_account": s.alice.id(),
    })).max_gas().transact().await.unwrap().into_result().unwrap();
    s.usdc.register_id(vault.id()).await;
    s.vault = vault;
    s.parts.vault_id = s.vault.id().to_string();
    s.gateway =
        stack::make_gateway(s.home.path(), &s.parts, Transport::Direct, s.clock.clone()).await;
    let verifier = nymira_testkit::intents::deploy_verifier(&s.worker, "intents", "wrap.test.near")
        .await
        .unwrap();
    s.usdc.register_id(verifier.id()).await;
    s.usdc
        .ft_transfer_call(
            &s.alice,
            verifier.id(),
            10_000_000,
            &json!({"receiver_id": s.alice.id()}).to_string(),
        )
        .await
        .unwrap();

    let (occupied_secret, occupied) = new_secret("near-mainnet");
    let occupied = format!("0x{}", hex::encode(occupied.0));
    let deliver = |amount: u128, commitment: &str| {
        s.alice.call(verifier.id(), "ft_withdraw").args_json(json!({
            "token": s.usdc.contract.id(), "receiver_id": s.vault.id(), "amount": amount.to_string(),
            "msg": json!({"commitment": commitment, "withdraw_to": s.alice.id()}).to_string()
        })).deposit(NearToken::from_yoctonear(1)).max_gas()
    };
    deliver(1_050_000, &occupied)
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    let sub = stack::submitter(&s, "personal", NearToken::from_near(5)).await;
    let key = s
        .gateway
        .submitters()
        .get("personal")
        .unwrap()
        .unwrap()
        .public_key();
    let rpc = Rpc::new(s.worker.rpc_addr(), &Transport::Direct).unwrap();
    let mut finalized = false;
    for _ in 0..30 {
        if rpc.call("query", json!({"request_type": "view_access_key", "finality": "final", "account_id": sub.id(), "public_key": key})).await.is_ok() {
            finalized = true; break;
        }
        s.worker.fast_forward(5).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(
        finalized,
        "the submitter key must be readable at final finality"
    );
    let listener = bind(s.home.path()).unwrap();
    let task = tokio::spawn(serve(listener, s.gateway.clone()));
    let status = Arc::new(Mutex::new(json!({})));
    let edited_status = status.clone();
    let app = axum::Router::new().route(
        "/v0/status",
        axum::routing::get(move || {
            let value = edited_status.lock().unwrap().clone();
            async move { axum::Json(value) }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let client = FundingClient::mock(
        &format!("http://{}", listener.local_addr().unwrap()),
        Mode::Mainnet,
        true,
    )
    .unwrap();
    let status_task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let (_, public) = backup::generate_identity();
    let funding = Funding {
        mode: Mode::Mainnet,
        home: s.home.path().into(),
        pins: nymirad::pins::read_live(&rpc, s.vault.id().as_str())
            .await
            .unwrap(),
        rpc,
        archival: None,
        notes: NoteStore::open(&s.home.path().join("notes.sealed"), stack::PASS, 10).unwrap(),
        backup: backup::Target::new(
            backup::parse_recipient(&public).unwrap(),
            vec![s.home.path().join("backup")],
        ),
        clock: s.clock.clone(),
    };
    for (case, amount) in [
        ("short_by_one_unit", 1_049_999),
        ("below_smallest_denomination", 900_000),
        ("repeated_commitment", 1_050_000),
        ("full_cap", 1_050_000),
    ] {
        let (secret, commitment) = new_secret("near-mainnet");
        let commitment = if case == "repeated_commitment" {
            occupied.clone()
        } else {
            format!("0x{}", hex::encode(commitment.0))
        };
        funding
            .notes
            .add(NoteEntry {
                chain_label: "near-mainnet".into(),
                secret: hex::encode(if case == "repeated_commitment" {
                    occupied_secret.0
                } else {
                    secret.0
                }),
                commitment: commitment.clone(),
                denomination: "1000000".into(),
                payout: s.alice.id().to_string(),
                status: NoteStatus::Pending,
                note_id: None,
                expiry_ns: None,
                deposit_amount: None,
                tx_hash: None,
                created_at: 1,
            })
            .unwrap();
        let mut attempt = Attempt {
            commitment: commitment.clone(),
            request: json!({"amount": "1050000"}),
            quote: json!({"quote": {"depositAddress": "fake"}}),
            receipt: None,
            phase: Phase::AddressReady,
            destination: None,
            bookings: vec![],
            notice: None,
            via: None,
        };
        funding.notes.save_funding(&attempt).unwrap();
        funding.back_up().unwrap();
        let result = deliver(amount, &commitment).transact().await.unwrap();
        let hash = result.outcome().transaction_hash.to_string();
        result.into_result().unwrap();
        *status.lock().unwrap() = json!({"status": "SUCCESS", "swapDetails": {
            "nearTxHashes": [hash], "destinationSigner": s.alice.id(), "amountOut": amount.to_string()
        }});
        let report = funding.progress(&client, &mut attempt).await.unwrap_err();
        assert!(
            report.contains(&format!("Receipt booked payout: {amount}"))
                && report.contains("claim-payout"),
            "{case}: {report}"
        );
        assert_eq!(attempt.phase, Phase::Booked);
        assert_eq!(
            funding
                .notes
                .list()
                .unwrap()
                .iter()
                .find(|n| n.commitment == commitment)
                .unwrap()
                .status,
            NoteStatus::Pending
        );
        let claim_args = vec![
            "claim-payout".into(),
            "--intents".into(),
            commitment.clone(),
        ];
        let before = s.usdc.balance_of(s.alice.id()).await;
        if case == "short_by_one_unit" {
            s.usdc.set_paused(true).await;
            let dir = s.home.path().to_path_buf();
            let args = claim_args.clone();
            let failed = tokio::task::spawn_blocking(move || cli(&dir, &args, ""))
                .await
                .unwrap();
            assert!(
                !failed.status.success(),
                "a failed token transfer must not report a claim"
            );
            assert_eq!(s.usdc.balance_of(s.alice.id()).await, before);
            let pending: Value = s
                .vault
                .view("pending_payout")
                .args_json(json!({"account_id": s.alice.id()}))
                .await
                .unwrap()
                .json()
                .unwrap();
            assert_eq!(pending, amount.to_string());
            s.usdc.set_paused(false).await;
        }
        let dir = s.home.path().to_path_buf();
        let output = tokio::task::spawn_blocking(move || cli(&dir, &claim_args, ""))
            .await
            .unwrap();
        assert!(
            output.status.success(),
            "{case}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("Pending payout claimed"));
        assert_eq!(s.usdc.balance_of(s.alice.id()).await, before + amount);
        assert_eq!(s.usdc.balance_of(sub.id()).await, 0);
        let after = funding.progress(&client, &mut attempt).await.unwrap_err();
        assert!(
            after.contains("nothing to claim") && !after.contains("Run nymirad claim-payout"),
            "{case}: {after}"
        );
    }
    task.abort();
    status_task.abort();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_state_directory_is_recovered_from_the_pre_payment_backup() {
    walk(true, false, true).await;
}
