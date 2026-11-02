use crate::errors::VerificationError;
use crate::types::GpuEvidenceVerifier;
use crate::util::{require_hex_length, NONCE_BYTES};
use async_trait::async_trait;
use jsonwebtoken::errors::ErrorKind;
use jsonwebtoken::jwk::JwkSet;
use jsonwebtoken::{
    decode, decode_header, get_current_timestamp, Algorithm, DecodingKey, Validation,
};
use reqwest::Client;
use serde::Deserialize;
use serde_json::Value;

pub const DEFAULT_NVIDIA_NRAS_URL: &str = "https://nras.attestation.nvidia.com/v3/attest/gpu";
pub const DEFAULT_NVIDIA_JWKS_URL: &str =
    "https://nras.attestation.nvidia.com/.well-known/jwks.json";
const NVIDIA_ISSUER: &str = "https://nras.attestation.nvidia.com";

/// Default NVIDIA adapter. It submits the payload to NRAS and
/// verifies the overall JWT's ES384 signature, issuer, timestamps and nonce.
/// Detached device claims are not consumed by this adapter.
/// Custom submission and JWKS URLs retain the required NVIDIA issuer. Use only
/// a trusted JWKS proxy: its keys are used to authenticate the signed verdict.
#[derive(Clone, Debug)]
pub struct NrasGpuEvidenceVerifier {
    client: Client,
    url: String,
    jwks_url: String,
}

impl Default for NrasGpuEvidenceVerifier {
    fn default() -> Self {
        Self::new(DEFAULT_NVIDIA_NRAS_URL)
    }
}

impl NrasGpuEvidenceVerifier {
    /// Submit evidence to this URL, using NVIDIA's official JWKS URL by default.
    pub fn new(url: impl Into<String>) -> Self {
        let client = Client::new();
        Self {
            client,
            url: url.into(),
            jwks_url: DEFAULT_NVIDIA_JWKS_URL.to_owned(),
        }
    }

    pub fn with_client(client: Client, url: impl Into<String>) -> Self {
        Self {
            client,
            url: url.into(),
            jwks_url: DEFAULT_NVIDIA_JWKS_URL.to_owned(),
        }
    }

    /// Fetch JWT signing keys from a trusted JWKS proxy. The required NVIDIA
    /// issuer, signature algorithm, nonce, timestamps and verdict remain checked.
    pub fn with_jwks_url(mut self, url: impl Into<String>) -> Self {
        self.jwks_url = url.into();
        self
    }
}

