//! The verifier for NEAR AI's real evidence: Intel TDX quotes and NVIDIA GPU evidence, checked
//! locally with the vendored SDK, plus what the SDK leaves to the caller (the accepted
//! deployments of the signed policy).
//!
//! What a pass means: the Intel quote verifies against Intel's root with fresh collateral and is
//! `UpToDate`, the debug flag is off, the nonce we sent is in it, the signing key that the chat
//! is encrypted to equals the attested signer, NVIDIA's service accepts the GPU evidence, and the
//! measured deployment (`mr_td`, `rtmr0-2`, the compose and OS image hashes) is one the signed
//! policy lists, as one entry. Everything is fetched through the same SOCKS proxy.
//!
//! Not checked: `mr_seam`, `xfam` and the TD attributes other than the debug bit. The quote's
//! signature covers them, the policy does not name them.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use nearai_inference_sdk::{
    verify_gateway_attestation, verify_gateway_response, verify_model_attestation,
    verify_model_response, AttestationClient, AttestationPolicy, AttestationVerifiers,
    CompletionSignature, CompletionSignatureKind, DeploymentVerifier,
    GatewayAttestationFetchOptions, GpuEvidenceRequirement, GpuEvidenceVerifier,
    MeasuredDeployment, ModelAttestationPolicy, ModelAttestationVerifiers, NrasGpuEvidenceVerifier,
    SigningAlgo, SigningIdentity, TcbStatus, TdxQuoteVerificationResult, TdxQuoteVerifier,
    VerificationError, DEFAULT_INTEL_PCCS_URL, DEFAULT_NVIDIA_NRAS_URL,
};
use nymira_operator::clock::{Clock, SystemClock};
use serde::Deserialize;
use serde_json::Value;

use crate::nras::NrasChain;

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
    #[cfg(feature = "mock")]
    Recorded {
        collateral: Box<dcap_qvl::QuoteCollateralV3>,
        at_secs: u64,
    },
}

/// Measurements a verified quote and its event log carried; the values a policy lists.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Observed {
    pub mr_td: String,
    pub rtmr0: String,
    pub rtmr1: String,
    pub rtmr2: String,
    pub compose_hash: Option<String>,
    pub os_image_hash: Option<String>,
}

/// Where an observing verifier writes what it saw.
pub type Sink = Arc<Mutex<Vec<Observed>>>;

/// Intel TDX quote verification that also reads `mr_td` and `rtmr0-2` and holds them to the policy.
pub struct IntelTdx {
    collateral: Collateral,
    accepted: Accepted,
    /// Release tooling only: accept any measurement and write it here (how a policy is drafted
    /// from a live report). `NearAiAttestor` never sets it.
    observe: Option<Sink>,
}

impl IntelTdx {
    /// A verifier for `role` of `policy`.
    pub fn new(collateral: Collateral, policy: &Policy, role: &str) -> Self {
        Self {
            collateral,
            accepted: Accepted::new(policy, role),
            observe: None,
        }
    }

    /// Verifies the quote fully but accepts any measurement, and records it in `sink`. Release
    /// tooling only (`record_evidence`): a build without the `mock` feature cannot reach it.
    #[cfg(feature = "mock")]
    pub fn observing(collateral: Collateral, sink: Sink) -> Self {
        Self {
            collateral,
            accepted: Accepted {
                deployments: vec![],
                matched: Arc::default(),
            },
            observe: Some(sink),
        }
    }

    /// The deployment check that goes with this verifier: the same matched entries, or, when
    /// observing, the same sink.
    pub fn deployment_check(&self) -> impl DeploymentVerifier {
        DeploymentCheck {
            accepted: self.accepted.clone(),
            observe: self.observe.clone(),
        }
    }

    fn result(
        &self,
        verified: &dcap_qvl::verify::VerifiedReport,
        td10: &dcap_qvl::quote::TDReport10,
    ) -> Result<TdxQuoteVerificationResult, VerificationError> {
        Ok(TdxQuoteVerificationResult {
            tcb_status: tcb_status(&verified.status)?,
            advisory_ids: verified.advisory_ids.clone(),
            debug_enabled: (td10.td_attributes[0] & 0x01) != 0,
            report_data: td10.report_data.to_vec(),
            mr_config_id: td10.mr_config_id.to_vec(),
            rt_mr3: td10.rt_mr3.to_vec(),
        })
    }
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
            #[cfg(feature = "mock")]
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
        // A new quote starts with no match, so a failure here never leaves the last one behind.
        self.accepted
            .matched
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
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
        if let Some(sink) = &self.observe {
            sink.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(Observed {
                    mr_td: measured[0].clone(),
                    rtmr0: measured[1].clone(),
                    rtmr1: measured[2].clone(),
                    rtmr2: measured[3].clone(),
                    ..Observed::default()
                });
            return self.result(&verified, td10);
        }
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
        self.result(&verified, td10)
    }
}

/// The compose and OS image hashes must belong to an entry whose Intel measurements the quote
/// carried, so entries are never mixed.
struct DeploymentCheck {
    accepted: Accepted,
    observe: Option<Sink>,
}

