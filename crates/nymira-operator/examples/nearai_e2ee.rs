//! End-to-end encrypted chat through the operator's own request checks and the real adapter,
//! non-streamed and streamed, with the real NEAR AI gateway. A client key pair is made here, the
//! prompt is encrypted to the model's key (taken from its attestation report, not verified), the
//! body and headers go through `parse_chat` and `forward_headers` exactly as the proxy applies
//! them, and the reply is decrypted here. Costs well under a cent. Prints no token.
//!
//! `cargo run -p nymira-operator --example nearai_e2ee` (after `nearai_live` set up the login)

use std::collections::HashSet;
use std::sync::Arc;

use axum::http::{HeaderMap, HeaderValue};
use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::SigningKey;
use hkdf::Hkdf;
use nymira_operator::alarm::Alarms;
use nymira_operator::clock::SystemClock;
use nymira_operator::nearai::{NearAiAdapter, NearAiConfig, BROWSER_USER_AGENT};
use nymira_operator::provider::{ChatRequest, EvidenceQuery, ProviderAdapter};
use nymira_operator::proxy::{forward_headers, parse_chat, ProxyConfig};
use nymira_operator::sealed::SealedFile;
use rand::RngCore;
use serde_json::{json, Value};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

const MODEL: &str = "z-ai/glm-5.3-flash";

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

/// `[ephemeral public key (32)][nonce (24)][ciphertext and tag]`, X25519, HKDF-SHA256 with info
/// `ed25519_encryption`, XChaCha20-Poly1305.
fn encrypt(plaintext: &[u8], recipient_x25519: [u8; 32]) -> Vec<u8> {
    let eph = StaticSecret::random_from_rng(rand::rngs::OsRng);
    let shared = eph.diffie_hellman(&PublicKey::from(recipient_x25519));
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(None, shared.as_bytes())
        .expand(b"ed25519_encryption", &mut key)
        .unwrap();
    let mut nonce = [0u8; 24];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ct = XChaCha20Poly1305::new((&key).into())
        .encrypt(XNonce::from_slice(&nonce), plaintext)
        .unwrap();
    [PublicKey::from(&eph).as_bytes().as_slice(), &nonce, &ct].concat()
}

fn decrypt(data: &[u8], secret_x25519: [u8; 32]) -> Option<Vec<u8>> {
    if data.len() < 32 + 24 + 16 {
        return None;
    }
    let shared = StaticSecret::from(secret_x25519)
        .diffie_hellman(&PublicKey::from(<[u8; 32]>::try_from(&data[..32]).ok()?));
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(None, shared.as_bytes())
        .expand(b"ed25519_encryption", &mut key)
        .ok()?;
    XChaCha20Poly1305::new((&key).into())
        .decrypt(XNonce::from_slice(&data[32..56]), &data[56..])
        .ok()
}

