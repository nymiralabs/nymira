//! Applies a replicated fact to a database. The replica does it as entries arrive, so its
//! store always shows what the second watcher needs; a restoring laptop does it for the entries
//! it missed.
//!
//! A lease restored from a reservation has no token and no provider key: its status is
//! `restored`, and it can only be settled at its bound (by the client's `POST /v2/reconcile`,
//! or by the operator six hours after the restore). Nothing here signs.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::replog::{kinds, Entry};
use crate::store::{self, state};

/// The status of a lease that came back from the replica.
pub const RESTORED: &str = "restored";

/// Applies one entry. Applying it twice changes nothing, and a payload that does not parse is
/// skipped: the chain stays whole either way.
pub fn apply(conn: &Connection, e: &Entry) -> rusqlite::Result<()> {
    match e.kind.as_str() {
        kinds::RESERVATION => {
            let Some(p) = e
                .payload
                .as_deref()
                .and_then(|p| serde_json::from_str::<Value>(p).ok())
            else {
                return Ok(());
            };
            let text = |k: &str| p[k].as_str().map(str::to_string);
            let (Some(lease_id), Some(nullifier), Some(request)) =
                (text("lease_id"), text("nullifier"), text("request_json"))
            else {
                return Ok(());
            };
            let client_request_id = text("client_request_id").unwrap_or_default();
            let bound = text("bound").unwrap_or_else(|| "0".into());
            let at = p["reserved_at"].as_u64().unwrap_or(e.hour);
            conn.execute(
                "INSERT OR IGNORE INTO nullifiers(nullifier, state, client_request_id, lease_id,
                                                  updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![nullifier, state::RESERVED, client_request_id, lease_id, at],
            )?;
            conn.execute(
                "INSERT OR IGNORE INTO leases(lease_id, nullifier, token_hash, bound, status,
                                              request_json, reserved_at, client_request_id,
                                              replicated)
                 VALUES (?1, ?2, X'', ?3, ?4, ?5, ?6, ?7, 1)",
                params![
                    lease_id,
                    nullifier,
                    bound,
                    RESTORED,
                    request,
                    at,
                    client_request_id
                ],
            )?;
        }
        kinds::DELIVERED => {
            conn.execute(
                "UPDATE leases SET delivered = 1 WHERE lease_id = ?1",
                [&e.key],
            )?;
        }
        kinds::SIGNED => {
            let Some(json) = e.payload.as_deref() else {
                return Ok(());
            };
            let Ok(response) = serde_json::from_str::<Value>(json) else {
                return Ok(());
            };
            let charge = response["charge_applied"].as_str().unwrap_or("0");
            let lease = store::lease(conn, &e.key)?;
            let (Some(lease), Ok(charge)) = (lease, charge.parse::<u128>()) else {
                return Ok(());
            };
            conn.execute(
                "INSERT OR IGNORE INTO signed_states(lease_id, nullifier, client_request_id,
                                                     charge, response_json, signed_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    e.key,
                    lease.nullifier,
                    response["client_request_id"].as_str().unwrap_or(""),
                    charge.to_string(),
                    json,
                    e.hour
                ],
            )?;
        }
        kinds::SETTLED => {
            let fact = e
                .payload
                .as_deref()
                .and_then(|p| serde_json::from_str::<Value>(p).ok());
            let charge = fact
                .as_ref()
                .and_then(|f| f["charge"].as_str())
                .unwrap_or("0");
            let basis = fact
                .as_ref()
                .and_then(|f| f["basis"].as_str())
                .unwrap_or("recovered");
            conn.execute(
                "UPDATE leases SET status = 'settled', charge = ?2, charge_basis = ?3,
                        settled_at = ?4 WHERE lease_id = ?1",
                params![e.key, charge, basis, e.hour],
            )?;
            conn.execute(
                "UPDATE nullifiers SET state = 'finalized', updated_at = ?2
                 WHERE lease_id = ?1 AND state = 'reserved'",
                params![e.key, e.hour],
            )?;
            // Settled at the bound while the replica was away: any state it holds for this
            // lease was never released and is dropped.
            if basis == "link_down" {
                conn.execute("DELETE FROM signed_states WHERE lease_id = ?1", [&e.key])?;
            }
        }
        kinds::CLEARANCE => {
            conn.execute(
                "INSERT OR IGNORE INTO nullifiers(nullifier, state, updated_at)
                 VALUES (?1, ?2, ?3)",
                params![e.key, state::CLEARANCE_RESERVED, e.hour],
            )?;
        }
        kinds::ROLLED_BACK => {
            let held: Option<String> = conn
                .query_row(
                    "SELECT status FROM leases WHERE lease_id = ?1",
                    [&e.key],
                    |r| r.get(0),
                )
                .optional()?;
            if held.is_some_and(|s| s == RESTORED) {
                conn.execute(
                    "DELETE FROM nullifiers WHERE lease_id = ?1 AND state = 'reserved'",
                    [&e.key],
                )?;
                conn.execute("DELETE FROM leases WHERE lease_id = ?1", [&e.key])?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Settles a restored lease at its bound with no next state. False when it is not restored.
pub fn settle_restored(
    conn: &mut Connection,
    lease_id: &str,
    basis: &str,
    now: u64,
) -> rusqlite::Result<bool> {
    let tx = conn.transaction()?;
    let moved = tx.execute(
        "UPDATE leases SET status = 'settled', charge = bound, charge_basis = ?2, settled_at = ?3
         WHERE lease_id = ?1 AND status = ?4",
        params![lease_id, basis, now, RESTORED],
    )?;
    if moved != 1 {
        return Ok(false);
    }
    tx.execute(
        "UPDATE nullifiers SET state = 'finalized', updated_at = ?2
         WHERE lease_id = ?1 AND state = 'reserved'",
        params![lease_id, now],
    )?;
    tx.commit()?;
    Ok(true)
}
