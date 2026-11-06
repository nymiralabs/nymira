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
    /// Seconds an exit stays open; a note is claimable this long after its expiry.
    #[serde(default)]
    pub exit_delay_secs: u64,
}

/// A note as the vault records it: its status word (`Active`, `PendingExit` or `Closed`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NoteInfo {
    pub status: String,
}

/// A pending exit: the nullifier it spent, the successor it published, what it pays and when it
/// can be finalized.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct ExitEntry {
    pub note_id: u32,
    pub nullifier: String,
    pub next_nullifier: String,
    #[serde(default, deserialize_with = "dec")]
    pub final_balance: u128,
    #[serde(default, deserialize_with = "dec")]
    pub conceded: u128,
    /// Block time in nanoseconds.
    #[serde(default, deserialize_with = "dec")]
    pub deadline: u128,
}

/// A note as `note_records` lists it.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct NoteRecord {
    pub note_id: u32,
    #[serde(deserialize_with = "dec")]
    pub deposit_amount: u128,
    pub expiry: u64,
    pub status: String,
    pub leaf_retired: bool,
}

#[async_trait]
pub trait VaultView: Send + Sync {
    async fn config(&self) -> Result<VaultConfig, ChainError>;
    async fn current_root(&self) -> Result<String, ChainError>;
    async fn retirement_epoch(&self) -> Result<u64, ChainError>;
    async fn open_notes(&self) -> Result<u32, ChainError>;
    /// NEAR the vault can spend: its balance minus the storage it must keep covered.
    async fn liquid_near(&self) -> Result<u128, ChainError>;
    /// Every pending exit, paged by note id inside the implementation.
    async fn exits(&self) -> Result<Vec<ExitEntry>, ChainError>;
    /// How many of the given notes are `Active`.
    async fn open_among(&self, note_ids: &[u32]) -> Result<u32, ChainError>;
    /// Notes `from..from + limit` (at most 100), in id order.
    async fn note_records(&self, from: u32, limit: u32) -> Result<Vec<NoteRecord>, ChainError>;
    /// Leaves in the tree.
    async fn leaf_count(&self) -> Result<u32, ChainError>;
    /// Leaves `from..from + limit` (at most 100) as the tree holds them, retired ones as zero.
    async fn leaves(&self, from: u32, limit: u32) -> Result<Vec<String>, ChainError>;
    /// The note's record, `None` for an id the vault never issued.
    async fn note(&self, note_id: u32) -> Result<Option<NoteInfo>, ChainError>;
    /// Whether the vault has spent the nullifier (`0x` and 64 hex digits).
    async fn nullifier_used(&self, nullifier: &str) -> Result<bool, ChainError>;
    /// The block this provider answered at and the leaf count it showed there.
    async fn head(&self) -> Result<Head, ChainError>;
}

/// What a provider showed at one block, for comparing providers with each other.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Head {
    pub height: u64,
    pub leaf_count: u32,
}

