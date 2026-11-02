//! The replica over HTTP: the second machine serves a [`ReplicaNode`], the laptop speaks to it
//! through [`HttpLink`]. The channel is the tailnet; its ACL admits the laptop's tag only. There
//! is no other authentication, and none is needed for safety: an entry is accepted only if it
//! extends the chain, and the laptop trusts a reply only if the replica's key signed it.

use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;

use crate::replog::{Entry, Link, LinkError, LogError, ReplicaNode, SignedHead};

/// Bodies are one entry or a short list of numbers.
const BODY_LIMIT: usize = 256 * 1024;

pub fn router(node: Arc<ReplicaNode>) -> Router {
    Router::new()
        .route("/replica/head", get(head))
        .route("/replica/append", post(append))
        .route("/replica/hashes", post(hashes))
        .route("/replica/entries", get(entries))
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
}

impl HttpLink {
    pub fn new(base: &str) -> Result<Self, reqwest::Error> {
        Ok(Self {
            base: base.trim_end_matches('/').to_string(),
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()?,
        })
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
        Self::read(
            self.http
                .post(format!("{}/replica/hashes", self.base))
                .json(&serde_json::json!({ "seqs": seqs }))
                .send()
                .await,
        )
        .await
    }

    async fn fetch(&self, from: u64, limit: u32) -> Result<Vec<Entry>, LinkError> {
        Self::read(
            self.http
                .get(format!("{}/replica/entries", self.base))
                .query(&[("from", from.to_string()), ("limit", limit.to_string())])
                .send()
                .await,
        )
        .await
    }
}
