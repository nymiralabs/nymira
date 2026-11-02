use crate::errors::VerificationError;
use crate::types::{
    CompletionSignature, CompletionSignatureKind, SigningAlgo, SigningIdentity,
    VerifiedGatewayAttestation, VerifiedModelAttestation,
};
use crate::util::{decode_hex, sha256};
use ed25519_dalek::Verifier;
use serde_json::Value;
use sha3::Digest;

/// Verify a model-serving signature over exact completion bytes against the
/// signer established by verified model evidence.
pub fn verify_model_response(
    request_body: &[u8],
    response_body: &[u8],
    signature: &CompletionSignature,
    attestation: &VerifiedModelAttestation,
) -> Result<(), VerificationError> {
    require_signature_kind(signature, CompletionSignatureKind::ProviderTee)?;
    let model = canonical_model_id(request_body)?;
    let expected = format!(
        "{model}:{}:{}",
        hex::encode(sha256(request_body)),
        hex::encode(sha256(response_body))
    );
    verify_signature_text_and_bytes(signature, &expected)?;
    verify_signature_matches_attestation(signature, &attestation.evidence.signer)
}

/// Verify Gateway-service provenance and integrity for exact client-visible
/// completion bytes. This does not establish that a model TEE generated them.
pub fn verify_gateway_response(
    request_body: &[u8],
    response_body: &[u8],
    signature: &CompletionSignature,
    attestation: &VerifiedGatewayAttestation,
) -> Result<(), VerificationError> {
    require_signature_kind(signature, CompletionSignatureKind::Gateway)?;
    let expected = format!(
        "{}:{}",
        hex::encode(sha256(request_body)),
        hex::encode(sha256(response_body))
    );
    verify_signature_text_and_bytes(signature, &expected)?;
    verify_signature_matches_attestation(signature, &attestation.evidence.signer)
}

fn require_signature_kind(
    signature: &CompletionSignature,
    expected: CompletionSignatureKind,
) -> Result<(), VerificationError> {
    if signature.kind != expected {
        return Err(VerificationError::SignatureKindMismatch {
            expected,
            actual: signature.kind,
        });
    }
    Ok(())
}

fn canonical_model_id(request_body: &[u8]) -> Result<String, VerificationError> {
    let parsed: Value = serde_json::from_slice(request_body).map_err(|_| {
        VerificationError::SignaturePayloadMismatch {
            reason: "invalid_json",
        }
    })?;
    parsed
        .as_object()
        .and_then(|object| object.get("model"))
        .and_then(Value::as_str)
        .filter(|model| !model.is_empty())
        .map(ToOwned::to_owned)
        .ok_or(VerificationError::SignaturePayloadMismatch {
            reason: "missing_model",
        })
}

fn verify_signature_text_and_bytes(
    signature: &CompletionSignature,
    expected_text: &str,
) -> Result<(), VerificationError> {
    if signature.signed_text != expected_text {
        return Err(VerificationError::SignaturePayloadMismatch {
            reason: "text_mismatch",
        });
    }
    verify_signature_bytes(signature)
}

fn verify_signature_matches_attestation(
    signature: &CompletionSignature,
    signer: &SigningIdentity,
) -> Result<(), VerificationError> {
    let signature_address = decode_hex(&signature.signer.signing_address)
        .map_err(|_| VerificationError::SignatureSignerMismatch)?;
    let attestation_address = decode_hex(&signer.signing_address)
        .map_err(|_| VerificationError::SignatureSignerMismatch)?;
    if signature.signer.signing_algo != signer.signing_algo
        || signature_address != attestation_address
    {
        return Err(VerificationError::SignatureSignerMismatch);
    }
    Ok(())
}

fn verify_signature_bytes(signature: &CompletionSignature) -> Result<(), VerificationError> {
    match signature.signer.signing_algo {
        SigningAlgo::Ecdsa => verify_ecdsa_signature(signature),
        SigningAlgo::Ed25519 => verify_ed25519_signature(signature),
    }
}

