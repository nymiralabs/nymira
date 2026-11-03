//! Restoring from the note secret alone.
//!
//! The secret and the note ID are enough to rebuild the note's first state, check it against the
//! vault's leaf, and exit. They are not enough to keep spending: the wallet's later states are
//! gone. The note is stored as `Exiting`, so no request is ever made on it; `nymirad exit` then
//! files the exit with concession 0.

use std::path::Path;

use serde_json::{json, Value};
use zkapi_types::Felt252;

use crate::rpc::{Rpc, RpcError};
use crate::secrets::{NoteEntry, NoteStatus, NoteStore, StoreError};
use crate::snapshot::{self, SnapshotError};

#[derive(Debug, thiserror::Error)]
pub enum SecretRestoreError {
    #[error("the secret must be 64 hex digits")]
    BadSecret,
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("the vault has no note {0}")]
    NoNote(u32),
    #[error("note {0} is not this secret's note (its leaf in the vault's tree differs), or it is not in the tree yet; check the note ID and wait for the next sync_root")]
    NotThisNote(u32),
    #[error("note {0} is {1} in the vault: nothing to restore")]
    NotActive(u32, String),
    #[error("this machine already knows that note")]
    Known,
}

/// Rebuilds the entry of note `note_id` from its secret and stores it, sealed under `passphrase`.
/// `payout` is the account named at deposit: the vault does not publish it, and an exit to a
/// different one is refused by the vault.
#[allow(clippy::too_many_arguments)]
pub async fn from_secret(
    home: &Path,
    rpc: &Rpc,
    vault: &str,
    chain_label: &str,
    secret_hex: &str,
    note_id: u32,
    payout: &str,
    passphrase: &str,
    rounds: u32,
    now_secs: u64,
) -> Result<NoteEntry, SecretRestoreError> {
    let secret = Felt252::from_hex(secret_hex.trim()).map_err(|_| SecretRestoreError::BadSecret)?;
    let commitment = zkapi_core::v2::registration_commitment(&secret);
    let commitment_hex = format!("0x{}", hex::encode(commitment.0));

    let page = rpc
        .view(
            vault,
            "note_records",
            json!({ "from": note_id, "limit": 1 }),
        )
        .await?;
    let record: &Value = page
        .as_array()
        .and_then(|p| p.first())
        .filter(|r| r["note_id"] == note_id)
        .ok_or(SecretRestoreError::NoNote(note_id))?;
    let amount: u128 = record["deposit_amount"]
        .as_str()
        .and_then(|a| a.parse().ok())
        .ok_or(SnapshotError::Malformed("deposit_amount"))?;
    let expiry = record["expiry"]
        .as_u64()
        .ok_or(SnapshotError::Malformed("expiry"))?;
    let status = record["status"].as_str().unwrap_or("unknown");
    if status != "Active" {
        return Err(SecretRestoreError::NotActive(note_id, status.into()));
    }
    let snap = snapshot::take(rpc, vault).await?;
    if snap.tree.leaf(note_id) != zkapi_core::v2::note_leaf(note_id, &commitment, amount, expiry) {
        return Err(SecretRestoreError::NotThisNote(note_id));
    }

    let notes = NoteStore::open(&home.join("notes.sealed"), passphrase, rounds)?;
    if notes.list()?.iter().any(|n| n.commitment == commitment_hex) {
        return Err(SecretRestoreError::Known);
    }
    let entry = NoteEntry {
        chain_label: chain_label.into(),
        secret: hex::encode(secret.0),
        commitment: commitment_hex,
        denomination: amount.to_string(),
        payout: payout.into(),
        // No request is made on a note rebuilt from its secret: it can only leave.
        status: NoteStatus::Exiting,
        note_id: Some(note_id),
        expiry_ns: Some(expiry),
        deposit_amount: Some(amount.to_string()),
        tx_hash: None,
        created_at: now_secs,
    };
    notes.add(entry.clone())?;
    Ok(entry)
}
