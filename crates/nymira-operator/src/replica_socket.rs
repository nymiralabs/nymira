//! The replication log has one owner process. Every other process (the public API, the signer)
//! reaches the replica through the owner's Unix socket, so two `Replicator`s never write one
//! log and fork it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::ipc;
use crate::replica::{Replica, ReplicaEntry, ReplicaError};
use crate::replog::Entry;

const WAIT: Duration = Duration::from_secs(30);

#[derive(Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum Call {
    Append(ReplicaEntry),
    Record {
        kind: String,
        key: String,
        payload: Option<String>,
    },
    Recorded {
        kind: String,
        key: String,
    },
    Sync,
    Restore,
    State,
}

#[derive(Serialize, Deserialize)]
struct Reply {
    ok: bool,
    error: Option<String>,
    payload: Option<String>,
    entries: Vec<Entry>,
    in_recovery: bool,
    health: String,
    real: bool,
}

/// Whether `line` is a request the replication owner would act on.
pub fn is_typed_request(line: &str) -> bool {
    serde_json::from_str::<Call>(line).is_ok()
}

/// Serves the owner's replica on `listener` to the one allowed user.
pub async fn serve(listener: tokio::net::UnixListener, replica: Arc<dyn Replica>, uid: u32) {
    let handler = move |call: Call| {
        let replica = replica.clone();
        async move {
            let result: Result<(Option<String>, Vec<Entry>), ReplicaError> = match call {
                Call::Append(entry) => replica.append(&entry).await.map(|_| (None, vec![])),
                Call::Record { kind, key, payload } => replica
                    .record(&kind, &key, payload.as_deref())
                    .await
                    .map(|_| (None, vec![])),
                Call::Recorded { kind, key } => Ok((replica.recorded(&kind, &key).await, vec![])),
                Call::Sync => replica.sync().await.map(|_| (None, vec![])),
                // The owner writes the entries into the shared database; sending them back would only
                // hit the line limit on a long log.
                Call::Restore => replica.restore().await.map(|_| (None, vec![])),
                Call::State => Ok((None, vec![])),
            };
            let (ok, error, payload, entries) = match result {
                Ok((payload, entries)) => (true, None, payload, entries),
                Err(e) => (false, Some(e.0), None, vec![]),
            };
            Reply {
                ok,
                error,
                payload,
                entries,
                in_recovery: replica.in_recovery(),
                health: replica.health().into(),
                real: replica.is_real(),
            }
        }
    };
    ipc::serve(listener, uid, handler, |why| Reply {
        ok: false,
        error: Some(format!("not a typed request: {why}")),
        payload: None,
        entries: vec![],
        in_recovery: true,
        health: "unreachable".into(),
        real: true,
    })
    .await;
}

/// The replica as another process sees it: calls go to the owner, and its recovery flag and
/// health ride back on every reply.
pub struct SocketReplica {
    path: PathBuf,
    recovery: AtomicBool,
    real: AtomicBool,
    health: Mutex<&'static str>,
}

impl SocketReplica {
    pub fn new(path: &Path) -> Self {
        Self {
            path: path.into(),
            // Until the owner has answered once, nothing may be signed.
            recovery: AtomicBool::new(true),
            real: AtomicBool::new(true),
            health: Mutex::new("unreachable"),
        }
    }

    async fn ask(&self, call: Call) -> Result<Reply, ReplicaError> {
        match ipc::call::<_, Reply>(&self.path, &call, WAIT).await {
            Ok(reply) => {
                self.recovery.store(reply.in_recovery, Ordering::SeqCst);
                self.real.store(reply.real, Ordering::SeqCst);
                *self.health.lock().unwrap_or_else(|e| e.into_inner()) = match reply.health.as_str()
                {
                    "ok" => "ok",
                    "recovery" => "recovery",
                    "diverged" => "diverged",
                    _ => "unreachable",
                };
                Ok(reply)
            }
            Err(e) => {
                *self.health.lock().unwrap_or_else(|e| e.into_inner()) = "unreachable";
                Err(ReplicaError(format!("the replication owner: {e}")))
            }
        }
    }

    async fn run(&self, call: Call) -> Result<Reply, ReplicaError> {
        let reply = self.ask(call).await?;
        if reply.ok {
            Ok(reply)
        } else {
            Err(ReplicaError(reply.error.unwrap_or_default()))
        }
    }
}

#[async_trait]
impl Replica for SocketReplica {
    async fn append(&self, entry: &ReplicaEntry) -> Result<(), ReplicaError> {
        self.run(Call::Append(entry.clone())).await.map(|_| ())
    }

    async fn record(
        &self,
        kind: &str,
        key: &str,
        payload: Option<&str>,
    ) -> Result<(), ReplicaError> {
        self.run(Call::Record {
            kind: kind.into(),
            key: key.into(),
            payload: payload.map(Into::into),
        })
        .await
        .map(|_| ())
    }

    async fn recorded(&self, kind: &str, key: &str) -> Option<String> {
        self.run(Call::Recorded {
            kind: kind.into(),
            key: key.into(),
        })
        .await
        .ok()
        .and_then(|r| r.payload)
    }

    fn in_recovery(&self) -> bool {
        self.recovery.load(Ordering::SeqCst)
    }

    async fn sync(&self) -> Result<(), ReplicaError> {
        self.run(Call::Sync).await.map(|_| ())
    }

    async fn restore(&self) -> Result<Vec<Entry>, ReplicaError> {
        self.run(Call::Restore).await.map(|r| r.entries)
    }

    fn health(&self) -> &'static str {
        *self.health.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn is_real(&self) -> bool {
        self.real.load(Ordering::SeqCst)
    }
}

impl SocketReplica {
    /// Asks the owner for its state, so `in_recovery` and `health` are current before the first
    /// request.
    pub async fn refresh(&self) -> Result<(), ReplicaError> {
        self.ask(Call::State).await.map(|_| ())
    }
}
