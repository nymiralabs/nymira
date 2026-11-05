//! The NRAS relay: two routes to fixed NVIDIA URLs, strictly allow-listed. A stand-in NVIDIA
//! host records what reaches it.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use common::*;
use nymira_operator::nras_relay::{
    ALARM_KEY, EVIDENCE_BODY_LIMIT, JWKS_CACHE_SECS, MAX_IN_FLIGHT, RELAYS_PER_MINUTE,
};
use serde_json::json;
use tokio::sync::Notify;

#[derive(Default)]
struct Nvidia {
    evidence: Mutex<Vec<(Option<String>, Vec<u8>)>>,
    jwks_calls: AtomicUsize,
    status: Mutex<u16>,
    /// Holds every evidence answer until opened.
    gate: Mutex<Option<Arc<Notify>>>,
}

const ANSWER: &str = r#"[["JWT","a.b.c"],{"x":1}]"#;
const KEYS: &str = r#"{"keys":[{"kid":"k"}]}"#;

async fn nvidia() -> (Arc<Nvidia>, String) {
    async fn evidence(
        State(n): State<Arc<Nvidia>>,
        headers: HeaderMap,
        body: Bytes,
    ) -> (StatusCode, &'static str) {
        let ct = headers
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_owned());
        n.evidence.lock().unwrap().push((ct, body.to_vec()));
        let gate = n.gate.lock().unwrap().clone();
        if let Some(g) = gate {
            g.notified().await;
        }
        let status = *n.status.lock().unwrap();
        (StatusCode::from_u16(status).unwrap(), ANSWER)
    }
    async fn jwks(State(n): State<Arc<Nvidia>>) -> &'static str {
        n.jwks_calls.fetch_add(1, Ordering::SeqCst);
        KEYS
    }
    let host = Arc::new(Nvidia {
        status: Mutex::new(200),
        ..Nvidia::default()
    });
    let app = Router::new()
        .route("/v3/attest/gpu", post(evidence))
        .route("/.well-known/jwks.json", get(jwks))
        .with_state(host.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (host, url)
}

async fn rig_for(url: &str) -> Rig {
    let url = url.to_owned();
    Rig::new(Setup {
        tune: Box::new(move |cfg| {
            cfg.nras_relay.evidence_url = format!("{url}/v3/attest/gpu");
            cfg.nras_relay.jwks_url = format!("{url}/.well-known/jwks.json");
        }),
        ..Setup::default()
    })
    .await
}

fn payload() -> String {
    json!({"nonce": "11".repeat(32), "evidence_list": []}).to_string()
}

async fn post_evidence(rig: &Rig, content_type: Option<&str>, body: Vec<u8>) -> reqwest::Response {
    let mut r = rig.http.post(format!("{}/v1/nras/attest", rig.url));
    if let Some(ct) = content_type {
        r = r.header("content-type", ct);
    }
    r.body(body).send().await.unwrap()
}

#[tokio::test]
async fn the_evidence_route_forwards_the_body_and_returns_the_answer_unchanged() {
    let (host, url) = nvidia().await;
    let rig = rig_for(&url).await;
    let sent = payload();
    let r = post_evidence(&rig, Some("application/json"), sent.clone().into_bytes()).await;
    assert_eq!(r.status(), 200);
    assert_eq!(r.text().await.unwrap(), ANSWER);
    let seen = host.evidence.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0.as_deref(), Some("application/json"));
    assert_eq!(seen[0].1, sent.as_bytes());
}

#[tokio::test]
async fn an_nvidia_error_status_and_body_pass_through_and_raise_the_alarm() {
    let (host, url) = nvidia().await;
    let rig = rig_for(&url).await;
    *host.status.lock().unwrap() = 403;
    let r = post_evidence(&rig, Some("application/json"), payload().into_bytes()).await;
    assert_eq!(r.status(), 403);
    assert_eq!(r.text().await.unwrap(), ANSWER);
    assert!(rig.sink.sent().iter().any(|a| a.key == ALARM_KEY));

    // A 400 for a bad payload is the caller's, not an outage: no alarm, same status.
    let (host, url) = nvidia().await;
    let rig = rig_for(&url).await;
    *host.status.lock().unwrap() = 400;
    let r = post_evidence(&rig, Some("application/json"), payload().into_bytes()).await;
    assert_eq!(r.status(), 400);
    assert!(rig.sink.sent().iter().all(|a| a.key != ALARM_KEY));
}

