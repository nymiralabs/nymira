//! What the client believes about the model: the attestation report that binds the encryption
//! key, and the signature over each exchange.
//!
//! The pinned, signed `attestation-policy.json` says which measurements and TCB statuses are
//! accepted. A report that fails it, or a key the report does not bind, stops the request before
//! anything is encrypted; a bad signature does not hide the answer but is reported as
//! "not verified".
//!
//! Only the verifier for the test evidence format of the fake gateway is built in (`mock`
//! feature). A release build has no verifier for NEAR AI's Intel and NVIDIA evidence yet, so it
//! refuses to chat rather than encrypt to a key it cannot check.

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How far an answer is vouched for.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", content = "reason", rename_all = "snake_case")]
pub enum Trust {
    /// Signed by the attested model key over the exact request and response.
    ModelSigned,
    /// Signed by the attested gateway key only.
    GatewaySigned,
    NotVerified(String),
}

impl Trust {
    pub fn label(&self) -> &'static str {
        match self {
            Trust::ModelSigned => "model-signed",
            Trust::GatewaySigned => "gateway-signed",
            Trust::NotVerified(_) => "not-verified",
        }
    }
}

/// What a passing report established.
#[derive(Clone, Debug)]
pub struct ModelIdentity {
    /// The Ed25519 key the chat is encrypted to (the model's signing key).
    pub model_pub: [u8; 32],
    pub model_signing_address: String,
    pub gateway_signing_address: String,
}

pub trait Attestor: Send + Sync {
    /// Checks the report for `model` against the policy and the nonce we sent, and returns the
    /// key it binds.
    fn verify_report(
        &self,
        report: &Value,
        model: &str,
        nonce: &str,
    ) -> Result<ModelIdentity, String>;

    /// Checks a `/v1/signature/{id}` answer against the exact bytes exchanged.
    fn verify_signature(
        &self,
        identity: &ModelIdentity,
        signature: &Value,
        model: &str,
        request: &[u8],
        response: &[u8],
    ) -> Trust;
}

/// The release default: no verifier for the real evidence is built in.
pub struct Unavailable;

impl Attestor for Unavailable {
    fn verify_report(&self, _: &Value, _: &str, _: &str) -> Result<ModelIdentity, String> {
        Err("this build cannot verify NEAR AI attestation reports, so it will not encrypt to an unchecked key".into())
    }

    fn verify_signature(&self, _: &ModelIdentity, _: &Value, _: &str, _: &[u8], _: &[u8]) -> Trust {
        Trust::NotVerified("no attestation verifier in this build".into())
    }
}

/// `attestation-policy.json`: signed, versioned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Policy {
    pub version: u32,
    pub attestation_root_pk: String,
    pub accepted_measurements: Vec<String>,
    pub accepted_tcb_statuses: Vec<String>,
    /// Ed25519 signature (hex) by the policy signer over [`Policy::signed_text`].
    pub signature: String,
}

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("the policy file is not valid: {0}")]
    Format(String),
    #[error("the policy signature does not verify")]
    Signature,
    #[error("the policy is version {have}; this build needs at least {need}")]
    TooOld { have: u32, need: u32 },
    #[error("the policy accepts TCB status {0}, which a strict policy never does")]
    Lenient(String),
    #[error("the policy accepts no measurement")]
    Empty,
}

/// The only TCB status a policy may accept. `OutOfDate` and every other status are refused.
pub const STRICT_TCB: &str = "UpToDate";

impl Policy {
    pub fn signed_text(&self) -> String {
        format!(
            "nymira/attestation-policy/v1|{}|{}|{}|{}",
            self.version,
            self.attestation_root_pk,
            self.accepted_measurements.join(","),
            self.accepted_tcb_statuses.join(",")
        )
    }

    /// Parses and checks `json` against the policy signer's key and the minimum version compiled
    /// into the binary.
    pub fn load(json: &[u8], signer_pk: &[u8; 32], min_version: u32) -> Result<Self, PolicyError> {
        let policy: Policy =
            serde_json::from_slice(json).map_err(|e| PolicyError::Format(e.to_string()))?;
        let sig = hex::decode(&policy.signature)
            .ok()
            .and_then(|b| Signature::from_slice(&b).ok())
            .ok_or(PolicyError::Signature)?;
        VerifyingKey::from_bytes(signer_pk)
            .map_err(|_| PolicyError::Signature)?
            .verify_strict(policy.signed_text().as_bytes(), &sig)
            .map_err(|_| PolicyError::Signature)?;
        if policy.version < min_version {
            return Err(PolicyError::TooOld {
                have: policy.version,
                need: min_version,
            });
        }
        if let Some(bad) = policy
            .accepted_tcb_statuses
            .iter()
            .find(|s| *s != STRICT_TCB)
        {
            return Err(PolicyError::Lenient(bad.clone()));
        }
        if policy.accepted_measurements.is_empty() || policy.accepted_tcb_statuses.is_empty() {
            return Err(PolicyError::Empty);
        }
        Ok(policy)
    }

    /// Signs a policy (the offline release step; tests).
    pub fn sign(mut self, signer: &ed25519_dalek::SigningKey) -> Self {
        use ed25519_dalek::Signer;
        self.signature = hex::encode(signer.sign(self.signed_text().as_bytes()).to_bytes());
        self
    }
}

#[cfg(feature = "mock")]
pub use test_scheme::TestEvidence;

/// The verifier for the fake gateway's test evidence. Not a verifier of Intel or NVIDIA evidence.
#[cfg(feature = "mock")]
mod test_scheme {
    use super::*;
    use sha2::{Digest, Sha256};

    pub struct TestEvidence {
        pub policy: Policy,
    }

