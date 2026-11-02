//! Verified Chat clients, encrypted transports, and verification primitives for
//! NEAR AI Cloud attestations and completion signatures.
//!
//! The public API deliberately separates three operations:
//! - fetch raw evidence from NEAR AI Cloud;
//! - verify a model or Gateway deployment; and
//! - verify an exact completion response against verified evidence.

mod attestation;
mod bindings;
mod cloud_api;
mod errors;
mod event_log;
mod gateway;
mod model;
mod nvidia;
mod quote;
mod response;
mod types;
mod util;

pub use cloud_api::{
    find_model_attestation_for_signature, AttestationClient, DEFAULT_NEAR_AI_CLOUD_BASE_URL,
    NO_ALIASING_HEADER,
};
pub use errors::{ApiError, ApiResource, ApiTransportReason, InferenceError, VerificationError};
pub use gateway::verify_gateway_attestation;
pub use model::verify_model_attestation;
pub use nvidia::{NrasGpuEvidenceVerifier, DEFAULT_NVIDIA_JWKS_URL, DEFAULT_NVIDIA_NRAS_URL};
pub use quote::{
    verify_tdx_quote, verify_tdx_quote_with_client, DefaultTdxQuoteVerifier, DEFAULT_INTEL_PCCS_URL,
};
pub use response::{verify_gateway_response, verify_model_response};
pub use types::*;

mod e2ee;
mod pinned_tls;
pub use e2ee::{prepare_e2ee_chat_request, E2eeModelKey, PreparedE2eeChatRequest};
pub use pinned_tls::{create_pinned_tls_client, create_pinned_tls_client_via};
mod direct;
mod inference;
mod ohttp;
mod sse;
pub use direct::{
    verify_direct_model_attestation, verify_direct_model_attestations,
    verify_direct_model_response, DirectAttestationClient, DirectAttestationVerificationResult,
    DirectClientBinding, DirectInferenceClient, DirectInferenceClientOptions,
    DirectModelAttestation, FetchedDirectModelAttestations, VerifiedDirectModelAttestation,
};
pub use inference::{
    AttestationVerificationResult, ByteStream, DeploymentPolicy, GatewayVerificationOptions,
    InferenceClient, InferenceClientOptions, InferenceResponse, ModelVerificationOptions,
    VerifiedCompletionAttestation, VerifiedCompletionResult,
};
pub use ohttp::{create_ohttp_client, verify_ohttp_key_config, OhttpClient};
