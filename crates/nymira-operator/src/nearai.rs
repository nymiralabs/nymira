//! The NEAR AI Cloud adapter.
//!
//! Key management, the balance and usage use the organisation login: a JWT from the stored
//! refresh token, with a rotated refresh token written back (encrypted, atomically) before
//! anything else uses it. Chat, signatures and attestation use the lease's own key.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use bytes::Bytes;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::alarm::Alarms;
use crate::clock::Clock;
use crate::provider::{
    key_cap_nano_usd, ChatBody, ChatRequest, ChatResponse, EvidenceQuery, KeyUsage, LiveModel,
    MintedKey, ProviderAdapter, ProviderError, KEY_LIFETIME_SECS,
};
use crate::sealed::SealedFile;

/// A session token without an `exp` claim is treated as valid this long.
const DEFAULT_SESSION_SECS: u64 = 300;
/// Renew the session token this long before it expires.
const SESSION_SLACK_SECS: u64 = 30;
/// Alarm when less than this share of the refresh token's measured lifetime is left.
const REFRESH_WARN_PERCENT: u64 = 25;
/// Chats whose signatures the evidence fetch covers, newest first.
const EVIDENCE_CHATS: usize = 20;
const PAGE: usize = 100;
/// How long `mint` waits for a new key to start working: 24 tries, half a second apart.
const KEY_ACTIVE_TRIES: u32 = 24;
const KEY_ACTIVE_POLL: Duration = Duration::from_millis(500);
/// Chat requests sent again after a 401 on a key that may be too new, and the wait between them.
const NEW_KEY_CHAT_RETRIES: u32 = 3;
const NEW_KEY_CHAT_RETRY_WAIT: Duration = Duration::from_secs(2);

/// The client identity the refresh token was made under. The service ties a refresh token to the
/// `User-Agent` of the client that made it (measured 2026-11-02: the exchange answered 401 for
/// every other value), so the adapter always sends the same one. A token taken from a browser
/// login needs that browser's identity; the first exchange then fixes it for later rotations.
pub const BROWSER_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/156.0.0.0 Safari/537.36";

pub struct NearAiConfig {
    /// Sent on every request; see [`BROWSER_USER_AGENT`].
    pub user_agent: String,
    /// `https://cloud-api.near.ai` outside tests.
    pub base_url: String,
    pub org_id: String,
    pub workspace_id: String,
}

// No `Debug` on the two secret-bearing types: a stray `{:?}` must not print a token.
#[derive(Clone, Serialize, Deserialize)]
struct Login {
    refresh_token: String,
    /// Unix seconds; the provider reports it with each exchange.
    expires_at: Option<u64>,
    /// When this refresh token was first seen: the start of its measured lifetime.
    issued_at: u64,
}

struct Session {
    login: Login,
    access: Option<(String, u64)>,
}

#[derive(Clone, Serialize, Deserialize)]
struct KeyEntry {
    id: String,
    secret: String,
    /// The key is dead; only the id stays, because settlement reads its usage afterwards.
    #[serde(default)]
    revoked: bool,
}

/// What the evidence fetch needs from a lease's chats: ids and models, newest last.
#[derive(Default)]
struct ChatLog(Vec<(String, String)>);

pub struct NearAiAdapter {
    cfg: NearAiConfig,
    http: reqwest::Client,
    clock: Arc<dyn Clock>,
    alarms: Arc<Alarms>,
    login_file: SealedFile,
    key_file: SealedFile,
    session: tokio::sync::Mutex<Session>,
    keys: Mutex<HashMap<String, KeyEntry>>,
    lease_chat: LeaseChat,
}

