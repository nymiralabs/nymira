//! Runs the real NEAR AI adapter once: balance, model list against the pins, one key, one tiny
//! plain chat, how long the usage takes to show, revoke. Costs well under a cent. Prints no token.
//!
//! The adapter keeps its login and key table in sealed files under `~/.nymira-live/`. The
//! refresh token moves there from `~/.nymira-operator.env` on the first run and the line in the
//! env file is blanked, so only one copy of the rotating token exists.
//!
//! `cargo run -p nymira-operator --example nearai_live`

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use nymira_operator::alarm::Alarms;
use nymira_operator::clock::SystemClock;
use nymira_operator::nearai::{NearAiAdapter, NearAiConfig, BROWSER_USER_AGENT};
use nymira_operator::provider::{ChatRequest, ProviderAdapter};
use nymira_operator::proxy::ProxyConfig;
use nymira_operator::sealed::SealedFile;
use rand::RngCore;

fn env_path() -> String {
    format!("{}/.nymira-operator.env", std::env::var("HOME").unwrap())
}

fn read_env() -> (Vec<String>, HashMap<String, String>) {
    let text = std::fs::read_to_string(env_path()).expect("settings file");
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    let map = lines
        .iter()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.trim().into(), v.trim().into()))
        .collect();
    (lines, map)
}

fn write_env(lines: &[String]) {
    use std::io::Write;
    let tmp = format!("{}.tmp", env_path());
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&tmp).expect("temp");
    writeln!(file, "{}", lines.join("\n")).expect("write");
    file.sync_all().expect("sync");
    std::fs::rename(&tmp, env_path()).expect("rename");
}

#[tokio::main]
async fn main() {
    let (mut lines, settings) = read_env();
    let get = |k: &str| settings.get(k).cloned().unwrap_or_default();

    // The seal passphrase lives in the env file for this check only (a prompt would be better).
    let mut pass = get("NYMIRA_SEAL_PASSPHRASE");
    if pass.is_empty() {
        let mut raw = [0u8; 24];
        rand::thread_rng().fill_bytes(&mut raw);
        pass = hex::encode(raw);
        lines.push(format!("NYMIRA_SEAL_PASSPHRASE={pass}"));
        write_env(&lines);
    }
    let dir = format!("{}/.nymira-live", std::env::var("HOME").unwrap());
    std::fs::create_dir_all(&dir).expect("dir");
    let login = SealedFile::open(format!("{dir}/login.enc").as_ref(), &pass, 600_000).unwrap();
    let keys = SealedFile::open(format!("{dir}/keys.enc").as_ref(), &pass, 600_000).unwrap();

    let first = get("NEAR_AI_REFRESH_TOKEN");
    let adapter = match NearAiAdapter::new(
        NearAiConfig {
            user_agent: BROWSER_USER_AGENT.into(),
            base_url: "https://cloud-api.near.ai".into(),
            org_id: get("NEAR_AI_ORG_ID"),
            workspace_id: get("NEAR_AI_WORKSPACE_ID"),
        },
        Arc::new(SystemClock),
        Arc::new(Alarms::new(vec![], Arc::new(SystemClock))),
        login,
        keys,
        (!first.is_empty()).then_some(first.as_str()),
    ) {
        Ok(a) => a,
        Err(e) => return println!("adapter not built: {e}"),
    };
    // The sealed file holds the login now; the env copy would go stale at the next rotation.
    if lines
        .iter()
        .any(|l| l.starts_with("NEAR_AI_REFRESH_TOKEN=") && !l.ends_with('='))
    {
        for l in lines
            .iter_mut()
            .filter(|l| l.starts_with("NEAR_AI_REFRESH_TOKEN="))
        {
            *l = "NEAR_AI_REFRESH_TOKEN=".into();
        }
        write_env(&lines);
        println!("refresh token moved into the sealed login file");
    }

    match adapter.balance().await {
        Ok(micro) => println!(
            "balance: {micro} micro-USD ({}.{:06} USD)",
            micro / 1_000_000,
            micro % 1_000_000
        ),
        Err(e) => return println!("balance failed: {e}"),
    }

    match adapter.models().await {
        Ok(Some(live)) => {
            println!("live models: {}", live.len());
            for pin in &ProxyConfig::default().models {
                match live.iter().find(|m| m.id == pin.id) {
                    Some(m) => println!(
                        "  {}: type {} attested {} price in {} out {} (pin in {} out {})",
                        pin.id,
                        m.provider_type,
                        m.attestation_supported,
                        m.input_nano_per_token,
                        m.output_nano_per_token,
                        pin.input_nano_per_token,
                        pin.output_nano_per_token
                    ),
                    None => println!("  {}: not on the live list", pin.id),
                }
            }
            let mut vllm: Vec<&str> = live
                .iter()
                .filter(|m| m.provider_type == "vllm" && m.attestation_supported)
                .map(|m| m.id.as_str())
                .collect();
            vllm.sort();
            println!("vllm and attested: {}", vllm.join(", "));
        }
        Ok(None) => println!("model list not available"),
        Err(e) => println!("models failed: {e}"),
    }

    let lease = format!("live-{}", std::process::id());
    let started = Instant::now();
    let minted = match adapter.mint(&lease, 100_000).await {
        Ok(m) => m,
        Err(e) => return println!("mint failed: {e}"),
    };
    println!("mint: ok in {} ms", started.elapsed().as_millis());
    match adapter.list().await {
        Ok(l) => println!(
            "list: {} live key(s), ours present: {}",
            l.len(),
            l.contains(&lease)
        ),
        Err(e) => println!("list failed: {e}"),
    }

    // The key needs up to 10 s to become active.
    let body = r#"{"model":"z-ai/glm-5.3-flash","max_tokens":5,"messages":[{"role":"user","content":"Say hi"}]}"#;
    let mut answered = false;
    let chat_started = Instant::now();
    for attempt in 1..=8 {
        match adapter
            .chat(
                &lease,
                ChatRequest {
                    body: body.into(),
                    headers: vec![],
                },
            )
            .await
        {
            Ok(mut reply) => {
                let mut bytes = 0;
                while let Some(Ok(chunk)) = reply.body.next_chunk().await {
                    bytes += chunk.len();
                }
                println!(
                    "chat: ok on attempt {attempt}, {bytes} bytes, {} ms",
                    chat_started.elapsed().as_millis()
                );
                answered = true;
                break;
            }
            Err(e) => {
                println!("chat attempt {attempt}: {e}");
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }

    if answered {
        let usage_started = Instant::now();
        for _ in 0..30 {
            match adapter.usage(&lease).await {
                Ok(u) if u.cost_nano_usd > 0 => {
                    println!(
                        "usage shown after {} s: {} nano-USD",
                        usage_started.elapsed().as_secs(),
                        u.cost_nano_usd
                    );
                    break;
                }
                Ok(_) => {}
                Err(e) => println!("usage: {e}"),
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }

    match adapter.revoke(&lease).await {
        Ok(()) => println!("revoke: ok"),
        Err(e) => println!("revoke failed: {e}"),
    }
    match adapter.usage(&lease).await {
        Ok(u) => println!("usage after revoke: {} nano-USD", u.cost_nano_usd),
        Err(e) => println!("usage after revoke failed: {e}"),
    }
    match adapter.list().await {
        Ok(l) => println!("list after revoke: ours present: {}", l.contains(&lease)),
        Err(e) => println!("list failed: {e}"),
    }
    println!("key expires_at was {}", minted.expires_at);
}
