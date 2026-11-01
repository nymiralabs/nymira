//! A fake NEAR JSON-RPC node for tests. See the crate README.

mod rpc;
mod state;

pub use state::*;

use axum::{
    body::Bytes,
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use tokio::task::JoinHandle;

type Shared = Arc<Mutex<Chain>>;

fn lock(s: &Shared) -> MutexGuard<'_, Chain> {
    s.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone, Debug)]
pub struct Config {
    pub addr: SocketAddr,
    pub chain_id: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            addr: ([127, 0, 0, 1], 0).into(),
            chain_id: "testnet".into(),
        }
    }
}

/// One running fake provider. Dropping it stops the server.
pub struct FakeRpc {
    addr: SocketAddr,
    chain: Shared,
    task: JoinHandle<()>,
}

impl Drop for FakeRpc {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl FakeRpc {
    /// Starts an instance on a free local port.
    pub async fn start() -> FakeRpc {
        Self::bind(Config::default())
            .await
            .expect("bind a local port")
    }

    pub async fn bind(config: Config) -> std::io::Result<FakeRpc> {
        let listener = tokio::net::TcpListener::bind(config.addr).await?;
        let addr = listener.local_addr()?;
        let chain: Shared = Arc::new(Mutex::new(Chain::new(&config.chain_id)));
        let app = Router::new()
            .route("/", post(rpc_endpoint))
            .route("/control", post(control_endpoint))
            .route(
                "/control/state",
                get(|State(s): State<Shared>| async move { Json(lock(&s).summary()) }),
            )
            .route(
                "/control/submissions",
                get(|State(s): State<Shared>| async move { Json(lock(&s).submissions.clone()) }),
            )
            .route(
                "/control/requests",
                get(|State(s): State<Shared>| async move { Json(lock(&s).requests.clone()) }),
            )
            .with_state(chain.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(FakeRpc { addr, chain, task })
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:PORT`, the JSON-RPC URL.
    pub fn url(&self) -> String {
        format!("http://{}", self.addr)
    }

    /// Applies one scripted change. Panics on a malformed one (a test bug).
    pub fn apply(&self, c: Command) {
        lock(&self.chain).apply(c).expect("valid command");
    }

    /// Scripts a view. A newer script for the same key wins; exact args beat `ArgsMatch::Any`.
    pub fn set_view(&self, account: &str, method: &str, args: ArgsMatch, result: ViewResult) {
        self.apply(Command::SetView {
            account: account.into(),
            method: method.into(),
            args,
            result,
        });
    }

    pub fn set_account(&self, account: &str, spec: AccountSpec) {
        self.apply(Command::SetAccount {
            account: account.into(),
            spec,
        });
    }

    pub fn set_access_keys(&self, account: &str, keys: Vec<AccessKeySpec>) {
        self.apply(Command::SetAccessKeys {
            account: account.into(),
            keys,
        });
    }

    pub fn set_block(&self, height: u64, timestamp_ns: u64) {
        self.apply(Command::SetBlock {
            height,
            timestamp_ns,
        });
    }

    pub fn set_block_hash(&self, height: u64, hash: &str) {
        self.apply(Command::SetBlockHash {
            height,
            hash: hash.into(),
        });
    }

    /// Adds `n` blocks, each `seconds_per_block` after the previous. Returns the new head height.
    pub fn advance_blocks(&self, n: u64, seconds_per_block: f64) -> u64 {
        self.apply(Command::AdvanceBlocks {
            n,
            seconds_per_block,
        });
        self.head()
    }

    pub fn head(&self) -> u64 {
        lock(&self.chain).head()
    }

    /// Lagging node: serves blocks and state `blocks` behind its head.
    pub fn set_lag(&self, blocks: u64) {
        self.apply(Command::SetLag { blocks });
    }

    /// Fork: blocks from `from_height` get different hashes. `None` heals it.
    pub fn set_fork(&self, fork: Option<ForkSpec>) {
        self.apply(Command::SetFork { fork });
    }

    pub fn set_failure(&self, failure: Failure) {
        self.apply(Command::SetFailure { failure });
    }

    pub fn set_latency(&self, latency: Duration) {
        self.apply(Command::SetLatencyMs {
            ms: latency.as_millis() as u64,
        });
    }

    pub fn set_tx_status(&self, status: TxStatusSpec) {
        self.apply(Command::SetTxStatus { status });
    }

    /// Every transaction sent through `send_tx`, `broadcast_tx_commit` or `broadcast_tx_async`.
    pub fn submissions(&self) -> Vec<Submission> {
        lock(&self.chain).submissions.clone()
    }

    /// Every JSON-RPC request seen, including those a failure switch then refused.
    pub fn requests(&self) -> Vec<RequestRecord> {
        lock(&self.chain).requests.clone()
    }

    pub fn reset(&self) {
        self.apply(Command::Reset);
    }
}

fn json_response(status: StatusCode, body: String) -> Response {
    (status, [(header::CONTENT_TYPE, "application/json")], body).into_response()
}

async fn rpc_endpoint(State(s): State<Shared>, body: Bytes) -> Response {
    let req: Option<Value> = serde_json::from_slice(&body).ok();
    let id = req
        .as_ref()
        .and_then(|r| r.get("id"))
        .cloned()
        .unwrap_or(Value::Null);
    let method = req
        .as_ref()
        .and_then(|r| r.get("method"))
        .and_then(Value::as_str)
        .map(String::from);
    let params = req
        .as_ref()
        .and_then(|r| r.get("params"))
        .cloned()
        .unwrap_or(Value::Null);
    let (failure, latency_ms) = {
        let mut c = lock(&s);
        if let Some(m) = &method {
            c.requests.push(RequestRecord {
                method: m.clone(),
                params: params.clone(),
            });
        }
        (c.failure.clone(), c.latency_ms)
    };
    if latency_ms > 0 {
        tokio::time::sleep(Duration::from_millis(latency_ms)).await;
    }
    let reply = |result: RpcResult| {
        let body = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        };
        json_response(StatusCode::OK, body.to_string())
    };
    match failure {
        Failure::Http(code) => {
            let status = StatusCode::from_u16(code).unwrap_or(StatusCode::SERVICE_UNAVAILABLE);
            (status, "upstream unavailable").into_response()
        }
        Failure::Timeout => std::future::pending().await,
        Failure::Garbage => json_response(StatusCode::OK, "<html>not json".into()),
        Failure::RpcError { code, message } => reply(Err(
            json!({"code": code, "message": message, "data": message}),
        )),
        Failure::None => {
            let Some(method) = method else {
                let e = json!({"code": -32700, "message": "Parse error", "data": "request is not a JSON-RPC call"});
                return reply(Err(e));
            };
            reply(rpc::dispatch(&mut lock(&s), &method, &params))
        }
    }
}

async fn control_endpoint(State(s): State<Shared>, Json(cmd): Json<Command>) -> Response {
    match lock(&s).apply(cmd) {
        Ok(()) => json_response(StatusCode::OK, json!({"ok": true}).to_string()),
        Err(e) => json_response(
            StatusCode::BAD_REQUEST,
            json!({"ok": false, "error": e}).to_string(),
        ),
    }
}
