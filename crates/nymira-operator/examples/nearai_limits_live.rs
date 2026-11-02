//! Live checks of the gateway's limits, with tiny keys and `z-ai/glm-5.3-flash` only:
//!
//! 1. Does a key stop at its spend cap, and by how much does it overspend (one request at a
//!    time, and several at once)?
//! 2. Does `signature/{id}` still answer after the key is deactivated and deleted?
//! 3. How many keys can the workspace hold (up to 30 are made, then all are revoked)?
//!
//! Costs a few cents at most. Prints no token. Uses the sealed login of `nearai_live`.
//!
//! `cargo run -p nymira-operator --example nearai_limits_live`

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use nymira_operator::alarm::Alarms;
use nymira_operator::clock::SystemClock;
use nymira_operator::nearai::{LeaseChat, NearAiAdapter, NearAiConfig, BROWSER_USER_AGENT};
use nymira_operator::provider::{key_cap_nano_usd, ChatRequest, EvidenceQuery, ProviderAdapter};
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

fn body(max_tokens: u32) -> ChatRequest {
    ChatRequest {
        body: format!(
            r#"{{"model":"{MODEL}","max_tokens":{max_tokens},"messages":[{{"role":"user","content":"Write a long story about a lighthouse."}}]}}"#
        )
        .into(),
        headers: vec![],
    }
}

async fn send(lc: &LeaseChat, secret: &str, lease: &str, max_tokens: u32) -> String {
    match lc.chat(secret, lease, body(max_tokens)).await {
        Err(e) => format!("refused: {e}"),
        Ok(mut reply) => {
            let mut n = 0;
            while let Some(Ok(c)) = reply.body.next_chunk().await {
                n += c.len();
            }
            format!("ok ({n} bytes)")
        }
    }
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

    // ---- 1. the cap. A bound of 2,060 micro-USD gives a cap of 2,000,000 nano-USD ($0.002).
    let lease = format!("cap-{}", std::process::id());
    let cap = key_cap_nano_usd(2_060);
    adapter.mint(&lease, 2_060).await.expect("mint");
    tokio::time::sleep(Duration::from_secs(8)).await;
    let secret = adapter.secret(&lease).expect("secret");
    println!("cap: {cap} nano-USD");
    let mut refused_at = None;
    for i in 1..=30 {
        let r = send(&lc, &secret, &lease, 400).await;
        tokio::time::sleep(Duration::from_secs(3)).await; // usage lags
        let used = adapter
            .usage(&lease)
            .await
            .map(|u| u.cost_nano_usd)
            .unwrap_or(0);
        println!("sequential {i}: {r}; usage so far {used}");
        if r.starts_with("refused") {
            refused_at = Some(used);
            break;
        }
    }
    println!(
        "sequential: refused at usage {:?} (cap {cap}); overspend {:?}",
        refused_at,
        refused_at.map(|u| u as i128 - cap as i128)
    );

    // ---- 1b. a fresh key, eight requests at once, near the cap.
    let burst = format!("burst-{}", std::process::id());
    adapter.mint(&burst, 2_060).await.expect("mint");
    tokio::time::sleep(Duration::from_secs(8)).await;
    let bsecret = adapter.secret(&burst).expect("secret");
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let (lc, secret, burst) = (lc.clone(), bsecret.clone(), burst.clone());
        tasks.push(tokio::spawn(async move {
            send(&lc, &secret, &burst, 400).await
        }));
    }
    let mut ok = 0;
    for t in tasks {
        if t.await.unwrap().starts_with("ok") {
            ok += 1;
        }
    }
    tokio::time::sleep(Duration::from_secs(10)).await;
    let used = adapter
        .usage(&burst)
        .await
        .map(|u| u.cost_nano_usd)
        .unwrap_or(0);
    println!(
        "burst of 8: {ok} answered, usage {used} (cap {cap}), overspend {}",
        used as i128 - cap as i128
    );

    // ---- 2. signature after the key is deactivated and deleted.
    let sig = format!("sig-{}", std::process::id());
    adapter.mint(&sig, 2_060).await.expect("mint");
    tokio::time::sleep(Duration::from_secs(8)).await;
    let ssecret = adapter.secret(&sig).expect("secret");
    println!(
        "signature probe chat: {}",
        send(&lc, &ssecret, &sig, 5).await
    );
    let q = EvidenceQuery {
        nonce: "ab".repeat(32),
        signing_algo: "ed25519".into(),
    };
    let before = lc.evidence(&ssecret, &sig, &q).await;
    println!("evidence before revoke: {}", summary(&before));
    adapter.revoke(&sig).await.expect("revoke");
    let after = lc.evidence(&ssecret, &sig, &q).await;
    println!("evidence after revoke:  {}", summary(&after));

    // ---- 3. key count.
    let mut made = Vec::new();
    for i in 0..30 {
        let id = format!("count-{}-{i}", std::process::id());
        match adapter.mint(&id, 2_060).await {
            Ok(_) => made.push(id),
            Err(e) => {
                println!("key {} refused: {e}", i + 1);
                break;
            }
        }
    }
    println!("keys made: {}", made.len());
    for id in made {
        let _ = adapter.revoke(&id).await;
    }
    let _ = adapter.revoke(&lease).await;
    let _ = adapter.revoke(&burst).await;
}

fn summary(
    r: &Result<Option<serde_json::Value>, nymira_operator::provider::ProviderError>,
) -> String {
    match r {
        Ok(Some(v)) => format!(
            "{} signature(s), attestation {}",
            v["signatures"].as_array().map_or(0, Vec::len),
            if v["attestation"].is_null() {
                "none"
            } else {
                "present"
            }
        ),
        Ok(None) => "none".into(),
        Err(e) => format!("error: {e}"),
    }
}
