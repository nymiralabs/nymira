//! The chat proxy: strict validation of an end-to-end encrypted chat request, concurrency
//! limits, a per-lease spend ledger, deadlines, and the relay of the provider's answer.
//!
//! The operator reads only what end-to-end encryption leaves in the clear: the model, sampling
//! settings, message count and size. It forwards the bytes it validated, never the client's
//! original bytes, with the encryption headers and nothing else.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::http::HeaderMap;
use bytes::Bytes;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::clock::Clock;
use crate::error::{ApiError, ApiResult, Code};
use crate::json::{only_keys, parse_strict};
use crate::operator::{Operator, LEASE_MAX_SECS};
use crate::provider::{ChatRequest, ProviderError};

/// Body limit of the chat endpoint.
pub const CHAT_BODY_LIMIT: usize = 4 * 1024 * 1024;
/// Bytes of envelope (nonce and tag) every ciphertext carries; the rest is payload.
const ENVELOPE_BYTES: u64 = 72;
const TOP_KEYS: [&str; 15] = [
    "model",
    "messages",
    "stream",
    "stream_options",
    "max_tokens",
    "max_completion_tokens",
    "n",
    "temperature",
    "top_p",
    "frequency_penalty",
    "presence_penalty",
    "stop",
    "seed",
    "tools",
    "tool_choice",
];
const MESSAGE_KEYS: [&str; 5] = ["role", "content", "name", "tool_calls", "tool_call_id"];

/// A pinned model with its prices (nano-USD per token) and the output cap used when a request
/// names none.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelSpec {
    pub id: String,
    pub input_nano_per_token: u64,
    pub output_nano_per_token: u64,
    pub output_cap_tokens: u64,
}

#[derive(Clone, Debug)]
pub struct ProxyConfig {
    pub models: Vec<ModelSpec>,
    pub per_lease: usize,
    pub per_model: usize,
    /// Above this share of the per-model limit each lease gets one slot.
    pub hot_pct: usize,
    pub first_byte: Duration,
    pub idle: Duration,
    pub total: Duration,
}

impl Default for ProxyConfig {
    fn default() -> Self {
        let model = |id: &str, input: u64, output: u64| ModelSpec {
            id: id.into(),
            input_nano_per_token: input,
            output_nano_per_token: output,
            output_cap_tokens: 4096,
        };
        Self {
            // Canonical ids and prices (nano-USD per token) as the live list gave them on 2026-11-02.
            models: vec![
                model("z-ai/glm-5.3-flash", 80, 500),
                model("Qwen/Qwen3.6-35B-A3B-FP8", 170, 1100),
                model("Qwen/Qwen3.8-27B", 40, 1350),
                model("Qwen/Qwen3-VL-30B-A3B-Instruct", 150, 550),
            ],
            per_lease: 2,
            per_model: 64,
            hot_pct: 75,
            first_byte: Duration::from_secs(30),
            idle: Duration::from_secs(15),
            total: Duration::from_secs(120),
        }
    }
}

fn malformed(message: impl Into<String>) -> ApiError {
    ApiError::new(Code::MalformedRequest, message)
}

fn unsupported(message: impl Into<String>) -> ApiError {
    ApiError::new(Code::UnsupportedRequest, message)
}