#[async_trait]
impl GpuEvidenceVerifier for NrasGpuEvidenceVerifier {
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
        let nonce = require_hex_length(nonce, NONCE_BYTES).map_err(|_| {
            VerificationError::InvalidInput {
                field: "nvidia_payload.nonce".to_owned(),
                reason: "expected a 32-byte hexadecimal nonce".to_owned(),
            }
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
        verify_nras_jwt(&jwt, &jwks, &nonce)
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
    #[serde(rename = "nbf")]
    _nbf: u64,
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

fn verify_nras_jwt(jwt: &str, jwks: &JwkSet, nonce: &[u8]) -> Result<(), VerificationError> {
    let header = decode_header(jwt).map_err(map_jwt_error)?;
    if header.alg != Algorithm::ES384 {
        return Err(jwt_failure("unsupported_algorithm"));
    }
    let kid = header.kid.ok_or_else(|| jwt_failure("key_not_found"))?;
    let jwk = jwks
        .find(&kid)
        .ok_or_else(|| jwt_failure("key_not_found"))?;
    let key = DecodingKey::from_jwk(jwk).map_err(|_| invalid_nras_response("invalid_jwks"))?;
    let mut validation = Validation::new(Algorithm::ES384);
    validation.set_issuer(&[NVIDIA_ISSUER]);
    validation.set_required_spec_claims(&["exp", "nbf", "iss"]);
    validation.validate_nbf = true;
    validation.validate_aud = false; // NRAS overall tokens have no audience contract.
    validation.leeway = 0;
    let claims = decode::<NrasOverallAttestationJwtClaimsWire>(jwt, &key, &validation)
        .map_err(map_jwt_error)?
        .claims;
    let now = get_current_timestamp();
    if claims.exp <= now {
        return Err(jwt_failure("expired"));
    }
    if claims.iat > now {
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

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine;
    use jsonwebtoken::{encode, EncodingKey, Header};
    use serde_json::json;
    use wiremock::{
        matchers::{body_string, header, method, path},
        Mock, MockServer, ResponseTemplate,
    };

    // Generated solely for tests, with no external signing authority.
    const TEST_KEY: &str = "MIG2AgEAMBAGByqGSM49AgEGBSuBBAAiBIGeMIGbAgEBBDCePTWKqKn0TfSE/GYSS2wdy5aHKuyYx9xqZ+zTvl+nUZcb1nD+Lh2wXFWw08fD3tuhZANiAAT24KHioVPwh2q+byRqsjf4RSQzHwTypoDo87l/T6cxCM/U5tO7HGrgk0Xjw6XVPe4vGTppRZpDqsia4KGU7M1ehd2V4U37KgNps9qPHLrSt7/n4j3mzrLWuE3/Gm2sUD0=";

    fn test_jwks() -> JwkSet {
        serde_json::from_value(json!({"keys": [{
            "kid": "test-nras", "kty": "EC", "crv": "P-384",
            "x": "9uCh4qFT8Idqvm8karI3-EUkMx8E8qaA6PO5f0-nMQjP1ObTuxxq4JNF48Ol1T3u",
            "y": "Lxk6aUWaQ6rImuChlOzNXoXdleFN-yoDabPajxy60re_5-I95s6y1rhN_xptrFA9"
        }]}))
        .unwrap()
    }

    fn claims() -> Value {
        let now = get_current_timestamp();
        json!({
            "iss": NVIDIA_ISSUER, "exp": now + 3600, "nbf": now - 60,
            "iat": now - 60, "eat_nonce": "11".repeat(32),
            "x-nvidia-overall-att-result": true,
        })
    }

    fn sign_claims(claims: &Value, kid: &str) -> String {
        let key_bytes = base64::engine::general_purpose::STANDARD
            .decode(TEST_KEY)
            .unwrap();
        let key = EncodingKey::from_ec_der(&key_bytes);
        let mut header = Header::new(Algorithm::ES384);
        header.kid = Some(kid.to_owned());
        encode(&header, claims, &key).unwrap()
    }

    #[test]
    fn verifies_the_signed_overall_token() {
        let jwt = sign_claims(&claims(), "test-nras");
        let raw = json!([
            ["JWT", jwt, {"extra_entry_value": true}],
            {"later_entry": "ignored"},
        ]);

        let jwt = decode_nras_overall_attestation_jwt(raw).unwrap();
        verify_nras_jwt(&jwt, &test_jwks(), &[0x11; NONCE_BYTES]).unwrap();
    }

    #[test]
    fn rejects_an_invalid_nras_jwt_envelope() {
        let error = decode_nras_overall_attestation_jwt(json!([["TOKEN", "value"]])).unwrap_err();

        assert!(matches!(
            error,
            VerificationError::NrasResponseInvalid {
                reason: "invalid_schema"
            }
        ));
    }

    #[test]
    fn rejects_unacceptable_signed_claims() {
        for (name, value, reason) in [
            ("exp", json!(1), "expired"),
            ("nbf", json!(4102444800_u64), "not_yet_valid"),
            ("iat", json!(4102444800_u64), "not_yet_valid"),
            ("iss", json!("https://untrusted.example"), "invalid_claims"),
            ("exp", Value::Null, "invalid_claims"),
            ("eat_nonce", Value::Null, "invalid_claims"),
            ("eat_nonce", json!("44".repeat(32)), "nonce_mismatch"),
            (
                "x-nvidia-overall-att-result",
                json!("PASS"),
                "invalid_claims",
            ),
        ] {
            let mut claims = claims();
            claims[name] = value;
            let token = sign_claims(&claims, "test-nras");
            let error = verify_nras_jwt(&token, &test_jwks(), &[0x11; NONCE_BYTES]).unwrap_err();
            assert!(
                matches!(error, VerificationError::NvidiaJwtVerificationFailed { reason: actual } if actual == reason),
                "{name}: {error}"
            );
        }
    }

    #[test]
    fn rejects_a_modified_signature_and_unknown_key() {
        let mut token = sign_claims(&claims(), "test-nras");
        let signature_start = token.rfind('.').unwrap() + 1;
        let replacement = if &token[signature_start..signature_start + 1] == "A" {
            "B"
        } else {
            "A"
        };
        token.replace_range(signature_start..signature_start + 1, replacement);
        let error = verify_nras_jwt(&token, &test_jwks(), &[0x11; NONCE_BYTES]).unwrap_err();
        assert!(matches!(
            error,
            VerificationError::NvidiaJwtVerificationFailed {
                reason: "invalid_signature"
            }
        ));

        let token = sign_claims(&claims(), "unknown-key");
        let error = verify_nras_jwt(&token, &test_jwks(), &[0x11; NONCE_BYTES]).unwrap_err();
        assert!(matches!(
            error,
            VerificationError::NvidiaJwtVerificationFailed {
                reason: "key_not_found"
            }
        ));
    }

    #[test]
    fn rejects_an_unsigned_token_and_a_signed_false_verdict() {
        let error = verify_nras_jwt(
            "eyJhbGciOiJub25lIn0.e30.",
            &test_jwks(),
            &[0x11; NONCE_BYTES],
        )
        .unwrap_err();
        assert!(matches!(
            error,
            VerificationError::NvidiaJwtVerificationFailed {
                reason: "invalid_claims"
            }
        ));

        let mut claims = claims();
        claims["x-nvidia-overall-att-result"] = json!(false);
        let token = sign_claims(&claims, "test-nras");
        let error = verify_nras_jwt(&token, &test_jwks(), &[0x11; NONCE_BYTES]).unwrap_err();
        assert!(matches!(
            error,
            VerificationError::GpuAttestationRejected { origin: "nras" }
        ));
    }

    #[test]
    fn uses_the_official_nras_endpoint_by_default() {
        let verifier = NrasGpuEvidenceVerifier::default();
        assert_eq!(
            verifier.url,
            "https://nras.attestation.nvidia.com/v3/attest/gpu"
        );
        assert_eq!(
            verifier.jwks_url,
            "https://nras.attestation.nvidia.com/.well-known/jwks.json"
        );
    }

    #[tokio::test]
    async fn verifies_proxy_responses_with_custom_nras_and_jwks_urls_and_a_fixed_issuer() {
        for issuer in [NVIDIA_ISSUER, "https://proxy.example.com"] {
            let server = MockServer::start().await;
            let payload = json!({"nonce": "11".repeat(32)}).to_string();
            let mut claims = claims();
            claims["iss"] = json!(issuer);
            let jwt = sign_claims(&claims, "test-nras");
            Mock::given(method("POST"))
                .and(path("/proxy/attest"))
                .and(body_string(&payload))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!([["JWT", jwt]])))
                .expect(1)
                .mount(&server)
                .await;
            Mock::given(method("GET"))
                .and(path("/proxy/jwks"))
                .respond_with(ResponseTemplate::new(200).set_body_json(test_jwks()))
                .expect(1)
                .mount(&server)
                .await;

            let verifier = NrasGpuEvidenceVerifier::with_client(
                Client::builder().no_proxy().build().unwrap(),
                format!("{}/proxy/attest", server.uri()),
            )
            .with_jwks_url(format!("{}/proxy/jwks", server.uri()));
            let result = verifier.verify(&payload).await;
            if issuer == NVIDIA_ISSUER {
                result.unwrap();
            } else {
                assert!(matches!(
                    result.unwrap_err(),
                    VerificationError::NvidiaJwtVerificationFailed {
                        reason: "invalid_claims"
                    }
                ));
            }
        }
    }

    #[tokio::test]
    async fn submits_the_unchanged_payload_to_the_custom_endpoint() {
        let server = MockServer::start().await;
        let payload = format!(
            r#"{{ "nonce": "0X{}", "evidence": ["opaque"] }}"#,
            "AB".repeat(32)
        );
        Mock::given(method("POST"))
            .and(path("/proxy/nras"))
            .and(header("content-type", "application/json"))
            .and(body_string(&payload))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"result": true})))
            .expect(1)
            .mount(&server)
            .await;

        let verifier = NrasGpuEvidenceVerifier::new(format!("{}/proxy/nras", server.uri()));
        let error = verifier.verify(&payload).await.unwrap_err();

        // A successful proxy HTTP response is not sufficient attestation evidence.
        assert!(matches!(
            error,
            VerificationError::NrasResponseInvalid {
                reason: "invalid_schema"
            }
        ));
    }

    #[tokio::test]
    async fn rejects_invalid_payload_nonces_before_contacting_the_custom_endpoint() {
        let server = MockServer::start().await;
        let verifier = NrasGpuEvidenceVerifier::new(server.uri());
        for nonce in [
            "".to_owned(),
            "11".repeat(31),
            "11".repeat(33),
            "GG".repeat(32),
            "1".repeat(63),
        ] {
            let error = verifier
                .verify(&json!({"nonce": nonce}).to_string())
                .await
                .unwrap_err();
            assert!(matches!(
                error,
                VerificationError::InvalidInput { field, .. } if field == "nvidia_payload.nonce"
            ));
        }
        let array_payload = json!(["11".repeat(32)]).to_string();
        for payload in ["{}", "null", r#"{"nonce": 1}"#, &array_payload] {
            assert!(matches!(
                verifier.verify(payload).await.unwrap_err(),
                VerificationError::GpuPayloadInvalid {
                    reason: "nonce_missing"
                }
            ));
        }
        assert!(matches!(
            verifier.verify("{").await.unwrap_err(),
            VerificationError::GpuPayloadInvalid {
                reason: "invalid_json"
            }
        ));
        assert!(server.received_requests().await.unwrap().is_empty());
    }
}