fn open_hex(field: &Value, secret: [u8; 32]) -> Option<String> {
    let bytes = hex::decode(field.as_str()?).ok()?;
    String::from_utf8(decrypt(&bytes, secret)?).ok()
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
    let lease = format!("e2ee-{}", std::process::id());
    adapter.mint(&lease, 100_000).await.expect("mint");

    // One plain chat so that the attestation report can be fetched with the lease key.
    let plain =
        json!({"model": MODEL, "max_tokens": 5, "messages": [{"role": "user", "content": "hi"}]});
    let mut reply = adapter
        .chat(
            &lease,
            ChatRequest {
                body: plain.to_string().into(),
                headers: vec![],
            },
        )
        .await
        .expect("plain chat");
    while reply.body.next_chunk().await.is_some() {}
    let query = EvidenceQuery {
        nonce: hex::encode(rand::random::<[u8; 32]>()),
        signing_algo: "ed25519".into(),
    };
    let evidence = adapter
        .evidence(&lease, &query)
        .await
        .expect("evidence")
        .expect("some");
    let candidates = evidence["attestation"]["model_attestations"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    println!("model attestation candidates: {}", candidates.len());
    let model_pub = candidates
        .first()
        .and_then(|c| c["signing_public_key"].as_str())
        .and_then(|h| hex::decode(h).ok())
        .and_then(|b| <[u8; 32]>::try_from(b).ok())
        .expect("a model public key (not verified here)");
    let model_x25519 = ed25519_dalek::VerifyingKey::from_bytes(&model_pub)
        .expect("ed25519 key")
        .to_montgomery()
        .to_bytes();

    let client = SigningKey::generate(&mut rand::rngs::OsRng);
    let client_pub = client.verifying_key().to_bytes();
    let client_secret = client.to_scalar_bytes();

    for stream in [false, true] {
        let prompt = "What is the capital of France? Answer in one word.";
        let encrypted = hex::encode(encrypt(prompt.as_bytes(), model_x25519));
        let body = json!({
            "model": MODEL,
            "max_tokens": 400,
            "stream": stream,
            "messages": [{"role": "user", "content": encrypted}],
        });
        let mut headers = HeaderMap::new();
        for (k, v) in [
            ("x-signing-algo", "ed25519".to_string()),
            ("x-client-pub-key", hex::encode(client_pub)),
            ("x-model-pub-key", hex::encode(model_pub)),
            ("x-encryption-version", "2".to_string()),
        ] {
            headers.insert(k, HeaderValue::from_str(&v).unwrap());
        }
        let forwarded = forward_headers(&headers).expect("headers pass the proxy check");
        let parsed = parse_chat(
            body.to_string().as_bytes(),
            &ProxyConfig::default(),
            &HashSet::new(),
        )
        .expect("body passes the proxy check");
        println!(
            "\n== stream: {stream} (proxy checks passed, input estimate {} tokens)",
            parsed.input_tokens
        );

        let mut reply = match adapter
            .chat(
                &lease,
                ChatRequest {
                    body: parsed.body.into(),
                    headers: forwarded,
                },
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                println!("chat refused: {e}");
                continue;
            }
        };
        println!("content-type: {}", reply.content_type);
        let mut raw = Vec::new();
        while let Some(chunk) = reply.body.next_chunk().await {
            match chunk {
                Ok(c) => raw.extend_from_slice(&c),
                Err(e) => {
                    println!("body error: {e}");
                    break;
                }
            }
        }
        let text = String::from_utf8_lossy(&raw).to_string();
        if stream {
            let (mut content, mut events, mut usage) = (String::new(), 0, None);
            for line in text.lines().filter_map(|l| l.strip_prefix("data:")) {
                let line = line.trim();
                if line == "[DONE]" {
                    continue;
                }
                events += 1;
                if let Ok(v) = serde_json::from_str::<Value>(line) {
                    if let Some(c) = open_hex(&v["choices"][0]["delta"]["content"], client_secret) {
                        content.push_str(&c);
                    }
                    if !v["usage"].is_null() {
                        usage = Some(v["usage"].clone());
                    }
                }
            }
            println!("events: {events}, decrypted: {content:?}, usage in stream: {usage:?}");
        } else {
            match serde_json::from_str::<Value>(&text) {
                Ok(v) => {
                    let m = &v["choices"][0]["message"];
                    println!("decrypted: {:?}", open_hex(&m["content"], client_secret));
                    println!(
                        "reasoning present: {}",
                        !m["reasoning_content"].is_null() || !m["reasoning"].is_null()
                    );
                    println!("usage: {}", v["usage"]);
                }
                Err(_) => println!("not json: {}", text.chars().take(200).collect::<String>()),
            }
        }
    }

    let query = EvidenceQuery {
        nonce: hex::encode(rand::random::<[u8; 32]>()),
        signing_algo: "ed25519".into(),
    };
    match adapter.evidence(&lease, &query).await {
        Ok(Some(e)) => {
            for sig in e["signatures"].as_array().cloned().unwrap_or_default() {
                let v = &sig["signature"];
                let fields: Vec<&str> = v
                    .as_object()
                    .map(|o| o.keys().map(String::as_str).collect())
                    .unwrap_or_default();
                println!(
                    "signature fields {:?} kind {} algo {}",
                    fields, v["signature_kind"], v["signing_algo"]
                );
            }
        }
        Ok(None) => println!("no evidence"),
        Err(e) => println!("evidence failed: {e}"),
    }
    adapter.revoke(&lease).await.expect("revoke");
    println!("revoked");
}
