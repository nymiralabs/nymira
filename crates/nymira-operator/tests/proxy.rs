mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use nymira_operator::clock::{Clock, ManualClock};
use nymira_operator::error::Code;
use nymira_operator::operator::Config;
use nymira_operator::provider::{ChatScript, LiveModel, ProviderError};
use nymira_operator::proxy::{
    forward_headers, parse_chat, valid_nonce, valid_signing_algo, Limiter, ProxyConfig,
};
use rand::{Rng, SeedableRng};
use serde_json::{json, Value};

const MODEL: &str = "z-ai/glm-5.3-flash";

struct Lease {
    id: String,
    token: String,
}

fn proxy_setup(tune: impl FnOnce(&mut Config) + 'static) -> Setup {
    Setup {
        tune: Box::new(move |c| {
            c.proxy.first_byte = Duration::from_millis(400);
            c.proxy.idle = Duration::from_millis(300);
            c.proxy.total = Duration::from_millis(1500);
            tune(c);
        }),
        ..Setup::default()
    }
}

async fn open(rig: &Rig, n: u64) -> Lease {
    let (status, body, _) = rig.lease(&rig.fresh(n)).await;
    assert_eq!(status, 201, "{body}");
    Lease {
        id: body["lease_id"].as_str().unwrap().into(),
        token: body["lease_token"].as_str().unwrap().into(),
    }
}

/// `payload_bytes` bytes of hex ciphertext behind the 72-byte envelope.
fn content(payload_bytes: usize) -> String {
    "ab".repeat(72 + payload_bytes)
}

fn body(max_tokens: Option<u64>, stream: bool) -> Value {
    let mut v = json!({"model": MODEL, "messages": [{"role": "user", "content": content(8)}], "stream": stream});
    if let Some(m) = max_tokens {
        v["max_tokens"] = m.into();
    }
    v
}

fn e2ee(request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
    request
        .header("x-signing-algo", "ed25519")
        .header("x-client-pub-key", "11".repeat(32))
        .header("x-model-pub-key", "22".repeat(32))
        .header("x-encryption-version", "2")
}

fn post(rig: &Rig, l: &Lease, body: Vec<u8>) -> reqwest::RequestBuilder {
    e2ee(rig.http.post(format!("{}/v1/chat/completions", rig.url)))
        .header("x-nymira-lease-id", &l.id)
        .header("x-nymira-lease-token", &l.token)
        .header("content-type", "application/json")
        .body(body)
}

async fn chat(rig: &Rig, l: &Lease, v: &Value) -> (u16, Vec<u8>, reqwest::header::HeaderMap) {
    let r = post(rig, l, serde_json::to_vec(v).unwrap())
        .send()
        .await
        .unwrap();
    let (status, headers) = (r.status().as_u16(), r.headers().clone());
    (
        status,
        r.bytes().await.map(|b| b.to_vec()).unwrap_or_default(),
        headers,
    )
}

/// Counts the `data:` events a client receives before the stream ends or is cut.
async fn count_events(mut r: reqwest::Response) -> usize {
    let mut events = 0;
    while let Ok(Some(chunk)) = r.chunk().await {
        events += String::from_utf8_lossy(&chunk).matches("data: ").count();
    }
    events
}

fn err_code(bytes: &[u8]) -> String {
    serde_json::from_slice::<Value>(bytes)
        .map(|v| code(&v).to_string())
        .unwrap_or_default()
}

