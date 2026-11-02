//! The replica seam: a reservation is replicated before a key is minted.
//!
//! The real channel (hash-chained log over Tailscale) lands with the durability ticket; the
//! operator already writes to this seam, holds its lock across the acknowledgement, and resends
//! or rolls back unacknowledged reservations at start-up.

use std::sync::Mutex;

use async_trait::async_trait;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplicaEntry {
    pub nullifier: String,
    pub lease_id: String,
    /// The request proof the watcher answers exits from.
    pub request_json: String,
    pub bound: u128,
    pub client_request_id: String,
    pub reserved_at: u64,
}

#[derive(Debug, thiserror::Error, Clone)]
#[error("replica unreachable: {0}")]
pub struct ReplicaError(pub String);

#[async_trait]
pub trait Replica: Send + Sync {
    /// Returns once the entry is on the replica's disk.
    async fn append(&self, entry: &ReplicaEntry) -> Result<(), ReplicaError>;

    /// Records a later fact about a lease or a nullifier (`replog::kinds`) and returns once the
    /// replica holds it. The default keeps nothing.
    async fn record(
        &self,
        _kind: &str,
        _key: &str,
        _payload: Option<&str>,
    ) -> Result<(), ReplicaError> {
        Ok(())
    }

    /// The payload recorded earlier for this kind and key, if any: a retry of a signature reuses
    /// the state the replica already holds instead of signing another.
    async fn recorded(&self, _kind: &str, _key: &str) -> Option<String> {
        None
    }

    /// True while this machine's log is behind the replica's: no signature may go out.
    fn in_recovery(&self) -> bool {
        false
    }

    /// Compares with the replica, brings it level, and notices when this machine is behind.
    async fn sync(&self) -> Result<(), ReplicaError> {
        Ok(())
    }

    /// Pulls what this machine lacks from the replica into the database and returns it.
    async fn restore(&self) -> Result<Vec<crate::replog::Entry>, ReplicaError> {
        Ok(Vec::new())
    }

    /// `ok`, or what is wrong with the link to the replica: shown in `/v1/status`.
    fn health(&self) -> &'static str {
        "ok"
    }

    /// False for stand-ins that keep nothing: only the sandbox may start with one.
    fn is_real(&self) -> bool {
        false
    }
}

/// Acknowledges at once and keeps nothing: for the sandbox profile only.
pub struct NoReplica;

#[async_trait]
impl Replica for NoReplica {
    async fn append(&self, _entry: &ReplicaEntry) -> Result<(), ReplicaError> {
        Ok(())
    }
}

/// Records entries; can be switched to fail.
#[derive(Default)]
pub struct FakeReplica {
    entries: Mutex<Vec<ReplicaEntry>>,
    records: Mutex<Vec<(String, String)>>,
    payloads: Mutex<std::collections::HashMap<(String, String), String>>,
    down: Mutex<bool>,
    recovery: Mutex<bool>,
}

impl FakeReplica {
    pub fn set_down(&self, down: bool) {
        *self.down.lock().unwrap_or_else(|e| e.into_inner()) = down;
    }

    /// Makes the fake say the operator is restoring (its log is behind the replica's).
    pub fn set_recovery(&self, on: bool) {
        *self.recovery.lock().unwrap_or_else(|e| e.into_inner()) = on;
    }

    /// `(kind, key)` of every later fact recorded.
    pub fn records(&self) -> Vec<(String, String)> {
        self.records
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    pub fn entries(&self) -> Vec<ReplicaEntry> {
        self.entries
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }
}

#[async_trait]
impl Replica for FakeReplica {
    async fn append(&self, entry: &ReplicaEntry) -> Result<(), ReplicaError> {
        if *self.down.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err(ReplicaError("down".into()));
        }
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if !entries.iter().any(|e| e.lease_id == entry.lease_id) {
            entries.push(entry.clone());
        }
        Ok(())
    }

    async fn record(
        &self,
        kind: &str,
        key: &str,
        payload: Option<&str>,
    ) -> Result<(), ReplicaError> {
        if *self.down.lock().unwrap_or_else(|e| e.into_inner()) {
            return Err(ReplicaError("down".into()));
        }
        if let Some(p) = payload {
            self.payloads
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .entry((kind.to_string(), key.to_string()))
                .or_insert_with(|| p.to_string());
        }
        let mut records = self.records.lock().unwrap_or_else(|e| e.into_inner());
        let fact = (kind.to_string(), key.to_string());
        if !records.contains(&fact) {
            records.push(fact);
        }
        Ok(())
    }

    fn in_recovery(&self) -> bool {
        *self.recovery.lock().unwrap_or_else(|e| e.into_inner())
    }

    async fn recorded(&self, kind: &str, key: &str) -> Option<String> {
        self.payloads
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(kind.to_string(), key.to_string()))
            .cloned()
    }
}
