//! The replication log: a numbered, hash-chained, append-only list of the facts the second
//! machine needs to answer exits and to restore the operator alone.
//!
//! Both machines keep the same table. An entry holds the hash of its payload, so the payload can
//! be deleted at its retention time and the chain still checks. Times are rounded to the hour.
//! The replica ([`ReplicaNode`]) acknowledges an entry only after it is on disk, and signs the
//! acknowledgement with a key only it holds.

use async_trait::async_trait;
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use rand::Rng;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::store::Store;

pub const HOUR: u64 = 3_600;

/// What an entry says. The payload of `RESERVATION` is the lease record the watcher needs.
pub mod kinds {
    pub const RESERVATION: &str = "reservation";
    pub const DELIVERED: &str = "delivered";
    pub const SETTLED: &str = "settled";
    pub const CLEARANCE: &str = "clearance";
    /// The signed next state of a lease: charge, how it was fixed and the response.
    pub const SIGNED: &str = "signed";
    /// A reservation that was undone before any key was minted: its request proof goes.
    pub const ROLLED_BACK: &str = "rolled_back";
}

pub(crate) const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS replog(
  seq INTEGER PRIMARY KEY,
  prev TEXT NOT NULL,
  kind TEXT NOT NULL,
  key TEXT NOT NULL,
  hour INTEGER NOT NULL,
  payload_hash TEXT NOT NULL,
  payload TEXT,
  hash TEXT NOT NULL,
  acked INTEGER NOT NULL DEFAULT 1
);
CREATE INDEX IF NOT EXISTS replog_key ON replog(key);";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub prev: String,
    pub kind: String,
    /// The lease id or nullifier the entry is about.
    pub key: String,
    pub hour: u64,
    pub payload_hash: String,
    pub payload: Option<String>,
    pub hash: String,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
pub enum LogError {
    /// The entry skips ahead: the log's head is `head`.
    #[error("entries are missing: the head is {head}")]
    Gap { head: u64 },
    /// The entry disagrees with the one held at its place.
    #[error("the chain forks at {seq}")]
    Fork { seq: u64 },
    #[error("bad entry: {0}")]
    Bad(String),
    #[error("log store: {0}")]
    Db(String),
}

impl From<rusqlite::Error> for LogError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(e.to_string())
    }
}

fn genesis() -> String {
    "0".repeat(64)
}

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn payload_hash(payload: Option<&str>) -> String {
    sha(payload.unwrap_or("").as_bytes())
}

fn entry_hash(e: &Entry) -> String {
    let mut h = Sha256::new();
    h.update(b"nymira/replog/v1\0");
    h.update(e.seq.to_be_bytes());
    h.update(e.prev.as_bytes());
    for part in [&e.kind, &e.key, &e.payload_hash] {
        h.update((part.len() as u32).to_be_bytes());
        h.update(part.as_bytes());
    }
    h.update(e.hour.to_be_bytes());
    hex::encode(h.finalize())
}

/// The entry's own consistency: its hash and, while the payload is still there, the payload hash.
fn check(e: &Entry) -> Result<(), LogError> {
    if let Some(p) = &e.payload {
        if payload_hash(Some(p)) != e.payload_hash {
            return Err(LogError::Bad("payload does not match its hash".into()));
        }
    }
    if entry_hash(e) != e.hash {
        return Err(LogError::Bad("entry does not match its hash".into()));
    }
    Ok(())
}

fn head_of(conn: &Connection) -> rusqlite::Result<(u64, String)> {
    Ok(conn
        .query_row(
            "SELECT seq, hash FROM replog ORDER BY seq DESC LIMIT 1",
            [],
            |r| Ok((r.get::<_, i64>(0)? as u64, r.get(1)?)),
        )
        .optional()?
        .unwrap_or((0, genesis())))
}

fn entry_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<Entry> {
    Ok(Entry {
        seq: r.get::<_, i64>(0)? as u64,
        prev: r.get(1)?,
        kind: r.get(2)?,
        key: r.get(3)?,
        hour: r.get::<_, i64>(4)? as u64,
        payload_hash: r.get(5)?,
        payload: r.get(6)?,
        hash: r.get(7)?,
    })
}