/// A chat request that passed validation.
#[derive(Debug)]
pub struct Parsed {
    pub model: String,
    pub stream: bool,
    /// `max_tokens` or `max_completion_tokens`, whichever was sent.
    pub max_output: Option<u64>,
    /// Upper bound on input tokens from the ciphertext lengths: one token per payload byte.
    pub input_tokens: u64,
    /// The bytes to forward: the validated value, serialised, with usage reporting switched on.
    pub body: Vec<u8>,
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.len().is_multiple_of(2) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Input tokens one string field can cost: a hex ciphertext minus its envelope, else one per byte.
fn field_tokens(s: &str) -> u64 {
    if is_hex(s) {
        ((s.len() / 2) as u64).saturating_sub(ENVELOPE_BYTES)
    } else {
        s.len() as u64
    }
}

fn positive_int(v: &Value, name: &str) -> ApiResult<u64> {
    v.as_u64()
        .filter(|n| *n > 0)
        .ok_or_else(|| malformed(format!("{name} must be a positive integer")))
}

/// Strict validation of a chat body against the pinned models. `blocked` are pinned models whose
/// live price differs from the pin.
pub fn parse_chat(body: &[u8], cfg: &ProxyConfig, blocked: &HashSet<String>) -> ApiResult<Parsed> {
    let mut value = parse_strict(body).map_err(malformed)?;
    if !only_keys(&value, &TOP_KEYS) {
        return Err(malformed("unknown or non-object body"));
    }
    let model = value["model"]
        .as_str()
        .ok_or_else(|| malformed("model must be a string"))?
        .to_string();
    // Exact match: no case folding, no trimming, no aliases.
    if !cfg.models.iter().any(|m| m.id == model) || blocked.contains(&model) {
        return Err(unsupported("model is not served"));
    }
    let mut input_tokens = 0u64;
    let messages = value["messages"]
        .as_array()
        .filter(|m| !m.is_empty() && m.len() <= 1024);
    for message in messages.ok_or_else(|| malformed("messages must be 1 to 1024 objects"))? {
        if !only_keys(message, &MESSAGE_KEYS) || !message["role"].is_string() {
            return Err(malformed("a message needs a role and only known fields"));
        }
        // Every plaintext field the provider will read counts as input, one token per byte.
        for field in ["role", "name", "tool_call_id"] {
            input_tokens += message[field].as_str().map_or(0, |s| s.len() as u64);
        }
        match &message["content"] {
            Value::String(s) => input_tokens += field_tokens(s),
            Value::Null => {}
            _ => return Err(unsupported("message content must be one string")),
        }
        if let Some(calls) = message.get("tool_calls") {
            input_tokens += calls.to_string().len() as u64;
        }
    }
    let stream = match value.get("stream") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(malformed("stream must be a boolean")),
    };
    if let Some(options) = value.get("stream_options") {
        if !only_keys(options, &["include_usage"]) {
            return Err(malformed("stream_options carries only include_usage"));
        }
    }
    if let Some(n) = value.get("n") {
        if n.as_u64() != Some(1) {
            return Err(unsupported("n must be 1"));
        }
    }
    let max_tokens = value
        .get("max_tokens")
        .map(|v| positive_int(v, "max_tokens"))
        .transpose()?;
    let max_completion = value
        .get("max_completion_tokens")
        .map(|v| positive_int(v, "max_completion_tokens"))
        .transpose()?;
    if max_tokens.is_some() && max_completion.is_some() {
        return Err(ApiError::new(
            Code::MaxTokensInvalid,
            "send max_tokens or max_completion_tokens, not both",
        ));
    }
    for key in [
        "temperature",
        "top_p",
        "frequency_penalty",
        "presence_penalty",
    ] {
        if value
            .get(key)
            .is_some_and(|v| !v.as_f64().is_some_and(f64::is_finite))
        {
            return Err(malformed(format!("{key} must be a number")));
        }
    }
    if value.get("seed").is_some_and(|v| v.as_i64().is_none()) {
        return Err(malformed("seed must be an integer"));
    }
    match value.get("stop") {
        None => {}
        Some(Value::String(s)) => input_tokens += s.len() as u64,
        Some(Value::Array(a)) if a.len() <= 4 && a.iter().all(Value::is_string) => {
            input_tokens += a
                .iter()
                .map(|s| s.as_str().map_or(0, str::len) as u64)
                .sum::<u64>();
        }
        Some(_) => return Err(malformed("stop must be a string or up to four strings")),
    }
    if let Some(tools) = value.get("tools") {
        let tools = tools
            .as_array()
            .filter(|t| t.len() <= 32)
            .ok_or_else(|| malformed("tools must be up to 32 objects"))?;
        for tool in tools {
            if !only_keys(tool, &["type", "function"]) {
                return Err(malformed("a tool has a type and a function"));
            }
            if tool["type"] != "function" {
                return Err(unsupported("only function tools are served"));
            }
            let function = &tool["function"];
            if !only_keys(function, &["name", "description", "parameters"]) {
                return Err(malformed(
                    "a tool function has name, description and parameters",
                ));
            }
            // Under end-to-end encryption these are ciphertext strings; an object is readable.
            for field in ["name", "description", "parameters"] {
                match function.get(field) {
                    None => {}
                    Some(Value::String(s)) if is_hex(s) => input_tokens += field_tokens(s),
                    Some(_) => return Err(unsupported("tool definitions must be encrypted")),
                }
            }
        }
    }
    match value.get("tool_choice") {
        None => {}
        Some(v @ (Value::String(_) | Value::Object(_))) => {
            input_tokens += v.to_string().len() as u64
        }
        Some(_) => return Err(malformed("tool_choice must be a string or an object")),
    }
    value["stream_options"] = json!({ "include_usage": true });
    Ok(Parsed {
        model,
        stream,
        max_output: max_tokens.or(max_completion),
        input_tokens,
        body: serde_json::to_vec(&value).map_err(|e| malformed(e.to_string()))?,
    })
}

