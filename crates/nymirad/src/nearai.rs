//! The verifier for NEAR AI's real evidence: Intel TDX quotes and NVIDIA GPU evidence, checked
//! locally with the vendored SDK, plus what the SDK leaves to the caller (the accepted
//! deployments of the signed policy).
//!
//! What a pass means: the Intel quote verifies against Intel's root with fresh collateral and is
//! `UpToDate`, the debug flag is off, the nonce we sent is in it, the signing key that the chat
//! is encrypted to equals the attested signer, NVIDIA's service accepts the GPU evidence, and the
//! measured deployment (`mr_td`, `rtmr0-2`, the compose and OS image hashes) is one the signed
//! policy lists, as one entry. Everything is fetched through the same SOCKS proxy.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use nearai_inference_sdk::{
    verify_gateway_attestation, verify_gateway_response, verify_model_attestation,
    verify_model_response, AttestationClient, AttestationPolicy, AttestationVerifiers,
    CompletionSignature, CompletionSignatureKind, DeploymentVerifier,
    GatewayAttestationFetchOptions, GpuEvidenceRequirement, MeasuredDeployment,
    ModelAttestationPolicy, ModelAttestationVerifiers, NrasGpuEvidenceVerifier, SigningAlgo,
    SigningIdentity, TcbStatus, TdxQuoteVerificationResult, TdxQuoteVerifier, VerificationError,
    DEFAULT_INTEL_PCCS_URL, DEFAULT_NVIDIA_NRAS_URL,
};
use serde::Deserialize;
use serde_json::Value;

use crate::attest::{Attestor, Deployment, ModelIdentity, Policy, ReportRequest, Trust};

fn norm(hex: &str) -> String {
    hex.strip_prefix("0x").unwrap_or(hex).to_ascii_lowercase()
}

/// The entries of the policy for one role, and which of them the quote has matched so far.
#[derive(Clone)]
struct Accepted {
    deployments: Vec<Deployment>,
    /// Indices of entries whose Intel measurements the verified quote carries. The deployment
    /// check then needs the compose and OS image hashes of one of these same entries.
    matched: Arc<Mutex<Vec<usize>>>,
}

impl Accepted {
    fn new(policy: &Policy, role: &str) -> Self {
        Self {
            deployments: policy
                .deployments
                .iter()
                .filter(|d| d.role == role)
                .cloned()
                .collect(),
            matched: Arc::default(),
        }
    }
}

/// Where the quote's collateral comes from.
#[derive(Clone)]
pub enum Collateral {
    /// Fetched from `pccs_url` over `client` (through the SOCKS proxy), or directly without one.
    Live {
        client: Option<reqwest::Client>,
        pccs_url: String,
    },
    /// A recording, checked at the time it was recorded (tests).
    Recorded {
        collateral: Box<dcap_qvl::QuoteCollateralV3>,
        at_secs: u64,
    },
}

/// Intel TDX quote verification that also reads `mr_td` and `rtmr0-2` and holds them to the policy.
pub struct IntelTdx {
    collateral: Collateral,
    accepted: Accepted,
}

fn tcb_status(status: &str) -> Result<TcbStatus, VerificationError> {
    Ok(match status {
        "UpToDate" => TcbStatus::UpToDate,
        "SWHardeningNeeded" => TcbStatus::SwHardeningNeeded,
        "ConfigurationNeeded" => TcbStatus::ConfigurationNeeded,
        "ConfigurationAndSWHardeningNeeded" => TcbStatus::ConfigurationAndSwHardeningNeeded,
        "OutOfDate" => TcbStatus::OutOfDate,
        "OutOfDateConfigurationNeeded" => TcbStatus::OutOfDateConfigurationNeeded,
        "Revoked" => TcbStatus::Revoked,
        "Unknown" => TcbStatus::Unknown,
        _ => {
            return Err(VerificationError::QuoteInvalidResult {
                reason: "unsupported_tcb_status".into(),
            })
        }
    })
}