fn verify_ecdsa_signature(signature: &CompletionSignature) -> Result<(), VerificationError> {
    let signature_bytes = signature_hex(&signature.signature, "signature")?;
    let signing_address =
        signature_hex(&signature.signer.signing_address, "signer.signing_address")?;
    if signature_bytes.len() != 65 {
        return Err(VerificationError::SignatureFormatInvalid {
            field: "signature",
            reason: "wrong_length",
        });
    }
    if signing_address.len() != 20 {
        return Err(VerificationError::SignatureFormatInvalid {
            field: "signer.signing_address",
            reason: "wrong_length",
        });
    }

    let mut eip191 = format!(
        "\x19Ethereum Signed Message:\n{}",
        signature.signed_text.len()
    )
    .into_bytes();
    eip191.extend_from_slice(signature.signed_text.as_bytes());
    let digest = sha3::Keccak256::digest(eip191);
    let recovery_id = normalize_recovery_id(signature_bytes[64])?;
    let parsed_signature =
        k256::ecdsa::Signature::from_slice(&signature_bytes[..64]).map_err(|_| {
            VerificationError::SignatureInvalid {
                signing_algo: SigningAlgo::Ecdsa,
            }
        })?;
    let key =
        k256::ecdsa::VerifyingKey::recover_from_prehash(&digest, &parsed_signature, recovery_id)
            .map_err(|_| VerificationError::SignatureInvalid {
                signing_algo: SigningAlgo::Ecdsa,
            })?;
    let point = key.to_encoded_point(false);
    let public_key = point.as_bytes();
    if public_key.len() != 65 || public_key[0] != 0x04 {
        return Err(VerificationError::SignatureInvalid {
            signing_algo: SigningAlgo::Ecdsa,
        });
    }
    let hash = sha3::Keccak256::digest(&public_key[1..]);
    if hash[12..] != signing_address {
        return Err(VerificationError::SignatureInvalid {
            signing_algo: SigningAlgo::Ecdsa,
        });
    }
    Ok(())
}

fn normalize_recovery_id(value: u8) -> Result<k256::ecdsa::RecoveryId, VerificationError> {
    let normalized = match value {
        27 | 28 => value - 27,
        value if value >= 35 => (value - 35) % 2,
        value => value,
    };
    k256::ecdsa::RecoveryId::try_from(normalized).map_err(|_| {
        VerificationError::SignatureFormatInvalid {
            field: "signature",
            reason: "invalid_recovery_id",
        }
    })
}

fn verify_ed25519_signature(signature: &CompletionSignature) -> Result<(), VerificationError> {
    let public_key = signature_hex(&signature.signer.signing_address, "signer.signing_address")?;
    let signed = signature_hex(&signature.signature, "signature")?;
    let public_key: [u8; 32] =
        public_key
            .try_into()
            .map_err(|_| VerificationError::SignatureFormatInvalid {
                field: "signer.signing_address",
                reason: "wrong_length",
            })?;
    let signed: [u8; 64] =
        signed
            .try_into()
            .map_err(|_| VerificationError::SignatureFormatInvalid {
                field: "signature",
                reason: "wrong_length",
            })?;
    let key = ed25519_dalek::VerifyingKey::from_bytes(&public_key).map_err(|_| {
        VerificationError::SignatureInvalid {
            signing_algo: SigningAlgo::Ed25519,
        }
    })?;
    let signed = ed25519_dalek::Signature::from_bytes(&signed);
    key.verify(signature.signed_text.as_bytes(), &signed)
        .map_err(|_| VerificationError::SignatureInvalid {
            signing_algo: SigningAlgo::Ed25519,
        })
}

fn signature_hex(value: &str, field: &'static str) -> Result<Vec<u8>, VerificationError> {
    decode_hex(value).map_err(|_| VerificationError::SignatureFormatInvalid {
        field,
        reason: "invalid_hex",
    })
}
