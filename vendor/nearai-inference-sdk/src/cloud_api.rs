use crate::errors::{ApiError, ApiResource, ApiTransportReason};
use crate::types::{
    AttestationEventLog, AttestationEvidence, CompletionSignature, CompletionSignatureKind,
    FetchedGatewayAttestation, FetchedModelAttestations, GatewayAttestation,
    GatewayAttestationFetchOptions, GatewayClientBinding, ModelAttestation, ModelClientBinding,
    SigningAlgo, SigningIdentity, VerifiedModelAttestation,
};
use crate::util::{decode_hex, generate_nonce, require_hex_length, sha256};
use reqwest::{
    header::{HeaderMap, HeaderName, HeaderValue, AUTHORIZATION},
    Client, Url,
};
use serde::{de::Error as _, Deserialize, Deserializer};
use x509_cert::{
    der::{Decode, Encode},
    Certificate,
};

/// Default production endpoint used by [`AttestationClient::new`].
pub const DEFAULT_NEAR_AI_CLOUD_BASE_URL: &str = "https://cloud-api.near.ai/v1";

/// Set this on model-attestation requests to reject aliases before dispatch.
pub const NO_ALIASING_HEADER: &str = "x-no-aliasing";

/// Client for retrieving attestation evidence and completion signatures from
/// NEAR AI Cloud.
///
/// The client owns its API key, base URL, and HTTP clients. Reuse one instance
/// for related evidence requests instead of passing the API key to each call.
#[derive(Clone)]
pub struct AttestationClient {
    headers: HeaderMap,
    api_key: String,
    base_url: Url,
    client: Client,
    gateway_client: Client,
}

impl AttestationClient {
    /// Create a client for the production Cloud API endpoint.
    pub fn new(api_key: String) -> Self {
        let base_url = parse_base_url(DEFAULT_NEAR_AI_CLOUD_BASE_URL)
            .expect("the SDK's default Cloud API URL is valid");
        Self::from_parts(api_key, base_url)
    }

    /// Create a client for an absolute Cloud API base URL, such as a staging
    /// endpoint. The base URL may include a path prefix such as `/v1`.
    pub fn with_base_url(api_key: String, base_url: &str) -> Result<Self, ApiError> {
        let base_url = parse_base_url(base_url)?;
        Ok(Self::from_parts(api_key, base_url))
    }

    fn from_parts(api_key: String, base_url: Url) -> Self {
        Self {
            api_key,
            base_url,
            headers: HeaderMap::new(),
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("valid client"),
            // A Gateway TLS binding must observe the Gateway's own peer, not
            // a system-configured HTTPS proxy. Other Cloud API requests keep
            // reqwest's normal proxy behavior.
            gateway_client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .no_proxy()
                .tls_info(true)
                .build()
                .expect("the SDK's default HTTP client configuration is valid"),
        }
    }

    /// Nymira patch: send every request, the Gateway binding fetch included, through a SOCKS5
    /// proxy such as `socks5h://identity:x@127.0.0.1:9050`. Names are resolved by the proxy.
    pub fn with_socks_proxy(mut self, proxy_url: &str) -> Result<Self, ApiError> {
        let proxy = reqwest::Proxy::all(proxy_url)
            .map_err(|_| invalid_api_input("socks_proxy", "invalid_proxy", None, None))?;
        let build = |gateway: bool| {
            let builder = Client::builder().redirect(reqwest::redirect::Policy::none());
            let builder = if gateway {
                // The binding must observe the Gateway's own peer; only our proxy is allowed.
                builder.no_proxy().tls_info(true)
            } else {
                builder.no_proxy()
            };
            builder.proxy(proxy.clone()).build()
        };
        self.client = build(false)
            .map_err(|_| invalid_api_input("socks_proxy", "invalid_proxy", None, None))?;
        self.gateway_client = build(true)
            .map_err(|_| invalid_api_input("socks_proxy", "invalid_proxy", None, None))?;
        Ok(self)
    }

    /// Use additional authentication or application headers on every evidence request.
    pub fn with_headers(mut self, headers: HeaderMap) -> Self {
        self.headers = headers;
        self
    }

    pub(crate) fn with_http_client(mut self, client: Client) -> Self {
        self.client = client.clone();
        self.gateway_client = client;
        self
    }

    pub async fn fetch_model_metadata(
        &self,
        model: &str,
    ) -> Result<crate::ModelMetadata, ApiError> {
        if model.is_empty() || matches!(model, "." | "..") {
            return Err(invalid_api_input("model", "invalid_model", None, None));
        }
        let mut url = self.endpoint("model/")?;
        url.path_segments_mut()
            .map_err(|_| invalid_base_url())?
            .pop_if_empty()
            .push(model);
        let response =
            get_cloud_api_response(self, url, ApiResource::ModelMetadata, None, false).await?;
        #[derive(Deserialize)]
        struct MetadataResponse {
            metadata: crate::ModelMetadata,
        }
        let response: MetadataResponse =
            decode_wire_response(&response.body, ApiResource::ModelMetadata, "model metadata")?;
        Ok(response.metadata)
    }

