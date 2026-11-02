//! Sends the odd-body corpus (duplicate keys, aliases, case and whitespace in `model`, nested
//! `extra_body`) to the real NEAR AI gateway through the adapter, and prints it next to what the
//! operator's parser decides. Each request asks for one token at most; the only model used is
//! `z-ai/glm-5.3-flash`. Prints no token.
//!
//! Uses the sealed login of `nearai_live` (`~/.nymira-live`, passphrase in `~/.nymira-operator.env`).
//!
//! `cargo run -p nymira-operator --example odd_bodies_live`

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use nymira_operator::alarm::Alarms;
use nymira_operator::clock::SystemClock;
use nymira_operator::nearai::{NearAiAdapter, NearAiConfig, BROWSER_USER_AGENT};
use nymira_operator::provider::{ChatRequest, ProviderAdapter};
use nymira_operator::proxy::{parse_chat, ProxyConfig};
use nymira_operator::sealed::SealedFile;
use serde_json::Value;

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

fn corpus() -> Vec<(&'static str, String)> {
    let msg = r#""messages":[{"role":"user","content":"Say hi"}]"#;
    let good = format!(r#"{{"model":"{MODEL}","max_tokens":1,{msg}}}"#);
    vec![
        ("plain", good.clone()),
        (
            "duplicate model, pinned first",
            format!(r#"{{"model":"{MODEL}","model":"z-ai/glm-5","max_tokens":1,{msg}}}"#),
        ),
        (
            "duplicate model, pinned last",
            format!(r#"{{"model":"z-ai/glm-5","model":"{MODEL}","max_tokens":1,{msg}}}"#),
        ),
        (
            "model in upper case",
            good.replace(MODEL, &MODEL.to_uppercase()),
        ),
        (
            "model with a trailing space",
            good.replace(MODEL, &format!("{MODEL} ")),
        ),
        (
            "model with a leading space",
            good.replace(MODEL, &format!(" {MODEL}")),
        ),
        (
            "key spelled with an escape",
            good.replace("\"model\"", "\"mod\\u0065l\""),
        ),
        (
            "space before the colon",
            good.replace("\"model\":", "\"model\" :"),
        ),
        (
            "both max_tokens fields",
            format!(r#"{{"model":"{MODEL}","max_tokens":1,"max_completion_tokens":1,{msg}}}"#),
        ),
        (
            "max_completion_tokens alone",
            format!(r#"{{"model":"{MODEL}","max_completion_tokens":1,{msg}}}"#),
        ),
        (
            "duplicate max_tokens",
            format!(r#"{{"model":"{MODEL}","max_tokens":1,"max_tokens":50,{msg}}}"#),
        ),
        (
            "n = 2",
            format!(r#"{{"model":"{MODEL}","max_tokens":1,"n":2,{msg}}}"#),
        ),
        (
            "extra_body with n",
            good.replace('}', r#","extra_body":{"n":3}}"#)
                .replacen(",}", "}", 1),
        ),
        (
            "extra_body with model",
            format!(
                r#"{{"model":"{MODEL}","max_tokens":1,"extra_body":{{"model":"z-ai/glm-5"}},{msg}}}"#
            ),
        ),
        (
            "unknown top-level field",
            format!(r#"{{"model":"{MODEL}","max_tokens":1,"plugins":[],{msg}}}"#),
        ),
        ("body twice", format!("{good}\n{good}")),
        ("leading whitespace", format!("  \n{good}")),
    ]
}

#[tokio::main]
async fn main() {
    let get = {
        let s = settings();
        move |k: &str| s.get(k).cloned().unwrap_or_default()
    };
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

    let lease = format!("odd-{}", std::process::id());
    adapter.mint(&lease, 100_000).await.expect("mint");
    // A new key answers 401 for a few seconds.
    tokio::time::sleep(Duration::from_secs(8)).await;

    let cfg = ProxyConfig::default();
    println!("{:<34} {:<9} gateway", "body", "operator");
    for (name, body) in corpus() {
        let ours = parse_chat(body.as_bytes(), &cfg, &HashSet::new());
        let decision = match &ours {
            Ok(_) => "accepts".to_string(),
            Err(e) => format!("refuses ({})", e.code.as_str()),
        };
        let outcome = match adapter
            .chat(
                &lease,
                ChatRequest {
                    body: body.clone().into(),
                    headers: vec![],
                },
            )
            .await
        {
            Err(e) => format!("error: {e}"),
            Ok(mut reply) => {
                let mut bytes = Vec::new();
                while let Some(Ok(chunk)) = reply.body.next_chunk().await {
                    bytes.extend_from_slice(&chunk);
                }
                let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
                format!(
                    "200, model {}, choices {}",
                    v["model"].as_str().unwrap_or("?"),
                    v["choices"].as_array().map_or(0, Vec::len)
                )
            }
        };
        println!("{name:<34} {decision:<9} -> {outcome}");
    }
    let _ = adapter.revoke(&lease).await;
}
