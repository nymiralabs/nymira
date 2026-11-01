//! Scripted chain state and the NEAR-shaped JSON built from it.

use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap};

/// Height of the first block every instance has.
pub const GENESIS_HEIGHT: u64 = 1_000;
/// Timestamp of the first block: 2026-11-01T00:00:00Z, in nanoseconds.
pub const GENESIS_TIME_NS: u64 = 1_793_491_200_000_000_000;
const GAS_PRICE: &str = "100000000";
const PROTOCOL_VERSION: u32 = 88;

pub type RpcResult = Result<Value, Value>;

/// Which `args_base64` payloads a scripted view answers.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArgsMatch {
    /// Any arguments (lowest priority).
    #[default]
    Any,
    /// Arguments that parse as this JSON value.
    Json(Value),
    /// Exactly these bytes, given as base64.
    Base64(String),
}

impl ArgsMatch {
    pub fn json(v: Value) -> Self {
        Self::Json(v)
    }
    pub fn bytes(b: &[u8]) -> Self {
        Self::Base64(STANDARD.encode(b))
    }
}

/// What a scripted view returns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViewResult {
    /// The result bytes are this value's JSON text.
    Json(Value),
    /// The result bytes, given as base64 (for junk or non-JSON returns).
    Base64(String),
    /// The call fails like a panicking contract (`error` instead of `result`).
    Error(String),
}

