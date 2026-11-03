//! The profiles of the configuration file: a mainnet operator needs three providers for its
//! watcher and an archival endpoint, and its watcher asks for all three to agree.

use nymira_operatord::config::Config;
use serde_json::{json, Value};

fn mainnet() -> Value {
    json!({
        "profile": "mainnet", "network": "mainnet", "vault_account": "vault.nymira.near",
        "operator_account": "operator.nymira.near", "answer_account": "answer.nymira.near",
        "host": "operator.example", "web_origin": "https://nymira.pages.dev",
        "listen": "127.0.0.1:8443", "run_dir": "/run/nymira", "data_dir": "/var/nymira",
        "secrets_dir": "/etc/nymira", "setup_dir": "/opt/nymira/setup",
        "rpc_urls": ["https://rpc.mainnet.near.org", "https://free.rpc.fastnear.com", "https://near.lava.build"],
        "archival_rpc_url": "https://archival-rpc.mainnet.near.org",
        "usage_public_key": "00",
        "near_ai": {"base_url": "https://cloud-api.near.ai", "org_id": "o", "workspace_id": "w"},
        "replica": {"url": "http://100.64.0.2:9000", "public_key": "00"},
    })
}

fn load(v: &Value) -> Result<Config, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.json");
    std::fs::write(&path, v.to_string()).unwrap();
    Config::load(&path)
}

#[test]
fn a_mainnet_config_has_quorum_three_and_an_archival_endpoint() {
    let config = load(&mainnet()).unwrap();
    assert_eq!(config.watcher_quorum(), 3);
    assert_eq!(config.rpc_urls.len(), 3);
    assert!(config.archival_rpc_url.is_some());

    let mut two = mainnet();
    two["rpc_urls"].as_array_mut().unwrap().pop();
    assert!(load(&two).unwrap_err().contains("at least 3 rpc_urls"));

    let mut no_archive = mainnet();
    no_archive
        .as_object_mut()
        .unwrap()
        .remove("archival_rpc_url");
    assert!(load(&no_archive).unwrap_err().contains("archival_rpc_url"));
}

#[test]
fn the_testnet_quorum_stays_two() {
    let mut v = mainnet();
    v["profile"] = "testnet".into();
    v["network"] = "testnet".into();
    v.as_object_mut().unwrap().remove("archival_rpc_url");
    v["rpc_urls"].as_array_mut().unwrap().pop();
    assert_eq!(load(&v).unwrap().watcher_quorum(), 2);
}

#[test]
fn the_watcher_role_asks_for_the_config_quorum() {
    let text =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/roles.rs")).unwrap();
    assert!(text.contains("quorum: cfg.watcher_quorum()"));
}
