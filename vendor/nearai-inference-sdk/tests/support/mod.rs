// Each integration-test binary uses a different subset of these shared fixtures.
#![allow(dead_code)]

use async_trait::async_trait;
use ed25519_dalek::Signer;
use nearai_inference_sdk::{
    AttestationEventLog, AttestationEvidence, CompletionSignature, CompletionSignatureKind,
    DeploymentProvenanceStatus, GatewayAttestation, GpuEvidenceStatus, GpuEvidenceVerifier,
    ModelAttestation, SigningAlgo, SigningIdentity, TcbStatus, TdxQuoteVerificationResult,
    TdxQuoteVerifier, VerificationError, VerifiedAttestationEvidence, VerifiedGatewayAttestation,
    VerifiedModelAttestation,
};
use sha2::Digest;

pub const NONCE: &str = "1111111111111111111111111111111111111111111111111111111111111111";
pub const ECDSA_ADDRESS: &str = "2222222222222222222222222222222222222222";
pub const TLS_FINGERPRINT: &str =
    "3333333333333333333333333333333333333333333333333333333333333333";
pub const APP_COMPOSE: &str = "{\"services\":{\"model\":\"example@sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"}}";

#[derive(Clone)]
pub struct FixtureTdxQuoteVerifier(pub TdxQuoteVerificationResult);

#[async_trait]
impl TdxQuoteVerifier for FixtureTdxQuoteVerifier {
    async fn verify(
        &self,
        _intel_quote: &str,
    ) -> Result<TdxQuoteVerificationResult, VerificationError> {
        Ok(self.0.clone())
    }
}

pub struct FixtureGpuVerifier;

#[async_trait]
impl GpuEvidenceVerifier for FixtureGpuVerifier {
    async fn verify(&self, _payload: &str) -> Result<(), VerificationError> {
        Ok(())
    }
}

pub fn model_attestation(nvidia_payload: Option<&str>) -> ModelAttestation {
    let report_data = hex::encode(model_quote(TcbStatus::UpToDate).report_data);
    ModelAttestation {
        signing_public_key: None,
        evidence: AttestationEvidence {
            nonce: NONCE.to_owned(),
            signer: SigningIdentity {
                signing_algo: SigningAlgo::Ecdsa,
                signing_address: ECDSA_ADDRESS.to_owned(),
            },
            intel_quote: "fixture".to_owned(),
            event_log: AttestationEventLog::Entries(vec![serde_json::json!({
                "digest": "00".repeat(48),
                "imr": 3,
            })]),
            app_compose: APP_COMPOSE.to_owned(),
        },
        reported_quote_data: Some(report_data),
        nvidia_payload: nvidia_payload.map(ToOwned::to_owned),
    }
}

pub fn gateway_attestation() -> GatewayAttestation {
    let quote = gateway_tls_quote(TcbStatus::UpToDate);
    let reported_quote_data = hex::encode(&quote.report_data);
    GatewayAttestation {
        evidence: AttestationEvidence {
            nonce: NONCE.to_owned(),
            signer: SigningIdentity {
                signing_algo: SigningAlgo::Ecdsa,
                signing_address: ECDSA_ADDRESS.to_owned(),
            },
            intel_quote: "fixture".to_owned(),
            event_log: AttestationEventLog::Entries(vec![serde_json::json!({
                "digest": "00".repeat(48),
                "imr": 3,
            })]),
            app_compose: APP_COMPOSE.to_owned(),
        },
        spki_fingerprint: Some(TLS_FINGERPRINT.to_owned()),
        reported_quote_data,
    }
}

pub fn gateway_attestation_without_tls_binding() -> GatewayAttestation {
    let quote = model_quote(TcbStatus::UpToDate);
    let reported_quote_data = hex::encode(&quote.report_data);
    GatewayAttestation {
        evidence: AttestationEvidence {
            nonce: NONCE.to_owned(),
            signer: SigningIdentity {
                signing_algo: SigningAlgo::Ecdsa,
                signing_address: ECDSA_ADDRESS.to_owned(),
            },
            intel_quote: "fixture".to_owned(),
            event_log: AttestationEventLog::Entries(vec![serde_json::json!({
                "digest": "00".repeat(48),
                "imr": 3,
            })]),
            app_compose: APP_COMPOSE.to_owned(),
        },
        spki_fingerprint: None,
        reported_quote_data,
    }
}

