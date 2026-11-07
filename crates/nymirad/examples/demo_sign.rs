//! The demo's test owner key. `make demo` writes `funding.key`, a full-access key of the demo
//! account; this tool signs a budget note's policy with it, as the owner's wallet would, so the
//! recipes run against the demo with no wallet. It is an example with `required-features =
//! ["mock"]`: a release build of `nymirad` has neither this tool nor any key.
//!
//!   demo_sign HOME AGENT [VERSION]    writes HOME/AGENT-policy.signed.json for `nymirad agent policy`

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};

/// The policy the recipes run under: the pinned test model, small limits, no approval prompts.
pub const DEMO_POLICY: &str = "allowed_models:\n  - z-ai/glm-5.3-flash\nmax_lease_usd: 0.5\nmax_call_usd: 0.05\nmax_daily_usd: 2\nconfirm_above_usd: 1\nrate_limit_per_min: 60\nrequire_tee: false\n";

/// What a wallet returns for `signMessage`, as the file `agent policy` reads.
pub fn sign(key: &SigningKey, account: &str, agent: &str, note: &str, version: u32) -> Value {
    let message =
        json!({"kind": "nymira-policy", "version": version, "policy": DEMO_POLICY}).to_string();
    let recipient = nymirad::policy::recipient(agent, note);
    let nonce: [u8; 32] = rand::random();
    let hash = nymirad::policy::nep413_hash(&message, &nonce, &recipient);
    let b64 = |b: &[u8]| base64::engine::general_purpose::STANDARD.encode(b);
    json!({
        "account_id": account,
        "public_key": format!("ed25519:{}", bs58::encode(key.verifying_key().as_bytes()).into_string()),
        "signature": b64(&key.sign(&hash).to_bytes()),
        "nonce": b64(&nonce),
        "message": message,
        "recipient": recipient,
    })
}

#[allow(dead_code)]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (home, agent, version) = match args.as_slice() {
        [h, a] => (h, a, 1),
        [h, a, v] => (h, a, v.parse().expect("VERSION is a number")),
        _ => {
            eprintln!("usage: demo_sign HOME AGENT [VERSION]");
            std::process::exit(2);
        }
    };
    let home = std::path::Path::new(home);
    let seed: [u8; 32] = hex::decode(
        std::fs::read_to_string(home.join("funding.key"))
            .expect("HOME/funding.key (written by `make demo`)")
            .trim(),
    )
    .ok()
    .and_then(|b| b.try_into().ok())
    .expect("funding.key holds 32 bytes in hex");
    let book = nymirad::agents::Registry::at(home)
        .load()
        .expect("agents.json");
    let entry = book
        .agents
        .iter()
        .find(|a| &a.name == agent)
        .unwrap_or_else(|| panic!("no agent named {agent}"));
    let signed = sign(
        &SigningKey::from_bytes(&seed),
        &entry.owner_account,
        &entry.name,
        &entry.note,
        version,
    );
    let out = home.join(format!("{agent}-policy.signed.json"));
    std::fs::write(&out, serde_json::to_vec_pretty(&signed).unwrap()).unwrap();
    println!("wrote {}", out.display());
}
