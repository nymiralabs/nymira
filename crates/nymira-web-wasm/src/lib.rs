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

/// The 16 public input words of a withdrawal statement, in circuit order, each 32 big-endian
/// bytes as 64 hex digits. This is what `mutual_close`, `request_exit` and `POST /v1/close` carry
/// as `inputs` (base64 of each word), built by the same call `nymirad` uses.
pub fn withdrawal_words(public_json: &str) -> Result<Vec<String>, String> {
    let public: zkapi_types::WithdrawalPublicInputsV2C =
        serde_json::from_str(public_json).map_err(|e| e.to_string())?;
    Ok(public
        .to_field_elements()
        .iter()
        .map(|word| hex::encode(word.as_bytes()))
        .collect())
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

    /// The clearance an operator whose clearance key is made from `seed` returns for the
    /// withdrawal nullifier `nullifier_hex` (`ClearanceResponseV2` as JSON).
    pub fn sign_clearance(
        seed: u64,
        nullifier_hex: &str,
        chain_id: u64,
        contract_hex: &str,
    ) -> Result<String, String> {
        let nullifier = Felt252::from_hex(nullifier_hex)?;
        let contract = Felt252::from_hex(contract_hex)?;
        let message =
            zkapi_core::v2::clearance_message(PROTOCOL_VERSION, chain_id, &contract, &nullifier);
        serde_json::to_string(&zkapi_types::wire::ClearanceResponseV2 {
            status: "signed".into(),
            withdrawal_nullifier: nullifier,
            signature: signer(seed).sign(&message),
        })
        .map_err(|e| e.to_string())
    }

    fn proof_wire(proof_b64: &str) -> zkapi_types::wire::Groth16ProofWire {
        zkapi_types::wire::Groth16ProofWire {
            backend: zkapi_types::wire::ProofBackendWire::Groth16Bn254,
            proof: proof_b64.to_string(),
        }
    }

    /// Verifies a withdrawal proof against its public inputs (what the operator does for an
    /// ownership proof) under the withdrawal verifying key file `vk`.
    pub fn verify_withdrawal(
        vk: &[u8],
        public_json: &str,
        proof_b64: &str,
    ) -> Result<bool, String> {
        let public: zkapi_types::WithdrawalPublicInputsV2C =
            serde_json::from_str(public_json).map_err(|e| e.to_string())?;
        zkapi_proof::compact::WithdrawalVerifier::from_bytes(vk)
            .and_then(|v| v.verify(&public, &proof_wire(proof_b64)))
            .map_err(|e| format!("{e:#}"))
    }

    /// Verifies a withdrawal proof against raw statement words (64 hex digits each), as the vault
    /// does with the `inputs` of a call.
    pub fn verify_withdrawal_words(
        vk: &[u8],
        words_hex: &[String],
        proof_b64: &str,
    ) -> Result<bool, String> {
        let words = words_hex
            .iter()
            .map(|w| Felt252::from_hex(w))
            .collect::<Result<Vec<_>, _>>()?;
        zkapi_proof::compact::WithdrawalVerifier::from_bytes(vk)
            .and_then(|v| v.verify_field_elements(&words, &proof_wire(proof_b64)))
            .map_err(|e| format!("{e:#}"))
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

    #[cfg(feature = "test-operator")]
    #[wasm_bindgen]
    pub fn test_operator_sign_clearance(
        seed: u64,
        nullifier_hex: &str,
        chain_id: u64,
        contract_hex: &str,
    ) -> Result<String, JsValue> {
        super::test_operator::sign_clearance(seed, nullifier_hex, chain_id, contract_hex)
            .map_err(|e| JsValue::from_str(&e))
    }

    #[cfg(feature = "test-operator")]
    #[wasm_bindgen]
    pub fn test_operator_verify_withdrawal(
        vk: &[u8],
        public_json: &str,
        proof_b64: &str,
    ) -> Result<bool, JsValue> {
        super::test_operator::verify_withdrawal(vk, public_json, proof_b64)
            .map_err(|e| JsValue::from_str(&e))
    }

    /// `words_json` is the JSON array `withdrawal_words` returns.
    #[cfg(feature = "test-operator")]
    #[wasm_bindgen]
    pub fn test_operator_verify_withdrawal_words(
        vk: &[u8],
        words_json: &str,
        proof_b64: &str,
    ) -> Result<bool, JsValue> {
        let words: Vec<String> =
            serde_json::from_str(words_json).map_err(|e| JsValue::from_str(&e.to_string()))?;
        super::test_operator::verify_withdrawal_words(vk, &words, proof_b64)
            .map_err(|e| JsValue::from_str(&e))
    }

    /// The statement words of a withdrawal as a JSON array of 64-digit hex strings.
    #[wasm_bindgen]
    pub fn withdrawal_words(public_json: &str) -> Result<String, JsValue> {
        let words = super::withdrawal_words(public_json).map_err(|e| JsValue::from_str(&e))?;
        serde_json::to_string(&words).map_err(|e| JsValue::from_str(&e.to_string()))
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
    fn withdrawal_words_follow_the_circuit_order() {
        let felt = |n: u64| Felt252::from_u64(n).to_hex();
        let json = format!(
            r#"{{"protocol_version":2,"chain_id":7718972759422244814,"contract_address":"{}","note_leaf":"{}","state_signing_key_x":"{}","state_signing_key_y":"{}","clearance_signing_key_x":"{}","clearance_signing_key_y":"{}","note_id":3,"deposit_amount":1000000,"expiry_ts":1900000000,"final_balance":900000,"destination":[0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,9],"withdrawal_nullifier":"{}","kind":1,"next_nullifier":"{}","conceded":0,"valid_until":1800010800}}"#,
            felt(1),
            felt(2),
            felt(3),
            felt(4),
            felt(5),
            felt(6),
            felt(7),
            felt(0)
        );
        let words = withdrawal_words(&json).unwrap();
        assert_eq!(words.len(), 16);
        assert!(words.iter().all(|w| w.len() == 64));
        let n = |i: usize| u128::from_str_radix(&words[i][32..], 16).unwrap();
        assert_eq!(n(0), 2); // protocol version
        assert_eq!(words[1], format!("{:064x}", 7_718_972_759_422_244_814u64));
        assert_eq!(n(2), 1); // contract
        assert_eq!(n(3), 2); // note leaf
        assert_eq!(n(9), 900_000); // final balance
        assert_eq!(n(10), 9); // destination, left padded
        assert_eq!(n(11), 7); // nullifier
        assert_eq!(n(12), 1); // kind
        assert_eq!(n(13), 0); // next nullifier
        assert_eq!(n(15), 1_800_010_800); // valid until
        assert!(withdrawal_words("{}").is_err());
    }

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
