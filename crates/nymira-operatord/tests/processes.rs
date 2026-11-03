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
    // On Linux the descriptors of a process that set `PR_SET_DUMPABLE=0` cannot be read, so
    // listeners are counted as the new ones in the kernel's table after the processes started.
    #[cfg(target_os = "linux")]
    let listening_before = tcp_listening_now();
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
    #[cfg(target_os = "linux")]
    {
        let new: Vec<String> = tcp_listening_now()
            .into_iter()
            .filter(|l| !listening_before.contains(l))
            .collect();
        assert_eq!(
            new,
            [format!("127.0.0.1:{port}")],
            "the processes listen on"
        );
    }
    #[cfg(not(target_os = "linux"))]
    for (name, child) in &procs.0 {
        let names = tcp_listeners(child.id());
        if name == "api" {
            assert_eq!(names, [format!("127.0.0.1:{port}")]);
        } else {
            assert!(names.is_empty(), "{name} listens on TCP: {names:?}");
        }
    }

    // The key-leak stop reaches the running API through the database.
    run(&conf("api"), &["emergency", "set", "test stop"]);
    // The API caches the flag for a second, so it shows within two.
    let started = Instant::now();
    let status: Value = loop {
        let status: Value = http
            .get(format!("{url}/v1/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if status["emergency_notice"] == "test stop" {
            break status;
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the stop never showed"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    // Watchers write their poll time where the API shows it.
    assert!(status["watchers"].as_array().is_some());

    // The signer has no TCP entry: a typed signer request sent to the one TCP port that exists
    // gets nothing a signer would say.
    {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut tcp = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        tcp.write_all(b"{\"op\":\"state_key\"}\n").await.unwrap();
        let mut answer = Vec::new();
        let _ = tokio::time::timeout(Duration::from_secs(3), tcp.read_to_end(&mut answer)).await;
        let answer = String::from_utf8_lossy(&answer);
        assert!(
            !answer.contains("state_key") && !answer.contains("refused"),
            "{answer}"
        );
    }

    // Kill the signer and start it again: the API shows no signed nonce while it is down, and a
    // nonce that verifies under the same state key once it is back.
    let at = procs.0.iter().position(|(n, _)| n == "signer").unwrap();
    procs.0[at].1.kill().unwrap();
    procs.0[at].1.wait().unwrap();
    let started = Instant::now();
    loop {
        let s: Value = http
            .get(format!("{url}/v1/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if s["nonce"].is_null() || s["nonce_sig"].is_null() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "a dead signer still signs: {s}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    procs.0[at] = spawn("signer", Some(&format!("{state_pass}\n{clearance_pass}\n")));
    let started = Instant::now();
    loop {
        let s: Value = http
            .get(format!("{url}/v1/status"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if let (Some(nonce), Some(ts)) = (s["nonce"]["nonce"].as_str(), s["nonce"]["ts"].as_u64()) {
            if let Ok(sig) =
                serde_json::from_value::<zkapi_types::SchnorrSignature>(s["nonce_sig"].clone())
            {
                if zkapi_proof::compact::verify_signature(
                    &sp,
                    &nymira_operator::status::status_message(nonce, "operator.test", ts),
                    &sig,
                )
                .unwrap()
                {
                    break;
                }
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(90),
            "the restarted signer never signed again"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Every TCP address some process of this user listens on, from the kernel's tables (Linux).
#[cfg(target_os = "linux")]
fn tcp_listening_now() -> Vec<String> {
    let uid = std::fs::metadata("/proc/self")
        .map(|m| std::os::unix::fs::MetadataExt::uid(&m))
        .unwrap();
    let mut out = Vec::new();
    for (file, v6) in [("/proc/net/tcp", false), ("/proc/net/tcp6", true)] {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let f: Vec<&str> = line.split_whitespace().collect();
            // local_address, state (0A is LISTEN), ..., uid.
            if f.len() > 7 && f[3] == "0A" && f[7].parse::<u32>() == Ok(uid) {
                let (addr, port) = f[1].split_once(':').unwrap();
                let port = u16::from_str_radix(port, 16).unwrap();
                let host = if v6 {
                    format!("[{addr}]")
                } else {
                    let n = u32::from_str_radix(addr, 16).unwrap().to_le_bytes();
                    format!("{}.{}.{}.{}", n[0], n[1], n[2], n[3])
                };
                out.push(format!("{host}:{port}"));
            }
        }
    }
    out
}

/// The TCP addresses `pid` listens on (`lsof`, where the descriptors can be read).
#[cfg(not(target_os = "linux"))]
fn tcp_listeners(pid: u32) -> Vec<String> {
    let out = Command::new("lsof")
        .args([
            "-nP",
            "-a",
            "-iTCP",
            "-sTCP:LISTEN",
            "-Fn",
            "-p",
            &pid.to_string(),
        ])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.strip_prefix('n').map(String::from))
        .collect()
}
