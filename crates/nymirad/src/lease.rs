//! Opening and retiring a lease at the operator.
//!
//! The 201 answer carries a `lease_token` that the embedded wallet ignores. This module keeps it
//! apart from every logged or serialized value and sends it as `X-Nymira-Lease-Token` on each call
//! made under the lease.

use std::future::Future;

use serde_json::Value;
use zkapi_types::wire::ApiRequestV2;

use crate::gate::{self, Refusal};

pub const TOKEN_HEADER: &str = "X-Nymira-Lease-Token";
pub const PUZZLE_HEADER: &str = "X-Nymira-Puzzle";

/// The lease credential. `Debug` and `Display` print nothing of it.
#[derive(Clone)]
pub struct LeaseToken(String);

impl LeaseToken {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for LeaseToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("LeaseToken(<redacted>)")
    }
}

#[derive(Clone, Debug)]
pub struct Lease {
    pub id: String,
    pub token: LeaseToken,
    pub client_request_id: String,
    /// Micro-USD.
    pub bound: u128,
    pub issued_at: u64,
    pub expires_at: u64,
}

/// An operator error: the HTTP status and the `code` of its error table.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("operator answered {status} {code}: {message}")]
pub struct OperatorError {
    pub status: u16,
    pub code: String,
    pub message: String,
    pub retry_after_secs: Option<u64>,
}

#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    #[error(transparent)]
    Operator(#[from] OperatorError),
    #[error("could not reach the operator: {0}")]
    Transport(String),
    #[error("the operator's answer did not parse: {0}")]
    Shape(String),
    #[error(transparent)]
    Gate(#[from] Refusal),
    #[error("could not build the request: {0}")]
    Build(String),
}

pub struct LeaseClient {
    http: reqwest::Client,
    base: String,
}

fn transport(e: reqwest::Error) -> LeaseError {
    LeaseError::Transport(e.without_url().to_string())
}

impl LeaseClient {
    pub fn new(http: reqwest::Client, base: impl Into<String>) -> Self {
        Self {
            http,
            base: base.into().trim_end_matches('/').to_string(),
        }
    }

    pub async fn status(&self) -> Result<Value, LeaseError> {
        let r = self
            .http
            .get(format!("{}/v1/status", self.base))
            .send()
            .await
            .map_err(transport)?;
        r.json()
            .await
            .map_err(|e| LeaseError::Shape(e.without_url().to_string()))
    }

    async fn answer(response: reqwest::Response) -> Result<Value, LeaseError> {
        let status = response.status().as_u16();
        let retry_header = response
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok());
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            return Ok(body);
        }
        let e = &body["error"];
        Err(OperatorError {
            status,
            code: e["code"].as_str().unwrap_or("").to_string(),
            message: e["message"].as_str().unwrap_or("").to_string(),
            retry_after_secs: e["retry_after_secs"].as_u64().or(retry_header),
        }
        .into())
    }

    /// `POST /v2/openrouter/leases`.
    pub async fn open(
        &self,
        request: &ApiRequestV2,
        puzzle: Option<&str>,
    ) -> Result<Lease, LeaseError> {
        let mut call = self
            .http
            .post(format!("{}/v2/openrouter/leases", self.base))
            .json(request);
        if let Some(p) = puzzle {
            call = call.header(PUZZLE_HEADER, p);
        }
        let body = Self::answer(call.send().await.map_err(transport)?).await?;
        let text = |k: &str| body[k].as_str().map(String::from);
        let num = |k: &str| body[k].as_u64();
        Ok(Lease {
            id: text("lease_id").ok_or_else(|| LeaseError::Shape("lease_id".into()))?,
            token: LeaseToken(
                text("lease_token").ok_or_else(|| LeaseError::Shape("lease_token".into()))?,
            ),
            client_request_id: text("client_request_id").unwrap_or_default(),
            bound: text("bound")
                .and_then(|b| b.parse().ok())
                .ok_or_else(|| LeaseError::Shape("bound".into()))?,
            issued_at: num("issued_at").ok_or_else(|| LeaseError::Shape("issued_at".into()))?,
            expires_at: num("expires_at").ok_or_else(|| LeaseError::Shape("expires_at".into()))?,
        })
    }

    /// `POST /v2/openrouter/leases/{id}`: ends the lease so the operator settles it.
    pub async fn retire(&self, lease: &Lease) -> Result<Value, LeaseError> {
        let r = self
            .http
            .post(format!("{}/v2/openrouter/leases/{}", self.base, lease.id))
            .header(TOKEN_HEADER, lease.token.expose())
            .send()
            .await
            .map_err(transport)?;
        Self::answer(r).await
    }

    /// `GET /v1/status` with the lease token (chat, status and x402 calls carry it).
    pub async fn lease_status(&self, lease: &Lease) -> Result<Value, LeaseError> {
        let r = self
            .http
            .get(format!("{}/v2/openrouter/leases/{}", self.base, lease.id))
            .header(TOKEN_HEADER, lease.token.expose())
            .send()
            .await
            .map_err(transport)?;
        Self::answer(r).await
    }
}

