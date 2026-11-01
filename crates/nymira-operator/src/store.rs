//! SQLite store: one row per nullifier with a state column, and the lease records.
//!
//! Durability settings are the spec's: `synchronous=FULL`, `fullfsync=ON`. Every transition is a
//! compare-and-set inside one SQL transaction; the operator additionally serialises them under
//! one lock that stays held across the replica acknowledgement.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};

/// Nullifier states.
pub mod state {
    pub const RESERVED: &str = "reserved";
    pub const FINALIZED: &str = "finalized";
    pub const CLEARANCE_RESERVED: &str = "clearance_reserved";
    pub const EXIT: &str = "exit";
}

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NullifierRow {
    pub state: String,
    pub client_request_id: Option<String>,
    pub lease_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NewLease {
    pub nullifier: String,
    pub client_request_id: String,
    pub lease_id: String,
    pub token_hash: Vec<u8>,
    pub bound: u128,
    pub request_json: String,
    pub reserved_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseRow {
    pub lease_id: String,
    pub nullifier: String,
    pub token_hash: Vec<u8>,
    pub key_id: Option<String>,
    pub bound: u128,
    pub status: String,
    pub issued_at: Option<u64>,
    pub expires_at: Option<u64>,
    pub replicated: bool,
    pub client_request_id: String,
    pub delivered: bool,
    pub last_activity: Option<u64>,
    pub retiring_at: Option<u64>,
    pub revoked_at: Option<u64>,
    pub usage_json: Option<String>,
    pub charge: Option<u128>,
    pub charge_basis: Option<String>,
    pub settled_at: Option<u64>,
    pub evidence_json: Option<String>,
}

/// Outcome of a compare-and-set on a nullifier.
#[derive(Debug, PartialEq, Eq)]
pub enum Cas {
    Done,
    /// The row exists in another state.
    Conflict(NullifierRow),
}

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS nullifiers(
  nullifier TEXT PRIMARY KEY,
  state TEXT NOT NULL,
  client_request_id TEXT,
  lease_id TEXT,
  updated_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS leases(
  lease_id TEXT PRIMARY KEY,
  nullifier TEXT NOT NULL UNIQUE,
  token_hash BLOB NOT NULL,
  key_id TEXT,
  bound TEXT NOT NULL,
  status TEXT NOT NULL,
  issued_at INTEGER,
  expires_at INTEGER,
  replicated INTEGER NOT NULL DEFAULT 0,
  request_json TEXT NOT NULL,
  reserved_at INTEGER NOT NULL,
  client_request_id TEXT NOT NULL DEFAULT '',
  delivered INTEGER NOT NULL DEFAULT 0,
  last_activity INTEGER,
  retiring_at INTEGER,
  revoked_at INTEGER,
  usage_json TEXT,
  charge TEXT,
  charge_basis TEXT,
  settled_at INTEGER,
  evidence_json TEXT
);
CREATE TABLE IF NOT EXISTS signed_states(
  lease_id TEXT PRIMARY KEY,
  nullifier TEXT NOT NULL UNIQUE,
  client_request_id TEXT NOT NULL,
  charge TEXT NOT NULL,
  response_json TEXT NOT NULL,
  signed_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS exit_seen(nullifier TEXT PRIMARY KEY);";

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA fullfsync=ON;
             PRAGMA checkpoint_fullfsync=ON; PRAGMA busy_timeout=5000;",
        )?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Runs `f` on the connection off the async runtime.
    pub async fn run<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> rusqlite::Result<T> {
        let conn = self.conn.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = conn.lock().unwrap_or_else(|e| e.into_inner());
            f(&mut conn)
        })
        .await
        .expect("store task does not panic")
    }

    /// `(synchronous, fullfsync)` as SQLite reports them: 2 is FULL.
    pub fn durability(&self) -> rusqlite::Result<(i64, i64)> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        Ok((
            conn.query_row("PRAGMA synchronous", [], |r| r.get(0))?,
            conn.query_row("PRAGMA fullfsync", [], |r| r.get(0))?,
        ))
    }
}