    pub(crate) async fn fetch_value(
        &self,
        path: &str,
        query: &[(&str, String)],
        resource: ApiResource,
    ) -> Result<serde_json::Value, ApiError> {
        let mut url = self.endpoint(path)?;
        url.query_pairs_mut()
            .extend_pairs(query.iter().map(|(k, v)| (*k, v.as_str())));
        let response = get_cloud_api_response(self, url, resource, None, false).await?;
        decode_wire_response(&response.body, resource, path)
    }

    pub(crate) async fn direct_signature(
        &self,
        id: &str,
        algo: Option<SigningAlgo>,
    ) -> Result<CompletionSignature, ApiError> {
        let mut url = self.endpoint("signature")?;
        url.path_segments_mut()
            .map_err(|_| invalid_base_url())?
            .push(id);
        if let Some(algo) = algo {
            url.query_pairs_mut()
                .append_pair("signing_algo", &algo.to_string());
        }
        let response =
            get_cloud_api_response(self, url, ApiResource::CompletionSignature, None, false)
                .await?;
        let mut value: serde_json::Value = decode_wire_response(
            &response.body,
            ApiResource::CompletionSignature,
            "signature",
        )?;
        if value.get("signature").is_some() && value.get("signature_kind").is_none() {
            value
                .as_object_mut()
                .ok_or_else(|| invalid_api_input("signature", "expected object", None, None))?
                .insert("signature_kind".into(), serde_json::json!("provider_tee"));
        }
        let wire = decode_wire_response(
            &value.to_string(),
            ApiResource::CompletionSignature,
            "signature",
        )?;
        let signature = map_completion_signature(wire)?;
        require_provider_signature(&signature)?;
        Ok(signature)
    }

    fn endpoint(&self, path: &str) -> Result<Url, ApiError> {
        self.base_url.join(path).map_err(|_| invalid_base_url())
    }
    /// Fetch model attestations for a canonical NEAR model ID.
    ///
    /// The result preserves Cloud API's `model_attestations` array and has a
    /// fresh nonce in its client binding. The optional signer fields only
    /// narrow the API response; local selection still matches the evidence
    /// signer.
    pub async fn fetch_model_attestations(
        &self,
        model: &str,
        signing_algo: Option<SigningAlgo>,
        signing_address: Option<&str>,
    ) -> Result<FetchedModelAttestations, ApiError> {
        self.fetch_model_attestations_with_nonce(
            model,
            signing_algo,
            signing_address,
            generate_nonce(),
        )
        .await
    }

