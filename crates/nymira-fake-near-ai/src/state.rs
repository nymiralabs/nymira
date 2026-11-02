//! Shared state, the control switches and the in-process handle.

use crate::policy::{default_measurement, SignatureKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard},
};

/// Bearer token the fake accepts where the real service wants the operator's session JWT.
pub const SESSION_TOKEN: &str = "fake-session-jwt";
/// Read-only reporting token accepted by the usage endpoints.
pub const REPORTING_TOKEN: &str = "rpt-fake-reporting";
/// The refresh token the fake starts with. Each exchange issues `rt-fake-{n+1}` and kills `rt-fake-{n}`.
pub const FIRST_REFRESH_TOKEN: &str = "rt-fake-0";

pub struct ModelInfo {
    pub id: &'static str,
    pub provider_type: &'static str,
    pub verifiable: bool,
    pub attestation_supported: bool,
}

/// The four pinned canonical ids (operator spec), plus one Chutes and one external
/// model so a test can check the `providerType == "vllm"` filter.
pub const MODELS: [ModelInfo; 6] = [
    ModelInfo {
        id: "z-ai/glm-5.3-flash",
        provider_type: "vllm",
        verifiable: true,
        attestation_supported: true,
    },
    ModelInfo {
        id: "Qwen/Qwen3.6-35B-A3B-FP8",
        provider_type: "vllm",
        verifiable: true,
        attestation_supported: true,
    },
    ModelInfo {
        id: "Qwen/Qwen3.8-27B",
        provider_type: "vllm",
        verifiable: true,
        attestation_supported: true,
    },
    ModelInfo {
        id: "Qwen/Qwen3-VL-30B-A3B-Instruct",
        provider_type: "vllm",
        verifiable: true,
        attestation_supported: true,
    },
    ModelInfo {
        id: "moonshotai/kimi-fake",
        provider_type: "chutes",
        verifiable: true,
        attestation_supported: true,
    },
    ModelInfo {
        id: "openai/gpt-fake",
        provider_type: "external",
        verifiable: false,
        attestation_supported: false,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
}

/// Every behaviour switch. Defaults describe a healthy provider.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Switches {
    /// Fail this many of the next matching requests with `fail_status`.
    pub fail_next: u32,
    pub fail_status: u16,
    /// Only requests whose path starts with this prefix can fail.
    pub fail_path_prefix: String,
    /// Delay added to every `/v1/` request.
    pub latency_ms: u64,
    /// When set, replaces the computed token counts in answers, usage rows and cost.
    pub usage: Option<Usage>,
    /// Kind for non-streamed answers.
    pub chat_signature_kind: SignatureKind,
    /// Kind for streamed answers. The real gateway signs streams itself.
    pub stream_signature_kind: SignatureKind,
    /// Flip one byte of every signature served.
    pub bad_signature: bool,
    /// Sign with a key that no attestation lists.
    pub unbound_key: bool,
    /// Measurement in the attestation report.
    pub measurement: String,
    /// TCB status in the attestation report.
    pub tcb_status: String,
    /// Organisation credit in nano-USD.
    pub balance_nano: i64,
    pub input_price_nano_per_token: u64,
    pub output_price_nano_per_token: u64,
    /// Fixed answer text. Default: `fake reply to: <last user message>`.
    pub reply: Option<String>,
    /// Whether an exchange replaces the refresh token (the real behaviour is a day-1 check).
    pub rotate_refresh_tokens: bool,
    /// Lifetime reported for a refresh token, seconds.
    pub refresh_ttl_secs: u64,
}

