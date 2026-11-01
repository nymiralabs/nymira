//! Prints the JSON arguments of the vault's `new()` for a development-keys build, for the
//! deploy runbook: `cargo run -q -p nymira-testkit --example vault_new_args -- <token> <treasury>
//! <owner> <operator_account>`.
//!
//! The keys are zkapi's development keys (public seeds): a vault made from them is for
//! measurements only and must never hold anything of value.

use base64::Engine;
use nymira_testkit::proofs::{canonical_verifying_key, signing_key, Kind};
use serde_json::json;

fn word(bytes: [u8; 32]) -> String {
    let digits: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("0x{digits}")
}

fn point(seed: u64) -> serde_json::Value {
    let key = signing_key(seed);
    json!({
        "x": word(key.x.0),
        "y": word(key.y.0),
    })
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [token, treasury, owner, operator] = args.as_slice() else {
        eprintln!("usage: vault_new_args <token> <treasury> <owner> <operator_account>");
        std::process::exit(2);
    };
    let b64 = |bytes: Vec<u8>| base64::engine::general_purpose::STANDARD.encode(bytes);
    println!(
        "{}",
        json!({
            "operator_pk": point(1),
            "clearance_pk": point(2),
            "token": token,
            "denominations": ["1000000", "5000000"],
            "tvl_cap": "100000000",
            "request_vk": b64(canonical_verifying_key(Kind::Request)),
            "withdrawal_vk": b64(canonical_verifying_key(Kind::Withdrawal)),
            "treasury": treasury,
            "note_fee": "50000",
            "exit_delay_secs": 7 * 86_400,
            "note_ttl": 30 * 86_400,
            "bound_classes": ["100000", "500000"],
            "charge_step": "1000",
            "min_charge": "2000",
            "slot_rate": 1000,
            "owner": owner,
            "operator_account": operator,
        })
    );
}