    fn field<'a>(v: &'a Value, k: &str) -> Result<&'a str, String> {
        v[k].as_str().ok_or_else(|| format!("missing field {k}"))
    }

    fn check_ed25519(pk_hex: &str, msg: &[u8], sig_hex: &str) -> Result<(), String> {
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

    fn check_entry(p: &Policy, e: &Value, nonce: &str, model_pk: &str) -> Result<String, String> {
        let (m, t, a) = (
            field(e, "measurement")?,
            field(e, "tcb_status")?,
            field(e, "signing_address")?,
        );
        if field(e, "nonce")? != nonce {
            return Err("nonce mismatch".into());
        }
        if !p.accepted_measurements.iter().any(|x| x == m) {
            return Err(format!("measurement {m} is not accepted"));
        }
        if !p.accepted_tcb_statuses.iter().any(|x| x == t) {
            return Err(format!("TCB status {t} is not accepted"));
        }
        let msg = format!("nymira-fake-near-ai/attestation/v1|{m}|{t}|{a}|{model_pk}|{nonce}");
        check_ed25519(
            &p.attestation_root_pk,
            msg.as_bytes(),
            field(e, "report_signature")?,
        )?;
        Ok(a.to_string())
    }

    impl Attestor for TestEvidence {
        fn verify_report(
            &self,
            report: &Value,
            model: &str,
            nonce: &str,
        ) -> Result<ModelIdentity, String> {
            let gateway = check_entry(&self.policy, &report["gateway_attestation"], nonce, "")?;
            let entry = report["model_attestations"]
                .as_array()
                .and_then(|a| a.iter().find(|e| e["model_name"] == model))
                .ok_or("no attestation for the model")?;
            let model_pk = field(entry, "model_public_key")?;
            let address = check_entry(&self.policy, entry, nonce, model_pk)?;
            // The key we encrypt to must be the attested signer itself.
            if model_pk != address {
                return Err("the model key is not the attested signing key".into());
            }
            let model_pub: [u8; 32] = hex::decode(model_pk)
                .map_err(|e| e.to_string())?
                .try_into()
                .map_err(|_| "bad model key length".to_string())?;
            Ok(ModelIdentity {
                model_pub,
                model_signing_address: address,
                gateway_signing_address: gateway,
            })
        }

        fn verify_signature(
            &self,
            id: &ModelIdentity,
            sig: &Value,
            model: &str,
            request: &[u8],
            response: &[u8],
        ) -> Trust {
            let hash = |b: &[u8]| hex::encode(Sha256::digest(b));
            let (rq, rs) = (hash(request), hash(response));
            let run = || -> Result<Trust, String> {
                let (text, signer, trust) = match field(sig, "signature_kind")? {
                    "provider_tee" => (
                        format!("{model}:{rq}:{rs}"),
                        &id.model_signing_address,
                        Trust::ModelSigned,
                    ),
                    "gateway" => (
                        format!("{rq}:{rs}"),
                        &id.gateway_signing_address,
                        Trust::GatewaySigned,
                    ),
                    k => return Err(format!("unknown signature kind {k}")),
                };
                if field(sig, "text")? != text {
                    return Err("signed text does not match the exchanged bytes".into());
                }
                if field(sig, "signing_address")? != signer {
                    return Err("signer is not the attested key".into());
                }
                check_ed25519(signer, text.as_bytes(), field(sig, "signature")?)?;
                Ok(trust)
            };
            run().unwrap_or_else(Trust::NotVerified)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    fn signer() -> SigningKey {
        SigningKey::from_bytes(&[9; 32])
    }

    fn policy(version: u32, tcb: &[&str]) -> Policy {
        Policy {
            version,
            attestation_root_pk: "aa".into(),
            accepted_measurements: vec!["m1".into()],
            accepted_tcb_statuses: tcb.iter().map(|s| s.to_string()).collect(),
            signature: String::new(),
        }
        .sign(&signer())
    }

    fn pk() -> [u8; 32] {
        signer().verifying_key().to_bytes()
    }

    #[test]
    fn a_signed_current_strict_policy_loads() {
        let json = serde_json::to_vec(&policy(3, &["UpToDate"])).unwrap();
        assert_eq!(Policy::load(&json, &pk(), 2).unwrap().version, 3);
    }

    #[test]
    fn a_policy_below_the_minimum_version_is_refused() {
        let json = serde_json::to_vec(&policy(1, &["UpToDate"])).unwrap();
        assert_eq!(
            Policy::load(&json, &pk(), 2),
            Err(PolicyError::TooOld { have: 1, need: 2 })
        );
    }

    #[test]
    fn a_tampered_or_foreign_signed_policy_is_refused() {
        let mut p = policy(3, &["UpToDate"]);
        p.accepted_measurements.push("evil".into());
        assert_eq!(
            Policy::load(&serde_json::to_vec(&p).unwrap(), &pk(), 1),
            Err(PolicyError::Signature)
        );
        let json = serde_json::to_vec(&policy(3, &["UpToDate"])).unwrap();
        let other = SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes();
        assert_eq!(Policy::load(&json, &other, 1), Err(PolicyError::Signature));
    }

    #[test]
    fn out_of_date_and_every_other_status_is_refused_even_when_signed() {
        for bad in ["OutOfDate", "SWHardeningNeeded", "ConfigurationNeeded"] {
            let json = serde_json::to_vec(&policy(3, &["UpToDate", bad])).unwrap();
            assert_eq!(
                Policy::load(&json, &pk(), 1),
                Err(PolicyError::Lenient(bad.into()))
            );
        }
    }

    #[test]
    fn the_release_default_refuses_to_pick_a_key() {
        assert!(Unavailable
            .verify_report(&Value::Null, "m", "n")
            .unwrap_err()
            .contains("will not encrypt"));
    }
}
