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
const FIELDS_TOP: [&str; 14] = [
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
    let x = VerifyingKey::from_bytes(model_pub)
        .map_err(|_| E2eeError::ModelKey)?
        .to_montgomery()
        .to_bytes();
    let mut seed = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let eph = StaticSecret::from(seed);
    let shared = eph.diffie_hellman(&PublicKey::from(x));
    let mut nonce = [0u8; 24];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let ct = cipher(shared.as_bytes())
        .encrypt(XNonce::from_slice(&nonce), plaintext)
        .expect("encryption does not fail");
    Ok(hex::encode(
        [PublicKey::from(&eph).as_bytes().as_slice(), &nonce, &ct].concat(),
    ))
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
}
