use crate::errors::VerificationError;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Caller-approved GitHub build identity. This approves a source, not a model
/// deployment: obtain the image digest from previously verified evidence.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ImageProvenancePolicy {
    /// GitHub source repository in `owner/repo` form.
    pub repository: String,
    /// Caller/source workflow path, for example `.github/workflows/build.yml`.
    pub workflow: String,
    /// Optional exact source Git ref, such as `refs/heads/main`.
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    /// Optional full source commit SHA, matched against both the SLSA statement
    /// and the signing certificate's authenticated source digest.
    pub commit: Option<String>,
    /// Expected certificate OIDC issuer.
    #[serde(default = "default_image_provenance_issuer")]
    pub issuer: String,
}

impl ImageProvenancePolicy {
    pub fn new(repository: String, workflow: String) -> Self {
        Self {
            repository,
            workflow,
            git_ref: None,
            commit: None,
            issuer: default_image_provenance_issuer(),
        }
    }
}

fn default_image_provenance_issuer() -> String {
    "https://token.actions.githubusercontent.com".to_owned()
}

/// Source information extracted only after the Sigstore bundle is verified.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VerifiedImageProvenance {
    pub digest: String,
    pub repository: String,
    pub workflow: String,
    /// Authenticated source ref, not the reusable signing workflow's ref.
    #[serde(rename = "ref")]
    pub git_ref: String,
    /// Source commit matched between the SLSA statement and signing certificate.
    pub commit: String,
    /// Authenticated signing workflow SAN URI; it may differ from the source.
    pub certificate_identity: String,
    pub issuer: String,
    pub predicate_type: String,
}

/// Why a candidate bundle failed image-provenance verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ImageProvenanceFailureReason {
    NoAttestations,
    InvalidBundle,
    UntrustedIdentity,
    InvalidStatement,
    DigestMismatch,
    SourceMismatch,
    CommitMismatch,
    TrustRootUnavailable,
}

/// Why the measured Compose images cannot be checked against the supplied policies.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentImagesFailureReason {
    EmptyPolicy,
    InvalidAppCompose,
    InvalidDockerCompose,
    UnresolvedImage,
    ImageMissing,
    ImageNotPinned,
}

/// Signature algorithms exposed by NEAR AI Cloud.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SigningAlgo {
    Ecdsa,
    Ed25519,
}

impl std::fmt::Display for SigningAlgo {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Ecdsa => formatter.write_str("ecdsa"),
            Self::Ed25519 => formatter.write_str("ed25519"),
        }
    }
}

/// Public identity of a key which signs a completion or attestation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SigningIdentity {
    pub signing_algo: SigningAlgo,
    pub signing_address: String,
}

/// dstack event-log data as returned by the attestation endpoint.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum AttestationEventLog {
    Json(String),
    Entries(Vec<Value>),
}

/// Evidence shared by NEAR model and Gateway attestations.
#[derive(Clone, Debug, PartialEq)]
pub struct AttestationEvidence {
    pub nonce: String,
    pub signer: SigningIdentity,
    pub intel_quote: String,
    pub event_log: AttestationEventLog,
    pub app_compose: String,
}

/// Raw model-serving TEE evidence returned through NEAR AI Cloud.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelAttestation {
    /// Optional encryption key; authenticated against the signer during verification.
    pub signing_public_key: Option<String>,
    pub evidence: AttestationEvidence,
    /// Optional server-declared copy of quote report data. An absent value
    /// becomes `None` at the Cloud API response boundary.
    pub reported_quote_data: Option<String>,
    /// NVIDIA evidence is optional for model deployments which do not expose it.
    /// Absent evidence is represented as `None`.
    pub nvidia_payload: Option<String>,
}

/// Raw NEAR AI Cloud Gateway evidence.
#[derive(Clone, Debug)]
pub struct GatewayAttestation {
    pub evidence: AttestationEvidence,
    /// SPKI fingerprint reported by the Gateway when evidence was fetched with
    /// `include_spki_fingerprint`. The quote authenticates this value only in
    /// the TLS-bound report-data layout.
    pub spki_fingerprint: Option<String>,
    /// Gateway reports always advertise the quote report-data copy.
    pub reported_quote_data: String,
}

/// Intel TDX status returned by a quote verifier.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum TcbStatus {
    #[serde(rename = "UpToDate")]
    UpToDate,
    #[serde(rename = "SWHardeningNeeded")]
    SwHardeningNeeded,
    #[serde(rename = "ConfigurationNeeded")]
    ConfigurationNeeded,
    #[serde(rename = "ConfigurationAndSWHardeningNeeded")]
    ConfigurationAndSwHardeningNeeded,
    #[serde(rename = "OutOfDate")]
    OutOfDate,
    #[serde(rename = "OutOfDateConfigurationNeeded")]
    OutOfDateConfigurationNeeded,
    #[serde(rename = "Revoked")]
    Revoked,
    #[serde(rename = "Unknown")]
    Unknown,
}

/// Facts returned by a TDX quote verifier before SDK policy and binding checks.
#[derive(Clone, Debug)]
pub struct TdxQuoteVerificationResult {
    pub tcb_status: TcbStatus,
    pub advisory_ids: Vec<String>,
    pub debug_enabled: bool,
    pub report_data: Vec<u8>,
    pub mr_config_id: Vec<u8>,
    pub rt_mr3: Vec<u8>,
}

/// A caller-supplied Intel TDX quote verifier. The SDK's default verifier uses
/// Intel DCAP through PCCS, but tests and specialized deployments can supply
/// their own implementation.
#[async_trait]
pub trait TdxQuoteVerifier: Send + Sync {
    async fn verify(
        &self,
        intel_quote: &str,
    ) -> Result<TdxQuoteVerificationResult, VerificationError>;
}