/// Why the request is being built: `Rebuild` after `root_stale`, when the caller rebuilds the
/// tree from `get_leaves`, sets the old pending request aside and proves again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Attempt {
    First,
    Rebuild,
}

/// The pinned values a lease needs: the vault's state key and lease parameters.
pub struct Pinned<'a> {
    pub operator_pk: &'a Value,
    pub config: &'a Value,
}

/// Verifies the operator's status, builds the request through `build`, and opens the lease.
///
/// `puzzle_required` refetches the status and retries once with a fresh solution; `root_stale`
/// calls `build(Attempt::Rebuild)` and retries once; a second refusal of either is returned, never
/// looped. Every other error is returned as the operator's `code`.
pub async fn open_lease<F, Fut>(
    client: &LeaseClient,
    pinned: &Pinned<'_>,
    now_secs: impl Fn() -> u64,
    mut build: F,
) -> Result<Lease, LeaseError>
where
    F: FnMut(Attempt) -> Fut,
    Fut: Future<Output = Result<ApiRequestV2, String>>,
{
    async fn checked(
        client: &LeaseClient,
        pinned: &Pinned<'_>,
        now: u64,
    ) -> Result<(Value, gate::Nonce), LeaseError> {
        let status = client.status().await?;
        gate::check_params(&status, pinned.config)?;
        let nonce = gate::verify_status(&status, pinned.operator_pk, now)?;
        Ok((status, nonce))
    }

    // No proof is built before the nonce verifies.
    let (status, nonce) = checked(client, pinned, now_secs()).await?;
    let mut puzzle = gate::puzzle_header(&status, &nonce);
    let mut request = build(Attempt::First).await.map_err(LeaseError::Build)?;
    let (mut retried_puzzle, mut rebuilt) = (false, false);
    loop {
        match client.open(&request, puzzle.as_deref()).await {
            Err(LeaseError::Operator(e)) if e.code == "puzzle_required" && !retried_puzzle => {
                retried_puzzle = true;
                let (status, nonce) = checked(client, pinned, now_secs()).await?;
                puzzle = gate::puzzle_header(&status, &nonce);
            }
            Err(LeaseError::Operator(e)) if e.code == "root_stale" && !rebuilt => {
                rebuilt = true;
                request = build(Attempt::Rebuild).await.map_err(LeaseError::Build)?;
            }
            other => return other,
        }
    }
}

/// Default lease bound in micro-USD (the 0.10 USDC class).
pub const DEFAULT_BOUND: u128 = 100_000;

/// The bound class for a request: 0.10 unless the caller asked for more with `max_lease_usd`
/// (0.5 or above), then the largest class the vault lists that stays within it. `None` when the
/// vault lists no usable class.
pub fn choose_bound(classes: &[u128], max_lease_usd: Option<f64>) -> Option<u128> {
    match max_lease_usd {
        Some(usd) if usd >= 0.5 => {
            let cap = (usd * 1_000_000.0) as u128;
            classes.iter().copied().filter(|c| *c <= cap).max()
        }
        _ => classes.contains(&DEFAULT_BOUND).then_some(DEFAULT_BOUND),
    }
}

/// Makes the wallet's lease request proof for `bound` at `root` with the note's `siblings`. The
/// wallet writes the bound to disk before it proves and journals the request before this returns.
pub fn prepare(
    wallet: &mut zkapi_client::wallet::Wallet,
    root: zkapi_types::Felt252,
    siblings: Vec<zkapi_types::Felt252>,
    bound: u128,
) -> Result<ApiRequestV2, String> {
    let payload =
        serde_json::to_string(&zkapi_types::wire::OpenRouterLeaseAuthorization::default())
            .map_err(|e| e.to_string())?;
    let hash = zkapi_types::canonical_payload_hash(payload.as_bytes());
    wallet
        .prepare_request_with_bound(&payload, hash, root, siblings, bound)
        .map_err(|e| e.to_string())
}

/// Sets the refused request aside before a new proof is made after `root_stale`. The operator
/// refused it before binding the nullifier, so the same state can prove again.
pub fn set_aside_stale(wallet: &zkapi_client::wallet::Wallet) -> Result<(), String> {
    wallet
        .archive_pending_request("root_stale")
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_small_class_is_the_default_and_the_large_one_needs_a_request() {
        let classes = [100_000, 500_000];
        assert_eq!(choose_bound(&classes, None), Some(100_000));
        assert_eq!(choose_bound(&classes, Some(0.1)), Some(100_000));
        assert_eq!(choose_bound(&classes, Some(0.49)), Some(100_000));
        assert_eq!(choose_bound(&classes, Some(0.5)), Some(500_000));
        assert_eq!(choose_bound(&classes, Some(5.0)), Some(500_000));
        assert_eq!(choose_bound(&[500_000], None), None);
    }


    #[test]
    fn the_token_never_prints() {
        let lease = Lease {
            id: "l1".into(),
            token: LeaseToken("secret-token-value".into()),
            client_request_id: "c".into(),
            bound: 100_000,
            issued_at: 1,
            expires_at: 2,
        };
        assert!(!format!("{lease:?}").contains("secret-token-value"));
        assert_eq!(lease.token.expose(), "secret-token-value");
    }
}