    /// Nymira patch: [`Self::fetch_model_attestations`] with the nonce chosen by the caller, so a
    /// recorded report can be replayed against a local server. A live caller uses the function
    /// above, which draws a fresh nonce.
    pub async fn fetch_model_attestations_with_nonce(
        &self,
        model: &str,
        signing_algo: Option<SigningAlgo>,
        signing_address: Option<&str>,
        nonce: String,
    ) -> Result<FetchedModelAttestations, ApiError> {
        if let Some(signing_address) = signing_address {
            validate_input_signing_address(signing_address, signing_algo, "signing_address")?;
        }
        let mut url = self.endpoint("attestation/report")?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("model", model);
            query.append_pair("provider", "near");
            query.append_pair("nonce", &nonce);
            query.append_pair("include_tls_fingerprint", "false");
            if let Some(signing_algo) = signing_algo {
                query.append_pair("signing_algo", &signing_algo.to_string());
            }
            if let Some(signing_address) = signing_address {
                query.append_pair("signing_address", signing_address);
            }
        }
        let cloud_response = get_cloud_api_response(
            self,
            url,
            ApiResource::ModelAttestation,
            Some((NO_ALIASING_HEADER, "true")),
            false,
        )
        .await?;
        let response: WireModelAttestationResponse = decode_wire_response(
            &cloud_response.body,
            ApiResource::ModelAttestation,
            "model_attestations",
        )?;
        let attestations = response
            .model_attestations
            .into_iter()
            .enumerate()
            .map(|(index, attestation)| {
                map_model_attestation(attestation, &format!("model_attestations[{index}]"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        for attestation in &attestations {
            require_matching_api_nonce(
                &attestation.evidence.nonce,
                &nonce,
                ApiResource::ModelAttestation,
            )?;
        }
        Ok(FetchedModelAttestations {
            attestations,
            client_binding: ModelClientBinding { nonce },
        })
    }

    /// Fetch standalone Gateway evidence using the requested options.
    ///
    /// When `include_spki_fingerprint` is enabled, the built-in Gateway client
    /// also records the TLS peer certificate for this exact HTTPS request.
    pub async fn fetch_gateway_attestation(
        &self,
        options: GatewayAttestationFetchOptions,
    ) -> Result<FetchedGatewayAttestation, ApiError> {
        self.fetch_gateway_attestation_with_nonce(options, generate_nonce())
            .await
    }

    /// Nymira patch: [`Self::fetch_gateway_attestation`] with the nonce chosen by the caller.
    pub async fn fetch_gateway_attestation_with_nonce(
        &self,
        options: GatewayAttestationFetchOptions,
        nonce: String,
    ) -> Result<FetchedGatewayAttestation, ApiError> {
        let mut url = self.endpoint("attestation/report")?;
        {
            let mut query = url.query_pairs_mut();
            query.append_pair("nonce", &nonce);
            if let Some(signing_algo) = options.signing_algo {
                query.append_pair("signing_algo", &signing_algo.to_string());
            }
            query.append_pair(
                "include_tls_fingerprint",
                if options.include_spki_fingerprint {
                    "true"
                } else {
                    "false"
                },
            );
        }
        let cloud_response = get_cloud_api_response(
            self,
            url,
            ApiResource::GatewayAttestation,
            None,
            options.include_spki_fingerprint,
        )
        .await?;
        let response: WireGatewayAttestationResponse = decode_wire_response(
            &cloud_response.body,
            ApiResource::GatewayAttestation,
            "gateway_attestation",
        )?;
        let attestation =
            map_gateway_attestation(response.gateway_attestation, "gateway_attestation")?;
        match (
            options.include_spki_fingerprint,
            attestation.spki_fingerprint.is_some(),
        ) {
            (true, false) => {
                return Err(ApiError::InvalidResponse {
                    path: "gateway_attestation.tls_cert_fingerprint".to_owned(),
                    expected: "present".to_owned(),
                    actual: "missing".to_owned(),
                });
            }
            (false, true) => {
                return Err(ApiError::InvalidResponse {
                    path: "gateway_attestation.tls_cert_fingerprint".to_owned(),
                    expected: "missing".to_owned(),
                    actual: "present".to_owned(),
                });
            }
            _ => {}
        }
        require_matching_api_nonce(
            &attestation.evidence.nonce,
            &nonce,
            ApiResource::GatewayAttestation,
        )?;
        Ok(FetchedGatewayAttestation {
            ohttp_attestation: response.ohttp_attestation,
            attestation,
            client_binding: GatewayClientBinding {
                nonce,
                spki_fingerprint: cloud_response.peer_spki_fingerprint,
            },
        })
    }

    /// Fetch a completion signature, optionally filtered by signing algorithm.
    ///
    /// A valid 2xx unavailable envelope becomes
    /// [`ApiError::CompletionSignatureUnavailable`]. A 404 remains a
    /// retryable HTTP error because the completion may not have reached its
    /// terminal state yet.
    pub async fn fetch_completion_signature(
        &self,
        completion_id: &str,
        signing_algo: Option<SigningAlgo>,
    ) -> Result<CompletionSignature, ApiError> {
        let mut url = self.endpoint("signature")?;
        url.path_segments_mut()
            .map_err(|_| invalid_base_url())?
            .push(completion_id);
        if let Some(signing_algo) = signing_algo {
            url.query_pairs_mut()
                .append_pair("signing_algo", &signing_algo.to_string());
        }
        let cloud_response =
            get_cloud_api_response(self, url, ApiResource::CompletionSignature, None, false)
                .await?;
        let response: WireCompletionSignatureResponse = decode_wire_response(
            &cloud_response.body,
            ApiResource::CompletionSignature,
            "signature",
        )?;
        map_completion_signature(response)
    }
}

/// Select the exact verified model attestation matching a `provider_tee`
/// signature. It performs no response-signature verification itself.
pub fn find_model_attestation_for_signature<'a>(
    attestations: &'a [VerifiedModelAttestation],
    signature: &CompletionSignature,
) -> Result<&'a VerifiedModelAttestation, ApiError> {
    require_provider_signature(signature)?;
    let requested_signing_address = validate_input_signer(&signature.signer, "signature.signer")?;
    let mut matching_index = None;
    let mut matching_count = 0;

    for (index, attestation) in attestations.iter().enumerate() {
        let attestation_signing_address = validate_input_signer(
            &attestation.evidence.signer,
            &format!("attestations[{index}].signer"),
        )?;
        if attestation.evidence.signer.signing_algo == signature.signer.signing_algo
            && attestation_signing_address == requested_signing_address
        {
            matching_index.get_or_insert(index);
            matching_count += 1;
        }
    }

    let Some(index) = matching_index else {
        return Err(ApiError::ModelAttestationSignerNotFound);
    };
    if matching_count == 1 {
        return Ok(&attestations[index]);
    }
    Err(ApiError::AmbiguousModelAttestationSigner {
        matching_count,
        total_count: attestations.len(),
    })
}

async fn get_cloud_api_response(
    client: &AttestationClient,
    url: Url,
    resource: ApiResource,
    extra_header: Option<(&str, &str)>,
    capture_peer_spki_fingerprint: bool,
) -> Result<CloudApiResponse, ApiError> {
    let headers = build_cloud_api_headers(client, extra_header)?;
    let http_client = if capture_peer_spki_fingerprint {
        &client.gateway_client
    } else {
        &client.client
    };
    let response = http_client
        .get(url)
        .headers(headers)
        .send()
        .await
        .map_err(|_| ApiError::Transport {
            resource,
            reason: ApiTransportReason::Request,
        })?;
    let status = response.status().as_u16();
    let peer_spki_fingerprint = capture_peer_spki_fingerprint
        .then(|| peer_spki_fingerprint(&response))
        .flatten();
    let body = response.text().await.map_err(|_| ApiError::Transport {
        resource,
        reason: ApiTransportReason::ResponseBody,
    })?;
    if !(200..300).contains(&status) {
        return Err(ApiError::HttpStatus { resource, status });
    }
    Ok(CloudApiResponse {
        body,
        peer_spki_fingerprint,
    })
}

fn build_cloud_api_headers(
    client: &AttestationClient,
    extra_header: Option<(&str, &str)>,
) -> Result<HeaderMap, ApiError> {
    let mut headers = client.headers.clone();
    let authorization =
        HeaderValue::from_str(&format!("Bearer {}", client.api_key)).map_err(|_| {
            invalid_api_input(
                "api_key",
                "invalid_header_value",
                Some("an HTTP header value".to_owned()),
                None,
            )
        })?;
    if !client.api_key.is_empty() {
        headers.insert(AUTHORIZATION, authorization);
        headers.remove("api-key");
    }
    if let Some((name, value)) = extra_header {
        let name = HeaderName::from_bytes(name.as_bytes())
            .expect("the SDK only supplies static valid header names");
        let value =
            HeaderValue::from_str(value).expect("the SDK only supplies static valid header values");
        headers.insert(name, value);
    }
    Ok(headers)
}

fn peer_spki_fingerprint(response: &reqwest::Response) -> Option<String> {
    let certificate = response
        .extensions()
        .get::<reqwest::tls::TlsInfo>()?
        .peer_certificate()?;
    let certificate = Certificate::from_der(certificate).ok()?;
    let spki = certificate
        .tbs_certificate
        .subject_public_key_info
        .to_der()
        .ok()?;
    Some(hex::encode(sha256(spki)))
}

struct CloudApiResponse {
    body: String,
    peer_spki_fingerprint: Option<String>,
}

pub(crate) fn parse_base_url(value: &str) -> Result<Url, ApiError> {
    let mut url = Url::parse(value).map_err(|_| invalid_base_url())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid_base_url());
    }
    if !url.path().ends_with('/') {
        let path = format!("{}/", url.path());
        url.set_path(&path);
    }
    Ok(url)
}