/// The headers a chat request may carry upstream, validated, plus the two the operator adds.
pub fn forward_headers(headers: &HeaderMap) -> ApiResult<Vec<(String, String)>> {
    let mut out = Vec::new();
    for name in [
        "x-signing-algo",
        "x-client-pub-key",
        "x-model-pub-key",
        "x-encryption-version",
    ] {
        let value = headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| {
                unsupported(format!(
                    "a request that is not end-to-end encrypted ({name} missing)"
                ))
            })?;
        let ok = match name {
            "x-signing-algo" => matches!(value, "ed25519" | "ecdsa"),
            "x-encryption-version" => value == "2",
            _ => {
                value.len() <= 256
                    && value.bytes().all(|b| b.is_ascii_hexdigit())
                    && !value.is_empty()
            }
        };
        if !ok {
            return Err(unsupported(format!("{name} is not in the expected format")));
        }
        out.push((name.to_string(), value.to_string()));
    }
    out.push(("content-type".into(), "application/json".into()));
    out.push(("x-encrypt-all-fields".into(), "true".into()));
    out.push(("x-no-aliasing".into(), "true".into()));
    Ok(out)
}

/// A nonce for a signature or attestation fetch: 64 lowercase hex digits and nothing else.
pub fn valid_nonce(nonce: &str) -> bool {
    nonce.len() == 64
        && nonce
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// The `signing_algo` of both fetches: one of the two the gateway knows.
pub fn valid_signing_algo(algo: &str) -> bool {
    matches!(algo, "ed25519" | "ecdsa")
}

// ---------------------------------------------------------------- spend ledger

#[derive(Default)]
struct LeaseSpend {
    /// Estimated cost of finished requests, micro-USD.
    finished: u128,
    reserved: HashMap<u64, u128>,
}

/// What each lease has spent and reserved: concurrent requests cannot overspend the bound.
#[derive(Default)]
pub struct SpendBook {
    leases: HashMap<String, LeaseSpend>,
    next: u64,
}

impl SpendBook {
    /// Reserves `cost` unless it exceeds 90% of what the lease has left.
    pub fn reserve(&mut self, lease: &str, bound: u128, cost: u128) -> Option<(u64, u128)> {
        let spend = self.leases.entry(lease.to_string()).or_default();
        let used = spend.finished + spend.reserved.values().sum::<u128>();
        let remaining = bound.saturating_sub(used);
        if cost * 10 > remaining * 9 {
            return None;
        }
        self.next += 1;
        spend.reserved.insert(self.next, cost);
        Some((self.next, spend.reserved.values().sum()))
    }

    /// The reservation gives way to the estimate from the signed usage. Returns what is still
    /// reserved by the lease's other requests.
    pub fn finish(&mut self, lease: &str, id: u64, actual: u128) -> Option<u128> {
        // A lease already settled or expired is gone: nothing to record.
        let spend = self.leases.get_mut(lease)?;
        spend.reserved.remove(&id);
        spend.finished += actual;
        Some(spend.reserved.values().sum())
    }

    /// Puts back what a restart lost: the finished spend of an open lease.
    pub fn restore(&mut self, lease: &str, finished: u128) {
        self.leases.entry(lease.to_string()).or_default().finished = finished;
    }

    pub fn forget(&mut self, lease: &str) {
        self.leases.remove(lease);
    }

    pub fn finished(&self, lease: &str) -> u128 {
        self.leases.get(lease).map_or(0, |s| s.finished)
    }

    /// Estimated cost of every lease's finished requests, micro-USD: spent at the provider, not
    /// yet settled in the ledger.
    pub fn total_finished(&self) -> u128 {
        self.leases.values().map(|s| s.finished).sum()
    }

    /// Whether the book has seen the lease: after a restart it has not.
    pub fn knows(&self, lease: &str) -> bool {
        self.leases.contains_key(lease)
    }
}

// ---------------------------------------------------------------- limiter

#[derive(Default)]
struct LimitState {
    per_model: HashMap<String, usize>,
    per_lease: HashMap<String, usize>,
    /// Milliseconds each lease has held slots, finished ones.
    held_ms: HashMap<String, u64>,
    /// When each running slot started, per lease.
    running: HashMap<String, Vec<u64>>,
}

/// Concurrency limits: slots per lease, per model across leases, and the fairness rule that
/// applies when a model is nearly full.
pub struct Limiter {
    state: Arc<Mutex<LimitState>>,
}

pub struct Slot {
    state: Arc<Mutex<LimitState>>,
    clock: Arc<dyn Clock>,
    model: String,
    lease: String,
    started_ms: u64,
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = self.clock.now_ms();
        if let Some(n) = s.per_model.get_mut(&self.model) {
            *n = n.saturating_sub(1);
        }
        if let Some(n) = s.per_lease.get_mut(&self.lease) {
            *n = n.saturating_sub(1);
        }
        if let Some(starts) = s.running.get_mut(&self.lease) {
            if let Some(i) = starts.iter().position(|t| *t == self.started_ms) {
                starts.swap_remove(i);
            }
        }
        *s.held_ms.entry(self.lease.clone()).or_insert(0) += now.saturating_sub(self.started_ms);
    }
}