const COLUMNS: &str = "seq, prev, kind, key, hour, payload_hash, payload, hash";

/// Appends `e` if it is the next entry, accepts it again if it is already there, refuses it
/// otherwise.
fn insert(conn: &mut Connection, e: &Entry, acked: bool, apply: bool) -> Result<(), LogError> {
    check(e)?;
    let tx = conn.transaction()?;
    let (head, head_hash) = head_of(&tx)?;
    if e.seq <= head {
        let held: Option<String> = tx
            .query_row(
                "SELECT hash FROM replog WHERE seq = ?1",
                [e.seq as i64],
                |r| r.get(0),
            )
            .optional()?;
        return match held {
            Some(h) if h == e.hash => Ok(()),
            _ => Err(LogError::Fork { seq: e.seq }),
        };
    }
    if e.seq != head + 1 {
        return Err(LogError::Gap { head });
    }
    if e.prev != head_hash {
        return Err(LogError::Fork { seq: e.seq });
    }
    tx.execute(
        "INSERT INTO replog(seq, prev, kind, key, hour, payload_hash, payload, hash, acked)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            e.seq as i64,
            e.prev,
            e.kind,
            e.key,
            e.hour as i64,
            e.payload_hash,
            e.payload,
            e.hash,
            acked as i64
        ],
    )?;
    if apply {
        crate::restore::apply(&tx, e)?;
    }
    if e.kind == kinds::ROLLED_BACK {
        tx.execute(
            "UPDATE replog SET payload = NULL WHERE key = ?1 AND payload IS NOT NULL",
            [&e.key],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// One machine's copy of the log.
#[derive(Clone)]
pub struct Log {
    store: Store,
}

impl Log {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    /// `(seq, hash)` of the last entry; `(0, 64 zeros)` for an empty log.
    pub async fn head(&self) -> Result<(u64, String), LogError> {
        Ok(self.store.run(|c| head_of(c)).await?)
    }

    /// Builds the next entry and stores it. `acked` is false on the laptop until the replica
    /// has confirmed it.
    pub async fn append(
        &self,
        kind: &str,
        key: &str,
        now_secs: u64,
        payload: Option<&str>,
        acked: bool,
    ) -> Result<Entry, LogError> {
        let (kind, key, payload) = (kind.to_string(), key.to_string(), payload.map(String::from));
        let hour = now_secs / HOUR * HOUR;
        self.store
            .run(move |c| {
                let (seq, prev) = head_of(c)?;
                let mut e = Entry {
                    seq: seq + 1,
                    prev,
                    kind,
                    key,
                    hour,
                    payload_hash: payload_hash(payload.as_deref()),
                    payload,
                    hash: String::new(),
                };
                e.hash = entry_hash(&e);
                Ok(insert(c, &e, acked, false).map(|()| e))
            })
            .await?
    }

    /// Stores an entry that came from the other machine (already acknowledged by definition)
    /// and applies it to the database in the same transaction.
    pub async fn accept(&self, entry: &Entry) -> Result<(), LogError> {
        let e = entry.clone();
        self.store
            .run(move |c| Ok(insert(c, &e, true, true)))
            .await?
    }

    /// True when an entry of this kind about this key is in the log.
    pub async fn has(&self, kind: &str, key: &str) -> Result<bool, LogError> {
        let (kind, key) = (kind.to_string(), key.to_string());
        Ok(self
            .store
            .run(move |c| {
                c.query_row(
                    "SELECT 1 FROM replog WHERE kind = ?1 AND key = ?2 LIMIT 1",
                    [kind, key],
                    |_| Ok(()),
                )
                .optional()
                .map(|r| r.is_some())
            })
            .await?)
    }

    pub async fn hash_at(&self, seq: u64) -> Result<Option<String>, LogError> {
        Ok(self
            .store
            .run(move |c| {
                c.query_row(
                    "SELECT hash FROM replog WHERE seq = ?1",
                    [seq as i64],
                    |r| r.get(0),
                )
                .optional()
            })
            .await?)
    }

    /// Up to `limit` entries from `seq` on.
    pub async fn from(&self, seq: u64, limit: u32) -> Result<Vec<Entry>, LogError> {
        Ok(self
            .store
            .run(move |c| {
                let mut s = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM replog WHERE seq >= ?1 ORDER BY seq LIMIT ?2"
                ))?;
                let rows = s.query_map(params![seq as i64, limit], entry_from)?;
                rows.collect()
            })
            .await?)
    }

    /// Entries the replica has not confirmed, oldest first.
    pub async fn unacked(&self) -> Result<Vec<Entry>, LogError> {
        Ok(self
            .store
            .run(|c| {
                let mut s = c.prepare(&format!(
                    "SELECT {COLUMNS} FROM replog WHERE acked = 0 ORDER BY seq"
                ))?;
                let rows = s.query_map([], entry_from)?;
                rows.collect()
            })
            .await?)
    }

    /// Marks every entry up to `seq` confirmed.
    pub async fn mark_acked(&self, seq: u64) -> Result<(), LogError> {
        Ok(self
            .store
            .run(move |c| {
                c.execute("UPDATE replog SET acked = 1 WHERE seq <= ?1", [seq as i64])
                    .map(|_| ())
            })
            .await?)
    }

    /// Drops the payload of every entry about `key`; the hashes stay. Returns how many.
    pub async fn redact(&self, key: &str) -> Result<usize, LogError> {
        let key = key.to_string();
        Ok(self
            .store
            .run(move |c| {
                c.execute(
                    "UPDATE replog SET payload = NULL WHERE key = ?1 AND payload IS NOT NULL",
                    [key],
                )
            })
            .await?)
    }

    /// Walks the whole chain: numbering, links and every entry's own hash.
    pub async fn verify(&self) -> Result<(), LogError> {
        let entries = self.from(1, u32::MAX).await?;
        let mut prev = genesis();
        for (i, e) in entries.iter().enumerate() {
            check(e)?;
            if e.seq != i as u64 + 1 || e.prev != prev {
                return Err(LogError::Fork { seq: e.seq });
            }
            prev = e.hash.clone();
        }
        Ok(())
    }
}

