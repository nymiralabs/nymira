//! The NEAR AI adapter against the fake gateway: the login and its refresh token, key
//! management, usage and balance reads, the model list, chat and the evidence fetch.

use std::sync::Arc;

use nymira_fake_near_ai::{start, Running, FIRST_REFRESH_TOKEN};
use nymira_operator::alarm::{Alarms, FakeSink};
use nymira_operator::clock::{Clock, ManualClock};
use nymira_operator::nearai::{NearAiAdapter, NearAiConfig};
use nymira_operator::provider::{
    ChatRequest, EvidenceQuery, ProviderAdapter, ProviderError, KEY_LIFETIME_SECS,
};
use nymira_operator::sealed::SealedFile;

const MODEL: &str = "z-ai/glm-5.3-flash";

struct Rig {
    fake: Running,
    clock: Arc<ManualClock>,
    sink: Arc<FakeSink>,
    alarms: Arc<Alarms>,
    dir: tempfile::TempDir,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

impl Rig {
    async fn new() -> Self {
        let clock = Arc::new(ManualClock::new(now_secs() * 1000));
        let sink = Arc::new(FakeSink::default());
        Self {
            fake: start("127.0.0.1:0".parse().unwrap()).await.unwrap(),
            alarms: Arc::new(Alarms::new(vec![sink.clone()], clock.clone())),
            clock,
            sink,
            dir: tempfile::tempdir().unwrap(),
        }
    }

    /// An adapter over the rig's files; a second call is a restarted process.
    fn adapter(&self, first_token: Option<&str>) -> Result<NearAiAdapter, ProviderError> {
        let file = |name: &str| SealedFile::open(&self.dir.path().join(name), "pass", 10).unwrap();
        NearAiAdapter::new(
            NearAiConfig {
                base_url: self.fake.url(),
                org_id: "org".into(),
                workspace_id: "ws".into(),
                reporting_token: "rpt-test".into(),
            },
            self.clock.clone(),
            self.alarms.clone(),
            file("login.enc"),
            file("keys.enc"),
            first_token,
        )
    }

    fn chat_request(&self) -> ChatRequest {
        ChatRequest {
            body: format!(
                r#"{{"model":"{MODEL}","messages":[{{"role":"user","content":"hello there"}}]}}"#
            )
            .into(),
            headers: vec![],
        }
    }
}

#[tokio::test]
async fn a_key_is_minted_capped_listed_and_revoked() {
    let rig = Rig::new().await;
    let adapter = rig.adapter(Some(FIRST_REFRESH_TOKEN)).unwrap();
    let minted = adapter.mint("lease-1", 100_000).await.unwrap();
    assert_eq!(minted.expires_at, rig.clock.now_secs() + KEY_LIFETIME_SECS);

    let keys = rig.fake.handle.keys();
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].id, minted.key_id);
    assert!(keys[0].name.starts_with("k-") && !keys[0].name.contains("lease"));
    // 0.10 USDC / 1.03 in nano-USD, floored.
    assert_eq!(keys[0].spend_limit.as_ref().unwrap().amount, 97_087_378);
    assert!(keys[0].expires_at.is_some());

    assert_eq!(adapter.list().await.unwrap(), vec!["lease-1".to_string()]);
    adapter.revoke("lease-1").await.unwrap();
    assert!(rig.fake.handle.keys()[0].deleted);
    assert!(adapter.list().await.unwrap().is_empty());
    // Settlement reads the usage after the revoke; the dead key cannot chat or give evidence.
    assert_eq!(adapter.usage("lease-1").await.unwrap().cost_nano_usd, 0);
    assert!(matches!(
        adapter.chat("lease-1", rig.chat_request()).await,
        Err(ProviderError::NotFound(_))
    ));
    assert!(matches!(
        adapter.usage("nobody").await,
        Err(ProviderError::NotFound(_))
    ));
}

#[tokio::test]
async fn usage_billed_before_the_revoke_is_still_read_after_it() {
    let rig = Rig::new().await;
    let adapter = rig.adapter(Some(FIRST_REFRESH_TOKEN)).unwrap();
    adapter.mint("a", 100_000).await.unwrap();
    let mut chat = adapter.chat("a", rig.chat_request()).await.unwrap();
    while chat.body.next_chunk().await.is_some() {}
    adapter.revoke("a").await.unwrap();
    assert_eq!(adapter.usage("a").await.unwrap().cost_nano_usd, 3_740);
    // A restarted process still knows the key id.
    drop(adapter);
    assert_eq!(
        rig.adapter(None)
            .unwrap()
            .usage("a")
            .await
            .unwrap()
            .cost_nano_usd,
        3_740
    );
}

