//! Prints the JSON arguments of the vault's `new()` for the deploy runbook:
//! `cargo run -q -p nymira-testkit --example vault_new_args -- <token> <treasury> <owner>
//! <operator_account> [--pubkeys-file FILE]`.
//!
//! Without options the verifying keys are zkapi's development keys and the operator and
//! clearance keys are the public dev seeds: a vault made from them is for measurements only and
//! must never hold anything of value, and a release build refuses it. For a release vault set
//! `NYMIRA_SETUP_DIR` to the ceremony's setup directory and pass the JSON that
//! `nymira-operatord pubkeys` printed as `--pubkeys-file`.

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
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let pubkeys: Option<serde_json::Value> =
        args.iter().position(|a| a == "--pubkeys-file").map(|at| {
            args.remove(at);
            let path = if at < args.len() {
                args.remove(at)
            } else {
                eprintln!("--pubkeys-file needs a file");
                std::process::exit(2)
            };
            let text = std::fs::read_to_string(&path).unwrap_or_else(|e| {
                eprintln!("{path}: {e}");
                std::process::exit(2)
            });
            serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!("{path}: {e}");
                std::process::exit(2)
            })
        });
    let [token, treasury, owner, operator] = args.as_slice() else {
        eprintln!(
            "usage: vault_new_args <token> <treasury> <owner> <operator_account> [--pubkeys-file FILE]"
        );
        std::process::exit(2);
    };
    if pubkeys.is_none() {
        eprintln!("warning: the operator and clearance keys are the public development seeds");
    }
    if std::env::var_os("NYMIRA_SETUP_DIR").is_none() {
        eprintln!("warning: the verifying keys are zkapi's development keys");
    }
    let (operator_pk, clearance_pk) = match &pubkeys {
        Some(v) => (v["operator_pk"].clone(), v["clearance_pk"].clone()),
        None => (point(1), point(2)),
    };
    let b64 = |bytes: Vec<u8>| base64::engine::general_purpose::STANDARD.encode(bytes);
    println!(
        "{}",
        json!({
            "operator_pk": operator_pk,
            "clearance_pk": clearance_pk,
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
