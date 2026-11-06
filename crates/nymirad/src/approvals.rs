//! Owner approval for agent calls above `confirm_above_usd`.
//!
//! Such a call is held instead of sent: the agent gets 402 `approval_required` with an approval
//! ID. The owner reads what is waiting (`GET /v1/nymira/approvals`), signs an approval with a
//! full-access key of the owner account (NEP-413) and hands it to `nymirad` with the owner token
//! (`POST /v1/nymira/approvals`). The agent then sends the same request again with the ID in the
//! `X-Nymira-Approval-Id` header, and it goes out once.
//!
//! A held entry keeps the request's hash and figures, never its body, and lives in memory only:
//! nothing here touches the state directory, a backup or a log. An entry lasts 10 minutes.
//!
//! The signed file is JSON like a signed policy: `account_id`, `public_key`, `signature`,
//! `nonce` (32 bytes), `message` and `recipient`. The recipient is
//! `nymirad-approval:<agent>:<note commitment>`, which no policy recipient can equal. The message
//! is `{"kind":"nymira-approval","id":..,"request_hash":..,"amount_micro":..,"nonce":..}`; the
//! nonce is the one `nymirad` showed next to the ID.

use std::sync::{Mutex, MutexGuard};

use axum::http::StatusCode;
use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use rand::RngCore;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::agents::Book;
use crate::openai::ApiError;
use crate::policy::nep413_hash;
use crate::rpc::Rpc;

pub const KIND: &str = "nymira-approval";
/// At most this many requests wait at once for one agent.
pub const MAX_HELD: usize = 5;
/// How long a held request, and an approval for it, stays valid.
pub const TTL_SECS: u64 = 600;
/// Entries are dropped this long after they were made; until then a used or expired ID is named.
const KEEP_SECS: u64 = 3_600;
pub const HEADER: &str = "x-nymira-approval-id";

/// The recipient tag an approval for this agent and note must be signed for.
pub fn recipient(agent: &str, note: &str) -> String {
    format!("nymirad-approval:{agent}:{note}")
}

/// SHA-256 of the request as JSON (keys sorted), hex.
pub fn request_hash(request: &Value) -> String {
    hex::encode(Sha256::digest(
        serde_json::to_vec(request).expect("a JSON value serializes"),
    ))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Pending,
    Approved,
    Used,
}

struct Held {
    id: String,
    agent: String,
    hash: String,
    model: String,
    input_tokens: u64,
    cost_micro: u128,
    nonce: String,
    created: u64,
    state: State,
}

impl Held {
    fn live(&self, now: u64) -> bool {
        self.state != State::Used && now < self.created + TTL_SECS
    }
}

#[derive(Default)]
pub struct Approvals {
    held: Mutex<Vec<Held>>,
}

fn lock(m: &Mutex<Vec<Held>>) -> MutexGuard<'_, Vec<Held>> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn refuse(status: StatusCode, code: &str, message: impl Into<String>) -> ApiError {
    ApiError {
        status,
        kind: "invalid_request_error",
        code: code.into(),
        message: message.into(),
        retry_after_secs: None,
        approval_id: None,
        detail: None,
    }
}

fn required(id: &str, cost_micro: u128) -> ApiError {
    ApiError {
        status: StatusCode::PAYMENT_REQUIRED,
        kind: "insufficient_funds",
        code: "approval_required".into(),
        message: format!(
            "this call (up to {} USD) needs the owner's approval; approval ID {id}. Send the same request again with the {HEADER} header once the owner has approved",
            usd(cost_micro)
        ),
        retry_after_secs: None,
        approval_id: Some(id.into()),
        detail: None,
    }
}

fn usd(micro: u128) -> String {
    format!("{}.{:06}", micro / 1_000_000, micro % 1_000_000)
}

fn random_hex(bytes: usize) -> String {
    let mut b = vec![0u8; bytes];
    rand::rngs::OsRng.fill_bytes(&mut b);
    hex::encode(b)
}