#[tokio::test]
async fn the_key_table_survives_a_restart_and_a_key_with_no_record_is_an_orphan() {
    let rig = Rig::new().await;
    let first = rig.adapter(Some(FIRST_REFRESH_TOKEN)).unwrap();
    first.mint("lease-1", 100_000).await.unwrap();
    drop(first);

    // The restarted process reads the rotated refresh token and the key table from disk.
    let second = rig.adapter(None).unwrap();
    assert_eq!(second.list().await.unwrap(), vec!["lease-1".to_string()]);
    // A key created while the key table was not written.
    let created = reqwest::Client::new()
        .post(format!("{}/v1/workspaces/ws/api-keys", rig.fake.url()))
        .bearer_auth(nymira_fake_near_ai::SESSION_TOKEN)
        .json(&serde_json::json!({"name": "k-stray"}))
        .send()
        .await
        .unwrap();
    assert!(created.status().is_success());
    let mut listed = second.list().await.unwrap();
    listed.sort();
    assert_eq!(listed.len(), 2);
    let orphan = listed.iter().find(|l| l.starts_with("orphan:")).unwrap();
    second.revoke(orphan).await.unwrap();
    assert_eq!(second.list().await.unwrap(), vec!["lease-1".to_string()]);
}

#[tokio::test]
async fn a_rotated_refresh_token_is_written_back_encrypted() {
    let rig = Rig::new().await;
    let adapter = rig.adapter(Some(FIRST_REFRESH_TOKEN)).unwrap();
    adapter.balance().await.unwrap();
    // The fake replaced rt-fake-0 with rt-fake-1; the old one is dead.
    let raw = std::fs::read(rig.dir.path().join("login.enc")).unwrap();
    assert!(!String::from_utf8_lossy(&raw).contains("rt-fake"));
    let reopened = SealedFile::open(&rig.dir.path().join("login.enc"), "pass", 10).unwrap();
    let stored = String::from_utf8(reopened.read().unwrap().unwrap()).unwrap();
    assert!(stored.contains("rt-fake-1"), "{stored}");
    // A process started from the old token alone is refused, and a person is told.
    let rig2 = Rig::new().await;
    let wrong = rig2.adapter(Some("rt-wrong")).unwrap();
    assert!(matches!(
        wrong.balance().await,
        Err(ProviderError::Rejected(_))
    ));
    assert_eq!(rig2.sink.sent()[0].key, "refresh_token_rejected");
}

#[tokio::test]
async fn a_refresh_token_with_a_quarter_of_its_life_left_raises_a_warning() {
    let rig = Rig::new().await;
    let adapter = rig.adapter(Some(FIRST_REFRESH_TOKEN)).unwrap();
    assert_eq!(adapter.check_login_expiry().await, None, "no exchange yet");
    adapter.balance().await.unwrap();
    let ttl = rig.fake.handle.switches().refresh_ttl_secs;
    let left = adapter.check_login_expiry().await.unwrap();
    assert!(left <= ttl && left + 5 >= ttl);
    assert!(rig.sink.sent().is_empty());

    rig.clock.advance_secs(ttl * 70 / 100);
    adapter.check_login_expiry().await;
    assert!(rig.sink.sent().is_empty(), "30% left is not yet a warning");
    rig.clock.advance_secs(ttl * 10 / 100);
    adapter.check_login_expiry().await;
    assert_eq!(rig.sink.sent().len(), 1);
    assert_eq!(rig.sink.sent()[0].key, "refresh_token_expiring");
}

#[tokio::test]
async fn usage_comes_from_the_reporting_export_by_key_and_balance_is_in_micro_usd() {
    let rig = Rig::new().await;
    let adapter = rig.adapter(Some(FIRST_REFRESH_TOKEN)).unwrap();
    adapter.mint("a", 100_000).await.unwrap();
    adapter.mint("b", 100_000).await.unwrap();
    assert_eq!(adapter.balance().await.unwrap(), 10_000_000);

    let mut chat = adapter.chat("a", rig.chat_request()).await.unwrap();
    while chat.body.next_chunk().await.is_some() {}
    // 3 input tokens * 80 + 7 output * 500 nano-USD.
    assert_eq!(adapter.usage("a").await.unwrap().cost_nano_usd, 3_740);
    assert_eq!(adapter.usage("b").await.unwrap().cost_nano_usd, 0);
    assert_eq!(adapter.balance().await.unwrap(), 10_000_000 - 4);
}