/// The head of a replica's log, signed by the replica's key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedHead {
    pub seq: u64,
    pub head: String,
    pub sig: String,
}

fn head_message(seq: u64, head: &str) -> Vec<u8> {
    let mut m = b"nymira/replica-ack/v1\0".to_vec();
    m.extend_from_slice(&seq.to_be_bytes());
    m.extend_from_slice(head.as_bytes());
    m
}

/// True when `h` was signed by the key `pk`.
pub fn verify_head(pk: &[u8; 32], h: &SignedHead) -> bool {
    let Ok(key) = VerifyingKey::from_bytes(pk) else {
        return false;
    };
    let Ok(sig) = hex::decode(&h.sig) else {
        return false;
    };
    let Ok(sig) = Signature::from_slice(&sig) else {
        return false;
    };
    key.verify(&head_message(h.seq, &h.head), &sig).is_ok()
}

/// What a link to the replica can fail with.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// The replica answered and said no.
    #[error("the replica refused: {0}")]
    Refused(LogError),
    /// No answer, or one that does not verify.
    #[error("replica unreachable: {0}")]
    Down(String),
}

/// How the laptop talks to the replica: in-process in tests, over HTTP on the tailnet.
#[async_trait]
pub trait Link: Send + Sync {
    async fn append(&self, entry: &Entry) -> Result<SignedHead, LinkError>;
    async fn head(&self) -> Result<SignedHead, LinkError>;
    async fn hashes(&self, seqs: &[u64]) -> Result<Vec<Option<String>>, LinkError>;
    async fn fetch(&self, from: u64, limit: u32) -> Result<Vec<Entry>, LinkError>;
}

/// The second machine's end: a log and the key that signs its acknowledgements.
pub struct ReplicaNode {
    log: Log,
    key: SigningKey,
}

