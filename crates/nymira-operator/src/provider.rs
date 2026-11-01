//! The provider adapter: the one internal seam, because NEAR AI cannot run in CI.
//!
//! Everything is keyed by lease ID. The key itself stays inside the adapter, and so does its
//! lifetime ([`KEY_LIFETIME_SECS`]); the operator only ever sees a key id.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use rand::RngCore;

use crate::clock::Clock;

/// How long a minted key lives at the provider. Longer than the 10-minute lease cap, so a key
/// never dies under a live lease and never outlives a forgotten one for long.
pub const KEY_LIFETIME_SECS: u64 = 15 * 60;

/// The key cap in nano-USD for a lease bound in micro-USD: `bound / 1.03`, floored, with integer
/// arithmetic (the 3% margin is a fixed constant).
pub fn key_cap_nano_usd(bound_micro_usd: u128) -> u128 {
    bound_micro_usd * 1_000 * 100 / 103
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum ProviderError {
    #[error("provider rejected the call: {0}")]
    Rejected(String),
    #[error("provider has no such key: {0}")]
    NotFound(String),
    /// The provider's own 429 (`rate_limit_exceeded`, `service_overloaded`).
    #[error("provider rate limit")]
    RateLimited { retry_after_secs: u64 },
    #[error("provider unavailable: {0}")]
    Unavailable(String),
}

/// What the operator records about a minted key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MintedKey {
    /// Random name unrelated to the lease id and the lease token.
    pub key_id: String,
    pub expires_at: u64,
}

/// Measured spend of one lease key, in nano-USD.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct KeyUsage {
    pub cost_nano_usd: u128,
}

/// A chat request as the proxy forwards it: the validated body and the headers that may pass.
#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub body: Bytes,
    pub headers: Vec<(String, String)>,
}

/// The upstream response body, chunk by chunk. Dropping it closes the upstream connection.
#[async_trait]
pub trait ChatBody: Send {
    async fn next_chunk(&mut self) -> Option<Result<Bytes, ProviderError>>;
}

pub struct ChatResponse {
    pub content_type: String,
    pub body: Box<dyn ChatBody>,
}

/// A model as the provider lists it live, with its prices in nano-USD per token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveModel {
    pub id: String,
    pub provider_type: String,
    pub attestation_supported: bool,
    pub input_nano_per_token: u64,
    pub output_nano_per_token: u64,
}

#[async_trait]
pub trait ProviderAdapter: Send + Sync {
    /// Sends a validated chat request under the lease's key. Returns once the response has begun;
    /// the provider's own 429 is `RateLimited`.
    async fn chat(
        &self,
        _lease_id: &str,
        _request: ChatRequest,
    ) -> Result<ChatResponse, ProviderError> {
        Err(ProviderError::Unavailable(
            "this adapter does not proxy chat".into(),
        ))
    }
    /// The provider's live model list with prices, or `None` when the adapter cannot list it.
    async fn models(&self) -> Result<Option<Vec<LiveModel>>, ProviderError> {
        Ok(None)
    }
    /// Mints a key for `lease_id`, capped at [`key_cap_nano_usd`]`(bound)`.
    async fn mint(&self, lease_id: &str, bound: u128) -> Result<MintedKey, ProviderError>;
    async fn usage(&self, lease_id: &str) -> Result<KeyUsage, ProviderError>;
    async fn revoke(&self, lease_id: &str) -> Result<(), ProviderError>;
    /// The lease ids of every live key, for the start-up sweep.
    async fn list(&self) -> Result<Vec<String>, ProviderError>;
    /// The organisation's credit in micro-USD.
    async fn balance(&self) -> Result<u128, ProviderError>;
    /// The provider's response signature and attestation report for the lease, fetched with the
    /// lease key before it is deactivated and passed to the client unchanged. An adapter that
    /// cannot fetch it returns none.
    async fn evidence(&self, _lease_id: &str) -> Result<Option<serde_json::Value>, ProviderError> {
        Ok(None)
    }
}

/// A key as the fake recorded it.
#[derive(Clone, Debug)]
pub struct FakeKey {
    pub key_id: String,
    pub cap_nano_usd: u128,
    pub expires_at: u64,
    pub revoked: bool,
}

/// How the fake answers a chat request.
#[derive(Clone, Debug)]
pub struct ChatScript {
    pub error: Option<ProviderError>,
    pub first_byte_delay: Duration,
    pub chunk_delay: Duration,
    /// The answer, chunk by chunk; `None` builds one from the request (SSE for a stream, JSON
    /// otherwise) with `usage` as (prompt, completion) tokens.
    pub chunks: Option<Vec<String>>,
    /// After this many chunks the upstream goes silent for good.
    pub stall_after: Option<usize>,
    pub usage: (u64, u64),
    pub stream_events: usize,
}