impl Limiter {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(LimitState::default())),
        }
    }

    pub fn in_flight(&self, model: &str) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .per_model
            .get(model)
            .copied()
            .unwrap_or(0)
    }

    pub fn lease_in_flight(&self, lease: &str) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .per_lease
            .get(lease)
            .copied()
            .unwrap_or(0)
    }

    /// `lifetime_ms` is how long the lease has existed.
    pub fn acquire(
        &self,
        cfg: &ProxyConfig,
        clock: &Arc<dyn Clock>,
        model: &str,
        lease: &str,
        lifetime_ms: u64,
    ) -> Result<Slot, ApiError> {
        let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let now = clock.now_ms();
        let in_model = s.per_model.get(model).copied().unwrap_or(0);
        let hot = in_model * 100 > cfg.per_model * cfg.hot_pct;
        let lease_limit = if hot { 1 } else { cfg.per_lease };
        if s.per_lease.get(lease).copied().unwrap_or(0) >= lease_limit {
            return Err(ApiError::new(
                Code::LeaseConcurrency,
                "too many requests in flight on this lease",
            )
            .retry_after(1));
        }
        if in_model >= cfg.per_model {
            return Err(ApiError::new(
                Code::OperatorCapacity,
                "the model is at its concurrency limit",
            )
            .retry_after(2));
        }
        if hot {
            // A lease that has held slots for over half of its life gives way first.
            let running: u64 = s
                .running
                .get(lease)
                .map_or(0, |v| v.iter().map(|t| now.saturating_sub(*t)).sum());
            let held = s.held_ms.get(lease).copied().unwrap_or(0) + running;
            if held > lifetime_ms / 2 {
                return Err(ApiError::new(
                    Code::OperatorCapacity,
                    "the model is busy and this lease has held slots long",
                )
                .retry_after(2));
            }
        }
        *s.per_model.entry(model.to_string()).or_insert(0) += 1;
        *s.per_lease.entry(lease.to_string()).or_insert(0) += 1;
        s.running.entry(lease.to_string()).or_default().push(now);
        Ok(Slot {
            state: self.state.clone(),
            clock: clock.clone(),
            model: model.to_string(),
            lease: lease.to_string(),
            started_ms: now,
        })
    }
}

impl Default for Limiter {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------- usage scan

/// Reads the usage and counts the content chunks of an upstream answer as it streams by.
struct UsageScan {
    stream: bool,
    buf: Vec<u8>,
    events: u64,
    usage: Option<(u64, u64)>,
}

impl UsageScan {
    fn new(stream: bool) -> Self {
        Self {
            stream,
            buf: Vec::new(),
            events: 0,
            usage: None,
        }
    }