fn row(conn: &Connection, nullifier: &str) -> rusqlite::Result<Option<NullifierRow>> {
    conn.query_row(
        "SELECT state, client_request_id, lease_id FROM nullifiers WHERE nullifier = ?1",
        [nullifier],
        |r| {
            Ok(NullifierRow {
                state: r.get(0)?,
                client_request_id: r.get(1)?,
                lease_id: r.get(2)?,
            })
        },
    )
    .optional()
}

pub fn nullifier_row(conn: &Connection, nullifier: &str) -> rusqlite::Result<Option<NullifierRow>> {
    row(conn, nullifier)
}

/// Reserves a nullifier for a lease and writes the lease record, in one transaction. A row that
/// exists in any state is a conflict.
pub fn reserve_lease(conn: &mut Connection, lease: &NewLease) -> rusqlite::Result<Cas> {
    let tx = conn.transaction()?;
    if let Some(existing) = row(&tx, &lease.nullifier)? {
        return Ok(Cas::Conflict(existing));
    }
    tx.execute(
        "INSERT INTO nullifiers(nullifier, state, client_request_id, lease_id, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            lease.nullifier,
            state::RESERVED,
            lease.client_request_id,
            lease.lease_id,
            lease.reserved_at
        ],
    )?;
    tx.execute(
        "INSERT INTO leases(lease_id, nullifier, token_hash, bound, status, request_json, reserved_at,
                            client_request_id)
         VALUES (?1, ?2, ?3, ?4, 'provisioning', ?5, ?6, ?7)",
        params![
            lease.lease_id,
            lease.nullifier,
            lease.token_hash,
            lease.bound.to_string(),
            lease.request_json,
            lease.reserved_at,
            lease.client_request_id
        ],
    )?;
    tx.commit()?;
    Ok(Cas::Done)
}

/// Moves an unseen nullifier into `to` (an exit import or a clearance reservation). An existing
/// row in a different state is a conflict; the same state again is `Done`.
pub fn claim_nullifier(
    conn: &mut Connection,
    nullifier: &str,
    to: &str,
    now: u64,
) -> rusqlite::Result<Cas> {
    let tx = conn.transaction()?;
    match row(&tx, nullifier)? {
        Some(existing) if existing.state == to => return Ok(Cas::Done),
        Some(existing) => return Ok(Cas::Conflict(existing)),
        None => {}
    }
    tx.execute(
        "INSERT INTO nullifiers(nullifier, state, updated_at) VALUES (?1, ?2, ?3)",
        params![nullifier, to, now],
    )?;
    tx.commit()?;
    Ok(Cas::Done)
}

pub fn activate_lease(
    conn: &mut Connection,
    lease_id: &str,
    key_id: &str,
    issued_at: u64,
    expires_at: u64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE leases SET status = 'active', key_id = ?2, issued_at = ?3, expires_at = ?4,
                last_activity = ?3
         WHERE lease_id = ?1",
        params![lease_id, key_id, issued_at, expires_at],
    )?;
    Ok(())
}

pub fn mark_replicated(conn: &mut Connection, lease_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE leases SET replicated = 1 WHERE lease_id = ?1",
        [lease_id],
    )?;
    Ok(())
}

/// Deletes the reservation and its lease: the nullifier is unbound again.
pub fn roll_back(conn: &mut Connection, lease_id: &str) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute(
        "DELETE FROM nullifiers WHERE lease_id = ?1 AND state = 'reserved'",
        [lease_id],
    )?;
    tx.execute("DELETE FROM leases WHERE lease_id = ?1", [lease_id])?;
    tx.commit()
}