impl NearAiAdapter {
    /// `first_refresh_token` is read only when the login file does not exist yet.
    pub fn new(
        cfg: NearAiConfig,
        clock: Arc<dyn Clock>,
        alarms: Arc<Alarms>,
        login_file: SealedFile,
        key_file: SealedFile,
        first_refresh_token: Option<&str>,
    ) -> Result<Self, ProviderError> {
        let stored = |e: crate::sealed::SealError| ProviderError::Unavailable(e.to_string());
        let login = match login_file.read().map_err(stored)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| ProviderError::Unavailable("login file is damaged".into()))?,
            None => {
                let token = first_refresh_token.ok_or_else(|| {
                    ProviderError::Unavailable("no stored login and no first refresh token".into())
                })?;
                let login = Login {
                    refresh_token: token.into(),
                    expires_at: None,
                    issued_at: clock.now_secs(),
                };
                login_file
                    .write(&serde_json::to_vec(&login).expect("login serializes"))
                    .map_err(stored)?;
                login
            }
        };
        let keys = match key_file.read().map_err(stored)? {
            Some(bytes) => serde_json::from_slice(&bytes)
                .map_err(|_| ProviderError::Unavailable("key file is damaged".into()))?,
            None => HashMap::new(),
        };
        let http = reqwest::Client::builder()
            .user_agent(cfg.user_agent.clone())
            .connect_timeout(Duration::from_secs(10))
            .build()
            .expect("client builds");
        let base = cfg.base_url.clone();
        Ok(Self {
            http: http.clone(),
            cfg,
            clock,
            alarms,
            login_file,
            key_file,
            session: tokio::sync::Mutex::new(Session {
                login,
                access: None,
            }),
            keys: Mutex::new(keys),
            lease_chat: LeaseChat::new(http, &base),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.cfg.base_url.trim_end_matches('/'), path)
    }

    fn keys(&self) -> std::sync::MutexGuard<'_, HashMap<String, KeyEntry>> {
        self.keys.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn save_keys(&self) -> Result<(), ProviderError> {
        let json = serde_json::to_vec(&*self.keys()).expect("keys serialize");
        self.key_file
            .write(&json)
            .map_err(|e| ProviderError::Unavailable(e.to_string()))
    }

    /// A valid session token, exchanged from the refresh token when needed.
    async fn session_token(&self, force: bool) -> Result<String, ProviderError> {
        let mut session = self.session.lock().await;
        let now = self.clock.now_secs();
        if !force {
            if let Some((token, exp)) = &session.access {
                if *exp > now + SESSION_SLACK_SECS {
                    return Ok(token.clone());
                }
            }
        }
        let reply = self
            .http
            .post(self.url("/v1/users/me/access-tokens"))
            .bearer_auth(&session.login.refresh_token)
            .timeout(Duration::from_secs(15))
            .send()
            .await
            .map_err(transport)?;
        if reply.status() == reqwest::StatusCode::UNAUTHORIZED {
            self.alarms
                .raise(
                    "refresh_token_rejected",
                    "NEAR AI refresh token rejected",
                    "The provider refused the stored refresh token. Minting is down until a person logs in again and stores a new one.",
                )
                .await;
            return Err(ProviderError::Rejected("refresh token rejected".into()));
        }
        let reply = checked(reply).await?;
        let body: Value = reply.json().await.map_err(transport)?;
        let access = body["access_token"]
            .as_str()
            .ok_or_else(|| ProviderError::Unavailable("no access token in the reply".into()))?
            .to_string();
        let refresh = body["refresh_token"]
            .as_str()
            .ok_or_else(|| ProviderError::Unavailable("no refresh token in the reply".into()))?;
        let expires_at = body["refresh_token_expiration"]
            .as_str()
            .and_then(parse_rfc3339);
        if refresh != session.login.refresh_token {
            session.login = Login {
                refresh_token: refresh.into(),
                expires_at,
                issued_at: now,
            };
        } else {
            // The expiry slides to a week after each exchange, so the lifetime to measure the
            // warning against starts at the last exchange, not at the first sight of the token.
            if expires_at.is_some() {
                session.login.issued_at = now;
            }
            session.login.expires_at = expires_at.or(session.login.expires_at);
        }
        // The old token may be dead already, so the new one is on disk before anything else
        // runs. A failed write keeps it in memory and raises the alarm.
        let bytes = serde_json::to_vec(&session.login).expect("login serializes");
        if self.login_file.write(&bytes).is_err() {
            self.alarms
                .raise(
                    "refresh_token_not_saved",
                    "NEAR AI refresh token could not be stored",
                    "A rotated refresh token could not be written to disk. It is held in memory only: do not restart the operator before the disk problem is fixed.",
                )
                .await;
        } else {
            self.alarms.resolve("refresh_token_not_saved");
        }
        self.alarms.resolve("refresh_token_rejected");
        let exp = jwt_exp(&access).unwrap_or(now + DEFAULT_SESSION_SECS);
        session.access = Some((access.clone(), exp));
        Ok(access)
    }

    /// Alarm while the refresh token has under a quarter of its measured lifetime left.
    /// Returns the seconds left, when the provider reported an expiry.
    pub async fn check_login_expiry(&self) -> Option<u64> {
        let login = self.session.lock().await.login.clone();
        let expires = login.expires_at?;
        let now = self.clock.now_secs();
        let left = expires.saturating_sub(now);
        let lifetime = expires.saturating_sub(login.issued_at).max(1);
        if left * 100 < lifetime * REFRESH_WARN_PERCENT {
            self.alarms
                .raise(
                    "refresh_token_expiring",
                    "NEAR AI refresh token is close to expiry",
                    &format!(
                        "{left} seconds of {lifetime} remain. A person must log in again and store a new refresh token."
                    ),
                )
                .await;
        } else {
            self.alarms.resolve("refresh_token_expiring");
        }
        Some(left)
    }

    /// A request with the session token; one retry with a fresh token on 401.
    async fn with_session(
        &self,
        build: impl Fn(&str) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ProviderError> {
        let token = self.session_token(false).await?;
        let reply = build(&token).send().await.map_err(transport)?;
        if reply.status() != reqwest::StatusCode::UNAUTHORIZED {
            return checked(reply).await;
        }
        let token = self.session_token(true).await?;
        checked(build(&token).send().await.map_err(transport)?).await
    }

    fn entry(&self, lease_id: &str) -> Result<KeyEntry, ProviderError> {
        self.keys()
            .get(lease_id)
            .cloned()
            .ok_or_else(|| ProviderError::NotFound("unknown lease".into()))
    }

    /// The entry of a key that can still be used.
    fn live_entry(&self, lease_id: &str) -> Result<KeyEntry, ProviderError> {
        Ok(self.entry(lease_id)?).and_then(|e| {
            if e.revoked {
                Err(ProviderError::NotFound("key is revoked".into()))
            } else {
                Ok(e)
            }
        })
    }

    /// Every active key of the workspace that this adapter named, as (provider id, name).
    async fn workspace_keys(&self) -> Result<Vec<(String, String)>, ProviderError> {
        let mut out = Vec::new();
        let mut offset = 0usize;
        loop {
            let path = format!("/v1/workspaces/{}/api-keys", self.cfg.workspace_id);
            let reply = self
                .with_session(|t| {
                    self.http
                        .get(self.url(&path))
                        .bearer_auth(t)
                        .query(&[("limit", PAGE), ("offset", offset)])
                        .timeout(Duration::from_secs(15))
                })
                .await?;
            let body: Value = reply.json().await.map_err(transport)?;
            let page = body["api_keys"].as_array().cloned().unwrap_or_default();
            for key in &page {
                let live = key["is_active"].as_bool().unwrap_or(false)
                    && key["deleted_at"].is_null()
                    && key["deleted"] != true;
                let name = key["name"].as_str().unwrap_or_default();
                if let (true, true, Some(id)) = (live, name.starts_with("k-"), key["id"].as_str()) {
                    out.push((id.to_string(), name.to_string()));
                }
            }
            offset += PAGE;
            if page.len() < PAGE || offset as u64 >= body["total"].as_u64().unwrap_or(0) {
                return Ok(out);
            }
        }
    }

    /// A new key answers 401 for a few seconds (2 to 5 s measured on 2026-11-02, up to 10 s
    /// documented). The lease is handed out only once the key works, or after this wait.
    async fn wait_until_active(&self, secret: &str) {
        for _ in 0..KEY_ACTIVE_TRIES {
            let ready = self
                .http
                .post(self.url("/v1/check_api_key"))
                .bearer_auth(secret)
                .timeout(Duration::from_secs(5))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success());
            if ready {
                return;
            }
            tokio::time::sleep(KEY_ACTIVE_POLL).await;
        }
    }

    /// A GET with the organisation login, for tools that look at what the provider returns.
    pub async fn session_get(&self, path: &str) -> Result<Value, ProviderError> {
        let reply = self
            .with_session(|t| {
                self.http
                    .get(self.url(path))
                    .bearer_auth(t)
                    .timeout(Duration::from_secs(20))
            })
            .await?;
        reply.json().await.map_err(transport)
    }

    /// True when the lease's key has been deactivated.
    pub fn is_revoked(&self, lease_id: &str) -> bool {
        self.entry(lease_id).is_ok_and(|e| e.revoked)
    }

    /// The secret of a lease's live key, for the process that proxies its chat.
    pub fn secret(&self, lease_id: &str) -> Option<String> {
        self.live_entry(lease_id).ok().map(|e| e.secret)
    }

    /// The provider's id for a lease's key, for tools.
    pub fn key_id(&self, lease_id: &str) -> Option<String> {
        self.keys().get(lease_id).map(|e| e.id.clone())
    }

    async fn deactivate(&self, key_id: &str) -> Result<(), ProviderError> {
        let path = format!("/v1/workspaces/{}/api-keys/{key_id}", self.cfg.workspace_id);
        match self
            .with_session(|t| {
                self.http
                    .patch(self.url(&path))
                    .bearer_auth(t)
                    .json(&json!({"is_active": false}))
                    .timeout(Duration::from_secs(15))
            })
            .await
        {
            Ok(_) => {}
            Err(ProviderError::NotFound(_)) => return Ok(()),
            Err(e) => return Err(e),
        }
        // Deleting frees the key count; the key is already dead, so a failure here is harmless.
        let _ = self
            .with_session(|t| {
                self.http
                    .delete(self.url(&path))
                    .bearer_auth(t)
                    .timeout(Duration::from_secs(15))
            })
            .await;
        Ok(())
    }
}

