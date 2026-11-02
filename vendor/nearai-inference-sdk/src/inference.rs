//! Verified Gateway Chat, shared admission checks, and exact-byte receipts.
use crate::*;
use crate::{
    e2ee::{decode_request, decrypt_response, invalid_response, remove_e2ee_headers},
    errors::protocol,
};
use futures_util::{
    future::{try_join_all, BoxFuture, Shared, WeakShared},
    FutureExt, Stream, StreamExt,
};
use reqwest::{
    header::{HeaderMap, HeaderValue},
    StatusCode,
};
use serde_json::Value;
use std::{
    collections::HashMap,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::Mutex;

pub type ByteStream = Pin<Box<dyn Stream<Item = Result<bytes::Bytes, InferenceError>> + Send>>;
/// Consume the entire body (including the EOF after SSE [DONE]) before verification.
pub struct InferenceResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: ByteStream,
}
impl InferenceResponse {
    pub async fn bytes(mut self) -> Result<Vec<u8>, InferenceError> {
        let mut bytes = Vec::new();
        while let Some(chunk) = self.body.next().await {
            bytes.extend_from_slice(&chunk?);
        }
        Ok(bytes)
    }
    pub async fn json(self) -> Result<Value, InferenceError> {
        serde_json::from_slice(&self.bytes().await?).map_err(|_| invalid_response().into())
    }
}
#[derive(Clone)]
pub struct GatewayVerificationOptions {
    pub include_spki_fingerprint: bool,
    pub policy: Option<AttestationPolicy>,
    pub tdx_quote: Option<Arc<dyn TdxQuoteVerifier>>,
    pub deployment: Option<Arc<dyn DeploymentVerifier>>,
}
impl Default for GatewayVerificationOptions {
    fn default() -> Self {
        Self {
            include_spki_fingerprint: true,
            policy: None,
            tdx_quote: None,
            deployment: None,
        }
    }
}
#[derive(Clone, Default)]
pub struct ModelVerificationOptions {
    pub policy: Option<ModelAttestationPolicy>,
    pub tdx_quote: Option<Arc<dyn TdxQuoteVerifier>>,
    pub gpu_evidence: Option<Arc<dyn GpuEvidenceVerifier>>,
    pub deployment: Option<Arc<dyn DeploymentVerifier>>,
}
#[async_trait::async_trait]
pub trait DeploymentPolicy: Send + Sync {
    async fn verify(
        &self,
        model: &str,
        deployment: &MeasuredDeployment,
    ) -> Result<(), VerificationError>;
}
#[derive(Clone)]
pub struct InferenceClientOptions {
    pub api_key: Option<String>,
    pub base_url: String,
    pub headers: HeaderMap,
    pub e2ee: bool,
    pub ohttp: bool,
    pub signing_algo: SigningAlgo,
    pub attestation_cache_ttl: Duration,
    pub response_cache_ttl: Duration,
    pub gateway_verification: GatewayVerificationOptions,
    pub model_verification: ModelVerificationOptions,
    pub deployment_policy: Option<Arc<dyn DeploymentPolicy>>,
    /// Exact bytes retained for a single response; larger responses fail closed.
    pub max_response_bytes: usize,
    /// Bound both completed session and receipt caches. Oldest entries are evicted.
    pub max_cache_entries: usize,
    /// Total request and response bytes retained in the receipt cache.
    pub max_receipt_cache_bytes: usize,
    /// Nymira patch: a SOCKS5 proxy URL (`socks5h://identity:x@127.0.0.1:9050`) for every
    /// request this client makes. `None` is the upstream behavior.
    pub socks_proxy: Option<String>,
}
impl Default for InferenceClientOptions {
    fn default() -> Self {
        Self {
            api_key: None,
            base_url: DEFAULT_NEAR_AI_CLOUD_BASE_URL.into(),
            headers: HeaderMap::new(),
            e2ee: false,
            ohttp: false,
            signing_algo: SigningAlgo::Ed25519,
            attestation_cache_ttl: Duration::from_secs(3600),
            response_cache_ttl: Duration::from_secs(3600),
            gateway_verification: GatewayVerificationOptions::default(),
            model_verification: ModelVerificationOptions::default(),
            deployment_policy: None,
            max_response_bytes: 64 * 1024 * 1024,
            max_cache_entries: 1024,
            max_receipt_cache_bytes: 64 * 1024 * 1024,
            socks_proxy: None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct AttestationVerificationResult {
    pub gateway: VerifiedGatewayAttestation,
    pub models: Vec<VerifiedModelAttestation>,
    pub verified_at: u64,
}
#[derive(Clone, Debug)]
pub enum VerifiedCompletionAttestation {
    Gateway(VerifiedGatewayAttestation),
    Model(VerifiedModelAttestation),
    Direct(Vec<crate::direct::VerifiedDirectModelAttestation>),
}
#[derive(Clone, Debug)]
pub struct VerifiedCompletionResult {
    pub id: String,
    pub signature: CompletionSignature,
    pub attestation: VerifiedCompletionAttestation,
}
impl VerifiedCompletionResult {
    pub fn signature_kind(&self) -> CompletionSignatureKind {
        self.signature.kind
    }
}
#[derive(Clone)]
pub(crate) enum EvidenceApi {
    Gateway(AttestationClient),
    Direct(crate::direct::DirectAttestationClient),
}
impl EvidenceApi {
    async fn signature(
        &self,
        id: &str,
        algo: SigningAlgo,
    ) -> Result<CompletionSignature, ApiError> {
        match self {
            Self::Gateway(api) => api.fetch_completion_signature(id, Some(algo)).await,
            Self::Direct(api) => api.fetch_completion_signature(id, Some(algo)).await,
        }
    }
}
pub(crate) struct Session {
    pub gateway: Option<VerifiedGatewayAttestation>,
    pub models: Vec<VerifiedModelAttestation>,
    pub selected: Option<VerifiedModelAttestation>,
    pub direct: Option<crate::direct::DirectAttestationVerificationResult>,
    pub verified_at: u64,
    pub client: reqwest::Client,
    pub api: EvidenceApi,
    pub ohttp: Option<crate::OhttpClient>,
}
impl Session {
    fn model_key(&self) -> Option<E2eeModelKey> {
        self.selected.as_ref().and_then(|m| {
            m.signing_public_key.as_ref().map(|key| E2eeModelKey {
                signing_algo: m.evidence.signer.signing_algo,
                public_key: key.clone(),
            })
        })
    }
}
type PendingSession = WeakShared<BoxFuture<'static, Result<Arc<Session>, InferenceError>>>;
type ReceiptFuture = Shared<BoxFuture<'static, Result<VerifiedCompletionResult, InferenceError>>>;
struct Record {
    request: Arc<[u8]>,
    response: Arc<[u8]>,
    session: Arc<Session>,
    finished: Instant,
    verification: Mutex<Option<ReceiptFuture>>,
}
#[derive(Default)]
struct State {
    sessions: HashMap<String, (Instant, Arc<Session>)>,
    pending: HashMap<String, PendingSession>,
    responses: HashMap<String, Arc<Record>>,
}
struct Inner {
    options: InferenceClientOptions,
    direct: bool,
    state: Mutex<State>,
}
#[derive(Clone)]
pub struct InferenceClient {
    inner: Arc<Inner>,
}
impl InferenceClient {
    pub fn new(api_key: String) -> Result<Self, InferenceError> {
        Self::with_options(InferenceClientOptions {
            api_key: Some(api_key),
            ..Default::default()
        })
    }
    pub fn with_options(options: InferenceClientOptions) -> Result<Self, InferenceError> {
        Self::create(options, false)
    }
    pub(crate) fn create(
        mut options: InferenceClientOptions,
        direct: bool,
    ) -> Result<Self, InferenceError> {
        options.base_url = crate::cloud_api::parse_base_url(&options.base_url)?.to_string();
        if options.ohttp && options.signing_algo != SigningAlgo::Ed25519 {
            return Err(input("signing_algo", "OHTTP requires Ed25519").into());
        }
        if options.max_cache_entries == 0
            || options.max_response_bytes == 0
            || options.max_receipt_cache_bytes == 0
        {
            return Err(input("cache_limits", "must be positive").into());
        }
        if let Some(key) = &options.api_key {
            options.headers.insert(
                "authorization",
                HeaderValue::from_str(&format!("Bearer {key}"))
                    .map_err(|_| input("api_key", "invalid header"))?,
            );
            options.headers.remove("api-key");
        }
        Ok(Self {
            inner: Arc::new(Inner {
                options,
                direct,
                state: Mutex::new(State::default()),
            }),
        })
    }
    pub fn base_url(&self) -> &str {
        &self.inner.options.base_url
    }
    /// Gateway and model evidence; cache hits preserve the original verification time.
    pub async fn verify(
        &self,
        model: &str,
    ) -> Result<AttestationVerificationResult, InferenceError> {
        let session = self.session(model).await?;
        Ok(AttestationVerificationResult {
            gateway: session
                .gateway
                .clone()
                .ok_or_else(|| input("client", "expected Gateway client"))?,
            models: session.models.clone(),
            verified_at: session.verified_at,
        })
    }
    pub(crate) async fn session(&self, model: &str) -> Result<Arc<Session>, InferenceError> {
        if model.is_empty() || matches!(model, "." | "..") {
            return Err(input("model", "expected a canonical model ID").into());
        }
        let options = &self.inner.options;
        let future = {
            let mut state = self.inner.state.lock().await;
            state
                .sessions
                .retain(|_, (time, _)| time.elapsed() < options.attestation_cache_ttl);
            if let Some((_, session)) = state.sessions.get(model) {
                return Ok(session.clone());
            }
            state.pending.retain(|_, f| f.upgrade().is_some());
            if let Some(future) = state.pending.get(model).and_then(|f| f.upgrade()) {
                future
            } else {
                if state.pending.len() >= options.max_cache_entries {
                    return Err(
                        input("model", "too many concurrent verification operations").into(),
                    );
                }
                let owned = model.to_owned();
                let options = options.clone();
                let direct = self.inner.direct;
                let future = async move {
                    if direct {
                        crate::direct::create_session(&owned, options).await
                    } else {
                        create_session(&owned, options).await
                    }
                }
                .boxed()
                .shared();
                state
                    .pending
                    .insert(model.to_owned(), future.downgrade().expect("live future"));
                future
            }
        };
        let result = future.clone().await;
        let mut state = self.inner.state.lock().await;
        if state
            .pending
            .get(model)
            .and_then(|f| f.upgrade())
            .is_some_and(|f| f.ptr_eq(&future))
        {
            state.pending.remove(model);
            if let Ok(session) = &result {
                if !options.attestation_cache_ttl.is_zero() {
                    if state.sessions.len() >= options.max_cache_entries {
                        if let Some(key) = state
                            .sessions
                            .iter()
                            .min_by_key(|(_, v)| v.0)
                            .map(|(k, _)| k.clone())
                        {
                            state.sessions.remove(&key);
                        }
                    }
                    state
                        .sessions
                        .insert(model.to_owned(), (Instant::now(), session.clone()));
                }
            }
        }
        result
    }
    /// OpenAI-compatible JSON Chat request. Use `send` for a byte-stream response.
    pub async fn chat_completions(&self, body: Value) -> Result<Value, InferenceError> {
        if body.get("stream") == Some(&Value::Bool(true)) {
            return Err(input("stream", "use send for streaming Chat").into());
        }
        let request = self.chat_request(body)?;
        let response = self.send(request).await?;
        if !response.status.is_success() {
            return Err(ApiError::HttpStatus {
                resource: ApiResource::Completion,
                status: response.status.as_u16(),
            }
            .into());
        }
        response.json().await
    }
    pub fn chat_request(&self, body: Value) -> Result<reqwest::Request, InferenceError> {
        let url = crate::cloud_api::parse_base_url(self.base_url())?
            .join("chat/completions")
            .map_err(|_| input("base_url", "invalid URL"))?;
        let mut request = reqwest::Request::new(reqwest::Method::POST, url);
        *request.body_mut() = Some(body.to_string().into());
        request
            .headers_mut()
            .insert("content-type", HeaderValue::from_static("application/json"));
        Ok(request)
    }
    /// Transport adapter for buffered reqwest Chat requests. Request authentication
    /// is always supplied by this client, including evidence and signature calls.
    pub async fn send(
        &self,
        mut request: reqwest::Request,
    ) -> Result<InferenceResponse, InferenceError> {
        let mut actual = request.url().clone();
        actual.set_query(None);
        let expected = crate::cloud_api::parse_base_url(self.base_url())?
            .join("chat/completions")
            .map_err(|_| input("base_url", "invalid URL"))?;
        if actual != expected {
            return Err(input("request.url", "expected configured Chat endpoint").into());
        }
        let parsed = decode_request(&request)?;
        let session = self
            .session(
                parsed["model"]
                    .as_str()
                    .ok_or_else(|| input("model", "missing model"))?,
            )
            .await?;
        let options = &self.inner.options;
        let mut headers = options.headers.clone();
        headers.extend(request.headers().clone());
        headers.remove("authorization");
        if let Some(value) = options.headers.get("authorization") {
            headers.insert("authorization", value.clone());
        }
        headers.remove("api-key");
        if let Some(value) = options.headers.get("api-key") {
            headers.insert("api-key", value.clone());
        }
        headers.insert("accept-encoding", HeaderValue::from_static("identity"));
        headers.remove("host");
        headers.remove("transfer-encoding");
        headers.remove("trailer");
        *request.headers_mut() = headers;
        let key = if options.e2ee {
            let model = session.model_key().ok_or_else(|| {
                protocol(
                    "policy.model_attestation_required",
                    "E2EE requires model evidence",
                )
            })?;
            let prepared = prepare_e2ee_chat_request(request, &model)?;
            request = prepared.request;
            Some(prepared.key)
        } else {
            remove_e2ee_headers(request.headers_mut());
            request
                .headers_mut()
                .insert(NO_ALIASING_HEADER, HeaderValue::from_static("true"));
            if let Some(model) = session.model_key() {
                request.headers_mut().insert(
                    "x-model-pub-key",
                    HeaderValue::from_str(&model.public_key)
                        .map_err(|_| input("model_key", "invalid header"))?,
                );
            }
            None
        };
        let request_body = request
            .body()
            .and_then(reqwest::Body::as_bytes)
            .ok_or_else(|| input("body", "buffered body required"))?
            .to_vec();
        let response = if let Some(ohttp) = &session.ohttp {
            ohttp.send(request).await?
        } else {
            let response = session
                .client
                .execute(request)
                .await
                .map_err(|_| transport(ApiTransportReason::Request))?;
            let status = response.status();
            let headers = response.headers().clone();
            let body = Box::pin(
                response
                    .bytes_stream()
                    .map(|r| r.map_err(|_| transport(ApiTransportReason::ResponseBody).into())),
            );
            InferenceResponse {
                status,
                headers,
                body,
            }
        };
        if !response.status.is_success() {
            return Ok(response);
        }
        let streaming = response
            .headers
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|s| s.to_ascii_lowercase().starts_with("text/event-stream"));
        let mut headers = response.headers;
        for name in [
            "content-length",
            "content-encoding",
            "etag",
            "content-md5",
            "digest",
            "content-digest",
            "repr-digest",
            "last-modified",
        ] {
            headers.remove(name);
        }
        let mut source = response.body;
        let owner = self.clone();
        let limit = options.max_response_bytes;
        let stream = async_stream::try_stream! {
            let mut wire = Vec::new();
            let mut decoder = crate::sse::SseDecoder::default();
            let mut id = None;
            while let Some(chunk) = source.next().await {
                let chunk = chunk?;
                if chunk.len() > limit.saturating_sub(wire.len()) {
                    Err(input("response", "response exceeds configured byte limit"))?;
                }
                wire.extend_from_slice(&chunk);
                if streaming {
                    for record in decoder.push(&chunk, false)? {
                        let transformed = crate::sse::transform(&record, key.as_ref(), &mut id)?;
                        yield bytes::Bytes::from(transformed);
                    }
                }
            }
            if streaming {
                for record in decoder.push(&[], true)? {
                    let transformed = crate::sse::transform(&record, key.as_ref(), &mut id)?;
                    yield bytes::Bytes::from(transformed);
                }
                let id = id.ok_or_else(invalid_response)?;
                owner.register(id, request_body, wire, session).await?;
            } else {
                let mut value: Value = serde_json::from_slice(&wire).map_err(|_| invalid_response())?;
                let id = value
                    .get("id")
                    .and_then(Value::as_str)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(invalid_response)?
                    .to_owned();
                if let Some(key) = key.as_ref() {
                    decrypt_response(&mut value, key, false)?;
                }
                let body = if key.is_some() {
                    value.to_string().into_bytes()
                } else {
                    wire.clone()
                };
                owner.register(id, request_body, wire, session).await?;
                yield bytes::Bytes::from(body);
            }
        };
        Ok(InferenceResponse {
            status: response.status,
            headers,
            body: Box::pin(stream),
        })
    }
    async fn register(
        &self,
        id: String,
        request: Vec<u8>,
        response: Vec<u8>,
        session: Arc<Session>,
    ) -> Result<(), InferenceError> {
        let options = &self.inner.options;
        let mut state = self.inner.state.lock().await;
        state
            .responses
            .retain(|_, r| r.finished.elapsed() < options.response_cache_ttl);
        // Never let a repeated ID replace the evidence/bytes of another completion.
        if state.responses.contains_key(&id) {
            return Err(input("response.id", "duplicate completion ID").into());
        }
        let size = request
            .len()
            .checked_add(response.len())
            .ok_or_else(|| input("response", "receipt exceeds configured cache byte limit"))?;
        if size > options.max_receipt_cache_bytes {
            return Err(input("response", "receipt exceeds configured cache byte limit").into());
        }
        let mut retained: usize = state
            .responses
            .values()
            .map(|r| r.request.len() + r.response.len())
            .sum();
        while state.responses.len() >= options.max_cache_entries
            || retained > options.max_receipt_cache_bytes - size
        {
            if let Some(key) = state
                .responses
                .iter()
                .min_by_key(|(_, r)| r.finished)
                .map(|(k, _)| k.clone())
            {
                if let Some(record) = state.responses.remove(&key) {
                    retained -= record.request.len() + record.response.len();
                }
            }
        }
        state.responses.insert(
            id,
            Arc::new(Record {
                request: request.into(),
                response: response.into(),
                session,
                finished: Instant::now(),
                verification: Mutex::new(None),
            }),
        );
        Ok(())
    }
    pub async fn verify_response(
        &self,
        id: &str,
    ) -> Result<VerifiedCompletionResult, InferenceError> {
        let record = {
            let mut state = self.inner.state.lock().await;
            state
                .responses
                .retain(|_, r| r.finished.elapsed() < self.inner.options.response_cache_ttl);
            state
                .responses
                .get(id)
                .cloned()
                .ok_or(ApiError::CompletionNotFound)?
        };
        let future = {
            let mut pending = record.verification.lock().await;
            if pending.is_none() {
                // Do not capture Record itself: the cached future must not form an Arc cycle.
                let request = record.request.clone();
                let response = record.response.clone();
                let session = record.session.clone();
                let id = id.to_owned();
                let algo = self.inner.options.signing_algo;
                *pending = Some(
                    async move {
                        let signature = session.api.signature(&id, algo).await?;
                        if signature.signer.signing_algo != algo {
                            return Err(VerificationError::SignatureSignerMismatch.into());
                        }
                        let attestation = match signature.kind {
                            CompletionSignatureKind::Gateway => {
                                let gateway = session.gateway.as_ref().ok_or(
                                    VerificationError::SignatureKindMismatch {
                                        expected: CompletionSignatureKind::ProviderTee,
                                        actual: signature.kind,
                                    },
                                )?;
                                verify_gateway_response(&request, &response, &signature, gateway)?;
                                VerifiedCompletionAttestation::Gateway(gateway.clone())
                            }
                            CompletionSignatureKind::ProviderTee => {
                                let model = session.selected.as_ref().ok_or(
                                    VerificationError::SignatureKindMismatch {
                                        expected: CompletionSignatureKind::Gateway,
                                        actual: signature.kind,
                                    },
                                )?;
                                verify_model_response(&request, &response, &signature, model)?;
                                if let Some(direct) = &session.direct {
                                    let matching = direct
                                        .attestations
                                        .iter()
                                        .filter(|a| {
                                            crate::direct::same_signer(
                                                &a.attestation.evidence.signer,
                                                &model.evidence.signer,
                                            )
                                        })
                                        .cloned()
                                        .collect();
                                    VerifiedCompletionAttestation::Direct(matching)
                                } else {
                                    VerifiedCompletionAttestation::Model(model.clone())
                                }
                            }
                        };
                        Ok(VerifiedCompletionResult {
                            id,
                            signature,
                            attestation,
                        })
                    }
                    .boxed()
                    .shared(),
                );
            }
            pending.as_ref().expect("initialized future").clone()
        };
        let result = future.clone().await;
        if result
            .as_ref()
            .err()
            .is_some_and(|e| e.retryable() || e.code() == "api.completion_signature_unavailable")
        {
            let mut pending = record.verification.lock().await;
            if pending.as_ref().is_some_and(|f| f.ptr_eq(&future)) {
                *pending = None;
            }
        }
        result
    }
}
pub(crate) fn input(field: &str, reason: &str) -> ApiError {
    ApiError::InvalidInput {
        field: field.into(),
        reason: reason.into(),
        expected: None,
        actual: None,
    }
}
fn transport(reason: ApiTransportReason) -> ApiError {
    ApiError::Transport {
        resource: ApiResource::Completion,
        reason,
    }
}
pub(crate) fn verified_at() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}
#[allow(dead_code)]
pub(crate) fn ordinary_client() -> Result<reqwest::Client, InferenceError> {
    ordinary_client_via(None)
}
pub(crate) fn ordinary_client_via(
    socks_proxy: Option<&str>,
) -> Result<reqwest::Client, InferenceError> {
    let mut builder = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none());
    if let Some(url) = socks_proxy {
        // An explicit proxy replaces the environment's; nothing falls back to a direct path.
        let proxy = reqwest::Proxy::all(url).map_err(|_| input("socks_proxy", "invalid proxy"))?;
        builder = builder.no_proxy().proxy(proxy);
    }
    builder
        .build()
        .map_err(|_| input("client", "invalid HTTP configuration").into())
}
pub(crate) struct PolicyVerifier<'a> {
    pub model: &'a str,
    pub original: Option<&'a dyn DeploymentVerifier>,
    pub policy: Option<&'a dyn DeploymentPolicy>,
}
#[async_trait::async_trait]
impl DeploymentVerifier for PolicyVerifier<'_> {
    async fn verify(&self, deployment: &MeasuredDeployment) -> Result<(), VerificationError> {
        if let Some(v) = self.original {
            v.verify(deployment).await?;
        }
        if let Some(v) = self.policy {
            v.verify(self.model, deployment).await?;
        }
        Ok(())
    }
}
async fn create_session(
    model: &str,
    options: InferenceClientOptions,
) -> Result<Arc<Session>, InferenceError> {
    let api = AttestationClient::with_base_url(
        options.api_key.clone().unwrap_or_default(),
        &options.base_url,
    )?
    .with_headers(options.headers.clone());
    let api = match options.socks_proxy.as_deref() {
        Some(proxy) => api.with_socks_proxy(proxy)?,
        None => api,
    };
    let fetched = api
        .fetch_gateway_attestation(GatewayAttestationFetchOptions {
            signing_algo: Some(options.signing_algo),
            include_spki_fingerprint: options.gateway_verification.include_spki_fingerprint,
        })
        .await?;
    let client = if options.gateway_verification.include_spki_fingerprint {
        let pin = fetched
            .client_binding
            .spki_fingerprint
            .as_ref()
            .ok_or(VerificationError::SpkiFingerprintRequired)?;
        crate::pinned_tls::create_pinned_tls_client_via(
            std::slice::from_ref(pin),
            options.socks_proxy.as_deref(),
        )?
    } else {
        ordinary_client_via(options.socks_proxy.as_deref())?
    };
    let api = api.with_http_client(client.clone());
    let gateway = async {
        let o = &options.gateway_verification;
        let result = verify_gateway_attestation(
            &fetched.attestation,
            &fetched.client_binding,
            o.policy.as_ref(),
            AttestationVerifiers {
                tdx_quote: o.tdx_quote.as_deref(),
                deployment: o.deployment.as_deref(),
            },
        )
        .await?;
        if result.evidence.signer.signing_algo != options.signing_algo {
            return Err(InferenceError::from(
                VerificationError::SignatureSignerMismatch,
            ));
        }
        let ohttp = if options.ohttp {
            let raw = fetched.ohttp_attestation.as_ref().ok_or_else(|| {
                protocol(
                    "ohttp.attestation_required",
                    "Gateway omitted OHTTP evidence",
                )
            })?;
            let config = verify_ohttp_key_config(raw, &result.evidence.signer)?;
            Some(create_ohttp_client(
                &config,
                &options.base_url,
                client.clone(),
                options.headers.keys().map(|k| k.to_string()).collect(),
            )?)
        } else {
            None
        };
        Ok((result, ohttp))
    };
    let models = async {
        let metadata = api.fetch_model_metadata(model).await?;
        let o = &options.model_verification;
        if metadata.provider_type != "vllm" || !metadata.attestation_supported {
            if options.e2ee
                || o.policy.is_some()
                || o.deployment.is_some()
                || options.deployment_policy.is_some()
            {
                return Err(protocol(
                    "policy.model_attestation_required",
                    "model does not support NEAR attestation",
                )
                .into());
            }
            return Ok(Vec::new());
        }
        let fetched = api
            .fetch_model_attestations(model, Some(options.signing_algo), None)
            .await?;
        if fetched.attestations.is_empty() {
            return Err(protocol(
                "policy.model_attestation_required",
                "no model reports returned",
            )
            .into());
        }
        let policy = PolicyVerifier {
            model,
            original: o.deployment.as_deref(),
            policy: options.deployment_policy.as_deref(),
        };
        let deployment = if o.deployment.is_some() || options.deployment_policy.is_some() {
            Some(&policy as &dyn DeploymentVerifier)
        } else {
            None
        };
        let results = try_join_all(fetched.attestations.iter().map(|report| {
            verify_model_attestation(
                report,
                &fetched.client_binding,
                o.policy.as_ref(),
                ModelAttestationVerifiers {
                    tdx_quote: o.tdx_quote.as_deref(),
                    gpu_evidence: o.gpu_evidence.as_deref(),
                    deployment,
                },
            )
        }))
        .await?;
        Ok::<_, InferenceError>(results)
    };
    let ((gateway, ohttp), models) = futures_util::try_join!(gateway, models)?;
    let selected = models
        .iter()
        .find(|m| {
            m.evidence.signer.signing_algo == options.signing_algo && m.signing_public_key.is_some()
        })
        .cloned();
    if !models.is_empty() && selected.is_none() {
        return Err(protocol(
            "e2ee.model_public_key_required",
            "no matching verified model public key",
        )
        .into());
    }
    Ok(Arc::new(Session {
        gateway: Some(gateway),
        models,
        selected,
        direct: None,
        verified_at: verified_at(),
        client,
        api: EvidenceApi::Gateway(api),
        ohttp,
    }))
}