impl ViewResult {
    pub fn json(v: Value) -> Self {
        Self::Json(v)
    }
    pub fn bytes(b: &[u8]) -> Self {
        Self::Base64(STANDARD.encode(b))
    }
    pub fn error(m: impl Into<String>) -> Self {
        Self::Error(m.into())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AccountSpec {
    pub amount: String,
    pub locked: String,
    pub code_hash: String,
    pub storage_usage: u64,
}

impl Default for AccountSpec {
    fn default() -> Self {
        Self {
            amount: "1000000000000000000000000000".into(),
            locked: "0".into(),
            code_hash: "11111111111111111111111111111111".into(),
            storage_usage: 182,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AccessKeySpec {
    pub public_key: String,
    #[serde(default)]
    pub nonce: u64,
    /// `"FullAccess"` or `{"FunctionCall": {...}}`, as NEAR prints it.
    #[serde(default = "full_access")]
    pub permission: Value,
}

fn full_access() -> Value {
    json!("FullAccess")
}

impl AccessKeySpec {
    pub fn full_access(public_key: &str, nonce: u64) -> Self {
        Self {
            public_key: public_key.into(),
            nonce,
            permission: full_access(),
        }
    }
    pub fn function_call(
        public_key: &str,
        nonce: u64,
        receiver_id: &str,
        methods: &[&str],
    ) -> Self {
        Self {
            public_key: public_key.into(),
            nonce,
            permission: json!({"FunctionCall": {"allowance": null, "receiver_id": receiver_id, "method_names": methods}}),
        }
    }
}

/// Outcome of a transaction or one receipt.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutcomeSpec {
    /// `SuccessValue` with this base64 value.
    Success(String),
    /// `Failure` with an execution error message.
    Failure(String),
}

impl Default for OutcomeSpec {
    fn default() -> Self {
        Self::Success(String::new())
    }
}

impl OutcomeSpec {
    fn json(&self) -> Value {
        match self {
            Self::Success(v) => json!({"SuccessValue": v}),
            Self::Failure(m) => {
                json!({"Failure": {"ActionError": {"index": 0, "kind": {"FunctionCallError": {"ExecutionError": m}}}}})
            }
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ReceiptSpec {
    pub executor_id: String,
    #[serde(default)]
    pub logs: Vec<String>,
    #[serde(default)]
    pub status: OutcomeSpec,
}

impl ReceiptSpec {
    pub fn new(executor_id: &str) -> Self {
        Self {
            executor_id: executor_id.into(),
            ..Self::default()
        }
    }
    pub fn log(mut self, line: impl Into<String>) -> Self {
        self.logs.push(line.into());
        self
    }
    /// Adds an NEP-297 `EVENT_JSON:` log line.
    pub fn event(self, standard: &str, version: &str, event: &str, data: Value) -> Self {
        self.log(event_log(standard, version, event, data))
    }
    pub fn status(mut self, s: OutcomeSpec) -> Self {
        self.status = s;
        self
    }
}

/// An NEP-297 event log line.
pub fn event_log(standard: &str, version: &str, event: &str, data: Value) -> String {
    format!(
        "EVENT_JSON:{}",
        json!({"standard": standard, "version": version, "event": event, "data": data})
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TxStatusSpec {
    pub tx_hash: String,
    pub signer_id: String,
    pub receiver_id: String,
    #[serde(default)]
    pub nonce: u64,
    /// Block that included the transaction. Default: the head at the time of scripting.
    /// A lagging node hides the transaction until it has served that block.
    #[serde(default)]
    pub block_height: Option<u64>,
    #[serde(default)]
    pub status: OutcomeSpec,
    /// Empty means one implicit receipt at `receiver_id` with `status`.
    #[serde(default)]
    pub receipts: Vec<ReceiptSpec>,
    #[serde(default)]
    pub actions: Vec<Value>,
    #[serde(default = "final_status")]
    pub final_execution_status: String,
}

fn final_status() -> String {
    "FINAL".into()
}

impl TxStatusSpec {
    pub fn success(tx_hash: &str, signer_id: &str, receiver_id: &str) -> Self {
        Self {
            tx_hash: tx_hash.into(),
            signer_id: signer_id.into(),
            receiver_id: receiver_id.into(),
            nonce: 1,
            block_height: None,
            status: OutcomeSpec::default(),
            receipts: vec![],
            actions: vec![],
            final_execution_status: final_status(),
        }
    }
    pub fn at_height(mut self, h: u64) -> Self {
        self.block_height = Some(h);
        self
    }
    pub fn status(mut self, s: OutcomeSpec) -> Self {
        self.status = s;
        self
    }
    pub fn receipt(mut self, r: ReceiptSpec) -> Self {
        self.receipts.push(r);
        self
    }
}

/// Blocks at or above `from_height` get hashes in the `tag` domain.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForkSpec {
    pub from_height: u64,
    pub tag: String,
}

/// How a provider misbehaves for every JSON-RPC request.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Failure {
    #[default]
    None,
    /// Answer with this HTTP status (use 5xx) and a plain body.
    Http(u16),
    /// Answer HTTP 200 with a JSON-RPC error.
    RpcError { code: i64, message: String },
    /// Accept the request and never answer.
    Timeout,
    /// Answer HTTP 200 with a body that is not JSON.
    Garbage,
}

/// One scripted change. The same shape drives the in-process handle and `POST /control`.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    SetView {
        account: String,
        method: String,
        #[serde(default)]
        args: ArgsMatch,
        result: ViewResult,
    },
    SetAccount {
        account: String,
        #[serde(default)]
        spec: AccountSpec,
    },
    SetAccessKeys {
        account: String,
        keys: Vec<AccessKeySpec>,
    },
    SetBlock {
        height: u64,
        timestamp_ns: u64,
    },
    SetBlockHash {
        height: u64,
        hash: String,
    },
    AdvanceBlocks {
        n: u64,
        seconds_per_block: f64,
    },
    SetLag {
        blocks: u64,
    },
    SetFork {
        fork: Option<ForkSpec>,
    },
    SetFailure {
        failure: Failure,
    },
    SetLatencyMs {
        ms: u64,
    },
    SetTxStatus {
        status: TxStatusSpec,
    },
    /// Clears scripted views, accounts, keys, transactions, logs and every switch. Blocks stay.
    Reset,
}

#[derive(Clone, Debug, Serialize)]
pub struct Submission {
    pub method: String,
    pub tx_base64: String,
    pub tx_hash: String,
    pub signer_id: String,
    pub receiver_id: String,
    pub nonce: u64,
    pub wait_until: Option<String>,
    pub at_height: u64,
}

#[derive(Clone, Debug, Serialize)]
pub struct RequestRecord {
    pub method: String,
    pub params: Value,
}

enum Matcher {
    Any,
    Json(Value),
    Bytes(Vec<u8>),
}

enum Output {
    Bytes(Vec<u8>),
    Error(String),
}

struct ViewEntry {
    since: u64,
    account: String,
    method: String,
    matcher: Matcher,
    out: Output,
}

/// Latest value at or below `height`.
fn at<T>(v: &[(u64, T)], height: u64) -> Option<&T> {
    v.iter()
        .rev()
        .find(|(since, _)| *since <= height)
        .map(|(_, t)| t)
}

fn b64(s: &str) -> Result<Vec<u8>, String> {
    STANDARD
        .decode(s)
        .map_err(|e| format!("invalid base64: {e}"))
}

fn hash58(s: &str) -> String {
    bs58::encode(Sha256::digest(s.as_bytes())).into_string()
}

pub struct Chain {
    pub chain_id: String,
    head: u64,
    times: BTreeMap<u64, u64>,
    hash_overrides: HashMap<u64, String>,
    lag: u64,
    fork: Option<ForkSpec>,
    views: Vec<ViewEntry>,
    accounts: HashMap<String, Vec<(u64, AccountSpec)>>,
    keys: HashMap<String, Vec<(u64, Vec<AccessKeySpec>)>>,
    txs: HashMap<String, TxStatusSpec>,
    pub failure: Failure,
    pub latency_ms: u64,
    pub submissions: Vec<Submission>,
    pub requests: Vec<RequestRecord>,
}

impl Chain {
    pub fn new(chain_id: &str) -> Self {
        Self {
            chain_id: chain_id.into(),
            head: GENESIS_HEIGHT,
            times: BTreeMap::from([(GENESIS_HEIGHT, GENESIS_TIME_NS)]),
            hash_overrides: HashMap::new(),
            lag: 0,
            fork: None,
            views: vec![],
            accounts: HashMap::new(),
            keys: HashMap::new(),
            txs: HashMap::new(),
            failure: Failure::None,
            latency_ms: 0,
            submissions: vec![],
            requests: vec![],
        }
    }

    pub fn head(&self) -> u64 {
        self.head
    }

    /// Newest block this node shows: the head minus the lag.
    pub fn served_head(&self) -> u64 {
        let target = self.head.saturating_sub(self.lag);
        self.times
            .range(..=target)
            .next_back()
            .map_or(self.head, |(h, _)| *h)
    }

    fn compute_hash(&self, h: u64) -> String {
        let tag = self
            .fork
            .as_ref()
            .filter(|f| h >= f.from_height)
            .map_or("", |f| f.tag.as_str());
        hash58(&format!("nymira-fake-block/{}/{h}/{tag}", self.chain_id))
    }

    pub fn block_hash(&self, h: u64) -> String {
        self.hash_overrides
            .get(&h)
            .cloned()
            .unwrap_or_else(|| self.compute_hash(h))
    }

    pub fn apply(&mut self, c: Command) -> Result<(), String> {
        match c {
            Command::SetView {
                account,
                method,
                args,
                result,
            } => {
                let matcher = match args {
                    ArgsMatch::Any => Matcher::Any,
                    ArgsMatch::Json(v) => Matcher::Json(v),
                    ArgsMatch::Base64(s) => Matcher::Bytes(b64(&s)?),
                };
                let out = match result {
                    ViewResult::Json(v) => {
                        Output::Bytes(serde_json::to_vec(&v).map_err(|e| e.to_string())?)
                    }
                    ViewResult::Base64(s) => Output::Bytes(b64(&s)?),
                    ViewResult::Error(m) => Output::Error(m),
                };
                self.views.push(ViewEntry {
                    since: self.head,
                    account,
                    method,
                    matcher,
                    out,
                });
            }
            Command::SetAccount { account, spec } => self
                .accounts
                .entry(account)
                .or_default()
                .push((self.head, spec)),
            Command::SetAccessKeys { account, keys } => self
                .keys
                .entry(account)
                .or_default()
                .push((self.head, keys)),
            Command::SetBlock {
                height,
                timestamp_ns,
            } => {
                self.times.insert(height, timestamp_ns);
                self.head = self.head.max(height);
            }
            Command::SetBlockHash { height, hash } => {
                self.hash_overrides.insert(height, hash);
            }
            Command::AdvanceBlocks {
                n,
                seconds_per_block,
            } => {
                if !seconds_per_block.is_finite() || seconds_per_block < 0.0 {
                    return Err("seconds_per_block must be a non-negative number".into());
                }
                let step = (seconds_per_block * 1e9) as u64;
                for _ in 0..n {
                    let last = self
                        .times
                        .get(&self.head)
                        .copied()
                        .unwrap_or(GENESIS_TIME_NS);
                    self.head += 1;
                    self.times.insert(self.head, last + step);
                }
            }
            Command::SetLag { blocks } => self.lag = blocks,
            Command::SetFork { fork } => self.fork = fork,
            Command::SetFailure { failure } => {
                if let Failure::Http(code) = failure {
                    if !(100..1000).contains(&code) {
                        return Err(format!("bad HTTP status {code}"));
                    }
                }
                self.failure = failure;
            }
            Command::SetLatencyMs { ms } => self.latency_ms = ms,
            Command::SetTxStatus { mut status } => {
                status.block_height.get_or_insert(self.head);
                self.txs.insert(status.tx_hash.clone(), status);
            }
            Command::Reset => {
                self.views.clear();
                self.accounts.clear();
                self.keys.clear();
                self.txs.clear();
                self.submissions.clear();
                self.requests.clear();
                self.hash_overrides.clear();
                self.failure = Failure::None;
                self.latency_ms = 0;
                self.lag = 0;
                self.fork = None;
            }
        }
        Ok(())
    }

    /// Height a `query`/`block` reference points at, or the NEAR-shaped error.
    pub fn resolve_block(&self, p: &Value) -> Result<u64, Value> {
        let served = self.served_head();
        if let Some(f) = p.get("finality").and_then(Value::as_str) {
            return match f {
                "final" | "optimistic" | "near-final" => Ok(served),
                other => Err(invalid_params(format!("unknown finality {other}"))),
            };
        }
        match p.get("block_id") {
            Some(Value::Number(n)) => {
                let h = n
                    .as_u64()
                    .ok_or_else(|| invalid_params("block_id must be unsigned".into()))?;
                if h <= served && self.times.contains_key(&h) {
                    Ok(h)
                } else {
                    Err(unknown_block(
                        json!({"block_height": h}),
                        format!("DB Not Found Error: BLOCK HEIGHT: {h} \n Cause: Unknown"),
                    ))
                }
            }
            Some(Value::String(s)) => self
                .times
                .keys()
                .take_while(|h| **h <= served)
                .find(|h| self.block_hash(**h) == *s)
                .copied()
                .ok_or_else(|| {
                    unknown_block(
                        json!({"block_hash": s}),
                        format!("DB Not Found Error: BLOCK: {s}"),
                    )
                }),
            _ => Err(invalid_params(
                "a block reference needs finality or block_id".into(),
            )),
        }
    }

    fn account_exists(&self, account: &str, h: u64) -> bool {
        self.accounts
            .get(account)
            .is_some_and(|v| at(v, h).is_some())
            || self
                .views
                .iter()
                .any(|e| e.account == account && e.since <= h)
    }

    pub fn unknown_account(&self, account: &str, h: u64) -> Value {
        handler_error(
            "UNKNOWN_ACCOUNT",
            json!({"block_hash": self.block_hash(h), "block_height": h, "requested_account_id": account}),
            format!("account {account} does not exist while viewing at block #{h}"),
        )
    }

    pub fn require_account(&self, account: &str, h: u64) -> Result<(), Value> {
        if self.account_exists(account, h) {
            Ok(())
        } else {
            Err(self.unknown_account(account, h))
        }
    }

    pub fn account(&self, account: &str, h: u64) -> AccountSpec {
        self.accounts
            .get(account)
            .and_then(|v| at(v, h))
            .cloned()
            .unwrap_or_default()
    }

    pub fn access_keys(&self, account: &str, h: u64) -> Vec<AccessKeySpec> {
        self.keys
            .get(account)
            .and_then(|v| at(v, h))
            .cloned()
            .unwrap_or_default()
    }

    /// Exact matches beat `Any`; among equals the newest script wins.
    pub fn call_view(
        &self,
        account: &str,
        method: &str,
        args: &[u8],
        h: u64,
    ) -> Result<Vec<u8>, String> {
        let parsed: Option<Value> = serde_json::from_slice(args).ok();
        let best = self
            .views
            .iter()
            .enumerate()
            .filter(|(_, e)| e.account == account && e.method == method && e.since <= h)
            .filter(|(_, e)| match &e.matcher {
                Matcher::Any => true,
                Matcher::Json(v) => parsed.as_ref() == Some(v),
                Matcher::Bytes(b) => b == args,
            })
            .max_by_key(|(i, e)| (!matches!(e.matcher, Matcher::Any), e.since, *i));
        match best {
            Some((_, e)) => match &e.out {
                Output::Bytes(b) => Ok(b.clone()),
                Output::Error(m) => Err(m.clone()),
            },
            None => Err("wasm execution failed with error: FunctionCallError(MethodResolveError(MethodNotFound))".into()),
        }
    }

    pub fn tx(&self, hash: &str) -> Option<&TxStatusSpec> {
        let served = self.served_head();
        self.txs
            .get(hash)
            .filter(|t| t.block_height.unwrap_or(0) <= served)
    }

    /// Stores `spec` unless a status for that hash is already scripted; returns what is stored.
    pub fn record_tx(&mut self, spec: TxStatusSpec) -> TxStatusSpec {
        self.txs.entry(spec.tx_hash.clone()).or_insert(spec).clone()
    }

    pub fn block_json(&self, h: u64) -> Value {
        let prev = self.times.range(..h).next_back().map_or_else(
            || self.compute_hash(h.saturating_sub(1)),
            |(p, _)| self.block_hash(*p),
        );
        let ts = self.times[&h];
        json!({
            "author": "nymira-fake.near",
            "header": {
                "height": h,
                "epoch_id": hash58("nymira-fake-epoch"),
                "next_epoch_id": hash58("nymira-fake-epoch"),
                "hash": self.block_hash(h),
                "prev_hash": prev,
                "prev_state_root": hash58("nymira-fake-state"),
                "timestamp": ts,
                "timestamp_nanosec": ts.to_string(),
                "gas_price": GAS_PRICE,
                "latest_protocol_version": PROTOCOL_VERSION,
                "approvals": [],
                "chunks_included": 0
            },
            "chunks": []
        })
    }

    pub fn status_json(&self) -> Value {
        let h = self.served_head();
        let (&first, &first_ts) = self.times.iter().next().expect("genesis block exists");
        json!({
            "version": {"version": "nymira-fake", "build": "nymira-fake"},
            "chain_id": self.chain_id,
            "protocol_version": PROTOCOL_VERSION,
            "latest_protocol_version": PROTOCOL_VERSION,
            "rpc_addr": "0.0.0.0:3030",
            "validators": [],
            "sync_info": {
                "latest_block_hash": self.block_hash(h),
                "latest_block_height": h,
                "latest_state_root": hash58("nymira-fake-state"),
                "latest_block_time": rfc3339(self.times[&h]),
                "syncing": false,
                "earliest_block_hash": self.block_hash(first),
                "earliest_block_height": first,
                "earliest_block_time": rfc3339(first_ts)
            }
        })
    }

    pub fn gas_price(&self) -> &'static str {
        GAS_PRICE
    }

    /// Switch state, for `GET /control/state`.
    pub fn summary(&self) -> Value {
        json!({
            "head": self.head,
            "served_head": self.served_head(),
            "lag": self.lag,
            "fork": self.fork,
            "failure": self.failure,
            "latency_ms": self.latency_ms,
            "chain_id": self.chain_id,
        })
    }

    pub fn tx_json(&self, t: &TxStatusSpec) -> Value {
        let h = t.block_height.unwrap_or(self.head);
        let block_hash = self.block_hash(h);
        let implicit = [ReceiptSpec {
            executor_id: t.receiver_id.clone(),
            logs: vec![],
            status: t.status.clone(),
        }];
        let receipts: &[ReceiptSpec] = if t.receipts.is_empty() {
            &implicit
        } else {
            &t.receipts
        };
        let ids: Vec<String> = (0..receipts.len())
            .map(|i| hash58(&format!("{}/receipt/{i}", t.tx_hash)))
            .collect();
        let outcomes: Vec<Value> = receipts
            .iter()
            .enumerate()
            .map(|(i, r)| {
                json!({
                    "proof": [],
                    "block_hash": block_hash,
                    "id": ids[i],
                    "outcome": {
                        "logs": r.logs,
                        "receipt_ids": ids.get(i + 1).map(|n| vec![n.clone()]).unwrap_or_default(),
                        "gas_burnt": 2_428_000_000_000u64,
                        "tokens_burnt": "242800000000000000000",
                        "executor_id": r.executor_id,
                        "status": if i + 1 == receipts.len() { r.status.json() } else { json!({"SuccessReceiptId": ids[i + 1]}) }
                    }
                })
            })
            .collect();
        let bodies: Vec<Value> = receipts
            .iter()
            .enumerate()
            .map(|(i, r)| {
                json!({
                    "predecessor_id": if i == 0 { t.signer_id.clone() } else { receipts[i - 1].executor_id.clone() },
                    "receiver_id": r.executor_id,
                    "receipt_id": ids[i],
                    "receipt": {"Action": {
                        "signer_id": t.signer_id, "signer_public_key": "", "gas_price": GAS_PRICE,
                        "output_data_receivers": [], "input_data_ids": [], "actions": []
                    }}
                })
            })
            .collect();
        json!({
            "final_execution_status": t.final_execution_status,
            "status": t.status.json(),
            "transaction": {
                "signer_id": t.signer_id, "public_key": "", "nonce": t.nonce,
                "receiver_id": t.receiver_id, "actions": t.actions, "signature": "", "hash": t.tx_hash
            },
            "transaction_outcome": {
                "proof": [], "block_hash": block_hash, "id": t.tx_hash,
                "outcome": {
                    "logs": [], "receipt_ids": [ids[0]], "gas_burnt": 308_000_000_000u64,
                    "tokens_burnt": "30800000000000000000", "executor_id": t.signer_id,
                    "status": {"SuccessReceiptId": ids[0]}
                }
            },
            "receipts_outcome": outcomes,
            "receipts": bodies
        })
    }
}

/// `2026-11-01T00:00:00.000000000Z`.
pub fn rfc3339(ns: u64) -> String {
    let (secs, nanos) = (ns / 1_000_000_000, ns % 1_000_000_000);
    let (days, rem) = (secs / 86_400, secs % 86_400);
    let z = days + 719_468;
    let era = z / 146_097;
    let doe = z % 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + u64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{nanos:09}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

/// What the fake reads from a signed transaction.
pub struct TxInfo {
    pub signer_id: String,
    pub receiver_id: String,
    pub nonce: u64,
    pub hash: String,
}

/// Parses the leading fields of a borsh `SignedTransaction` and hashes the
/// transaction part (everything before the trailing signature), as NEAR does.
/// An ed25519 signature is 65 bytes with its type byte, secp256k1 is 66; the
/// ed25519 layout is tried first.
pub fn parse_signed_tx(bytes: &[u8]) -> Option<TxInfo> {
    fn take<'a>(b: &'a [u8], at: &mut usize, n: usize) -> Option<&'a [u8]> {
        let s = b.get(*at..at.checked_add(n)?)?;
        *at += n;
        Some(s)
    }
    fn string(b: &[u8], at: &mut usize) -> Option<String> {
        let len = u32::from_le_bytes(take(b, at, 4)?.try_into().ok()?) as usize;
        String::from_utf8(take(b, at, len)?.to_vec()).ok()
    }
    let mut at = 0;
    let signer_id = string(bytes, &mut at)?;
    let key_len = match take(bytes, &mut at, 1)?[0] {
        0 => 32,
        1 => 64,
        _ => return None,
    };
    take(bytes, &mut at, key_len)?;
    let nonce = u64::from_le_bytes(take(bytes, &mut at, 8)?.try_into().ok()?);
    let receiver_id = string(bytes, &mut at)?;
    let sig_len = if bytes.len() >= 65 && bytes[bytes.len() - 65] == 0 {
        65
    } else {
        66
    };
    let tx = bytes.get(..bytes.len().checked_sub(sig_len)?)?;
    Some(TxInfo {
        signer_id,
        receiver_id,
        nonce,
        hash: bs58::encode(Sha256::digest(tx)).into_string(),
    })
}

pub fn handler_error(cause: &str, info: Value, data: String) -> Value {
    json!({"name": "HANDLER_ERROR", "cause": {"name": cause, "info": info}, "code": -32000, "message": "Server error", "data": data})
}

pub fn unknown_block(reference: Value, data: String) -> Value {
    handler_error("UNKNOWN_BLOCK", json!({"block_reference": reference}), data)
}

pub fn invalid_params(msg: String) -> Value {
    json!({
        "name": "REQUEST_VALIDATION_ERROR",
        "cause": {"name": "PARSE_ERROR", "info": {"error_message": msg}},
        "code": -32602, "message": "Invalid params", "data": {"error_message": msg}
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_known_values() {
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000000000Z");
        assert_eq!(rfc3339(GENESIS_TIME_NS), "2026-11-01T00:00:00.000000000Z");
        assert_eq!(
            rfc3339(1_790_000_000_500_000_001),
            "2026-09-21T14:13:20.500000001Z"
        );
        assert_eq!(
            rfc3339(951_782_400_000_000_000),
            "2000-02-29T00:00:00.000000000Z"
        );
    }

    fn put_str(out: &mut Vec<u8>, s: &str) {
        out.extend((s.len() as u32).to_le_bytes());
        out.extend(s.as_bytes());
    }

    #[test]
    fn signed_tx_parses_and_hashes_the_unsigned_part() {
        for sig_type in [0u8, 1] {
            let mut tx = vec![];
            put_str(&mut tx, "alice.testnet");
            tx.push(0);
            tx.extend([7u8; 32]);
            tx.extend(42u64.to_le_bytes());
            put_str(&mut tx, "vault.testnet");
            tx.extend([9u8; 32]); // block hash
            tx.extend(0u32.to_le_bytes()); // no actions
            let mut signed = tx.clone();
            signed.push(sig_type);
            signed.extend(vec![5u8; if sig_type == 0 { 64 } else { 65 }]);
            let info = parse_signed_tx(&signed).expect("parses");
            assert_eq!(
                (
                    info.signer_id.as_str(),
                    info.receiver_id.as_str(),
                    info.nonce
                ),
                ("alice.testnet", "vault.testnet", 42)
            );
            assert_eq!(info.hash, bs58::encode(Sha256::digest(&tx)).into_string());
        }
        assert!(parse_signed_tx(b"junk").is_none());
    }
}