pub fn model_quote(tcb_status: TcbStatus) -> TdxQuoteVerificationResult {
    quote_with_report_data(signer_nonce_report_data(), tcb_status)
}

pub fn gateway_tls_quote(tcb_status: TcbStatus) -> TdxQuoteVerificationResult {
    let mut input = hex::decode(ECDSA_ADDRESS).expect("fixture address is hexadecimal");
    input.extend(hex::decode(TLS_FINGERPRINT).expect("fixture fingerprint is hexadecimal"));
    quote_with_report_data(sha256_bytes(&input), tcb_status)
}

fn signer_nonce_report_data() -> Vec<u8> {
    let mut report_data = hex::decode(ECDSA_ADDRESS).expect("fixture address is hexadecimal");
    report_data.extend([0u8; 12]);
    report_data
}

fn quote_with_report_data(
    mut report_data: Vec<u8>,
    tcb_status: TcbStatus,
) -> TdxQuoteVerificationResult {
    report_data.extend(hex::decode(NONCE).expect("fixture nonce is hexadecimal"));

    let event_digest = vec![0u8; 48];
    let mut rtmr_input = vec![0u8; 48];
    rtmr_input.extend(event_digest);
    let mut mr_config_id = vec![0x01];
    mr_config_id.extend(sha256_bytes(APP_COMPOSE.as_bytes()));
    mr_config_id.extend([0u8; 15]);

    TdxQuoteVerificationResult {
        tcb_status,
        advisory_ids: vec![],
        debug_enabled: false,
        report_data,
        mr_config_id,
        rt_mr3: sha384_bytes(&rtmr_input),
    }
}

pub fn signed_signature(
    signing_key: &ed25519_dalek::SigningKey,
    kind: CompletionSignatureKind,
    signed_text: String,
    signer: SigningIdentity,
) -> CompletionSignature {
    CompletionSignature {
        kind,
        signature: hex::encode(signing_key.sign(signed_text.as_bytes()).to_bytes()),
        signed_text,
        signer,
    }
}

pub fn verified_model_attestation(signer: SigningIdentity) -> VerifiedModelAttestation {
    VerifiedModelAttestation {
        signing_public_key: None,
        evidence: verified_evidence(signer),
        gpu_evidence: GpuEvidenceStatus::NotProvided,
    }
}

pub fn verified_gateway_attestation(signer: SigningIdentity) -> VerifiedGatewayAttestation {
    VerifiedGatewayAttestation {
        evidence: verified_evidence(signer),
        tls_binding: nearai_inference_sdk::GatewayTlsBinding::Attested {
            spki_fingerprint: TLS_FINGERPRINT.to_owned(),
        },
    }
}

fn verified_evidence(signer: SigningIdentity) -> VerifiedAttestationEvidence {
    VerifiedAttestationEvidence {
        signer,
        tcb_status: TcbStatus::UpToDate,
        advisory_ids: vec![],
        deployment: nearai_inference_sdk::MeasuredDeployment {
            app_compose: APP_COMPOSE.to_owned(),
            runtime_measurements: Default::default(),
        },
        deployment_provenance: DeploymentProvenanceStatus::NotChecked,
    }
}

pub fn sha256_bytes(value: impl AsRef<[u8]>) -> Vec<u8> {
    sha2::Sha256::digest(value.as_ref()).to_vec()
}

fn sha384_bytes(value: impl AsRef<[u8]>) -> Vec<u8> {
    sha2::Sha384::digest(value.as_ref()).to_vec()
}

pub fn sha256_hex(value: impl AsRef<[u8]>) -> String {
    hex::encode(sha256_bytes(value))
}
