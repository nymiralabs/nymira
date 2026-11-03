//! End-to-end encryption through the operator's HTTP path, against the fake NEAR AI gateway:
//! a client encrypts the prompt to the model's attested key, the operator forwards the bytes it
//! validated without reading them, and the reply comes back as ciphertext only the client can
//! open. The gateway's record of the request is the witness that nothing was decrypted on the way.

mod common;

use std::sync::Arc;

use common::*;
use nymira_fake_near_ai::crypto::{
    client_keypair, decrypt_hex, encrypt_hex, model_e2ee_public_hex, parse_key32,
};
use nymira_fake_near_ai::{start, FIRST_REFRESH_TOKEN};
use nymira_operator::alarm::{Alarms, FakeSink};
use nymira_operator::clock::ManualClock;
use nymira_operator::nearai::{NearAiAdapter, NearAiConfig};
use nymira_operator::sealed::SealedFile;
use serde_json::{json, Value};

const MODEL: &str = "z-ai/glm-5.3-flash";
const PROMPT: &str = "tell me a secret about hedgehogs";

fn real_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

struct World {
    rig: Rig,
    gateway: nymira_fake_near_ai::Running,
    lease_id: String,
    lease_token: String,
}

async fn world() -> World {
    let gateway = start("127.0.0.1:0".parse().unwrap()).await.unwrap();
    let clock = Arc::new(ManualClock::new(real_now_ms()));
    let sink = Arc::new(FakeSink::default());
    let dir = tempfile::tempdir().unwrap();
    let file = |name: &str| SealedFile::open(&dir.path().join(name), "pass", 10).unwrap();
    let adapter = NearAiAdapter::new(
        NearAiConfig {
            user_agent: "test-agent".into(),
            base_url: gateway.url(),
            org_id: "org".into(),
            workspace_id: "ws".into(),
        },
        clock.clone(),
        Arc::new(Alarms::new(vec![sink], clock)),
        file("login.enc"),
        file("keys.enc"),
        Some(FIRST_REFRESH_TOKEN),
    )
    .unwrap();
    let rig = Rig::new(Setup {
        provider: Some(Arc::new(adapter)),
        // The fake gateway prices every model alike; pin the one this test uses.
        tune: Box::new(|c| c.proxy.models.retain(|m| m.id == MODEL)),
        ..Setup::default()
    })
    .await;
    let (status, body, _) = rig.lease(&rig.fresh(1)).await;
    assert_eq!(status, 201, "{body}");
    let (lease_id, lease_token) = (
        body["lease_id"].as_str().unwrap().to_string(),
        body["lease_token"].as_str().unwrap().to_string(),
    );
    std::mem::forget(dir);
    World {
        rig,
        gateway,
        lease_id,
        lease_token,
    }
}

fn request(
    w: &World,
    client_pub: &[u8; 32],
    model_pub: &str,
    body: &Value,
) -> reqwest::RequestBuilder {
    w.rig
        .http
        .post(format!("{}/v1/chat/completions", w.rig.url))
        .header("x-nymira-lease-id", &w.lease_id)
        .header("x-nymira-lease-token", &w.lease_token)
        .header("x-signing-algo", "ed25519")
        .header("x-client-pub-key", hex::encode(client_pub))
        .header("x-model-pub-key", model_pub)
        .header("x-encryption-version", "2")
        .header("content-type", "application/json")
        .body(serde_json::to_vec(body).unwrap())
}

fn encrypted_body(stream: bool) -> Value {
    let model_pk = parse_key32(&model_e2ee_public_hex()).unwrap();
    json!({
        "model": MODEL, "stream": stream,
        "messages": [{"role": "user", "content": encrypt_hex(&model_pk, PROMPT)}],
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn an_encrypted_chat_reaches_the_gateway_as_ciphertext_and_the_reply_is_readable_only_by_the_client(
) {
    let w = world().await;
    let (secret, public) = client_keypair();
    let r = request(
        &w,
        &public,
        &model_e2ee_public_hex(),
        &encrypted_body(false),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let reply: Value = r.json().await.unwrap();
    let ciphertext = reply["choices"][0]["message"]["content"].as_str().unwrap();
    assert!(!ciphertext.contains("hedgehog"));
    assert_eq!(
        decrypt_hex(&secret, ciphertext).unwrap(),
        format!("fake reply to: {PROMPT}")
    );

    // What the gateway got: E2EE on, every field encrypted, and no plaintext anywhere.
    let chats = w.gateway.handle.chats();
    assert_eq!(chats.len(), 1);
    assert!(chats[0].e2ee && chats[0].encrypt_all_fields);
    let upstream = String::from_utf8_lossy(&chats[0].body);
    assert!(
        !upstream.contains("hedgehog"),
        "the operator forwarded plaintext"
    );
    assert!(!String::from_utf8_lossy(&chats[0].response).contains("hedgehog"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_streamed_encrypted_chat_decrypts_event_by_event() {
    let w = world().await;
    let (secret, public) = client_keypair();
    let text = request(&w, &public, &model_e2ee_public_hex(), &encrypted_body(true))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let mut plain = String::new();
    for line in text.lines().filter_map(|l| l.strip_prefix("data: ")) {
        if line == "[DONE]" {
            continue;
        }
        let v: Value = serde_json::from_str(line).unwrap();
        if let Some(piece) = v["choices"][0]["delta"]["content"]
            .as_str()
            .filter(|c| !c.is_empty())
        {
            plain.push_str(&decrypt_hex(&secret, piece).unwrap());
        }
    }
    assert_eq!(plain, format!("fake reply to: {PROMPT}"));
    assert!(w.gateway.handle.chats()[0].stream);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_key_other_than_the_attested_one_gets_no_answer() {
    let w = world().await;
    let (_, public) = client_keypair();
    let r = request(&w, &public, &"22".repeat(32), &encrypted_body(false))
        .send()
        .await
        .unwrap();
    assert!(!r.status().is_success(), "{}", r.status());
    assert!(w.gateway.handle.chats().is_empty(), "nothing was answered");
}
