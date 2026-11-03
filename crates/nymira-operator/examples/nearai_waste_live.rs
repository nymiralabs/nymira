//! Per-lease waste on the live gateway: for several request sizes, one lease key, one chat, the
//! measured cost from the usage history, and the charge the vault formula gives for it.
//! `z-ai/glm-5.3-flash` only, a few cents at most, prints no token.
//!
//! `cargo run -p nymira-operator --example nearai_waste_live`

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nymira_operator::alarm::Alarms;
use nymira_operator::charge::{charge, ChargeParams};
use nymira_operator::clock::SystemClock;
use nymira_operator::nearai::{LeaseChat, NearAiAdapter, NearAiConfig, BROWSER_USER_AGENT};
use nymira_operator::provider::{ChatRequest, ProviderAdapter};
use nymira_operator::sealed::SealedFile;

const MODEL: &str = "z-ai/glm-5.3-flash";
const BOUND: u128 = 20_000; // micro-USD

fn settings() -> HashMap<String, String> {
    let path = format!("{}/.nymira-operator.env", std::env::var("HOME").unwrap());
    std::fs::read_to_string(path)
        .expect("settings file")
        .lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().into(), v.trim().into()))
        .collect()
}

#[tokio::main]
async fn main() {
    let s = settings();
    let get = |k: &str| s.get(k).cloned().unwrap_or_default();
    let dir = format!("{}/.nymira-live", std::env::var("HOME").unwrap());
    let pass = get("NYMIRA_SEAL_PASSPHRASE");
    let open =
        |name: &str| SealedFile::open(format!("{dir}/{name}").as_ref(), &pass, 600_000).unwrap();
    let adapter = NearAiAdapter::new(
        NearAiConfig {
            user_agent: BROWSER_USER_AGENT.into(),
            base_url: "https://cloud-api.near.ai".into(),
            org_id: get("NEAR_AI_ORG_ID"),
            workspace_id: get("NEAR_AI_WORKSPACE_ID"),
        },
        Arc::new(SystemClock),
        Arc::new(Alarms::new(vec![], Arc::new(SystemClock))),
        open("login.enc"),
        open("keys.enc"),
        None,
    )
    .expect("adapter");
    let http = reqwest::Client::builder()
        .user_agent(BROWSER_USER_AGENT)
        .build()
        .unwrap();
    let lc = LeaseChat::new(http, "https://cloud-api.near.ai");
    let p = ChargeParams {
        step: 1_000,
        min_charge: 2_000,
        slot_rate: 0,
    };

    // (prompt words, max_tokens, instruction)
    let cases = [
        (20usize, 50u32, "Answer in one short sentence."),
        (200, 300, "Summarise the text in three sentences."),
        (1_000, 700, "Summarise the text in a paragraph."),
        (4_000, 1_500, "Summarise the text in detail."),
    ];
    println!("request | cost (micro-USD) | charge | waste | waste/charge");
    for (i, (words, max_tokens, ask)) in cases.iter().enumerate() {
        let lease = format!("waste-{}-{i}", std::process::id());
        adapter.mint(&lease, BOUND).await.expect("mint");
        tokio::time::sleep(Duration::from_secs(10)).await;
        let secret = adapter.secret(&lease).expect("secret");
        let text = "lighthouse keeper storm harbour ".repeat(*words / 4);
        let body = serde_json::json!({
            "model": MODEL, "max_tokens": max_tokens,
            "messages": [{"role": "user", "content": format!("{ask}\n\n{text}")}]
        });
        let req = ChatRequest {
            body: body.to_string().into(),
            headers: vec![],
        };
        match lc.chat(&secret, &lease, req).await {
            Err(e) => println!("case {i}: refused: {e}"),
            Ok(mut reply) => while let Some(Ok(_)) = reply.body.next_chunk().await {},
        }
        tokio::time::sleep(Duration::from_secs(8)).await;
        let cost = adapter
            .usage(&lease)
            .await
            .map(|u| u.cost_nano_usd)
            .unwrap_or(0);
        let billed = charge(BOUND, cost, 0, p);
        let meter = (cost * 103) / 100_000; // micro-USD at cost x 1.03
        let waste = billed.saturating_sub(meter);
        println!(
            "{words} words in, max {max_tokens} out | {meter} | {billed} | {waste} | {}%",
            if billed == 0 { 0 } else { waste * 100 / billed }
        );
        let _ = adapter.revoke(&lease).await;
    }
}