impl ReplicaNode {
    pub fn new(log: Log, key: SigningKey) -> Self {
        Self { log, key }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    pub fn log(&self) -> &Log {
        &self.log
    }

    fn sign(&self, seq: u64, head: String) -> SignedHead {
        let sig = hex::encode(self.key.sign(&head_message(seq, &head)).to_bytes());
        SignedHead { seq, head, sig }
    }

    /// Stores the entry, then signs the head. The SQLite settings sync before `accept` returns.
    pub async fn append(&self, entry: &Entry) -> Result<SignedHead, LogError> {
        self.log.accept(entry).await?;
        Ok(self.head().await)
    }

    pub async fn head(&self) -> SignedHead {
        let (seq, head) = self.log.head().await.unwrap_or((0, genesis()));
        self.sign(seq, head)
    }

    pub async fn hashes(&self, seqs: &[u64]) -> Result<Vec<Option<String>>, LogError> {
        let mut out = Vec::with_capacity(seqs.len());
        for s in seqs {
            out.push(self.log.hash_at(*s).await?);
        }
        Ok(out)
    }

    pub async fn from(&self, seq: u64, limit: u32) -> Result<Vec<Entry>, LogError> {
        self.log.from(seq, limit).await
    }

    pub async fn redact(&self, key: &str) -> Result<usize, LogError> {
        self.log.redact(key).await
    }

    pub async fn verify(&self) -> Result<(), LogError> {
        self.log.verify().await
    }
}

#[async_trait]
impl Link for ReplicaNode {
    async fn append(&self, entry: &Entry) -> Result<SignedHead, LinkError> {
        ReplicaNode::append(self, entry)
            .await
            .map_err(LinkError::Refused)
    }
    async fn head(&self) -> Result<SignedHead, LinkError> {
        Ok(ReplicaNode::head(self).await)
    }
    async fn hashes(&self, seqs: &[u64]) -> Result<Vec<Option<String>>, LinkError> {
        ReplicaNode::hashes(self, seqs)
            .await
            .map_err(LinkError::Refused)
    }
    async fn fetch(&self, from: u64, limit: u32) -> Result<Vec<Entry>, LinkError> {
        ReplicaNode::from(self, from, limit)
            .await
            .map_err(LinkError::Refused)
    }
}

/// What a challenge found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Audit {
    /// Same head, and every asked entry agrees.
    Match,
    /// The replica holds a shorter chain that agrees with ours as far as it goes: it lost
    /// entries it once acknowledged (an old snapshot) or never had them.
    Behind { replica: u64, local: u64 },
    /// The replica holds entries we do not: our log is the one that lost data.
    Ahead { replica: u64, local: u64 },
    /// An entry at `seq` differs.
    Diverged { seq: u64 },
}

/// Asks the replica for its signed head and for the hashes of `samples` random entries below
/// both heads (and always the last one both hold), and compares them with the local log. A head
/// that does not verify against `pk` is `Down`.
pub async fn audit(
    local: &Log,
    replica: &dyn Link,
    pk: &[u8; 32],
    samples: usize,
) -> Result<Audit, LinkError> {
    let head = replica.head().await?;
    if !verify_head(pk, &head) {
        return Err(LinkError::Down(
            "the head is not signed by the pinned key".into(),
        ));
    }
    let (mine, _) = local.head().await.map_err(LinkError::Refused)?;
    let common = mine.min(head.seq);
    if common > 0 {
        let mut asked = vec![common];
        {
            // Dropped before the next await: the generator is not `Send`.
            let mut rng = rand::thread_rng();
            for _ in 0..samples {
                asked.push(rng.gen_range(1..=common));
            }
        }
        asked.sort_unstable();
        asked.dedup();
        let theirs = replica.hashes(&asked).await?;
        for (seq, theirs) in asked.iter().zip(theirs) {
            let ours = local.hash_at(*seq).await.map_err(LinkError::Refused)?;
            if ours != theirs {
                return Ok(Audit::Diverged { seq: *seq });
            }
        }
    }
    Ok(match head.seq.cmp(&mine) {
        std::cmp::Ordering::Equal => Audit::Match,
        std::cmp::Ordering::Less => Audit::Behind {
            replica: head.seq,
            local: mine,
        },
        std::cmp::Ordering::Greater => Audit::Ahead {
            replica: head.seq,
            local: mine,
        },
    })
}