fn lease_from(r: &rusqlite::Row<'_>) -> rusqlite::Result<LeaseRow> {
    let bound: String = r.get(4)?;
    Ok(LeaseRow {
        lease_id: r.get(0)?,
        nullifier: r.get(1)?,
        token_hash: r.get(2)?,
        key_id: r.get(3)?,
        bound: bound.parse().unwrap_or(0),
        status: r.get(5)?,
        issued_at: r.get(6)?,
        expires_at: r.get(7)?,
        replicated: r.get::<_, i64>(8)? != 0,
        client_request_id: r.get(9)?,
        delivered: r.get::<_, i64>(10)? != 0,
        last_activity: r.get(11)?,
        retiring_at: r.get(12)?,
        revoked_at: r.get(13)?,
        usage_json: r.get(14)?,
        charge: r.get::<_, Option<String>>(15)?.and_then(|c| c.parse().ok()),
        charge_basis: r.get(16)?,
        settled_at: r.get(17)?,
        evidence_json: r.get(18)?,
    })
}

const LEASE_COLUMNS: &str = "lease_id, nullifier, token_hash, key_id, bound, status, issued_at, \
     expires_at, replicated, client_request_id, delivered, last_activity, retiring_at, revoked_at, \
     usage_json, charge, charge_basis, settled_at, evidence_json";

impl LeaseRow {
    /// Seconds the key was live: issue to deactivation.
    pub fn held_secs(&self) -> u64 {
        self.revoked_at
            .unwrap_or(0)
            .saturating_sub(self.issued_at.unwrap_or(0))
    }
}

pub fn lease(conn: &Connection, lease_id: &str) -> rusqlite::Result<Option<LeaseRow>> {
    conn.query_row(
        &format!("SELECT {LEASE_COLUMNS} FROM leases WHERE lease_id = ?1"),
        [lease_id],
        lease_from,
    )
    .optional()
}

pub fn lease_by_nullifier(
    conn: &Connection,
    nullifier: &str,
) -> rusqlite::Result<Option<LeaseRow>> {
    conn.query_row(
        &format!("SELECT {LEASE_COLUMNS} FROM leases WHERE nullifier = ?1"),
        [nullifier],
        lease_from,
    )
    .optional()
}

pub fn all_leases(conn: &Connection) -> rusqlite::Result<Vec<LeaseRow>> {
    let mut statement = conn.prepare(&format!("SELECT {LEASE_COLUMNS} FROM leases"))?;
    let rows = statement.query_map([], lease_from)?;
    rows.collect()
}

pub fn request_json(conn: &Connection, lease_id: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT request_json FROM leases WHERE lease_id = ?1",
        [lease_id],
        |r| r.get(0),
    )
    .optional()
}

pub fn count_nullifiers(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM nullifiers", [], |r| r.get(0))
}

/// Moves a lease from one status to the next and sets one timestamp column in the same
/// statement; false when the lease is not in `from` (another caller moved it first).
pub fn advance(
    conn: &Connection,
    lease_id: &str,
    from: &str,
    to: &str,
    time_column: &str,
    now: u64,
) -> rusqlite::Result<bool> {
    // The column name is spliced into the statement: only these four are ever allowed.
    assert!(["retiring_at", "revoked_at", "settled_at", "last_activity"].contains(&time_column));
    let changed = conn.execute(
        &format!(
            "UPDATE leases SET status = ?3, {time_column} = ?4 WHERE lease_id = ?1 AND status = ?2"
        ),
        params![lease_id, from, to, now],
    )?;
    Ok(changed == 1)
}

/// Records the first request proxied under a lease (delivery) and its activity time.
pub fn mark_delivered(conn: &Connection, lease_id: &str, now: u64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE leases SET delivered = 1, last_activity = ?2 WHERE lease_id = ?1",
        params![lease_id, now],
    )?;
    Ok(())
}

/// The signed usage record, stored when the lease is retired.
pub fn store_usage(conn: &Connection, lease_id: &str, usage_json: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE leases SET usage_json = ?2, status = 'retired' WHERE lease_id = ?1 AND status = 'revoked'",
        params![lease_id, usage_json],
    )?;
    Ok(())
}