impl Approvals {
    /// The check an agent request above `confirm_above_usd` must pass. Without an ID the request
    /// is held (or found again, if it is already held) and refused; with the ID of an approved
    /// entry for the same request it passes, once.
    pub fn gate(
        &self,
        agent: &str,
        request: &Value,
        id: Option<&str>,
        input_tokens: u64,
        cost_micro: u128,
        now: u64,
    ) -> Result<(), ApiError> {
        let hash = request_hash(request);
        let mut held = lock(&self.held);
        held.retain(|h| now < h.created + KEEP_SECS);
        let Some(id) = id else {
            if let Some(h) = held
                .iter()
                .find(|h| h.agent == agent && h.hash == hash && h.live(now))
            {
                return Err(required(&h.id, h.cost_micro));
            }
            if held
                .iter()
                .filter(|h| h.agent == agent && h.live(now))
                .count()
                >= MAX_HELD
            {
                let mut e = refuse(
                    StatusCode::TOO_MANY_REQUESTS,
                    "approval_limit",
                    format!("{MAX_HELD} requests of this agent already wait for approval"),
                );
                e.kind = "rate_limit_error";
                return Err(e);
            }
            let entry = Held {
                id: format!("apv_{}", random_hex(16)),
                agent: agent.into(),
                hash,
                model: request["model"].as_str().unwrap_or("").into(),
                input_tokens,
                cost_micro,
                nonce: random_hex(32),
                created: now,
                state: State::Pending,
            };
            let e = required(&entry.id, cost_micro);
            held.push(entry);
            return Err(e);
        };
        let Some(h) = held.iter_mut().find(|h| h.id == id && h.agent == agent) else {
            return Err(refuse(
                StatusCode::NOT_FOUND,
                "approval_unknown",
                "no request is held under this approval ID",
            ));
        };
        if h.state == State::Used {
            return Err(refuse(
                StatusCode::CONFLICT,
                "approval_used",
                "this approval was already used",
            ));
        }
        if now >= h.created + TTL_SECS {
            return Err(refuse(
                StatusCode::GONE,
                "approval_expired",
                "the approval ID expired; send the request again for a new one",
            ));
        }
        if h.hash != hash {
            return Err(refuse(
                StatusCode::CONFLICT,
                "approval_mismatch",
                "this is not the request the approval ID was issued for",
            ));
        }
        match h.state {
            State::Pending => Err(required(&h.id, h.cost_micro)),
            _ => {
                h.state = State::Used;
                Ok(())
            }
        }
    }

    /// What waits for the owner.
    pub fn list(&self, now: u64) -> Value {
        let held = lock(&self.held);
        Value::Array(
            held.iter()
                .filter(|h| h.live(now))
                .map(|h| {
                    json!({
                        "approval_id": h.id,
                        "agent": h.agent,
                        "model": h.model,
                        "input_tokens": h.input_tokens,
                        "amount_micro": h.cost_micro.to_string(),
                        "estimated_cost_usd": usd(h.cost_micro),
                        "request_hash": h.hash,
                        "nonce": h.nonce,
                        "approved": h.state == State::Approved,
                        "expires_in_secs": h.created + TTL_SECS - now,
                    })
                })
                .collect(),
        )
    }

