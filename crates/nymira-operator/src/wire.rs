//! Every wire type the operator parses from a byte string, behind one entry point.
//!
//! The fuzz targets in `fuzz/` and the corpus test in `tests/wire.rs` both call [`exercise`]:
//! any input must come back as `Ok` or a refusal, never a panic.

use std::collections::HashSet;

use crate::json::parse_strict;
use serde_json::Value;

use crate::proxy::{parse_chat, ProxyConfig};

/// A kind of input the operator reads from outside a trust boundary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wire {
    /// `POST /v2/openrouter/leases`.
    Lease,
    /// `POST /v2/withdraw/clearance`.
    Clearance,
    /// `POST /v1/close`.
    Close,
    /// `POST /v1/chat/completions`.
    Chat,
    /// `POST /v2/reconcile`.
    Reconcile,
    /// A line on the signer's socket.
    SignerCall,
    /// A line on the replication owner's socket.
    ReplicaCall,
    /// An entry sent to the replica over HTTP.
    ReplogEntry,
    /// The strict JSON reader every body goes through first.
    Json,
}

impl Wire {
    pub const ALL: [Wire; 9] = [
        Wire::Lease,
        Wire::Clearance,
        Wire::Close,
        Wire::Chat,
        Wire::Reconcile,
        Wire::SignerCall,
        Wire::ReplicaCall,
        Wire::ReplogEntry,
        Wire::Json,
    ];
}

/// Parses `bytes` as `wire` would. Returns whether it was accepted; never panics.
pub fn exercise(wire: Wire, bytes: &[u8]) -> bool {
    match wire {
        Wire::Lease => crate::operator::parse_request(bytes).is_ok(),
        Wire::Clearance => parse_strict(bytes)
            .ok()
            .and_then(|v| serde_json::from_value::<zkapi_types::wire::ClearanceRequestV2C>(v).ok())
            .is_some(),
        Wire::Close => crate::closer::decode(bytes).is_ok(),
        Wire::Chat => parse_chat(bytes, &ProxyConfig::default(), &HashSet::new()).is_ok(),
        Wire::Reconcile => parse_strict(bytes)
            .ok()
            .filter(|v| v["lease_id"].is_string())
            .is_some_and(|v| crate::operator::parse_request(v["request"].to_string().as_bytes()).is_ok()),
        Wire::SignerCall => std::str::from_utf8(bytes).is_ok_and(crate::signer::is_typed_request),
        Wire::ReplicaCall => {
            std::str::from_utf8(bytes).is_ok_and(crate::replica_socket::is_typed_request)
        }
        Wire::ReplogEntry => serde_json::from_slice::<crate::replog::Entry>(bytes).is_ok(),
        Wire::Json => parse_strict(bytes).is_ok(),
    }
}

/// Independent reading of a body: what a reference parser decides, with its own duplicate-key
/// detector over the raw text.
pub fn reference_accepts(raw: &str, cfg: &ProxyConfig) -> bool {
    fn has_duplicate_keys(v: &str) -> bool {
        // A second, independent detector: scan objects with a tiny tokenizer.
        let b = v.as_bytes();
        let (mut i, mut stack): (usize, Vec<Vec<String>>) = (0, vec![]);
        while i < b.len() {
            match b[i] {
                b'{' => stack.push(vec![]),
                b'}' => {
                    stack.pop();
                }
                b'"' => {
                    let start = i + 1;
                    i += 1;
                    while i < b.len() && b[i] != b'"' {
                        if b[i] == b'\\' {
                            i += 1;
                        }
                        i += 1;
                    }
                    let token = v[start..i.min(b.len())].to_string();
                    let mut j = i + 1;
                    while j < b.len() && b[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < b.len() && b[j] == b':' {
                        if let Some(keys) = stack.last_mut() {
                            if keys.contains(&token) {
                                return true;
                            }
                            keys.push(token);
                        }
                    }
                }
                _ => {}
            }
            i += 1;
        }
        false
    }
    if has_duplicate_keys(raw) {
        return false;
    }
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return false;
    };
    let known = [
        "model",
        "messages",
        "stream",
        "stream_options",
        "max_tokens",
        "max_completion_tokens",
        "n",
    ];
    v.as_object().is_some_and(|o| {
        o.keys().all(|k| known.contains(&k.as_str()))
            && v["model"]
                .as_str()
                .is_some_and(|m| cfg.models.iter().any(|p| p.id == m))
            && v["messages"].as_array().is_some_and(|m| {
                !m.is_empty()
                    && m.iter().all(|x| {
                        x.as_object().is_some_and(|o| {
                            o.keys().all(|k| {
                                ["role", "content", "name", "tool_calls", "tool_call_id"]
                                    .contains(&k.as_str())
                            })
                        }) && x["role"].is_string()
                            && (x["content"].is_string() || x["content"].is_null())
                    })
            })
            && (v.get("n").is_none() || v["n"] == 1)
            && v.get("stream").is_none_or(Value::is_boolean)
            && v.get("stream_options").is_none_or(|s| {
                s.as_object()
                    .is_some_and(|o| o.keys().all(|k| k == "include_usage"))
            })
            && ["max_tokens", "max_completion_tokens"]
                .iter()
                .all(|k| v.get(*k).is_none_or(|n| n.as_u64().is_some_and(|n| n > 0)))
            && !(v.get("max_tokens").is_some() && v.get("max_completion_tokens").is_some())
    })
}

/// Whether the chat parser and the reference reader decide alike on `bytes`. The fuzz target
/// for the chat body asserts this; so does the corpus test.
pub fn chat_agrees(bytes: &[u8]) -> bool {
    let Ok(raw) = std::str::from_utf8(bytes) else {
        return parse_chat(bytes, &ProxyConfig::default(), &HashSet::new()).is_err();
    };
    // The reference reads the seven fields a chat is built from; a body with another key (which
    // the parser judges by rules of its own) is checked for panics only.
    let modelled = [
        "model",
        "messages",
        "stream",
        "stream_options",
        "max_tokens",
        "max_completion_tokens",
        "n",
    ];
    if serde_json::from_str::<Value>(raw)
        .ok()
        .and_then(|v| v.as_object().map(|o| o.keys().any(|k| !modelled.contains(&k.as_str()))))
        .unwrap_or(false)
    {
        return true;
    }
    let cfg = ProxyConfig::default();
    parse_chat(bytes, &cfg, &HashSet::new()).is_ok() == reference_accepts(raw, &cfg)
}
