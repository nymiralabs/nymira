use crate::types::{
    CompletionSignatureKind, DeploymentImagesFailureReason, ImageProvenanceFailureReason,
    SigningAlgo, TcbStatus,
};
use thiserror::Error;

/// External resource involved in an API failure.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApiResource {
    ModelAttestation,
    ModelMetadata,
    Completion,
    Ohttp,
    GatewayAttestation,
    CompletionSignature,
    ImageProvenance,
}

/// The external request stage that failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApiTransportReason {
    Request,
    ResponseBody,
}

impl std::fmt::Display for ApiTransportReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Request => "request",
            Self::ResponseBody => "response body",
        })
    }
}

impl std::fmt::Display for ApiResource {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let value = match self {
            Self::ModelMetadata => "model metadata",
            Self::Completion => "completion",
            Self::Ohttp => "OHTTP",
            Self::ModelAttestation => "model attestation",
            Self::GatewayAttestation => "gateway attestation",
            Self::CompletionSignature => "completion signature",
            Self::ImageProvenance => "image provenance",
        };
        formatter.write_str(value)
    }
}

/// Failures while retrieving or selecting external evidence.
#[derive(Clone, Debug, Error)]
pub enum ApiError {
    #[error("completion not retained; consume the entire response before verification")]
    CompletionNotFound,
    #[error("invalid API input {field}: {reason}")]
    InvalidInput {
        field: String,
        reason: String,
        expected: Option<String>,
        actual: Option<String>,
    },

    #[error("API {resource} {reason} failed")]
    Transport {
        resource: ApiResource,
        reason: ApiTransportReason,
    },

    #[error("API {resource} returned HTTP {status}")]
    HttpStatus { resource: ApiResource, status: u16 },

    #[error("API {resource} returned invalid JSON")]
    InvalidJson { resource: ApiResource },

    #[error("API response has an invalid {path}: expected {expected}, received {actual}")]
    InvalidResponse {
        path: String,
        expected: String,
        actual: String,
    },

    #[error("{resource} response nonce does not match the request")]
    NonceMismatch { resource: ApiResource },

    #[error("Cloud API returned no model attestation for the requested signer")]
    ModelAttestationSignerNotFound,

    #[error(
        "Cloud API returned {matching_count} model attestations for the requested signer ({total_count} total)"
    )]
    AmbiguousModelAttestationSigner {
        matching_count: usize,
        total_count: usize,
    },

    #[error(
        "Cloud API did not provide a completion signature ({provider_error_code}): {provider_message}"
    )]
    CompletionSignatureUnavailable {
        provider_error_code: String,
        provider_message: String,
    },
}

impl ApiError {
    /// Stable machine-readable error code shared with the TypeScript SDK.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::CompletionNotFound => "api.completion_not_found",
            Self::InvalidInput { .. } => "api.invalid_input",
            Self::Transport { .. } => "api.transport_failed",
            Self::HttpStatus { .. } => "api.http_status",
            Self::InvalidJson { .. } => "api.invalid_json",
            Self::InvalidResponse { .. } => "api.invalid_response",
            Self::NonceMismatch { .. } => "api.nonce_mismatch",
            Self::ModelAttestationSignerNotFound => "api.model_attestation_signer_not_found",
            Self::AmbiguousModelAttestationSigner { .. } => {
                "api.ambiguous_model_attestation_signer"
            }
            Self::CompletionSignatureUnavailable { .. } => "api.completion_signature_unavailable",
        }
    }

    /// Whether retrying the same operation may reasonably succeed.
    pub const fn retryable(&self) -> bool {
        match self {
            Self::Transport { .. } => true,
            Self::HttpStatus { resource, status } => {
                (matches!(resource, ApiResource::CompletionSignature) && *status == 404)
                    || *status == 408
                    || *status == 425
                    || *status == 429
                    || *status >= 500
            }
            _ => false,
        }
    }
}