#[async_trait]
impl TdxQuoteVerifier for IntelTdx {
    async fn verify(
        &self,
        intel_quote: &str,
    ) -> Result<TdxQuoteVerificationResult, VerificationError> {
        let bytes = hex::decode(norm(intel_quote)).map_err(|_| {
            VerificationError::QuoteVerificationFailed {
                reason: "invalid_encoding",
            }
        })?;
        dcap_qvl::quote::Quote::parse(&bytes).map_err(|_| {
            VerificationError::QuoteVerificationFailed {
                reason: "invalid_quote",
            }
        })?;
        let (collateral, now) = match &self.collateral {
            Collateral::Recorded {
                collateral,
                at_secs,
            } => ((**collateral).clone(), *at_secs),
            Collateral::Live { client, pccs_url } => {
                let got = match client {
                    Some(c) => {
                        dcap_qvl::collateral::get_collateral_with_client(c, pccs_url, &bytes).await
                    }
                    None => dcap_qvl::collateral::get_collateral(pccs_url, &bytes).await,
                };
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| VerificationError::QuoteVerificationFailed {
                        reason: "verifier_error",
                    })?
                    .as_secs();
                (
                    got.map_err(|_| VerificationError::QuoteCollateralUnavailable)?,
                    now,
                )
            }
        };
        let verified = dcap_qvl::verify::verify(&bytes, &collateral, now).map_err(|_| {
            VerificationError::QuoteVerificationFailed {
                reason: "verifier_error",
            }
        })?;
        let td10 = verified
            .report
            .as_td10()
            .ok_or(VerificationError::QuoteUnsupportedReportType)?;

        // The quote's signature covers these values; the policy says which are NEAR AI's.
        let measured = [
            hex::encode(td10.mr_td),
            hex::encode(td10.rt_mr0),
            hex::encode(td10.rt_mr1),
            hex::encode(td10.rt_mr2),
        ];
        let matched: Vec<usize> = self
            .accepted
            .deployments
            .iter()
            .enumerate()
            .filter(|(_, d)| {
                [&d.mr_td, &d.rtmr0, &d.rtmr1, &d.rtmr2].map(String::as_str) == measured
            })
            .map(|(i, _)| i)
            .collect();
        if matched.is_empty() {
            return Err(VerificationError::QuoteVerificationFailed {
                reason: "measurement_not_accepted",
            });
        }
        *self
            .accepted
            .matched
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = matched;

        Ok(TdxQuoteVerificationResult {
            tcb_status: tcb_status(&verified.status)?,
            advisory_ids: verified.advisory_ids,
            debug_enabled: (td10.td_attributes[0] & 0x01) != 0,
            report_data: td10.report_data.to_vec(),
            mr_config_id: td10.mr_config_id.to_vec(),
            rt_mr3: td10.rt_mr3.to_vec(),
        })
    }
}

/// The compose and OS image hashes must belong to an entry whose Intel measurements the quote
/// carried, so entries are never mixed.
struct DeploymentCheck {
    accepted: Accepted,
}

#[async_trait]
impl DeploymentVerifier for DeploymentCheck {
    async fn verify(&self, deployment: &MeasuredDeployment) -> Result<(), VerificationError> {
        let m = &deployment.runtime_measurements;
        let (Some(compose), Some(os)) = (&m.compose_hash, &m.os_image_hash) else {
            return Err(VerificationError::DeploymentProvenanceRejected);
        };
        let (compose, os) = (norm(compose), norm(os));
        let matched = self
            .accepted
            .matched
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let ok = matched.iter().any(|i| {
            let d = &self.accepted.deployments[*i];
            d.compose_hash == compose && d.os_image_hash == os
        });
        if ok {
            Ok(())
        } else {
            Err(VerificationError::DeploymentProvenanceRejected)
        }
    }
}

/// The wire form of `/v1/signature/{id}`.
#[derive(Deserialize)]
struct WireSignature {
    signature_kind: CompletionSignatureKind,
    text: String,
    signature: String,
    signing_address: String,
    signing_algo: SigningAlgo,
}

pub struct NearAiAttestor {
    policy: Policy,
    pccs_url: String,
    nras_url: String,
}

impl NearAiAttestor {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            pccs_url: DEFAULT_INTEL_PCCS_URL.into(),
            nras_url: DEFAULT_NVIDIA_NRAS_URL.into(),
        }
    }

    fn proxied(socks_url: Option<&str>) -> Result<Option<reqwest::Client>, String> {
        let Some(url) = socks_url else {
            return Ok(None);
        };
        let proxy = reqwest::Proxy::all(url).map_err(|e| e.to_string())?;
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(60))
            .no_proxy()
            .proxy(proxy)
            .build()
            .map(Some)
            .map_err(|e| e.to_string())
    }
}