fn invalid_base_url() -> ApiError {
    invalid_api_input(
        "base_url",
        "invalid_url",
        Some("an absolute HTTP(S) URL".to_owned()),
        None,
    )
}

fn require_provider_signature(signature: &CompletionSignature) -> Result<(), ApiError> {
    if signature.kind != CompletionSignatureKind::ProviderTee {
        return Err(invalid_api_input(
            "signature.kind",
            "unsupported_value",
            Some("provider_tee".to_owned()),
            Some(completion_signature_kind_name(signature.kind).to_owned()),
        ));
    }
    Ok(())
}

fn validate_input_signer(signer: &SigningIdentity, field: &str) -> Result<Vec<u8>, ApiError> {
    validate_input_signing_address(
        &signer.signing_address,
        Some(signer.signing_algo),
        &format!("{field}.signing_address"),
    )
}

pub(crate) fn validate_input_signing_address(
    signing_address: &str,
    signing_algo: Option<SigningAlgo>,
    field: &str,
) -> Result<Vec<u8>, ApiError> {
    let signing_address = decode_hex(signing_address)
        .map_err(|_| invalid_api_input(field, "invalid_hex", None, None))?;

    let (is_valid_length, expected) = match signing_algo {
        Some(SigningAlgo::Ecdsa) => (
            signing_address.len() == 20,
            "20-byte hexadecimal signing address",
        ),
        Some(SigningAlgo::Ed25519) => (
            signing_address.len() == 32,
            "32-byte hexadecimal signing address",
        ),
        None => (
            matches!(signing_address.len(), 20 | 32),
            "a 20- or 32-byte hexadecimal signing address",
        ),
    };
    if is_valid_length {
        return Ok(signing_address);
    }
    Err(invalid_api_input(
        field,
        "wrong_length",
        Some(expected.to_owned()),
        Some(format!("{} bytes", signing_address.len())),
    ))
}

fn completion_signature_kind_name(kind: CompletionSignatureKind) -> &'static str {
    match kind {
        CompletionSignatureKind::ProviderTee => "provider_tee",
        CompletionSignatureKind::Gateway => "gateway",
    }
}

fn invalid_api_input(
    field: impl Into<String>,
    reason: impl Into<String>,
    expected: Option<String>,
    actual: Option<String>,
) -> ApiError {
    ApiError::InvalidInput {
        field: field.into(),
        reason: reason.into(),
        expected,
        actual,
    }
}

