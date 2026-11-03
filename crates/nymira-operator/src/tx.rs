//! Transactions the operator sends from its own account (the heartbeat and the operator-sent
//! close): a function call signed with a function-call key.
//!
//! The order is fixed. The intent is written first; then the nonce and block hash are read and
//! the signed bytes, nonce and hash are written; only then is the transaction broadcast with
//! `wait_until: FINAL`. After a timeout the hash is queried and, if the node never saw it, the
//! same bytes go out again, unchanged. The receipts of the final result are read: a failed
//! receipt is a failed transaction.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use ed25519_dalek::{Signer as _, SigningKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::clock::Clock;
use crate::store::{self, Store, TxRow};

/// A repeat of the same request inside this window gets the first transaction back.
pub const IDEM_WINDOW_SECS: u64 = 600;

#[derive(Debug, thiserror::Error)]
pub enum TxError {
    #[error("rpc: {0}")]
    Rpc(String),
    #[error("store: {0}")]
    Store(String),
}

fn store_error(e: rusqlite::Error) -> TxError {
    TxError::Store(e.to_string())
}

#[derive(Clone, Copy, Debug)]
pub struct TxConfig {
    /// Longest wait for one `send_tx` answer.
    pub send_timeout: Duration,
    /// Pause before the same bytes are sent again.
    pub retry_delay: Duration,
    /// Sends of the same bytes before the driver gives up (the row stays `signed`, and `resume`
    /// picks it up again).
    pub max_attempts: u32,
}

impl Default for TxConfig {
    fn default() -> Self {
        Self {
            send_timeout: Duration::from_secs(30),
            retry_delay: Duration::from_secs(3),
            max_attempts: 10,
        }
    }
}

pub struct TxRequest {
    /// What the row is counted and deduplicated under (`close`, `heartbeat`).
    pub kind: &'static str,
    pub method: &'static str,
    pub args: Vec<u8>,
    pub gas: u64,
    /// A repeat with the same key inside [`IDEM_WINDOW_SECS`] sends nothing new.
    pub idem_key: Option<String>,
}

/// How a transaction ended once the chain finalised it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxEnd {
    pub hash: String,
    pub success: bool,
    /// The failure the receipts carry, empty on success.
    pub detail: String,
}

pub struct NearTx {
    http: reqwest::Client,
    rpc_url: String,
    signer_id: String,
    receiver_id: String,
    key: SigningKey,
    store: Store,
    clock: Arc<dyn Clock>,
    cfg: TxConfig,
    /// yoctoNEAR attached to every call this sender makes.
    deposit: u128,
    /// Serialises nonce allocation.
    signing: tokio::sync::Mutex<()>,
}

enum Rpc {
    /// No answer, or one that says nothing about the transaction: try again.
    Transient(String),
    /// The node refused the transaction itself.
    Invalid(String),
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    out.extend((s.len() as u32).to_le_bytes());
    out.extend(s.as_bytes());
}

/// The Borsh bytes of a transaction holding one function call with no attached deposit.
#[allow(clippy::too_many_arguments)]
pub fn transaction_bytes(
    signer_id: &str,
    public_key: &[u8; 32],
    nonce: u64,
    receiver_id: &str,
    block_hash: &[u8; 32],
    method: &str,
    args: &[u8],
    gas: u64,
) -> Vec<u8> {
    transaction_bytes_with_deposit(
        signer_id,
        public_key,
        nonce,
        receiver_id,
        block_hash,
        method,
        args,
        gas,
        0,
    )
}

/// The same with `deposit` yoctoNEAR attached (`ft_transfer_call` wants exactly one).
#[allow(clippy::too_many_arguments)]
pub fn transaction_bytes_with_deposit(
    signer_id: &str,
    public_key: &[u8; 32],
    nonce: u64,
    receiver_id: &str,
    block_hash: &[u8; 32],
    method: &str,
    args: &[u8],
    gas: u64,
    deposit: u128,
) -> Vec<u8> {
    let mut out = Vec::new();
    put_str(&mut out, signer_id);
    out.push(0); // ed25519
    out.extend(public_key);
    out.extend(nonce.to_le_bytes());
    put_str(&mut out, receiver_id);
    out.extend(block_hash);
    out.extend(1u32.to_le_bytes()); // one action
    out.push(2); // FunctionCall
    put_str(&mut out, method);
    out.extend((args.len() as u32).to_le_bytes());
    out.extend(args);
    out.extend(gas.to_le_bytes());
    out.extend(deposit.to_le_bytes());
    out
}