#[async_trait]
impl ProviderAdapter for NearAiAdapter {
    async fn mint(&self, lease_id: &str, bound: u128) -> Result<MintedKey, ProviderError> {
        let mut raw = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut raw);
        let name = format!("k-{}", hex::encode(raw));
        let expires_at = self.clock.now_secs() + KEY_LIFETIME_SECS;
        let cap = u64::try_from(key_cap_nano_usd(bound))
            .map_err(|_| ProviderError::Rejected("bound too large".into()))?;
        let path = format!("/v1/workspaces/{}/api-keys", self.cfg.workspace_id);
        let reply = self
            .with_session(|t| {
                self.http
                    .post(self.url(&path))
                    .bearer_auth(t)
                    .json(&json!({
                        "name": name,
                        "expires_at": format_rfc3339(expires_at),
                        "spendLimit": {"amount": cap, "currency": "USD"},
                    }))
                    .timeout(Duration::from_secs(15))
            })
            .await?;
        let body: Value = reply.json().await.map_err(transport)?;
        let (Some(id), Some(secret)) = (body["id"].as_str(), body["key"].as_str()) else {
            return Err(ProviderError::Unavailable("key reply is incomplete".into()));
        };
        self.keys().insert(
            lease_id.into(),
            KeyEntry {
                id: id.into(),
                secret: secret.into(),
                revoked: false,
            },
        );
        if let Err(e) = self.save_keys() {
            // A key whose secret is not on disk cannot be used after a restart: kill it now.
            self.keys().remove(lease_id);
            let _ = self.deactivate(id).await;
            return Err(e);
        }
        self.wait_until_active(secret).await;
        Ok(MintedKey {
            key_id: id.into(),
            expires_at,
        })
    }

    async fn housekeeping(&self) {
        self.check_login_expiry().await;
    }

    /// The key's cost from the organisation's usage history, filtered by key id. Works after the
    /// key is deleted. (The reporting endpoints `usage/export` and `usage/summary` answered 404
    /// on the live gateway on 2026-11-02, so the read-only reporting token is not used.)
    async fn usage(&self, lease_id: &str) -> Result<KeyUsage, ProviderError> {
        let entry = self.entry(lease_id)?;
        let path = format!("/v1/organizations/{}/usage/history", self.cfg.org_id);
        let mut cost: u128 = 0;
        let mut offset = 0usize;
        loop {
            let reply = self
                .with_session(|t| {
                    self.http
                        .get(self.url(&path))
                        .bearer_auth(t)
                        .query(&[
                            ("api_key_id", entry.id.clone()),
                            ("limit", PAGE.to_string()),
                            ("offset", offset.to_string()),
                        ])
                        .timeout(Duration::from_secs(30))
                })
                .await?;
            let body: Value = reply.json().await.map_err(transport)?;
            let rows = body["data"].as_array().cloned().unwrap_or_default();
            for row in &rows {
                cost += u128::from(row["total_cost"].as_u64().ok_or_else(|| {
                    ProviderError::Unavailable("usage row without a cost".into())
                })?);
            }
            offset += rows.len();
            if rows.is_empty() || offset as u64 >= body["total"].as_u64().unwrap_or(0) {
                return Ok(KeyUsage {
                    cost_nano_usd: cost,
                });
            }
        }
    }

    async fn revoke(&self, lease_id: &str) -> Result<(), ProviderError> {
        let id = match lease_id.strip_prefix("orphan:") {
            Some(id) => id.to_string(),
            None => self.entry(lease_id)?.id,
        };
        self.deactivate(&id).await?;
        // shortcut: revoked entries (an id each) stay in the table, so it grows with the number
        // of leases; prune them once settlement is known to be done.
        if let Some(entry) = self.keys().get_mut(lease_id) {
            entry.secret.clear();
            entry.revoked = true;
        }
        self.lease_chat.forget(lease_id);
        self.save_keys()
    }

    /// Lease ids of live keys. A live key with no record here (a crash between creating it and
    /// writing it down) comes back as `orphan:<provider id>`, which `revoke` understands.
    async fn list(&self) -> Result<Vec<String>, ProviderError> {
        let live = self.workspace_keys().await?;
        let by_id: HashMap<String, String> = self
            .keys()
            .iter()
            .map(|(lease, entry)| (entry.id.clone(), lease.clone()))
            .collect();
        Ok(live
            .into_iter()
            .map(|(id, _)| by_id.get(&id).cloned().unwrap_or(format!("orphan:{id}")))
            .collect())
    }

    /// The organisation's credit in micro-USD (`remaining`, nano-USD, floored).
    async fn balance(&self) -> Result<u128, ProviderError> {
        let path = format!("/v1/organizations/{}/usage/balance", self.cfg.org_id);
        let reply = self
            .with_session(|t| {
                self.http
                    .get(self.url(&path))
                    .bearer_auth(t)
                    .timeout(Duration::from_secs(15))
            })
            .await?;
        let body: Value = reply.json().await.map_err(transport)?;
        let nano = body["remaining"]
            .as_i64()
            .ok_or_else(|| ProviderError::Unavailable("balance has no remaining credit".into()))?;
        Ok(u128::try_from(nano).unwrap_or(0) / 1000)
    }

    async fn models(&self) -> Result<Option<Vec<LiveModel>>, ProviderError> {
        let mut out = Vec::new();
        let mut offset = 0usize;
        loop {
            let reply = checked(
                self.http
                    .get(self.url("/v1/model/list"))
                    .query(&[("limit", 1000usize), ("offset", offset)])
                    .timeout(Duration::from_secs(15))
                    .send()
                    .await
                    .map_err(transport)?,
            )
            .await?;
            let body: Value = reply.json().await.map_err(transport)?;
            let page = body["models"].as_array().cloned().unwrap_or_default();
            for m in &page {
                let (Some(id), Some(input), Some(output)) = (
                    m["modelId"].as_str(),
                    nano_per_token(&m["inputCostPerToken"]),
                    nano_per_token(&m["outputCostPerToken"]),
                ) else {
                    continue;
                };
                out.push(LiveModel {
                    id: id.into(),
                    provider_type: m["metadata"]["providerType"]
                        .as_str()
                        .unwrap_or_default()
                        .into(),
                    attestation_supported: m["metadata"]["attestationSupported"] == true,
                    input_nano_per_token: input,
                    output_nano_per_token: output,
                });
            }
            offset += page.len();
            if page.is_empty() || offset as u64 >= body["total"].as_u64().unwrap_or(0) {
                return Ok(Some(out));
            }
        }
    }

    async fn chat(
        &self,
        lease_id: &str,
        request: ChatRequest,
    ) -> Result<ChatResponse, ProviderError> {
        let entry = self.live_entry(lease_id)?;
        self.lease_chat.chat(&entry.secret, lease_id, request).await
    }

    /// For the lease's recent chats: the response signature of each, and one attestation report,
    /// all with the lease key, the chat's `signing_algo` and the caller's nonce.
    async fn evidence(
        &self,
        lease_id: &str,
        query: &EvidenceQuery,
    ) -> Result<Option<Value>, ProviderError> {
        let entry = self.live_entry(lease_id)?;
        self.lease_chat
            .evidence(&entry.secret, lease_id, query)
            .await
    }
}

