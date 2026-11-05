//! NRAS token and signing-chain verification at the operator's signed time.

use async_trait::async_trait;
use base64::{
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
    Engine,
};
use jsonwebtoken::{
    decode, decode_header, errors::ErrorKind, get_current_timestamp, jwk::JwkSet, Algorithm,
    DecodingKey, Validation,
};
use nearai_inference_sdk::{GpuEvidenceVerifier, VerificationError};
use reqwest::Client;
use rustls_pki_types::{CertificateDer, UnixTime};
use serde::Deserialize;
use serde_json::Value;
use x509_cert::{der::Decode, Certificate};

use crate::attest::NvidiaCertificate;

const NONCE_BYTES: usize = 32;
pub const DEFAULT_NVIDIA_JWKS_URL: &str =
    "https://nras.attestation.nvidia.com/.well-known/jwks.json";
const NVIDIA_ISSUER: &str = "https://nras.attestation.nvidia.com";

/// Checks NRAS responses against the certificate authorities in the signed policy.
pub(crate) struct PinnedNras {
    client: Client,
    url: String,
    jwks_url: String,
    pins: Vec<NvidiaCertificate>,
    at_secs: u64,
    #[cfg(feature = "mock")]
    jwt_at_secs: Option<u64>,
}

impl PinnedNras {
    pub(crate) fn new(
        client: Client,
        url: String,
        jwks_url: String,
        pins: Vec<NvidiaCertificate>,
        at_secs: u64,
    ) -> Self {
        Self {
            client,
            url,
            jwks_url,
            pins,
            at_secs,
            #[cfg(feature = "mock")]
            jwt_at_secs: None,
        }
    }
    #[cfg(feature = "mock")]
    pub(crate) fn with_jwt_time(mut self, at_secs: Option<u64>) -> Self {
        self.jwt_at_secs = at_secs;
        self
    }

    fn jwt_now(&self) -> u64 {
        #[cfg(feature = "mock")]
        if let Some(at) = self.jwt_at_secs {
            return at;
        }
        get_current_timestamp()
    }
}

