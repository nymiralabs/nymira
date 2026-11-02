mod support;

use async_trait::async_trait;
use nearai_inference_sdk::{
    verify_gateway_attestation, AttestationPolicy, AttestationVerifiers,
    DeploymentProvenanceStatus, DeploymentVerifier, GatewayClientBinding, GatewayTlsBinding,
    MeasuredDeployment, TcbStatus, VerificationError,
};
use support::{
    gateway_attestation, gateway_attestation_without_tls_binding, gateway_tls_quote, model_quote,
    FixtureTdxQuoteVerifier, APP_COMPOSE, NONCE, TLS_FINGERPRINT,
};

fn client_binding(spki_fingerprint: Option<String>) -> GatewayClientBinding {
    GatewayClientBinding {
        nonce: NONCE.to_owned(),
        spki_fingerprint,
    }
}

struct ExpectedDeploymentVerifier;

#[async_trait]
impl DeploymentVerifier for ExpectedDeploymentVerifier {
    async fn verify(&self, deployment: &MeasuredDeployment) -> Result<(), VerificationError> {
        if deployment.app_compose == APP_COMPOSE {
            Ok(())
        } else {
            Err(VerificationError::DeploymentProvenanceRejected)
        }
    }
}

struct RejectingDeploymentVerifier;

#[async_trait]
impl DeploymentVerifier for RejectingDeploymentVerifier {
    async fn verify(&self, _deployment: &MeasuredDeployment) -> Result<(), VerificationError> {
        Err(VerificationError::DeploymentProvenanceRejected)
    }
}

#[tokio::test]
async fn binds_the_observed_tls_peer_when_gateway_attestation_reports_an_spki_fingerprint() {
    let tdx_quote = FixtureTdxQuoteVerifier(gateway_tls_quote(TcbStatus::UpToDate));
    let attestation = gateway_attestation();

    let verified = verify_gateway_attestation(
        &attestation,
        &client_binding(Some(TLS_FINGERPRINT.to_owned())),
        None,
        AttestationVerifiers {
            tdx_quote: Some(&tdx_quote),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(
        verified.tls_binding,
        GatewayTlsBinding::Attested {
            spki_fingerprint: TLS_FINGERPRINT.to_owned(),
        }
    );
}

#[tokio::test]
async fn requires_an_observed_spki_fingerprint_when_gateway_attestation_reports_one() {
    let tdx_quote = FixtureTdxQuoteVerifier(gateway_tls_quote(TcbStatus::UpToDate));
    let attestation = gateway_attestation();

    let error = verify_gateway_attestation(
        &attestation,
        &client_binding(None),
        None,
        AttestationVerifiers {
            tdx_quote: Some(&tdx_quote),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();

    assert!(matches!(error, VerificationError::SpkiFingerprintRequired));
    assert_eq!(error.code(), "binding.spki_fingerprint_required");
}

#[tokio::test]
async fn rejects_another_tls_peer() {
    let tdx_quote = FixtureTdxQuoteVerifier(gateway_tls_quote(TcbStatus::UpToDate));
    let attestation = gateway_attestation();

    let error = verify_gateway_attestation(
        &attestation,
        &client_binding(Some("44".repeat(32))),
        None,
        AttestationVerifiers {
            tdx_quote: Some(&tdx_quote),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();

    assert!(matches!(error, VerificationError::SpkiFingerprintMismatch));
}

#[tokio::test]
async fn uses_signer_and_nonce_binding_when_gateway_attestation_has_no_spki_fingerprint() {
    let tdx_quote = FixtureTdxQuoteVerifier(model_quote(TcbStatus::UpToDate));
    let attestation = gateway_attestation_without_tls_binding();

    let verified = verify_gateway_attestation(
        &attestation,
        &client_binding(Some("44".repeat(32))),
        None,
        AttestationVerifiers {
            tdx_quote: Some(&tdx_quote),
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(verified.tls_binding, GatewayTlsBinding::None);
}

#[tokio::test]
async fn rejects_an_invalid_tls_fingerprint() {
    let tdx_quote = FixtureTdxQuoteVerifier(gateway_tls_quote(TcbStatus::UpToDate));
    let mut attestation = gateway_attestation();
    attestation.spki_fingerprint = Some(String::new());

    let error = verify_gateway_attestation(
        &attestation,
        &client_binding(Some(TLS_FINGERPRINT.to_owned())),
        None,
        AttestationVerifiers {
            tdx_quote: Some(&tdx_quote),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();

    assert!(
        matches!(error, VerificationError::InvalidInput { ref field, .. } if field == "attestation.spki_fingerprint")
    );
}

#[tokio::test]
async fn applies_an_explicit_gateway_tcb_policy() {
    let tdx_quote = FixtureTdxQuoteVerifier(gateway_tls_quote(TcbStatus::UpToDate));
    let attestation = gateway_attestation();
    let policy = AttestationPolicy {
        accepted_tcb_statuses: Some(vec![TcbStatus::OutOfDate]),
    };

    let error = verify_gateway_attestation(
        &attestation,
        &client_binding(Some(TLS_FINGERPRINT.to_owned())),
        Some(&policy),
        AttestationVerifiers {
            tdx_quote: Some(&tdx_quote),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        VerificationError::TcbStatusNotAllowed {
            actual: TcbStatus::UpToDate,
            accepted,
            ..
        } if accepted == vec![TcbStatus::OutOfDate]
    ));
}

#[tokio::test]
async fn runs_gateway_deployment_verifiers() {
    let tdx_quote = FixtureTdxQuoteVerifier(gateway_tls_quote(TcbStatus::UpToDate));
    let attestation = gateway_attestation();
    let deployment = ExpectedDeploymentVerifier;

    let verified = verify_gateway_attestation(
        &attestation,
        &client_binding(Some(TLS_FINGERPRINT.to_owned())),
        None,
        AttestationVerifiers {
            tdx_quote: Some(&tdx_quote),
            deployment: Some(&deployment),
        },
    )
    .await
    .unwrap();

    assert_eq!(
        verified.evidence.deployment_provenance,
        DeploymentProvenanceStatus::Verified
    );
}

#[tokio::test]
async fn propagates_a_rejected_gateway_deployment_verifier() {
    let tdx_quote = FixtureTdxQuoteVerifier(gateway_tls_quote(TcbStatus::UpToDate));
    let attestation = gateway_attestation();
    let deployment = RejectingDeploymentVerifier;

    let error = verify_gateway_attestation(
        &attestation,
        &client_binding(Some(TLS_FINGERPRINT.to_owned())),
        None,
        AttestationVerifiers {
            tdx_quote: Some(&tdx_quote),
            deployment: Some(&deployment),
        },
    )
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        VerificationError::DeploymentProvenanceRejected
    ));
}