    fn note(&mut self, v: &Value) {
        if let (Some(p), Some(c)) = (
            v["usage"]["prompt_tokens"].as_u64(),
            v["usage"]["completion_tokens"].as_u64(),
        ) {
            self.usage = Some((p, c));
        } else if v["choices"][0]["delta"]["content"]
            .as_str()
            .is_some_and(|c| !c.is_empty())
            || v["choices"][0]["delta"].get("tool_calls").is_some()
        {
            // Content only: the opening chunk that carries just the role costs no output token.
            self.events += 1;
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        if self.buf.len() + chunk.len() > CHAT_BODY_LIMIT {
            return;
        }
        self.buf.extend_from_slice(chunk);
        if !self.stream {
            return;
        }
        while let Some(end) = self.buf.windows(2).position(|w| w == b"\n\n") {
            let event: Vec<u8> = self.buf.drain(..end + 2).collect();
            for line in String::from_utf8_lossy(&event).lines() {
                if let Some(data) = line.strip_prefix("data:") {
                    if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                        self.note(&v);
                    }
                }
            }
        }
    }

    fn finish(&mut self) {
        if !self.stream {
            if let Ok(v) = serde_json::from_slice::<Value>(&self.buf) {
                self.note(&v);
            }
        }
    }
}

// ---------------------------------------------------------------- the operator side

/// A request's reservation in the lease's spend book: released on drop, with `actual` as the
/// request's cost, so no path out of `chat` can leak it.
struct Reservation {
    op: Arc<Operator>,
    lease: String,
    id: u64,
    actual: u128,
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.op
            .release_reservation(&self.lease, self.id, self.actual);
    }
}

/// A running request's cancel signal, registered for retirement and removed on drop.
struct CancelReg {
    op: Arc<Operator>,
    lease: String,
    cancel: Arc<tokio::sync::Notify>,
}

impl Drop for CancelReg {
    fn drop(&mut self) {
        let mut cancels = self.op.cancels.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(list) = cancels.get_mut(&self.lease) {
            list.retain(|c| !Arc::ptr_eq(c, &self.cancel));
        }
    }
}

#[derive(Debug)]
pub enum PumpError {
    Timeout,
    Provider(ProviderError),
    Cut,
}

/// What the HTTP layer sends back: the first chunk, then the rest as it arrives.
pub struct ChatReply {
    pub content_type: String,
    pub first: Bytes,
    pub rest: mpsc::Receiver<Result<Bytes, PumpError>>,
}

fn provider_error(e: ProviderError) -> ApiError {
    match e {
        ProviderError::RateLimited { retry_after_secs } => {
            ApiError::new(Code::ProviderRateLimit, "the provider is rate limiting")
                .retry_after(retry_after_secs.max(1))
        }
        _ => ApiError::new(Code::ProviderError, "the provider could not answer"),
    }
}

fn cost_micro(model: &ModelSpec, input_tokens: u64, output_tokens: u64) -> u128 {
    let nano = u128::from(input_tokens) * u128::from(model.input_nano_per_token)
        + u128::from(output_tokens) * u128::from(model.output_nano_per_token);
    nano.div_ceil(1000)
}

