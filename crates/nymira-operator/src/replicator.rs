//! The laptop's end of replication: every fact goes into the local log, then to the replica, and
//! the call returns only when the replica has acknowledged it with its own key.
//!
//! An entry that could not be delivered stays in the local log, unacknowledged. Before anything
//! new is written, the backlog is pushed first, in order, so the replica's chain is always a
//! prefix of ours. While that fails, nothing that needs a signature or a reservation goes out.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::clock::Clock;
use crate::replica::{Replica, ReplicaEntry, ReplicaError};
use crate::replog::{audit, kinds, verify_head, Audit, Entry, Link, LinkError, Log, SignedHead};

/// Entries pushed or fetched per call.
const BATCH: u32 = 100;
/// Random old entries asked about at each sync, besides the last common one.
const SAMPLES: usize = 3;

/// What the last sync found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Ready,
    /// The replica holds entries this machine does not: this disk is old or new. Signing stays
    /// off until [`Replicator::restore`] has brought the log level.
    Recovery {
        replica: u64,
        local: u64,
    },
    /// The two chains disagree at `seq`. Nothing is signed; a person looks.
    Diverged {
        seq: u64,
    },
    Unreachable(String),
}

pub struct Replicator {
    log: Log,
    link: Arc<dyn Link>,
    /// The replica's public key, pinned in the laptop's config.
    pk: [u8; 32],
    clock: Arc<dyn Clock>,
    gate: tokio::sync::Mutex<()>,
    state: Mutex<State>,
}

fn down(e: impl std::fmt::Display) -> ReplicaError {
    ReplicaError(e.to_string())
}

impl Replicator {
    pub fn new(log: Log, link: Arc<dyn Link>, pk: [u8; 32], clock: Arc<dyn Clock>) -> Self {
        Self {
            log,
            link,
            pk,
            clock,
            gate: tokio::sync::Mutex::new(()),
            state: Mutex::new(State::Ready),
        }
    }

    pub fn log(&self) -> &Log {
        &self.log
    }

    pub fn state(&self) -> State {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn set(&self, s: State) {
        *self.state.lock().unwrap_or_else(|e| e.into_inner()) = s;
    }

    /// True while the replica is ahead of this log.
    pub fn in_recovery(&self) -> bool {
        matches!(self.state(), State::Recovery { .. })
    }

    /// An acknowledgement that names the entry, signed by the pinned key.
    fn acknowledges(&self, ack: &SignedHead, entry: &Entry) -> bool {
        verify_head(&self.pk, ack) && ack.seq == entry.seq && ack.head == entry.hash
    }

    /// Brings the replica level with the local log and settles what it found.
    pub async fn sync(&self) -> Result<(), ReplicaError> {
        let _gate = self.gate.lock().await;
        self.sync_locked().await
    }

    async fn sync_locked(&self) -> Result<(), ReplicaError> {
        let found = match audit(&self.log, &*self.link, &self.pk, SAMPLES).await {
            Ok(found) => found,
            Err(e) => {
                self.set(State::Unreachable(e.to_string()));
                return Err(down(e));
            }
        };
        match found {
            Audit::Match => {
                self.set(State::Ready);
                Ok(())
            }
            Audit::Ahead { replica, local } => {
                self.set(State::Recovery { replica, local });
                Err(down("the replica holds entries this machine lacks"))
            }
            Audit::Diverged { seq } => {
                self.set(State::Diverged { seq });
                Err(down(format!("the chains differ at {seq}")))
            }
            Audit::Behind { replica, .. } => {
                let mut next = replica + 1;
                loop {
                    let batch = self.log.from(next, BATCH).await.map_err(down)?;
                    let Some(last) = batch.last().map(|e| e.seq) else {
                        break;
                    };
                    for entry in &batch {
                        let ack = self.link.append(entry).await.map_err(|e| {
                            if let LinkError::Refused(crate::replog::LogError::Fork { seq }) = e {
                                self.set(State::Diverged { seq });
                            } else {
                                self.set(State::Unreachable(e.to_string()));
                            }
                            down(e)
                        })?;
                        if !self.acknowledges(&ack, entry) {
                            self.set(State::Unreachable("bad acknowledgement".into()));
                            return Err(down("the acknowledgement does not verify"));
                        }
                    }
                    self.log.mark_acked(last).await.map_err(down)?;
                    next = last + 1;
                }
                self.set(State::Ready);
                Ok(())
            }
        }
    }

    /// Writes the fact to the local log once, then delivers the log. Repeating a fact is safe.
    ///
    /// A fact that releases something (a reservation, a signed state, a clearance) is written
    /// only after the backlog went through: with the replica away, nothing new piles up behind
    /// the signature it would have allowed. Plain status (delivery, settlement, a rolled-back
    /// reservation) is written at once and delivered when it can be.
    pub async fn write(
        &self,
        kind: &str,
        key: &str,
        payload: Option<&str>,
    ) -> Result<(), ReplicaError> {
        let _gate = self.gate.lock().await;
        if self.log.has(kind, key).await.map_err(down)? {
            return self.sync_locked().await;
        }
        let releases = matches!(kind, kinds::RESERVATION | kinds::SIGNED | kinds::CLEARANCE);
        if releases {
            self.sync_locked().await?;
        }
        self.log
            .append(kind, key, self.clock.now_secs(), payload, false)
            .await
            .map_err(down)?;
        self.sync_locked().await
    }

    /// Pulls what the replica holds beyond the local head into the local log and returns it,
    /// oldest first, for the caller to apply to its database. The log is level afterwards.
    pub async fn restore(&self) -> Result<Vec<Entry>, ReplicaError> {
        let _gate = self.gate.lock().await;
        let mut restored = Vec::new();
        loop {
            let (head, _) = self.log.head().await.map_err(down)?;
            let batch = self.link.fetch(head + 1, BATCH).await.map_err(down)?;
            if batch.is_empty() {
                break;
            }
            for entry in batch {
                self.log.accept(&entry).await.map_err(down)?;
                restored.push(entry);
            }
        }
        // Level now only if the replica's signed head is the local head.
        let signed = self.link.head().await.map_err(down)?;
        let (seq, hash) = self.log.head().await.map_err(down)?;
        if !verify_head(&self.pk, &signed) || signed.seq != seq || signed.head != hash {
            self.set(State::Unreachable(
                "heads do not match after restore".into(),
            ));
            return Err(down("the heads do not match after the restore"));
        }
        self.set(State::Ready);
        Ok(restored)
    }
}

#[async_trait]
impl Replica for Replicator {
    async fn append(&self, entry: &ReplicaEntry) -> Result<(), ReplicaError> {
        let payload = serde_json::json!({
            "lease_id": entry.lease_id,
            "nullifier": entry.nullifier,
            "bound": entry.bound.to_string(),
            "client_request_id": entry.client_request_id,
            "reserved_at": entry.reserved_at,
            "request_json": entry.request_json,
        })
        .to_string();
        self.write(kinds::RESERVATION, &entry.lease_id, Some(&payload))
            .await
    }

    async fn record(
        &self,
        kind: &str,
        key: &str,
        payload: Option<&str>,
    ) -> Result<(), ReplicaError> {
        self.write(kind, key, payload).await
    }

    fn in_recovery(&self) -> bool {
        Replicator::in_recovery(self)
    }
}