#[async_trait]
impl Attestor for NearAiAttestor {
    async fn verify_model(&self, ask: &ReportRequest<'_>) -> Result<ModelIdentity, String> {
        let describe = |e: VerificationError| format!("{e} ({})", e.code());
        let base = format!("{}/v1", ask.gateway_url.trim_end_matches('/'));
        let mut api =
            AttestationClient::with_base_url(String::new(), &base).map_err(|e| e.to_string())?;
        if let Some(url) = ask.socks_url {
            api = api.with_socks_proxy(url).map_err(|e| e.to_string())?;
        }
        let http = Self::proxied(ask.socks_url)?;
        let collateral = Collateral::Live {
            client: http.clone(),
            pccs_url: self.pccs_url.clone(),
        };
        let nras = match &http {
            Some(c) => NrasGpuEvidenceVerifier::with_client(c.clone(), &self.nras_url),
            None => NrasGpuEvidenceVerifier::new(&self.nras_url),
        };

        let fetched = api
            .fetch_model_attestations(ask.model, Some(SigningAlgo::Ed25519), None)
            .await
            .map_err(|e| format!("{e} ({})", e.code()))?;
        let model_accepted = Accepted::new(&self.policy, "model");
        let tdx = IntelTdx {
            collateral: collateral.clone(),
            accepted: model_accepted.clone(),
        };
        let deployment = DeploymentCheck {
            accepted: model_accepted,
        };
        let policy = ModelAttestationPolicy {
            accepted_tcb_statuses: Some(vec![TcbStatus::UpToDate]),
            gpu_evidence: GpuEvidenceRequirement::Required,
        };
        let mut last = "the report holds no model attestation with a signing key".to_string();
        let mut verified = None;
        for candidate in &fetched.attestations {
            if candidate.signing_public_key.is_none()
                || candidate.evidence.signer.signing_algo != SigningAlgo::Ed25519
            {
                continue;
            }
            match verify_model_attestation(
                candidate,
                &fetched.client_binding,
                Some(&policy),
                ModelAttestationVerifiers {
                    tdx_quote: Some(&tdx),
                    deployment: Some(&deployment),
                    gpu_evidence: Some(&nras),
                },
            )
            .await
            {
                Ok(v) => {
                    verified = Some(v);
                    break;
                }
                Err(e) => last = describe(e),
            }
        }
        let verified = verified.ok_or(last)?;

        let gateway = api
            .fetch_gateway_attestation(GatewayAttestationFetchOptions {
                signing_algo: Some(SigningAlgo::Ed25519),
                include_spki_fingerprint: false,
            })
            .await
            .map_err(|e| format!("{e} ({})", e.code()))?;
        let gateway_accepted = Accepted::new(&self.policy, "gateway");
        let tdx_gateway = IntelTdx {
            collateral,
            accepted: gateway_accepted.clone(),
        };
        let deployment_gateway = DeploymentCheck {
            accepted: gateway_accepted,
        };
        let verified_gateway = verify_gateway_attestation(
            &gateway.attestation,
            &gateway.client_binding,
            Some(&AttestationPolicy {
                accepted_tcb_statuses: Some(vec![TcbStatus::UpToDate]),
            }),
            AttestationVerifiers {
                tdx_quote: Some(&tdx_gateway),
                deployment: Some(&deployment_gateway),
            },
        )
        .await
        .map_err(describe)?;

        let key = verified
            .signing_public_key
            .as_deref()
            .ok_or("the model attestation carries no encryption key")?;
        let model_pub: [u8; 32] = hex::decode(key)
            .ok()
            .and_then(|b| b.try_into().ok())
            .ok_or("the model key is not 32 bytes")?;
        Ok(ModelIdentity {
            model_pub,
            model_signing_address: verified.evidence.signer.signing_address.clone(),
            gateway_signing_address: verified_gateway.evidence.signer.signing_address.clone(),
            verified_model: Some(verified),
            verified_gateway: Some(verified_gateway),
        })
    }

    fn verify_signature(
        &self,
        identity: &ModelIdentity,
        signature: &Value,
        _model: &str,
        request: &[u8],
        response: &[u8],
    ) -> Trust {
        let wire: WireSignature = match serde_json::from_value(signature.clone()) {
            Ok(w) => w,
            Err(e) => return Trust::NotVerified(format!("the signature is malformed: {e}")),
        };
        let sig = CompletionSignature {
            kind: wire.signature_kind,
            signed_text: wire.text,
            signature: wire.signature,
            signer: SigningIdentity {
                signing_algo: wire.signing_algo,
                signing_address: wire.signing_address,
            },
        };
        let checked = match sig.kind {
            CompletionSignatureKind::ProviderTee => identity
                .verified_model
                .as_ref()
                .ok_or_else(|| "no verified model attestation".to_string())
                .and_then(|v| {
                    verify_model_response(request, response, &sig, v)
                        .map(|()| Trust::ModelSigned)
                        .map_err(|e| format!("{e} ({})", e.code()))
                }),
            CompletionSignatureKind::Gateway => identity
                .verified_gateway
                .as_ref()
                .ok_or_else(|| "no verified gateway attestation".to_string())
                .and_then(|v| {
                    verify_gateway_response(request, response, &sig, v)
                        .map(|()| Trust::GatewaySigned)
                        .map_err(|e| format!("{e} ({})", e.code()))
                }),
        };
        checked.unwrap_or_else(Trust::NotVerified)
    }
}