#[tokio::test]
async fn an_unreachable_nvidia_raises_the_alarm_and_a_later_answer_clears_it() {
    let (host, url) = nvidia().await;
    let down = "http://127.0.0.1:1";
    let rig = Rig::new(Setup {
        tune: Box::new(move |cfg| {
            cfg.nras_relay.evidence_url = format!("{down}/v3/attest/gpu");
            cfg.nras_relay.jwks_url = format!("{down}/.well-known/jwks.json");
        }),
        ..Setup::default()
    })
    .await;
    let r = post_evidence(&rig, Some("application/json"), payload().into_bytes()).await;
    assert_eq!(r.status(), 502);
    let r = rig
        .http
        .get(format!("{}/v1/nras/jwks", rig.url))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    let alarms: Vec<_> = rig
        .sink
        .sent()
        .into_iter()
        .filter(|a| a.key == ALARM_KEY)
        .collect();
    assert_eq!(alarms.len(), 1, "one alarm inside the repeat window");
    assert!(!alarms[0].body.contains(&"11".repeat(32)));
    assert!(rig.op.alarms().active(ALARM_KEY));
    let _ = (host, url);

    // Reachable again: a 200 clears it.
    let (_, up) = nvidia().await;
    let rig2 = rig_for(&up).await;
    post_evidence(&rig2, Some("application/json"), payload().into_bytes()).await;
    assert!(!rig2.op.alarms().active(ALARM_KEY));
}