/// A caller-supplied GPU evidence verifier. The default implementation
/// delegates to NRAS over HTTPS.
#[async_trait]
pub trait GpuEvidenceVerifier: Send + Sync {
    async fn verify(&self, payload: &str) -> Result<(), VerificationError>;
}

/// A caller-owned deployment-provenance verifier. It receives measurements
/// that have already been authenticated by the Intel quote.
#[async_trait]
pub trait DeploymentVerifier: Send + Sync {
    async fn verify(&self, deployment: &MeasuredDeployment) -> Result<(), VerificationError>;
}

/// TCB statuses accepted during attestation verification.
#[derive(Clone, Debug, Default)]
pub struct AttestationPolicy {
    /// Defaults to `UpToDate` and `OutOfDate` when omitted.
    pub accepted_tcb_statuses: Option<Vec<TcbStatus>>,
}

/// Model-specific policy.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum GpuEvidenceRequirement {
    /// Verify a supplied payload; accept a report that does not provide one.
    #[default]
    IfPresent,
    /// Reject reports that do not provide GPU evidence.
    Required,
}

#[derive(Clone, Debug, Default)]
pub struct ModelAttestationPolicy {
    pub accepted_tcb_statuses: Option<Vec<TcbStatus>>,
    pub gpu_evidence: GpuEvidenceRequirement,
}

/// Optional verifier implementations used by a Gateway verification call.
#[derive(Default)]
pub struct AttestationVerifiers<'a> {
    pub tdx_quote: Option<&'a dyn TdxQuoteVerifier>,
    pub deployment: Option<&'a dyn DeploymentVerifier>,
}

/// Optional verifier implementations used by a model verification call.
#[derive(Default)]
pub struct ModelAttestationVerifiers<'a> {
    pub tdx_quote: Option<&'a dyn TdxQuoteVerifier>,
    pub deployment: Option<&'a dyn DeploymentVerifier>,
    pub gpu_evidence: Option<&'a dyn GpuEvidenceVerifier>,
}

/// Runtime measurements reconstructed from dstack's RTMR3 event log.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RuntimeMeasurements {
    pub os_image_hash: Option<String>,
    pub compose_hash: Option<String>,
}

/// Compose configuration and runtime measurements authenticated by a quote.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeasuredDeployment {
    pub app_compose: String,
    pub runtime_measurements: RuntimeMeasurements,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeploymentProvenanceStatus {
    NotChecked,
    Verified,
}

/// Measurements and signer identity established by a successful attestation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedAttestationEvidence {
    pub signer: SigningIdentity,
    pub tcb_status: TcbStatus,
    pub advisory_ids: Vec<String>,
    pub deployment: MeasuredDeployment,
    pub deployment_provenance: DeploymentProvenanceStatus,
}

/// TLS information authenticated for a Gateway report.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum GatewayTlsBinding {
    /// The attestation omitted an SPKI fingerprint, so only the signer-and-
    /// nonce report-data layout was verified.
    None,
    /// The quote-bound TLS fingerprint matched the peer observed by the
    /// client for this evidence request.
    Attested { spki_fingerprint: String },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GpuEvidenceStatus {
    NotProvided,
    Verified,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedModelAttestation {
    pub signing_public_key: Option<String>,
    pub evidence: VerifiedAttestationEvidence,
    pub gpu_evidence: GpuEvidenceStatus,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedGatewayAttestation {
    pub evidence: VerifiedAttestationEvidence,
    pub tls_binding: GatewayTlsBinding,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionSignatureKind {
    ProviderTee,
    Gateway,
}

/// Completion signature normalized from the Cloud API wire response.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CompletionSignature {
    pub kind: CompletionSignatureKind,
    pub signed_text: String,
    pub signature: String,
    pub signer: SigningIdentity,
}

/// Model evidence candidates and the client values used to obtain them.
#[derive(Clone, Debug)]
pub struct FetchedModelAttestations {
    pub attestations: Vec<ModelAttestation>,
    pub client_binding: ModelClientBinding,
}

/// Client values associated with a model-attestation request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelClientBinding {
    /// Fresh nonce sent in the model-attestation request.
    pub nonce: String,
}

/// Client values associated with a Gateway-attestation request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GatewayClientBinding {
    /// Fresh nonce sent in the Gateway-attestation request.
    pub nonce: String,
    /// SHA-256 SPKI fingerprint observed for that exact HTTPS request, when
    /// the runtime exposes peer certificate information.
    pub spki_fingerprint: Option<String>,
}

/// Options for fetching Gateway evidence from NEAR AI Cloud.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GatewayAttestationFetchOptions {
    /// Optional Gateway signing algorithm filter.
    pub signing_algo: Option<SigningAlgo>,
    /// Request Gateway TLS fingerprint evidence and capture the TLS peer for
    /// the same HTTPS request.
    pub include_spki_fingerprint: bool,
}

impl Default for GatewayAttestationFetchOptions {
    fn default() -> Self {
        Self {
            signing_algo: None,
            include_spki_fingerprint: true,
        }
    }
}

/// Gateway evidence and the client values associated with its request.
#[derive(Clone, Debug)]
pub struct FetchedGatewayAttestation {
    pub ohttp_attestation: Option<OhttpAttestation>,
    pub attestation: GatewayAttestation,
    pub client_binding: GatewayClientBinding,
}

/// Catalog capabilities are untrusted routing metadata, not attestation evidence.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelMetadata {
    pub provider_type: String,
    pub attestation_supported: bool,
}

#[derive(Clone, Debug, Deserialize)]
pub struct OhttpAttestation {
    pub signing_algo: SigningAlgo,
    pub signing_key: String,
    pub key_config: String,
    pub signature: String,
}
