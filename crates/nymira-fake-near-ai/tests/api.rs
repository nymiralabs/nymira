//! Self-test: starts the fake on port 0 and drives it over HTTP.

use nymira_fake_near_ai::{
    crypto::{self, client_keypair, model_e2ee_public_hex},
    start, verify_attestation, verify_signature, Running, SignatureKind, TestPolicy, SESSION_TOKEN,
};
use reqwest::{Client, Response};
use serde_json::{json, Value};
use std::time::{Duration, Instant};

const MODEL: &str = "z-ai/glm-5.3-flash";
const NONCE: &str = "00ff00ff";

struct T {
    fake: Running,
    http: Client,
}

async fn t() -> T {
    T {
        fake: start("127.0.0.1:0".parse().unwrap()).await.unwrap(),
        http: Client::new(),
    }
}

impl T {
    fn url(&self, p: &str) -> String {
        format!("{}{p}", self.fake.url())
    }
    /// Mints a key through the API and returns its secret.
    async fn mint(&self, name: &str, limit_nano: Option<u64>) -> String {
        let mut body = json!({"name": name, "expires_at": "2099-01-01T00:00:00Z"});
        if let Some(a) = limit_nano {
            body["spendLimit"] = json!({"amount": a, "currency": "USD"});
        }
        let r = self
            .http
            .post(self.url("/v1/workspaces/ws/api-keys"))
            .bearer_auth(SESSION_TOKEN)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        r.json::<Value>().await.unwrap()["key"]
            .as_str()
            .unwrap()
            .to_string()
    }
    async fn chat(&self, key: &str, body: &Value) -> Response {
        self.http
            .post(self.url("/v1/chat/completions"))
            .bearer_auth(key)
            .json(body)
            .send()
            .await
            .unwrap()
    }
    async fn get_json(&self, key: &str, path: &str) -> Value {
        let r = self
            .http
            .get(self.url(path))
            .bearer_auth(key)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "{path}");
        r.json().await.unwrap()
    }
    async fn report(&self, key: &str) -> Value {
        self.get_json(
            key,
            &format!("/v1/attestation/report?model={MODEL}&signing_algo=ed25519&nonce={NONCE}"),
        )
        .await
    }
    async fn signature(&self, key: &str, chat_id: &str) -> Value {
        self.get_json(
            key,
            &format!("/v1/signature/{chat_id}?model={MODEL}&signing_algo=ed25519"),
        )
        .await
    }
    /// Sends a chat, then verifies signature and attestation against the exact bytes.
    async fn verified(&self, key: &str, body: &Value) -> Result<SignatureKind, String> {
        let r = self.chat(key, body).await;
        assert_eq!(r.status(), 200);
        let resp = r.bytes().await.unwrap();
        let chat = self.fake.handle.chats().pop().unwrap();
        assert_eq!(
            chat.response,
            resp.to_vec(),
            "recorded response equals the bytes on the wire"
        );
        let att = verify_attestation(&TestPolicy::fake(), &self.report(key).await, MODEL, NONCE)?;
        verify_signature(
            &att,
            &self.signature(key, &chat.chat_id).await,
            MODEL,
            &chat.body,
            &resp,
        )
    }
}

fn user(text: &str) -> Value {
    json!({"model": MODEL, "messages": [{"role": "user", "content": text}]})
}