#[async_trait]
impl GpuEvidenceVerifier for PinnedNras {
    async fn verify(&self, nvidia_payload: &str) -> Result<(), VerificationError> {
        let raw: Value = serde_json::from_str(nvidia_payload).map_err(|_| {
            VerificationError::GpuPayloadInvalid {
                reason: "invalid_json",
            }
        })?;
        let nonce = raw
            .as_object()
            .and_then(|object| object.get("nonce"))
            .and_then(Value::as_str)
            .ok_or(VerificationError::GpuPayloadInvalid {
                reason: "nonce_missing",
            })?;
        let nonce = hex::decode(
            nonce
                .strip_prefix("0x")
                .or_else(|| nonce.strip_prefix("0X"))
                .unwrap_or(nonce),
        )
        .ok()
        .filter(|n| n.len() == NONCE_BYTES)
        .ok_or_else(|| VerificationError::InvalidInput {
            field: "nvidia_payload.nonce".to_owned(),
            reason: "expected a 32-byte hexadecimal nonce".to_owned(),
        })?;
        let response = self
            .client
            .post(&self.url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(nvidia_payload.to_owned())
            .send()
            .await
            .map_err(|_| VerificationError::NrasRequestFailed {
                reason: "transport",
                status: None,
                retryable: true,
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(VerificationError::NrasRequestFailed {
                reason: "http_status",
                status: Some(status.as_u16()),
                retryable: status.as_u16() == 408
                    || status.as_u16() == 429
                    || status.is_server_error(),
            });
        }
        // Keep HTTP JSON decoding separate from NRAS wire decoding so an
        // invalid JSON document and an invalid NRAS envelope remain distinct
        // public failures.
        let raw: Value =
            response
                .json()
                .await
                .map_err(|_| VerificationError::NrasResponseInvalid {
                    reason: "invalid_json",
                })?;
        let jwt = decode_nras_overall_attestation_jwt(raw)?;
        let response = self.client.get(&self.jwks_url).send().await.map_err(|_| {
            VerificationError::NvidiaJwksRequestFailed {
                reason: "transport",
                status: None,
                retryable: true,
            }
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(VerificationError::NvidiaJwksRequestFailed {
                reason: "http_status",
                status: Some(status.as_u16()),
                retryable: status.as_u16() == 408
                    || status.as_u16() == 429
                    || status.is_server_error(),
            });
        }
        let jwks: JwkSet = response
            .json()
            .await
            .map_err(|_| invalid_nras_response("invalid_jwks"))?;
        verify_nras_jwt(
            &jwt,
            &jwks,
            &nonce,
            &self.pins,
            self.at_secs,
            self.jwt_now(),
        )
    }
}

/// NRAS returns a heterogeneous list. Only the first entry is part of the
/// contract consumed by this SDK; later entries remain intentionally opaque.
#[derive(Deserialize)]
#[serde(transparent)]
struct NrasResponseWire(Vec<Value>);

/// The first NRAS entry is also extensible. Decode it as an array and consume
/// only the documented `JWT` and token prefix.
#[derive(Deserialize)]
#[serde(transparent)]
struct NrasJwtEntryWire(Vec<Value>);

#[derive(Deserialize)]
struct NrasOverallAttestationJwtClaimsWire {
    #[serde(rename = "x-nvidia-overall-att-result")]
    overall_attestation_result: bool,
    eat_nonce: String,
    exp: u64,
    iat: u64,
    nbf: u64,
}

fn decode_nras_overall_attestation_jwt(raw: Value) -> Result<String, VerificationError> {
    let NrasResponseWire(entries) =
        serde_json::from_value(raw).map_err(|_| invalid_nras_response("invalid_schema"))?;
    let first = entries
        .into_iter()
        .next()
        .ok_or_else(|| invalid_nras_response("invalid_schema"))?;
    let NrasJwtEntryWire(entry) =
        serde_json::from_value(first).map_err(|_| invalid_nras_response("invalid_schema"))?;
    match (
        entry.first().and_then(Value::as_str),
        entry.get(1).and_then(Value::as_str),
    ) {
        (Some("JWT"), Some(jwt)) => Ok(jwt.to_owned()),
        _ => Err(invalid_nras_response("invalid_schema")),
    }
}

fn verify_nras_jwt(
    jwt: &str,
    jwks: &JwkSet,
    nonce: &[u8],
    pins: &[NvidiaCertificate],
    at_secs: u64,
    now: u64,
) -> Result<(), VerificationError> {
    let header_bytes = URL_SAFE_NO_PAD
        .decode(jwt.split('.').next().unwrap_or_default())
        .map_err(|_| jwt_failure("invalid_claims"))?;
    let raw_header: Value =
        serde_json::from_slice(&header_bytes).map_err(|_| jwt_failure("invalid_claims"))?;
    if raw_header["alg"].as_str() != Some("ES384") {
        return Err(jwt_failure("unsupported_algorithm"));
    }
    let header = decode_header(jwt).map_err(map_jwt_error)?;
    if header.alg != Algorithm::ES384 {
        return Err(jwt_failure("unsupported_algorithm"));
    }
    let kid = header.kid.ok_or_else(|| jwt_failure("key_not_found"))?;
    let jwk = jwks
        .find(&kid)
        .ok_or_else(|| jwt_failure("key_not_found"))?;
    check_chain(jwk, pins, at_secs)?;
    let key = DecodingKey::from_jwk(jwk).map_err(|_| invalid_nras_response("invalid_jwks"))?;
    let mut validation = Validation::new(Algorithm::ES384);
    validation.set_issuer(&[NVIDIA_ISSUER]);
    validation.set_required_spec_claims(&["exp", "nbf", "iss"]);
    validation.validate_nbf = false;
    validation.validate_exp = false;
    validation.validate_aud = false; // NRAS overall tokens have no audience contract.
    validation.leeway = 0;
    let claims = decode::<NrasOverallAttestationJwtClaimsWire>(jwt, &key, &validation)
        .map_err(map_jwt_error)?
        .claims;
    if claims.exp <= now {
        return Err(jwt_failure("expired"));
    }
    if claims.iat > now || claims.nbf > now {
        return Err(jwt_failure("not_yet_valid"));
    }
    let actual_nonce = hex::decode(&claims.eat_nonce).map_err(|_| jwt_failure("invalid_claims"))?;
    if actual_nonce.len() != NONCE_BYTES {
        return Err(jwt_failure("invalid_claims"));
    }
    if actual_nonce != nonce {
        return Err(jwt_failure("nonce_mismatch"));
    }
    if !claims.overall_attestation_result {
        return Err(VerificationError::GpuAttestationRejected { origin: "nras" });
    }
    Ok(())
}

fn map_jwt_error(error: jsonwebtoken::errors::Error) -> VerificationError {
    let reason = match error.kind() {
        ErrorKind::ExpiredSignature => "expired",
        ErrorKind::ImmatureSignature => "not_yet_valid",
        ErrorKind::InvalidSignature => "invalid_signature",
        ErrorKind::InvalidAlgorithm | ErrorKind::InvalidAlgorithmName => "unsupported_algorithm",
        _ => "invalid_claims",
    };
    jwt_failure(reason)
}

fn jwt_failure(reason: &'static str) -> VerificationError {
    VerificationError::NvidiaJwtVerificationFailed { reason }
}

fn invalid_nras_response(reason: &'static str) -> VerificationError {
    VerificationError::NrasResponseInvalid { reason }
}

fn certificate(der: &[u8]) -> Result<Certificate, VerificationError> {
    Certificate::from_der(der).map_err(|_| jwt_failure("certificate_invalid"))
}

fn check_validity(cert: &Certificate, at: u64, leaf: bool) -> Result<(), VerificationError> {
    let v = &cert.tbs_certificate.validity;
    if at < v.not_before.to_unix_duration().as_secs() {
        return Err(jwt_failure(if leaf {
            "leaf_not_yet_valid"
        } else {
            "intermediate_not_yet_valid"
        }));
    }
    if at > v.not_after.to_unix_duration().as_secs() {
        return Err(jwt_failure(if leaf {
            "leaf_expired"
        } else {
            "intermediate_expired"
        }));
    }
    Ok(())
}

fn check_chain(
    jwk: &jsonwebtoken::jwk::Jwk,
    pins: &[NvidiaCertificate],
    at: u64,
) -> Result<(), VerificationError> {
    let raw = serde_json::to_value(jwk).map_err(|_| invalid_nras_response("invalid_jwks"))?;
    let x5c = raw["x5c"]
        .as_array()
        .filter(|v| !v.is_empty())
        .ok_or_else(|| jwt_failure("x5c_missing"))?;
    let chain: Vec<CertificateDer<'static>> = x5c
        .iter()
        .map(|v| {
            let der = STANDARD
                .decode(
                    v.as_str()
                        .ok_or_else(|| jwt_failure("certificate_invalid"))?,
                )
                .map_err(|_| jwt_failure("certificate_invalid"))?;
            Ok(CertificateDer::from(der))
        })
        .collect::<Result<_, VerificationError>>()?;
    let leaf = certificate(&chain[0])?;
    // RFC 7517: the first certificate must hold the JWK's public key.
    let coordinate = |name: &str| -> Result<Vec<u8>, VerificationError> {
        URL_SAFE_NO_PAD
            .decode(
                raw[name]
                    .as_str()
                    .ok_or_else(|| jwt_failure("signing_key_outside_chain"))?,
            )
            .map_err(|_| jwt_failure("signing_key_outside_chain"))
    };
    let (x, y) = (coordinate("x")?, coordinate("y")?);
    let mut point = vec![4];
    point.extend_from_slice(&x);
    point.extend_from_slice(&y);
    let spki = &leaf.tbs_certificate.subject_public_key_info;
    if raw["kty"] != "EC"
        || raw["crv"] != "P-384"
        || x.len() != 48
        || y.len() != 48
        || spki.algorithm.oid.to_string() != "1.2.840.10045.2.1"
        || spki
            .algorithm
            .parameters
            .as_ref()
            .and_then(|v| v.decode_as::<x509_cert::der::asn1::ObjectIdentifier>().ok())
            .map(|oid| oid.to_string())
            .as_deref()
            != Some("1.3.132.0.34")
        || spki.subject_public_key.as_bytes() != Some(point.as_slice())
    {
        return Err(jwt_failure("signing_key_outside_chain"));
    }
    if leaf
        .tbs_certificate
        .get::<x509_cert::ext::pkix::KeyUsage>()
        .map_err(|_| jwt_failure("certificate_invalid"))?
        .is_some_and(|(_, usage)| !usage.digital_signature())
    {
        return Err(jwt_failure("signing_key_outside_chain"));
    }
    for (i, der) in chain.iter().enumerate() {
        check_validity(&certificate(der)?, at, i == 0)?;
    }
    let end = webpki::EndEntityCert::try_from(&chain[0])
        .map_err(|_| jwt_failure("certificate_invalid"))?;
    let mut invalid_time = None;
    for pin in pins {
        let der = STANDARD
            .decode(&pin.der)
            .map_err(|_| jwt_failure("certificate_invalid"))?;
        let cert = certificate(&der)?;
        let der = CertificateDer::from(der);
        let anchor = webpki::anchor_from_trusted_cert(&der)
            .map_err(|_| jwt_failure("certificate_invalid"))?;
        let anchors = [anchor];
        if end
            .verify_for_usage(
                webpki::ALL_VERIFICATION_ALGS,
                &anchors,
                &chain[1..],
                UnixTime::since_unix_epoch(std::time::Duration::from_secs(at)),
                webpki::KeyUsage::server_auth(),
                None,
                None,
            )
            .is_ok()
        {
            // Trust anchors are exempt from webpki's validity checks; policy pins are not.
            match check_validity(&cert, at, false) {
                Ok(()) => return Ok(()),
                Err(error) => invalid_time = Some(error),
            }
        }
    }
    Err(invalid_time.unwrap_or_else(|| jwt_failure("chain_wrong_root")))
}