impl Operator {
    /// `POST /v1/chat/completions`.
    pub async fn chat(
        self: &Arc<Self>,
        lease_id: &str,
        token: Option<&str>,
        headers: &HeaderMap,
        body: &[u8],
    ) -> ApiResult<ChatReply> {
        if body.len() > CHAT_BODY_LIMIT {
            return Err(ApiError::new(Code::BodyTooLarge, "request body over 4 MiB"));
        }
        let lease = self.authorized(lease_id, token).await?;
        if lease.status != "active" || self.is_closing(lease_id) {
            return Err(ApiError::new(
                Code::LeaseEnded,
                "the lease takes no more requests",
            ));
        }
        let blocked = self
            .blocked_models
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let cfg = &self.config().proxy;
        let parsed = parse_chat(body, cfg, &blocked)?;
        let forward = forward_headers(headers)?;
        let spec = cfg
            .models
            .iter()
            .find(|m| m.id == parsed.model)
            .expect("validated model")
            .clone();

        let now_ms = self.clock.now_ms();
        let lifetime_ms = now_ms
            .saturating_sub(lease.issued_at.unwrap_or(0) * 1000)
            .min(LEASE_MAX_SECS * 1000);
        let slot = self
            .limiter
            .acquire(cfg, &self.clock, &parsed.model, lease_id, lifetime_ms)?;
        let guard = self
            .begin_request(lease_id)
            .map_err(|_| ApiError::new(Code::LeaseEnded, "the lease takes no more requests"))?;

        let max_output = parsed.max_output.unwrap_or(spec.output_cap_tokens);
        let reserve_cost = cost_micro(&spec, parsed.input_tokens, max_output);
        let reserved = self
            .spend
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .reserve(lease_id, lease.bound, reserve_cost);
        let Some((id, reserved_total)) = reserved else {
            return Err(ApiError::new(
                Code::MaxTokensTooLarge,
                "the worst case does not fit the lease's remaining bound",
            ));
        };
        self.ledger.set_inflight(lease_id, reserved_total);
        // From here the reservation is released whatever happens, even if the client drops this
        // future. Until the provider answers it stands at the worst case.
        let mut reservation = Reservation {
            op: self.clone(),
            lease: lease_id.to_string(),
            id,
            actual: reserve_cost,
        };
        let cancel = Arc::new(tokio::sync::Notify::new());
        self.cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(lease_id.to_string())
            .or_default()
            .push(cancel.clone());
        let cancel_reg = CancelReg {
            op: self.clone(),
            lease: lease_id.to_string(),
            cancel: cancel.clone(),
        };
        // One clock for both deadlines: the first byte within `first_byte` of the start, the
        // whole answer within `total` of it.
        let started = Instant::now();
        if let Err(e) = self.mark_delivered(lease_id).await {
            reservation.actual = 0;
            return Err(e);
        }
        if let Some((_, algo)) = forward.iter().find(|(n, _)| n == "x-signing-algo") {
            self.algos
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(lease_id.to_string(), algo.clone());
        }

        let request = ChatRequest {
            body: Bytes::from(parsed.body),
            headers: forward,
        };
        let called = tokio::select! {
            // Retirement ran out of patience while the provider had not answered yet.
            () = cancel.notified() => return Err(ApiError::new(Code::ProviderError, "the request was cancelled")),
            called = tokio::time::timeout(cfg.first_byte, self.provider.chat(lease_id, request)) => called,
        };
        let response = match called {
            Ok(Ok(response)) => response,
            Ok(Err(e)) => {
                // The provider refused outright: nothing was generated.
                reservation.actual = 0;
                return Err(provider_error(e));
            }
            Err(_) => {
                return Err(ApiError::new(
                    Code::ProviderTimeout,
                    "the provider did not answer in time",
                ));
            }
        };

        let (tx, mut rx) = mpsc::channel::<Result<Bytes, PumpError>>(8);
        let (stream, input_tokens) = (parsed.stream, parsed.input_tokens);
        let (idle, first_byte, total) = (cfg.idle, cfg.first_byte, cfg.total);
        let mut upstream = response.body;
        tokio::spawn(async move {
            // The slot, the in-flight guard, the cancel registration and the reservation live
            // until upstream is closed; a panic here leaves the reservation at the worst case.
            let (_slot, _guard, _cancel_reg) = (slot, guard, cancel_reg);
            // Move the whole reservation in (a closure would otherwise capture only its field).
            let mut reservation = reservation;
            let mut scan = UsageScan::new(stream);
            let mut first = true;
            let mut cut = false;
            loop {
                let left = total.saturating_sub(started.elapsed());
                let wait = if first {
                    first_byte.saturating_sub(started.elapsed())
                } else {
                    idle
                };
                let next = tokio::select! {
                    // The client went away: stop reading and let upstream close.
                    () = tx.closed() => break,
                    // Retirement ran out of patience: abort upstream.
                    () = cancel.notified() => {
                        let _ = tx.send(Err(PumpError::Cut)).await;
                        break;
                    }
                    next = tokio::time::timeout(wait.min(left), upstream.next_chunk()) => next,
                };
                let item = match next {
                    Err(_) => {
                        let _ = tx.send(Err(PumpError::Timeout)).await;
                        break;
                    }
                    Ok(None) => break,
                    Ok(Some(Err(e))) => {
                        let _ = tx.send(Err(PumpError::Provider(e))).await;
                        break;
                    }
                    Ok(Some(Ok(chunk))) => {
                        first = false;
                        scan.push(&chunk);
                        // A stream is cut once its estimated cost passes its reservation.
                        if stream && cost_micro(&spec, input_tokens, scan.events) > reserve_cost {
                            cut = true;
                            let _ = tx.send(Err(PumpError::Cut)).await;
                            break;
                        }
                        Ok(chunk)
                    }
                };
                // A client that stops reading must not pin the slot: the hand-over has the same
                // deadline as a read.
                let hand_over = total.saturating_sub(started.elapsed()).min(idle);
                let delivered = tokio::select! {
                    sent = tx.send(item) => sent.is_ok(),
                    () = cancel.notified() => false,
                    () = tokio::time::sleep(hand_over) => false,
                };
                if !delivered {
                    break;
                }
            }
            drop(upstream);
            scan.finish();
            reservation.actual = match (cut, scan.usage) {
                (false, Some((prompt, completion))) => cost_micro(&spec, prompt, completion),
                // No signed usage, or cut: the whole reservation counts.
                _ => reserve_cost,
            };
        });

        match rx.recv().await {
            Some(Ok(first)) => Ok(ChatReply {
                content_type: response.content_type,
                first,
                rest: rx,
            }),
            Some(Err(PumpError::Timeout)) => Err(ApiError::new(
                Code::ProviderTimeout,
                "the provider sent nothing in time",
            )),
            Some(Err(PumpError::Provider(e))) => Err(provider_error(e)),
            Some(Err(PumpError::Cut)) => Err(ApiError::new(
                Code::ProviderError,
                "the answer exceeded its reservation",
            )),
            None => Ok(ChatReply {
                content_type: response.content_type,
                first: Bytes::new(),
                rest: rx,
            }),
        }
    }