/// Chat and evidence with a lease key. It holds no login: the key is handed in per call, so the
/// process that proxies chat needs only the lease keys, never the organisation's tokens.
#[derive(Clone)]
pub struct LeaseChat {
    http: reqwest::Client,
    base_url: String,
    chats: Arc<Mutex<HashMap<String, ChatLog>>>,
    /// When set, chat ids are also kept in the database, so the evidence fetch survives a restart.
    store: Option<crate::store::Store>,
}

impl LeaseChat {
    pub fn new(http: reqwest::Client, base_url: &str) -> Self {
        Self {
            http,
            base_url: base_url.trim_end_matches('/').into(),
            chats: Arc::new(Mutex::new(HashMap::new())),
            store: None,
        }
    }

    /// Keeps the chat ids of each lease in the database as well as in memory.
    pub fn with_store(mut self, store: crate::store::Store) -> Self {
        self.store = Some(store);
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Forgets the chat ids of a lease.
    pub fn forget(&self, lease_id: &str) {
        self.chats
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(lease_id);
    }

    pub async fn chat(
        &self,
        secret: &str,
        lease_id: &str,
        request: ChatRequest,
    ) -> Result<ChatResponse, ProviderError> {
        // A new key can answer the readiness check and still get 401 on the chat route for a few
        // seconds (seen on 6 Nov 2026: the first chat of a fresh lease failed, the next passed). The
        // request has not started upstream, so it is sent again, a few times.
        let mut attempt = 0;
        let reply = loop {
            let mut builder = self
                .http
                .post(self.url("/v1/chat/completions"))
                .bearer_auth(secret)
                // The signature covers the bytes as sent; no re-encoding on the way.
                .header("Accept-Encoding", "identity")
                .header("Content-Type", "application/json")
                .body(request.body.clone());
            for (name, value) in &request.headers {
                builder = builder.header(name, value);
            }
            match checked(builder.send().await.map_err(transport)?).await {
                Err(ProviderError::Rejected(reason))
                    if reason.starts_with("status 401") && attempt < NEW_KEY_CHAT_RETRIES =>
                {
                    attempt += 1;
                    tokio::time::sleep(NEW_KEY_CHAT_RETRY_WAIT).await;
                }
                other => break other?,
            }
        };
        let content_type = reply
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("application/json")
            .to_string();
        Ok(ChatResponse {
            content_type,
            body: Box::new(UpstreamBody {
                reply,
                lease_id: lease_id.into(),
                chats: self.chats.clone(),
                store: self.store.clone(),
                head: Vec::new(),
                noted: false,
            }),
        })
    }

    /// For the lease's recent chats: the response signature of each, and one attestation report,
    /// all with the lease key, the chat's `signing_algo` and the caller's nonce.
    pub async fn evidence(
        &self,
        secret: &str,
        lease_id: &str,
        query: &EvidenceQuery,
    ) -> Result<Option<Value>, ProviderError> {
        let mut chats: Vec<(String, String)> = {
            let log = self.chats.lock().unwrap_or_else(|e| e.into_inner());
            match log.get(lease_id) {
                Some(l) => l.0.iter().rev().take(EVIDENCE_CHATS).cloned().collect(),
                None => Vec::new(),
            }
        };
        // After a restart the memory is empty; the database still has the ids.
        if chats.is_empty() {
            if let Some(store) = &self.store {
                let lease = lease_id.to_string();
                chats = store
                    .run(move |c| crate::store::chats_of(c, &lease))
                    .await
                    .unwrap_or_default()
                    .into_iter()
                    .rev()
                    .take(EVIDENCE_CHATS)
                    .collect();
            }
        }
        if chats.is_empty() {
            return Ok(None);
        }
        let Some((_, model)) = chats.first().cloned() else {
            return Ok(None);
        };
        let get = |path: String, params: Vec<(&'static str, String)>| {
            self.http
                .get(self.url(&path))
                .bearer_auth(secret)
                .header("Accept-Encoding", "identity")
                .query(&params)
                .timeout(Duration::from_secs(20))
                .send()
        };
        let mut signatures = Vec::new();
        for (chat_id, chat_model) in chats {
            // A failed fetch (404: not stored or never made; 5xx; 429) must not cost the other
            // signatures and the attestation, because the key dies right after this call.
            let Ok(reply) = get(
                format!("/v1/signature/{}", encode_segment(&chat_id)),
                vec![
                    ("model", chat_model),
                    ("signing_algo", query.signing_algo.clone()),
                ],
            )
            .await
            else {
                continue;
            };
            let Ok(reply) = checked(reply).await else {
                continue;
            };
            let Ok(value) = reply.json::<Value>().await else {
                continue;
            };
            signatures.push(json!({"chat_id": chat_id, "signature": value}));
        }
        let attestation: Value = checked(
            get(
                "/v1/attestation/report".into(),
                vec![
                    ("model", model),
                    ("signing_algo", query.signing_algo.clone()),
                    ("nonce", query.nonce.clone()),
                ],
            )
            .await
            .map_err(transport)?,
        )
        .await?
        .json()
        .await
        .map_err(transport)?;
        Ok(Some(
            json!({"signatures": signatures, "attestation": attestation}),
        ))
    }
}

/// The upstream response. The first bytes are scanned once for the chat id and model, which the
/// evidence fetch needs later.
struct UpstreamBody {
    reply: reqwest::Response,
    lease_id: String,
    chats: Arc<Mutex<HashMap<String, ChatLog>>>,
    store: Option<crate::store::Store>,
    head: Vec<u8>,
    noted: bool,
}

impl UpstreamBody {
    /// Scans the first bytes; returns the chat's id and model the first time they are found.
    fn note(&mut self, chunk: &[u8]) -> Option<(String, String)> {
        if self.noted {
            return None;
        }
        self.head.extend_from_slice(chunk);
        // Non-streamed: one JSON object. Streamed: the first `data:` line is one.
        let text = String::from_utf8_lossy(&self.head);
        let line = text.lines().find_map(|l| {
            l.strip_prefix("data:")
                .map(str::trim)
                .or(Some(l))
                .filter(|l| l.starts_with('{'))
        });
        let parsed = line.and_then(|l| serde_json::from_str::<Value>(l).ok());
        if let Some(v) = parsed {
            self.noted = true;
            if let (Some(id), Some(model)) = (v["id"].as_str(), v["model"].as_str()) {
                self.chats
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .entry(self.lease_id.clone())
                    .or_default()
                    .0
                    .push((id.into(), model.into()));
                return Some((id.into(), model.into()));
            }
        } else if self.head.len() > 64 * 1024 {
            self.noted = true;
        }
        None
    }
}

#[async_trait]
impl ChatBody for UpstreamBody {
    async fn next_chunk(&mut self) -> Option<Result<Bytes, ProviderError>> {
        match self.reply.chunk().await {
            Ok(Some(chunk)) => {
                if let (Some((id, model)), Some(store)) = (self.note(&chunk), &self.store) {
                    let lease = self.lease_id.clone();
                    let _ = store
                        .run(move |c| crate::store::add_chat(c, &lease, &id, &model))
                        .await;
                }
                Some(Ok(chunk))
            }
            Ok(None) => None,
            Err(e) => Some(Err(transport(e))),
        }
    }
}

fn transport(e: reqwest::Error) -> ProviderError {
    ProviderError::Unavailable(e.without_url().to_string())
}

/// Maps a non-2xx answer to a [`ProviderError`]; the body is read only for a short reason.
async fn checked(reply: reqwest::Response) -> Result<reqwest::Response, ProviderError> {
    let status = reply.status();
    if status.is_success() {
        return Ok(reply);
    }
    let retry_after = reply
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(5);
    let text = reply.text().await.unwrap_or_default();
    let reason: String = text.chars().take(200).collect();
    Err(match status.as_u16() {
        404 => ProviderError::NotFound(reason),
        429 => ProviderError::RateLimited {
            retry_after_secs: retry_after,
        },
        402 => ProviderError::Rejected("insufficient credit or spend limit".into()),
        408 | 500..=599 => ProviderError::Unavailable(format!("status {}", status.as_u16())),
        code => ProviderError::Rejected(format!("status {code}: {reason}")),
    })
}

/// Nano-USD per token from a `{amount, scale}` price; none when it is not a whole number of
/// nano-USD.
fn nano_per_token(price: &Value) -> Option<u64> {
    let amount = price["amount"].as_u64()?;
    let scale = price["scale"].as_u64()?;
    if scale <= 9 {
        amount.checked_mul(10u64.checked_pow(u32::try_from(9 - scale).ok()?)?)
    } else {
        let div = 10u64.checked_pow(u32::try_from(scale - 9).ok()?)?;
        (amount % div == 0).then_some(amount / div)
    }
}

fn jwt_exp(token: &str) -> Option<u64> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload.trim_end_matches('='))
        .ok()?;
    serde_json::from_slice::<Value>(&bytes).ok()?["exp"].as_u64()
}

