//! End-to-end encryption of a chat to the model's attested key, and the reading of its answer.
//!
//! The suite is the one the live gateway accepted on 2026-11-01: Ed25519 public keys on
//! the wire, X25519 through the Montgomery map, HKDF-SHA256 (no salt, info `ed25519_encryption`),
//! XChaCha20-Poly1305, `[ephemeral key 32][nonce 24][ciphertext and tag]`, hex.
//!
//! Every field the model reads is encrypted: message `content` and `name`, tool-call names and
//! arguments, and tool names, descriptions and parameters. A plaintext tool definition would be
//! readable by the operator, who refuses it.

use chacha20poly1305::aead::{Aead, KeyInit};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use ed25519_dalek::{SigningKey, VerifyingKey};
use hkdf::Hkdf;
use rand::RngCore;
use serde_json::{json, Map, Value};
use sha2::Sha256;
use x25519_dalek::{PublicKey, StaticSecret};

const INFO: &[u8] = b"ed25519_encryption";
const FIELDS_TOP: [&str; 15] = [
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
    "seed",
    "stop",
    "tools",
    "tool_choice",
];

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum E2eeError {
    #[error("the request cannot be sent end-to-end encrypted: {0}")]
    Unsupported(String),
    #[error("the model's key is not a valid Ed25519 public key")]
    ModelKey,
    #[error("an encrypted field of the answer did not decrypt")]
    Decrypt,
}

/// The client's key pair for one conversation.
pub struct ClientKeys {
    signing: SigningKey,
}

impl ClientKeys {
    pub fn generate() -> Self {
        let mut seed = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut seed);
        Self {
            signing: SigningKey::from_bytes(&seed),
        }
    }

    /// Keys from a fixed seed, for tests that need the same key every run.
    pub fn from_seed(seed: &[u8; 32]) -> Self {
        Self {
            signing: SigningKey::from_bytes(seed),
        }
    }

    pub fn public_hex(&self) -> String {
        hex::encode(self.signing.verifying_key().to_bytes())
    }

    fn secret(&self) -> StaticSecret {
        StaticSecret::from(self.signing.to_scalar_bytes())
    }
}

fn cipher(shared: &[u8; 32]) -> XChaCha20Poly1305 {
    let mut key = [0u8; 32];
    Hkdf::<Sha256>::new(None, shared)
        .expand(INFO, &mut key)
        .expect("32 bytes is a valid HKDF length");
    XChaCha20Poly1305::new((&key).into())
}

/// Encrypts `plaintext` to the Ed25519 public key `model_pub`; hex.
pub fn encrypt(model_pub: &[u8; 32], plaintext: &[u8]) -> Result<String, E2eeError> {
    #[cfg(test)]
    if let Some(n) = tests::next_fixed() {
        return encrypt_with(model_pub, plaintext, &[n; 32], &[n; 24]);
    }
    let mut seed = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let mut nonce = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    encrypt_with(model_pub, plaintext, &seed, &nonce)
}

/// [`encrypt`] with the ephemeral secret and the nonce given. Only for known-answer tests: a
/// reused pair breaks the encryption.
pub fn encrypt_with(
    model_pub: &[u8; 32],
    plaintext: &[u8],
    eph_seed: &[u8; 32],
    nonce: &[u8; 24],
) -> Result<String, E2eeError> {
    let x = VerifyingKey::from_bytes(model_pub)
        .map_err(|_| E2eeError::ModelKey)?
        .to_montgomery()
        .to_bytes();
    let eph = StaticSecret::from(*eph_seed);
    let shared = eph.diffie_hellman(&PublicKey::from(x));
    let ct = cipher(shared.as_bytes())
        .encrypt(XNonce::from_slice(nonce), plaintext)
        .expect("encryption does not fail");
    Ok(hex::encode(
        [PublicKey::from(&eph).as_bytes().as_slice(), nonce, &ct].concat(),
    ))
}