pub struct RpcVault {
    http: reqwest::Client,
    rpc_url: String,
    /// Tried in order when `rpc_url` cannot be reached or says it is rate limited.
    fallbacks: Vec<String>,
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
            fallbacks: Vec::new(),
            account: account.into(),
            finality: "final",
        }
    }

    /// Other providers for the same chain. The free public ones answer 429 to a burst of reads, so
    /// a lease that needs several reads in a row must not depend on one of them.
    pub fn with_fallbacks(mut self, urls: impl IntoIterator<Item = String>) -> Self {
        self.fallbacks = urls.into_iter().collect();
        self
    }

    /// One JSON-RPC round trip; a transport failure or a rate limit (code -429) is the signal to
    /// try the next provider, any other answer is final.
    async fn round_trip(&self, url: &str, body: &Value) -> Result<Value, ChainError> {
        let reply: Value = self
            .http
            .post(url)
            .json(body)
            .send()
            .await
            .map_err(|e| ChainError(e.to_string()))?
            .json()
            .await
            .map_err(|e| ChainError(e.to_string()))?;
        if reply["error"]["code"].as_i64() == Some(-429) {
            return Err(ChainError(reply["error"].to_string()));
        }
        Ok(reply)
    }

    async fn view<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        args: Value,
    ) -> Result<T, ChainError> {
        Ok(self.view_at(method, args).await?.0)
    }

    /// The decoded result and the height of the block it was read at.
    async fn view_at<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        args: Value,
    ) -> Result<(T, u64), ChainError> {
        let body = json!({
            "jsonrpc": "2.0", "id": "nymira", "method": "query",
            "params": {
                "request_type": "call_function", "finality": self.finality,
                "account_id": self.account, "method_name": method,
                "args_base64": base64::engine::general_purpose::STANDARD.encode(args.to_string()),
            }
        });
        let mut last = ChainError("no provider".into());
        let mut answer = None;
        for url in std::iter::once(&self.rpc_url).chain(self.fallbacks.iter()) {
            match self.round_trip(url, &body).await {
                Ok(reply) => {
                    answer = Some(reply);
                    break;
                }
                Err(e) => last = e,
            }
        }
        let reply = answer.ok_or(last)?;
        if let Some(error) = reply.get("error") {
            return Err(ChainError(error.to_string()));
        }
        let bytes: Vec<u8> = reply["result"]["result"]
            .as_array()
            .ok_or_else(|| ChainError(format!("no result for {method}: {reply}")))?
            .iter()
            .map(|b| b.as_u64().unwrap_or(0) as u8)
            .collect();
        let height = reply["result"]["block_height"].as_u64().unwrap_or(0);
        let value =
            serde_json::from_slice(&bytes).map_err(|e| ChainError(format!("{method}: {e}")))?;
        Ok((value, height))
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

    async fn liquid_near(&self) -> Result<u128, ChainError> {
        let yocto: String = self.view("liquid_near", json!({})).await?;
        yocto
            .parse()
            .map_err(|_| ChainError(format!("liquid_near: {yocto}")))
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

    async fn note_records(&self, from: u32, limit: u32) -> Result<Vec<NoteRecord>, ChainError> {
        self.view("note_records", json!({ "from": from, "limit": limit }))
            .await
    }

    async fn leaf_count(&self) -> Result<u32, ChainError> {
        self.view("leaf_count", json!({})).await
    }

    async fn leaves(&self, from: u32, limit: u32) -> Result<Vec<String>, ChainError> {
        self.view("get_leaves", json!({ "from": from, "limit": limit }))
            .await
    }

    async fn note(&self, note_id: u32) -> Result<Option<NoteInfo>, ChainError> {
        let records: Vec<Value> = self
            .view("note_records", json!({ "from": note_id, "limit": 1 }))
            .await?;
        Ok(records
            .into_iter()
            .find(|r| r["note_id"] == note_id)
            .and_then(|r| r["status"].as_str().map(String::from))
            .map(|status| NoteInfo { status }))
    }

    async fn nullifier_used(&self, nullifier: &str) -> Result<bool, ChainError> {
        self.view("is_nullifier_used", json!({ "nullifier": nullifier }))
            .await
    }

    async fn head(&self) -> Result<Head, ChainError> {
        let (leaf_count, height) = self.view_at("leaf_count", json!({})).await?;
        Ok(Head { height, leaf_count })
    }
}

#[derive(Clone)]
pub struct FakeVaultState {
    pub config: VaultConfig,
    pub root: String,
    pub epoch: u64,
    pub open_notes: u32,
    /// What `liquid_near` reports, in yoctoNEAR.
    pub liquid_near: u128,
    pub open_team_notes: u32,
    pub exits: Vec<ExitEntry>,
    /// Note id -> status word. A note not listed does not exist.
    pub notes: std::collections::HashMap<u32, String>,
    /// What `note_records` lists, by position (note id = index).
    pub records: Vec<NoteRecord>,
    /// What `get_leaves` lists.
    pub leaves: Vec<String>,
    pub leaf_count: u32,
    /// The block height `head` reports.
    pub height: u64,
    /// Spent nullifiers, lowercase.
    pub used: std::collections::HashSet<String>,
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
            liquid_near: 10_000_000_000_000_000_000_000_000,
            open_team_notes: 0,
            exits: Vec::new(),
            notes: Default::default(),
            records: Vec::new(),
            leaves: Vec::new(),
            leaf_count: 0,
            height: 0,
            used: Default::default(),
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

    async fn liquid_near(&self) -> Result<u128, ChainError> {
        self.read(|s| s.liquid_near)
    }

    async fn exits(&self) -> Result<Vec<ExitEntry>, ChainError> {
        self.read(|s| s.exits.clone())
    }

    async fn open_among(&self, _note_ids: &[u32]) -> Result<u32, ChainError> {
        self.read(|s| s.open_team_notes)
    }

    async fn note_records(&self, from: u32, limit: u32) -> Result<Vec<NoteRecord>, ChainError> {
        self.read(|s| {
            s.records
                .iter()
                .skip(from as usize)
                .take(limit.min(100) as usize)
                .cloned()
                .collect()
        })
    }

    async fn leaf_count(&self) -> Result<u32, ChainError> {
        self.read(|s| s.leaf_count)
    }

    async fn leaves(&self, from: u32, limit: u32) -> Result<Vec<String>, ChainError> {
        self.read(|s| {
            s.leaves
                .iter()
                .skip(from as usize)
                .take(limit.min(100) as usize)
                .cloned()
                .collect()
        })
    }

    async fn note(&self, note_id: u32) -> Result<Option<NoteInfo>, ChainError> {
        self.read(|s| {
            s.records
                .get(note_id as usize)
                .map(|r| r.status.clone())
                .or_else(|| s.notes.get(&note_id).cloned())
                .map(|status| NoteInfo { status })
        })
    }

    async fn nullifier_used(&self, nullifier: &str) -> Result<bool, ChainError> {
        self.read(|s| s.used.contains(&nullifier.to_ascii_lowercase()))
    }

    async fn head(&self) -> Result<Head, ChainError> {
        self.read(|s| Head {
            height: s.height,
            leaf_count: s.leaf_count,
        })
    }
}
