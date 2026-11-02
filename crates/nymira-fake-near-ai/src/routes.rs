//! HTTP routes: the provider API under `/v1` and the control API under `/control`.

use crate::crypto::{
    self, encrypt_hex, gateway_signing_key, model_e2ee_public_hex, model_signing_key, parse_key32,
    rogue_signing_key, verifying_hex,
};
use crate::policy::{build_report, sha256_hex, SignatureKind, TestPolicy, SIGNING_ALGO};
use crate::state::*;
use axum::{
    body::{Body, Bytes},
    extract::{Path, Query, Request, State},
    http::{header, HeaderMap, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, patch, post},
    Json, Router,
};
use ed25519_dalek::Signer;
use serde_json::{json, Value};
use std::{collections::HashMap, convert::Infallible, time::Duration};

pub fn router(h: Handle) -> Router {
    let api = Router::new()
        .route("/v1/model/list", get(model_list))
        .route("/v1/check_api_key", post(check_api_key))
        .route("/v1/chat/completions", post(chat))
        .route("/v1/signature/{chat_id}", get(signature))
        .route("/v1/attestation/report", get(attestation))
        .route(
            "/v1/workspaces/{ws}/api-keys",
            post(create_key).get(list_keys),
        )
        .route(
            "/v1/workspaces/{ws}/api-keys/{id}",
            patch(patch_key).delete(delete_key),
        )
        .route("/v1/organizations/{org}/usage/history", get(usage_history))
        .route("/v1/organizations/{org}/usage/balance", get(balance))
        .route("/v1/users/me/access-tokens", post(access_token))
        .layer(middleware::from_fn_with_state(h.clone(), inject));
    let control = Router::new()
        .route(
            "/control/switches",
            get(control_switches).post(control_patch),
        )
        .route("/control/keys", get(control_keys))
        .route("/control/chats", get(control_chats))
        .route("/control/reset", post(control_reset))
        .route("/control/policy", get(control_policy));
    api.merge(control).with_state(h)
}

pub struct ApiErr(StatusCode, &'static str, String);

fn err(status: u16, code: &'static str, msg: impl Into<String>) -> ApiErr {
    ApiErr(
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        code,
        msg.into(),
    )
}

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        (
            self.0,
            Json(json!({"error": {"code": self.1, "message": self.2}})),
        )
            .into_response()
    }
}

type R<T = Json<Value>> = Result<T, ApiErr>;