/// Settlement: the charge, how it was fixed, and the nullifier turning `finalized`, in one
/// transaction. False when the lease or the nullifier was not in the expected state.
pub fn settle(
    conn: &mut Connection,
    lease_id: &str,
    charge: u128,
    basis: &str,
    now: u64,
) -> rusqlite::Result<bool> {
    let tx = conn.transaction()?;
    let moved = tx.execute(
        "UPDATE leases SET status = 'settled', charge = ?2, charge_basis = ?3, settled_at = ?4
         WHERE lease_id = ?1 AND status = 'retired'",
        params![lease_id, charge.to_string(), basis, now],
    )?;
    let finalized = tx.execute(
        "UPDATE nullifiers SET state = 'finalized', updated_at = ?2
         WHERE lease_id = ?1 AND state = 'reserved'",
        params![lease_id, now],
    )?;
    if moved != 1 || finalized != 1 {
        return Ok(false);
    }
    tx.commit()?;
    Ok(true)
}

/// The lease of a client request id; none when no lease or more than one carries it (two
/// clients that picked the same id must not read each other's state).
pub fn lease_by_client_request_id(
    conn: &Connection,
    id: &str,
) -> rusqlite::Result<Option<LeaseRow>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {LEASE_COLUMNS} FROM leases WHERE client_request_id = ?1 LIMIT 2"
    ))?;
    let mut rows: Vec<LeaseRow> = statement
        .query_map([id], lease_from)?
        .collect::<Result<_, _>>()?;
    Ok(if rows.len() == 1 { rows.pop() } else { None })
}

pub fn record_exit_seen(conn: &Connection, nullifier: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO exit_seen(nullifier) VALUES (?1)",
        [nullifier],
    )?;
    Ok(())
}

pub fn is_exit_seen(conn: &Connection, nullifier: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM exit_seen WHERE nullifier = ?1",
        [nullifier],
        |_| Ok(()),
    )
    .optional()
    .map(|r| r.is_some())
}

/// The stored signed state of a lease: `(client_request_id, charge, response_json)`.
pub fn signed_state(
    conn: &Connection,
    lease_id: &str,
) -> rusqlite::Result<Option<(String, u128, String)>> {
    conn.query_row(
        "SELECT client_request_id, charge, response_json FROM signed_states WHERE lease_id = ?1",
        [lease_id],
        |r| {
            let charge: String = r.get(1)?;
            Ok((r.get(0)?, charge.parse().unwrap_or(0), r.get(2)?))
        },
    )
    .optional()
}

/// Writes a signed state; false when one exists already for the lease or the nullifier.
pub fn insert_signed_state(
    conn: &Connection,
    lease_id: &str,
    nullifier: &str,
    client_request_id: &str,
    charge: u128,
    response_json: &str,
    now: u64,
) -> rusqlite::Result<bool> {
    let changed = conn.execute(
        "INSERT OR IGNORE INTO signed_states(lease_id, nullifier, client_request_id, charge,
                                             response_json, signed_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            lease_id,
            nullifier,
            client_request_id,
            charge.to_string(),
            response_json,
            now
        ],
    )?;
    Ok(changed == 1)
}

/// Charges settled at or after `since` (Unix seconds): the day's spend, restored at start-up.
pub fn settled_since(conn: &Connection, since: u64) -> rusqlite::Result<u128> {
    let mut statement =
        conn.prepare("SELECT charge FROM leases WHERE status = 'settled' AND settled_at >= ?1")?;
    let rows = statement.query_map([since], |r| r.get::<_, String>(0))?;
    let mut total = 0u128;
    for charge in rows {
        // A charge that does not parse must not read as 0: that would under-count the day.
        total += charge?
            .parse::<u128>()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
    }
    Ok(total)
}

/// The provider's signature and attestation report for a lease, kept for its holder.
pub fn store_evidence(conn: &Connection, lease_id: &str, json: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE leases SET evidence_json = ?2 WHERE lease_id = ?1",
        params![lease_id, json],
    )?;
    Ok(())
}