#[tokio::test]
async fn the_live_model_list_carries_type_attestation_and_prices() {
    let rig = Rig::new().await;
    let adapter = rig.adapter(Some(FIRST_REFRESH_TOKEN)).unwrap();
    let models = adapter.models().await.unwrap().unwrap();
    let glm = models.iter().find(|m| m.id == MODEL).unwrap();
    assert_eq!(
        (
            glm.provider_type.as_str(),
            glm.attestation_supported,
            glm.input_nano_per_token,
            glm.output_nano_per_token
        ),
        ("vllm", true, 80, 500)
    );
    assert!(models.iter().any(|m| m.provider_type == "chutes"));
    rig.fake
        .handle
        .update(|s| s.output_price_nano_per_token = 900);
    let again = adapter.models().await.unwrap().unwrap();
    assert_eq!(
        again
            .iter()
            .find(|m| m.id == MODEL)
            .unwrap()
            .output_nano_per_token,
        900
    );
}

#[tokio::test]
async fn provider_errors_keep_their_meaning() {
    let rig = Rig::new().await;
    let adapter = rig.adapter(Some(FIRST_REFRESH_TOKEN)).unwrap();
    adapter.mint("a", 3).await.unwrap(); // 2,912 nano-USD: under one request
    let mut ok = adapter.chat("a", rig.chat_request()).await.unwrap();
    while ok.body.next_chunk().await.is_some() {}
    // The key is now at its spend limit: 402 at request start.
    assert!(matches!(
        adapter.chat("a", rig.chat_request()).await,
        Err(ProviderError::Rejected(_))
    ));

    adapter.mint("b", 100_000).await.unwrap();
    rig.fake.handle.update(|s| {
        s.fail_next = 1;
        s.fail_status = 429;
        s.fail_path_prefix = "/v1/chat".into();
    });
    assert!(matches!(
        adapter.chat("b", rig.chat_request()).await,
        Err(ProviderError::RateLimited { .. })
    ));
    rig.fake.handle.update(|s| {
        s.fail_next = 1;
        s.fail_status = 503;
        s.fail_path_prefix = "/v1/chat".into();
    });
    assert!(matches!(
        adapter.chat("b", rig.chat_request()).await,
        Err(ProviderError::Unavailable(_))
    ));
    assert!(matches!(
        adapter.chat("nobody", rig.chat_request()).await,
        Err(ProviderError::NotFound(_))
    ));
}

#[tokio::test]
async fn evidence_covers_the_lease_chats_with_the_given_nonce_and_algorithm() {
    let rig = Rig::new().await;
    let adapter = rig.adapter(Some(FIRST_REFRESH_TOKEN)).unwrap();
    adapter.mint("a", 100_000).await.unwrap();
    let query = EvidenceQuery {
        nonce: "ab".repeat(32),
        signing_algo: "ed25519".into(),
    };
    assert_eq!(
        adapter.evidence("a", &query).await.unwrap(),
        None,
        "no chat yet"
    );
    for _ in 0..2 {
        let mut chat = adapter.chat("a", rig.chat_request()).await.unwrap();
        while chat.body.next_chunk().await.is_some() {}
    }
    let evidence = adapter.evidence("a", &query).await.unwrap().unwrap();
    let signatures = evidence["signatures"].as_array().unwrap();
    assert_eq!(signatures.len(), 2);
    assert_eq!(signatures[0]["signature"]["signing_algo"], "ed25519");
    assert_eq!(evidence["attestation"]["nonce"], "ab".repeat(32));
    // Both chats are the ones the gateway recorded, fetched with the lease key.
    let ids: Vec<String> = rig
        .fake
        .handle
        .chats()
        .iter()
        .map(|c| c.chat_id.clone())
        .collect();
    for s in signatures {
        assert!(ids.contains(&s["chat_id"].as_str().unwrap().to_string()));
    }
    assert!(rig
        .fake
        .handle
        .chats()
        .iter()
        .all(|c| c.accept_encoding.as_deref() == Some("identity")));
}

#[tokio::test]
async fn a_streamed_chat_is_noted_from_its_first_event() {
    let rig = Rig::new().await;
    let adapter = rig.adapter(Some(FIRST_REFRESH_TOKEN)).unwrap();
    adapter.mint("a", 100_000).await.unwrap();
    let mut request = rig.chat_request();
    request.body = format!(
        r#"{{"model":"{MODEL}","stream":true,"messages":[{{"role":"user","content":"hi"}}]}}"#
    )
    .into();
    let mut chat = adapter.chat("a", request).await.unwrap();
    assert_eq!(chat.content_type, "text/event-stream");
    let mut bytes = Vec::new();
    while let Some(chunk) = chat.body.next_chunk().await {
        bytes.extend_from_slice(&chunk.unwrap());
    }
    assert!(String::from_utf8_lossy(&bytes).ends_with("data: [DONE]\n\n"));
    let query = EvidenceQuery {
        nonce: "cd".repeat(32),
        signing_algo: "ed25519".into(),
    };
    let evidence = adapter.evidence("a", &query).await.unwrap().unwrap();
    assert_eq!(evidence["signatures"].as_array().unwrap().len(), 1);
}
