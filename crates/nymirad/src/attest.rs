//! What the client believes about the model: the attestation report that binds the encryption
//! key, and the signature over each exchange.
//!
//! The pinned, signed `attestation-policy.json` says which measurements and TCB statuses are
//! accepted. A report that fails it, or a key the report does not bind, stops the request before
//! anything is encrypted; a bad signature does not hide the answer but is reported as
//! "not verified".
//!
//! A release build verifies NEAR AI's Intel TDX and NVIDIA GPU evidence ([`crate::nearai`]); the
//! verifier for the fake gateway's test evidence is built only with the `mock` feature. A build
//! with no verifier refuses to chat rather than encrypt to a key it cannot check.

use async_trait::async_trait;
use ed25519_dalek::{Signature, VerifyingKey};
use nearai_inference_sdk::{VerifiedGatewayAttestation, VerifiedModelAttestation};
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
    /// What the SDK established, kept to check completion signatures. `None` for test evidence.
    pub verified_model: Option<VerifiedModelAttestation>,
    pub verified_gateway: Option<VerifiedGatewayAttestation>,
}

/// What an attestor needs to fetch and check the evidence for one model.
pub struct ReportRequest<'a> {
    pub gateway_url: &'a str,
    pub model: &'a str,
    /// A fresh nonce for verifiers that fetch the report themselves with it.
    pub nonce: &'a str,
    /// A client on its own connection and identity, for verifiers that fetch with it.
    pub http: &'a reqwest::Client,
    /// The SOCKS5 URL the same identity uses (`socks5h://<identity>:x@127.0.0.1:port`), `None` on
    /// a direct transport. Verifiers that build their own clients (Intel, NVIDIA) use it.
    pub socks_url: Option<&'a str>,
}

