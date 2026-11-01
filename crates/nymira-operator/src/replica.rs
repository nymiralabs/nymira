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
}

#[derive(Debug, thiserror::Error, Clone)]
#[error("replica unreachable: {0}")]
pub struct ReplicaError(pub String);

#[async_trait]
pub trait Replica: Send + Sync {
    /// Returns once the entry is on the replica's disk.
    async fn append(&self, entry: &ReplicaEntry) -> Result<(), ReplicaError>;
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
    down: Mutex<bool>,
}

impl FakeReplica {
    pub fn set_down(&self, down: bool) {
        *self.down.lock().unwrap_or_else(|e| e.into_inner()) = down;
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
}
