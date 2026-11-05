//! The minter reads the first refresh token only while there is no sealed login: a restart after
//! the first start must work although the config still names a first-token file that is gone.

use std::os::unix::fs::PermissionsExt;

use nymira_operatord::config::Config;
use nymira_operatord::roles::first_refresh_token;
use serde_json::json;

fn config(data_dir: &std::path::Path, first: &std::path::Path) -> Config {
    let v = json!({
        "profile": "mainnet", "network": "mainnet", "vault_account": "vault.nymira.near",
        "operator_account": "operator.nymira.near", "answer_account": "answer.nymira.near",
        "host": "operator.example", "web_origin": "https://nymira.pages.dev",
        "listen": "127.0.0.1:8443", "run_dir": "/run/nymira", "data_dir": data_dir,
        "secrets_dir": "/etc/nymira", "setup_dir": "/opt/nymira/setup",
        "rpc_urls": ["https://rpc.mainnet.near.org", "https://free.rpc.fastnear.com", "https://near.lava.build"],
        "archival_rpc_url": "https://archival-rpc.mainnet.near.org",
        "usage_public_key": "00",
        "near_ai": {
            "base_url": "https://cloud-api.near.ai", "org_id": "o", "workspace_id": "w",
            "first_refresh_token_file": first,
        },
        "replica": {"url": "http://100.64.0.2:9000", "public_key": "00"},
    });
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("c.json");
    std::fs::write(&path, v.to_string()).unwrap();
    Config::load(&path).unwrap()
}

#[test]
fn a_missing_first_token_file_is_ignored_once_the_sealed_login_exists() {
    let data = tempfile::tempdir().unwrap();
    std::fs::write(data.path().join("nearai-login.sealed"), b"sealed").unwrap();
    let gone = data.path().join("first_refresh_token");
    let cfg = config(data.path(), &gone);
    assert_eq!(first_refresh_token(&cfg).unwrap(), None);
}

#[test]
fn without_a_sealed_login_the_first_token_file_is_required_and_read() {
    let data = tempfile::tempdir().unwrap();
    let file = data.path().join("first_refresh_token");
    let cfg = config(data.path(), &file);
    // No login and no token file: the start fails, so a missing token is never silent.
    assert!(first_refresh_token(&cfg).is_err());
    // The token file exists (mode 0400, owned by this user): its text is returned, trimmed.
    std::fs::write(&file, b"rt_abc\n").unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o400)).unwrap();
    assert_eq!(
        first_refresh_token(&cfg).unwrap().as_deref(),
        Some("rt_abc")
    );
}

#[test]
fn no_first_token_file_in_the_config_means_none() {
    let data = tempfile::tempdir().unwrap();
    let mut cfg = config(data.path(), &data.path().join("x"));
    cfg.near_ai.first_refresh_token_file = None;
    assert_eq!(first_refresh_token(&cfg).unwrap(), None);
}
