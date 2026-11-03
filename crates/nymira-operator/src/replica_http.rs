//! The replica over HTTP: the second machine serves a [`ReplicaNode`], the laptop speaks to it
//! through [`HttpLink`]. The channel is the tailnet; its ACL admits the laptop's tag only. When
//! the replica pins the operator's entry key, every append must be signed by it and so must every
//! read of entries or hashes: the laptop signs `METHOD|path?query|timestamp` with the same key
//! and the replica refuses a missing, wrong or stale (over a minute) signature. The head is
//! public; the key signed it. The laptop trusts a reply only if the replica's key signed the head
//! it leads to.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::replog::{Entry, Link, LinkError, LogError, ReplicaNode, SignedHead};

/// Bodies are one entry or a short list of numbers.
const BODY_LIMIT: usize = 256 * 1024;

/// Requests older or newer than this many seconds are refused.
const SIGNATURE_SKEW_SECS: u64 = 60;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// What the laptop signs for a read.
fn request_message(method: &str, target: &str, ts: u64) -> Vec<u8> {
    format!("{method}|{target}|{ts}").into_bytes()
}

/// Refuses a read of entries or hashes that the pinned entry key did not sign (when a key is
/// pinned).
async fn require_signature(
    State(node): State<Arc<ReplicaNode>>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let Some(pk) = node.laptop_key() else {
        return Ok(next.run(request).await);
    };
    let target = request.uri().path_and_query().map_or_else(
        || request.uri().path().to_string(),
        |p| p.as_str().to_string(),
    );
    let headers: &HeaderMap = request.headers();
    let ts: u64 = headers
        .get("x-nymira-ts")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    let sig = headers
        .get("x-nymira-sig")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| hex::decode(v).ok())
        .and_then(|v| <[u8; 64]>::try_from(v).ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if now_secs().abs_diff(ts) > SIGNATURE_SKEW_SECS {
        return Err(StatusCode::UNAUTHORIZED);
    }
    let key = ed25519_dalek::VerifyingKey::from_bytes(&pk).map_err(|_| StatusCode::UNAUTHORIZED)?;
    let message = request_message(request.method().as_str(), &target, ts);
    key.verify_strict(&message, &ed25519_dalek::Signature::from_bytes(&sig))
        .map_err(|_| StatusCode::UNAUTHORIZED)?;
    Ok(next.run(request).await)
}

pub fn router(node: Arc<ReplicaNode>) -> Router {
    let reads = Router::new()
        .route("/replica/hashes", post(hashes))
        .route("/replica/entries", get(entries))
        .route_layer(axum::middleware::from_fn_with_state(
            node.clone(),
            require_signature,
        ));
    Router::new()
        .route("/replica/head", get(head))
        .route("/replica/append", post(append))
        .merge(reads)
        .layer(axum::extract::DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(node)
}

type Failure = (StatusCode, Json<LogError>);

fn refused(e: LogError) -> Failure {
    (StatusCode::CONFLICT, Json(e))
}

async fn head(State(n): State<Arc<ReplicaNode>>) -> Json<SignedHead> {
    Json(n.head().await)
}

async fn append(
    State(n): State<Arc<ReplicaNode>>,
    Json(entry): Json<Entry>,
) -> Result<Json<SignedHead>, Failure> {
    n.append(&entry).await.map(Json).map_err(refused)
}

#[derive(Deserialize)]
struct Seqs {
    seqs: Vec<u64>,
}

async fn hashes(
    State(n): State<Arc<ReplicaNode>>,
    Json(body): Json<Seqs>,
) -> Result<Json<Vec<Option<String>>>, Failure> {
    if body.seqs.len() > 64 {
        return Err(refused(LogError::Bad(
            "at most 64 entries per challenge".into(),
        )));
    }
    n.hashes(&body.seqs).await.map(Json).map_err(refused)
}

#[derive(Deserialize)]
struct Page {
    from: u64,
    limit: u32,
}

async fn entries(
    State(n): State<Arc<ReplicaNode>>,
    Query(page): Query<Page>,
) -> Result<Json<Vec<Entry>>, Failure> {
    n.from(page.from, page.limit.min(100))
        .await
        .map(Json)
        .map_err(refused)
}

/// The laptop's side of the channel.
pub struct HttpLink {
    base: String,
    http: reqwest::Client,
    /// Signs reads when the replica pins the operator's entry key.
    signing: Option<ed25519_dalek::SigningKey>,
}

impl HttpLink {
    pub fn new(base: &str) -> Result<Self, reqwest::Error> {
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()?,
            signing: None,
        })
    }

    /// Signs every read with the operator's entry key.
    pub fn with_signing_key(mut self, key: ed25519_dalek::SigningKey) -> Self {
        self.signing = Some(key);
        self
    }

    /// The headers a read carries: a timestamp and the signature over method, target and time.
    fn signed(
        &self,
        builder: reqwest::RequestBuilder,
        method: &str,
        target: &str,
    ) -> reqwest::RequestBuilder {
        use ed25519_dalek::Signer;
        let Some(key) = &self.signing else {
            return builder;
        };
        let ts = now_secs();
        let sig = key.sign(&request_message(method, target, ts));
        builder
            .header("x-nymira-ts", ts.to_string())
            .header("x-nymira-sig", hex::encode(sig.to_bytes()))
    }

    async fn read<T: serde::de::DeserializeOwned>(
        response: reqwest::Result<reqwest::Response>,
    ) -> Result<T, LinkError> {
        let response = response.map_err(|e| LinkError::Down(e.to_string()))?;
        if response.status() == StatusCode::CONFLICT {
            let e: LogError = response
                .json()
                .await
                .map_err(|e| LinkError::Down(e.to_string()))?;
            return Err(LinkError::Refused(e));
        }
        if !response.status().is_success() {
            return Err(LinkError::Down(format!("status {}", response.status())));
        }
        response
            .json()
            .await
            .map_err(|e| LinkError::Down(e.to_string()))
    }
}

#[async_trait]
impl Link for HttpLink {
    async fn append(&self, entry: &Entry) -> Result<SignedHead, LinkError> {
        Self::read(
            self.http
                .post(format!("{}/replica/append", self.base))
                .json(entry)
                .send()
                .await,
        )
        .await
    }

    async fn head(&self) -> Result<SignedHead, LinkError> {
        Self::read(
            self.http
                .get(format!("{}/replica/head", self.base))
                .send()
                .await,
        )
        .await
    }

    async fn hashes(&self, seqs: &[u64]) -> Result<Vec<Option<String>>, LinkError> {
        let builder = self
            .http
            .post(format!("{}/replica/hashes", self.base))
            .json(&serde_json::json!({ "seqs": seqs }));
        Self::read(self.signed(builder, "POST", "/replica/hashes").send().await).await
    }

    async fn fetch(&self, from: u64, limit: u32) -> Result<Vec<Entry>, LinkError> {
        let target = format!("/replica/entries?from={from}&limit={limit}");
        let builder = self.http.get(format!("{}{target}", self.base));
        Self::read(self.signed(builder, "GET", &target).send().await).await
    }
}