#[async_trait]
impl DeploymentVerifier for DeploymentCheck {
    async fn verify(&self, deployment: &MeasuredDeployment) -> Result<(), VerificationError> {
        let m = &deployment.runtime_measurements;
        let (Some(compose), Some(os)) = (&m.compose_hash, &m.os_image_hash) else {
            return Err(VerificationError::DeploymentProvenanceRejected);
        };
        let (compose, os) = (norm(compose), norm(os));
        if let Some(sink) = &self.observe {
            if let Some(last) = sink.lock().unwrap_or_else(|e| e.into_inner()).last_mut() {
                last.compose_hash = Some(compose);
                last.os_image_hash = Some(os);
            }
            return Ok(());
        }
        let matched = self
            .accepted
            .matched
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let ok = matched.iter().any(|i| {
            let d = &self.accepted.deployments[*i];
            d.compose_hashes.contains(&compose) && d.os_image_hash == os
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

/// Recorded evidence replayed in place of live fetches (tests). The nonces are the ones the
/// recording was fetched under, and the collaterals are checked at the time they were recorded.
pub struct Replay {
    pub model_collateral: Collateral,
    pub gateway_collateral: Collateral,
    /// A recorded verdict in place of NVIDIA's service. `None` runs the attestor's own GPU check
    /// against its NRAS URLs (a local stand-in).
    pub gpu: Option<Arc<dyn GpuEvidenceVerifier>>,
    pub nonce_model: String,
    pub nonce_gateway: String,
}

pub struct NearAiAttestor {
    policy: Policy,
    pccs_url: String,
    nras_url: String,
    nras_jwks_url: String,
    /// The time the GPU certificate chain is checked at.
    clock: Arc<dyn Clock>,
    replay: Option<Replay>,
}

/// What `status` says about the GPU check: whether the policy pins NVIDIA certificates.
pub fn gpu_chain_line(policy: &Policy) -> String {
    if policy.nvidia_pins.is_empty() {
        "GPU chain: not pinned (the policy pins no NVIDIA certificate; the verdict is checked against the key NVIDIA's service serves)".into()
    } else {
        let unchecked = policy
            .nvidia_pins
            .iter()
            .filter(|p| !p.official_source_verified)
            .count();
        format!(
            "GPU chain: pinned ({} certificates, {unchecked} not checked against an official NVIDIA source)",
            policy.nvidia_pins.len()
        )
    }
}

impl NearAiAttestor {
    pub fn new(policy: Policy) -> Self {
        Self {
            policy,
            pccs_url: DEFAULT_INTEL_PCCS_URL.into(),
            nras_url: DEFAULT_NVIDIA_NRAS_URL.into(),
            nras_jwks_url: crate::nras::JWKS_URL.into(),
            clock: Arc::new(SystemClock),
            replay: None,
        }
    }

    /// Where the GPU evidence and the signing keys are fetched from (tests).
    pub fn with_nras(mut self, url: impl Into<String>, jwks_url: impl Into<String>) -> Self {
        self.nras_url = url.into();
        self.nras_jwks_url = jwks_url.into();
        self
    }

    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// An attestor that checks recorded evidence served by a local server (tests).
    #[cfg(feature = "mock")]
    pub fn replaying(policy: Policy, replay: Replay) -> Self {
        Self {
            replay: Some(replay),
            ..Self::new(policy)
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
        let live = Collateral::Live {
            client: http.clone(),
            pccs_url: self.pccs_url.clone(),
        };
        // A policy with pinned NVIDIA certificates gets the chain check. One without keeps the
        // check against the JWKS key, so a build does not break on a policy signed before pins.
        let nras_live: Box<dyn GpuEvidenceVerifier> = if self.policy.nvidia_pins.is_empty() {
            let client = http.clone().unwrap_or_default();
            Box::new(
                NrasGpuEvidenceVerifier::with_client(client, &self.nras_url)
                    .with_jwks_url(&self.nras_jwks_url),
            )
        } else {
            Box::new(NrasChain::new(
                http.clone().unwrap_or_default(),
                &self.nras_url,
                &self.nras_jwks_url,
                &self.policy.nvidia_pins,
                self.clock.clone(),
            ))
        };
        let (collateral, collateral_gateway, nras): (_, _, &dyn GpuEvidenceVerifier) =
            match &self.replay {
                Some(r) => (
                    r.model_collateral.clone(),
                    r.gateway_collateral.clone(),
                    r.gpu.as_deref().unwrap_or(nras_live.as_ref()),
                ),
                None => (live.clone(), live, nras_live.as_ref()),
            };

        let fetched = match &self.replay {
            Some(r) => {
                api.fetch_model_attestations_with_nonce(
                    ask.model,
                    Some(SigningAlgo::Ed25519),
                    None,
                    r.nonce_model.clone(),
                )
                .await
            }
            None => {
                api.fetch_model_attestations(ask.model, Some(SigningAlgo::Ed25519), None)
                    .await
            }
        }
        .map_err(|e| format!("{e} ({})", e.code()))?;
        let tdx = IntelTdx::new(collateral, &self.policy, "model");
        let deployment = tdx.deployment_check();
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
                    gpu_evidence: Some(nras),
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

        let options = GatewayAttestationFetchOptions {
            signing_algo: Some(SigningAlgo::Ed25519),
            include_spki_fingerprint: false,
        };
        let gateway = match &self.replay {
            Some(r) => {
                api.fetch_gateway_attestation_with_nonce(options, r.nonce_gateway.clone())
                    .await
            }
            None => api.fetch_gateway_attestation(options).await,
        }
        .map_err(|e| format!("{e} ({})", e.code()))?;
        let tdx_gateway = IntelTdx::new(collateral_gateway, &self.policy, "gateway");
        let deployment_gateway = tdx_gateway.deployment_check();
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
        // The gateway's answer for a model signature may omit the kind; the SDK reads a bare
        // `signature` as `provider_tee`, and so do we.
        let mut signature = signature.clone();
        if let Some(o) = signature.as_object_mut() {
            if o.contains_key("signature") && !o.contains_key("signature_kind") {
                o.insert(
                    "signature_kind".into(),
                    Value::String("provider_tee".into()),
                );
            }
        }
        let wire: WireSignature = match serde_json::from_value(signature) {
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
