//! Two checks the chat runs in the browser that need Rust code Nymira already has:
//!
//! - the operator's signed status nonce, verified with the vault's state key (the zkapi compact
//!   Schnorr scheme), with the message built exactly as `nymira-operator` builds it;
//! - Intel TDX quote verification (`dcap-qvl`, the same crate `nymirad` verifies with): the
//!   quote's signature chain to Intel's root, the collateral, and the TCB status.
//!
//! The plain functions are the API and are tested natively; `wasm` wraps them for JavaScript.

use serde::Serialize;
use sha2::{Digest, Sha256};
use zkapi_core::v2::{domain_value, hash_felts};
use zkapi_proof::compact::verify_signature;
use zkapi_types::wire::CurvePointWire;
use zkapi_types::{Felt252, SchnorrSignature};

/// Domain tag of the status nonce signature (`nymira-operator`'s `STATUS_DOMAIN`).
const STATUS_DOMAIN: &[u8] = b"nymira/status/v1";

fn small_felt(bytes: &[u8]) -> Felt252 {
    // At most 31 bytes: always below the field modulus.
    let mut word = [0u8; 32];
    word[32 - bytes.len()..].copy_from_slice(bytes);
    Felt252(word)
}

/// The signed message: `Poseidon(domain, nonce, H(host), ts)`.
pub fn status_message(nonce: &str, host: &str, ts: u64) -> Felt252 {
    let nonce_bytes = hex::decode(nonce).unwrap_or_default();
    let host_hash = Sha256::digest(host.as_bytes());
    hash_felts(&[
        domain_value(STATUS_DOMAIN),
        small_felt(&nonce_bytes[..nonce_bytes.len().min(16)]),
        small_felt(&host_hash[..31]),
        Felt252::from_u64(ts),
    ])
}

/// Whether `signature_json` (`{"r_x":..}` as the operator prints it) signs the status slot under
/// the pinned key `(x, y)` (hex field elements).
pub fn status_signature_valid(
    x: &str,
    y: &str,
    nonce: &str,
    host: &str,
    ts: u64,
    signature_json: &str,
) -> Result<bool, String> {
    let pk = CurvePointWire {
        x: Felt252::from_hex(x).map_err(|e| e.to_string())?,
        y: Felt252::from_hex(y).map_err(|e| e.to_string())?,
    };
    let sig: SchnorrSignature = serde_json::from_str(signature_json).map_err(|e| e.to_string())?;
    verify_signature(&pk, &status_message(nonce, host, ts), &sig).map_err(|e| e.to_string())
}

/// What a verified quote carries; values are lower-case hex.
#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct QuoteFacts {
    pub status: String,
    pub advisory_ids: Vec<String>,
    pub debug: bool,
    pub mr_td: String,
    pub rtmr0: String,
    pub rtmr1: String,
    pub rtmr2: String,
    pub rtmr3: String,
    pub mr_config_id: String,
    pub report_data: String,
}

/// Verifies `quote` against `collateral_json` (a serialized `QuoteCollateralV3`) at `now_secs`.
pub fn verify_tdx_quote(
    quote: &[u8],
    collateral_json: &str,
    now_secs: u64,
) -> Result<QuoteFacts, String> {
    let collateral: dcap_qvl::QuoteCollateralV3 =
        serde_json::from_str(collateral_json).map_err(|e| format!("collateral: {e}"))?;
    let verified =
        dcap_qvl::verify::verify(quote, &collateral, now_secs).map_err(|e| format!("{e:#}"))?;
    let td = verified
        .report
        .as_td10()
        .ok_or("the quote is not a TDX quote")?;
    Ok(QuoteFacts {
        status: verified.status.clone(),
        advisory_ids: verified.advisory_ids.clone(),
        debug: td.td_attributes[0] & 1 != 0,
        mr_td: hex::encode(td.mr_td),
        rtmr0: hex::encode(td.rt_mr0),
        rtmr1: hex::encode(td.rt_mr1),
        rtmr2: hex::encode(td.rt_mr2),
        rtmr3: hex::encode(td.rt_mr3),
        mr_config_id: hex::encode(td.mr_config_id),
        report_data: hex::encode(td.report_data),
    })
}

/// What the browser tests need to play an operator: its keys and the responses it signs.
#[cfg(feature = "test-operator")]
pub mod test_operator {
    use super::*;
    use zkapi_core::v2::{next_anchor, state_message};
    use zkapi_proof::compact::{random_scalar, server_update, CompactSigner};
    use zkapi_types::wire::{ApiRequestV2, CurvePointWire, RequestResponseV2};
    use zkapi_types::{canonical_response_hash, PROTOCOL_VERSION};

    fn signer(seed: u64) -> CompactSigner {
        CompactSigner::from_seed(&Felt252::from_u64(seed))
    }

    /// `{"x","y"}` of the key made from `seed`.
    pub fn public_key(seed: u64) -> String {
        serde_json::to_string(&signer(seed).public_key()).expect("key serialises")
    }