/// Applies the latency and failure switches to every `/v1/` request.
async fn inject(State(h): State<Handle>, req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    let (latency, fail) = {
        let mut g = h.lock();
        let s = &mut g.switches;
        let fail = (s.fail_next > 0 && path.starts_with(&s.fail_path_prefix)).then(|| {
            s.fail_next -= 1;
            s.fail_status
        });
        (s.latency_ms, fail)
    };
    if latency > 0 {
        tokio::time::sleep(Duration::from_millis(latency)).await;
    }
    if let Some(status) = fail {
        let code = if status == 429 {
            "rate_limit_exceeded"
        } else {
            "injected_failure"
        };
        return err(status, code, "failure injected by the fake").into_response();
    }
    next.run(req).await
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

fn require_session(headers: &HeaderMap) -> R<()> {
    (bearer(headers) == Some(SESSION_TOKEN))
        .then_some(())
        .ok_or_else(|| err(401, "unauthorized", "session token required"))
}

/// Index of the active key the request carries.
fn key_auth(g: &Inner, headers: &HeaderMap) -> R<usize> {
    let token = bearer(headers).ok_or_else(|| err(401, "unauthorized", "missing API key"))?;
    g.keys
        .iter()
        .position(|k| k.secret == token && k.is_active && !k.deleted)
        .ok_or_else(|| err(401, "unauthorized", "unknown or inactive API key"))
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

// ---- models -------------------------------------------------------------

/// Public, no auth. Shape: NEAR AI's `GET /v1/model/list` (`metadata.providerType` and friends).
async fn model_list(State(h): State<Handle>) -> Json<Value> {
    let s = h.switches();
    let price = |n: u64| json!({"amount": n, "scale": 9, "currency": "USD"});
    let models: Vec<Value> = MODELS
        .iter()
        .map(|m| {
            json!({
                "modelId": m.id,
                "inputCostPerToken": price(s.input_price_nano_per_token),
                "outputCostPerToken": price(s.output_price_nano_per_token),
                "metadata": {
                    "verifiable": m.verifiable,
                    "providerType": m.provider_type,
                    "attestationSupported": m.attestation_supported,
                },
            })
        })
        .collect();
    Json(json!({"models": models}))
}

/// 200 when the key is known and active, 401 otherwise (the real one also checks credit).
async fn check_api_key(State(h): State<Handle>, headers: HeaderMap) -> R<StatusCode> {
    let g = h.lock();
    key_auth(&g, &headers)?;
    Ok(StatusCode::OK)
}

// ---- key management -----------------------------------------------------

fn parse_spend_limit(v: &Value) -> R<Option<SpendLimit>> {
    if v.is_null() {
        return Ok(None);
    }
    serde_json::from_value(v.clone())
        .map(Some)
        .map_err(|e| err(400, "malformed_request", format!("spendLimit: {e}")))
}

async fn create_key(
    State(h): State<Handle>,
    headers: HeaderMap,
    Path(_ws): Path<String>,
    Json(body): Json<Value>,
) -> R {
    require_session(&headers)?;
    let name = body["name"]
        .as_str()
        .ok_or_else(|| err(400, "malformed_request", "name is required"))?
        .to_string();
    let spend_limit = parse_spend_limit(&body["spendLimit"])?;
    let mut g = h.lock();
    if g.keys.iter().any(|k| k.name == name && !k.deleted) {
        return Err(err(409, "conflict", "a key with this name exists"));
    }
    let mut random = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut random);
    let key = KeyRecord {
        id: format!("key-{}", g.keys.len() + 1),
        name,
        secret: format!("sk-fake-{}", hex::encode(random)),
        spend_limit,
        expires_at: body.get("expires_at").filter(|v| !v.is_null()).cloned(),
        is_active: true,
        deleted: false,
        spent_nano: 0,
    };
    let out = key.to_json(true);
    g.keys.push(key);
    Ok(Json(out))
}

async fn list_keys(
    State(h): State<Handle>,
    headers: HeaderMap,
    Path(_ws): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> R {
    require_session(&headers)?;
    let limit = q
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(20usize);
    let offset = q
        .get("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0usize);
    let g = h.lock();
    let live: Vec<&KeyRecord> = g.keys.iter().filter(|k| !k.deleted).collect();
    let page: Vec<Value> = live
        .iter()
        .skip(offset)
        .take(limit)
        .map(|k| k.to_json(false))
        .collect();
    Ok(Json(
        json!({"api_keys": page, "total": live.len(), "limit": limit, "offset": offset}),
    ))
}

async fn patch_key(
    State(h): State<Handle>,
    headers: HeaderMap,
    Path((_ws, id)): Path<(String, String)>,
    Json(body): Json<Value>,
) -> R {
    require_session(&headers)?;
    let mut g = h.lock();
    let k = g
        .keys
        .iter_mut()
        .find(|k| k.id == id && !k.deleted)
        .ok_or_else(|| err(404, "not_found", "no such key"))?;
    if let Some(a) = body.get("is_active") {
        k.is_active = a
            .as_bool()
            .ok_or_else(|| err(400, "malformed_request", "is_active must be a boolean"))?;
    }
    if let Some(e) = body.get("expires_at") {
        k.expires_at = (!e.is_null()).then(|| e.clone());
    }
    if let Some(s) = body.get("spendLimit") {
        k.spend_limit = parse_spend_limit(s)?;
    }
    Ok(Json(k.to_json(false)))
}

/// Soft delete, as in the real API: the key stops working and leaves the listing.
async fn delete_key(
    State(h): State<Handle>,
    headers: HeaderMap,
    Path((_ws, id)): Path<(String, String)>,
) -> R<StatusCode> {
    require_session(&headers)?;
    let mut g = h.lock();
    let k = g
        .keys
        .iter_mut()
        .find(|k| k.id == id && !k.deleted)
        .ok_or_else(|| err(404, "not_found", "no such key"))?;
    k.deleted = true;
    k.is_active = false;
    Ok(StatusCode::NO_CONTENT)
}

// ---- usage and credit ---------------------------------------------------

/// Real shape: `{data: [rows], total, limit, offset}`, rows with `total_cost` in nano-USD.
/// The documented `usage/export` and `usage/summary` answered 404 on the live gateway
/// (2026-11-02), so the fake does not serve them.
async fn usage_history(
    State(h): State<Handle>,
    headers: HeaderMap,
    Path(_org): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> R {
    require_session(&headers)?;
    let limit = q
        .get("limit")
        .and_then(|v| v.parse().ok())
        .unwrap_or(100usize);
    let offset = q
        .get("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0usize);
    let g = h.lock();
    let rows: Vec<_> = g
        .usage
        .iter()
        .filter(|r| q.get("api_key_id").is_none_or(|id| *id == r.api_key_id))
        .collect();
    let page: Vec<_> = rows.iter().skip(offset).take(limit).collect();
    Ok(Json(
        json!({"data": page, "total": rows.len(), "limit": limit, "offset": offset}),
    ))
}

/// The real balance response: `remaining` is the credit left, in nano-USD.
async fn balance(State(h): State<Handle>, headers: HeaderMap, Path(org): Path<String>) -> R {
    require_session(&headers)?;
    let g = h.lock();
    let spent: u64 = g.usage.iter().map(|r| r.cost_nano_usd).sum();
    let tokens: u64 = g
        .usage
        .iter()
        .map(|r| r.input_tokens + r.output_tokens)
        .sum();
    Ok(Json(json!({
        "organization_id": org,
        "remaining": g.switches.balance_nano,
        "remaining_display": null,
        "total_spent": spent,
        "total_spent_display": "",
        "total_requests": g.usage.len(),
        "total_tokens": tokens,
        "updated_at": "1970-01-01T00:00:00Z",
        "credit_limits": [],
    })))
}

/// Exchanges a refresh token for a session token and, unless switched off, a replacement
/// refresh token. The old one stops working.
async fn access_token(State(h): State<Handle>, headers: HeaderMap) -> R {
    let mut g = h.lock();
    let current = format!("rt-fake-{}", g.refresh_count);
    if bearer(&headers) != Some(current.as_str()) {
        return Err(err(401, "unauthorized", "refresh token not valid"));
    }
    let agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok());
    if g.switches
        .exchange_user_agent
        .as_deref()
        .is_some_and(|want| agent != Some(want))
    {
        return Err(err(401, "unauthorized", "refresh token not valid"));
    }
    if g.switches.rotate_refresh_tokens {
        g.refresh_count += 1;
    }
    let ttl = g.switches.refresh_ttl_secs;
    Ok(Json(json!({
        "access_token": SESSION_TOKEN,
        "refresh_token": format!("rt-fake-{}", g.refresh_count),
        "refresh_token_expiration": rfc3339(now() + ttl),
    })))
}

/// `YYYY-MM-DDTHH:MM:SSZ` for a Unix time (civil-from-days, proleptic Gregorian).
pub fn rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

// ---- chat ---------------------------------------------------------------

fn tokens(s: &str) -> u64 {
    (s.chars().count().div_ceil(4)).max(1) as u64
}

fn sse(v: &Value) -> Bytes {
    Bytes::from(format!("data: {v}\n\n"))
}

async fn chat(State(h): State<Handle>, headers: HeaderMap, body: Bytes) -> R<Response> {
    let hdr = |n: &str| headers.get(n).and_then(|v| v.to_str().ok());
    // Auth, spend and credit checks first, like the real gateway's `check_api_key`.
    let (key_id, chat_id, s) = {
        let mut g = h.lock();
        let i = key_auth(&g, &headers)?;
        let k = &g.keys[i];
        if k.spend_limit
            .as_ref()
            .is_some_and(|l| k.spent_nano >= l.amount)
            || g.switches.balance_nano <= 0
        {
            return Err(err(
                402,
                "insufficient_credits",
                "insufficient credits or spend limit exceeded",
            ));
        }
        let key_id = k.id.clone();
        g.counter += 1;
        (
            key_id,
            format!("chatcmpl-{:08x}", g.counter),
            g.switches.clone(),
        )
    };
    let req: Value = serde_json::from_slice(&body)
        .map_err(|_| err(400, "malformed_request", "body is not valid JSON"))?;
    let model = req["model"]
        .as_str()
        .ok_or_else(|| err(400, "malformed_request", "model is required"))?
        .to_string();
    if !MODELS.iter().any(|m| m.id == model) {
        return Err(err(404, "model_not_found", "unknown model"));
    }
    let messages = req["messages"]
        .as_array()
        .ok_or_else(|| err(400, "malformed_request", "messages is required"))?;
    let stream = req["stream"].as_bool().unwrap_or(false);
    let include_usage = req["stream_options"]["include_usage"]
        .as_bool()
        .unwrap_or(false);
    let all_fields = hdr("x-encrypt-all-fields") == Some("true");

    // E2EE: the client sends its public key and the model's key it encrypted to.
    let client_pk = match hdr("x-client-pub-key") {
        None => None,
        Some(pk) => {
            if hdr("x-model-pub-key") != Some(model_e2ee_public_hex().as_str()) {
                return Err(err(
                    400,
                    "unsupported",
                    "X-Model-Pub-Key is not this model's attested key",
                ));
            }
            if hdr("x-signing-algo").is_some_and(|a| a != SIGNING_ALGO) {
                return Err(err(400, "unsupported", "only ed25519 is supported"));
            }
            Some(parse_key32(pk).ok_or_else(|| {
                err(
                    400,
                    "malformed_request",
                    "X-Client-Pub-Key is not 32 hex bytes",
                )
            })?)
        }
    };
    let mut prompt = String::new();
    let mut last_user = String::new();
    for m in messages {
        // Encrypted fields: `content` always, `name` too under X-Encrypt-All-Fields.
        for field in ["content", "name"] {
            let Some(text) = m[field].as_str() else {
                continue;
            };
            let plain = match (client_pk, field == "content" || all_fields) {
                (Some(_), true) => crypto::decrypt_hex(&crypto::model_e2ee_secret(), text)
                    .map_err(|e| err(400, "decryption_failed", format!("{field}: {e}")))?,
                _ => text.to_string(),
            };
            if field == "content" {
                prompt.push_str(&plain);
                if m["role"] == "user" {
                    last_user = plain;
                }
            }
        }
    }

    let reply = s
        .reply
        .clone()
        .unwrap_or_else(|| format!("fake reply to: {last_user}"));
    let usage = s.usage.unwrap_or(Usage {
        prompt_tokens: tokens(&prompt),
        completion_tokens: tokens(&reply),
    });
    let cost = usage.prompt_tokens * s.input_price_nano_per_token
        + usage.completion_tokens * s.output_price_nano_per_token;
    let usage_json = json!({
        "prompt_tokens": usage.prompt_tokens,
        "completion_tokens": usage.completion_tokens,
        "total_tokens": usage.prompt_tokens + usage.completion_tokens,
    });
    let enc = |t: &str| client_pk.map_or_else(|| t.to_string(), |pk| encrypt_hex(&pk, t));
    let created = now();
    let chunk = |delta: Value, finish: Value| {
        json!({"id": chat_id, "object": "chat.completion.chunk", "created": created, "model": model,
               "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    };

    let (response, content_type, kind) = if stream {
        let mut events = vec![sse(&chunk(
            json!({"role": "assistant", "content": ""}),
            Value::Null,
        ))];
        for piece in reply.split_inclusive(' ') {
            events.push(sse(&chunk(json!({"content": enc(piece)}), Value::Null)));
        }
        events.push(sse(&chunk(json!({}), json!("stop"))));
        if include_usage {
            events.push(sse(
                &json!({"id": chat_id, "object": "chat.completion.chunk", "created": created,
                                    "model": model, "choices": [], "usage": usage_json}),
            ));
        }
        events.push(Bytes::from_static(b"data: [DONE]\n\n"));
        (events, "text/event-stream", s.stream_signature_kind)
    } else {
        let v = json!({"id": chat_id, "object": "chat.completion", "created": created, "model": model,
                       "choices": [{"index": 0, "message": {"role": "assistant", "content": enc(&reply)}, "finish_reason": "stop"}],
                       "usage": usage_json});
        (
            vec![Bytes::from(
                serde_json::to_vec(&v).expect("json serializes"),
            )],
            "application/json",
            s.chat_signature_kind,
        )
    };
    let response_bytes: Vec<u8> = response.concat();

    {
        let mut g = h.lock();
        if let Some(k) = g.keys.iter_mut().find(|k| k.id == key_id) {
            k.spent_nano += cost;
        }
        g.switches.balance_nano -= i64::try_from(cost).unwrap_or(i64::MAX);
        g.usage.push(UsageRow {
            inference_id: chat_id.clone(),
            api_key_id: key_id.clone(),
            model: model.clone(),
            input_tokens: usage.prompt_tokens,
            output_tokens: usage.completion_tokens,
            cache_read_tokens: 0,
            cost_nano_usd: cost,
        });
        g.sigs.insert(
            chat_id.clone(),
            SigRecord {
                key_id: key_id.clone(),
                model: model.clone(),
                kind,
                request_hash: sha256_hex(&body),
                response_hash: sha256_hex(&response_bytes),
            },
        );
        g.chats.push(ChatRecord {
            chat_id,
            key_id,
            model,
            stream,
            e2ee: client_pk.is_some(),
            encrypt_all_fields: all_fields,
            no_aliasing: hdr("x-no-aliasing") == Some("true"),
            accept_encoding: hdr("accept-encoding").map(str::to_string),
            include_usage,
            body: body.to_vec(),
            response: response_bytes,
        });
    }

    let stream_body = Body::from_stream(tokio_stream::iter(
        response.into_iter().map(Ok::<_, Infallible>),
    ));
    Ok((
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, "no-store"),
        ],
        stream_body,
    )
        .into_response())
}

// ---- signature and attestation -----------------------------------------

fn check_algo(q: &HashMap<String, String>) -> R<()> {
    match q.get("signing_algo").map(String::as_str) {
        None | Some(SIGNING_ALGO) => Ok(()),
        Some(_) => Err(err(
            400,
            "unsupported_signing_algo",
            "only ed25519 is supported",
        )),
    }
}

async fn signature(
    State(h): State<Handle>,
    headers: HeaderMap,
    Path(chat_id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> R {
    check_algo(&q)?;
    let g = h.lock();
    let i = key_auth(&g, &headers)?;
    let rec = g
        .sigs
        .get(&chat_id)
        .filter(|r| r.key_id == g.keys[i].id)
        .ok_or_else(|| err(404, "not_found", "no signature for this id"))?;
    let (text, signer) = match rec.kind {
        SignatureKind::ProviderTee => (
            format!("{}:{}:{}", rec.model, rec.request_hash, rec.response_hash),
            model_signing_key(),
        ),
        SignatureKind::Gateway => (
            format!("{}:{}", rec.request_hash, rec.response_hash),
            gateway_signing_key(),
        ),
    };
    let signer = if g.switches.unbound_key {
        rogue_signing_key()
    } else {
        signer
    };
    let mut sig = signer.sign(text.as_bytes()).to_bytes();
    if g.switches.bad_signature {
        sig[0] ^= 0xff;
    }
    Ok(Json(json!({
        "text": text,
        "signature": hex::encode(sig),
        "signing_address": verifying_hex(&signer),
        "signing_algo": SIGNING_ALGO,
        "signature_kind": rec.kind.as_str(),
    })))
}

async fn attestation(
    State(h): State<Handle>,
    headers: HeaderMap,
    Query(q): Query<HashMap<String, String>>,
) -> R {
    check_algo(&q)?;
    // Public, as the live endpoint is (checked 2026-11-02): no key needed.
    let _ = &headers;
    let g = h.lock();
    let model = q.get("model").map_or(MODELS[0].id, String::as_str);
    if !MODELS.iter().any(|m| m.id == model) {
        return Err(err(404, "model_not_found", "unknown model"));
    }
    let nonce = q.get("nonce").map_or("", String::as_str);
    if nonce.len() > 128 || !nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(err(
            400,
            "malformed_request",
            "nonce must be at most 128 hex characters",
        ));
    }
    Ok(Json(build_report(
        model,
        &g.switches.measurement,
        &g.switches.tcb_status,
        nonce,
    )))
}

// ---- control ------------------------------------------------------------

async fn control_switches(State(h): State<Handle>) -> Json<Switches> {
    Json(h.switches())
}

async fn control_patch(State(h): State<Handle>, Json(p): Json<Value>) -> R<Json<Switches>> {
    h.patch(&p)
        .map(Json)
        .map_err(|e| err(400, "malformed_request", e))
}

async fn control_keys(State(h): State<Handle>) -> Json<Value> {
    Json(Value::Array(
        h.keys().iter().map(|k| k.to_json(true)).collect(),
    ))
}

async fn control_chats(State(h): State<Handle>) -> Json<Value> {
    let chats: Vec<Value> = h
        .chats()
        .iter()
        .map(|c| {
            json!({
                "chat_id": c.chat_id, "key_id": c.key_id, "model": c.model, "stream": c.stream,
                "e2ee": c.e2ee, "encrypt_all_fields": c.encrypt_all_fields, "no_aliasing": c.no_aliasing,
                "accept_encoding": c.accept_encoding, "include_usage": c.include_usage,
            })
        })
        .collect();
    Json(Value::Array(chats))
}

async fn control_reset(State(h): State<Handle>) -> StatusCode {
    h.reset();
    StatusCode::NO_CONTENT
}

async fn control_policy() -> Json<TestPolicy> {
    Json(TestPolicy::fake())
}
