//! The second machine's process: it answers with a head signed by its own key, and it refuses
//! an entry the operator's key did not sign.

use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_nymira-operatord");

struct Proc(Child);

impl Drop for Proc {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_replica_process_signs_its_head_and_refuses_an_unsigned_entry() {
    let base = tempfile::tempdir().unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let config = base.path().join("replica.json");
    let write = |entry_pk: &str| {
        std::fs::write(
            &config,
            json!({
                "profile": "testnet", "network": "testnet", "vault_account": "vault.nymira.testnet",
                "operator_account": "operator.nymira.testnet", "answer_account": "answer.nymira.testnet",
                "host": "replica.test", "web_origin": "https://nymira.pages.dev",
                "listen": "127.0.0.1:1", "run_dir": base.path().join("run"),
                "data_dir": base.path().join("data"), "secrets_dir": base.path().join("secrets"),
                "setup_dir": base.path(), "rpc_urls": ["http://127.0.0.1:1"],
                "usage_public_key": "00", "near_ai": {"base_url": "http://127.0.0.1:1", "org_id": "o", "workspace_id": "w"},
                "replica_node": {"listen": format!("127.0.0.1:{port}"), "entry_public_key": entry_pk},
            })
            .to_string(),
        )
        .unwrap();
    };
    write(&"00".repeat(32));
    let keygen = |name: &str| {
        let out = Command::new(BIN)
            .args(["--config", config.to_str().unwrap(), "keygen", name])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    };
    let replica_pk = keygen("replica");
    let entry_pk = keygen("entry");
    write(&entry_pk);

    let _proc = Proc(
        Command::new(BIN)
            .args(["--config", config.to_str().unwrap(), "replica"])
            .stdin(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let http = reqwest::Client::new();
    let url = format!("http://127.0.0.1:{port}");
    let started = Instant::now();
    let head: Value = loop {
        if let Ok(r) = http.get(format!("{url}/replica/head")).send().await {
            if r.status().is_success() {
                break r.json().await.unwrap();
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "the replica never answered"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(head["seq"], 0);
    let signed: nymira_operator::replog::SignedHead = serde_json::from_value(head).unwrap();
    let pk: [u8; 32] = hex::decode(replica_pk).unwrap().try_into().unwrap();
    assert!(nymira_operator::replog::verify_head(&pk, &signed));

    // An entry without the operator's signature is refused.
    let unsigned = json!({"seq": 1, "prev": "0".repeat(64), "kind": "x", "key": "k", "hour": 0,
        "payload_hash": "", "payload": null, "hash": ""});
    let r = http
        .post(format!("{url}/replica/append"))
        .json(&unsigned)
        .send()
        .await
        .unwrap();
    assert!(!r.status().is_success());
}
