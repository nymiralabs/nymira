use crate::bindings::{
    validate_signing_identity, verify_advertised_report_data,
    verify_app_compose_mrconfigid_binding, verify_reported_nonce,
};
use crate::errors::VerificationError;
use crate::event_log::verify_and_replay_rtmr3;
use crate::quote::DefaultTdxQuoteVerifier;
use crate::types::{
    AttestationEvidence, AttestationPolicy, DeploymentProvenanceStatus, DeploymentVerifier,
    MeasuredDeployment, SigningIdentity, TcbStatus, TdxQuoteVerificationResult, TdxQuoteVerifier,
    VerifiedAttestationEvidence,
};

const DEFAULT_ACCEPTED_TCB_STATUSES: [TcbStatus; 2] = [TcbStatus::UpToDate, TcbStatus::OutOfDate];

pub(crate) struct VerifiedDstackQuote<'a> {
    pub attestation: &'a AttestationEvidence,
    pub quote: TdxQuoteVerificationResult,
    pub signer: SigningIdentity,
}

pub(crate) async fn verify_dstack_quote<'a>(
    attestation: &'a AttestationEvidence,
    nonce: &str,
    policy: Option<&AttestationPolicy>,
    tdx_quote_verifier: Option<&dyn TdxQuoteVerifier>,
    advertised_report_data: Option<&str>,
) -> Result<VerifiedDstackQuote<'a>, VerificationError> {
    verify_reported_nonce(&attestation.nonce, nonce, "attestation_nonce")?;
    let signer = validate_signing_identity(&attestation.signer)?;
    let default_verifier = DefaultTdxQuoteVerifier::default();
    let verifier = tdx_quote_verifier.unwrap_or(&default_verifier);
    let quote = verifier.verify(&attestation.intel_quote).await?;

    verify_advertised_report_data(advertised_report_data, &quote.report_data)?;
    if quote.debug_enabled {
        return Err(VerificationError::DebugEnabled);
    }
    let accepted = policy
        .and_then(|policy| policy.accepted_tcb_statuses.as_deref())
        .unwrap_or(&DEFAULT_ACCEPTED_TCB_STATUSES);
    if !accepted.contains(&quote.tcb_status) {
        return Err(VerificationError::TcbStatusNotAllowed {
            actual: quote.tcb_status,
            accepted: accepted.to_vec(),
            advisory_ids: quote.advisory_ids.clone(),
        });
    }
    Ok(VerifiedDstackQuote {
        attestation,
        quote,
        signer,
    })
}

pub(crate) async fn verify_dstack_deployment(
    verified: &VerifiedDstackQuote<'_>,
    deployment_verifier: Option<&dyn DeploymentVerifier>,
) -> Result<VerifiedAttestationEvidence, VerificationError> {
    let runtime_measurements =
        verify_and_replay_rtmr3(&verified.attestation.event_log, &verified.quote.rt_mr3)?;
    verify_app_compose_mrconfigid_binding(
        &verified.attestation.app_compose,
        &verified.quote.mr_config_id,
    )?;
    let deployment = MeasuredDeployment {
        app_compose: verified.attestation.app_compose.clone(),
        runtime_measurements,
    };
    let deployment_provenance = if let Some(verifier) = deployment_verifier {
        verifier.verify(&deployment).await?;
        DeploymentProvenanceStatus::Verified
    } else {
        DeploymentProvenanceStatus::NotChecked
    };
    Ok(VerifiedAttestationEvidence {
        signer: verified.signer.clone(),
        tcb_status: verified.quote.tcb_status,
        advisory_ids: verified.quote.advisory_ids.clone(),
        deployment,
        deployment_provenance,
    })
}
