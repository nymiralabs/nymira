//! Gateway checks left over from day 1: key creation rate, a server-side tool request and a
//! stored-response read. Tiny, `z-ai/glm-5.3-flash` only, prints no token.
//! `z-ai/glm-5.3-flash` only, a few cents at most, prints no token.
//!
//! `cargo run -p nymira-operator --example nearai_misc_live`

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nymira_operator::alarm::Alarms;
use nymira_operator::clock::SystemClock;
use nymira_operator::nearai::{LeaseChat, NearAiAdapter, NearAiConfig, BROWSER_USER_AGENT};
use nymira_operator::provider::{ChatRequest, ProviderAdapter};
use nymira_operator::sealed::SealedFile;

const MODEL: &str = "z-ai/glm-5.3-flash";

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
    // 1. creation rate: 12 keys back to back, time each.
    let mut ids = vec![];
    let t0 = std::time::Instant::now();
    for i in 0..12 {
        let id = format!("rate-{}-{i}", std::process::id());
        let t = std::time::Instant::now();
        match adapter.mint(&id, 2_060).await {
            Ok(_) => {
                println!("mint {i}: {:?}", t.elapsed());
                ids.push(id);
            }
            Err(e) => {
                println!("mint {i} refused: {e}");
                break;
            }
        }
    }
    println!("{} keys in {:?}", ids.len(), t0.elapsed());
    tokio::time::sleep(Duration::from_secs(10)).await;
    // 2. server-side tool and stored response, on the first key.
    let lease = ids[0].clone();
    let secret = adapter.secret(&lease).expect("secret");
    for (name, body) in [
        (
            "tools web_search",
            serde_json::json!({"model": MODEL, "max_tokens": 50,
            "tools": [{"type": "web_search"}],
            "messages": [{"role": "user", "content": "What is the weather in Paris today?"}]}),
        ),
        (
            "store:true",
            serde_json::json!({"model": MODEL, "max_tokens": 20, "store": true,
            "messages": [{"role": "user", "content": "Say hi."}]}),
        ),
    ] {
        let req = ChatRequest {
            body: body.to_string().into(),
            headers: vec![],
        };
        match lc.chat(&secret, &lease, req).await {
            Err(e) => println!("{name}: refused: {e}"),
            Ok(mut reply) => {
                let mut n = 0;
                let mut first = Vec::new();
                while let Some(Ok(c)) = reply.body.next_chunk().await {
                    if first.len() < 300 {
                        first.extend_from_slice(&c[..c.len().min(300)]);
                    }
                    n += c.len();
                }
                println!(
                    "{name}: ok {n} bytes; head: {}",
                    String::from_utf8_lossy(&first[..first.len().min(300)])
                );
            }
        }
    }
    for id in ids {
        let _ = adapter.revoke(&id).await;
    }
}
