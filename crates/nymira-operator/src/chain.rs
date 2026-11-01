//! Reads of the vault: `config()`, the root, the retirement epoch, note counts and pending exits.
//!
//! [`RpcVault`] speaks NEAR JSON-RPC `query/call_function` (a sandbox, the fake RPC or a public
//! node); [`FakeVault`] is the scripted double for tests that need to move time or break reads.

use std::sync::Mutex;

use async_trait::async_trait;
use base64::Engine;
use serde::{Deserialize, Deserializer};
use serde_json::{json, Value};

#[derive(Debug, thiserror::Error, Clone)]
#[error("vault read failed: {0}")]
pub struct ChainError(pub String);

fn dec<'de, D: Deserializer<'de>>(d: D) -> Result<u128, D::Error> {
    match Value::deserialize(d)? {
        Value::String(s) => s.parse().map_err(serde::de::Error::custom),
        Value::Number(n) => n
            .as_u64()
            .map(u128::from)
            .ok_or_else(|| serde::de::Error::custom("not an unsigned integer")),
        _ => Err(serde::de::Error::custom("expected a decimal string")),
    }
}

fn dec_vec<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u128>, D::Error> {
    Vec::<Value>::deserialize(d)?
        .into_iter()
        .map(|v| match v {
            Value::String(s) => s.parse().map_err(serde::de::Error::custom),
            Value::Number(n) => n
                .as_u64()
                .map(u128::from)
                .ok_or_else(|| serde::de::Error::custom("not an unsigned integer")),
            _ => Err(serde::de::Error::custom("expected a decimal string")),
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct Key {
    pub x: String,
    pub y: String,
}

/// The fields of the vault's `config()` the operator uses; the rest is ignored.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct VaultConfig {
    pub operator_pk: Key,
    pub clearance_pk: Key,
    #[serde(deserialize_with = "dec")]
    pub chain_id: u128,
    #[serde(deserialize_with = "dec")]
    pub note_fee: u128,
    #[serde(deserialize_with = "dec")]
    pub close_fee: u128,
    #[serde(deserialize_with = "dec_vec")]
    pub bound_classes: Vec<u128>,
    #[serde(deserialize_with = "dec")]
    pub charge_step: u128,
    #[serde(deserialize_with = "dec")]
    pub min_charge: u128,
    pub slot_rate: u32,
}

/// A pending exit: the nullifier it spent and the successor it published.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ExitEntry {
    pub note_id: u32,
    pub nullifier: String,
    pub next_nullifier: String,
}

#[async_trait]
pub trait VaultView: Send + Sync {
    async fn config(&self) -> Result<VaultConfig, ChainError>;
    async fn current_root(&self) -> Result<String, ChainError>;
    async fn retirement_epoch(&self) -> Result<u64, ChainError>;
    async fn open_notes(&self) -> Result<u32, ChainError>;
    /// Every pending exit, paged by note id inside the implementation.
    async fn exits(&self) -> Result<Vec<ExitEntry>, ChainError>;
    /// How many of the given notes are `Active`.
    async fn open_among(&self, note_ids: &[u32]) -> Result<u32, ChainError>;
}

pub struct RpcVault {
    http: reqwest::Client,
    rpc_url: String,
    account: String,
    finality: &'static str,
}

const PAGE: u32 = 100;

impl RpcVault {
    pub fn new(rpc_url: impl Into<String>, account: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .expect("http client"),
            rpc_url: rpc_url.into(),
            account: account.into(),
            finality: "final",
        }
    }

    async fn view<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        args: Value,
    ) -> Result<T, ChainError> {
        let body = json!({
            "jsonrpc": "2.0", "id": "nymira", "method": "query",
            "params": {
                "request_type": "call_function", "finality": self.finality,
                "account_id": self.account, "method_name": method,
                "args_base64": base64::engine::general_purpose::STANDARD.encode(args.to_string()),
            }
        });
        let reply: Value = self
            .http
            .post(&self.rpc_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| ChainError(e.to_string()))?
            .json()
            .await
            .map_err(|e| ChainError(e.to_string()))?;
        if let Some(error) = reply.get("error") {
            return Err(ChainError(error.to_string()));
        }
        let bytes: Vec<u8> = reply["result"]["result"]
            .as_array()
            .ok_or_else(|| ChainError(format!("no result for {method}: {reply}")))?
            .iter()
            .map(|b| b.as_u64().unwrap_or(0) as u8)
            .collect();
        serde_json::from_slice(&bytes).map_err(|e| ChainError(format!("{method}: {e}")))
    }
}

