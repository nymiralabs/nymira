//! Deposit: one `ft_transfer_call` from the owner's funding account, signed here (in the CLI
//! process) after a terminal confirmation. The note secret is sealed on disk before the
//! transfer is signed, and acceptance is read from the `deposit` event in the transaction's own
//! receipts.

use std::path::PathBuf;
use std::sync::Arc;

use nymira_operator::clock::Clock;
use nymira_operator::store::{self, Store};
use nymira_operator::tx::{NearTx, TxError, TxRequest};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::backup;
use crate::calendar;
use crate::mode::{Mode, FAUCET};
use crate::pins::{Pins, Tripwire};
use crate::rpc::{Rpc, RpcError};
use crate::secrets::{new_secret, NoteEntry, NoteStatus, NoteStore, StoreError};

/// `ft_transfer_call` forwards gas to the vault's `ft_on_transfer` and back.
pub const GAS_TRANSFER_CALL: u64 = 150_000_000_000_000;
/// Wait this long after a deposit before the first lease.
pub const WAIT_HINT: &str =
    "Wait a few hours, ideally a day, before the first lease so the deposit and the lease are not close in time.";

#[derive(Debug, thiserror::Error)]
pub enum DepositError {
    #[error("the vault does not match this build: {}", .0.join(", "))]
    Tripwire(Vec<String>),
    #[error("the vault's token is {found}, not the one this network uses ({expected})")]
    WrongToken { found: String, expected: String },
    #[error("{0} is not a denomination of this vault")]
    Denomination(u128),
    #[error("{account} has {have} of the token and the deposit needs {need}{hint}")]
    ShortBalance {
        account: String,
        have: u128,
        need: u128,
        hint: String,
    },
    #[error("{0} has not called storage_deposit on the token; do that before the transfer")]
    NotRegistered(String),
    #[error("deposit declined")]
    Declined,
    #[error("the transfer failed: {0}")]
    Transfer(String),
    #[error("the vault refunded the transfer: no deposit event for this commitment")]
    Refunded,
    #[error("the vault recorded something other than what was sent: {0}")]
    Mismatch(String),
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("transaction: {0}")]
    Tx(String),
    #[error("backup: {0}")]
    Backup(#[from] backup::BackupError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

impl From<TxError> for DepositError {
    fn from(e: TxError) -> Self {
        Self::Tx(e.to_string())
    }
}

/// Four groups of hex from the account name's hash, to compare by eye.
pub fn fingerprint(account: &str) -> String {
    let digest = Sha256::digest(account.as_bytes());
    digest[..8]
        .chunks(2)
        .map(hex::encode)
        .collect::<Vec<_>>()
        .join("-")
}

/// Where a short testnet balance is topped up.
pub fn short_hint(mode: Mode) -> String {
    match mode {
        Mode::Testnet => format!(
            "; get test USDC at {FAUCET} (Near Testnet, 20 USDC per request, once every 2 hours)"
        ),
        Mode::Mainnet => String::new(),
    }
}

/// What the user sees before the transfer is signed.
#[derive(Clone, Debug)]
pub struct Summary {
    pub sender: String,
    pub payout: String,
    pub payout_fingerprint: String,
    pub denomination: u128,
    pub note_fee: u128,
    pub total: u128,
}

/// The terminal confirmation. The CLI asks; tests answer.
pub trait Confirm {
    fn confirm(&self, summary: &Summary) -> bool;
}

/// The funding transaction's sender. Its HTTP client is the transport's own (a fresh Tor identity
/// in Tor mode), so the transaction and the polling of its status leave this machine the way
/// every other request does: a sender with the default client would go out from the user's
/// address even with Tor down.
pub fn funding_tx(
    rpc_url: &str,
    account: &str,
    token: String,
    key: ed25519_dalek::SigningKey,
    store: Store,
    clock: Arc<dyn Clock>,
    transport: &crate::rpc::Transport,
) -> Result<NearTx, RpcError> {
    let http = crate::rpc::client(transport)?;
    Ok(NearTx::new(rpc_url, account, token, key, store, clock)
        .with_deposit(1)
        .with_http(http))
}

pub struct Ctx {
    pub mode: Mode,
    pub rpc: Rpc,
    pub pins: Pins,
    pub tripwire: Arc<Tripwire>,
    pub notes: NoteStore,
    /// The funding account's sender; its receiver is the token and it attaches 1 yoctoNEAR.
    pub tx: NearTx,
    /// The sender's database, to find a transfer again after a crash.
    pub tx_store: Store,
    pub sender: String,
    pub clock: Arc<dyn Clock>,
    /// Where the encrypted backup goes and who can read it, if the user set it up.
    pub backup: Option<backup::Target>,
    /// Where the calendar file goes.
    pub calendar_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct Accepted {
    pub note_id: u32,
    pub expiry_secs: u64,
    pub deposit_amount: u128,
    pub tx_hash: String,
    pub calendar: PathBuf,
    /// Things that did not work after the deposit was accepted.
    pub warnings: Vec<String>,
}

impl Accepted {
    /// What `nymirad deposit` prints on stdout once the note is accepted.
    pub fn report(&self) -> String {
        format!(
            "Note {} accepted. Expires at Unix time {}.\nCalendar file: {}\n{WAIT_HINT}",
            self.note_id,
            self.expiry_secs,
            self.calendar.display()
        )
    }
}

fn amount(v: &Value) -> u128 {
    v.as_str().and_then(|s| s.parse().ok()).unwrap_or(0)
}

/// `0x` and 64 hex digits, the vault's spelling of a word.
fn hex_word(felt: &zkapi_types::Felt252) -> String {
    format!("0x{}", hex::encode(felt.0))
}

/// The `nymira` events logged by `executor` (the vault) in the receipts of a transaction. A log
/// from any other contract in the same transaction, the token's included, is ignored.
pub fn events_in(status: &Value, executor: &str) -> Vec<Value> {
    let mut events = Vec::new();
    for outcome in status["receipts_outcome"].as_array().into_iter().flatten() {
        if outcome["outcome"]["executor_id"].as_str() != Some(executor) {
            continue;
        }
        for log in outcome["outcome"]["logs"].as_array().into_iter().flatten() {
            let Some(text) = log.as_str().and_then(|l| l.strip_prefix("EVENT_JSON:")) else {
                continue;
            };
            if let Ok(event) = serde_json::from_str::<Value>(text) {
                if event["standard"] == "nymira" {
                    events.push(event);
                }
            }
        }
    }
    events
}

pub async fn tx_events(
    rpc: &Rpc,
    tx_hash: &str,
    sender: &str,
    vault: &str,
) -> Result<Vec<Value>, RpcError> {
    let status = rpc
        .call(
            "EXPERIMENTAL_tx_status",
            json!({ "tx_hash": tx_hash, "sender_account_id": sender, "wait_until": "FINAL" }),
        )
        .await?;
    Ok(events_in(&status, vault))
}

impl Ctx {
    async fn registered(&self, token: &str, account: &str) -> Result<bool, DepositError> {
        let balance = self
            .rpc
            .view(
                token,
                "storage_balance_of",
                json!({ "account_id": account }),
            )
            .await?;
        Ok(!balance.is_null())
    }

    /// Runs the whole deposit.
    pub async fn deposit(
        &self,
        denomination: u128,
        payout: Option<&str>,
        confirm: &dyn Confirm,
    ) -> Result<Accepted, DepositError> {
        self.tripwire
            .check_against(&self.pins, &self.rpc)
            .await
            .map_err(DepositError::Tripwire)?;
        let vault = self.pins.vault.as_str();
        let config = self.rpc.view(vault, "config", json!({})).await?;
        let token = config["token"].as_str().unwrap_or_default().to_string();
        if let Some(expected) = self.mode.pinned_token() {
            if token != expected {
                return Err(DepositError::WrongToken {
                    found: token,
                    expected: expected.into(),
                });
            }
        }
        if !config["denominations"]
            .as_array()
            .is_some_and(|d| d.iter().any(|v| amount(v) == denomination))
        {
            return Err(DepositError::Denomination(denomination));
        }
        let note_fee = amount(&config["note_fee"]);
        let total = denomination + note_fee;
        for account in [self.sender.as_str(), vault] {
            if !self.registered(&token, account).await? {
                return Err(DepositError::NotRegistered(account.into()));
            }
        }
        let have = amount(
            &self
                .rpc
                .view(
                    &token,
                    "ft_balance_of",
                    json!({ "account_id": self.sender }),
                )
                .await?,
        );
        if have < total {
            return Err(DepositError::ShortBalance {
                account: self.sender.clone(),
                have,
                need: total,
                hint: short_hint(self.mode),
            });
        }
        let payout = payout.unwrap_or(&self.sender).to_string();
        let summary = Summary {
            sender: self.sender.clone(),
            payout_fingerprint: fingerprint(&payout),
            payout: payout.clone(),
            denomination,
            note_fee,
            total,
        };
        if !confirm.confirm(&summary) {
            return Err(DepositError::Declined);
        }

        // The secret is on disk, sealed and synced, before the transfer is signed.
        let (secret, commitment) = new_secret(self.mode.chain_label());
        let commitment_hex = hex_word(&commitment);
        self.notes.add(NoteEntry {
            chain_label: self.mode.chain_label().into(),
            secret: hex::encode(secret.0),
            commitment: commitment_hex.clone(),
            denomination: denomination.to_string(),
            payout: payout.clone(),
            status: NoteStatus::Pending,
            note_id: None,
            expiry_ns: None,
            deposit_amount: None,
            tx_hash: None,
            created_at: self.clock.now_secs(),
        })?;

        let msg = json!({ "commitment": commitment_hex, "withdraw_to": payout }).to_string();
        let args = json!({ "receiver_id": vault, "amount": total.to_string(), "msg": msg });
        let row = self
            .tx
            .submit(TxRequest {
                kind: "deposit",
                method: "ft_transfer_call",
                args: args.to_string().into_bytes(),
                gas: GAS_TRANSFER_CALL,
                idem_key: Some(commitment_hex.clone()),
            })
            .await?;
        let hash = row.hash.clone().unwrap_or_default();
        self.notes
            .update(&commitment_hex, |n| n.tx_hash = Some(hash.clone()))?;
        let end = self.tx.drive(row.id).await?;
        if !end.success {
            self.notes
                .update(&commitment_hex, |n| n.status = NoteStatus::Failed)?;
            return Err(DepositError::Transfer(end.detail));
        }
        self.accept(&commitment_hex, &end.hash).await
    }

    /// Reads the `deposit` event of a finished transfer and settles the stored note.
    async fn accept(&self, commitment: &str, tx_hash: &str) -> Result<Accepted, DepositError> {
        let entry = self
            .notes
            .list()?
            .into_iter()
            .find(|n| n.commitment == commitment)
            .ok_or_else(|| DepositError::Mismatch("note is not stored".into()))?;
        let events = tx_events(&self.rpc, tx_hash, &self.sender, &self.pins.vault).await?;
        let Some(event) = events.iter().find(|e| {
            e["event"] == "deposit" && e["data"]["commitment"].as_str() == Some(commitment)
        }) else {
            self.notes
                .update(commitment, |n| n.status = NoteStatus::Failed)?;
            return Err(DepositError::Refunded);
        };
        let data = &event["data"];
        let note_id = data["note_id"].as_u64().unwrap_or(u64::MAX) as u32;
        let expiry = data["expiry"].as_u64().unwrap_or(0);
        if data["withdraw_to"].as_str() != Some(entry.payout.as_str()) {
            return Err(DepositError::Mismatch("payout account".into()));
        }
        if data["denomination"].as_str() != Some(entry.denomination.as_str()) {
            return Err(DepositError::Mismatch("denomination".into()));
        }
        let records = self
            .rpc
            .view(
                &self.pins.vault,
                "note_records",
                json!({ "from": note_id, "limit": 1 }),
            )
            .await?;
        let record = records
            .as_array()
            .and_then(|r| r.first())
            .filter(|r| r["note_id"].as_u64() == Some(u64::from(note_id)))
            .ok_or_else(|| DepositError::Mismatch("note_records has no such note".into()))?;
        let stored = amount(&record["deposit_amount"]);
        if stored.to_string() != entry.denomination {
            return Err(DepositError::Mismatch("deposit_amount".into()));
        }
        if record["expiry"].as_u64() != Some(expiry) {
            return Err(DepositError::Mismatch("expiry".into()));
        }
        self.notes.update(commitment, |n| {
            n.status = NoteStatus::Accepted;
            n.note_id = Some(note_id);
            n.expiry_ns = Some(expiry);
            n.deposit_amount = Some(stored.to_string());
            n.tx_hash = Some(tx_hash.into());
        })?;
        // The money has moved and the note is stored: a failed backup or calendar file is a
        // warning, never an error that invites a second deposit.
        let mut warnings = Vec::new();
        if let Err(e) = self.back_up() {
            warnings.push(format!("the backup was not written: {e}"));
        }
        let calendar = self
            .calendar_dir
            .join(format!("nymira-note-{note_id}-expiry.ics"));
        let written = std::fs::create_dir_all(&self.calendar_dir).and_then(|()| {
            std::fs::write(
                &calendar,
                calendar::expiry_event(note_id, expiry, self.clock.now_secs()),
            )
        });
        if let Err(e) = written {
            warnings.push(format!("the calendar file was not written: {e}"));
        }
        Ok(Accepted {
            note_id,
            expiry_secs: expiry,
            deposit_amount: stored,
            tx_hash: tx_hash.into(),
            calendar,
            warnings,
        })
    }

    /// Writes the encrypted backup of the note list, if a recipient is set up. The daemon's
    /// `state.age` carries the wallets too; this one covers the time before the first lease.
    pub fn back_up(&self) -> Result<(), DepositError> {
        let Some(target) = &self.backup else {
            return Ok(());
        };
        let state = backup::StateBackup {
            version: backup::VERSION,
            taken_at: self.clock.now_secs(),
            notes: self.notes.list()?,
            ..Default::default()
        };
        backup::write_backups(target, &state.to_bytes()?, backup::NOTES_FILE)?;
        Ok(())
    }

    /// After a crash: finds each pending note again from its transfer. A transfer that was
    /// never signed leaves the note failed (its secret was never used); a signed one is driven
    /// to its end and read from its receipts.
    pub async fn recover(
        &self,
    ) -> Result<Vec<(String, Result<Accepted, DepositError>)>, DepositError> {
        let mut out = Vec::new();
        for entry in self.notes.list()? {
            if entry.status != NoteStatus::Pending {
                continue;
            }
            let key = entry.commitment.clone();
            let row = self
                .tx_store
                .run({
                    let key = key.clone();
                    move |c| store::tx_by_idem_key(c, "deposit", &key, 0)
                })
                .await
                .map_err(|e| DepositError::Tx(e.to_string()))?;
            let result = match row {
                None => {
                    self.notes.update(&key, |n| n.status = NoteStatus::Failed)?;
                    Err(DepositError::Transfer(
                        "the transfer was never signed".into(),
                    ))
                }
                Some(row) => match self.tx.drive(row.id).await {
                    Ok(end) if end.success => self.accept(&key, &end.hash).await,
                    Ok(end) => {
                        self.notes.update(&key, |n| n.status = NoteStatus::Failed)?;
                        Err(DepositError::Transfer(end.detail))
                    }
                    Err(e) => Err(e.into()),
                },
            };
            out.push((key, result));
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_vaults_own_logs_count_as_events() {
        let event =
            r#"EVENT_JSON:{"standard":"nymira","version":"1.0.0","event":"deposit","data":{}}"#;
        let status = json!({ "receipts_outcome": [
            { "outcome": { "executor_id": "usdc.test", "logs": [event] } },
            { "outcome": { "executor_id": "vault.test", "logs": [event, "plain log"] } },
        ]});
        assert_eq!(events_in(&status, "vault.test").len(), 1);
        assert_eq!(events_in(&status, "other.test").len(), 0);
    }
}
