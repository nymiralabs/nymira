//! What must hold before a proof leaves the device: the operator's `/v1/status` is signed by the
//! vault's state key, its lease parameters equal the pinned ones, and the time and puzzle fields
//! are derived from it.
//!
//! Fields of the status document this build does not know are ignored.

use nymira_operator::puzzle;
use nymira_operator::status::status_message;
use serde_json::Value;
use zkapi_proof::compact::verify_signature;
use zkapi_types::wire::CurvePointWire;
use zkapi_types::{Felt252, SchnorrSignature};

/// A status nonce older than this is not used: the operator's slots are 5 seconds, its puzzle
/// window 30.
pub const MAX_STATUS_AGE_SECS: u64 = 30;
/// `request_time` of a lease proof is the operator's signed time rounded down to this grid, so it
/// does not show the client's clock. The operator accepts a time up to 330 s old; a proof is
/// stamped at most 60 s plus its proving time before it is sent.
pub const TIME_GRID_SECS: u64 = 60;

/// The lease parameters of `config()` that `/v1/status` repeats.
const PARAM_FIELDS: [&str; 4] = ["bound_classes", "charge_step", "min_charge", "slot_rate"];

#[derive(Debug, PartialEq, Eq)]
pub struct Nonce {
    pub nonce: String,
    pub host: String,
    pub ts: u64,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    #[error("the operator sent no signed nonce (its signer is unreachable)")]
    NoNonce,
    #[error("the nonce signature does not verify against the vault's state key")]
    BadSignature,
    #[error("the nonce is {0} s away from this machine's clock; check that the clock is right")]
    Stale(u64),
    #[error("the operator's status is malformed: {0}")]
    Malformed(&'static str),
    #[error("the operator's lease parameters differ from the vault's: {}", .0.join(", "))]
    Params(Vec<String>),
}

fn felt(v: &Value) -> Option<Felt252> {
    Felt252::from_hex(v.as_str()?).ok()
}

fn point(v: &Value) -> Option<CurvePointWire> {
    Some(CurvePointWire {
        x: felt(&v["x"])?,
        y: felt(&v["y"])?,
    })
}

/// Checks the nonce in `status` against the pinned state key `operator_pk` (`{x, y}` as in the
/// vault's `config()`), at operator time `now_secs`.
pub fn verify_status(status: &Value, operator_pk: &Value, now_secs: u64) -> Result<Nonce, Refusal> {
    let slot = &status["nonce"];
    if slot.is_null() || status["nonce_sig"].is_null() {
        return Err(Refusal::NoNonce);
    }
    let nonce = Nonce {
        nonce: slot["nonce"]
            .as_str()
            .ok_or(Refusal::Malformed("nonce"))?
            .to_string(),
        host: slot["host"]
            .as_str()
            .ok_or(Refusal::Malformed("host"))?
            .to_string(),
        ts: slot["ts"].as_u64().ok_or(Refusal::Malformed("ts"))?,
    };
    let sig: SchnorrSignature = serde_json::from_value(status["nonce_sig"].clone())
        .map_err(|_| Refusal::Malformed("nonce_sig"))?;
    let pk = point(operator_pk).ok_or(Refusal::Malformed("pinned operator_pk"))?;
    let message = status_message(&nonce.nonce, &nonce.host, nonce.ts);
    if !verify_signature(&pk, &message, &sig).unwrap_or(false) {
        return Err(Refusal::BadSignature);
    }
    let age = now_secs.saturating_sub(nonce.ts);
    if age > MAX_STATUS_AGE_SECS || nonce.ts > now_secs + MAX_STATUS_AGE_SECS {
        return Err(Refusal::Stale(age));
    }
    Ok(nonce)
}

/// The lease parameters in `status` against the pinned `config` fields. Both sides are compared
/// as the strings the vault and the operator print.
pub fn check_params(status: &Value, pinned_config: &Value) -> Result<(), Refusal> {
    let differing: Vec<String> = PARAM_FIELDS
        .iter()
        .filter(|f| {
            // The operator prints integers it holds as u128 as strings and `slot_rate` as a number;
            // the vault does the same. Compare through one spelling.
            let spell = |v: &Value| match v {
                Value::Array(a) => a.iter().map(|x| x.to_string().replace('"', "")).collect(),
                other => vec![other.to_string().replace('"', "")],
            };
            let (s, p): (Vec<String>, Vec<String>) =
                (spell(&status[**f]), spell(&pinned_config[**f]));
            s != p || status[**f].is_null()
        })
        .map(|f| (*f).to_string())
        .collect();
    if differing.is_empty() {
        Ok(())
    } else {
        Err(Refusal::Params(differing))
    }
}

/// `request_time` for a proof: the operator's slot time rounded down to the grid.
pub fn request_time(nonce: &Nonce) -> u64 {
    nonce.ts / TIME_GRID_SECS * TIME_GRID_SECS
}

/// The `X-Nymira-Puzzle` value when the operator asks for one.
pub fn puzzle_header(status: &Value, nonce: &Nonce) -> Option<String> {
    let difficulty = status["admission"]["puzzle_difficulty"].as_u64()?;
    (difficulty > 0).then(|| puzzle::solve(&nonce.nonce, difficulty.min(32) as u32))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use zkapi_proof::compact::CompactSigner;

    const NOW: u64 = 1_800_000_000;

    fn signer(seed: u64) -> CompactSigner {
        CompactSigner::from_seed(&Felt252::from_u64(seed))
    }

    fn pk_json(s: &CompactSigner) -> Value {
        let p = s.public_key();
        json!({"x": p.x.to_string(), "y": p.y.to_string()})
    }

    fn status(s: &CompactSigner, ts: u64) -> Value {
        let sig = s.sign(&status_message("aabb", "op.test", ts));
        json!({
            "nonce": {"nonce": "aabb", "host": "op.test", "ts": ts},
            "nonce_sig": sig,
            "bound_classes": ["100000", "500000"],
            "charge_step": "1000",
            "min_charge": "2000",
            "slot_rate": 1000,
            "future_field": {"anything": 1},
            "admission": {"puzzle_difficulty": 0},
        })
    }

    #[test]
    fn a_nonce_signed_by_the_state_key_verifies() {
        let s = signer(11);
        let n = verify_status(&status(&s, NOW - 3), &pk_json(&s), NOW).unwrap();
        assert_eq!((n.nonce.as_str(), n.host.as_str()), ("aabb", "op.test"));
    }

    #[test]
    fn a_fake_operator_with_another_key_is_refused() {
        let (real, fake) = (signer(11), signer(12));
        assert_eq!(
            verify_status(&status(&fake, NOW), &pk_json(&real), NOW),
            Err(Refusal::BadSignature)
        );
    }

    #[test]
    fn a_tampered_nonce_or_host_is_refused() {
        let s = signer(11);
        let mut st = status(&s, NOW);
        st["nonce"]["host"] = json!("evil.test");
        assert_eq!(
            verify_status(&st, &pk_json(&s), NOW),
            Err(Refusal::BadSignature)
        );
        let mut st = status(&s, NOW);
        st["nonce"]["nonce"] = json!("ccdd");
        assert_eq!(
            verify_status(&st, &pk_json(&s), NOW),
            Err(Refusal::BadSignature)
        );
    }

    #[test]
    fn a_missing_or_old_nonce_is_refused() {
        let s = signer(11);
        let mut st = status(&s, NOW);
        st["nonce"] = Value::Null;
        assert_eq!(verify_status(&st, &pk_json(&s), NOW), Err(Refusal::NoNonce));
        assert_eq!(
            verify_status(&status(&s, NOW - 31), &pk_json(&s), NOW),
            Err(Refusal::Stale(31))
        );
    }

    #[test]
    fn the_status_key_in_the_document_is_never_trusted() {
        // Only the pinned key counts, so a status that names its own key gains nothing.
        let (real, fake) = (signer(11), signer(12));
        let mut st = status(&fake, NOW);
        st["operator_pk"] = pk_json(&fake);
        assert_eq!(
            verify_status(&st, &pk_json(&real), NOW),
            Err(Refusal::BadSignature)
        );
    }

    #[test]
    fn matching_parameters_pass_and_unknown_fields_are_ignored() {
        let s = signer(11);
        let pinned = json!({
            "bound_classes": ["100000", "500000"], "charge_step": "1000",
            "min_charge": "2000", "slot_rate": 1000,
        });
        assert_eq!(check_params(&status(&s, NOW), &pinned), Ok(()));
    }

    #[test]
    fn each_changed_parameter_is_named() {
        let s = signer(11);
        let pinned = json!({
            "bound_classes": ["100000"], "charge_step": "500",
            "min_charge": "2000", "slot_rate": 2000,
        });
        assert_eq!(
            check_params(&status(&s, NOW), &pinned),
            Err(Refusal::Params(vec![
                "bound_classes".into(),
                "charge_step".into(),
                "slot_rate".into()
            ]))
        );
    }

    #[test]
    fn a_status_without_a_parameter_is_refused() {
        let pinned = json!({"bound_classes": ["100000"], "charge_step": "1000",
            "min_charge": "2000", "slot_rate": 1000});
        let st = json!({"bound_classes": ["100000"]});
        assert!(matches!(check_params(&st, &pinned), Err(Refusal::Params(f)) if f.len() == 3));
    }

    #[test]
    fn request_time_sits_on_the_grid() {
        let n = Nonce {
            nonce: "aa".into(),
            host: "h".into(),
            ts: 1_800_000_119,
        };
        assert_eq!(request_time(&n), 1_800_000_060);
        assert_eq!(request_time(&n) % TIME_GRID_SECS, 0);
    }

    #[test]
    fn the_puzzle_is_solved_only_when_asked_and_binds_to_the_slot_nonce() {
        let n = Nonce {
            nonce: "aabb".into(),
            host: "h".into(),
            ts: NOW,
        };
        assert_eq!(
            puzzle_header(&json!({"admission": {"puzzle_difficulty": 0}}), &n),
            None
        );
        let header = puzzle_header(&json!({"admission": {"puzzle_difficulty": 8}}), &n).unwrap();
        assert!(puzzle::check(&header, 8, &["aabb".to_string()]));
    }
}