impl Default for ChatScript {
    fn default() -> Self {
        Self {
            error: None,
            first_byte_delay: Duration::ZERO,
            chunk_delay: Duration::ZERO,
            chunks: None,
            stall_after: None,
            usage: (10, 20),
            stream_events: 3,
        }
    }
}

/// A chat request as the fake received it.
#[derive(Clone, Debug)]
pub struct RecordedChat {
    pub lease_id: String,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
}

struct FakeBody {
    chunks: VecDeque<Bytes>,
    delay: Duration,
    stall_after: Option<usize>,
    sent: usize,
    open: Arc<AtomicUsize>,
}

impl Drop for FakeBody {
    fn drop(&mut self) {
        self.open.fetch_sub(1, Ordering::SeqCst);
    }
}

#[async_trait]
impl ChatBody for FakeBody {
    async fn next_chunk(&mut self) -> Option<Result<Bytes, ProviderError>> {
        if self.stall_after == Some(self.sent) {
            std::future::pending::<()>().await;
        }
        if !self.delay.is_zero() {
            tokio::time::sleep(self.delay).await;
        }
        self.sent += 1;
        self.chunks.pop_front().map(Ok)
    }
}

#[derive(Default)]
struct FakeState {
    chat_script: ChatScript,
    chat_calls: Vec<RecordedChat>,
    evidence: Option<serde_json::Value>,
    models: Option<Vec<LiveModel>>,
    keys: HashMap<String, FakeKey>,
    usage: HashMap<String, u128>,
    balance_micro_usd: u128,
    mint_delay: Duration,
    mint_error: Option<ProviderError>,
    balance_error: Option<ProviderError>,
    usage_error: Option<ProviderError>,
    revoke_error: Option<ProviderError>,
    mint_calls: u32,
}

/// Records keys and returns configurable usage, balance, delays and errors.
pub struct FakeProvider {
    clock: Arc<dyn Clock>,
    state: Mutex<FakeState>,
    open_streams: Arc<AtomicUsize>,
}

