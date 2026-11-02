use crate::attestation::{verify_dstack_deployment, verify_dstack_quote};
use crate::bindings::{verify_report_data_binding, verify_reported_nonce};
use crate::errors::VerificationError;
use crate::nvidia::NrasGpuEvidenceVerifier;
use crate::types::{
    AttestationPolicy, GpuEvidenceRequirement, GpuEvidenceStatus, GpuEvidenceVerifier,
    ModelAttestation, ModelAttestationPolicy, ModelAttestationVerifiers, ModelClientBinding,
    VerifiedModelAttestation,
};
use serde_json::Value;

/// Verify model evidence returned through NEAR AI Cloud. A successful model
/// verification establishes the model signing identity and deployment
/// measurements, but not a client-to-model TLS connection: the client talks to
/// the Cloud Gateway rather than the upstream model CVM.
pub async fn verify_model_attestation(
    attestation: &ModelAttestation,
    client_binding: &ModelClientBinding,
    policy: Option<&ModelAttestationPolicy>,
    verifiers: ModelAttestationVerifiers<'_>,
) -> Result<VerifiedModelAttestation, VerificationError> {
    verify_model_with_tls(attestation, client_binding, policy, verifiers, None).await
}

pub(crate) async fn verify_model_with_tls(
    attestation: &ModelAttestation,
    client_binding: &ModelClientBinding,
    policy: Option<&ModelAttestationPolicy>,
    verifiers: ModelAttestationVerifiers<'_>,
    fingerprint: Option<&str>,
) -> Result<VerifiedModelAttestation, VerificationError> {
    let common_policy = policy.map(|policy| AttestationPolicy {
        accepted_tcb_statuses: policy.accepted_tcb_statuses.clone(),
    });
    let verified_quote = verify_dstack_quote(
        &attestation.evidence,
        &client_binding.nonce,
        common_policy.as_ref(),
        verifiers.tdx_quote,
        attestation.reported_quote_data.as_deref(),
    )
    .await?;
    if let Some(fingerprint) = fingerprint {
        crate::bindings::verify_report_data_binding_with_tls_fingerprint(
            &verified_quote.quote.report_data,
            &client_binding.nonce,
            &verified_quote.signer.signing_address,
            fingerprint,
            fingerprint,
        )?;
    } else {
        verify_report_data_binding(
            &verified_quote.quote.report_data,
            &client_binding.nonce,
            &verified_quote.signer.signing_address,
        )?;
    }
    let evidence = verify_dstack_deployment(&verified_quote, verifiers.deployment).await?;
    let gpu_evidence = verify_gpu_evidence(
        attestation.nvidia_payload.as_deref(),
        &client_binding.nonce,
        policy
            .map(|policy| policy.gpu_evidence)
            .unwrap_or(GpuEvidenceRequirement::IfPresent),
        verifiers.gpu_evidence,
    )
    .await?;
    Ok(VerifiedModelAttestation {
        signing_public_key: verify_signing_public_key(attestation)?,
        evidence,
        gpu_evidence,
    })
}

async fn verify_gpu_evidence(
    payload: Option<&str>,
    nonce: &str,
    requirement: GpuEvidenceRequirement,
    verifier: Option<&dyn GpuEvidenceVerifier>,
) -> Result<GpuEvidenceStatus, VerificationError> {
    let Some(payload) = payload else {
        return match requirement {
            GpuEvidenceRequirement::IfPresent => Ok(GpuEvidenceStatus::NotProvided),
            GpuEvidenceRequirement::Required => Err(VerificationError::GpuEvidenceRequired),
        };
    };
    let parsed: Value =
        serde_json::from_str(payload).map_err(|_| VerificationError::GpuPayloadInvalid {
            reason: "invalid_json",
        })?;
    let reported_nonce = parsed
        .as_object()
        .and_then(|object| object.get("nonce"))
        .and_then(Value::as_str)
        .ok_or(VerificationError::GpuPayloadInvalid {
            reason: "nonce_missing",
        })?;
    verify_reported_nonce(reported_nonce, nonce, "nvidia_payload")?;

    let default_verifier = NrasGpuEvidenceVerifier::default();
    let verifier = verifier.unwrap_or(&default_verifier);
    verifier.verify(payload).await?;
    Ok(GpuEvidenceStatus::Verified)
}

fn verify_signing_public_key(
    attestation: &ModelAttestation,
) -> Result<Option<String>, VerificationError> {
    use crate::{errors::protocol, util::decode_hex, SigningAlgo};
    use sha3::Digest;
    let Some(key) = &attestation.signing_public_key else {
        return Ok(None);
    };
    let invalid = || {
        protocol(
            "binding.model_public_key_mismatch",
            "model key does not match the attested signer",
        )
    };
    let mut key = decode_hex(key).map_err(|_| invalid())?;
    let signer = &attestation.evidence.signer;
    let address = decode_hex(&signer.signing_address).map_err(|_| invalid())?;
    let matches = match signer.signing_algo {
        SigningAlgo::Ed25519 => key.len() == 32 && key == address,
        SigningAlgo::Ecdsa => {
            if key.len() == 65 && key[0] == 4 {
                key.remove(0);
            }
            let mut encoded = vec![4];
            encoded.extend_from_slice(&key);
            key.len() == 64
                && k256::PublicKey::from_sec1_bytes(&encoded).is_ok()
                && sha3::Keccak256::digest(&key)[12..] == address
        }
    };
    if !matches {
        return Err(invalid());
    }
    Ok(Some(hex::encode(key)))
}