/// Local attestation, policy, measurement, or response-signature failure.
///
/// Consumers should branch on [`Self::code`] rather than the display message.
#[derive(Clone, Debug, Error)]
pub enum VerificationError {
    #[error("{code}: {reason}")]
    Protocol { code: &'static str, reason: String },
    #[error("invalid input {field}: {reason}")]
    InvalidInput { field: String, reason: String },

    #[error("Intel quote collateral is unavailable")]
    QuoteCollateralUnavailable,

    #[error("Intel quote verification failed: {reason}")]
    QuoteVerificationFailed { reason: &'static str },

    #[error("Intel quote verifier returned an invalid result: {reason}")]
    QuoteInvalidResult { reason: String },

    #[error("Intel quote report type is unsupported; expected TD10")]
    QuoteUnsupportedReportType,

    #[error("quote was created with debug enabled")]
    DebugEnabled,

    #[error("TCB status {actual:?} is not accepted by policy")]
    TcbStatusNotAllowed {
        actual: TcbStatus,
        accepted: Vec<TcbStatus>,
        advisory_ids: Vec<String>,
    },

    #[error("GPU evidence is required by policy but was not provided")]
    GpuEvidenceRequired,

    #[error("Gateway attestation requires an observed TLS peer fingerprint")]
    SpkiFingerprintRequired,

    #[error("attestation nonce binding did not match ({binding})")]
    NonceMismatch { binding: &'static str },

    #[error("quote report data is invalid: {reason}")]
    ReportDataInvalid { reason: &'static str },

    #[error("quote report data binding did not match ({binding})")]
    ReportDataMismatch { binding: &'static str },

    #[error("attestation TLS SPKI fingerprint does not match the observed peer")]
    SpkiFingerprintMismatch,

    #[error("event log is invalid at {path}: {reason}")]
    EventLogInvalid { path: String, reason: &'static str },

    #[error("RTMR3 replay did not match: {reason}")]
    Rtmr3Mismatch { reason: &'static str },

    #[error("MRCONFIGID is invalid: {reason}")]
    MrConfigIdInvalid { reason: &'static str },

    #[error("app compose does not match the MRCONFIGID measurement")]
    AppComposeMrConfigIdMismatch,

    #[error("NVIDIA payload is invalid: {reason}")]
    GpuPayloadInvalid { reason: &'static str },

    #[error("NVIDIA NRAS request failed: {reason}")]
    NrasRequestFailed {
        reason: &'static str,
        status: Option<u16>,
        retryable: bool,
    },

    #[error("NVIDIA NRAS response is invalid: {reason}")]
    NrasResponseInvalid { reason: &'static str },

    #[error("NVIDIA JWKS request failed: {reason}")]
    NvidiaJwksRequestFailed {
        reason: &'static str,
        status: Option<u16>,
        retryable: bool,
    },

    #[error("NVIDIA JWT verification failed: {reason}")]
    NvidiaJwtVerificationFailed { reason: &'static str },

    #[error("NVIDIA attestation was rejected ({origin})")]
    GpuAttestationRejected { origin: &'static str },

    #[error("deployment provenance verifier rejected the measured deployment")]
    DeploymentProvenanceRejected,

    #[error("no trusted image provenance for {digest}: {reasons:?}")]
    ImageProvenanceVerificationFailed {
        digest: String,
        reasons: Vec<ImageProvenanceFailureReason>,
    },

    #[error("deployment images cannot be verified: {reason:?}")]
    DeploymentImagesInvalid {
        reason: DeploymentImagesFailureReason,
        image_repository: Option<String>,
        service: Option<String>,
    },

    #[error("image provenance request failed for {image_repository}@{digest}")]
    ImageProvenanceRequestFailed {
        image_repository: String,
        digest: String,
        #[source]
        source: Box<ApiError>,
    },

    #[error("completion signature kind {actual:?} cannot be used here; expected {expected:?}")]
    SignatureKindMismatch {
        expected: CompletionSignatureKind,
        actual: CompletionSignatureKind,
    },

    #[error("completion signed payload does not match ({reason})")]
    SignaturePayloadMismatch { reason: &'static str },

    #[error("completion signature format is invalid for {field}: {reason}")]
    SignatureFormatInvalid {
        field: &'static str,
        reason: &'static str,
    },

    #[error("completion signature is invalid for {signing_algo:?}")]
    SignatureInvalid { signing_algo: SigningAlgo },

    #[error("completion signature signer does not match verified attestation evidence")]
    SignatureSignerMismatch,
}

impl VerificationError {
    /// Stable machine-readable error code shared with the TypeScript SDK.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Protocol { code, .. } => code,
            Self::InvalidInput { .. } => "input.invalid",
            Self::QuoteCollateralUnavailable => "quote.collateral_unavailable",
            Self::QuoteVerificationFailed { .. } => "quote.verification_failed",
            Self::QuoteInvalidResult { .. } => "quote.invalid_result",
            Self::QuoteUnsupportedReportType => "quote.unsupported_report_type",
            Self::DebugEnabled => "policy.debug_enabled",
            Self::TcbStatusNotAllowed { .. } => "policy.tcb_status_not_allowed",
            Self::GpuEvidenceRequired => "policy.gpu_evidence_required",
            Self::SpkiFingerprintRequired => "binding.spki_fingerprint_required",
            Self::NonceMismatch { .. } => "binding.nonce_mismatch",
            Self::ReportDataInvalid { .. } => "binding.report_data_invalid",
            Self::ReportDataMismatch { .. } => "binding.report_data_mismatch",
            Self::SpkiFingerprintMismatch => "binding.spki_fingerprint_mismatch",
            Self::EventLogInvalid { .. } => "measurement.event_log_invalid",
            Self::Rtmr3Mismatch { .. } => "measurement.rtmr3_mismatch",
            Self::MrConfigIdInvalid { .. } => "measurement.mrconfigid_invalid",
            Self::AppComposeMrConfigIdMismatch => "measurement.app_compose_mrconfigid_mismatch",
            Self::GpuPayloadInvalid { .. } => "gpu.payload_invalid",
            Self::NrasRequestFailed { .. } => "gpu.nras_request_failed",
            Self::NrasResponseInvalid { .. } => "gpu.nras_response_invalid",
            Self::NvidiaJwksRequestFailed { .. } => "gpu.jwks_request_failed",
            Self::NvidiaJwtVerificationFailed { .. } => "gpu.jwt_verification_failed",
            Self::GpuAttestationRejected { .. } => "gpu.attestation_rejected",
            Self::DeploymentProvenanceRejected => "provenance.verification_failed",
            Self::ImageProvenanceVerificationFailed { .. } => {
                "provenance.image_verification_failed"
            }
            Self::DeploymentImagesInvalid { .. } => "provenance.deployment_images_invalid",
            Self::ImageProvenanceRequestFailed { .. } => "provenance.image_request_failed",
            Self::SignatureKindMismatch { .. } => "signature.kind_mismatch",
            Self::SignaturePayloadMismatch { .. } => "signature.payload_mismatch",
            Self::SignatureFormatInvalid { .. } => "signature.format_invalid",
            Self::SignatureInvalid { .. } => "signature.invalid",
            Self::SignatureSignerMismatch => "signature.signer_mismatch",
        }
    }

    /// True only for failures where a retry might change the result.
    pub const fn retryable(&self) -> bool {
        match self {
            Self::QuoteCollateralUnavailable => true,
            Self::NrasRequestFailed { retryable, .. } => *retryable,
            Self::NvidiaJwksRequestFailed { retryable, .. } => *retryable,
            Self::ImageProvenanceRequestFailed { source, .. } => source.retryable(),
            _ => false,
        }
    }
}

/// Integrated operations can fail at either the retrieval or verification stage.
#[derive(Clone, Debug, Error)]
pub enum InferenceError {
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error(transparent)]
    Verification(#[from] VerificationError),
}
impl InferenceError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Api(e) => e.code(),
            Self::Verification(e) => e.code(),
        }
    }
    pub fn retryable(&self) -> bool {
        match self {
            Self::Api(e) => e.retryable(),
            Self::Verification(e) => e.retryable(),
        }
    }
}
pub(crate) fn protocol(code: &'static str, reason: impl Into<String>) -> VerificationError {
    VerificationError::Protocol {
        code,
        reason: reason.into(),
    }
}