    /// Aborts every running proxied request of a lease.
    pub(crate) fn cancel_requests(&self, lease_id: &str) {
        let cancels = self.cancels.lock().unwrap_or_else(|e| e.into_inner());
        for cancel in cancels.get(lease_id).into_iter().flatten() {
            cancel.notify_one();
        }
    }

    fn is_closing(&self, lease_id: &str) -> bool {
        self.closing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(lease_id)
    }

    /// A request ended: its reservation becomes the estimate from usage (0 for a request that
    /// never reached the provider), and the daily budget sees what is still reserved.
    fn release_reservation(&self, lease_id: &str, reservation: u64, actual: u128) {
        let still = self.spend.lock().unwrap_or_else(|e| e.into_inner()).finish(
            lease_id,
            reservation,
            actual,
        );
        // A lease that already settled or expired has no book: nothing to tell the budget.
        if let Some(still) = still {
            self.ledger.set_inflight(lease_id, still);
            // Kept on disk, so a restart does not start the lease's worst-case check from zero.
            let finished = self.spent_estimate(lease_id);
            let (store, id) = (self.store.clone(), lease_id.to_string());
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn(async move {
                    let _ = store
                        .run(move |c| crate::store::set_spend(c, &id, finished))
                        .await;
                });
            }
        }
    }

    /// Compares the pinned models with the provider's live list: a model that is missing, not
    /// attested, or priced differently from its pin is blocked, and a price that differs also
    /// stops key minting (see [`Operator::minting_stop`]).
    pub async fn check_models(&self) {
        // Stamped before the call: a failing provider is asked again in five minutes, not every tick.
        *self
            .models_checked_ms
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = self.clock.now_ms();
        let live = match tokio::time::timeout(Duration::from_secs(10), self.provider.models()).await
        {
            Ok(Ok(Some(live))) => live,
            Ok(Ok(None)) => return,
            _ => {
                self.models_unreadable().await;
                return;
            }
        };
        let mut blocked = HashSet::new();
        let mut priced = HashSet::new();
        for pinned in &self.config().proxy.models {
            let listed = live.iter().find(|m| m.id == pinned.id);
            let same_price = listed.is_some_and(|m| {
                m.input_nano_per_token == pinned.input_nano_per_token
                    && m.output_nano_per_token == pinned.output_nano_per_token
            });
            if listed.is_some() && !same_price {
                priced.insert(pinned.id.clone());
            }
            let ok = same_price
                && listed.is_some_and(|m| m.provider_type == "vllm" && m.attestation_supported);
            if !ok {
                blocked.insert(pinned.id.clone());
            }
        }
        *self
            .blocked_models
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = blocked.clone();
        self.judge_models(&blocked, priced).await;
    }

    pub fn blocked_models(&self) -> HashSet<String> {
        self.blocked_models
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Requests in flight under a lease.
    pub fn limiter_in_flight(&self, lease_id: &str) -> usize {
        self.limiter.lease_in_flight(lease_id)
    }

    /// Estimated cost of the lease's finished requests, micro-USD.
    pub fn spent_estimate(&self, lease_id: &str) -> u128 {
        self.spend
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .finished(lease_id)
    }
}

/// Re-read the pinned prices every 5 minutes.
pub const MODEL_CHECK_EVERY_MS: u64 = 5 * 60 * 1000;
