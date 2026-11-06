//! The owner approval flow through the real `nymirad` binary: a running `serve` with a gateway
//! on the sandbox stack, the owner commands `agent approvals` and `agent approve`, and a scan of
//! everything the process leaves behind. The in-process routes and refusals are in `approvals.rs`.

#[path = "support/policy.rs"]
mod policy;
#[path = "support/stack.rs"]
mod stack;

use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::Duration;

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use near_workspaces::types::{AccessKey, PublicKey};
use nymirad::agents::{Registry, Spec, Transport};
use nymirad::backup;
use nymirad::pins::read_live;
use nymirad::rpc::{Rpc, Transport as Net};
use nymirad::secrets::{NoteStore, ROUNDS};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use stack::*;

const MARKER: &str = "zq-private-question-77";
const BIG: u64 = 5_000;

struct Run {
    // Held so the sandbox, the operator and the fake NEAR AI keep running.
    _s: Stack,
    home: tempfile::TempDir,
    /// Logs, the passphrase file and the signed approval: nothing the scan reads.
    out: tempfile::TempDir,
    second_place: tempfile::TempDir,
    backup_secret: String,
    serve: Child,
    port: u16,
    agent_token: String,
    owner_token: String,
    owner_key: SigningKey,
}

impl Run {
    fn nymirad(&self, args: &[&str]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_nymirad"));
        cmd.env("NYMIRA_MODE", "mainnet")
            .arg("--home")
            .arg(self.home.path())
            .args(args)
            .stdin(Stdio::null());
        cmd.output().unwrap()
    }

    fn logs(&self) -> (PathBuf, PathBuf) {
        (
            self.out.path().join("serve.out"),
            self.out.path().join("serve.err"),
        )
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder()
            .timeout(Duration::from_secs(90))
            .no_proxy()
            .build()
            .unwrap()
    }