impl FakeProvider {
    pub fn new(clock: Arc<dyn Clock>, balance_micro_usd: u128) -> Self {
        Self {
            clock,
            state: Mutex::new(FakeState {
                balance_micro_usd,
                ..FakeState::default()
            }),
            open_streams: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_chat_script(&self, script: ChatScript) {
        self.state().chat_script = script;
    }

    pub fn chat_calls(&self) -> Vec<RecordedChat> {
        self.state().chat_calls.clone()
    }

    /// Chat answers whose upstream connection is still open.
    pub fn open_streams(&self) -> usize {
        self.open_streams.load(Ordering::SeqCst)
    }

    pub fn set_evidence(&self, evidence: Option<serde_json::Value>) {
        self.state().evidence = evidence;
    }

    pub fn set_models(&self, models: Option<Vec<LiveModel>>) {
        self.state().models = models;
    }

    pub fn set_balance(&self, micro_usd: u128) {
        self.state().balance_micro_usd = micro_usd;
    }

    pub fn set_usage(&self, lease_id: &str, cost_nano_usd: u128) {
        self.state().usage.insert(lease_id.into(), cost_nano_usd);
    }

    pub fn set_mint_delay(&self, delay: Duration) {
        self.state().mint_delay = delay;
    }

    pub fn set_mint_error(&self, error: Option<ProviderError>) {
        self.state().mint_error = error;
    }

    pub fn set_revoke_error(&self, error: Option<ProviderError>) {
        self.state().revoke_error = error;
    }

    pub fn set_usage_error(&self, error: Option<ProviderError>) {
        self.state().usage_error = error;
    }

    pub fn set_balance_error(&self, error: Option<ProviderError>) {
        self.state().balance_error = error;
    }

    /// Every key ever minted, by lease id.
    pub fn keys(&self) -> HashMap<String, FakeKey> {
        self.state().keys.clone()
    }

    pub fn live_keys(&self) -> usize {
        self.state().keys.values().filter(|k| !k.revoked).count()
    }

    pub fn mint_calls(&self) -> u32 {
        self.state().mint_calls
    }

    /// A key that exists at the provider with no record at the operator: what a crash between
    /// minting and writing leaves behind.
    pub fn orphan_key(&self, lease_id: &str, cap_nano_usd: u128) {
        self.state().keys.insert(
            lease_id.into(),
            FakeKey {
                key_id: random_name(),
                cap_nano_usd,
                expires_at: self.clock.now_secs() + KEY_LIFETIME_SECS,
                revoked: false,
            },
        );
    }
}

fn random_name() -> String {
    let mut bytes = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("k-{}", hex::encode(bytes))
}

#[async_trait]
impl ProviderAdapter for FakeProvider {
    async fn mint(&self, lease_id: &str, bound: u128) -> Result<MintedKey, ProviderError> {
        let (delay, error) = {
            let mut state = self.state();
            state.mint_calls += 1;
            (state.mint_delay, state.mint_error.clone())
        };
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        if let Some(error) = error {
            return Err(error);
        }
        let key = FakeKey {
            key_id: random_name(),
            cap_nano_usd: key_cap_nano_usd(bound),
            expires_at: self.clock.now_secs() + KEY_LIFETIME_SECS,
            revoked: false,
        };
        let minted = MintedKey {
            key_id: key.key_id.clone(),
            expires_at: key.expires_at,
        };
        self.state().keys.insert(lease_id.into(), key);
        Ok(minted)
    }

    async fn chat(
        &self,
        lease_id: &str,
        request: ChatRequest,
    ) -> Result<ChatResponse, ProviderError> {
        let script = {
            let mut state = self.state();
            state.chat_calls.push(RecordedChat {
                lease_id: lease_id.into(),
                body: request.body.to_vec(),
                headers: request.headers.clone(),
            });
            state.chat_script.clone()
        };
        if let Some(error) = script.error {
            return Err(error);
        }
        if !script.first_byte_delay.is_zero() {
            tokio::time::sleep(script.first_byte_delay).await;
        }
        let stream = serde_json::from_slice::<serde_json::Value>(&request.body)
            .is_ok_and(|v| v["stream"] == true);
        let usage = serde_json::json!({"prompt_tokens": script.usage.0, "completion_tokens": script.usage.1});
        let chunks: Vec<Bytes> = match script.chunks {
            Some(chunks) => chunks.into_iter().map(Bytes::from).collect(),
            None if stream => {
                let mut out: Vec<Bytes> = (0..script.stream_events)
                    .map(|i| Bytes::from(format!("data: {}\n\n", serde_json::json!({"choices": [{"delta": {"content": format!("{i:02x}")}}]}))))
                    .collect();
                out.push(Bytes::from(format!(
                    "data: {}\n\n",
                    serde_json::json!({"choices": [], "usage": usage})
                )));
                out.push(Bytes::from("data: [DONE]\n\n"));
                out
            }
            None => vec![Bytes::from(
                serde_json::json!({"id": "chatcmpl-1", "usage": usage}).to_string(),
            )],
        };
        self.open_streams.fetch_add(1, Ordering::SeqCst);
        Ok(ChatResponse {
            content_type: if stream {
                "text/event-stream"
            } else {
                "application/json"
            }
            .into(),
            body: Box::new(FakeBody {
                chunks: chunks.into(),
                delay: script.chunk_delay,
                stall_after: script.stall_after,
                sent: 0,
                open: self.open_streams.clone(),
            }),
        })
    }

    async fn evidence(&self, _lease_id: &str) -> Result<Option<serde_json::Value>, ProviderError> {
        Ok(self.state().evidence.clone())
    }

    async fn models(&self) -> Result<Option<Vec<LiveModel>>, ProviderError> {
        Ok(self.state().models.clone())
    }

    async fn usage(&self, lease_id: &str) -> Result<KeyUsage, ProviderError> {
        let state = self.state();
        if let Some(error) = &state.usage_error {
            return Err(error.clone());
        }
        if !state.keys.contains_key(lease_id) {
            return Err(ProviderError::NotFound("unknown lease".into()));
        }
        Ok(KeyUsage {
            cost_nano_usd: state.usage.get(lease_id).copied().unwrap_or(0),
        })
    }

    async fn revoke(&self, lease_id: &str) -> Result<(), ProviderError> {
        if let Some(error) = self.state().revoke_error.clone() {
            return Err(error);
        }
        match self.state().keys.get_mut(lease_id) {
            Some(key) => key.revoked = true,
            None => return Err(ProviderError::NotFound("unknown lease".into())),
        }
        Ok(())
    }

    async fn list(&self) -> Result<Vec<String>, ProviderError> {
        let state = self.state();
        Ok(state
            .keys
            .iter()
            .filter(|(_, key)| !key.revoked)
            .map(|(lease, _)| lease.clone())
            .collect())
    }

    async fn balance(&self) -> Result<u128, ProviderError> {
        let state = self.state();
        match &state.balance_error {
            Some(error) => Err(error.clone()),
            None => Ok(state.balance_micro_usd),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::key_cap_nano_usd;

    #[test]
    fn the_key_cap_is_bound_over_1_03_in_nano_usd_rounded_down() {
        // 0.10 USDC = 100_000 micro-USD -> 97_087_378.64 nano-USD
        assert_eq!(key_cap_nano_usd(100_000), 97_087_378);
        assert_eq!(key_cap_nano_usd(500_000), 485_436_893);
        assert_eq!(key_cap_nano_usd(1), 970);
        assert_eq!(key_cap_nano_usd(0), 0);
    }
}