#[async_trait]
pub trait Attestor: Send + Sync {
    /// Fetches the evidence for `ask.model`, checks it against the policy, and returns the key it
    /// binds.
    async fn verify_model(&self, ask: &ReportRequest<'_>) -> Result<ModelIdentity, String>;

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

#[async_trait]
impl Attestor for Unavailable {
    async fn verify_model(&self, _: &ReportRequest<'_>) -> Result<ModelIdentity, String> {
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
    /// The key that signs the fake gateway's test evidence. Real evidence is rooted in Intel's and
    /// NVIDIA's keys, which are built into the verifiers.
    pub attestation_root_pk: String,
    /// Measurements the fake gateway's test evidence may carry.
    pub accepted_measurements: Vec<String>,
    /// The deployments NEAR AI's real evidence may come from. Every field is checked.
    #[serde(default)]
    pub deployments: Vec<Deployment>,
    pub accepted_tcb_statuses: Vec<String>,
    /// Ed25519 signature (hex) by the policy signer over [`Policy::signed_text`].
    pub signature: String,
}

/// One accepted deployment: the Intel TDX measurements of the confidential VM and the hashes
/// dstack derives from its event log. All values are lower-case hex without `0x`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Deployment {
    /// `model` for a model-serving VM, `gateway` for the gateway's.
    pub role: String,
    pub compose_hash: String,
    pub os_image_hash: String,
    pub mr_td: String,
    pub rtmr0: String,
    pub rtmr1: String,
    pub rtmr2: String,
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
    #[error("the policy accepts no measurement or deployment")]
    Empty,
    #[error("the policy lists a deployment with a malformed value: {0}")]
    BadDeployment(String),
}

/// The only TCB status a policy may accept. `OutOfDate` and every other status are refused.
pub const STRICT_TCB: &str = "UpToDate";

impl Policy {
    pub fn signed_text(&self) -> String {
        let deployments: Vec<String> = self
            .deployments
            .iter()
            .map(|d| {
                [
                    &d.role,
                    &d.compose_hash,
                    &d.os_image_hash,
                    &d.mr_td,
                    &d.rtmr0,
                    &d.rtmr1,
                    &d.rtmr2,
                ]
                .map(String::as_str)
                .join(",")
            })
            .collect();
        format!(
            "nymira/attestation-policy/v2|{}|{}|{}|{}|{}",
            self.version,
            self.attestation_root_pk,
            self.accepted_measurements.join(","),
            self.accepted_tcb_statuses.join(","),
            deployments.join(";")
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
        if (policy.accepted_measurements.is_empty() && policy.deployments.is_empty())
            || policy.accepted_tcb_statuses.is_empty()
        {
            return Err(PolicyError::Empty);
        }
        for d in &policy.deployments {
            let sizes = [
                ("role", &d.role, 0),
                ("compose_hash", &d.compose_hash, 32),
                ("os_image_hash", &d.os_image_hash, 32),
                ("mr_td", &d.mr_td, 48),
                ("rtmr0", &d.rtmr0, 48),
                ("rtmr1", &d.rtmr1, 48),
                ("rtmr2", &d.rtmr2, 48),
            ];
            for (name, value, bytes) in sizes {
                let ok = if bytes == 0 {
                    matches!(value.as_str(), "model" | "gateway")
                } else {
                    value.len() == bytes * 2
                        && value
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                };
                if !ok {
                    return Err(PolicyError::BadDeployment(name.into()));
                }
            }
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

    impl TestEvidence {
        fn check_report(
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
                verified_model: None,
                verified_gateway: None,
            })
        }
    }

    #[async_trait]
    impl Attestor for TestEvidence {
        async fn verify_model(&self, ask: &ReportRequest<'_>) -> Result<ModelIdentity, String> {
            let report: Value = ask
                .http
                .get(format!(
                    "{}/v1/attestation/report",
                    ask.gateway_url.trim_end_matches('/')
                ))
                .query(&[
                    ("model", ask.model),
                    ("signing_algo", "ed25519"),
                    ("nonce", ask.nonce),
                ])
                .send()
                .await
                .map_err(|e| e.without_url().to_string())?
                .json()
                .await
                .map_err(|e| e.without_url().to_string())?;
            self.check_report(&report, ask.model, ask.nonce)
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
            deployments: vec![],
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

    #[tokio::test]
    async fn the_release_default_refuses_to_pick_a_key() {
        let http = reqwest::Client::new();
        let ask = ReportRequest {
            gateway_url: "http://127.0.0.1:1",
            model: "m",
            nonce: "n",
            http: &http,
            socks_url: None,
        };
        assert!(Unavailable
            .verify_model(&ask)
            .await
            .unwrap_err()
            .contains("will not encrypt"));
    }

    fn deployment() -> Deployment {
        Deployment {
            role: "model".into(),
            compose_hash: "a".repeat(64),
            os_image_hash: "b".repeat(64),
            mr_td: "c".repeat(96),
            rtmr0: "d".repeat(96),
            rtmr1: "e".repeat(96),
            rtmr2: "f".repeat(96),
        }
    }

    #[test]
    fn deployments_are_signed_and_each_value_is_checked() {
        let mut p = policy(3, &["UpToDate"]);
        p.accepted_measurements.clear();
        p.deployments.push(deployment());
        let p = p.sign(&signer());
        let json = serde_json::to_vec(&p).unwrap();
        assert_eq!(Policy::load(&json, &pk(), 1).unwrap().deployments.len(), 1);

        // A changed measurement breaks the signature.
        let mut tampered = p.clone();
        tampered.deployments[0].mr_td = "0".repeat(96);
        assert_eq!(
            Policy::load(&serde_json::to_vec(&tampered).unwrap(), &pk(), 1),
            Err(PolicyError::Signature)
        );
        // A signed value of the wrong shape is refused all the same.
        for (field, bad) in [("mr_td", "ab"), ("role", "other"), ("rtmr1", "ZZ")] {
            let mut q = p.clone();
            match field {
                "mr_td" => q.deployments[0].mr_td = bad.into(),
                "role" => q.deployments[0].role = bad.into(),
                _ => q.deployments[0].rtmr1 = bad.into(),
            }
            let q = q.sign(&signer());
            assert!(matches!(
                Policy::load(&serde_json::to_vec(&q).unwrap(), &pk(), 1),
                Err(PolicyError::BadDeployment(_))
            ));
        }
    }

    #[test]
    fn a_policy_with_neither_measurements_nor_deployments_is_refused() {
        let mut p = policy(3, &["UpToDate"]);
        p.accepted_measurements.clear();
        let p = p.sign(&signer());
        assert_eq!(
            Policy::load(&serde_json::to_vec(&p).unwrap(), &pk(), 1),
            Err(PolicyError::Empty)
        );
    }
}
