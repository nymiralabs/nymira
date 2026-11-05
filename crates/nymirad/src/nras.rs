//! NVIDIA's GPU verdict, checked down to the certificate chain of its signing key.
//!
//! The vendored SDK checks the NRAS token against the JWKS key it is served, and never reads
//! `x5c`. That is enough when the JWKS comes straight from NVIDIA over TLS. It is not enough
//! when something else relays it: a relay could sign its own verdict and serve its own key. This
//! verifier takes the `x5c` chain of the signing key and requires it to reach a certificate
//! pinned in the signed attestation policy, with every certificate valid at the time given by
//! the clock. The keys themselves are never pinned: the JWKS holds 200+ of them and they rotate
//! every few minutes.
//!
//! What a pass means: ES384 signature by the leaf of a pinned chain, issuer, `exp`, `nbf`, `iat`,
//! `eat_nonce` equal to the nonce we sent, and a true overall result.
//!
//! Not checked: the per-device tokens inside the NRAS answer, and certificate revocation.

use std::sync::Arc;

use async_trait::async_trait;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine;
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use nearai_inference_sdk::{GpuEvidenceVerifier, VerificationError};
use nymira_operator::clock::Clock;
use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use x509_parser::prelude::{ASN1Time, FromDer, X509Certificate};

use crate::attest::NvidiaPin;

pub const NRAS_URL: &str = "https://nras.attestation.nvidia.com/v3/attest/gpu";
pub const JWKS_URL: &str = "https://nras.attestation.nvidia.com/.well-known/jwks.json";
const ISSUER: &str = "https://nras.attestation.nvidia.com";
const NONCE_BYTES: usize = 32;

/// Submits GPU evidence to `url`, fetches the keys from `jwks_url`, and checks the verdict
/// against the pins.
pub struct NrasChain {
    client: reqwest::Client,
    url: String,
    jwks_url: String,
    pins: Vec<[u8; 32]>,
    clock: Arc<dyn Clock>,
}

impl NrasChain {
    /// `pins` are the policy's pinned certificates. A pin that is not valid hex of 32 bytes is
    /// dropped (the policy loader refuses such a policy before it gets here).
    pub fn new(
        client: reqwest::Client,
        url: impl Into<String>,
        jwks_url: impl Into<String>,
        pins: &[NvidiaPin],
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            client,
            url: url.into(),
            jwks_url: jwks_url.into(),
            pins: pins
                .iter()
                .filter_map(|p| hex::decode(&p.sha256).ok()?.try_into().ok())
                .collect(),
            clock,
        }
    }
}

fn fail(reason: &'static str) -> VerificationError {
    VerificationError::NvidiaJwtVerificationFailed { reason }
}

fn invalid_response(reason: &'static str) -> VerificationError {
    VerificationError::NrasResponseInvalid { reason }
}

fn retryable(status: u16) -> bool {
    status == 408 || status == 429 || status >= 500
}

#[async_trait]
impl GpuEvidenceVerifier for NrasChain {
    async fn verify(&self, nvidia_payload: &str) -> Result<(), VerificationError> {
        let raw: Value = serde_json::from_str(nvidia_payload).map_err(|_| {
            VerificationError::GpuPayloadInvalid {
                reason: "invalid_json",
            }
        })?;
        let nonce = raw.get("nonce").and_then(Value::as_str).ok_or(
            VerificationError::GpuPayloadInvalid {
                reason: "nonce_missing",
            },
        )?;
        let nonce = hex::decode(nonce)
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
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(VerificationError::NrasRequestFailed {
                reason: "http_status",
                status: Some(status),
                retryable: retryable(status),
            });
        }
        let answer: Value = response
            .json()
            .await
            .map_err(|_| invalid_response("invalid_json"))?;
        let jwt = overall_token(&answer)?;
        let response = self.client.get(&self.jwks_url).send().await.map_err(|_| {
            VerificationError::NvidiaJwksRequestFailed {
                reason: "transport",
                status: None,
                retryable: true,
            }
        })?;
        let status = response.status().as_u16();
        if !response.status().is_success() {
            return Err(VerificationError::NvidiaJwksRequestFailed {
                reason: "http_status",
                status: Some(status),
                retryable: retryable(status),
            });
        }
        let jwks: Value = response
            .json()
            .await
            .map_err(|_| invalid_response("invalid_jwks"))?;
        check_token(jwt, &jwks, &nonce, &self.pins, self.clock.now_secs())
    }
}

/// The first NRAS entry is `["JWT", "<token>", ...]`; later entries are opaque.
fn overall_token(answer: &Value) -> Result<&str, VerificationError> {
    let entry = answer.get(0).and_then(Value::as_array);
    match (
        entry.and_then(|e| e.first()).and_then(Value::as_str),
        entry.and_then(|e| e.get(1)).and_then(Value::as_str),
    ) {
        (Some("JWT"), Some(jwt)) => Ok(jwt),
        _ => Err(invalid_response("invalid_schema")),
    }
}

