//! Prints how many seconds the sealed NEAR AI refresh token has left, after one session call
//! (a balance read), so the exchange can move the expiry. Prints no token.
//!
//! `cargo run -p nymira-operator --example nearai_login_expiry`

use std::sync::Arc;

use nymira_operator::alarm::Alarms;
use nymira_operator::clock::SystemClock;
use nymira_operator::nearai::{NearAiAdapter, NearAiConfig, BROWSER_USER_AGENT};
use nymira_operator::sealed::SealedFile;

#[tokio::main]
async fn main() {
    let home = std::env::var("HOME").unwrap();
    let env = std::fs::read_to_string(format!("{home}/.nymira-operator.env")).expect("settings");
    let s: std::collections::HashMap<String, String> = env
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().into(), v.trim().into()))
        .collect();
    let get = |k: &str| s.get(k).cloned().unwrap_or_default();
    let dir = format!("{home}/.nymira-live");
    let pass = get("NYMIRA_SEAL_PASSPHRASE");
    let open =
        |name: &str| SealedFile::open(format!("{dir}/{name}").as_ref(), &pass, 600_000).unwrap();
    let org = get("NEAR_AI_ORG_ID");
    let adapter = NearAiAdapter::new(
        NearAiConfig {
            user_agent: BROWSER_USER_AGENT.into(),
            base_url: "https://cloud-api.near.ai".into(),
            org_id: org.clone(),
            workspace_id: get("NEAR_AI_WORKSPACE_ID"),
        },
        Arc::new(SystemClock),
        Arc::new(Alarms::new(vec![], Arc::new(SystemClock))),
        open("login.enc"),
        open("keys.enc"),
        None,
    )
    .expect("adapter");
    println!("before: {:?} s left", adapter.check_login_expiry().await);
    let r = adapter
        .session_get(&format!("/v1/organizations/{org}/usage/balance"))
        .await;
    println!("session call ok: {}", r.is_ok());
    println!("after:  {:?} s left", adapter.check_login_expiry().await);
}