/// The signed transaction: the bytes, then the signature (type byte 0 and 64 bytes) over their
/// SHA-256. Returns it with the transaction hash in base58.
pub fn sign_transaction(key: &SigningKey, tx: &[u8]) -> (Vec<u8>, String) {
    let digest = Sha256::digest(tx);
    let signature = key.sign(&digest);
    let mut signed = tx.to_vec();
    signed.push(0);
    signed.extend(signature.to_bytes());
    (signed, bs58::encode(digest).into_string())
}

impl NearTx {
    pub fn new(
        rpc_url: impl Into<String>,
        signer_id: impl Into<String>,
        receiver_id: impl Into<String>,
        key: SigningKey,
        store: Store,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            http: reqwest::Client::new(),
            rpc_url: rpc_url.into(),
            signer_id: signer_id.into(),
            receiver_id: receiver_id.into(),
            key,
            store,
            clock,
            cfg: TxConfig::default(),
            deposit: 0,
            signing: tokio::sync::Mutex::new(()),
        }
    }

    pub fn with_config(mut self, cfg: TxConfig) -> Self {
        self.cfg = cfg;
        self
    }

    /// Sends through `http` instead of the default client (a client behind a SOCKS proxy).
    pub fn with_http(mut self, http: reqwest::Client) -> Self {
        self.http = http;
        self
    }

    /// Attaches `yocto` to every call (a sender for `ft_transfer_call` uses 1).
    pub fn with_deposit(mut self, yocto: u128) -> Self {
        self.deposit = yocto;
        self
    }

    /// The account the transactions are sent from.
    pub fn signer_id(&self) -> &str {
        &self.signer_id
    }

    /// The signer account's balance in yoctoNEAR at a final block.
    pub async fn balance(&self) -> Result<u128, TxError> {
        let account = self
            .rpc(
                "query",
                json!({
                    "request_type": "view_account", "finality": "final",
                    "account_id": self.signer_id,
                }),
                self.cfg.send_timeout,
            )
            .await
            .map_err(Self::rpc_error)?;
        account["amount"]
            .as_str()
            .and_then(|a| a.parse().ok())
            .ok_or_else(|| TxError::Rpc("the account balance is not readable".into()))
    }

    /// `ed25519:` and the base58 key, as NEAR prints it.
    pub fn public_key(&self) -> String {
        format!(
            "ed25519:{}",
            bs58::encode(self.key.verifying_key().as_bytes()).into_string()
        )
    }

    async fn rpc(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, Rpc> {
        let body = json!({ "jsonrpc": "2.0", "id": "nymira", "method": method, "params": params });
        let reply: Value = self
            .http
            .post(&self.rpc_url)
            .timeout(timeout)
            .json(&body)
            .send()
            .await
            .map_err(|e| Rpc::Transient(e.to_string()))?
            .json()
            .await
            .map_err(|e| Rpc::Transient(e.to_string()))?;
        if let Some(error) = reply.get("error") {
            let cause = error["cause"]["name"].as_str().unwrap_or("");
            return Err(if cause == "INVALID_TRANSACTION" {
                Rpc::Invalid(error.to_string())
            } else {
                Rpc::Transient(error.to_string())
            });
        }
        reply
            .get("result")
            .cloned()
            .ok_or_else(|| Rpc::Transient("no result".into()))
    }

    fn rpc_error(e: Rpc) -> TxError {
        match e {
            Rpc::Transient(m) | Rpc::Invalid(m) => TxError::Rpc(m),
        }
    }

    /// Writes the intent, then signs it and writes the signed bytes. A repeat of an idempotency
    /// key inside the window returns the first row and writes nothing.
    pub async fn submit(&self, req: TxRequest) -> Result<TxRow, TxError> {
        let _serial = self.signing.lock().await;
        let now = self.clock.now_secs();
        let public_key = self.public_key();
        let id = {
            let (key, kind, method, args, gas, pk) = (
                req.idem_key.clone(),
                req.kind,
                req.method,
                req.args,
                req.gas,
                public_key,
            );
            self.store
                .run(move |c| {
                    if let Some(key) = &key {
                        let since = now.saturating_sub(IDEM_WINDOW_SECS);
                        if let Some(existing) = store::tx_by_idem_key(c, kind, key, since)? {
                            return Ok(existing.id);
                        }
                    }
                    store::insert_tx_intent(c, kind, method, &args, gas, &pk, key.as_deref(), now)
                })
                .await
                .map_err(store_error)?
        };
        self.ensure_signed(id).await?;
        self.row(id).await
    }

    async fn row(&self, id: i64) -> Result<TxRow, TxError> {
        self.store
            .run(move |c| store::tx(c, id))
            .await
            .map_err(store_error)?
            .ok_or_else(|| TxError::Store("transaction row is missing".into()))
    }

    /// Signs an intent that has no signed bytes yet.
    async fn ensure_signed(&self, id: i64) -> Result<(), TxError> {
        let row = self.row(id).await?;
        if row.state != "intent" {
            return Ok(());
        }
        let timeout = self.cfg.send_timeout;
        let key = self
            .rpc(
                "query",
                json!({
                    "request_type": "view_access_key", "finality": "final",
                    "account_id": self.signer_id, "public_key": self.public_key(),
                }),
                timeout,
            )
            .await
            .map_err(Self::rpc_error)?;
        // A node answers a key it cannot see yet (not in a final block) inside a normal result.
        if let Some(why) = key.get("error") {
            return Err(TxError::Rpc(format!(
                "the access key is not readable: {why}"
            )));
        }
        let on_chain = key["nonce"].as_u64().unwrap_or(0);
        let pk = self.public_key();
        let local = self
            .store
            .run(move |c| store::max_tx_nonce(c, &pk))
            .await
            .map_err(store_error)?;
        let nonce = on_chain.max(local) + 1;
        let block = self
            .rpc("block", json!({ "finality": "final" }), timeout)
            .await
            .map_err(Self::rpc_error)?;
        let block_hash: [u8; 32] = block["header"]["hash"]
            .as_str()
            .and_then(|h| bs58::decode(h).into_vec().ok())
            .and_then(|b| b.try_into().ok())
            .ok_or_else(|| TxError::Rpc("the block hash is not readable".into()))?;
        let tx = transaction_bytes_with_deposit(
            &self.signer_id,
            self.key.verifying_key().as_bytes(),
            nonce,
            &self.receiver_id,
            &block_hash,
            &row.method,
            &row.args,
            row.gas,
            self.deposit,
        );
        let (signed, hash) = sign_transaction(&self.key, &tx);
        self.store
            .run(move |c| store::sign_tx(c, id, nonce, &signed, &hash))
            .await
            .map_err(store_error)
    }

    /// Broadcasts a signed row and waits for it to be final, resending the same bytes when the
    /// node never saw them.
    pub async fn drive(&self, id: i64) -> Result<TxEnd, TxError> {
        self.ensure_signed(id).await?;
        let row = self.row(id).await?;
        let hash = row.hash.clone().unwrap_or_default();
        let end = |success, detail: &str| TxEnd {
            hash: hash.clone(),
            success,
            detail: detail.into(),
        };
        match row.state.as_str() {
            "final" => return Ok(end(true, "")),
            "failed" | "rejected" => return Ok(end(false, row.detail.as_deref().unwrap_or(""))),
            _ => {}
        }
        let signed = row.signed_bytes.clone().unwrap_or_default();
        let b64 = base64::engine::general_purpose::STANDARD.encode(&signed);
        let mut last = String::from("not sent");
        for attempt in 0..self.cfg.max_attempts {
            if attempt > 0 {
                tokio::time::sleep(self.cfg.retry_delay).await;
            }
            let sent = self
                .rpc(
                    "send_tx",
                    json!({ "signed_tx_base64": b64, "wait_until": "FINAL" }),
                    self.cfg.send_timeout,
                )
                .await;
            let invalid = match sent {
                Ok(result) => {
                    if let Some(done) = self.settle(id, &hash, &result).await? {
                        return Ok(done);
                    }
                    String::new()
                }
                Err(Rpc::Invalid(m)) => m,
                Err(Rpc::Transient(m)) => {
                    last = m;
                    String::new()
                }
            };
            // The answer may have been lost, or a resend may have raced the first copy: ask the
            // node about the hash before judging.
            let asked = self
                .rpc(
                    "tx",
                    json!({
                        "tx_hash": hash, "sender_account_id": self.signer_id,
                        "wait_until": "FINAL",
                    }),
                    self.cfg.send_timeout,
                )
                .await;
            if let Ok(result) = asked {
                if let Some(done) = self.settle(id, &hash, &result).await? {
                    return Ok(done);
                }
            }
            if !invalid.is_empty() {
                let detail = invalid;
                self.finish(id, "rejected", &detail).await?;
                return Ok(end(false, &detail));
            }
        }
        Err(TxError::Rpc(format!(
            "no final result after {} sends: {last}",
            self.cfg.max_attempts
        )))
    }

    /// Reads a `send_tx` or `tx` result. `None` when it is not final yet.
    async fn settle(&self, id: i64, hash: &str, result: &Value) -> Result<Option<TxEnd>, TxError> {
        if result["final_execution_status"] != "FINAL" {
            return Ok(None);
        }
        let failure = failure_in(result);
        let (state, detail) = match &failure {
            Some(why) => ("failed", why.as_str()),
            None => ("final", ""),
        };
        self.finish(id, state, detail).await?;
        Ok(Some(TxEnd {
            hash: hash.into(),
            success: failure.is_none(),
            detail: detail.into(),
        }))
    }

    async fn finish(&self, id: i64, state: &'static str, detail: &str) -> Result<(), TxError> {
        let detail = detail.to_string();
        self.store
            .run(move |c| store::finish_tx(c, id, state, &detail))
            .await
            .map_err(store_error)
    }

    /// Submits and drives to the end.
    pub async fn send(&self, req: TxRequest) -> Result<TxEnd, TxError> {
        let row = self.submit(req).await?;
        self.drive(row.id).await
    }

    /// Drives every transaction of this key that start-up found unfinished.
    pub async fn resume(&self) {
        let pk = self.public_key();
        let open = self
            .store
            .run(move |c| store::open_txs(c, &pk))
            .await
            .unwrap_or_default();
        for row in open {
            let _ = self.drive(row.id).await;
        }
    }
}

/// The first failure in a final result: the transaction's own status or any receipt's.
fn failure_in(result: &Value) -> Option<String> {
    if let Some(failure) = result["status"].get("Failure") {
        return Some(failure.to_string());
    }
    result["receipts_outcome"]
        .as_array()?
        .iter()
        .find_map(|r| r["outcome"]["status"].get("Failure").map(|f| f.to_string()))
}

/// The heartbeat as a real transaction from the operator account.
pub struct NearHeartbeat(pub Arc<NearTx>);

/// Gas attached to `heartbeat`: it emits one event.
pub const HEARTBEAT_GAS: u64 = 10_000_000_000_000;

#[async_trait::async_trait]
impl crate::signer::HeartbeatSender for NearHeartbeat {
    async fn send(&self) -> Result<(), String> {
        let end = self
            .0
            .send(TxRequest {
                kind: "heartbeat",
                method: "heartbeat",
                args: b"{}".to_vec(),
                gas: HEARTBEAT_GAS,
                idem_key: None,
            })
            .await
            .map_err(|e| e.to_string())?;
        if end.success {
            Ok(())
        } else {
            Err(end.detail)
        }
    }
}
