//! A full local run: the five real processes, each with only the secrets it needs, against the
//! fake NEAR RPC and the fake NEAR AI gateway. Needs the rehearsal verifying keys:
//! `cargo test -p nymira-operatord --features dev-setup`.
#![cfg(feature = "dev-setup")]

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use nymira_fake_near_rpc::{ArgsMatch, FakeRpc, ViewResult};
use nymira_operator::operator::felt_hex;
use nymira_operator::signer::{seeds_from_passphrases, Signer, KDF_ROUNDS};
use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_nymira-operatord");
const VAULT: &str = "vault.nymira.testnet";
const SETUP: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../vendor/zkapi/protocol/setup/v2-concession-dev"
);

struct Procs(Vec<(String, Child)>);

impl Drop for Procs {
    fn drop(&mut self) {
        for (_, child) in &mut self.0 {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

fn run(config: &Path, args: &[&str]) -> String {
    let out = Command::new(BIN)
        .arg("--config")
        .arg(config)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn word(f: &zkapi_types::Felt252) -> String {
    felt_hex(f)
}

#[tokio::test(flavor = "multi_thread")]
async fn five_processes_each_with_only_its_own_secrets_and_one_tcp_listener() {
    let root = tempfile::tempdir().unwrap();
    let base = root.path();
    let (run_dir, data_dir) = (base.join("run"), base.join("data"));
    let port = free_port();

    // One secrets directory per process: the code of a role reads only what its directory holds.
    let dirs: Vec<(&str, PathBuf)> = ["api", "minter", "signer", "closer", "watcher"]
        .iter()
        .map(|r| (*r, base.join("secrets").join(r)))
        .collect();
    let dir = |role: &str| dirs.iter().find(|(r, _)| *r == role).unwrap().1.clone();

    // The fakes.
    let rpc = FakeRpc::start().await;
    let gateway = nymira_fake_near_ai::start("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    let token_file = base.join("first-token");
    std::fs::write(&token_file, nymira_fake_near_ai::FIRST_REFRESH_TOKEN).unwrap();
    std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o400)).unwrap();

    // Passphrases typed at start-up, and the keys they give: the vault must name them.
    let (state_pass, clearance_pass) = (
        "state passphrase for the test",
        "clearance passphrase, other",
    );
    let (s_seed, c_seed) = seeds_from_passphrases(state_pass, clearance_pass, KDF_ROUNDS).unwrap();
    let (state, clearance) = Signer::signers_from_seeds(&s_seed, &c_seed, true).unwrap();
    let (sp, cp) = (state.public_key(), clearance.public_key());
    let words = |p: &zkapi_types::wire::CurvePointWire| json!({"x": word(&p.x), "y": word(&p.y)});
    let chain = nymira_near_codec::chain_id(nymira_near_codec::Network::Testnet);
    let view = |method: &str, result: Value| {
        rpc.set_view(VAULT, method, ArgsMatch::Any, ViewResult::json(result));
    };
    view(
        "config",
        json!({
            "operator_pk": words(&sp), "clearance_pk": words(&cp),
            "operator_account": "operator.nymira.testnet", "token": "usdc.testnet",
            "treasury": "t.testnet", "owner": "o.testnet", "chain_id": chain.to_string(),
            "denominations": ["1000000"], "note_fee": "10000", "close_fee": "10000",
            "bound_classes": ["100000", "500000"], "charge_step": "1000", "min_charge": "2000",
            "slot_rate": 1000, "tvl_cap": "100000000", "current_tvl": "0",
            "exit_delay_secs": 604800, "note_ttl": 86400
        }),
    );
    view("current_root", json!(format!("0x{}", "09".repeat(32))));
    view("retirement_epoch", json!("0"));
    view("open_notes", json!(0));
    view("liquid_near", json!("5000000000000000000000000"));
    view("exit_requests", json!([]));

    // The configuration, one per process (the secrets directory differs).
    let config_for = |role: &str| -> PathBuf {
        let path = base.join(format!("{role}.json"));
        let text = json!({
            "profile": "sandbox", "network": "testnet", "vault_account": VAULT,
            "operator_account": "operator.nymira.testnet", "answer_account": "answer.nymira.testnet",
            "host": "operator.test", "web_origin": "https://nymira.pages.dev",
            "listen": format!("127.0.0.1:{port}"),
            "run_dir": run_dir, "data_dir": data_dir, "secrets_dir": dir(role),
            "setup_dir": SETUP,
            "rpc_urls": [rpc.url(), rpc.url()],
            "usage_public_key": std::fs::read_to_string(base.join("usage.pub")).unwrap_or_default(),
            "near_ai": {"base_url": gateway.url(), "org_id": "org", "workspace_id": "ws",
                        "first_refresh_token_file": token_file},
        });
        std::fs::write(&path, text.to_string()).unwrap();
        path
    };

    // Secret files, made by the tool itself.
    for (role, d) in &dirs {
        std::fs::create_dir_all(d).unwrap();
        let _ = role;
    }
    std::fs::create_dir_all(&run_dir).unwrap();
    std::fs::set_permissions(&run_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let boot = config_for("minter");
    let usage_pub = run(&boot, &["keygen", "usage"]);
    std::fs::write(base.join("usage.pub"), &usage_pub).unwrap();
    run(&config_for("signer"), &["keygen", "heartbeat"]);
    run(&config_for("closer"), &["keygen", "mutual_close"]);
    run(&config_for("watcher"), &["keygen", "upkeep"]);
    run(&config_for("watcher"), &["keygen", "answer"]);
    run(&config_for("watcher"), &["keygen", "entry"]);
    // The usage key's secret belongs to the minter alone; the signer needs only its public half.
    let configs: Vec<(&str, PathBuf)> = dirs.iter().map(|(r, _)| (*r, config_for(r))).collect();
    let conf = |role: &str| configs.iter().find(|(r, _)| *r == role).unwrap().1.clone();

    let spawn = |role: &str, stdin: Option<&str>| -> (String, Child) {
        let mut child = Command::new(BIN)
            .arg("--config")
            .arg(conf(role))
            .arg(role)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        if let Some(text) = stdin {
            child
                .stdin
                .take()
                .unwrap()
                .write_all(text.as_bytes())
                .unwrap();
        }
        (role.to_string(), child)
    };
    let mut procs = Procs(vec![]);
    procs.0.push(spawn("minter", Some("seal passphrase\n")));
    procs.0.push(spawn(
        "signer",
        Some(&format!("{state_pass}\n{clearance_pass}\n")),
    ));
    procs.0.push(spawn("closer", None));
    procs.0.push(spawn("watcher", None));
    for socket in ["minter", "signer", "closer", "replica"] {
        let path = run_dir.join(format!("{socket}.sock"));
        let started = Instant::now();
        while !path.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(60),
                "{socket} never came up"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    procs.0.push(spawn("api", None));

    let http = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}");
    let started = Instant::now();
    let status: Value = loop {
        if let Ok(r) = http.get(format!("{url}/v1/status")).send().await {
            if r.status().is_success() {
                break r.json().await.unwrap();
            }
        }
        for (name, child) in &mut procs.0 {
            assert!(child.try_wait().unwrap().is_none(), "{name} exited early");
        }
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "the API never answered"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    };

    // The API answers with the signer's signature on the nonce: the state key is not in its memory.
    assert_eq!(status["operator_pk"]["x"], words(&sp)["x"]);
    let nonce = status["nonce"]["nonce"].as_str().expect("a signed nonce");
    let ts = status["nonce"]["ts"].as_u64().unwrap();
    let sig: zkapi_types::SchnorrSignature =
        serde_json::from_value(status["nonce_sig"].clone()).unwrap();
    assert!(zkapi_proof::compact::verify_signature(
        &sp,
        &nymira_operator::status::status_message(nonce, "operator.test", ts),
        &sig
    )
    .unwrap());

    // Sockets are 0600; secret files are 0400; the API's directory holds nothing.
    for name in ["minter", "signer", "closer", "replica"] {
        let mode = std::fs::metadata(run_dir.join(format!("{name}.sock")))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "{name} socket");
    }
    for (role, d) in &dirs {
        for entry in std::fs::read_dir(d).unwrap() {
            let mode = entry.unwrap().metadata().unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o400, "a secret of {role}");
        }
    }
    assert_eq!(std::fs::read_dir(dir("api")).unwrap().count(), 0);
    assert_eq!(
        std::fs::read_dir(dir("minter")).unwrap().count(),
        1,
        "the usage key"
    );
    assert_eq!(
        std::fs::read_dir(dir("signer")).unwrap().count(),
        1,
        "the heartbeat key"
    );
    assert_eq!(
        std::fs::read_dir(dir("closer")).unwrap().count(),
        1,
        "the mutual_close key"
    );

    // No TCP listener except the API's.
    for (name, child) in &procs.0 {
        let out = Command::new("lsof")
            .args([
                "-nP",
                "-a",
                "-iTCP",
                "-sTCP:LISTEN",
                "-Fn",
                "-p",
                &child.id().to_string(),
            ])
            .output()
            .unwrap();
        let names: Vec<String> = String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.starts_with('n'))
            .map(String::from)
            .collect();
        if name == "api" {
            assert_eq!(names, [format!("n127.0.0.1:{port}")]);
        } else {
            assert!(names.is_empty(), "{name} listens on TCP: {names:?}");
        }
    }

    // The key-leak stop reaches the running API through the database.
    run(&conf("api"), &["emergency", "set", "test stop"]);
    let status: Value = http
        .get(format!("{url}/v1/status"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["emergency_notice"], "test stop");
    // Watchers write their poll time where the API shows it.
    assert!(status["watchers"].as_array().is_some());
}
