mod support;

use nearai_inference_sdk::{
    verify_gateway_response, verify_model_response, CompletionSignature, CompletionSignatureKind,
    SigningAlgo, SigningIdentity, VerificationError,
};
use sha3::Digest as _;
use support::{
    sha256_hex, signed_signature, verified_gateway_attestation, verified_model_attestation,
};

#[test]
fn uses_distinct_model_and_gateway_payloads() {
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
    let signing_address = hex::encode(signing_key.verifying_key().to_bytes());
    let request_body = br#"{"model":"glm-5.3-flash","messages":[]}"#;
    let response_body = br#"{"id":"response"}"#;
    let signer = SigningIdentity {
        signing_algo: SigningAlgo::Ed25519,
        signing_address,
    };
    let model_signature = signed_signature(
        &signing_key,
        CompletionSignatureKind::ProviderTee,
        format!(
            "glm-5.3-flash:{}:{}",
            sha256_hex(request_body),
            sha256_hex(response_body)
        ),
        signer.clone(),
    );
    let gateway_signature = signed_signature(
        &signing_key,
        CompletionSignatureKind::Gateway,
        format!("{}:{}", sha256_hex(request_body), sha256_hex(response_body)),
        signer.clone(),
    );
    let model_attestation = verified_model_attestation(signer.clone());
    let gateway_attestation = verified_gateway_attestation(signer);

    verify_model_response(
        request_body,
        response_body,
        &model_signature,
        &model_attestation,
    )
    .unwrap();
    verify_gateway_response(
        request_body,
        response_body,
        &gateway_signature,
        &gateway_attestation,
    )
    .unwrap();

    let mismatched_gateway_attestation = verified_gateway_attestation(SigningIdentity {
        signing_algo: SigningAlgo::Ed25519,
        signing_address: "11".repeat(32),
    });
    let error = verify_gateway_response(
        request_body,
        response_body,
        &gateway_signature,
        &mismatched_gateway_attestation,
    )
    .unwrap_err();
    assert!(matches!(error, VerificationError::SignatureSignerMismatch));

    let error = verify_model_response(
        request_body,
        response_body,
        &gateway_signature,
        &model_attestation,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        VerificationError::SignatureKindMismatch {
            expected: CompletionSignatureKind::ProviderTee,
            actual: CompletionSignatureKind::Gateway,
        }
    ));
}

#[test]
fn accepts_an_ethereum_personal_signature() {
    let signing_key = k256::ecdsa::SigningKey::from_bytes((&[1u8; 32]).into()).unwrap();
    let request_body = br#"{"model":"glm-5.3-flash","messages":[]}"#;
    let response_body = br#"{"id":"response"}"#;
    let signed_text = format!(
        "glm-5.3-flash:{}:{}",
        sha256_hex(request_body),
        sha256_hex(response_body)
    );
    let mut personal_message =
        format!("\x19Ethereum Signed Message:\n{}", signed_text.len()).into_bytes();
    personal_message.extend_from_slice(signed_text.as_bytes());
    let digest = sha3::Keccak256::digest(personal_message);
    let (signature, recovery_id) = signing_key.sign_prehash_recoverable(&digest).unwrap();
    let mut signature_bytes = signature.to_bytes().to_vec();
    signature_bytes.push(u8::from(recovery_id) + 27);

    let public_key = signing_key.verifying_key().to_encoded_point(false);
    let public_key_hash = sha3::Keccak256::digest(&public_key.as_bytes()[1..]);
    let signer = SigningIdentity {
        signing_algo: SigningAlgo::Ecdsa,
        signing_address: hex::encode(&public_key_hash[12..]),
    };
    let completion_signature = CompletionSignature {
        kind: CompletionSignatureKind::ProviderTee,
        signed_text,
        signature: hex::encode(signature_bytes),
        signer: signer.clone(),
    };
    let attestation = verified_model_attestation(signer);

    verify_model_response(
        request_body,
        response_body,
        &completion_signature,
        &attestation,
    )
    .unwrap();
}

#[test]
fn accepts_an_equivalent_hex_signing_address() {
    let signing_key = ed25519_dalek::SigningKey::from_bytes(&[9u8; 32]);
    let signing_address = hex::encode(signing_key.verifying_key().to_bytes());
    let request_body = br#"{"model":"glm-5.3-flash","messages":[]}"#;
    let response_body = br#"{"id":"response"}"#;
    let signed_text = format!(
        "glm-5.3-flash:{}:{}",
        sha256_hex(request_body),
        sha256_hex(response_body)
    );
    let signature = signed_signature(
        &signing_key,
        CompletionSignatureKind::ProviderTee,
        signed_text,
        SigningIdentity {
            signing_algo: SigningAlgo::Ed25519,
            signing_address: format!("0X{}", signing_address.to_uppercase()),
        },
    );
    let attestation = verified_model_attestation(SigningIdentity {
        signing_algo: SigningAlgo::Ed25519,
        signing_address,
    });

    verify_model_response(request_body, response_body, &signature, &attestation).unwrap();
}