fn require_matching_api_nonce(
    reported_nonce: &str,
    requested_nonce: &str,
    resource: ApiResource,
) -> Result<(), ApiError> {
    let matches = matches!(
        (
            decode_hex(reported_nonce),
            decode_hex(requested_nonce),
        ),
        (Ok(reported), Ok(requested)) if reported == requested
    );
    if matches {
        return Ok(());
    }
    Err(ApiError::NonceMismatch { resource })
}

/// Decode one complete endpoint response into its wire type. This is the only
/// JSON shape-decoding step between a Cloud API response body and its domain
/// mapping, and attaches a stable root path to shape errors.
fn decode_wire_response<T>(
    body: &str,
    resource: ApiResource,
    root_path: &str,
) -> Result<T, ApiError>
where
    T: for<'de> Deserialize<'de>,
{
    let mut deserializer = serde_json::Deserializer::from_str(body);
    let response = serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
        if error.inner().is_syntax() || error.inner().is_eof() {
            return ApiError::InvalidJson { resource };
        }
        ApiError::InvalidResponse {
            path: response_error_path(root_path, &error.path().to_string()),
            expected: "the documented Cloud API response shape".to_owned(),
            actual: "invalid".to_owned(),
        }
    })?;
    deserializer
        .end()
        .map_err(|_| ApiError::InvalidJson { resource })?;
    Ok(response)
}

fn response_error_path(root_path: &str, nested_path: &str) -> String {
    match nested_path {
        "" | "." => root_path.to_owned(),
        _ if root_path.is_empty() || nested_path == root_path => nested_path.to_owned(),
        _ if nested_path.starts_with(root_path)
            && matches!(
                nested_path.as_bytes().get(root_path.len()),
                Some(b'.' | b'[')
            ) =>
        {
            nested_path.to_owned()
        }
        _ if nested_path.starts_with('[') => format!("{root_path}{nested_path}"),
        _ => format!("{root_path}.{nested_path}"),
    }
}

fn map_model_attestation(
    value: WireModelAttestation,
    path: &str,
) -> Result<ModelAttestation, ApiError> {
    let (evidence, reported_quote_data) = map_evidence(value.attestation, path)?;
    if let Some(key) = &value.signing_public_key {
        decode_hex(key).map_err(|_| ApiError::InvalidResponse {
            path: format!("{path}.signing_public_key"),
            expected: "a hexadecimal public key".into(),
            actual: "invalid".into(),
        })?;
    }
    Ok(ModelAttestation {
        signing_public_key: value.signing_public_key,
        evidence,
        reported_quote_data,
        nvidia_payload: value.nvidia_payload,
    })
}

fn map_gateway_attestation(
    value: WireAttestation,
    path: &str,
) -> Result<GatewayAttestation, ApiError> {
    let spki_fingerprint = value.tls_cert_fingerprint.clone();
    let (evidence, reported_quote_data) = map_evidence(value, path)?;
    let reported_quote_data = reported_quote_data.ok_or_else(|| ApiError::InvalidResponse {
        path: format!("{path}.report_data"),
        expected: "a string".to_owned(),
        actual: "missing".to_owned(),
    })?;
    Ok(GatewayAttestation {
        evidence,
        spki_fingerprint,
        reported_quote_data,
    })
}

fn map_evidence(
    value: WireAttestation,
    path: &str,
) -> Result<(AttestationEvidence, Option<String>), ApiError> {
    validate_api_nonce(&value.request_nonce, &format!("{path}.request_nonce"))?;
    validate_api_signing_identity(
        value.signing_algo,
        &value.signing_address,
        &format!("{path}.signing_address"),
    )?;
    Ok((
        AttestationEvidence {
            nonce: value.request_nonce,
            signer: SigningIdentity {
                signing_algo: value.signing_algo,
                signing_address: value.signing_address,
            },
            intel_quote: value.intel_quote,
            event_log: value.event_log,
            app_compose: value.info.tcb_info.app_compose,
        },
        value.report_data,
    ))
}

fn validate_api_nonce(value: &str, path: &str) -> Result<(), ApiError> {
    require_hex_length(value, 32).map_err(|_| ApiError::InvalidResponse {
        path: path.to_owned(),
        expected: "a 32-byte hexadecimal nonce".to_owned(),
        actual: "invalid".to_owned(),
    })?;
    Ok(())
}

fn validate_api_signing_identity(
    signing_algo: SigningAlgo,
    signing_address: &str,
    path: &str,
) -> Result<(), ApiError> {
    let expected = match signing_algo {
        SigningAlgo::Ecdsa => 20,
        SigningAlgo::Ed25519 => 32,
    };
    require_hex_length(signing_address, expected).map_err(|_| ApiError::InvalidResponse {
        path: path.to_owned(),
        expected: format!("a {expected}-byte hexadecimal signing address"),
        actual: "invalid".to_owned(),
    })?;
    Ok(())
}