    pub fn sign_status(seed: u64, nonce: &str, host: &str, ts: u64) -> String {
        let sig = signer(seed).sign(&status_message(nonce, host, ts));
        serde_json::to_string(&sig).expect("signature serialises")
    }

    /// The finalized response an operator with state key `seed` returns for `request_json`.
    pub fn respond(
        seed: u64,
        request_json: &str,
        chain_id: u64,
        contract_hex: &str,
        charge: u128,
    ) -> Result<String, String> {
        let request: ApiRequestV2 =
            serde_json::from_str(request_json).map_err(|e| e.to_string())?;
        let inputs = &request.public_inputs;
        let anonymous = CurvePointWire {
            x: inputs.anonymous_commitment_x,
            y: inputs.anonymous_commitment_y,
        };
        let delta = random_scalar();
        let next = server_update(&anonymous, charge, &delta).map_err(|e| e.to_string())?;
        let anchor = next_anchor(&inputs.request_nullifier);
        let contract = Felt252::from_hex(contract_hex).map_err(|e| e.to_string())?;
        let message = state_message(
            PROTOCOL_VERSION,
            chain_id,
            &contract,
            &next.x,
            &next.y,
            &anchor,
        );
        let payload = "{}".to_string();
        let response = RequestResponseV2 {
            status: "finalized".into(),
            client_request_id: request.client_request_id.clone(),
            request_nullifier: inputs.request_nullifier,
            response_code: 200,
            response_hash: canonical_response_hash(payload.as_bytes()),
            response_payload: payload,
            charge_applied: charge,
            next_commitment: next,
            next_anchor: anchor,
            blind_delta_srv: delta,
            next_state_signature: signer(seed).sign(&message),
            policy_reason_code: None,
            policy_evidence_hash: None,
        };
        serde_json::to_string(&response).map_err(|e| e.to_string())
    }
}

mod wasm {
    use wasm_bindgen::prelude::*;

    #[cfg(feature = "test-operator")]
    #[wasm_bindgen]
    pub fn test_operator_public_key(seed: u64) -> String {
        super::test_operator::public_key(seed)
    }

    #[cfg(feature = "test-operator")]
    #[wasm_bindgen]
    pub fn test_operator_sign_status(seed: u64, nonce: &str, host: &str, ts: u64) -> String {
        super::test_operator::sign_status(seed, nonce, host, ts)
    }

    #[cfg(feature = "test-operator")]
    #[wasm_bindgen]
    pub fn test_operator_respond(
        seed: u64,
        request_json: &str,
        chain_id: u64,
        contract_hex: &str,
        charge: u128,
    ) -> Result<String, JsValue> {
        super::test_operator::respond(seed, request_json, chain_id, contract_hex, charge)
            .map_err(|e| JsValue::from_str(&e))
    }

    #[wasm_bindgen]
    pub fn status_signature_valid(
        x: &str,
        y: &str,
        nonce: &str,
        host: &str,
        ts: u64,
        signature_json: &str,
    ) -> Result<bool, JsValue> {
        super::status_signature_valid(x, y, nonce, host, ts, signature_json)
            .map_err(|e| JsValue::from_str(&e))
    }

    /// `QuoteFacts` as JSON text.
    #[wasm_bindgen]
    pub fn verify_tdx_quote(
        quote: &[u8],
        collateral_json: &str,
        now_secs: u64,
    ) -> Result<String, JsValue> {
        let facts = super::verify_tdx_quote(quote, collateral_json, now_secs)
            .map_err(|e| JsValue::from_str(&e))?;
        serde_json::to_string(&facts).map_err(|e| JsValue::from_str(&e.to_string()))
    }

    /// Collateral for `quote` from a PCCS, as `QuoteCollateralV3` JSON text.
    #[wasm_bindgen]
    pub async fn fetch_collateral(pccs_url: String, quote: Vec<u8>) -> Result<String, JsValue> {
        let collateral = dcap_qvl::collateral::get_collateral(&pccs_url, &quote)
            .await
            .map_err(|e| JsValue::from_str(&format!("{e:#}")))?;
        serde_json::to_string(&collateral).map_err(|e| JsValue::from_str(&e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zkapi_proof::compact::CompactSigner;

    #[test]
    fn a_status_nonce_signed_by_the_state_key_verifies_and_a_changed_host_does_not() {
        let signer = CompactSigner::from_seed(&Felt252::from_u64(11));
        let pk = signer.public_key();
        let sig =
            serde_json::to_string(&signer.sign(&status_message("aabb", "op.test", 7))).unwrap();
        let (x, y) = (pk.x.to_string(), pk.y.to_string());
        assert_eq!(
            status_signature_valid(&x, &y, "aabb", "op.test", 7, &sig),
            Ok(true)
        );
        assert_eq!(
            status_signature_valid(&x, &y, "aabb", "evil.test", 7, &sig),
            Ok(false)
        );
    }
}
