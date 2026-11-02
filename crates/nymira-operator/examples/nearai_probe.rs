//! Looks at which per-key usage reads the live NEAR AI gateway serves. Mints one key, sends one
//! tiny chat, reads the key's usage through each candidate endpoint before and after the revoke,
//! and prints only the shape of each answer: field names, numbers and short strings; no token,
//! key or id value.
//!
//! `cargo run -p nymira-operator --example nearai_probe` (after `nearai_live` has set up the login)

use std::sync::Arc;
use std::time::Duration;

use nymira_operator::alarm::Alarms;
use nymira_operator::clock::SystemClock;
use nymira_operator::nearai::{NearAiAdapter, NearAiConfig, BROWSER_USER_AGENT};
use nymira_operator::provider::{ChatRequest, ProviderAdapter};
use nymira_operator::sealed::SealedFile;
use serde_json::Value;

fn env() -> std::collections::HashMap<String, String> {
    let text = std::fs::read_to_string(format!(
        "{}/.nymira-operator.env",
        std::env::var("HOME").unwrap()
    ))
    .unwrap();
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
}

/// The answer with identifiers and long strings replaced, so it shows shape and numbers only.
fn shape(v: &Value) -> Value {
    match v {
        Value::String(s) if s.len() > 14 => Value::String(format!("<{} chars>", s.len())),
        Value::Array(a) => Value::Array(a.iter().take(3).map(shape).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .filter(|(k, _)| !matches!(k.as_str(), "key" | "key_prefix" | "token"))
                .map(|(k, v)| (k.clone(), shape(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

async fn show(adapter: &NearAiAdapter, label: &str, path: &str) {
    match adapter.session_get(path).await {
        Ok(v) => println!("{label}: {}", shape(&v)),
        Err(e) => println!(
            "{label}: {}",
            e.to_string().chars().take(120).collect::<String>()
        ),
    }
}

#[tokio::main]
async fn main() {
    let s = env();
    let get = |k: &str| s.get(k).cloned().unwrap_or_default();
    let dir = format!("{}/.nymira-live", std::env::var("HOME").unwrap());
    let pass = get("NYMIRA_SEAL_PASSPHRASE");
    let adapter = NearAiAdapter::new(
        NearAiConfig {
            user_agent: BROWSER_USER_AGENT.into(),
            base_url: "https://cloud-api.near.ai".into(),
            org_id: get("NEAR_AI_ORG_ID"),
            workspace_id: get("NEAR_AI_WORKSPACE_ID"),
        },
        Arc::new(SystemClock),
        Arc::new(Alarms::new(vec![], Arc::new(SystemClock))),
        SealedFile::open(format!("{dir}/login.enc").as_ref(), &pass, 600_000).unwrap(),
        SealedFile::open(format!("{dir}/keys.enc").as_ref(), &pass, 600_000).unwrap(),
        None,
    )
    .expect("adapter");
    let (org, ws) = (get("NEAR_AI_ORG_ID"), get("NEAR_AI_WORKSPACE_ID"));

    show(
        &adapter,
        "org balance",
        &format!("/v1/organizations/{org}/usage/balance"),
    )
    .await;

    let lease = format!("probe-{}", std::process::id());
    adapter.mint(&lease, 100_000).await.expect("mint");
    let id = adapter.key_id(&lease).expect("id");
    let body = r#"{"model":"z-ai/glm-5.3-flash","max_tokens":5,"messages":[{"role":"user","content":"Say hi"}]}"#;
    for _ in 0..8 {
        if let Ok(mut r) = adapter
            .chat(
                &lease,
                ChatRequest {
                    body: body.into(),
                    headers: vec![],
                },
            )
            .await
        {
            while r.body.next_chunk().await.is_some() {}
            break;
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!("-- before revoke, right after the chat");
    let candidates = |when: &str| {
        vec![
            (
                format!("{when} key history"),
                format!("/v1/workspaces/{ws}/api-keys/{id}/usage/history"),
            ),
            (
                format!("{when} key list"),
                format!("/v1/workspaces/{ws}/api-keys?limit=5"),
            ),
            (
                format!("{when} org history by key"),
                format!("/v1/organizations/{org}/usage/history?api_key_id={id}"),
            ),
        ]
    };
    for (label, path) in candidates("early") {
        show(&adapter, &label, &path).await;
    }
    for wait in [10u64, 20] {
        tokio::time::sleep(Duration::from_secs(wait)).await;
        println!("-- {wait} s later");
        show(
            &adapter,
            "key history",
            &format!("/v1/workspaces/{ws}/api-keys/{id}/usage/history"),
        )
        .await;
    }
    adapter.revoke(&lease).await.expect("revoke");
    println!("-- after revoke (deactivate + delete)");
    for (label, path) in candidates("late") {
        show(&adapter, &label, &path).await;
    }
}
