//! Signed policies for tests: NEP-413 signing as a wallet does it, and the shortcut that puts a
//! policy in force without the network checks (the key check has its own tests).

#![allow(dead_code)]

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use nymirad::agents::Registry;
use nymirad::policy::{self, Context};
use serde_json::json;

pub const OWNER: &str = "owner.test";

pub fn ctx() -> Context {
    Context::new(
        nymira_operator::proxy::ProxyConfig::default()
            .models
            .into_iter()
            .map(|m| m.id)
            .collect(),
        &[100_000, 500_000],
    )
}

pub fn yaml(models: &[&str]) -> String {
    format!(
        "allowed_models:\n{}max_lease_usd: 0.5\nmax_call_usd: 0.05\nmax_daily_usd: 2\nconfirm_above_usd: 0.03\nrate_limit_per_min: 30\n",
        models.iter().map(|m| format!("  - {m}\n")).collect::<String>()
    )
}

pub fn public_key(key: &SigningKey) -> String {
    format!(
        "ed25519:{}",
        bs58::encode(key.verifying_key().as_bytes()).into_string()
    )
}

/// What a wallet returns for `signMessage`, as the file `agent policy` reads.
pub fn sign_for(
    key: &SigningKey,
    account: &str,
    recipient: &str,
    version: u32,
    policy_text: &str,
) -> serde_json::Value {
    let message =
        json!({"kind": "nymira-policy", "version": version, "policy": policy_text}).to_string();
    let nonce = [9u8; 32];
    let hash = policy::nep413_hash(&message, &nonce, recipient);
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    json!({
        "account_id": account,
        "public_key": public_key(key),
        "signature": b64(&key.sign(&hash).to_bytes()),
        "nonce": b64(&nonce),
        "message": message,
        "recipient": recipient,
    })
}

/// A policy for the named agent as its owner would sign it.
pub fn signed(
    registry: &Registry,
    name: &str,
    key: &SigningKey,
    version: u32,
    policy_text: &str,
) -> Vec<u8> {
    let book = registry.load().unwrap();
    let a = book.agents.iter().find(|a| a.name == name).unwrap();
    serde_json::to_vec(&sign_for(
        key,
        &a.owner_account,
        &policy::recipient(&a.name, &a.note),
        version,
        policy_text,
    ))
    .unwrap()
}

/// Puts a policy in force for `name`, allowing `models`, without the key check.
pub fn give_policy_for(registry: &Registry, name: &str, version: u32, models: &[&str]) {
    let key = SigningKey::from_bytes(&[5u8; 32]);
    let bytes = signed(registry, name, &key, version, &yaml(models));
    policy::activate(registry, name, &bytes, &ctx()).unwrap();
}

pub fn give_policy(registry: &Registry, name: &str, version: u32) {
    give_policy_for(registry, name, version, &["z-ai/glm-5.3-flash"]);
}

/// A policy text with the limits given: models, `max_lease_usd`, `max_call_usd`,
/// `max_daily_usd` and `rate_limit_per_min`.
pub fn yaml_limits(models: &[&str], lease: &str, call: &str, daily: &str, rate: u32) -> String {
    format!(
        "allowed_models:\n{}max_lease_usd: {lease}\nmax_call_usd: {call}\nmax_daily_usd: {daily}\nconfirm_above_usd: 0.03\nrate_limit_per_min: {rate}\n",
        models.iter().map(|m| format!("  - {m}\n")).collect::<String>()
    )
}

/// Puts a policy with this exact text in force for `name`, without the key check.
pub fn give_policy_text(registry: &Registry, name: &str, version: u32, text: &str) {
    let key = SigningKey::from_bytes(&[5u8; 32]);
    let bytes = signed(registry, name, &key, version, text);
    policy::activate(registry, name, &bytes, &ctx()).unwrap();
}