fn map_completion_signature(
    value: WireCompletionSignatureResponse,
) -> Result<CompletionSignature, ApiError> {
    if let Some((provider_error_code, provider_message)) = value.unavailable() {
        return Err(ApiError::CompletionSignatureUnavailable {
            provider_error_code: provider_error_code.to_owned(),
            provider_message: provider_message.to_owned(),
        });
    }
    let signature = value.require_signature()?;
    validate_api_signing_identity(
        signature.signing_algo,
        &signature.signing_address,
        "signature.signing_address",
    )?;
    Ok(CompletionSignature {
        kind: signature.signature_kind,
        signed_text: signature.text,
        signature: signature.signature,
        signer: SigningIdentity {
            signing_algo: signature.signing_algo,
            signing_address: signature.signing_address,
        },
    })
}

#[derive(Deserialize)]
struct WireModelAttestationResponse {
    #[serde(default)]
    model_attestations: Vec<WireModelAttestation>,
}

#[derive(Deserialize)]
struct WireGatewayAttestationResponse {
    ohttp_attestation: Option<crate::OhttpAttestation>,
    gateway_attestation: WireAttestation,
}

#[derive(Deserialize)]
struct WireModelAttestation {
    signing_public_key: Option<String>,
    #[serde(flatten)]
    attestation: WireAttestation,
    nvidia_payload: Option<String>,
}

#[derive(Deserialize)]
struct WireAttestation {
    request_nonce: String,
    signing_algo: SigningAlgo,
    signing_address: String,
    intel_quote: String,
    event_log: AttestationEventLog,
    tls_cert_fingerprint: Option<String>,
    report_data: Option<String>,
    info: WireInfo,
}

#[derive(Deserialize)]
struct WireInfo {
    tcb_info: WireTcbInfo,
}

struct WireTcbInfo {
    app_compose: String,
}

#[derive(Deserialize)]
struct WireTcbInfoObject {
    app_compose: String,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum WireTcbInfoSource {
    Object(WireTcbInfoObject),
    Json(String),
}

impl<'de> Deserialize<'de> for WireTcbInfo {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let source = WireTcbInfoSource::deserialize(deserializer)?;
        let value = match source {
            WireTcbInfoSource::Object(value) => value,
            WireTcbInfoSource::Json(value) => {
                serde_json::from_str(&value).map_err(D::Error::custom)?
            }
        };
        Ok(Self {
            app_compose: value.app_compose,
        })
    }
}

#[derive(Deserialize)]
struct WireCompletionSignatureResponse {
    #[serde(default, deserialize_with = "deserialize_optional_signature_field")]
    text: OptionalSignatureField<String>,
    #[serde(default, deserialize_with = "deserialize_optional_signature_field")]
    signature: OptionalSignatureField<String>,
    #[serde(default, deserialize_with = "deserialize_optional_signature_field")]
    signing_address: OptionalSignatureField<String>,
    #[serde(default, deserialize_with = "deserialize_optional_signature_field")]
    signing_algo: OptionalSignatureField<SigningAlgo>,
    #[serde(default, deserialize_with = "deserialize_optional_signature_field")]
    signature_kind: OptionalSignatureField<CompletionSignatureKind>,
    #[serde(default)]
    error_code: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

impl WireCompletionSignatureResponse {
    fn unavailable(&self) -> Option<(&str, &str)> {
        let (Some(error_code), Some(message)) = (&self.error_code, &self.message) else {
            return None;
        };
        if self.text.is_missing()
            && self.signature.is_missing()
            && self.signing_address.is_missing()
            && self.signing_algo.is_missing()
            && self.signature_kind.is_missing()
        {
            return Some((error_code, message));
        }
        None
    }

    fn require_signature(self) -> Result<WireCompletionSignature, ApiError> {
        Ok(WireCompletionSignature {
            text: require_signature_field(self.text, "text")?,
            signature: require_signature_field(self.signature, "signature")?,
            signing_address: require_signature_field(self.signing_address, "signing_address")?,
            signing_algo: require_signature_field(self.signing_algo, "signing_algo")?,
            signature_kind: require_signature_field(self.signature_kind, "signature_kind")?,
        })
    }
}

/// Preserve whether a response field was omitted or explicitly set to `null`.
///
/// An unavailable signature response must omit every signature field. A `null`
/// field is instead a malformed signature response and must not be treated as
/// an unavailable result.
#[derive(Default)]
enum OptionalSignatureField<T> {
    #[default]
    Missing,
    Null,
    Value(T),
}

impl<T> OptionalSignatureField<T> {
    fn is_missing(&self) -> bool {
        matches!(self, Self::Missing)
    }
}

fn deserialize_optional_signature_field<'de, D, T>(
    deserializer: D,
) -> Result<OptionalSignatureField<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(|value| match value {
        Some(value) => OptionalSignatureField::Value(value),
        None => OptionalSignatureField::Null,
    })
}

