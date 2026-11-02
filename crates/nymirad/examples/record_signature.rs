//! Records one real NEAR AI chat signature for offline tests.
//!
//! It mints a lease key with the sealed login that `nymira-operator`'s `nearai_live` example set
//! up (`~/.nymira-live`, passphrase in `~/.nymira-operator.env`), sends one tiny plain chat to
//! `z-ai/glm-5.3-flash`, fetches the response signature through the adapter, revokes the key, and
//! then records the attestation report of the VM that signed it (`record::record` with its
//! signing address). Costs well under a cent. Prints no token and writes none.
//!
//!   cargo run -p nymirad --features mock --example record_signature -- [OUT_DIR]
//!
//! Writes `request.json` and `response.json` (the exact bytes), `signature.json`, and the
//! recording of the signer's report under OUT_DIR (default
//! `crates/nymirad/tests/fixtures/near-ai-recorded/chat`).

use std::sync::Arc;

use nymira_operator::alarm::Alarms;
use nymira_operator::clock::SystemClock;
use nymira_operator::nearai::{NearAiAdapter, NearAiConfig, BROWSER_USER_AGENT};
use nymira_operator::provider::{ChatRequest, EvidenceQuery, ProviderAdapter};
use nymira_operator::sealed::SealedFile;
use serde_json::json;

const MODEL: &str = "z-ai/glm-5.3-flash";

#[tokio::main]
async fn main() {
    let out: std::path::PathBuf = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "crates/nymirad/tests/fixtures/near-ai-recorded/chat".into())
        .into();
    std::fs::create_dir_all(&out).unwrap();

    let home = std::env::var("HOME").unwrap();
    let settings: std::collections::HashMap<String, String> =
        std::fs::read_to_string(format!("{home}/.nymira-operator.env"))
            .expect("~/.nymira-operator.env")
            .lines()
            .filter_map(|l| l.split_once('='))
            .map(|(k, v)| (k.trim().into(), v.trim().into()))
            .collect();
    let get = |k: &str| settings.get(k).cloned().unwrap_or_default();
    let dir = format!("{home}/.nymira-live");
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
    .expect("adapter (run nearai_live first if the login is missing)");

    let lease = format!("sig-{}", std::process::id());
    adapter.mint(&lease, 100_000).await.expect("mint");
    let request = serde_json::to_vec(
        &json!({"model": MODEL, "max_tokens": 8, "messages": [{"role": "user", "content": "Say OK."}]}),
    )
    .unwrap();
    let mut reply = adapter
        .chat(
            &lease,
            ChatRequest {
                body: request.clone().into(),
                headers: vec![],
            },
        )
        .await
        .expect("chat");
    let mut response = Vec::new();
    while let Some(chunk) = reply.body.next_chunk().await {
        response.extend_from_slice(&chunk.expect("body"));
    }
    let evidence = adapter
        .evidence(
            &lease,
            &EvidenceQuery {
                nonce: hex::encode(rand::random::<[u8; 32]>()),
                signing_algo: "ed25519".into(),
            },
        )
        .await
        .expect("evidence")
        .expect("some");
    adapter.revoke(&lease).await.expect("revoke");

    let signature = evidence["signatures"][0]["signature"].clone();
    let signer = signature["signing_address"]
        .as_str()
        .expect("the signature names its signer")
        .to_string();
    println!(
        "signature fields: {:?}, kind: {}",
        signature.as_object().map(|o| o.keys().collect::<Vec<_>>()),
        signature["signature_kind"]
    );
    std::fs::write(out.join("request.json"), &request).unwrap();
    std::fs::write(out.join("response.json"), &response).unwrap();
    std::fs::write(
        out.join("signature.json"),
        serde_json::to_vec_pretty(&signature).unwrap(),
    )
    .unwrap();

    // The report of the VM that signed, with the Intel collateral and the time.
    nymirad::record::record(out.clone(), MODEL.into(), Some(signer), 0).await;
    println!("wrote {}", out.display());
}