#[tokio::test]
async fn the_jwks_route_forwards_and_caches_for_a_short_time() {
    let (host, url) = nvidia().await;
    let rig = rig_for(&url).await;
    let get_jwks = || async {
        let r = rig
            .http
            .get(format!("{}/v1/nras/jwks", rig.url))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        r.text().await.unwrap()
    };
    assert_eq!(get_jwks().await, KEYS);
    assert_eq!(get_jwks().await, KEYS);
    assert_eq!(host.jwks_calls.load(Ordering::SeqCst), 1, "cached");
    rig.clock.advance_secs(JWKS_CACHE_SECS + 1);
    get_jwks().await;
    assert_eq!(host.jwks_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn other_methods_paths_and_targets_are_refused_and_nothing_reaches_nvidia() {
    let (host, url) = nvidia().await;
    let rig = rig_for(&url).await;
    let base = &rig.url;
    let status = |r: reqwest::Response| r.status().as_u16();
    // Wrong methods on the two routes.
    assert_eq!(
        status(
            rig.http
                .get(format!("{base}/v1/nras/attest"))
                .send()
                .await
                .unwrap()
        ),
        405
    );
    assert_eq!(
        status(
            rig.http
                .put(format!("{base}/v1/nras/attest"))
                .body("{}")
                .send()
                .await
                .unwrap()
        ),
        405
    );
    assert_eq!(
        status(
            rig.http
                .post(format!("{base}/v1/nras/jwks"))
                .body("{}")
                .send()
                .await
                .unwrap()
        ),
        405
    );
    assert_eq!(
        status(
            rig.http
                .delete(format!("{base}/v1/nras/jwks"))
                .send()
                .await
                .unwrap()
        ),
        405
    );
    // Other paths: nothing is forwarded.
    for path in [
        "/v1/nras",
        "/v1/nras/",
        "/v1/nras/attest/extra",
        "/v1/nras/other",
        "/v1/nras/attest/../jwks",
        "/v3/attest/gpu",
        "/.well-known/jwks.json",
    ] {
        let r = rig
            .http
            .post(format!("{base}{path}"))
            .header("content-type", "application/json")
            .body("{}")
            .send()
            .await
            .unwrap();
        assert!(
            r.status().as_u16() == 404 || r.status().as_u16() == 405,
            "{path}: {}",
            r.status()
        );
    }
    // The target is never the caller's choice.
    let r = rig
        .http
        .post(format!("{base}/v1/nras/attest?url=http://127.0.0.1:1/evil"))
        .header("content-type", "application/json")
        .header("host", "127.0.0.1:1")
        .header("x-forwarded-host", "127.0.0.1:1")
        .body(payload())
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        host.evidence.lock().unwrap().len(),
        1,
        "it went to the configured host only"
    );
}

#[tokio::test]
async fn a_wrong_content_type_a_body_that_is_not_json_and_an_oversize_body_are_refused() {
    let (host, url) = nvidia().await;
    let rig = rig_for(&url).await;
    for ct in [
        None,
        Some("text/plain"),
        Some("application/x-www-form-urlencoded"),
        Some("application/jsonx"),
    ] {
        let r = post_evidence(&rig, ct, payload().into_bytes()).await;
        assert_eq!(r.status(), 400, "{ct:?}");
    }
    for body in ["", "not json", "[1,2]", "42", "null"] {
        let r = post_evidence(&rig, Some("application/json"), body.as_bytes().to_vec()).await;
        assert_eq!(r.status(), 400, "{body:?}");
    }
    // One byte over the cap: refused as too large, and a JSON content type with a charset is fine.
    let big = format!("{{\"pad\":\"{}\"}}", "a".repeat(EVIDENCE_BODY_LIMIT));
    let r = post_evidence(&rig, Some("application/json"), big.into_bytes()).await;
    assert_eq!(r.status(), 413);
    let r = post_evidence(
        &rig,
        Some("application/json; charset=utf-8"),
        payload().into_bytes(),
    )
    .await;
    assert_eq!(r.status(), 200);
    assert_eq!(
        host.evidence.lock().unwrap().len(),
        1,
        "only the good call got through"
    );
}

#[tokio::test]
async fn the_rate_limit_and_the_in_flight_limit_apply() {
    // Per minute, across callers.
    let (host, url) = nvidia().await;
    let rig = rig_for(&url).await;
    for _ in 0..RELAYS_PER_MINUTE {
        let r = post_evidence(&rig, Some("application/json"), payload().into_bytes()).await;
        assert_eq!(r.status(), 200);
    }
    let r = post_evidence(&rig, Some("application/json"), payload().into_bytes()).await;
    assert_eq!(r.status(), 429);
    assert!(r.headers().contains_key("retry-after"));
    assert_eq!(host.evidence.lock().unwrap().len(), RELAYS_PER_MINUTE);
    rig.clock.advance_secs(61);
    let r = post_evidence(&rig, Some("application/json"), payload().into_bytes()).await;
    assert_eq!(r.status(), 200, "the minute has passed");

    // At once: calls NVIDIA has not answered.
    let (host, url) = nvidia().await;
    let gate = Arc::new(Notify::new());
    *host.gate.lock().unwrap() = Some(gate.clone());
    let rig = Arc::new(rig_for(&url).await);
    let mut held = Vec::new();
    for _ in 0..MAX_IN_FLIGHT {
        let rig = rig.clone();
        held.push(tokio::spawn(async move {
            post_evidence(&rig, Some("application/json"), payload().into_bytes())
                .await
                .status()
        }));
    }
    while host.evidence.lock().unwrap().len() < MAX_IN_FLIGHT {
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let r = post_evidence(&rig, Some("application/json"), payload().into_bytes()).await;
    assert_eq!(r.status(), 429);
    assert!(r.text().await.unwrap().contains("operator_capacity"));
    gate.notify_waiters();
    for h in held {
        assert_eq!(h.await.unwrap(), 200);
    }
}

#[tokio::test]
async fn nothing_of_the_payload_is_kept() {
    let (_, url) = nvidia().await;
    let rig = rig_for(&url).await;
    let marker = "c0ffeec0ffeec0ffeec0ffee";
    let body = json!({"nonce": "11".repeat(32), "evidence_list": [marker]}).to_string();
    let r = post_evidence(&rig, Some("application/json"), body.into_bytes()).await;
    assert_eq!(r.status(), 200);
    let dump = rig
        .op
        .store()
        .run(|c| {
            let tables: Vec<String> = c
                .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            let mut out = String::new();
            for table in tables {
                let mut stmt = c.prepare(&format!("SELECT * FROM \"{table}\""))?;
                let columns = stmt.column_count();
                let mut rows = stmt.query([])?;
                while let Some(row) = rows.next()? {
                    for i in 0..columns {
                        use rusqlite::types::ValueRef;
                        match row.get_ref(i)? {
                            ValueRef::Text(t) => out.push_str(&String::from_utf8_lossy(t)),
                            ValueRef::Blob(b) => {
                                out.push_str(&hex::encode(b));
                                out.push_str(&String::from_utf8_lossy(b));
                            }
                            ValueRef::Integer(n) => out.push_str(&n.to_string()),
                            _ => {}
                        }
                        out.push('\n');
                    }
                }
            }
            Ok(out)
        })
        .await
        .unwrap();
    assert!(!dump.contains(marker), "the payload reached the database");
    assert!(rig.sink.sent().iter().all(|a| !a.body.contains(marker)));
}