    /// Takes the owner's signed approval. Everything that needs no network is checked first;
    /// the key is read from the chain last, and only then is the entry marked approved.
    pub async fn approve(
        &self,
        bytes: &[u8],
        book: &Book,
        rpcs: &[Rpc],
        now: u64,
    ) -> Result<String, ApiError> {
        let bad = |m: &str| refuse(StatusCode::BAD_REQUEST, "approval_invalid", m);
        let env: Envelope =
            serde_json::from_slice(bytes).map_err(|e| bad(&format!("signed approval: {e}")))?;
        let agent = env
            .recipient
            .strip_prefix("nymirad-approval:")
            .and_then(|r| r.split(':').next())
            .and_then(|name| book.agents.iter().find(|a| a.name == name))
            .filter(|a| env.recipient == recipient(&a.name, &a.note))
            .ok_or_else(|| bad("signed for no agent or note of this nymirad"))?;
        if env.account_id != agent.owner_account {
            return Err(refuse(
                StatusCode::FORBIDDEN,
                "approval_wrong_signer",
                format!(
                    "signed by {}, not by the budget note's owner account {}",
                    env.account_id, agent.owner_account
                ),
            ));
        }
        let b64 = |s: &str| {
            base64::engine::general_purpose::STANDARD
                .decode(s)
                .map_err(|_| bad("bad base64"))
        };
        let key = env
            .public_key
            .strip_prefix("ed25519:")
            .and_then(|k| bs58::decode(k).into_vec().ok())
            .and_then(|k| <[u8; 32]>::try_from(k).ok())
            .and_then(|k| VerifyingKey::from_bytes(&k).ok())
            .ok_or_else(|| bad("the public key is not an ed25519 key"))?;
        let nonce: [u8; 32] = b64(&env.nonce)?
            .try_into()
            .map_err(|_| bad("the nonce must be 32 bytes"))?;
        let sig: [u8; 64] = b64(&env.signature)?
            .try_into()
            .map_err(|_| bad("the signature must be 64 bytes"))?;
        key.verify_strict(
            &nep413_hash(&env.message, &nonce, &env.recipient),
            &Signature::from_bytes(&sig),
        )
        .map_err(|_| bad("the signature does not match the message"))?;
        let msg: Message =
            serde_json::from_str(&env.message).map_err(|e| bad(&format!("message: {e}")))?;
        if msg.kind != KIND {
            return Err(bad("the message is not a Nymira approval"));
        }
        {
            let mut held = lock(&self.held);
            held.retain(|h| now < h.created + KEEP_SECS);
            let h = held
                .iter()
                .find(|h| h.id == msg.id && h.agent == agent.name)
                .ok_or_else(|| {
                    refuse(
                        StatusCode::NOT_FOUND,
                        "approval_unknown",
                        "no request is held under this approval ID",
                    )
                })?;
            if h.state != State::Pending {
                return Err(refuse(
                    StatusCode::CONFLICT,
                    "approval_used",
                    "this approval ID was already approved or used",
                ));
            }
            if now >= h.created + TTL_SECS {
                return Err(refuse(
                    StatusCode::GONE,
                    "approval_expired",
                    "the approval ID expired",
                ));
            }
            if h.hash != msg.request_hash
                || h.cost_micro.to_string() != msg.amount_micro
                || h.nonce != msg.nonce
            {
                return Err(refuse(
                    StatusCode::CONFLICT,
                    "approval_mismatch",
                    "the signed request hash, amount or nonce differs from the held request",
                ));
            }
        }
        crate::policy::check_key(rpcs, &env.account_id, &env.public_key)
            .await
            .map_err(|e| refuse(StatusCode::FORBIDDEN, "approval_wrong_signer", e))?;
        // The entry may have changed during the key check: look again before marking it.
        let mut held = lock(&self.held);
        match held
            .iter_mut()
            .find(|h| h.id == msg.id && h.agent == agent.name)
        {
            Some(h) if h.state == State::Pending && now < h.created + TTL_SECS => {
                h.state = State::Approved;
                Ok(h.id.clone())
            }
            _ => Err(refuse(
                StatusCode::CONFLICT,
                "approval_used",
                "this approval ID changed while its key was checked",
            )),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Envelope {
    account_id: String,
    public_key: String,
    signature: String,
    nonce: String,
    message: String,
    recipient: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    kind: String,
    id: String,
    request_hash: String,
    amount_micro: String,
    nonce: String,
}

// ---- HTTP: owner routes ----

pub const PATH: &str = "/v1/nymira/approvals";
/// The owner's read-only view of each agent's spend, budget note and policy.
pub const AGENTS_PATH: &str = "/v1/nymira/agents";

#[derive(Clone)]
struct Owner {
    gw: std::sync::Arc<crate::gateway::Gateway>,
    registry: crate::agents::Registry,
}

/// The two owner-token routes. The guard lets nothing else reach them: an agent token gets
/// `agent_route_forbidden` there before a handler runs.
pub fn routes<S: Clone + Send + Sync + 'static>(
    gw: std::sync::Arc<crate::gateway::Gateway>,
    registry: crate::agents::Registry,
) -> axum::Router<S> {
    axum::Router::new()
        .route(PATH, axum::routing::get(list_handler).post(approve_handler))
        .route(AGENTS_PATH, axum::routing::get(agents_handler))
        .with_state(Owner { gw, registry })
}

async fn list_handler(axum::extract::State(o): axum::extract::State<Owner>) -> axum::Json<Value> {
    axum::Json(o.gw.approvals().list(o.gw.now_secs()))
}

async fn agents_handler(
    axum::extract::State(o): axum::extract::State<Owner>,
) -> Result<axum::Json<Value>, ApiError> {
    let book = o
        .registry
        .load()
        .map_err(|why| refuse(StatusCode::SERVICE_UNAVAILABLE, "registry_unreadable", why))?;
    Ok(axum::Json(o.gw.agent_overview(&book)))
}

async fn approve_handler(
    axum::extract::State(o): axum::extract::State<Owner>,
    body: axum::body::Bytes,
) -> Result<axum::Json<Value>, ApiError> {
    let book = o
        .registry
        .load()
        .map_err(|why| refuse(StatusCode::SERVICE_UNAVAILABLE, "registry_unreadable", why))?;
    let id =
        o.gw.approvals()
            .approve(&body, &book, &o.gw.key_rpcs(), o.gw.now_secs())
            .await?;
    Ok(axum::Json(json!({ "approved": id })))
}