async fn settled(rig: &Rig) {
    for _ in 0..200 {
        if rig.provider.open_streams() == 0 {
            tokio::time::sleep(Duration::from_millis(20)).await;
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("upstream never closed");
}

// ---------------------------------------------------------------- what is forwarded

#[tokio::test(flavor = "multi_thread")]
async fn a_valid_request_is_forwarded_as_the_validated_bytes_with_only_the_encryption_headers() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    let l = open(&rig, 1).await;
    let sent = body(Some(50), false);
    let (status, answer, headers) = chat(&rig, &l, &sent).await;
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&answer));
    assert_eq!(headers["content-type"], "application/json");
    let v: Value = serde_json::from_slice(&answer).unwrap();
    assert_eq!(v["usage"]["completion_tokens"], 20);

    let calls = rig.provider.chat_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].lease_id, l.id);
    let forwarded: Value = serde_json::from_slice(&calls[0].body).unwrap();
    let mut expected = sent.clone();
    expected["stream_options"] = json!({"include_usage": true});
    assert_eq!(
        forwarded, expected,
        "the validated value, with usage switched on"
    );
    let mut names: Vec<_> = calls[0].headers.iter().map(|(n, _)| n.as_str()).collect();
    names.sort();
    assert_eq!(
        names,
        [
            "content-type",
            "x-client-pub-key",
            "x-encrypt-all-fields",
            "x-encryption-version",
            "x-model-pub-key",
            "x-no-aliasing",
            "x-signing-algo"
        ]
    );
    let value = |n: &str| {
        calls[0]
            .headers
            .iter()
            .find(|(h, _)| h == n)
            .unwrap()
            .1
            .clone()
    };
    assert_eq!(
        (
            value("x-encrypt-all-fields").as_str(),
            value("x-no-aliasing").as_str()
        ),
        ("true", "true")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn other_request_headers_never_reach_the_provider() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    let l = open(&rig, 1).await;
    let r = post(
        &rig,
        &l,
        serde_json::to_vec(&body(Some(10), false)).unwrap(),
    )
    .header("authorization", "Bearer sk-not-forwarded")
    .header("cookie", "a=b")
    .header("x-forwarded-for", "203.0.113.9")
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let sent = rig.provider.chat_calls().remove(0).headers;
    assert!(
        sent.iter()
            .all(|(n, v)| (n.starts_with("x-") || n == "content-type")
                && !v.contains("203.0.113.9")
                && !v.contains("sk-")),
        "{sent:?}"
    );
    assert!(!sent.iter().any(|(n, _)| n == "x-forwarded-for"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_arrives_chunk_by_chunk_and_its_usage_replaces_the_reservation() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    let l = open(&rig, 1).await;
    let (status, answer, headers) = chat(&rig, &l, &body(Some(100), true)).await;
    assert_eq!(status, 200);
    assert_eq!(headers["content-type"], "text/event-stream");
    let text = String::from_utf8(answer).unwrap();
    assert_eq!(
        text.matches("data: ").count(),
        5,
        "three deltas, usage, DONE: {text}"
    );
    settled(&rig).await;
    // 10 prompt tokens x 80 + 20 completion tokens x 500 = 10_800 nano = 11 micro-USD.
    assert_eq!(rig.op.spent_estimate(&l.id), 11);
    assert_eq!(
        rig.status().await["daily_budget"]["used"],
        "0",
        "nothing is reserved any more"
    );
}

// ---------------------------------------------------------------- what is refused

#[tokio::test(flavor = "multi_thread")]
async fn the_proxy_rules_refuse_what_they_must() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    let l = open(&rig, 1).await;
    let mut cases: Vec<(&str, Value, u16, &str)> = Vec::new();
    let mut with = |name: &'static str, f: &dyn Fn(&mut Value), status: u16, code: &'static str| {
        let mut v = body(Some(10), false);
        f(&mut v);
        cases.push((name, v, status, code));
    };
    with(
        "unpinned model",
        &|v| v["model"] = "gpt-4o".into(),
        400,
        "unsupported_request",
    );
    with(
        "model case",
        &|v| v["model"] = "Z-AI/GLM-5.3-FLASH".into(),
        400,
        "unsupported_request",
    );
    with(
        "model whitespace",
        &|v| v["model"] = " z-ai/glm-5.3-flash".into(),
        400,
        "unsupported_request",
    );
    with(
        "model alias suffix",
        &|v| v["model"] = "z-ai/glm-5.3-flash:latest".into(),
        400,
        "unsupported_request",
    );
    with(
        "n of two",
        &|v| v["n"] = 2.into(),
        400,
        "unsupported_request",
    );
    with(
        "both token fields",
        &|v| v["max_completion_tokens"] = 10.into(),
        400,
        "max_tokens_invalid",
    );
    with(
        "zero max_tokens",
        &|v| v["max_tokens"] = 0.into(),
        400,
        "malformed_request",
    );
    with(
        "string max_tokens",
        &|v| v["max_tokens"] = "10".into(),
        400,
        "malformed_request",
    );
    with(
        "plugins",
        &|v| v["plugins"] = json!([{"id": "web"}]),
        400,
        "malformed_request",
    );
    with(
        "extra_body",
        &|v| v["extra_body"] = json!({"n": 5}),
        400,
        "malformed_request",
    );
    with(
        "readable tool",
        &|v| v["tools"] = json!([{"type": "function", "function": {"name": "get_weather", "parameters": {"type": "object"}}}]),
        400,
        "unsupported_request",
    );
    with(
        "server-side tool",
        &|v| v["tools"] = json!([{"type": "web_search"}]),
        400,
        "unsupported_request",
    );
    with(
        "non-string content",
        &|v| v["messages"][0]["content"] = json!([{"type": "text", "text": "x"}]),
        400,
        "unsupported_request",
    );
    with(
        "no messages",
        &|v| v["messages"] = json!([]),
        400,
        "malformed_request",
    );
    with(
        "bad stream_options",
        &|v| v["stream_options"] = json!({"include_usage": true, "x": 1}),
        400,
        "malformed_request",
    );
    with(
        "stop of five",
        &|v| v["stop"] = json!(["a", "b", "c", "d", "e"]),
        400,
        "malformed_request",
    );
    for (name, v, status, expected) in cases {
        let (s, answer, _) = chat(&rig, &l, &v).await;
        assert_eq!(
            (s, err_code(&answer).as_str()),
            (status, expected),
            "{name}: {}",
            String::from_utf8_lossy(&answer)
        );
    }
    // Duplicate keys, anywhere.
    let text = serde_json::to_string(&body(Some(10), false)).unwrap();
    for dup in [
        text.replacen("\"model\"", "\"model\":\"gpt-4o\",\"model\"", 1),
        text.replacen("\"role\"", "\"role\":\"system\",\"role\"", 1),
        text.replacen(
            "\"max_tokens\":10",
            "\"max_tokens\":10,\"max_tokens\":99999",
            1,
        ),
    ] {
        let r = post(&rig, &l, dup.into_bytes()).send().await.unwrap();
        assert_eq!(r.status(), 400);
        assert_eq!(err_code(&r.bytes().await.unwrap()), "malformed_request");
    }
    assert!(
        rig.provider.chat_calls().is_empty(),
        "nothing refused reached the provider"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_without_end_to_end_encryption_is_refused_as_unsupported() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    let l = open(&rig, 1).await;
    let plain = || {
        rig.http
            .post(format!("{}/v1/chat/completions", rig.url))
            .header("x-nymira-lease-id", &l.id)
            .header("x-nymira-lease-token", &l.token)
            .body(serde_json::to_vec(&body(Some(10), false)).unwrap())
    };
    let r = plain().send().await.unwrap();
    assert_eq!(
        (
            r.status().as_u16(),
            err_code(&r.bytes().await.unwrap()).as_str()
        ),
        (400, "unsupported_request")
    );
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("x-signing-algo", "rsa".parse().unwrap());
    h.insert("x-client-pub-key", "11".parse().unwrap());
    h.insert("x-model-pub-key", "22".parse().unwrap());
    h.insert("x-encryption-version", "2".parse().unwrap());
    assert!(forward_headers(&h).is_err(), "an unknown signing algorithm");
    h.insert("x-signing-algo", "ed25519".parse().unwrap());
    assert!(forward_headers(&h).is_ok());
    h.insert("x-encryption-version", "1".parse().unwrap());
    assert!(forward_headers(&h).is_err());
    assert!(rig.provider.chat_calls().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_lease_id_alone_is_refused_and_a_retiring_lease_takes_nothing() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    let l = open(&rig, 1).await;
    let v = serde_json::to_vec(&body(Some(10), false)).unwrap();
    let no_token = e2ee(rig.http.post(format!("{}/v1/chat/completions", rig.url)))
        .header("x-nymira-lease-id", &l.id)
        .body(v.clone());
    let r = no_token.send().await.unwrap();
    assert_eq!(
        (
            r.status().as_u16(),
            err_code(&r.bytes().await.unwrap()).as_str()
        ),
        (401, "lease_token_invalid")
    );
    let wrong = Lease {
        id: l.id.clone(),
        token: "0".repeat(32),
    };
    assert_eq!(
        post(&rig, &wrong, v.clone()).send().await.unwrap().status(),
        401
    );
    let ghost = Lease {
        id: "nope".into(),
        token: l.token.clone(),
    };
    let r = post(&rig, &ghost, v.clone()).send().await.unwrap();
    assert_eq!(
        (
            r.status().as_u16(),
            err_code(&r.bytes().await.unwrap()).as_str()
        ),
        (404, "lease_not_found")
    );
    // Retired: no new request.
    let retire = rig
        .http
        .post(format!("{}/v2/openrouter/leases/{}", rig.url, l.id))
        .header("x-nymira-lease-token", &l.token)
        .send()
        .await
        .unwrap();
    assert_eq!(retire.status(), 200);
    let r = post(&rig, &l, v).send().await.unwrap();
    assert_eq!(
        (
            r.status().as_u16(),
            err_code(&r.bytes().await.unwrap()).as_str()
        ),
        (410, "lease_ended")
    );
    assert!(rig.provider.chat_calls().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_chat_body_limit_is_four_mebibytes() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    let l = open(&rig, 1).await;
    let r = post(&rig, &l, vec![b' '; 4 * 1024 * 1024 + 1])
        .send()
        .await
        .unwrap();
    assert_eq!(
        (
            r.status().as_u16(),
            err_code(&r.bytes().await.unwrap()).as_str()
        ),
        (413, "body_too_large")
    );
    let r = post(&rig, &l, vec![b' '; 4 * 1024 * 1024])
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        400,
        "at the limit it is parsed, and is not JSON"
    );
}

// ---------------------------------------------------------------- the spend ledger

#[tokio::test(flavor = "multi_thread")]
async fn the_worst_case_must_fit_nine_tenths_of_the_remaining_bound() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    let l = open(&rig, 1).await;
    // Bound 0.10 USDC = 100_000 micro-USD = 100_000_000 nano-USD; 500 nano per output token and
    // 640 nano of input: 90 % is 90_000_000, so 179_999 output tokens fit and 180_000 do not.
    let (s, a, _) = chat(&rig, &l, &body(Some(180_000), false)).await;
    assert_eq!((s, err_code(&a).as_str()), (400, "max_tokens_too_large"));
    let (s, _, _) = chat(&rig, &l, &body(Some(179_000), false)).await;
    assert_eq!(s, 200);
    // The max_completion_tokens alias counts the same.
    let mut alias = body(None, false);
    alias["max_completion_tokens"] = 180_000.into();
    let (s, a, _) = chat(&rig, &l, &alias).await;
    assert_eq!((s, err_code(&a).as_str()), (400, "max_tokens_too_large"));
    assert_eq!(rig.provider.chat_calls().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn two_concurrent_requests_that_together_exceed_the_bound_cannot_both_go() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    rig.provider.set_chat_script(ChatScript {
        stall_after: Some(1),
        ..ChatScript::default()
    });
    let l = open(&rig, 1).await;
    // 100_000 output tokens reserve 50_000 micro-USD each: one fits, two do not.
    let first = post(
        &rig,
        &l,
        serde_json::to_vec(&body(Some(100_000), true)).unwrap(),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(first.status(), 200);
    let (s, a, _) = chat(&rig, &l, &body(Some(100_000), true)).await;
    assert_eq!((s, err_code(&a).as_str()), (400, "max_tokens_too_large"));
    assert_eq!(
        rig.status().await["daily_budget"]["used"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap(),
        50_001,
        "the first reservation counts against the day"
    );
    drop(first);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_with_neither_field_is_reserved_at_the_models_output_cap() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    rig.provider.set_chat_script(ChatScript {
        stall_after: Some(1),
        ..ChatScript::default()
    });
    let l = open(&rig, 1).await;
    let held = post(&rig, &l, serde_json::to_vec(&body(None, true)).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(held.status(), 200);
    // 8 payload bytes x 80 + 4096 cap tokens x 500 = 2_048_640 nano -> 2_049 micro-USD.
    assert_eq!(rig.status().await["daily_budget"]["used"], "2049");
    drop(held);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_is_cut_when_its_estimated_cost_reaches_its_reservation() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    rig.provider.set_chat_script(ChatScript {
        stream_events: 40,
        chunk_delay: Duration::from_millis(20),
        ..ChatScript::default()
    });
    let l = open(&rig, 1).await;
    // max_tokens 4 reserves (640 + 2000) nano = 3 micro-USD; five content events cost that much.
    let r = post(&rig, &l, serde_json::to_vec(&body(Some(4), true)).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let events = count_events(r).await;
    assert!((1..40).contains(&events), "cut early: {events} events");
    settled(&rig).await;
    assert_eq!(
        rig.op.spent_estimate(&l.id),
        3,
        "a cut stream counts its whole reservation"
    );
    assert_eq!(rig.provider.chat_calls().len(), 1, "never retried upstream");
}

// ---------------------------------------------------------------- concurrency and deadlines

#[tokio::test(flavor = "multi_thread")]
async fn a_third_request_in_flight_on_one_lease_is_refused() {
    let rig = Rig::new(proxy_setup(|c| c.proxy.idle = Duration::from_secs(5))).await;
    rig.provider.set_chat_script(ChatScript {
        stall_after: Some(1),
        ..ChatScript::default()
    });
    let l = open(&rig, 1).await;
    let a = post(&rig, &l, serde_json::to_vec(&body(Some(10), true)).unwrap())
        .send()
        .await
        .unwrap();
    let b = post(&rig, &l, serde_json::to_vec(&body(Some(10), true)).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!((a.status().as_u16(), b.status().as_u16()), (200, 200));
    let (s, answer, headers) = chat(&rig, &l, &body(Some(10), true)).await;
    assert_eq!((s, err_code(&answer).as_str()), (429, "lease_concurrency"));
    assert!(headers.contains_key("retry-after"));
    drop((a, b));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_at_its_limit_refuses_other_leases_with_operator_capacity() {
    let rig = Rig::new(proxy_setup(|c| {
        c.proxy.per_model = 2;
        c.proxy.idle = Duration::from_secs(5);
    }))
    .await;
    rig.provider.set_chat_script(ChatScript {
        stall_after: Some(1),
        ..ChatScript::default()
    });
    let (a, b, c) = (
        open(&rig, 1).await,
        open(&rig, 2).await,
        open(&rig, 3).await,
    );
    let held_a = post(&rig, &a, serde_json::to_vec(&body(Some(10), true)).unwrap())
        .send()
        .await
        .unwrap();
    let held_b = post(&rig, &b, serde_json::to_vec(&body(Some(10), true)).unwrap())
        .send()
        .await
        .unwrap();
    let (s, answer, headers) = chat(&rig, &c, &body(Some(10), true)).await;
    assert_eq!(
        (s, err_code(&answer).as_str()),
        (429, Code::OperatorCapacity.as_str())
    );
    assert!(headers.contains_key("retry-after"));
    drop((held_a, held_b));
}

#[test]
fn above_the_hot_mark_each_lease_gets_one_slot_and_a_lease_that_held_long_gives_way_first() {
    let clock: Arc<dyn Clock> = Arc::new(ManualClock::new(1_000_000));
    let cfg = ProxyConfig {
        per_model: 4,
        per_lease: 2,
        hot_pct: 50,
        ..ProxyConfig::default()
    };
    let limiter = Limiter::new();
    let lifetime = 600_000;
    // Below the mark a lease may hold two slots.
    let a1 = limiter.acquire(&cfg, &clock, MODEL, "a", lifetime).unwrap();
    let a2 = limiter.acquire(&cfg, &clock, MODEL, "a", lifetime).unwrap();
    // Two of four in flight: not hot. A third lease slot is refused by the per-lease limit.
    assert_eq!(
        limiter
            .acquire(&cfg, &clock, MODEL, "a", lifetime)
            .err()
            .unwrap()
            .code,
        Code::LeaseConcurrency
    );
    let b1 = limiter.acquire(&cfg, &clock, MODEL, "b", lifetime).unwrap();
    // Three of four is above 50 %: hot. Lease b has one slot and now gets no second.
    assert_eq!(
        limiter
            .acquire(&cfg, &clock, MODEL, "b", lifetime)
            .err()
            .unwrap()
            .code,
        Code::LeaseConcurrency
    );
    drop((a1, a2));
    assert_eq!(limiter.in_flight(MODEL), 1);
    let _ = b1;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lease_that_held_slots_for_half_its_life_is_refused_first_when_the_model_is_busy() {
    let clock = Arc::new(ManualClock::new(1_000_000));
    let dynclock: Arc<dyn Clock> = clock.clone();
    let cfg = ProxyConfig {
        per_model: 4,
        hot_pct: 50,
        ..ProxyConfig::default()
    };
    let limiter = Limiter::new();
    let lifetime = 100_000;
    let hog = limiter
        .acquire(&cfg, &dynclock, MODEL, "hog", lifetime)
        .unwrap();
    clock.advance_ms(60_000); // held 60 s of a 100 s life
    drop(hog);
    // Make the model hot with other leases.
    let _x = limiter
        .acquire(&cfg, &dynclock, MODEL, "x", lifetime)
        .unwrap();
    let _y = limiter
        .acquire(&cfg, &dynclock, MODEL, "y", lifetime)
        .unwrap();
    let _z = limiter
        .acquire(&cfg, &dynclock, MODEL, "z", lifetime)
        .unwrap();
    let refused = limiter.acquire(&cfg, &dynclock, MODEL, "hog", lifetime);
    assert_eq!(
        refused.err().unwrap().code,
        Code::OperatorCapacity,
        "held over half its life"
    );
    // A lease that held little is admitted to the last slot.
    assert!(limiter
        .acquire(&cfg, &dynclock, MODEL, "fresh", lifetime)
        .is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stalled_upstream_is_cut_at_its_deadline_and_its_slot_comes_back() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    rig.provider.set_chat_script(ChatScript {
        stall_after: Some(1),
        ..ChatScript::default()
    });
    let l = open(&rig, 1).await;
    let r = post(&rig, &l, serde_json::to_vec(&body(Some(10), true)).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(rig.op.limiter_in_flight(&l.id), 1);
    let started = std::time::Instant::now();
    let read = r.bytes().await;
    assert!(read.is_err(), "the stream is cut, not completed");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "within the idle deadline"
    );
    settled(&rig).await;
    assert_eq!(
        rig.op.limiter_in_flight(&l.id),
        0,
        "the slot is back once upstream closed"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn no_first_byte_in_time_is_a_provider_timeout() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    rig.provider.set_chat_script(ChatScript {
        first_byte_delay: Duration::from_secs(5),
        ..ChatScript::default()
    });
    let l = open(&rig, 1).await;
    let (s, a, _) = chat(&rig, &l, &body(Some(10), false)).await;
    assert_eq!((s, err_code(&a).as_str()), (504, "provider_timeout"));
    assert_eq!(rig.op.limiter_in_flight(&l.id), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_total_deadline_ends_a_stream_that_never_stops() {
    let rig = Rig::new(proxy_setup(|c| c.proxy.total = Duration::from_millis(500))).await;
    let chunks: Vec<String> = (0..1000)
        .map(|i| {
            format!(
                "data: {}\n\n",
                json!({"choices": [{"delta": {"content": format!("{i:04x}")}}]})
            )
        })
        .collect();
    rig.provider.set_chat_script(ChatScript {
        chunks: Some(chunks),
        chunk_delay: Duration::from_millis(30),
        ..ChatScript::default()
    });
    let l = open(&rig, 1).await;
    let r = post(
        &rig,
        &l,
        serde_json::to_vec(&body(Some(100_000), true)).unwrap(),
    )
    .send()
    .await
    .unwrap();
    let events = count_events(r).await;
    assert!(
        events < 100,
        "cut by the total deadline after {events} events"
    );
    settled(&rig).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_disconnects_cancels_upstream_and_nothing_is_retried() {
    let rig = Rig::new(proxy_setup(|c| c.proxy.idle = Duration::from_secs(10))).await;
    rig.provider.set_chat_script(ChatScript {
        stall_after: Some(1),
        ..ChatScript::default()
    });
    let l = open(&rig, 1).await;
    let r = post(&rig, &l, serde_json::to_vec(&body(Some(10), true)).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(rig.provider.open_streams(), 1);
    assert_eq!(
        rig.op.limiter_in_flight(&l.id),
        1,
        "the slot is held while upstream is open"
    );
    drop(r);
    settled(&rig).await;
    assert_eq!(rig.provider.open_streams(), 0, "upstream was closed");
    assert_eq!(
        rig.op.limiter_in_flight(&l.id),
        0,
        "and only then is the slot free"
    );
    assert_eq!(rig.provider.chat_calls().len(), 1, "never retried");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_that_outlives_the_drain_deadline_is_cancelled_by_retirement() {
    let rig = Rig::new(proxy_setup(|c| c.proxy.idle = Duration::from_secs(30))).await;
    rig.provider.set_chat_script(ChatScript {
        stall_after: Some(1),
        ..ChatScript::default()
    });
    let l = open(&rig, 1).await;
    let held = post(&rig, &l, serde_json::to_vec(&body(Some(10), true)).unwrap())
        .send()
        .await
        .unwrap();
    rig.http
        .post(format!("{}/v2/openrouter/leases/{}", rig.url, l.id))
        .header("x-nymira-lease-token", &l.token)
        .send()
        .await
        .unwrap();
    rig.clock.advance_secs(119);
    rig.op.tick().await;
    assert_eq!(rig.provider.open_streams(), 1, "still inside its deadline");
    rig.clock.advance_secs(1);
    rig.op.tick().await;
    settled(&rig).await;
    assert_eq!(
        rig.provider.open_streams(),
        0,
        "upstream aborted at the deadline"
    );
    assert!(rig.provider.keys()[&l.id].revoked);
    drop(held);
}

// ---------------------------------------------------------------- provider errors

#[tokio::test(flavor = "multi_thread")]
async fn the_providers_429_is_passed_on_as_a_different_error_from_the_operators_own() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    let l = open(&rig, 1).await;
    rig.provider.set_chat_script(ChatScript {
        error: Some(ProviderError::RateLimited {
            retry_after_secs: 7,
        }),
        ..ChatScript::default()
    });
    let (s, a, h) = chat(&rig, &l, &body(Some(10), false)).await;
    assert_eq!((s, err_code(&a).as_str()), (429, "provider_rate_limit"));
    assert_eq!(h["retry-after"], "7");
    rig.provider.set_chat_script(ChatScript {
        error: Some(ProviderError::Unavailable("secret detail".into())),
        ..ChatScript::default()
    });
    let (s, a, _) = chat(&rig, &l, &body(Some(10), false)).await;
    assert_eq!((s, err_code(&a).as_str()), (502, "provider_error"));
    assert!(
        !String::from_utf8_lossy(&a).contains("secret detail"),
        "no upstream text"
    );
    assert_eq!(
        rig.op.spent_estimate(&l.id),
        0,
        "a request that never started costs nothing"
    );
}

// ---------------------------------------------------------------- delivery, retire, evidence

#[tokio::test(flavor = "multi_thread")]
async fn the_first_proxied_request_is_a_delivery_and_expire_is_refused_after_it() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    let l = open(&rig, 1).await;
    assert_eq!(chat(&rig, &l, &body(Some(10), false)).await.0, 200);
    let r = rig
        .http
        .post(format!("{}/v2/openrouter/leases/{}/expire", rig.url, l.id))
        .header("x-nymira-lease-token", &l.token)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 409);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_providers_evidence_is_fetched_after_the_drain_before_the_key_is_deactivated_and_kept()
{
    let rig = Rig::new(proxy_setup(|c| c.proxy.idle = Duration::from_secs(10))).await;
    let evidence = json!({"signature": {"text": "ab", "signing_algo": "ed25519"}, "attestation": {"report": "cd"}});
    rig.provider.set_evidence(Some(evidence.clone()));
    rig.provider.set_chat_script(ChatScript {
        stall_after: Some(1),
        ..ChatScript::default()
    });
    let l = open(&rig, 1).await;
    let held = post(&rig, &l, serde_json::to_vec(&body(Some(10), true)).unwrap())
        .send()
        .await
        .unwrap();
    let path = format!("{}/v2/openrouter/leases/{}", rig.url, l.id);
    rig.http
        .post(&path)
        .header("x-nymira-lease-token", &l.token)
        .send()
        .await
        .unwrap();
    rig.op.tick().await;
    let during: Value = rig
        .http
        .get(&path)
        .header("x-nymira-lease-token", &l.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        during.get("provider_evidence").is_none(),
        "nothing is fetched while a request runs"
    );
    assert!(!rig.provider.keys()[&l.id].revoked);
    drop(held);
    settled(&rig).await;
    rig.op.tick().await;
    assert!(rig.provider.keys()[&l.id].revoked);
    let after: Value = rig
        .http
        .get(&path)
        .header("x-nymira-lease-token", &l.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(after["provider_evidence"], evidence, "passed on unchanged");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_retire_with_a_request_in_flight_stays_inside_the_bound() {
    let rig = Rig::new(proxy_setup(|c| c.proxy.idle = Duration::from_secs(10))).await;
    rig.provider.set_chat_script(ChatScript {
        stall_after: Some(1),
        usage: (10, 20),
        ..ChatScript::default()
    });
    let l = open(&rig, 1).await;
    let held = post(
        &rig,
        &l,
        serde_json::to_vec(&body(Some(179_000), true)).unwrap(),
    )
    .send()
    .await
    .unwrap();
    rig.http
        .post(format!("{}/v2/openrouter/leases/{}", rig.url, l.id))
        .header("x-nymira-lease-token", &l.token)
        .send()
        .await
        .unwrap();
    rig.provider.set_usage(&l.id, 5_000_000_000_000); // the provider reports a huge spend
    drop(held);
    settled(&rig).await;
    for _ in 0..3 {
        rig.op.tick().await;
        rig.clock.advance_secs(11);
    }
    let v: Value = rig
        .http
        .get(format!("{}/v2/openrouter/leases/{}", rig.url, l.id))
        .header("x-nymira-lease-token", &l.token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        v["charge_applied"], "100000",
        "the charge is capped at the bound"
    );
}

// ---------------------------------------------------------------- the pinned model table

fn live(price_in: u64, price_out: u64, kind: &str, attested: bool) -> Vec<LiveModel> {
    ProxyConfig::default()
        .models
        .iter()
        .map(|m| LiveModel {
            id: m.id.clone(),
            provider_type: kind.into(),
            attestation_supported: attested,
            input_nano_per_token: if m.id == MODEL {
                price_in
            } else {
                m.input_nano_per_token
            },
            output_nano_per_token: if m.id == MODEL {
                price_out
            } else {
                m.output_nano_per_token
            },
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_model_whose_live_price_differs_from_its_pin_is_blocked_and_the_check_repeats() {
    let mut rig = Rig::new(proxy_setup(|_| {})).await;
    let l = open(&rig, 1).await;
    let held = open(&rig, 4).await;
    rig.provider.set_models(Some(live(80, 500, "vllm", true)));
    rig.op.check_models().await;
    assert!(rig.op.blocked_models().is_empty());
    assert_eq!(rig.op.minting_stop(), None);
    assert_eq!(chat(&rig, &l, &body(Some(10), false)).await.0, 200);

    rig.provider.set_models(Some(live(80, 900, "vllm", true)));
    rig.clock.advance_secs(150);
    assert_eq!(chat(&rig, &held, &body(Some(10), false)).await.0, 200); // keeps it from idling out
    rig.clock.advance_secs(151);
    rig.op.tick().await; // the five-minute check (and the idle lease retires)
    assert!(rig.op.blocked_models().contains(MODEL));
    let (s, a, _) = chat(&rig, &held, &body(Some(10), false)).await;
    assert_eq!((s, err_code(&a).as_str()), (400, "unsupported_request"));
    // A different price also stops minting, and a person is told.
    assert_eq!(rig.op.minting_stop(), Some("price"));
    let (s, a, _) = rig.lease(&rig.fresh(2)).await;
    assert_eq!(
        (s, a["error"]["code"].as_str()),
        (429, Some("operator_capacity"))
    );
    assert_eq!(rig.sink.sent().len(), 1);
    assert_eq!(rig.sink.sent()[0].key, "model_mismatch");

    // Back to the pin: unblocked on the next check. A model that is not attested is blocked too.
    rig.provider.set_models(Some(live(80, 500, "vllm", false)));
    rig.clock.advance_secs(301);
    rig.op.tick().await;
    assert!(rig.op.blocked_models().contains(MODEL));
    assert_eq!(
        rig.op.minting_stop(),
        None,
        "not attested blocks the model, not minting"
    );
    rig.provider.set_models(Some(live(80, 500, "vllm", true)));
    rig.clock.advance_secs(301);
    rig.op.tick().await;
    assert!(rig.op.blocked_models().is_empty());

    // The comparison also runs at start-up.
    rig.provider.set_models(Some(live(1, 1, "vllm", true)));
    rig.restart(proxy_setup(|_| {})).await;
    assert!(rig.op.blocked_models().contains(MODEL));
}

// ---------------------------------------------------------------- strict formats and the odd-body corpus

#[test]
fn nonces_and_signing_algorithms_are_strictly_formatted() {
    assert!(valid_nonce(&"ab".repeat(32)));
    for bad in [
        "",
        &"AB".repeat(32),
        &"ab".repeat(31),
        &"ab".repeat(33),
        &format!("{}\n", "a".repeat(63)),
        &format!("{}%2e", "a".repeat(62)),
    ] {
        assert!(!valid_nonce(bad), "{bad:?}");
    }
    assert!(valid_signing_algo("ed25519") && valid_signing_algo("ecdsa"));
    for bad in ["", "ED25519", "ed25519 ", "rsa", "ed25519,ecdsa"] {
        assert!(!valid_signing_algo(bad), "{bad:?}");
    }
}

#[test]
fn the_parser_and_a_reference_parser_agree_over_the_odd_body_corpus_and_random_mutations() {
    let cfg = ProxyConfig::default();
    let blocked = Default::default();
    let good = serde_json::to_string(&body(Some(10), true)).unwrap();
    let mut corpus: Vec<String> = vec![
        good.clone(),
        good.replacen("\"model\"", "\"model\":\"gpt-4o\",\"model\"", 1),
        good.replace("z-ai/glm-5.3-flash", "Z-AI/GLM-5.3-FLASH"),
        good.replace("\"z-ai/glm-5.3-flash\"", "\"z-ai/glm-5.3-flash \""),
        good.replace("\"z-ai/glm-5.3-flash\"", "\" z-ai/glm-5.3-flash\""),
        good.replace("\"model\":", "\"model\" :"),
        good.replace("\"model\":", "\"mod\\u0065l\":"),
        good.replace("}", ",\"extra_body\":{\"n\":3}}"),
        good.replacen("{", "{\"n\":1,", 1),
        good.replacen("{", "{\"n\":2,", 1),
        good.replacen("{", "{\"max_completion_tokens\":5,", 1),
        good.replacen("{", "{\"max_tokens\":5,\"max_tokens\":6,", 1),
        format!("  {good}  "),
        format!("{good}\n{good}"),
        "[]".into(),
        "null".into(),
        String::new(),
    ];
    let mut rng = rand::rngs::StdRng::seed_from_u64(24);
    for _ in 0..1500 {
        let mut s = good.clone();
        for _ in 0..rng.gen_range(1..4) {
            match rng.gen_range(0..6) {
                0 => s = s.replacen("\"model\"", "\"model\":\"z-ai/glm-5.3-flash\",\"model\"", 1),
                1 => s = s.replace("glm", ["GLM", "gl m", "glm ", "g1m"][rng.gen_range(0..4)]),
                2 => {
                    s = s.replacen(
                        '{',
                        &format!(
                            "{{\"{}\":{},",
                            ["n", "max_completion_tokens", "plugins", "tools"][rng.gen_range(0..4)],
                            rng.gen_range(0..3)
                        ),
                        1,
                    )
                }
                3 => s = s.replace(',', " , "),
                4 => s = s.replacen('"', "\\\"", 1),
                _ => s.insert(
                    rng.gen_range(0..s.len()),
                    [' ', '\n', '\t'][rng.gen_range(0..3)],
                ),
            }
        }
        corpus.push(s);
    }
    for raw in corpus {
        let ours = parse_chat(raw.as_bytes(), &cfg, &blocked);
        let reference = nymira_operator::wire::reference_accepts(&raw, &cfg);
        assert_eq!(
            ours.is_ok(),
            reference,
            "disagreement on {raw:?}: ours {:?}",
            ours.as_ref().err().map(|e| e.code)
        );
        if let Ok(parsed) = ours {
            // What is forwarded parses to exactly one pinned model, one of the two limits, no dup keys.
            let forwarded: Value = serde_json::from_slice(&parsed.body).unwrap();
            assert!(cfg
                .models
                .iter()
                .any(|m| forwarded["model"] == m.id.as_str()));
            assert_eq!(forwarded["stream_options"], json!({"include_usage": true}));
            assert!(
                forwarded.get("max_tokens").is_none()
                    || forwarded.get("max_completion_tokens").is_none()
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_stops_reading_does_not_pin_the_slot() {
    let rig = Rig::new(proxy_setup(|c| c.proxy.total = Duration::from_secs(30))).await;
    // Far more than the socket buffers hold, sent as fast as the client allows.
    let big = format!(
        "data: {}\n\n",
        json!({"choices": [{"delta": {"content": "ab".repeat(128 * 1024)}}]})
    );
    rig.provider.set_chat_script(ChatScript {
        chunks: Some(vec![big; 200]),
        ..ChatScript::default()
    });
    let l = open(&rig, 1).await;
    let mut held = post(
        &rig,
        &l,
        serde_json::to_vec(&body(Some(100_000), true)).unwrap(),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(held.status(), 200);
    let _first = held.chunk().await.unwrap(); // read once, then stop reading
    tokio::time::sleep(Duration::from_millis(1500)).await;
    settled(&rig).await;
    assert_eq!(
        rig.provider.open_streams(),
        0,
        "the unread stream was cut at its deadline"
    );
    assert_eq!(rig.op.limiter_in_flight(&l.id), 0);
    drop(held);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_hangs_up_before_the_provider_answers_leaks_no_reservation() {
    let rig = Rig::new(proxy_setup(|c| c.proxy.first_byte = Duration::from_secs(5))).await;
    rig.provider.set_chat_script(ChatScript {
        first_byte_delay: Duration::from_millis(800),
        ..ChatScript::default()
    });
    let l = open(&rig, 1).await;
    let hung_up = post(
        &rig,
        &l,
        serde_json::to_vec(&body(Some(100), false)).unwrap(),
    )
    .timeout(Duration::from_millis(150))
    .send()
    .await;
    assert!(hung_up.is_err());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        rig.status().await["daily_budget"]["used"],
        "0",
        "nothing stays reserved"
    );
    assert_eq!(rig.op.limiter_in_flight(&l.id), 0);
    // The provider may have generated something: the worst case counts as spent, not zero.
    assert!(rig.op.spent_estimate(&l.id) > 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn retirement_aborts_a_request_the_provider_has_not_answered_yet() {
    let rig = Rig::new(proxy_setup(|c| {
        c.proxy.first_byte = Duration::from_secs(20)
    }))
    .await;
    rig.provider.set_chat_script(ChatScript {
        first_byte_delay: Duration::from_secs(10),
        ..ChatScript::default()
    });
    let l = open(&rig, 1).await;
    let pending = {
        let request = post(
            &rig,
            &l,
            serde_json::to_vec(&body(Some(10), false)).unwrap(),
        );
        tokio::spawn(async move { request.send().await.unwrap().status().as_u16() })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    rig.http
        .post(format!("{}/v2/openrouter/leases/{}", rig.url, l.id))
        .header("x-nymira-lease-token", &l.token)
        .send()
        .await
        .unwrap();
    rig.clock.advance_secs(120);
    rig.op.tick().await;
    assert_eq!(
        pending.await.unwrap(),
        502,
        "aborted, not left running on a deactivated key"
    );
    assert!(rig.provider.keys()[&l.id].revoked);
    assert_eq!(rig.status().await["daily_budget"]["used"], "0");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_evidence_fetch_carries_a_fresh_nonce_and_the_chats_signing_algorithm() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    rig.provider.set_evidence(Some(json!({"signature": "s"})));
    let l = open(&rig, 1).await;
    assert_eq!(chat(&rig, &l, &body(Some(10), false)).await.0, 200);
    rig.http
        .post(format!("{}/v2/openrouter/leases/{}", rig.url, l.id))
        .header("x-nymira-lease-token", &l.token)
        .send()
        .await
        .unwrap();
    rig.op.tick().await;
    let queries = rig.provider.evidence_queries();
    assert_eq!(queries.len(), 1);
    assert!(valid_nonce(&queries[0].nonce));
    assert_eq!(queries[0].signing_algo, "ed25519");
}

// ---------------------------------------------------------------- provider cost against the estimate

async fn chat_and_retire(rig: &Rig, n: u64, provider_nano: u128) -> Lease {
    let l = open(rig, n).await;
    assert_eq!(chat(rig, &l, &body(Some(10), false)).await.0, 200);
    settled(rig).await;
    rig.http
        .post(format!("{}/v2/openrouter/leases/{}", rig.url, l.id))
        .header("x-nymira-lease-token", &l.token)
        .send()
        .await
        .unwrap();
    rig.provider.set_usage(&l.id, provider_nano);
    for _ in 0..3 {
        rig.op.tick().await;
        rig.clock.advance_secs(11);
    }
    l
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lease_billed_what_the_pinned_prices_say_changes_nothing() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    // 10 x 80 + 20 x 500 = 10_800 nano-USD, estimated as 11 micro-USD.
    chat_and_retire(&rig, 1, 10_800).await;
    assert_eq!(rig.op.minting_stop(), None);
    assert!(rig.sink.sent().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lease_billed_more_than_the_estimate_stops_minting_and_raises_an_alarm() {
    let rig = Rig::new(proxy_setup(|_| {})).await;
    chat_and_retire(&rig, 1, 40_000).await;
    assert_eq!(rig.op.minting_stop(), Some("cost"));
    let sent = rig.sink.sent();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].key, "lease_cost_mismatch");
    let (s, a, _) = rig.lease(&rig.fresh(2)).await;
    assert_eq!(
        (s, a["error"]["code"].as_str()),
        (429, Some("operator_capacity"))
    );
}

// ---------------------------------------------------------------- admission and reserved cost

#[tokio::test(flavor = "multi_thread")]
async fn a_request_in_flight_holds_back_the_next_lease_until_it_ends() {
    // A budget under one request's worst case (2_049 micro-USD for an uncapped stream).
    let rig = Rig::new(proxy_setup(|c| c.budget.daily_limit = 2_000)).await;
    rig.provider.set_chat_script(ChatScript {
        stream_events: 40,
        chunk_delay: Duration::from_millis(50),
        ..ChatScript::default()
    });
    let l = open(&rig, 1).await;
    let held = post(&rig, &l, serde_json::to_vec(&body(None, true)).unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(held.status(), 200);
    assert_eq!(rig.status().await["daily_budget"]["used"], "2049");

    let (s, v, _) = rig.lease(&rig.fresh(2)).await;
    json_error(s, &v, 429, "operator_capacity");

    // The request ends: its reservation falls to the estimate and the next lease is admitted.
    drop(held);
    settled(&rig).await;
    let (s, v, _) = rig.lease(&rig.fresh(2)).await;
    assert_eq!(s, 201, "{v}");
}