struct WireCompletionSignature {
    text: String,
    signature: String,
    signing_address: String,
    signing_algo: SigningAlgo,
    signature_kind: CompletionSignatureKind,
}

fn require_signature_field<T>(
    value: OptionalSignatureField<T>,
    field: &str,
) -> Result<T, ApiError> {
    match value {
        OptionalSignatureField::Value(value) => Ok(value),
        OptionalSignatureField::Missing => Err(ApiError::InvalidResponse {
            path: format!("signature.{field}"),
            expected: "the documented Cloud API response shape".to_owned(),
            actual: "missing".to_owned(),
        }),
        OptionalSignatureField::Null => Err(ApiError::InvalidResponse {
            path: format!("signature.{field}"),
            expected: "the documented Cloud API response shape".to_owned(),
            actual: "null".to_owned(),
        }),
    }
}

pub(crate) fn decode_model_value(value: &serde_json::Value) -> Result<ModelAttestation, ApiError> {
    let wire = decode_wire_response(
        &value.to_string(),
        ApiResource::ModelAttestation,
        "attestation",
    )?;
    map_model_attestation(wire, "attestation")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn default_base_url_preserves_the_v1_path_when_resolving_an_endpoint() {
        let client = AttestationClient::new("test-key".to_owned());

        assert_eq!(
            client.endpoint("attestation/report").unwrap().as_str(),
            "https://cloud-api.near.ai/v1/attestation/report",
        );
    }

    fn decode_test_wire<T>(
        value: serde_json::Value,
        resource: ApiResource,
        root_path: &str,
    ) -> Result<T, ApiError>
    where
        T: serde::de::DeserializeOwned,
    {
        decode_wire_response(&value.to_string(), resource, root_path)
    }

    #[test]
    fn malformed_model_public_key_is_a_response_error() {
        let error = decode_model_value(&json!({
            "request_nonce": "11".repeat(32), "signing_algo": "ed25519",
            "signing_address": "22".repeat(32), "signing_public_key": "not hex",
            "intel_quote": "aa", "event_log": [],
            "info": {"tcb_info": {"app_compose": "{}"}},
        }))
        .unwrap_err();
        assert!(
            matches!(error, ApiError::InvalidResponse { path, .. } if path == "attestation.signing_public_key")
        );
    }

    #[test]
    fn normalizes_optional_model_wire_fields_to_none() {
        for value in [
            json!({
                "request_nonce": "11".repeat(32),
                "signing_algo": "ecdsa",
                "signing_address": "22".repeat(20),
                "intel_quote": "aa",
                "event_log": [],
                "info": {"tcb_info": {"app_compose": "{}"}},
            }),
            json!({
                "request_nonce": "11".repeat(32),
                "signing_algo": "ecdsa",
                "signing_address": "22".repeat(20),
                "intel_quote": "aa",
                "event_log": [],
                "tls_cert_fingerprint": null,
                "report_data": null,
                "nvidia_payload": null,
                "info": {"tcb_info": {"app_compose": "{}"}},
            }),
        ] {
            let response: WireModelAttestationResponse = decode_test_wire(
                json!({"model_attestations": [value]}),
                ApiResource::ModelAttestation,
                "model_attestations",
            )
            .unwrap();
            let attestation = map_model_attestation(
                response
                    .model_attestations
                    .into_iter()
                    .next()
                    .expect("one model attestation"),
                "model_attestations[0]",
            )
            .unwrap();
            assert_eq!(attestation.reported_quote_data, None);
            assert_eq!(attestation.nvidia_payload, None);
        }
    }

    #[test]
    fn gateway_response_requires_report_data() {
        let response: WireGatewayAttestationResponse = decode_test_wire(
            json!({
                "gateway_attestation": {
                    "request_nonce": "11".repeat(32),
                    "signing_algo": "ecdsa",
                    "signing_address": "22".repeat(20),
                    "intel_quote": "aa",
                    "event_log": [],
                    "tls_cert_fingerprint": "33".repeat(32),
                    "info": {"tcb_info": {"app_compose": "{}"}},
                },
            }),
            ApiResource::GatewayAttestation,
            "gateway_attestation",
        )
        .unwrap();
        let error = map_gateway_attestation(response.gateway_attestation, "gateway_attestation")
            .unwrap_err();

        assert!(matches!(
            error,
            ApiError::InvalidResponse { ref path, .. }
                if path == "gateway_attestation.report_data"
        ));
    }

    #[test]
    fn gateway_response_allows_an_omitted_tls_fingerprint() {
        let response: WireGatewayAttestationResponse = decode_test_wire(
            json!({
                "gateway_attestation": {
                    "request_nonce": "11".repeat(32),
                    "signing_algo": "ecdsa",
                    "signing_address": "22".repeat(20),
                    "intel_quote": "aa",
                    "event_log": [],
                    "report_data": "00".repeat(64),
                    "info": {"tcb_info": {"app_compose": "{}"}},
                },
            }),
            ApiResource::GatewayAttestation,
            "gateway_attestation",
        )
        .unwrap();
        let attestation =
            map_gateway_attestation(response.gateway_attestation, "gateway_attestation").unwrap();

        assert_eq!(attestation.spki_fingerprint, None);
    }

    #[test]
    fn wire_decoder_keeps_malformed_json_as_an_api_json_error() {
        let result = decode_wire_response::<WireModelAttestationResponse>(
            "{",
            ApiResource::ModelAttestation,
            "model_attestations",
        );

        assert!(matches!(
            result,
            Err(ApiError::InvalidJson {
                resource: ApiResource::ModelAttestation
            })
        ));
    }

    #[test]
    fn typed_model_records_preserve_wire_nonce_and_signer_validation() {
        for (request_nonce, signing_address, expected_path) in [
            ("aa".to_owned(), "22".repeat(20), "request_nonce"),
            ("11".repeat(32), "aa".to_owned(), "signing_address"),
        ] {
            let response: WireModelAttestationResponse = decode_test_wire(
                json!({
                    "model_attestations": [{
                        "request_nonce": request_nonce,
                        "signing_algo": "ecdsa",
                        "signing_address": signing_address,
                        "intel_quote": "aa",
                        "event_log": [],
                        "info": {"tcb_info": {"app_compose": "{}"}},
                    }],
                }),
                ApiResource::ModelAttestation,
                "model_attestations",
            )
            .unwrap();

            let error = map_model_attestation(
                response
                    .model_attestations
                    .into_iter()
                    .next()
                    .expect("one model attestation"),
                "model_attestations[0]",
            )
            .unwrap_err();
            assert!(matches!(
                error,
                ApiError::InvalidResponse { ref path, .. }
                    if path == &format!("model_attestations[0].{expected_path}")
            ));
        }
    }

    #[test]
    fn typed_model_records_accept_json_string_tcb_info() {
        let response: WireModelAttestationResponse = decode_test_wire(
            json!({
                "model_attestations": [{
                    "request_nonce": "11".repeat(32),
                    "signing_algo": "ecdsa",
                    "signing_address": "22".repeat(20),
                    "intel_quote": "aa",
                    "event_log": [],
                    "info": {"tcb_info": "{\"app_compose\":\"{}\"}"},
                }],
            }),
            ApiResource::ModelAttestation,
            "model_attestations",
        )
        .unwrap();
        let attestation = map_model_attestation(
            response
                .model_attestations
                .into_iter()
                .next()
                .expect("one model attestation"),
            "model_attestations[0]",
        )
        .unwrap();

        assert_eq!(attestation.evidence.app_compose, "{}");
    }

    #[test]
    fn accepts_an_equivalent_nonce_encoding() {
        require_matching_api_nonce(
            &format!("0X{}", "AB".repeat(32)),
            &"ab".repeat(32),
            ApiResource::ModelAttestation,
        )
        .unwrap();
    }

    #[test]
    fn typed_model_records_reject_invalid_json_string_tcb_info() {
        let result = decode_test_wire::<WireModelAttestationResponse>(
            json!({
                "model_attestations": [{
                    "request_nonce": "11".repeat(32),
                    "signing_algo": "ecdsa",
                    "signing_address": "22".repeat(20),
                    "intel_quote": "aa",
                    "event_log": [],
                    "info": {"tcb_info": "not JSON"},
                }],
            }),
            ApiResource::ModelAttestation,
            "model_attestations",
        );

        assert!(matches!(
            result,
            Err(ApiError::InvalidResponse { ref path, .. })
                if path.starts_with("model_attestations[0]")
        ));
    }

    #[test]
    fn rejects_missing_or_unknown_completion_signature_kind() {
        for response in [
            json!({
                "text": "signed",
                "signature": "aa",
                "signing_address": "22".repeat(20),
                "signing_algo": "ecdsa",
            }),
            json!({
                "text": "signed",
                "signature": "aa",
                "signing_address": "22".repeat(20),
                "signing_algo": "ecdsa",
                "signature_kind": "unknown",
            }),
        ] {
            let error = decode_test_wire::<WireCompletionSignatureResponse>(
                response,
                ApiResource::CompletionSignature,
                "signature",
            )
            .and_then(map_completion_signature)
            .unwrap_err();
            assert!(matches!(error, ApiError::InvalidResponse { .. }));
        }
    }

    #[test]
    fn signature_fields_prevent_an_error_envelope_from_being_treated_as_unavailable() {
        for response in [
            json!({
                "error_code": "SIGNATURE_UNSUPPORTED",
                "message": "the provider does not support completion signatures",
                "text": "partial signature",
            }),
            json!({
                "error_code": "SIGNATURE_UNSUPPORTED",
                "message": "the provider does not support completion signatures",
                "signature": null,
            }),
        ] {
            let response: WireCompletionSignatureResponse =
                decode_test_wire(response, ApiResource::CompletionSignature, "signature").unwrap();

            let error = map_completion_signature(response).unwrap_err();
            assert!(matches!(error, ApiError::InvalidResponse { .. }));
        }
    }
}