#[tokio::test]
async fn model_list_marks_provider_type() {
    let t = t().await;
    let v: Value = t
        .http
        .get(t.url("/v1/model/list"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let vllm: Vec<_> = v["models"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| {
            m["metadata"]["providerType"] == "vllm" && m["metadata"]["attestationSupported"] == true
        })
        .collect();
    assert_eq!(vllm.len(), 4);
    assert_eq!(vllm[0]["outputCostPerToken"]["amount"], 500);
}

#[tokio::test]
async fn key_lifecycle_and_recording() {
    let t = t().await;
    let bad = t
        .http
        .post(t.url("/v1/workspaces/ws/api-keys"))
        .bearer_auth("nope")
        .json(&json!({"name": "a"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);

    let key = t.mint("lease-a", Some(1_000_000)).await;
    let dup = t
        .http
        .post(t.url("/v1/workspaces/ws/api-keys"))
        .bearer_auth(SESSION_TOKEN)
        .json(&json!({"name": "lease-a"}))
        .send()
        .await
        .unwrap();
    assert_eq!(dup.status(), 409);

    let minted = t.fake.handle.keys();
    assert_eq!((minted.len(), minted[0].secret.as_str()), (1, key.as_str()));
    assert_eq!(minted[0].spend_limit.as_ref().unwrap().amount, 1_000_000);

    // List hides the secret and pages.
    let list: Value = t
        .http
        .get(t.url("/v1/workspaces/ws/api-keys?limit=1"))
        .bearer_auth(SESSION_TOKEN)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(list["total"], 1);
    assert!(list["api_keys"][0].get("key").is_none());

    // Lower the limit, then deactivate: chat stops working, signature fetch too.
    let id = minted[0].id.clone();
    let p = t
        .http
        .patch(t.url(&format!("/v1/workspaces/ws/api-keys/{id}")))
        .bearer_auth(SESSION_TOKEN)
        .json(&json!({"spendLimit": {"amount": 5, "currency": "USD"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(p.status(), 200);
    assert_eq!(
        t.fake.handle.keys()[0].spend_limit.as_ref().unwrap().amount,
        5
    );
    t.http
        .patch(t.url(&format!("/v1/workspaces/ws/api-keys/{id}")))
        .bearer_auth(SESSION_TOKEN)
        .json(&json!({"is_active": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(t.chat(&key, &user("hi")).await.status(), 401);

    let k2 = t.mint("lease-b", None).await;
    let del = t
        .http
        .delete(t.url("/v1/workspaces/ws/api-keys/key-2"))
        .bearer_auth(SESSION_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(del.status(), 204);
    assert_eq!(t.chat(&k2, &user("hi")).await.status(), 401);
    assert!(t.fake.handle.keys()[1].deleted);

    let ctl: Value = t
        .http
        .get(t.url("/control/keys"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(ctl.as_array().unwrap().len(), 2);
    assert_eq!(ctl[1]["key"], k2);
}

#[tokio::test]
async fn chat_non_streamed_is_signed_by_the_model() {
    let t = t().await;
    let key = t.mint("k", None).await;
    let r = t.chat(&key, &user("hello there")).await;
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(
        v["choices"][0]["message"]["content"],
        "fake reply to: hello there"
    );
    assert_eq!(v["usage"]["prompt_tokens"], 3);
    assert_eq!(
        t.verified(&key, &user("hello there")).await,
        Ok(SignatureKind::ProviderTee)
    );
}

#[tokio::test]
async fn chat_streamed_has_usage_and_a_gateway_signature() {
    let t = t().await;
    let key = t.mint("k", None).await;
    let mut body = user("stream me");
    body["stream"] = json!(true);
    body["stream_options"] = json!({"include_usage": true});
    let text = t.chat(&key, &body).await.text().await.unwrap();
    assert!(text.ends_with("data: [DONE]\n\n"));
    let events: Vec<Value> = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter(|d| *d != "[DONE]")
        .map(|d| serde_json::from_str(d).unwrap())
        .collect();
    let joined: String = events
        .iter()
        .filter_map(|e| e["choices"][0]["delta"]["content"].as_str())
        .collect();
    assert_eq!(joined, "fake reply to: stream me");
    assert_eq!(events.last().unwrap()["usage"]["completion_tokens"], 6);

    assert_eq!(t.verified(&key, &body).await, Ok(SignatureKind::Gateway));
    // Streams can be switched to the model signature.
    t.fake
        .handle
        .update(|s| s.stream_signature_kind = SignatureKind::ProviderTee);
    assert_eq!(
        t.verified(&key, &body).await,
        Ok(SignatureKind::ProviderTee)
    );

    // Without include_usage there is no usage chunk.
    body["stream_options"] = json!({});
    let text = t.chat(&key, &body).await.text().await.unwrap();
    assert!(!text.contains("\"usage\""));
}

#[tokio::test]
async fn e2ee_round_trip_with_all_fields() {
    let t = t().await;
    let key = t.mint("k", None).await;
    let model_pk = crypto::parse_key32(&model_e2ee_public_hex()).unwrap();
    let (client_sk, client_pk) = client_keypair();
    let body = json!({"model": MODEL, "stream": true, "stream_options": {"include_usage": true},
        "messages": [{"role": "user", "name": crypto::encrypt_hex(&model_pk, "alice"), "content": crypto::encrypt_hex(&model_pk, "secret question")}]});
    let send = |model_key: String| {
        t.http
            .post(t.url("/v1/chat/completions"))
            .bearer_auth(&key)
            .header("X-Encrypt-All-Fields", "true")
            .header("x-no-aliasing", "true")
            .header("X-Signing-Algo", "ed25519")
            .header("X-Client-Pub-Key", hex::encode(client_pk))
            .header("X-Model-Pub-Key", model_key)
            .json(&body)
            .send()
    };
    let text = send(model_e2ee_public_hex())
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert!(!text.contains("fake reply"), "stream content is encrypted");
    let plain: String = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| serde_json::from_str::<Value>(d).ok())
        .filter_map(|e| {
            e["choices"][0]["delta"]["content"]
                .as_str()
                .filter(|c| !c.is_empty())
                .map(str::to_string)
        })
        .map(|c| crypto::decrypt_hex(&client_sk, &c).unwrap())
        .collect();
    assert_eq!(plain, "fake reply to: secret question");

    let rec = t.fake.handle.chats().pop().unwrap();
    assert!(rec.e2ee && rec.encrypt_all_fields && rec.no_aliasing && rec.include_usage);

    // Non-streamed, same scheme. The request body the fake signed is the ciphertext body.
    let mut ns = body.clone();
    ns["stream"] = json!(false);
    let v: Value = t
        .http
        .post(t.url("/v1/chat/completions"))
        .bearer_auth(&key)
        .header("X-Client-Pub-Key", hex::encode(client_pk))
        .header("X-Model-Pub-Key", model_e2ee_public_hex())
        .json(&ns)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    // Without X-Encrypt-All-Fields the plain-text `name` is not decrypted, only `content` is.
    assert_eq!(
        crypto::decrypt_hex(
            &client_sk,
            v["choices"][0]["message"]["content"].as_str().unwrap()
        )
        .unwrap(),
        "fake reply to: secret question"
    );

    // Wrong model key, plaintext content under E2EE headers, and a bad ciphertext are all 400.
    assert_eq!(send(hex::encode([9u8; 32])).await.unwrap().status(), 400);
    let mut junk = body.clone();
    junk["messages"][0]["content"] = json!("not encrypted");
    let r = t
        .http
        .post(t.url("/v1/chat/completions"))
        .bearer_auth(&key)
        .header("X-Client-Pub-Key", hex::encode(client_pk))
        .header("X-Model-Pub-Key", model_e2ee_public_hex())
        .json(&junk)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
}

#[tokio::test]
async fn usage_balance_and_spend_limit() {
    let t = t().await;
    let key = t.mint("k", Some(600)).await;
    let bal = |t: &T| {
        let r = t
            .http
            .get(t.url("/v1/organizations/org/usage/balance"))
            .bearer_auth(SESSION_TOKEN)
            .send();
        async move {
            r.await.unwrap().json::<Value>().await.unwrap()["remaining"]
                .as_i64()
                .unwrap()
        }
    };
    assert_eq!(bal(&t).await, 10_000_000_000);

    // 3 input tokens * 80 + 7 output * 500 = 3740 nano-USD, over the 600 limit.
    assert_eq!(t.chat(&key, &user("hello there")).await.status(), 200);
    assert_eq!(bal(&t).await, 10_000_000_000 - 3740);
    let r = t.chat(&key, &user("again")).await;
    assert_eq!(r.status(), 402);

    let hist = |p: &str| t.http.get(t.url(p)).bearer_auth(SESSION_TOKEN).send();
    let id = t.fake.handle.keys()[0].id.clone();
    let history: Value = hist(&format!(
        "/v1/organizations/org/usage/history?api_key_id={id}"
    ))
    .await
    .unwrap()
    .json()
    .await
    .unwrap();
    assert_eq!(history["data"][0]["total_cost"], 3740);
    assert_eq!(history["data"][0]["input_tokens"], 3);
    assert_eq!(history["total"], 1);
    let other: Value = hist("/v1/organizations/org/usage/history?api_key_id=key-99")
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(other["total"], 0);
    // A reporting token is no session.
    let b = t
        .http
        .get(t.url("/v1/organizations/org/usage/balance"))
        .bearer_auth("rpt-x")
        .send()
        .await
        .unwrap();
    assert_eq!(b.status(), 401);

    // Zero credit stops chat for every key.
    let k2 = t.mint("k2", None).await;
    t.fake.handle.update(|s| s.balance_nano = 0);
    assert_eq!(t.chat(&k2, &user("x")).await.status(), 402);
}

#[tokio::test]
async fn switches_fail_latency_usage() {
    let t = t().await;
    let key = t.mint("k", None).await;

    t.fake
        .handle
        .update(|s| (s.fail_next, s.fail_status) = (2, 429));
    for _ in 0..2 {
        let r = t.chat(&key, &user("x")).await;
        assert_eq!(r.status(), 429);
        assert_eq!(r.headers()[reqwest::header::RETRY_AFTER], "17");
        assert_eq!(
            r.json::<Value>().await.unwrap()["error"]["code"],
            "rate_limit_exceeded"
        );
    }
    assert_eq!(t.chat(&key, &user("x")).await.status(), 200);

    // A prefix confines failures: usage reads fail, chat does not.
    t.fake.handle.update(|s| {
        (s.fail_next, s.fail_status, s.fail_path_prefix) = (1, 503, "/v1/organizations".into())
    });
    assert_eq!(t.chat(&key, &user("x")).await.status(), 200);
    let r = t
        .http
        .get(t.url("/v1/organizations/org/usage/history"))
        .bearer_auth(SESSION_TOKEN)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);

    t.fake.handle.update(|s| s.latency_ms = 150);
    let start = Instant::now();
    assert_eq!(t.chat(&key, &user("x")).await.status(), 200);
    assert!(start.elapsed() >= Duration::from_millis(150));
    t.fake.handle.update(|s| s.latency_ms = 0);

    // Inflated usage shows in the answer, the export and the cost.
    t.fake.handle.update(|s| {
        s.usage = Some(nymira_fake_near_ai::Usage {
            prompt_tokens: 1_000,
            completion_tokens: 2_000,
        })
    });
    let v: Value = t.chat(&key, &user("x")).await.json().await.unwrap();
    assert_eq!(v["usage"]["total_tokens"], 3_000);
    let row = t.fake.handle.usage_rows().pop().unwrap();
    assert_eq!(row.cost_nano_usd, 1_000 * 80 + 2_000 * 500);

    // Price change shows in the live list.
    t.fake
        .handle
        .update(|s| s.output_price_nano_per_token = 600);
    let v: Value = t
        .http
        .get(t.url("/v1/model/list"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["models"][0]["outputCostPerToken"]["amount"], 600);
}

#[tokio::test]
async fn switches_break_verification() {
    let t = t().await;
    let key = t.mint("k", None).await;
    let body = user("check me");
    assert_eq!(
        t.verified(&key, &body).await,
        Ok(SignatureKind::ProviderTee)
    );

    t.fake.handle.update(|s| s.measurement = "ab".repeat(32));
    assert!(t
        .verified(&key, &body)
        .await
        .unwrap_err()
        .contains("measurement"));
    t.fake
        .handle
        .update(|s| s.measurement = nymira_fake_near_ai::policy::default_measurement());

    t.fake.handle.update(|s| s.tcb_status = "OutOfDate".into());
    assert!(t.verified(&key, &body).await.unwrap_err().contains("tcb"));
    t.fake.handle.update(|s| s.tcb_status = "UpToDate".into());

    t.fake.handle.update(|s| s.bad_signature = true);
    assert!(t
        .verified(&key, &body)
        .await
        .unwrap_err()
        .contains("does not verify"));
    t.fake.handle.update(|s| s.bad_signature = false);

    t.fake.handle.update(|s| s.unbound_key = true);
    assert!(t
        .verified(&key, &body)
        .await
        .unwrap_err()
        .contains("not the attested key"));
    t.fake.handle.update(|s| s.unbound_key = false);

    assert_eq!(
        t.verified(&key, &body).await,
        Ok(SignatureKind::ProviderTee)
    );
}

#[tokio::test]
async fn signature_and_attestation_input_rules() {
    let t = t().await;
    let key = t.mint("k", None).await;
    let other = t.mint("other", None).await;
    t.chat(&key, &user("x")).await;
    let id = t.fake.handle.chats()[0].chat_id.clone();
    let get = |k: &str, p: String| t.http.get(t.url(&p)).bearer_auth(k.to_string()).send();

    assert_eq!(
        get(&key, format!("/v1/signature/{id}?signing_algo=ecdsa"))
            .await
            .unwrap()
            .status(),
        400
    );
    // Another key cannot read this key's signature, nor can an unknown id be fetched.
    assert_eq!(
        get(&other, format!("/v1/signature/{id}"))
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        get(&key, "/v1/signature/nope".into())
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        get(&key, "/v1/attestation/report?nonce=zz".into())
            .await
            .unwrap()
            .status(),
        400
    );
    // The attestation report is public, as the live one is: an unknown key does not matter.
    assert_eq!(
        get("sk-unknown", "/v1/attestation/report".into())
            .await
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test]
async fn control_http_endpoints() {
    let t = t().await;
    let key = t.mint("k", None).await;

    let r = t.http.post(t.url("/control/switches")).json(&json!({"measurement": "cd".repeat(32), "usage": {"prompt_tokens": 5, "completion_tokens": 6}})).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(t.fake.handle.switches().usage.unwrap().completion_tokens, 6);
    assert_eq!(
        t.report(&key).await["gateway_attestation"]["measurement"],
        "cd".repeat(32)
    );

    // null clears an option; an unknown switch is rejected and changes nothing.
    t.http
        .post(t.url("/control/switches"))
        .json(&json!({"usage": null}))
        .send()
        .await
        .unwrap();
    assert!(t.fake.handle.switches().usage.is_none());
    let r = t
        .http
        .post(t.url("/control/switches"))
        .json(&json!({"typo": 1}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);

    // The policy endpoint matches the library value, and accepts the default report only after reset.
    let policy: TestPolicy = t
        .http
        .get(t.url("/control/policy"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(policy, TestPolicy::fake());
    assert!(verify_attestation(&policy, &t.report(&key).await, MODEL, NONCE).is_err());
    assert_eq!(
        t.http
            .post(t.url("/control/reset"))
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert!(t.fake.handle.keys().is_empty());
    let key = t.mint("k", None).await;
    assert!(verify_attestation(&policy, &t.report(&key).await, MODEL, NONCE).is_ok());

    // Control stays reachable while every /v1 request is failing.
    t.fake.handle.update(|s| s.fail_next = 100);
    assert_eq!(
        t.http
            .get(t.url("/control/switches"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
}

#[tokio::test]
async fn unknown_model_and_bad_body() {
    let t = t().await;
    let key = t.mint("k", None).await;
    let mut b = user("x");
    b["model"] = json!("not/a-model");
    assert_eq!(t.chat(&key, &b).await.status(), 404);
    let r = t
        .http
        .post(t.url("/v1/chat/completions"))
        .bearer_auth(&key)
        .body("{")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
}
