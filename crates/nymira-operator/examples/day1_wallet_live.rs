//! Live first-day measurements that ticket #31 needed (`docs/day-1-checks.md`):
//!
//! 1. Overspend from requests that run at the same time at the cap: three fresh keys capped at
//!    $0.002, 24 requests at once on each.
//! 2. How long the usage reading lags a finished chat (eight samples).
//! 3. Whether the gateway's signature covers a streamed answer: the signed text against the
//!    SHA-256 of the exact request and response bytes, streamed and not.
//!
//! Costs about two cents with `z-ai/glm-5.3-flash`. Prints no token.
//!
//! `cargo run -p nymira-operator --example day1_wallet_live`

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nymira_operator::alarm::Alarms;
use nymira_operator::clock::SystemClock;
use nymira_operator::nearai::{LeaseChat, NearAiAdapter, NearAiConfig, BROWSER_USER_AGENT};
use nymira_operator::provider::{key_cap_nano_usd, ChatRequest, EvidenceQuery, ProviderAdapter};
use nymira_operator::sealed::SealedFile;
use sha2::{Digest, Sha256};

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

fn body(max_tokens: u32, stream: bool, prompt: &str) -> Vec<u8> {
    format!(
        r#"{{"model":"{MODEL}","max_tokens":{max_tokens},"stream":{stream}{},"messages":[{{"role":"user","content":"{prompt}"}}]}}"#,
        if stream {
            r#","stream_options":{"include_usage":true}"#
        } else {
            ""
        }
    )
    .into_bytes()
}

async fn send(
    lc: &LeaseChat,
    secret: &str,
    lease: &str,
    request: Vec<u8>,
) -> Result<Vec<u8>, String> {
    let mut reply = lc
        .chat(
            secret,
            lease,
            ChatRequest {
                body: request.into(),
                headers: vec![],
            },
        )
        .await
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    while let Some(chunk) = reply.body.next_chunk().await {
        out.extend_from_slice(&chunk.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

fn median(v: &mut [i128]) -> i128 {
    v.sort();
    v[v.len() / 2]
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

    // ---- 1. concurrent burst at the cap
    let cap = key_cap_nano_usd(2_060);
    let mut overspends = Vec::new();
    let rounds = if std::env::var("SKIP_BURST").is_ok() {
        0
    } else {
        3
    };
    for round in 1..=rounds {
        let lease = format!("burst{round}-{}", std::process::id());
        adapter.mint(&lease, 2_060).await.expect("mint");
        tokio::time::sleep(Duration::from_secs(8)).await;
        let secret = adapter.secret(&lease).expect("secret");
        let mut tasks = Vec::new();
        for _ in 0..24 {
            let (lc, secret, lease) = (lc.clone(), secret.clone(), lease.clone());
            tasks.push(tokio::spawn(async move {
                send(
                    &lc,
                    &secret,
                    &lease,
                    body(400, false, "Write a long story about a lighthouse."),
                )
                .await
            }));
        }
        let mut answered = 0;
        for t in tasks {
            if t.await.unwrap().is_ok() {
                answered += 1;
            }
        }
        tokio::time::sleep(Duration::from_secs(12)).await;
        let used = adapter
            .usage(&lease)
            .await
            .map(|u| u.cost_nano_usd)
            .unwrap_or(0);
        let over = used as i128 - cap as i128;
        println!(
            "burst {round}: {answered}/24 answered, usage {used}, cap {cap}, overspend {over} ({:.1}% of the cap)",
            over as f64 * 100.0 / cap as f64
        );
        overspends.push(over);
        let _ = adapter.revoke(&lease).await;
    }
    if !overspends.is_empty() {
        println!(
            "overspend median {} nano-USD over the cap {cap}\n",
            median(&mut overspends)
        );
    }

    // ---- 2 and 3 on one key
    let lease = format!("lag-{}", std::process::id());
    adapter.mint(&lease, 100_000).await.expect("mint");
    tokio::time::sleep(Duration::from_secs(8)).await;
    let secret = adapter.secret(&lease).expect("secret");
    let mut lags = Vec::new();
    let mut last = 0u128;
    for i in 1..=8 {
        let request = body(20, false, &format!("Say the number {i}."));
        send(&lc, &secret, &lease, request).await.expect("chat");
        let start = Instant::now();
        let lag = loop {
            let used = adapter
                .usage(&lease)
                .await
                .map(|u| u.cost_nano_usd)
                .unwrap_or(0);
            if used > last {
                last = used;
                break start.elapsed().as_millis();
            }
            if start.elapsed() > Duration::from_secs(60) {
                break 60_000;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        };
        lags.push(lag as i128);
    }
    let mut sorted = lags.clone();
    println!(
        "usage lag after the reply (ms): {lags:?}; median {}, max {}\n",
        median(&mut sorted),
        lags.iter().max().unwrap()
    );

    // ---- 3. does the signature cover a streamed answer?
    for stream in [false, true] {
        let request = body(30, stream, "Name one colour.");
        let response = send(&lc, &secret, &lease, request.clone())
            .await
            .expect("chat");
        let query = EvidenceQuery {
            nonce: hex::encode(rand::random::<[u8; 32]>()),
            signing_algo: "ed25519".into(),
        };
        let evidence = lc
            .evidence(&secret, &lease, &query)
            .await
            .expect("evidence")
            .expect("some");
        let sigs = evidence["signatures"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        // The newest signature is the chat just made.
        let sig = &sigs[0]["signature"];
        let text = sig["text"].as_str().unwrap_or("");
        let (rq, rs) = (
            hex::encode(Sha256::digest(&request)),
            hex::encode(Sha256::digest(&response)),
        );
        println!(
            "stream={stream}: kind {}, text {text}\n  sha256(request)  {rq}\n  sha256(response) {rs}\n  text contains request hash: {}, response hash: {}",
            sig["signature_kind"],
            text.contains(&rq),
            text.contains(&rs)
        );
    }
    let _ = adapter.revoke(&lease).await;
}