/// The headers a chat request carries to the operator next to the sealed body.
pub fn request_headers(client_pub_hex: &str, model_pub: &[u8; 32]) -> [(&'static str, String); 4] {
    [
        ("x-signing-algo", "ed25519".into()),
        ("x-client-pub-key", client_pub_hex.into()),
        ("x-model-pub-key", hex::encode(model_pub)),
        ("x-encryption-version", "2".into()),
    ]
}

/// Opens a hex ciphertext addressed to `keys`.
pub fn decrypt(keys: &ClientKeys, text: &str) -> Result<String, E2eeError> {
    let data = hex::decode(text).map_err(|_| E2eeError::Decrypt)?;
    if data.len() < 32 + 24 + 16 {
        return Err(E2eeError::Decrypt);
    }
    let eph: [u8; 32] = data[..32].try_into().expect("32 bytes");
    let shared = keys.secret().diffie_hellman(&PublicKey::from(eph));
    let plain = cipher(shared.as_bytes())
        .decrypt(XNonce::from_slice(&data[32..56]), &data[56..])
        .map_err(|_| E2eeError::Decrypt)?;
    String::from_utf8(plain).map_err(|_| E2eeError::Decrypt)
}

fn unsupported(why: impl Into<String>) -> E2eeError {
    E2eeError::Unsupported(why.into())
}

fn enc_str(v: &Value, key: &[u8; 32], what: &str) -> Result<Value, E2eeError> {
    match v {
        Value::String(s) => Ok(Value::String(encrypt(key, s.as_bytes())?)),
        _ => Err(unsupported(format!("{what} must be a string"))),
    }
}

/// The request body the operator and the gateway receive: OpenAI's body with the sensitive fields
/// encrypted to `model_pub`, `max_tokens` set, usage always requested. Unknown fields are refused,
/// not forwarded in the clear.
pub fn seal_request(
    request: &Value,
    model_pub: &[u8; 32],
    max_tokens: Option<u64>,
) -> Result<Value, E2eeError> {
    let obj = request
        .as_object()
        .ok_or_else(|| unsupported("the body is not an object"))?;
    if let Some(k) = obj.keys().find(|k| !FIELDS_TOP.contains(&k.as_str())) {
        return Err(unsupported(format!("the field {k} is not supported")));
    }
    let mut out = Map::new();
    for (k, v) in obj {
        match k.as_str() {
            "messages" => {
                let messages = v
                    .as_array()
                    .ok_or_else(|| unsupported("messages must be a list"))?;
                let sealed: Result<Vec<_>, _> = messages
                    .iter()
                    .map(|m| seal_message(m, model_pub))
                    .collect();
                out.insert(k.clone(), Value::Array(sealed?));
            }
            "tools" => {
                let tools = v
                    .as_array()
                    .ok_or_else(|| unsupported("tools must be a list"))?;
                let sealed: Result<Vec<_>, _> =
                    tools.iter().map(|t| seal_tool(t, model_pub)).collect();
                out.insert(k.clone(), Value::Array(sealed?));
            }
            "max_tokens" | "max_completion_tokens" => {}
            "stream_options" => {}
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    if let Some(m) = max_tokens {
        out.insert("max_tokens".into(), json!(m));
    }
    out.insert("stream_options".into(), json!({ "include_usage": true }));
    Ok(Value::Object(out))
}

fn seal_message(m: &Value, key: &[u8; 32]) -> Result<Value, E2eeError> {
    let obj = m
        .as_object()
        .ok_or_else(|| unsupported("a message is not an object"))?;
    let mut out = Map::new();
    for (k, v) in obj {
        let sealed = match (k.as_str(), v) {
            ("content", Value::Null) => Value::Null,
            ("content", Value::String(_)) => enc_str(v, key, "content")?,
            ("content", _) => return Err(unsupported("message content must be one string")),
            ("name", Value::String(_)) => enc_str(v, key, "name")?,
            ("tool_calls", Value::Array(calls)) => Value::Array(
                calls
                    .iter()
                    .map(|c| seal_call(c, key))
                    .collect::<Result<_, _>>()?,
            ),
            ("role" | "tool_call_id", _) => v.clone(),
            _ => {
                return Err(unsupported(format!(
                    "the message field {k} is not supported"
                )))
            }
        };
        out.insert(k.clone(), sealed);
    }
    Ok(Value::Object(out))
}

fn seal_call(c: &Value, key: &[u8; 32]) -> Result<Value, E2eeError> {
    let mut call = c
        .as_object()
        .cloned()
        .ok_or_else(|| unsupported("a tool call is not an object"))?;
    let function = call
        .get_mut("function")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| unsupported("a tool call has no function"))?;
    for field in ["name", "arguments"] {
        if let Some(v) = function.get(field).cloned() {
            function.insert(field.into(), enc_str(&v, key, field)?);
        }
    }
    Ok(Value::Object(call))
}

fn seal_tool(t: &Value, key: &[u8; 32]) -> Result<Value, E2eeError> {
    let mut tool = t
        .as_object()
        .cloned()
        .ok_or_else(|| unsupported("a tool is not an object"))?;
    let function = tool
        .get_mut("function")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| unsupported("a tool has no function"))?;
    for field in ["name", "description"] {
        if let Some(v) = function.get(field).cloned() {
            function.insert(field.into(), enc_str(&v, key, field)?);
        }
    }
    if let Some(p) = function.get("parameters").cloned() {
        // The gateway reads an encrypted JSON string, not an object.
        function.insert(
            "parameters".into(),
            Value::String(encrypt(key, p.to_string().as_bytes())?),
        );
    }
    Ok(Value::Object(tool))
}

fn open_field(keys: &ClientKeys, parent: &mut Value, field: &str) -> Result<(), E2eeError> {
    // The empty string opens a stream (`"content": ""`); it holds nothing to decrypt.
    if let Some(Value::String(s)) = parent.get(field).filter(|v| v.as_str() != Some("")) {
        let plain = decrypt(keys, s)?;
        parent[field] = Value::String(plain);
    }
    Ok(())
}

/// Decrypts the content-bearing fields of an answer or of a streamed chunk (`message` or `delta`).
fn open_message(keys: &ClientKeys, m: &mut Value) -> Result<(), E2eeError> {
    for field in ["content", "reasoning_content", "reasoning"] {
        open_field(keys, m, field)?;
    }
    if let Some(calls) = m.get_mut("tool_calls").and_then(Value::as_array_mut) {
        for call in calls {
            if let Some(function) = call.get_mut("function") {
                for field in ["name", "arguments"] {
                    open_field(keys, function, field)?;
                }
            }
        }
    }
    Ok(())
}

/// A non-streamed answer with its encrypted fields opened. An encrypted field that does not
/// decrypt is an error, never passed on as ciphertext.
pub fn open_response(keys: &ClientKeys, response: &mut Value) -> Result<(), E2eeError> {
    if let Some(choices) = response.get_mut("choices").and_then(Value::as_array_mut) {
        for c in choices {
            for part in ["message", "delta"] {
                if let Some(m) = c.get_mut(part) {
                    open_message(keys, m)?;
                }
            }
        }
    }
    Ok(())
}

/// One line of a streamed answer: `data: {chunk}` is opened, everything else is returned as is.
pub fn open_sse_line(keys: &ClientKeys, line: &str) -> Result<String, E2eeError> {
    let Some(data) = line.strip_prefix("data:") else {
        // Only lines that carry no answer text pass: blank lines, comments and SSE fields.
        let harmless = line.is_empty()
            || line.starts_with(':')
            || ["event:", "id:", "retry:"]
                .iter()
                .any(|f| line.starts_with(f));
        return if harmless {
            Ok(line.to_string())
        } else {
            Err(E2eeError::Decrypt)
        };
    };
    let data = data.trim();
    if data == "[DONE]" {
        return Ok(line.to_string());
    }
    let mut chunk: Value = serde_json::from_str(data).map_err(|_| E2eeError::Decrypt)?;
    open_response(keys, &mut chunk)?;
    Ok(format!("data: {chunk}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    thread_local! {
        /// When set, `encrypt` uses `[n; 32]` and `[n; 24]` as ephemeral secret and nonce and counts
        /// `n` up, so a whole sealed request is the same bytes every run (fixtures only).
        static FIXED: Cell<Option<u8>> = const { Cell::new(None) };
    }

    pub(super) fn next_fixed() -> Option<u8> {
        FIXED.with(|c| {
            let n = c.get()?;
            c.set(Some(n + 1));
            Some(n)
        })
    }

    fn model() -> (SigningKey, [u8; 32]) {
        let k = SigningKey::from_bytes(&[5; 32]);
        let p = k.verifying_key().to_bytes();
        (k, p)
    }

    /// What the gateway does with a request: opens it with the model's secret.
    fn model_opens(model: &SigningKey, text: &str) -> String {
        let keys = ClientKeys {
            signing: model.clone(),
        };
        decrypt(&keys, text).unwrap()
    }

    #[test]
    fn a_round_trip_needs_the_right_secret() {
        let (sk, pk) = model();
        let sealed = encrypt(&pk, b"hello").unwrap();
        assert_eq!(model_opens(&sk, &sealed), "hello");
        assert!(decrypt(&ClientKeys::generate(), &sealed).is_err());
        assert_ne!(
            sealed,
            encrypt(&pk, b"hello").unwrap(),
            "fresh ephemeral key"
        );
    }

    #[test]
    fn every_sensitive_field_is_ciphertext() {
        let (sk, pk) = model();
        let request = json!({
            "model": "z-ai/glm-5.3-flash", "stream": false, "temperature": 0.2,
            "messages": [
                {"role": "user", "name": "alice", "content": "secret question"},
                {"role": "assistant", "content": null, "tool_calls": [{"id": "c1", "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"Paris\"}"}}]},
                {"role": "tool", "tool_call_id": "c1", "content": "sunny"}
            ],
            "tools": [{"type": "function", "function": {"name": "get_weather",
                "description": "Weather", "parameters": {"type": "object"}}}],
        });
        let sealed = seal_request(&request, &pk, Some(500)).unwrap();
        let text = sealed.to_string();
        for secret in [
            "secret question",
            "alice",
            "get_weather",
            "Paris",
            "sunny",
            "Weather",
        ] {
            assert!(!text.contains(secret), "{secret} leaked");
        }
        assert_eq!(sealed["model"], "z-ai/glm-5.3-flash");
        assert_eq!(sealed["max_tokens"], 500);
        assert_eq!(sealed["stream_options"], json!({"include_usage": true}));
        assert_eq!(sealed["messages"][2]["tool_call_id"], "c1");
        let opened = model_opens(&sk, sealed["messages"][0]["content"].as_str().unwrap());
        assert_eq!(opened, "secret question");
        let params = model_opens(
            &sk,
            sealed["tools"][0]["function"]["parameters"]
                .as_str()
                .unwrap(),
        );
        assert_eq!(params, "{\"type\":\"object\"}");
    }

    #[test]
    fn the_operators_own_parser_accepts_a_sealed_request_with_tools() {
        use nymira_operator::proxy::{parse_chat, ProxyConfig};
        let (_, pk) = model();
        let request = json!({"model": "z-ai/glm-5.3-flash",
            "messages": [{"role": "user", "content": "hi"}],
            "tools": [{"type": "function", "function": {"name": "f", "description": "d",
                "parameters": {"type": "object"}}}]});
        let sealed = seal_request(&request, &pk, Some(100)).unwrap();
        let parsed = parse_chat(
            sealed.to_string().as_bytes(),
            &ProxyConfig::default(),
            &Default::default(),
        )
        .unwrap();
        assert!(parsed.input_tokens > 0);
        // The same request with a plaintext tool is what the operator refuses.
        let mut plain = sealed.clone();
        plain["tools"][0]["function"]["name"] = json!("f");
        assert!(parse_chat(
            plain.to_string().as_bytes(),
            &ProxyConfig::default(),
            &Default::default()
        )
        .is_err());
    }

    #[test]
    fn unknown_fields_and_array_content_are_refused_not_forwarded() {
        let (_, pk) = model();
        let bad = json!({"model": "m", "messages": [], "logit_bias": {"1": 5}});
        assert!(matches!(
            seal_request(&bad, &pk, None),
            Err(E2eeError::Unsupported(_))
        ));
        let parts = json!({"model": "m", "messages": [{"role": "user",
            "content": [{"type": "text", "text": "hi"}]}]});
        assert!(matches!(
            seal_request(&parts, &pk, None),
            Err(E2eeError::Unsupported(_))
        ));
        let extra =
            json!({"model": "m", "messages": [{"role": "user", "content": "x", "audio": 1}]});
        assert!(seal_request(&extra, &pk, None).is_err());
    }

    #[test]
    fn tool_choice_stays_readable_and_passes_through() {
        // The operator reads `tool_choice` (it stays in the clear, as the threat model says), so
        // agent frameworks that send it with `tools` must not get a refusal.
        let (_, pk) = model();
        let r = json!({"model": "m", "messages": [{"role": "user", "content": "x"}],
                       "tool_choice": "auto"});
        let sealed = seal_request(&r, &pk, None).unwrap();
        assert_eq!(sealed["tool_choice"], "auto");
        let named = json!({"model": "m", "messages": [{"role": "user", "content": "x"}],
                           "tool_choice": {"type": "function", "function": {"name": "f"}}});
        assert_eq!(
            seal_request(&named, &pk, None).unwrap()["tool_choice"]["function"]["name"],
            "f"
        );
    }

    #[test]
    fn the_callers_token_limit_is_replaced_by_ours() {
        let (_, pk) = model();
        let r = json!({"model": "m", "max_completion_tokens": 9_999_999,
                       "messages": [{"role": "user", "content": "x"}]});
        let s = seal_request(&r, &pk, Some(100)).unwrap();
        assert_eq!(s["max_tokens"], 100);
        assert!(s.get("max_completion_tokens").is_none());
    }

    #[test]
    fn answers_open_and_a_bad_ciphertext_is_an_error() {
        let keys = ClientKeys::generate();
        let me: [u8; 32] = hex::decode(keys.public_hex()).unwrap().try_into().unwrap();
        let mut answer = json!({"choices": [{"message": {"role": "assistant",
            "content": encrypt(&me, b"Paris").unwrap(),
            "tool_calls": [{"function": {"name": encrypt(&me, b"f").unwrap(),
                                         "arguments": encrypt(&me, b"{}").unwrap()}}]}}],
            "usage": {"prompt_tokens": 3}});
        open_response(&keys, &mut answer).unwrap();
        assert_eq!(answer["choices"][0]["message"]["content"], "Paris");
        assert_eq!(
            answer["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
            "f"
        );
        assert_eq!(answer["usage"]["prompt_tokens"], 3);

        let mut broken = json!({"choices": [{"message": {"content": "ab12"}}]});
        assert_eq!(open_response(&keys, &mut broken), Err(E2eeError::Decrypt));
        // A line that is not a data line is left alone; [DONE] too.
        assert_eq!(
            open_sse_line(&keys, ": keep-alive").unwrap(),
            ": keep-alive"
        );
        assert_eq!(
            open_sse_line(&keys, "data: [DONE]").unwrap(),
            "data: [DONE]"
        );
        let line = format!(
            "data: {}",
            json!({"choices": [{"delta": {"content": encrypt(&me, b"Pa").unwrap()}}]})
        );
        assert!(open_sse_line(&keys, &line).unwrap().contains("\"Pa\""));
    }

    // ---- interop with web/src/chat/e2ee.ts: crates/nymirad/tests/fixtures/e2ee-{rust,ts}.json ----

    const MODEL_SEED: [u8; 32] = [5; 32];
    const CLIENT_SEED: [u8; 32] = [6; 32];
    const RECORDED_EPH: [u8; 32] = [0x41; 32];
    const RECORDED_NONCE: [u8; 24] = [0x42; 24];
    const RECORDED_TEXT: &str = "interop: sealed to the key NEAR AI attested";

    fn fixture_path(name: &str) -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(name)
    }

    fn recorded_model_pub() -> [u8; 32] {
        let report: Value = serde_json::from_slice(
            &std::fs::read(fixture_path("near-ai-recorded/model-report.json")).unwrap(),
        )
        .unwrap();
        let key = report["model_attestations"][0]["signing_public_key"]
            .as_str()
            .unwrap();
        hex::decode(key).unwrap().try_into().unwrap()
    }

    fn pub_of(seed: &[u8; 32]) -> [u8; 32] {
        SigningKey::from_bytes(seed).verifying_key().to_bytes()
    }

    /// The request both sides seal. Non-ASCII text and nested parameters on purpose.
    pub(crate) fn interop_request() -> Value {
        json!({
            "model": "z-ai/glm-5.3-flash", "stream": true, "temperature": 0.25,
            "messages": [
                {"role": "system", "content": "Answer in one line."},
                {"role": "user", "name": "alice", "content": "Wie geht's? 你好 🙂"},
                {"role": "assistant", "content": null, "tool_calls": [{"id": "call_1", "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"Zürich\",\"unit\":\"c\"}"}}]},
                {"role": "tool", "tool_call_id": "call_1", "content": "12 degrees"}
            ],
            "tools": [{"type": "function", "function": {"name": "get_weather",
                "description": "Current weather for a city",
                "parameters": {"type": "object", "properties": {"city": {"type": "string"},
                    "unit": {"enum": ["c", "f"], "type": "string"}}, "required": ["city"]}}}]
        })
    }

    /// The answer both sides open: every field the gateway encrypts.
    fn interop_answer_plain() -> Value {
        json!({"choices": [{"message": {"role": "assistant",
            "content": "It is 12 degrees. 🙂", "reasoning_content": "check the tool result",
            "tool_calls": [{"id": "call_2", "type": "function", "function":
                {"name": "get_weather", "arguments": "{\"city\":\"Bern\"}"}}]}}],
            "usage": {"prompt_tokens": 3}})
    }

    fn seal_answer(to: &[u8; 32]) -> Value {
        let e = |s: &str| json!(encrypt(to, s.as_bytes()).unwrap());
        json!({"choices": [{"message": {"role": "assistant",
            "content": e("It is 12 degrees. 🙂"), "reasoning_content": e("check the tool result"),
            "tool_calls": [{"id": "call_2", "type": "function", "function":
                {"name": e("get_weather"), "arguments": e("{\"city\":\"Bern\"}")}}]}}],
            "usage": {"prompt_tokens": 3}})
    }

    fn sse_chunk(to: &[u8; 32]) -> String {
        let e = encrypt(to, "Pa ris".as_bytes()).unwrap();
        format!("data: {}", json!({"choices": [{"delta": {"content": e}}]}))
    }

    fn recorded_seal() -> Value {
        let ct = encrypt_with(
            &recorded_model_pub(),
            RECORDED_TEXT.as_bytes(),
            &RECORDED_EPH,
            &RECORDED_NONCE,
        )
        .unwrap();
        json!({"model_pub": hex::encode(recorded_model_pub()), "plaintext": RECORDED_TEXT,
               "eph_secret": hex::encode(RECORDED_EPH), "nonce": hex::encode(RECORDED_NONCE),
               "ciphertext": ct})
    }

    fn rust_fixture() -> Value {
        let model_pub = pub_of(&MODEL_SEED);
        let client = ClientKeys::from_seed(&CLIENT_SEED);
        let client_pub: [u8; 32] = hex::decode(client.public_hex())
            .unwrap()
            .try_into()
            .unwrap();
        FIXED.with(|c| c.set(Some(1)));
        let request = interop_request();
        let sealed_request = seal_request(&request, &model_pub, Some(256)).unwrap();
        let answer = seal_answer(&client_pub);
        let sse = sse_chunk(&client_pub);
        FIXED.with(|c| c.set(None));
        let headers: Map<String, Value> = request_headers(&client.public_hex(), &model_pub)
            .into_iter()
            .map(|(k, v)| (k.to_string(), Value::String(v)))
            .collect();
        json!({
            "model_seed": hex::encode(MODEL_SEED), "model_pub": hex::encode(model_pub),
            "client_seed": hex::encode(CLIENT_SEED), "client_pub": client.public_hex(),
            "request": request, "max_tokens": 256, "sealed_request": sealed_request,
            "headers": headers,
            "answer_plain": interop_answer_plain(), "answer": answer,
            "sse_plain_content": "Pa ris", "sse_line": sse,
            "recorded": recorded_seal(),
        })
    }

    fn write_or_compare(name: &str, fresh: &Value) {
        let path = fixture_path(name);
        let text = serde_json::to_string_pretty(fresh).unwrap() + "\n";
        if std::env::var_os("NYMIRA_BLESS_FIXTURES").is_some() {
            std::fs::write(&path, &text).unwrap();
        }
        let have = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(
            have, text,
            "{name} is stale: rerun with NYMIRA_BLESS_FIXTURES=1 and commit it"
        );
    }

    #[test]
    fn the_rust_interop_fixture_regenerates_byte_for_byte() {
        write_or_compare("e2ee-rust.json", &rust_fixture());
    }

    /// What the TS side committed (`web/test/e2ee-interop.test.ts` regenerates it): Rust opens it.
    #[test]
    fn rust_opens_what_the_web_code_sealed() {
        let f: Value = serde_json::from_slice(
            &std::fs::read(fixture_path("e2ee-ts.json")).expect("e2ee-ts.json is committed"),
        )
        .unwrap();
        let model = ClientKeys::from_seed(&MODEL_SEED);
        let client = ClientKeys::from_seed(&CLIENT_SEED);
        assert_eq!(f["model_pub"], hex::encode(pub_of(&MODEL_SEED)));
        assert_eq!(f["client_pub"], client.public_hex());
        let open = |v: &Value| decrypt(&model, v.as_str().unwrap()).unwrap();
        let plain = interop_request();
        assert_eq!(f["request"], plain);

        let sealed = &f["sealed_request"];
        assert_eq!(sealed["max_tokens"], 256);
        assert_eq!(sealed["stream_options"], json!({"include_usage": true}));
        assert_eq!(sealed["model"], plain["model"]);
        for (i, m) in plain["messages"].as_array().unwrap().iter().enumerate() {
            let s = &sealed["messages"][i];
            assert_eq!(s["role"], m["role"]);
            assert_eq!(s["tool_call_id"], m["tool_call_id"]);
            match m["content"].as_str() {
                Some(c) => assert_eq!(open(&s["content"]), c, "content {i}"),
                None => assert!(s["content"].is_null()),
            }
            if let Some(name) = m["name"].as_str() {
                assert_eq!(open(&s["name"]), name);
            }
            for (j, call) in m["tool_calls"].as_array().into_iter().flatten().enumerate() {
                let sc = &s["tool_calls"][j]["function"];
                assert_eq!(
                    open(&sc["name"]),
                    call["function"]["name"].as_str().unwrap()
                );
                assert_eq!(
                    open(&sc["arguments"]),
                    call["function"]["arguments"].as_str().unwrap()
                );
            }
        }
        let tool = &sealed["tools"][0]["function"];
        let want = &plain["tools"][0]["function"];
        assert_eq!(open(&tool["name"]), want["name"].as_str().unwrap());
        assert_eq!(
            open(&tool["description"]),
            want["description"].as_str().unwrap()
        );
        // The gateway reads parameters as an encrypted JSON string.
        let params: Value = serde_json::from_str(&open(&tool["parameters"])).unwrap();
        assert_eq!(params, want["parameters"]);
        for text in ["Zürich", "alice", "get_weather", "12 degrees", "你好"] {
            assert!(!sealed.to_string().contains(text), "{text} leaked");
        }

        // The answer, as the gateway would encrypt it to the client key.
        let mut answer = f["answer"].clone();
        open_response(&client, &mut answer).unwrap();
        assert_eq!(answer, interop_answer_plain());
        let line = open_sse_line(&client, f["sse_line"].as_str().unwrap()).unwrap();
        assert!(line.contains("\"Pa ris\""), "{line}");

        // Sealed to the model key NEAR AI attested: the same bytes as Rust makes.
        assert_eq!(f["recorded"], recorded_seal());

        // The request headers are what the Rust gateway sends.
        let headers: Map<String, Value> =
            request_headers(&client.public_hex(), &pub_of(&MODEL_SEED))
                .into_iter()
                .map(|(k, v)| (k.to_string(), Value::String(v)))
                .collect();
        assert_eq!(f["headers"], Value::Object(headers));
    }

    #[test]
    fn the_recorded_near_ai_model_key_is_a_valid_ed25519_key() {
        assert!(encrypt(&recorded_model_pub(), b"x").is_ok());
    }
}