#[derive(Deserialize)]
struct Claims {
    #[serde(rename = "x-nvidia-overall-att-result")]
    overall: bool,
    eat_nonce: String,
    exp: u64,
    iat: u64,
    nbf: u64,
}

fn check_token(
    jwt: &str,
    jwks: &Value,
    nonce: &[u8],
    pins: &[[u8; 32]],
    now: u64,
) -> Result<(), VerificationError> {
    let header = decode_header(jwt).map_err(|_| fail("invalid_claims"))?;
    if header.alg != Algorithm::ES384 {
        return Err(fail("unsupported_algorithm"));
    }
    let kid = header.kid.as_deref().ok_or_else(|| fail("key_not_found"))?;
    let jwk = jwks["keys"]
        .as_array()
        .and_then(|keys| keys.iter().find(|k| k["kid"] == kid))
        .ok_or_else(|| fail("key_not_found"))?;

    // The chain comes with the JWKS key; a token that carries its own is the fallback.
    let chain: Vec<String> = match jwk["x5c"].as_array() {
        Some(list) => list
            .iter()
            .filter_map(|c| c.as_str().map(str::to_owned))
            .collect(),
        None => header.x5c.clone().unwrap_or_default(),
    };
    if chain.is_empty() {
        return Err(fail("x5c_missing"));
    }
    let ders: Vec<Vec<u8>> = chain
        .iter()
        .map(|c| STANDARD.decode(c).map_err(|_| fail("x5c_invalid")))
        .collect::<Result<_, _>>()?;
    let certs: Vec<X509Certificate> = ders
        .iter()
        .map(|d| {
            X509Certificate::from_der(d)
                .map(|(_, c)| c)
                .map_err(|_| fail("x5c_invalid"))
        })
        .collect::<Result<_, _>>()?;
    check_chain(&certs, &ders, pins, now)?;

    // The key that signed the token must be the leaf's own key.
    let point = certs[0].public_key().subject_public_key.data.as_ref();
    if point.len() != 97 || point[0] != 4 {
        return Err(fail("x5c_invalid"));
    }
    let (x, y) = (
        URL_SAFE_NO_PAD.encode(&point[1..49]),
        URL_SAFE_NO_PAD.encode(&point[49..]),
    );
    if let (Some(jx), Some(jy)) = (jwk["x"].as_str(), jwk["y"].as_str()) {
        if jx != x || jy != y {
            return Err(fail("key_not_in_chain"));
        }
    }
    let key = DecodingKey::from_ec_components(&x, &y).map_err(|_| fail("x5c_invalid"))?;

    let mut validation = Validation::new(Algorithm::ES384);
    validation.set_issuer(&[ISSUER]);
    validation.set_required_spec_claims(&["iss"]);
    validation.validate_exp = false;
    validation.validate_nbf = false;
    validation.validate_aud = false;
    validation.leeway = 0;
    let claims = decode::<Claims>(jwt, &key, &validation)
        .map_err(|e| match e.kind() {
            ErrorKind::InvalidSignature => fail("invalid_signature"),
            _ => fail("invalid_claims"),
        })?
        .claims;
    if claims.exp <= now {
        return Err(fail("expired"));
    }
    if claims.nbf > now || claims.iat > now {
        return Err(fail("not_yet_valid"));
    }
    let actual = hex::decode(&claims.eat_nonce).map_err(|_| fail("invalid_claims"))?;
    if actual.len() != NONCE_BYTES {
        return Err(fail("invalid_claims"));
    }
    if actual != nonce {
        return Err(fail("nonce_mismatch"));
    }
    if !claims.overall {
        return Err(VerificationError::GpuAttestationRejected { origin: "nras" });
    }
    Ok(())
}

/// Walks from the leaf up to the first certificate whose DER hash is pinned. Every certificate
/// on the way, the pinned one included, must be valid at `now`; every other one must be signed by
/// the next and the next must be a CA. A chain that ends before a pin is refused.
fn check_chain(
    certs: &[X509Certificate],
    ders: &[Vec<u8>],
    pins: &[[u8; 32]],
    now: u64,
) -> Result<(), VerificationError> {
    let at = ASN1Time::from_timestamp(i64::try_from(now).map_err(|_| fail("x5c_invalid"))?)
        .map_err(|_| fail("x5c_invalid"))?;
    for (i, cert) in certs.iter().enumerate() {
        let validity = cert.validity();
        if at < validity.not_before {
            return Err(fail("certificate_not_yet_valid"));
        }
        if at > validity.not_after {
            return Err(fail(if i == 0 {
                "leaf_expired"
            } else {
                "intermediate_expired"
            }));
        }
        let hash: [u8; 32] = Sha256::digest(&ders[i]).into();
        if pins.contains(&hash) {
            return Ok(());
        }
        let issuer = certs.get(i + 1).ok_or_else(|| fail("chain_untrusted"))?;
        if !issuer.is_ca() {
            return Err(fail("chain_untrusted"));
        }
        cert.verify_signature(Some(issuer.public_key()))
            .map_err(|_| fail("chain_untrusted"))?;
    }
    Err(fail("chain_untrusted"))
}