#[async_trait]
impl VaultView for RpcVault {
    async fn config(&self) -> Result<VaultConfig, ChainError> {
        self.view("config", json!({})).await
    }

    async fn current_root(&self) -> Result<String, ChainError> {
        self.view("current_root", json!({})).await
    }

    async fn retirement_epoch(&self) -> Result<u64, ChainError> {
        let epoch: String = self.view("retirement_epoch", json!({})).await?;
        epoch.parse().map_err(|_| ChainError("bad epoch".into()))
    }

    async fn open_notes(&self) -> Result<u32, ChainError> {
        self.view("open_notes", json!({})).await
    }

    async fn exits(&self) -> Result<Vec<ExitEntry>, ChainError> {
        let mut all = Vec::new();
        let mut from = 0u32;
        loop {
            let page: Vec<ExitEntry> = self
                .view(
                    "exit_requests",
                    json!({ "from_note_id": from, "limit": PAGE }),
                )
                .await?;
            let full = page.len() as u32 >= PAGE;
            if let Some(last) = page.last() {
                from = last.note_id + 1;
            }
            all.extend(page);
            if !full {
                return Ok(all);
            }
        }
    }

    async fn open_among(&self, note_ids: &[u32]) -> Result<u32, ChainError> {
        let mut open = 0;
        for id in note_ids {
            let records: Vec<Value> = self
                .view("note_records", json!({ "from": id, "limit": 1 }))
                .await?;
            if records.first().is_some_and(|r| r["status"] == "Active") {
                open += 1;
            }
        }
        Ok(open)
    }
}

#[derive(Clone)]
pub struct FakeVaultState {
    pub config: VaultConfig,
    pub root: String,
    pub epoch: u64,
    pub open_notes: u32,
    pub open_team_notes: u32,
    pub exits: Vec<ExitEntry>,
    pub failing: bool,
}

/// A scripted vault.
pub struct FakeVault(Mutex<FakeVaultState>);

impl FakeVault {
    pub fn new(config: VaultConfig, root: &str) -> Self {
        Self(Mutex::new(FakeVaultState {
            config,
            root: root.into(),
            epoch: 0,
            open_notes: 0,
            open_team_notes: 0,
            exits: Vec::new(),
            failing: false,
        }))
    }

    pub fn edit(&self, f: impl FnOnce(&mut FakeVaultState)) {
        f(&mut self.0.lock().unwrap_or_else(|e| e.into_inner()));
    }

    fn read<T>(&self, f: impl FnOnce(&FakeVaultState) -> T) -> Result<T, ChainError> {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.failing {
            return Err(ChainError("vault unreachable".into()));
        }
        Ok(f(&state))
    }
}

#[async_trait]
impl VaultView for FakeVault {
    async fn config(&self) -> Result<VaultConfig, ChainError> {
        self.read(|s| s.config.clone())
    }

    async fn current_root(&self) -> Result<String, ChainError> {
        self.read(|s| s.root.clone())
    }

    async fn retirement_epoch(&self) -> Result<u64, ChainError> {
        self.read(|s| s.epoch)
    }

    async fn open_notes(&self) -> Result<u32, ChainError> {
        self.read(|s| s.open_notes)
    }

    async fn exits(&self) -> Result<Vec<ExitEntry>, ChainError> {
        self.read(|s| s.exits.clone())
    }

    async fn open_among(&self, _note_ids: &[u32]) -> Result<u32, ChainError> {
        self.read(|s| s.open_team_notes)
    }
}
