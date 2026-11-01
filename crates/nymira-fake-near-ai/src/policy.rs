//! The test policy and a reference verifier for the fake's attestation and signatures.
//!
//! Pin [`TestPolicy::fake`] (or the JSON from `GET /control/policy`) in `nymirad`
//! and the web app when they run against this fake. It plays the role of
//! `attestation-policy.json`: a root key for the evidence, accepted measurements
//! and accepted TCB statuses.
//!
//! Test attestation entry, one for the gateway and one per model:
//! `{signing_address, signing_algo, measurement, tcb_status, nonce, report_signature}`
//! plus `model_name` and `model_public_key` (X25519, hex) on model entries.
//! `report_signature` is an Ed25519 signature by the root key over
//! [`attestation_message`]. It replaces the real Intel quote and NVIDIA payload.

use crate::crypto::{
    attestation_root_key, gateway_signing_key, model_e2ee_public_hex, model_signing_key,
    verifying_hex,
};
use ed25519_dalek::{Signature, Signer, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const SIGNING_ALGO: &str = "ed25519";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureKind {
    /// Binds the answer to the model's TEE report. Signed text: `model:request_hash:response_hash`.
    ProviderTee,
    /// Binds the answer to the gateway only. Signed text: `request_hash:response_hash`.
    Gateway,
}

impl SignatureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ProviderTee => "provider_tee",
            Self::Gateway => "gateway",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct TestPolicy {
    pub attestation_root_pk: String,
    pub accepted_measurements: Vec<String>,
    pub accepted_tcb_statuses: Vec<String>,
}

/// The measurement the fake reports until a test changes it.
pub fn default_measurement() -> String {
    hex::encode(Sha256::digest(b"nymira-fake-near-ai/measurement/v1"))
}

impl TestPolicy {
    pub fn fake() -> Self {
        Self {
            attestation_root_pk: verifying_hex(&attestation_root_key()),
            accepted_measurements: vec![default_measurement()],
            accepted_tcb_statuses: vec!["UpToDate".into()],
        }
    }
}

pub fn attestation_message(
    measurement: &str,
    tcb: &str,
    address: &str,
    model_pk: &str,
    nonce: &str,
) -> String {
    format!("nymira-fake-near-ai/attestation/v1|{measurement}|{tcb}|{address}|{model_pk}|{nonce}")
}

fn entry(measurement: &str, tcb: &str, address: String, model_pk: &str, nonce: &str) -> Value {
    let msg = attestation_message(measurement, tcb, &address, model_pk, nonce);
    json!({
        "signing_address": address,
        "signing_algo": SIGNING_ALGO,
        "measurement": measurement,
        "tcb_status": tcb,
        "nonce": nonce,
        "report_signature": hex::encode(attestation_root_key().sign(msg.as_bytes()).to_bytes()),
    })
}

pub(crate) fn build_report(model: &str, measurement: &str, tcb: &str, nonce: &str) -> Value {
    let gateway = entry(
        measurement,
        tcb,
        verifying_hex(&gateway_signing_key()),
        "",
        nonce,
    );
    let pk = model_e2ee_public_hex();
    let mut model_entry = entry(
        measurement,
        tcb,
        verifying_hex(&model_signing_key()),
        &pk,
        nonce,
    );
    model_entry["model_name"] = json!(model);
    model_entry["model_public_key"] = json!(pk);
    json!({"nonce": nonce, "gateway_attestation": gateway, "model_attestations": [model_entry]})
}

/// What a passing attestation proved: the signers and the E2EE key it vouches for.
#[derive(Clone, Debug)]
pub struct VerifiedAttestation {
    pub model_signing_address: String,
    pub gateway_signing_address: String,
    pub model_public_key: String,
}

fn field<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
    v[k].as_str().ok_or_else(|| format!("missing field {k}"))
}

fn verify_ed25519(pk_hex: &str, msg: &[u8], sig_hex: &str) -> Result<(), String> {
    let pk: [u8; 32] = hex::decode(pk_hex)
        .map_err(|e| e.to_string())?
        .try_into()
        .map_err(|_| "bad key length".to_string())?;
    let sig = Signature::from_slice(&hex::decode(sig_hex).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    VerifyingKey::from_bytes(&pk)
        .map_err(|e| e.to_string())?
        .verify_strict(msg, &sig)
        .map_err(|_| "signature does not verify".to_string())
}

fn check_entry(
    policy: &TestPolicy,
    e: &Value,
    nonce: &str,
    model_pk: &str,
) -> Result<String, String> {
    let (m, t, a) = (
        field(e, "measurement")?,
        field(e, "tcb_status")?,
        field(e, "signing_address")?,
    );
    if field(e, "nonce")? != nonce {
        return Err("nonce mismatch".into());
    }
    if !policy.accepted_measurements.iter().any(|x| x == m) {
        return Err(format!("measurement {m} not accepted"));
    }
    if !policy.accepted_tcb_statuses.iter().any(|x| x == t) {
        return Err(format!("tcb status {t} not accepted"));
    }
    let msg = attestation_message(m, t, a, model_pk, nonce);
    verify_ed25519(
        &policy.attestation_root_pk,
        msg.as_bytes(),
        field(e, "report_signature")?,
    )?;
    Ok(a.to_string())
}

/// Checks a report from `GET /v1/attestation/report` against the policy.
pub fn verify_attestation(
    policy: &TestPolicy,
    report: &Value,
    model: &str,
    nonce: &str,
) -> Result<VerifiedAttestation, String> {
    let gateway_signing_address = check_entry(policy, &report["gateway_attestation"], nonce, "")?;
    let m = report["model_attestations"]
        .as_array()
        .and_then(|a| a.iter().find(|e| e["model_name"] == model))
        .ok_or("no attestation for the model")?;
    let model_public_key = field(m, "model_public_key")?.to_string();
    let model_signing_address = check_entry(policy, m, nonce, &model_public_key)?;
    Ok(VerifiedAttestation {
        model_signing_address,
        gateway_signing_address,
        model_public_key,
    })
}

pub fn sha256_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

/// Checks a `GET /v1/signature/{id}` answer against the exact request and response bytes
/// and the attested signer. The signer must be the one the kind names.
pub fn verify_signature(
    att: &VerifiedAttestation,
    sig: &Value,
    model: &str,
    request: &[u8],
    response: &[u8],
) -> Result<SignatureKind, String> {
    let (rq, rs) = (sha256_hex(request), sha256_hex(response));
    let kind = match field(sig, "signature_kind")? {
        "provider_tee" => SignatureKind::ProviderTee,
        "gateway" => SignatureKind::Gateway,
        k => return Err(format!("unknown signature kind {k}")),
    };
    let (expected_text, expected_signer) = match kind {
        SignatureKind::ProviderTee => (format!("{model}:{rq}:{rs}"), &att.model_signing_address),
        SignatureKind::Gateway => (format!("{rq}:{rs}"), &att.gateway_signing_address),
    };
    let text = field(sig, "text")?;
    if text != expected_text {
        return Err("signed text does not match the exchanged bytes".into());
    }
    let signer = field(sig, "signing_address")?;
    if signer != expected_signer {
        return Err("signer is not the attested key".into());
    }
    verify_ed25519(signer, text.as_bytes(), field(sig, "signature")?)?;
    Ok(kind)
}