impl Default for Switches {
    fn default() -> Self {
        Self {
            fail_next: 0,
            fail_status: 500,
            fail_path_prefix: "/v1/".into(),
            latency_ms: 0,
            usage: None,
            chat_signature_kind: SignatureKind::ProviderTee,
            stream_signature_kind: SignatureKind::Gateway,
            bad_signature: false,
            unbound_key: false,
            measurement: default_measurement(),
            tcb_status: "UpToDate".into(),
            balance_nano: 10_000_000_000,
            input_price_nano_per_token: 80,
            output_price_nano_per_token: 500,
            reply: None,
            rotate_refresh_tokens: true,
            refresh_ttl_secs: 30 * 86_400,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SpendLimit {
    /// Nano-USD.
    pub amount: u64,
    pub currency: String,
}

#[derive(Clone, Debug)]
pub struct KeyRecord {
    pub id: String,
    pub name: String,
    pub secret: String,
    pub spend_limit: Option<SpendLimit>,
    /// Stored and returned, never enforced.
    pub expires_at: Option<Value>,
    pub is_active: bool,
    pub deleted: bool,
    pub spent_nano: u64,
}

impl KeyRecord {
    /// The key as the API shows it. The secret appears only when asked, as on creation.
    pub fn to_json(&self, with_secret: bool) -> Value {
        let mut v = json!({
            "id": self.id,
            "name": self.name,
            "spendLimit": self.spend_limit,
            "expires_at": self.expires_at,
            "is_active": self.is_active,
            "deleted": self.deleted,
            "spent_nano_usd": self.spent_nano,
        });
        if with_secret {
            v["key"] = json!(self.secret);
        }
        v
    }
}

/// One chat request as the fake received it, for tests to inspect.
#[derive(Clone, Debug)]
pub struct ChatRecord {
    pub chat_id: String,
    pub key_id: String,
    pub model: String,
    pub stream: bool,
    /// `X-Client-Pub-Key` was present and the content fields were encrypted.
    pub e2ee: bool,
    pub encrypt_all_fields: bool,
    pub no_aliasing: bool,
    pub accept_encoding: Option<String>,
    pub include_usage: bool,
    /// The request body exactly as received.
    pub body: Vec<u8>,
    /// The response body exactly as sent.
    pub response: Vec<u8>,
}

#[derive(Clone, Debug, Serialize)]
pub struct UsageRow {
    pub inference_id: String,
    pub api_key_id: String,
    pub model: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    /// Serialized as the real export names it.
    #[serde(rename = "total_cost_nano_usd")]
    pub cost_nano_usd: u64,
}

pub(crate) struct SigRecord {
    pub key_id: String,
    pub model: String,
    pub kind: SignatureKind,
    pub request_hash: String,
    pub response_hash: String,
}

#[derive(Default)]
pub(crate) struct Inner {
    pub switches: Switches,
    pub keys: Vec<KeyRecord>,
    pub chats: Vec<ChatRecord>,
    pub usage: Vec<UsageRow>,
    pub sigs: HashMap<String, SigRecord>,
    pub counter: u64,
    /// How many times the refresh token was exchanged and replaced.
    pub refresh_count: u64,
}

/// In-process control and inspection of a running fake. Cheap to clone.
#[derive(Clone, Default)]
pub struct Handle(pub(crate) Arc<Mutex<Inner>>);

impl Handle {
    pub(crate) fn lock(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn switches(&self) -> Switches {
        self.lock().switches.clone()
    }
    pub fn update(&self, f: impl FnOnce(&mut Switches)) {
        f(&mut self.lock().switches);
    }
    /// Merge-patch from JSON, as `POST /control/switches` does. `null` clears an `Option`.
    pub fn patch(&self, patch: &Value) -> Result<Switches, String> {
        let mut g = self.lock();
        let mut cur = serde_json::to_value(&g.switches).map_err(|e| e.to_string())?;
        let (Some(cur_map), Some(p)) = (cur.as_object_mut(), patch.as_object()) else {
            return Err("patch must be a JSON object".into());
        };
        for (k, v) in p {
            cur_map.insert(k.clone(), v.clone());
        }
        g.switches = serde_json::from_value(cur).map_err(|e| e.to_string())?;
        Ok(g.switches.clone())
    }
    /// Every key minted so far, deleted ones included.
    pub fn keys(&self) -> Vec<KeyRecord> {
        self.lock().keys.clone()
    }
    pub fn chats(&self) -> Vec<ChatRecord> {
        self.lock().chats.clone()
    }
    pub fn usage_rows(&self) -> Vec<UsageRow> {
        self.lock().usage.clone()
    }
    /// Back to defaults, with all keys, chats and usage forgotten.
    pub fn reset(&self) {
        *self.lock() = Inner::default();
    }
}