/// Path segment with everything but unreserved characters escaped.
fn encode_segment(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// `YYYY-MM-DDTHH:MM:SSZ` for Unix seconds.
pub fn format_rfc3339(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// Unix seconds from an RFC 3339 time with `Z` or a numeric offset; fractions are dropped.
pub fn parse_rfc3339(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() < 20 || b[4] != b'-' || b[7] != b'-' || !matches!(b[10], b'T' | b't' | b' ') {
        return None;
    }
    let num = |a: usize, z: usize| s.get(a..z)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut rest = &s[19..];
    if rest.starts_with('.') {
        rest = rest[1..].trim_start_matches(|c: char| c.is_ascii_digit());
    }
    let offset = match rest {
        "Z" | "z" => 0,
        _ if rest.len() == 6 && (rest.starts_with('+') || rest.starts_with('-')) => {
            let sign = if rest.starts_with('-') { -1 } else { 1 };
            sign * (rest.get(1..3)?.parse::<i64>().ok()? * 3600
                + rest.get(4..6)?.parse::<i64>().ok()? * 60)
        }
        _ => return None,
    };
    // Days from civil (Howard Hinnant).
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2.rem_euclid(400);
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + sec - offset).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_round_trips_and_reads_offsets_and_fractions() {
        for secs in [0u64, 951_782_400, 1_793_577_600, 1_793_664_000 + 3661] {
            assert_eq!(parse_rfc3339(&format_rfc3339(secs)), Some(secs));
        }
        assert_eq!(parse_rfc3339("2026-11-02T00:00:00Z"), Some(1_793_577_600));
        assert_eq!(
            parse_rfc3339("2026-11-02T02:00:00.123456+02:00"),
            Some(1_793_577_600)
        );
        assert_eq!(
            parse_rfc3339("2026-11-01T22:00:00-02:00"),
            Some(1_793_577_600)
        );
        assert_eq!(parse_rfc3339("2026-13-02T00:00:00Z"), None);
        assert_eq!(parse_rfc3339("tomorrow"), None);
    }

    #[test]
    fn prices_become_whole_nano_usd_or_nothing() {
        let p = |amount: u64, scale: u64| json!({"amount": amount, "scale": scale});
        assert_eq!(nano_per_token(&p(80, 9)), Some(80));
        assert_eq!(nano_per_token(&p(8, 8)), Some(80));
        assert_eq!(nano_per_token(&p(800, 10)), Some(80));
        assert_eq!(nano_per_token(&p(805, 10)), None);
        assert_eq!(nano_per_token(&json!({"amount": 1})), None);
    }

    #[test]
    fn the_jwt_expiry_is_read_from_the_payload() {
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"exp":1234}"#);
        assert_eq!(jwt_exp(&format!("h.{payload}.s")), Some(1234));
        assert_eq!(jwt_exp("fake-session-jwt"), None);
    }

    #[test]
    fn a_chat_id_is_escaped_into_one_path_segment() {
        assert_eq!(encode_segment("chatcmpl-1"), "chatcmpl-1");
        assert_eq!(encode_segment("../a?b"), "..%2Fa%3Fb");
    }
}