    /// Calls the running `serve`; the listener checks the Host against 8788 whatever port the
    /// test build binds.
    async fn http(
        &self,
        method: &str,
        path: &str,
        token: &str,
        extra: &[(&str, &str)],
        body: Option<Value>,
    ) -> (u16, Value) {
        let client = Self::client();
        let mut tries = 0;
        loop {
            let mut req = client
                .request(
                    method.parse().unwrap(),
                    format!("http://127.0.0.1:{}{path}", self.port),
                )
                .header("host", "localhost:8788")
                .bearer_auth(token.trim());
            for (k, v) in extra {
                req = req.header(*k, *v);
            }
            if let Some(b) = &body {
                req = req.json(b);
            }
            match req.send().await {
                Ok(r) => {
                    let status = r.status().as_u16();
                    return (status, r.json().await.unwrap_or(Value::Null));
                }
                Err(e) if tries > 600 || !e.is_connect() => panic!("{e}"),
                Err(_) => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    async fn hold(&self, text: &str, max_tokens: u64) -> (u16, Value) {
        let body = json!({"model": MODEL, "stream": false, "max_tokens": max_tokens,
            "messages": [{"role": "user", "content": text}]});
        self.http(
            "POST",
            "/v1/chat/completions",
            &self.agent_token,
            &[],
            Some(body),
        )
        .await
    }

    async fn waiting(&self) -> Vec<Value> {
        let (status, v) = self
            .http("GET", "/v1/nymira/approvals", &self.owner_token, &[], None)
            .await;
        assert_eq!(status, 200, "{v}");
        v.as_array().unwrap().clone()
    }

    /// What the owner's wallet returns for the approval message of a waiting entry.
    fn signed_approval(&self, held: &Value) -> Value {
        let registry = Registry::at(self.home.path());
        let book = registry.load().unwrap();
        let agent = &book.agents[0];
        let message = json!({
            "kind": "nymira-approval",
            "id": held["approval_id"],
            "request_hash": held["request_hash"],
            "amount_micro": held["amount_micro"],
            "nonce": held["nonce"],
        })
        .to_string();
        let recipient = nymirad::approvals::recipient(&agent.name, &agent.note);
        let nonce = [7u8; 32];
        let hash = nymirad::policy::nep413_hash(&message, &nonce, &recipient);
        let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
        json!({
            "account_id": agent.owner_account,
            "public_key": policy::public_key(&self.owner_key),
            "signature": b64(&self.owner_key.sign(&hash).to_bytes()),
            "nonce": b64(&nonce),
            "message": message,
            "recipient": recipient,
        })
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        let _ = self.serve.kill();
        let _ = self.serve.wait();
    }
}

fn confirm_text() -> String {
    policy::yaml_limits(&[MODEL], "0.5", "0.05", "2", 30)
        .replace("confirm_above_usd: 5", "confirm_above_usd: 0.001")
}

/// A mock `serve` with a gateway over the sandbox stack, one funded budget note, an agent on a TCP
/// token with a policy that holds anything above 0.001 USD, and a backup set up in two places.
async fn launch() -> Run {
    let s = build(Net::Direct, false).await;
    let home = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let second_place = tempfile::tempdir().unwrap();

    // One funded, accepted note for the agent.
    let notes = NoteStore::open(&home.path().join("notes.sealed"), PASS, ROUNDS).unwrap();
    deposit(&s.usdc, &s.alice, &s.vault, &notes, 0, &s.clock).await;
    let commitment = notes.list().unwrap()[0].commitment.clone();
    drop(notes);
    sync_root(&s.worker, &s.alice, &s.vault).await;

    // The owner's signing key is a full-access key of the owner account.
    let owner_key = SigningKey::from_bytes(&[22u8; 32]);
    let public: PublicKey = policy::public_key(&owner_key).parse().unwrap();
    s.alice
        .batch(s.alice.id())
        .add_key(public, AccessKey::full_access())
        .transact()
        .await
        .unwrap()
        .into_result()
        .unwrap();
    s.worker.fast_forward(5).await.unwrap();

    // What `make demo` writes next to the state.
    let rpc = Rpc::new(s.worker.rpc_addr(), &Net::Direct).unwrap();
    let pins = read_live(&rpc, s.vault.id().as_str()).await.unwrap();
    std::fs::write(
        home.path().join("demo-pins.json"),
        serde_json::to_vec(&pins).unwrap(),
    )
    .unwrap();
    let signer = SigningKey::from_bytes(&[42u8; 32]);
    let test = nymira_fake_near_ai::TestPolicy::fake();
    let attestation = nymirad::attest::Policy {
        version: 5,
        attestation_root_pk: test.attestation_root_pk,
        accepted_measurements: test.accepted_measurements,
        deployments: vec![],
        accepted_tcb_statuses: test.accepted_tcb_statuses,
        nvidia_pins: vec![],
        signature: String::new(),
    }
    .sign(&signer);
    std::fs::write(
        home.path().join("attestation-policy.json"),
        serde_json::to_vec(&attestation).unwrap(),
    )
    .unwrap();
    std::fs::write(
        home.path().join("policy-signer.pub"),
        hex::encode(signer.verifying_key().to_bytes()),
    )
    .unwrap();
    let (private, recipient) = backup::generate_identity();
    let hash_of = |name: &str| {
        hex::encode(Sha256::digest(
            std::fs::read(format!("{SETUP_DIR}/{name}")).unwrap(),
        ))
    };
    nymirad::config::Config {
        rpc_url: s.worker.rpc_addr(),
        operator_url: s.parts.operator_url.clone(),
        account: s.alice.id().to_string(),
        vault: Some(s.vault.id().to_string()),
        gateway_url: Some(s.fake.url()),
        setup_dir: Some(SETUP_DIR.into()),
        proving_key_sha256: Some(hash_of("request.pk")),
        withdrawal_key_sha256: Some(hash_of("withdrawal.pk")),
        direct: true,
        backup_recipient: Some(recipient),
        backup_places: vec![home.path().join("backup"), second_place.path().into()],
        ..Default::default()
    }
    .save(home.path())
    .unwrap();

    let registry = Registry::at(home.path());
    Spec {
        name: "cursor".into(),
        transport: Transport::Tcp,
        uid: None,
        group: None,
        gid: None,
        token_file: home.path().join("tokens").join("cursor.token"),
    }
    .register(&registry, &commitment, s.alice.id().as_str(), 1)
    .unwrap();
    policy::give_policy_text(&registry, "cursor", 1, &confirm_text());
    let agent_token =
        std::fs::read_to_string(home.path().join("tokens").join("cursor.token")).unwrap();

    let pass_file = out.path().join("pass");
    std::fs::write(&pass_file, PASS).unwrap();
    std::fs::set_permissions(&pass_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut serve = Command::new(env!("CARGO_BIN_EXE_nymirad"));
    serve
        .env("NYMIRA_MODE", "mainnet")
        .arg("--home")
        .arg(home.path())
        .arg("--passphrase-file")
        .arg(&pass_file)
        .arg("serve")
        .arg("--port")
        .arg(port.to_string())
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(out.path().join("serve.out")).unwrap())
        .stderr(std::fs::File::create(out.path().join("serve.err")).unwrap());
    // SAFETY: setsid is async-signal-safe and has no preconditions here.
    unsafe {
        serve.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    let serve = serve.spawn().unwrap();
    let owner_token = {
        let mut tries = 0;
        loop {
            match std::fs::read_to_string(home.path().join("owner.token")) {
                Ok(t) if !t.trim().is_empty() => break t,
                _ if tries > 600 => panic!("serve wrote no owner token"),
                _ => {
                    tries += 1;
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    };
    // The token file is written before the port is bound: wait for the listener.
    let mut serve = serve;
    for _ in 0..900 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
        {
            break;
        }
        if let Some(status) = serve.try_wait().unwrap() {
            let log = std::fs::read_to_string(out.path().join("serve.err")).unwrap_or_default();
            panic!("serve exited with {status}:\n{log}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Run {
        _s: s,
        home,
        out,
        second_place,
        backup_secret: private.as_str().to_string(),
        serve,
        port,
        agent_token,
        owner_token,
        owner_key,
    }
}

fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap().flatten() {
        let p = e.path();
        if p.is_dir() {
            files_under(&p, out);
        } else if p.is_file() {
            out.push(p);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_owner_command_shows_model_size_and_cost_next_to_each_approval_id() {
    let run = launch().await;
    // Before anything is held.
    let none = run.nymirad(&["agent", "approvals", "--port", &run.port.to_string()]);
    assert!(
        none.status.success() && String::from_utf8_lossy(&none.stdout).contains("Nothing waits"),
        "{}",
        String::from_utf8_lossy(&none.stderr)
    );

    let (status, reply) = run.hold(MARKER, BIG).await;
    assert_eq!(status, 402, "{reply}");
    let id = reply["error"]["approval_id"].as_str().unwrap().to_string();
    let (status, other) = run.hold("a second, larger question", BIG * 2).await;
    assert_eq!(status, 402, "{other}");
    let other_id = other["error"]["approval_id"].as_str().unwrap().to_string();
    assert_ne!(id, other_id);

    let port = run.port.to_string();
    let out = run.nymirad(&["agent", "approvals", "--port", &port]);
    let said = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{said}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Each held request has its own line, and that line carries the figures the route reports.
    for held in run.waiting().await {
        let id = held["approval_id"].as_str().unwrap();
        let line = said
            .lines()
            .find(|l| l.starts_with(id))
            .unwrap_or_else(|| panic!("no line for {id}:\n{said}"));
        let input = held["input_tokens"].as_u64().unwrap();
        let cost = held["estimated_cost_usd"].as_str().unwrap();
        assert!(input > 0 && cost.parse::<f64>().unwrap() > 0.001, "{held}");
        for part in [
            "agent cursor".to_string(),
            format!("model {MODEL}"),
            format!("input {input} tokens"),
            format!("up to {cost} USD"),
            "waiting".to_string(),
        ] {
            assert!(line.contains(&part), "`{part}` missing from: {line}");
        }
    }
    assert!(said.contains(&id) && said.contains(&other_id), "{said}");
    // The larger request is estimated higher than the smaller one.
    let cost_of = |want: &str| -> f64 {
        let line = said.lines().find(|l| l.starts_with(want)).unwrap();
        line.split("up to ")
            .nth(1)
            .unwrap()
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap()
    };
    assert!(cost_of(&other_id) > cost_of(&id));
}

#[tokio::test(flavor = "multi_thread")]
async fn held_request_bodies_reach_no_state_file_backup_or_log() {
    let run = launch().await;
    let port = run.port.to_string();
    let (status, reply) = run.hold(MARKER, BIG).await;
    assert_eq!(status, 402, "{reply}");
    let id = reply["error"]["approval_id"].as_str().unwrap().to_string();
    let held = run.waiting().await.remove(0);
    let hash = held["request_hash"].as_str().unwrap().to_string();
    let nonce = held["nonce"].as_str().unwrap().to_string();

    // A backup taken while the request is held.
    let backup1 = run.nymirad(&["backup"]);
    assert!(
        backup1.status.success(),
        "{}",
        String::from_utf8_lossy(&backup1.stderr)
    );

    // The owner approves through the binary; the signed file sits outside the state directory.
    let file = run.out.path().join("approval.json");
    std::fs::write(&file, run.signed_approval(&held).to_string()).unwrap();
    let approved = run.nymirad(&[
        "agent",
        "approve",
        "--file",
        file.to_str().unwrap(),
        "--port",
        &port,
    ]);
    let said = String::from_utf8_lossy(&approved.stdout).into_owned();
    assert!(
        approved.status.success() && said.contains(&format!("Approved {id}")),
        "{said}\n{}",
        String::from_utf8_lossy(&approved.stderr)
    );

    // The retry passes the approval check; what happens after it is not this test's concern.
    let body = json!({"model": MODEL, "stream": false, "max_tokens": BIG,
        "messages": [{"role": "user", "content": MARKER}]});
    let (status, retry) = run
        .http(
            "POST",
            "/v1/chat/completions",
            &run.agent_token,
            &[("x-nymira-approval-id", &id)],
            Some(body),
        )
        .await;
    let code = retry["error"]["code"].as_str().unwrap_or("");
    assert!(!code.starts_with("approval_"), "{status} {retry}");

    let backup2 = run.nymirad(&["backup"]);
    assert!(
        backup2.status.success(),
        "{}",
        String::from_utf8_lossy(&backup2.stderr)
    );

    // Scan: the state directory, both backup places (decrypted, so the scan reads what is inside),
    // and what the process printed.
    let needles = [MARKER, id.as_str(), hash.as_str(), nonce.as_str()];
    let clean = |what: &str, bytes: &[u8]| {
        let hay = String::from_utf8_lossy(bytes);
        for needle in needles {
            assert!(!hay.contains(needle), "{what} holds {needle}");
        }
    };
    let mut state = Vec::new();
    files_under(run.home.path(), &mut state);
    assert!(state.len() > 5, "{state:?}");
    for f in &state {
        clean(&f.display().to_string(), &std::fs::read(f).unwrap());
    }
    let mut copies = Vec::new();
    files_under(&run.home.path().join("backup"), &mut copies);
    files_under(run.second_place.path(), &mut copies);
    assert!(copies.len() >= 2, "a backup in each place: {copies:?}");
    for f in &copies {
        let raw = std::fs::read(f).unwrap();
        clean(&f.display().to_string(), &raw);
        let plain = backup::decrypt(&run.backup_secret, &raw)
            .unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        // The copy is real: it holds the agent registry, so the scan is not reading noise.
        if f.file_name().is_some_and(|n| n == backup::STATE_FILE) {
            assert!(String::from_utf8_lossy(&plain).contains("cursor"));
        }
        clean(&format!("{} (decrypted)", f.display()), &plain);
    }
    let (out_log, err_log) = run.logs();
    for log in [out_log, err_log] {
        clean(&log.display().to_string(), &std::fs::read(&log).unwrap());
    }
    for output in [&backup1, &approved] {
        clean("a command's stderr", &output.stderr);
    }
}
