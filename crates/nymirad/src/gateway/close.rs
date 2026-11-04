//! Leaving a note: `close` (the operator's clearance, then `mutual_close`), `exit`
//! (`request_exit`, with no clearance), `finalize` (`finalize_exit` after the deadline) and
//! `claim_payout`.
//!
//! Chain transactions come from the note family's submitter account, never the funding account.
//! They go over Tor; if Tor is down, only a direct send the user confirmed in words goes out
//! (see [`direct_text`]). The clearance and the operator's `POST /v1/close` never go direct: they
//! would show the user's address to the operator.

use std::time::Duration;

use async_trait::async_trait;
use base64::Engine;
use nymira_operator::tx::{NearTx, TxRequest};
use serde_json::{json, Value};
use zkapi_client::exit_lock::ExitMode;
use zkapi_types::wire::{ClearanceOwnershipProofV2C, ClearanceRequestV2C, ClearanceResponseV2};
use zkapi_types::{Felt252, SchnorrSignature, WithdrawalPublicInputsV2C};

use super::*;
use crate::direct::DirectClient;
use crate::exits::Left;
use crate::submitter::{Submitter, PERSONAL};

/// Gas attached to every submitter call: `mutual_close` needs 80 Tgas and the payouts 30.
const GAS: u64 = 100_000_000_000_000;
/// A submitter with less than this (0.02 NEAR) is treated as having no NEAR. A call prepays its
/// attached gas: 100 Tgas at the minimum gas price (0.0001 NEAR per Tgas) is 0.01 NEAR, which is
/// refunded when unused but must be there when the transaction is made; the margin covers a
/// higher gas price.
const MIN_SUBMITTER_YOCTO: u128 = 20_000_000_000_000_000_000_000;
/// How long a random close delay lasts at most, in seconds. The ownership proof lives 55
/// minutes; the delay must leave it room.
pub const MAX_CLOSE_DELAY_SECS: u64 = 1_200;
/// Slack on the local clock when comparing it with the vault's block time.
const DEADLINE_SLACK_SECS: u64 = 2;

#[derive(Debug, thiserror::Error)]
pub enum LeaveError {
    #[error("no note with ID {0} that this command can act on")]
    NoNote(u32),
    #[error("note {0} has a lease open or settling; wait until it is retired, then try again")]
    Busy(u32),
    #[error("family {family:?} has no submitter account; run `nymirad submitter add --family {family} --account ACCOUNT` and follow what it prints")]
    NoSubmitter { family: String },
    #[error("the submitter account {account} has no NEAR for gas; fund it, or send the close through the operator with --via-operator (the operator takes its close fee)")]
    NoNear { account: String },
    #[error("Tor is down: {0}")]
    TorDown(String),
    #[error("Tor is down and the direct send was not confirmed; nothing was sent")]
    Declined,
    #[error("the operator refused: {0}")]
    Refused(String),
    #[error("the exit window of note {note_id} is open until Unix time {deadline}")]
    NotYet { note_id: u32, deadline: u64 },
    #[error("note {0} has no exit filed")]
    NoExit(u32),
    #[error("nothing is pending for {0}")]
    NothingPending(String),
    #[error("{0}")]
    Failed(String),
}

fn failed(e: impl std::fmt::Display) -> LeaveError {
    LeaveError::Failed(e.to_string())
}

/// Asks the user about a direct send.
#[async_trait]
pub trait Ask: Send + Sync {
    async fn confirm_direct(&self, text: &str) -> bool;
}

/// The question put to the user before a send that bypasses Tor. Fixed wording: the numbers are
/// the only part that changes.
pub fn direct_text(note_id: u32, spent: &str) -> String {
    format!(
        "Tor is down. Sending this transaction directly shows your IP address to the RPC endpoint, together with this note's ID ({note_id}) and its total spend ({spent} base units of the token). Type yes to send directly, or anything else to stop."
    )
}

#[derive(Clone, Debug)]
pub struct CloseOptions {
    /// Wait this long before the ownership proof goes out, so the close is not tied to the last
    /// lease by time. The CLI picks a random value; tests pass zero.
    pub delay: Duration,
    /// The submitter family; the note's own when `None`.
    pub family: Option<String>,
    /// Hand the close proof to the operator's `POST /v1/close` (it takes the close fee).
    pub via_operator: bool,
    /// After the operator took a close, look this many times, `poll_every` apart, for the vault
    /// to show the note closed.
    pub polls: u32,
    pub poll_every: Duration,
}

impl Default for CloseOptions {
    fn default() -> Self {
        Self {
            delay: Duration::from_secs(60 + rand::random::<u64>() % (MAX_CLOSE_DELAY_SECS - 60)),
            family: None,
            via_operator: false,
            polls: 30,
            poll_every: Duration::from_secs(4),
        }
    }
}

#[derive(Debug)]
pub struct Closed {
    pub note_id: u32,
    pub tx_hash: String,
    pub payout_account: String,
    /// The vault showed the note closed. `false` after `--via-operator` when it had not yet.
    pub confirmed: bool,
    /// Unix seconds the close proof was valid until.
    pub valid_until: u64,
}

#[derive(Debug)]
pub struct Exiting {
    pub note_id: u32,
    pub tx_hash: Option<String>,
    pub conceded: u128,
    /// Unix seconds after which `finalize_exit` works.
    pub deadline_secs: u64,
}

#[derive(Debug)]
pub struct Paid {
    pub note_id: u32,
    pub tx_hash: String,
}

/// How a transaction goes out: the client for the sender and the `Rpc` for reads.
struct Route {
    http: reqwest::Client,
    rpc: Rpc,
}

/// Keeps a note out of `pick_note` for as long as a leave operation holds it.
struct Hold<'a> {
    gw: &'a Gateway,
    key: String,
}

impl Drop for Hold<'_> {
    fn drop(&mut self) {
        // `try_lock` cannot fail for long: every other holder keeps the lock for one statement.
        let (gw, key) = (self.gw, self.key.clone());
        let settling = &gw.settling;
        match settling.try_lock() {
            Ok(mut set) => {
                set.remove(&key);
            }
            Err(_) => {
                // Removed by the next `release` call; see `Gateway::release_held`.
                gw.stuck_holds
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(key);
            }
        }
    }
}

pub fn words(public: &WithdrawalPublicInputsV2C) -> Vec<String> {
    let b64 = base64::engine::general_purpose::STANDARD;
    public
        .to_field_elements()
        .iter()
        .map(|f| b64.encode(f.as_bytes()))
        .collect()
}

fn spent_of(wallet: &Wallet) -> String {
    wallet
        .state()
        .map_or(0, |s| s.deposit_amount.saturating_sub(s.current_balance))
        .to_string()
}

impl Gateway {
    /// Puts the note out of reach of new leases; the guard lifts it on drop.
    async fn hold(&self, entry: &NoteEntry) -> Result<Hold<'_>, LeaveError> {
        self.release_held().await;
        let id = entry.note_id.unwrap_or(0);
        if self.sessions.lock().await.contains_key(&entry.commitment) {
            return Err(LeaveError::Busy(id));
        }
        if !self.settling.lock().await.insert(entry.commitment.clone()) {
            return Err(LeaveError::Busy(id));
        }
        Ok(Hold {
            gw: self,
            key: entry.commitment.clone(),
        })
    }

    async fn release_held(&self) {
        let keys: Vec<String> =
            std::mem::take(&mut *self.stuck_holds.lock().unwrap_or_else(|e| e.into_inner()));
        if !keys.is_empty() {
            let mut set = self.settling.lock().await;
            for k in keys {
                set.remove(&k);
            }
        }
    }

    fn note(&self, note_id: u32, allowed: &[NoteStatus]) -> Result<NoteEntry, LeaveError> {
        self.notes
            .list()
            .map_err(failed)?
            .into_iter()
            .find(|n| n.note_id == Some(note_id) && allowed.contains(&n.status))
            .ok_or(LeaveError::NoNote(note_id))
    }

    /// The commitment of the note with this ID.
    pub fn commitment_of(&self, note_id: u32) -> Result<String, LeaveError> {
        self.notes
            .list()
            .map_err(failed)?
            .into_iter()
            .find(|n| n.note_id == Some(note_id))
            .map(|n| n.commitment.clone())
            .ok_or(LeaveError::NoNote(note_id))
    }

    fn set_status(&self, entry: &NoteEntry, status: NoteStatus) -> Result<(), LeaveError> {
        self.notes
            .update(&entry.commitment, |n| n.status = status)
            .map_err(failed)
    }

    fn submitter(&self, entry: &NoteEntry, family: Option<&str>) -> Result<Submitter, LeaveError> {
        let family = match family {
            Some(f) => f.to_string(),
            None => self
                .submitters
                .family_of(&entry.commitment)
                .unwrap_or_else(|_| PERSONAL.into()),
        };
        self.submitters
            .get(&family)
            .map_err(failed)?
            .ok_or(LeaveError::NoSubmitter { family })
    }

    /// The route for a chain transaction: Tor on its own circuit; if Tor is down, a direct one only
    /// when `ask` is given and the user says yes to [`direct_text`]. A remote hint never gets here.
    async fn route(
        &self,
        note_id: u32,
        spent: &str,
        ask: Option<&dyn Ask>,
    ) -> Result<Route, LeaveError> {
        match self.transport.ensure_ready() {
            Ok(()) => Ok(Route {
                http: client(&self.transport).map_err(|e| match e {
                    RpcError::TorDown(m) => LeaveError::TorDown(m),
                    other => failed(other),
                })?,
                rpc: self.rpc.clone(),
            }),
            Err(RpcError::TorDown(why)) => {
                let Some(ask) = ask else {
                    return Err(LeaveError::TorDown(why));
                };
                if !ask.confirm_direct(&direct_text(note_id, spent)).await {
                    return Err(LeaveError::Declined);
                }
                let direct = DirectClient::new(self.rpc.url()).map_err(failed)?;
                Ok(Route {
                    http: direct.http(),
                    rpc: Rpc::confirmed_direct(self.rpc.url(), direct.http()),
                })
            }
            Err(other) => Err(failed(other)),
        }
    }

    fn sender(&self, sub: &Submitter, route: &Route) -> NearTx {
        NearTx::new(
            self.rpc.url(),
            sub.account.clone(),
            self.cfg.vault.clone(),
            sub.key.clone(),
            self.tx_store.clone(),
            self.clock.clone(),
        )
        .with_config(self.tx_config)
        .with_http(route.http.clone())
    }

    /// Sends one call and returns the hash with whether the receipts show a failure. A failed
    /// payout receipt marks the whole transaction failed although the vault has already closed the
    /// note and credited a pending payout, so callers check the vault before believing a failure.
    async fn send(
        &self,
        tx: &NearTx,
        kind: &'static str,
        method: &'static str,
        args: Value,
        idem: String,
    ) -> Result<nymira_operator::tx::TxEnd, LeaveError> {
        tx.send(TxRequest {
            kind,
            method,
            args: args.to_string().into_bytes(),
            gas: GAS,
            idem_key: Some(idem),
        })
        .await
        .map_err(failed)
    }

    fn refused(method: &str, end: &nymira_operator::tx::TxEnd) -> LeaveError {
        LeaveError::Failed(format!("the vault refused {method}: {}", end.detail))
    }

    async fn funded(&self, tx: &NearTx, sub: &Submitter) -> Result<(), LeaveError> {
        match tx.balance().await {
            Ok(b) if b >= MIN_SUBMITTER_YOCTO => Ok(()),
            Ok(_) => Err(LeaveError::NoNear {
                account: sub.account.clone(),
            }),
            Err(e) => Err(failed(e)),
        }
    }

    /// The wallet of a note, ready to make exit proofs. A request the operator has settled is
    /// installed; one it has not stays pending, and the exit concedes its bound.
    async fn leaving_wallet(&self, entry: &NoteEntry) -> Result<Wallet, LeaveError> {
        match self.open_wallet(entry, 0) {
            Ok(mut wallet) => {
                if wallet.has_pending_request() {
                    let _ = crate::rpc::recover_pending_over(&self.transport, &mut wallet).await;
                }
                Ok(wallet)
            }
            Err(Skip::Reason(r)) => {
                let _ = self.vault.checkin(&entry.commitment);
                Err(LeaveError::Failed(r))
            }
            Err(Skip::Fatal(e)) => {
                let _ = self.vault.checkin(&entry.commitment);
                Err(LeaveError::Failed(e.message))
            }
        }
    }

    /// The exit request of a note, from the vault.
    async fn exit_request(&self, rpc: &Rpc, note_id: u32) -> Result<Option<Value>, LeaveError> {
        let mut from = 0u32;
        loop {
            let page = rpc
                .view(
                    &self.cfg.vault,
                    "exit_requests",
                    json!({ "from_note_id": from, "limit": 100 }),
                )
                .await
                .map_err(failed)?;
            let rows = page.as_array().cloned().unwrap_or_default();
            if let Some(row) = rows.iter().find(|r| r["note_id"] == note_id) {
                return Ok(Some(row.clone()));
            }
            match rows.last().and_then(|r| r["note_id"].as_u64()) {
                Some(last) if rows.len() == 100 => from = last as u32 + 1,
                _ => return Ok(None),
            }
        }
    }

    fn clearance_error(status: u16, body: &Value) -> LeaveError {
        let code = body["error"]["code"].as_str().unwrap_or("");
        let message = body["error"]["message"].as_str().unwrap_or("");
        LeaveError::Refused(format!("HTTP {status} {code}: {message}"))
    }

    /// `POST /v2/withdraw/clearance` on its own circuit.
    async fn clearance(
        &self,
        nullifier: Felt252,
        ownership: ClearanceOwnershipProofV2C,
    ) -> Result<SchnorrSignature, LeaveError> {
        self.operator_post(
            "/v2/withdraw/clearance",
            serde_json::to_value(ClearanceRequestV2C {
                withdrawal_nullifier: nullifier,
                ownership_proof: ownership,
            })
            .map_err(failed)?,
        )
        .await
        .and_then(|body| {
            serde_json::from_value::<ClearanceResponseV2>(body)
                .map(|r| r.signature)
                .map_err(failed)
        })
    }

    /// A POST to the operator over Tor, one connection of its own.
    async fn operator_post(&self, path: &str, body: Value) -> Result<Value, LeaveError> {
        if let Err(RpcError::TorDown(m)) = self.transport.ensure_ready() {
            return Err(LeaveError::TorDown(m));
        }
        let http = client(&self.transport).map_err(failed)?;
        let reply = http
            .post(format!(
                "{}{path}",
                self.cfg.operator_url.trim_end_matches('/')
            ))
            .json(&body)
            .send()
            .await
            .map_err(|e| match send_error(e) {
                RpcError::TorDown(m) => LeaveError::TorDown(m),
                other => failed(other),
            })?;
        let status = reply.status().as_u16();
        let body: Value = reply.json().await.unwrap_or(Value::Null);
        if (200..300).contains(&status) {
            Ok(body)
        } else {
            Err(Self::clearance_error(status, &body))
        }
    }

    fn remember(&self, entry: &NoteEntry, deadline_secs: Option<u64>, spent: String) {
        let _ = self.exits.put(
            &entry.commitment,
            Left {
                note_id: entry.note_id.unwrap_or(0),
                deadline_secs,
                spent,
            },
        );
    }

    /// The note is closed on chain: archive the wallet, mark it, write the backup.
    fn archive(&self, entry: &NoteEntry, mut wallet: Wallet) -> Result<(), LeaveError> {
        wallet.archive_note().map_err(failed)?;
        drop(wallet);
        self.vault.checkin(&entry.commitment).map_err(failed)?;
        self.set_status(entry, NoteStatus::Closed)?;
        self.back_up();
        Ok(())
    }

    /// Closes a note: clearance, then `mutual_close` to the payout account.
    pub async fn close_note(
        &self,
        note_id: u32,
        opts: CloseOptions,
        ask: Option<&dyn Ask>,
    ) -> Result<Closed, LeaveError> {
        let entry = self.note(note_id, &[NoteStatus::Accepted])?;
        let _hold = self.hold(&entry).await?;
        let sub = if opts.via_operator {
            None
        } else {
            Some(self.submitter(&entry, opts.family.as_deref())?)
        };
        // Clearance and the operator's close both need Tor, so a dead Tor stops here.
        if let Err(RpcError::TorDown(m)) = self.transport.ensure_ready() {
            return Err(LeaveError::TorDown(m));
        }
        let wallet = self.leaving_wallet(&entry).await?;
        let spent = spent_of(&wallet);
        let sent = self
            .close_with(&entry, &wallet, &opts, sub.as_ref(), &spent, ask)
            .await;
        match sent {
            Ok(closed) if closed.confirmed => {
                self.remember(&entry, None, spent);
                self.archive(&entry, wallet)?;
                Ok(closed)
            }
            // The lock stays: the clearance was used or may have been. The wallet is sealed again.
            other => {
                drop(wallet);
                let _ = self.vault.checkin(&entry.commitment);
                other
            }
        }
    }

    async fn close_with(
        &self,
        entry: &NoteEntry,
        wallet: &Wallet,
        opts: &CloseOptions,
        sub: Option<&Submitter>,
        spent: &str,
        ask: Option<&dyn Ask>,
    ) -> Result<Closed, LeaveError> {
        let note_id = entry.note_id.unwrap_or(0);
        // A close sent earlier may have landed after the answer was lost.
        if self.is_closed(entry).await {
            return Ok(Closed {
                note_id,
                tx_hash: String::new(),
                payout_account: entry.payout.clone(),
                confirmed: true,
                valid_until: 0,
            });
        }
        wallet.set_exit_lock(ExitMode::Mutual).map_err(failed)?;
        if !opts.delay.is_zero() {
            tokio::time::sleep(opts.delay).await;
        }
        let nullifier = wallet
            .current_nullifier()
            .ok_or_else(|| LeaveError::Failed("the note has no state".into()))?;
        let ownership = wallet.ownership_proof().map_err(failed)?;
        let signature = self.clearance(nullifier, ownership).await?;
        let destination = nymira_near_codec::destination(&entry.payout);
        let (public, proof) = wallet
            .mutual_close_from_clearance(destination, &signature)
            .map_err(failed)?;
        let args = json!({ "proof": proof.proof, "inputs": words(&public) });

        match sub {
            None => {
                let answer = self.operator_post("/v1/close", args).await?;
                let tx_hash = answer["tx_hash"].as_str().unwrap_or("").to_string();
                let mut confirmed = false;
                for _ in 0..opts.polls {
                    if self.is_closed(entry).await {
                        confirmed = true;
                        break;
                    }
                    tokio::time::sleep(opts.poll_every).await;
                }
                Ok(Closed {
                    note_id,
                    tx_hash,
                    payout_account: entry.payout.clone(),
                    confirmed,
                    valid_until: public.valid_until,
                })
            }
            Some(sub) => {
                let route = self.route(note_id, spent, ask).await?;
                let tx = self.sender(sub, &route);
                self.funded(&tx, sub).await?;
                let end = self
                    .send(
                        &tx,
                        "close",
                        "mutual_close",
                        args,
                        format!("close:{note_id}:{}", public.valid_until),
                    )
                    .await?;
                if !end.success && !self.is_closed(entry).await {
                    return Err(Self::refused("mutual_close", &end));
                }
                Ok(Closed {
                    note_id,
                    tx_hash: end.hash,
                    payout_account: entry.payout.clone(),
                    confirmed: true,
                    valid_until: public.valid_until,
                })
            }
        }
    }

    /// Whether the vault shows the note closed. Read through `self.rpc` only (Tor in a release
    /// build): the commitment tells the node which note is yours.
    async fn is_closed(&self, entry: &NoteEntry) -> bool {
        matches!(
            self.rpc
                .view(&self.cfg.vault, "note_status", json!({ "commitment": entry.commitment }))
                .await,
            Ok(Value::String(s)) if s == "Closed"
        )
    }

    /// Files `request_exit` for the latest state. The concession is the stored bound of the state
    /// (0 when no request ever left it), so the exit claims what zkapi's rules allow.
    pub async fn exit_note(
        &self,
        note_id: u32,
        family: Option<&str>,
        ask: Option<&dyn Ask>,
    ) -> Result<Exiting, LeaveError> {
        let entry = self.note(note_id, &[NoteStatus::Accepted, NoteStatus::Exiting])?;
        let _hold = self.hold(&entry).await?;
        let sub = self.submitter(&entry, family)?;
        let wallet = self.leaving_wallet(&entry).await?;
        let result = self.exit_with(&entry, &wallet, &sub, ask).await;
        drop(wallet);
        let _ = self.vault.checkin(&entry.commitment);
        result
    }

    async fn exit_with(
        &self,
        entry: &NoteEntry,
        wallet: &Wallet,
        sub: &Submitter,
        ask: Option<&dyn Ask>,
    ) -> Result<Exiting, LeaveError> {
        let note_id = entry.note_id.unwrap_or(0);
        let spent = spent_of(wallet);
        let route = self.route(note_id, &spent, ask).await?;
        // Filed already (a lost answer, or a second run): report it.
        if let Some(row) = self.exit_request(&route.rpc, note_id).await? {
            return self.filed(entry, &row, None, spent);
        }
        // Before any proof exists: a submitter without gas must not leave a half-started exit.
        let tx = self.sender(sub, &route);
        self.funded(&tx, sub).await?;
        let conceded = wallet
            .state()
            .ok_or_else(|| LeaveError::Failed("the note has no state".into()))?
            .bound_at_state;
        wallet.set_exit_lock(ExitMode::Escape).map_err(failed)?;
        let destination = nymira_near_codec::destination(&entry.payout);
        let (public, proof) = wallet
            .withdrawal_escape_hatch(destination, conceded)
            .map_err(failed)?;
        // From here the proof may leave: no request is made on this note any more.
        self.set_status(entry, NoteStatus::Exiting)?;
        let args = json!({ "proof": proof.proof, "inputs": words(&public) });
        let end = self
            .send(
                &tx,
                "exit",
                "request_exit",
                args,
                format!("exit:{note_id}:{}", public.valid_until),
            )
            .await?;
        if !end.success {
            return Err(Self::refused("request_exit", &end));
        }
        let hash = end.hash;
        let row = self
            .exit_request(&route.rpc, note_id)
            .await?
            .ok_or_else(|| {
                LeaveError::Failed("request_exit landed but the vault lists no exit".into())
            })?;
        self.filed(entry, &row, Some(hash), spent)
    }

    fn filed(
        &self,
        entry: &NoteEntry,
        row: &Value,
        tx_hash: Option<String>,
        spent: String,
    ) -> Result<Exiting, LeaveError> {
        let deadline_ns: u64 = row["deadline"]
            .as_str()
            .and_then(|d| d.parse().ok())
            .or_else(|| row["deadline"].as_u64())
            .ok_or_else(|| LeaveError::Failed("the exit's deadline is unreadable".into()))?;
        let deadline_secs = deadline_ns / 1_000_000_000;
        self.set_status(entry, NoteStatus::Exiting)?;
        self.remember(entry, Some(deadline_secs), spent);
        Ok(Exiting {
            note_id: entry.note_id.unwrap_or(0),
            tx_hash,
            conceded: row["conceded"]
                .as_str()
                .and_then(|c| c.parse().ok())
                .unwrap_or(0),
            deadline_secs,
        })
    }

    /// Sends `finalize_exit` once the deadline has passed.
    pub async fn finalize_note(
        &self,
        note_id: u32,
        family: Option<&str>,
        ask: Option<&dyn Ask>,
    ) -> Result<Paid, LeaveError> {
        let entry = self.note(note_id, &[NoteStatus::Exiting])?;
        let _hold = self.hold(&entry).await?;
        let sub = self.submitter(&entry, family)?;
        let known = self.exits.get(&entry.commitment);
        let spent = known
            .as_ref()
            .map_or_else(|| "not recorded".to_string(), |l| l.spent.clone());
        let route = self.route(note_id, &spent, ask).await?;
        // The book is local; if it was lost (a restored backup), the vault has the deadline.
        let deadline = match known.and_then(|l| l.deadline_secs) {
            Some(d) => d,
            None => {
                let row = self
                    .exit_request(&route.rpc, note_id)
                    .await?
                    .ok_or(LeaveError::NoExit(note_id))?;
                let ns: u64 = row["deadline"]
                    .as_str()
                    .and_then(|d| d.parse().ok())
                    .unwrap_or(u64::MAX);
                let d = ns / 1_000_000_000;
                self.remember(&entry, Some(d), spent);
                d
            }
        };
        if self.clock.now_secs() < deadline + DEADLINE_SLACK_SECS {
            return Err(LeaveError::NotYet { note_id, deadline });
        }
        let tx = self.sender(&sub, &route);
        self.funded(&tx, &sub).await?;
        let end = self
            .send(
                &tx,
                "finalize",
                "finalize_exit",
                json!({ "note_id": note_id }),
                format!("finalize:{note_id}"),
            )
            .await?;
        if !end.success {
            // A failed transaction is a paid exit only if the vault shows the note closed (its
            // payout leg failed after the note closed and a pending payout was booked). An exit
            // that was defeated has no record either, and its note is `Active` again: believing
            // the failure away would archive a wallet whose note still holds money.
            if self.exit_request(&route.rpc, note_id).await?.is_some()
                || !self.is_closed(&entry).await
            {
                return Err(Self::refused("finalize_exit", &end));
            }
        }
        let hash = end.hash;
        // The wallet's state is done with: the exit lock goes with the archive.
        if let Ok(wallet) = self.leaving_wallet(&entry).await {
            self.archive(&entry, wallet)?;
        } else {
            self.set_status(&entry, NoteStatus::Closed)?;
        }
        let _ = self.exits.finalized(&entry.commitment);
        Ok(Paid {
            note_id,
            tx_hash: hash,
        })
    }

    /// Collects the pending payout of a note's payout account (a payout whose transfer failed).
    pub async fn claim_payout(
        &self,
        note_id: u32,
        family: Option<&str>,
        ask: Option<&dyn Ask>,
    ) -> Result<Paid, LeaveError> {
        let entry = self.note(
            note_id,
            &[
                NoteStatus::Accepted,
                NoteStatus::Exiting,
                NoteStatus::Closed,
            ],
        )?;
        let sub = self.submitter(&entry, family)?;
        let spent = self
            .exits
            .get(&entry.commitment)
            .map_or_else(|| "not recorded".to_string(), |l| l.spent);
        let route = self.route(note_id, &spent, ask).await?;
        let hash = self
            .claim_from(&entry, &sub, &route, format!("claim:{note_id}"))
            .await?;
        Ok(Paid {
            note_id,
            tx_hash: hash,
        })
    }

    /// Claims the payout account of a saved Intents attempt, including one that made no note.
    pub async fn claim_intents_payout(
        &self,
        commitment: &str,
        family: Option<&str>,
    ) -> Result<String, LeaveError> {
        if !self
            .notes
            .funding_attempts()
            .map_err(failed)?
            .iter()
            .any(|a| a.commitment == commitment)
        {
            return Err(failed("no saved Intents attempt for that commitment"));
        }
        let entry = self
            .notes
            .list()
            .map_err(failed)?
            .into_iter()
            .find(|n| n.commitment == commitment)
            .ok_or_else(|| failed("pending note missing"))?;
        let sub = self.submitter(&entry, family)?;
        self.transport.ensure_ready().map_err(|e| match e {
            RpcError::TorDown(message) => LeaveError::TorDown(message),
            other => failed(other),
        })?;
        let route = Route {
            http: client(&self.transport).map_err(failed)?,
            rpc: self.rpc.clone(),
        };
        self.claim_from(
            &entry,
            &sub,
            &route,
            format!("claim-intents:{commitment}:{}", crate::net::identity()),
        )
        .await
    }

    async fn claim_from(
        &self,
        entry: &NoteEntry,
        sub: &Submitter,
        route: &Route,
        idem: String,
    ) -> Result<String, LeaveError> {
        let pending = route
            .rpc
            .view(
                &self.cfg.vault,
                "pending_payout",
                json!({"account_id": entry.payout}),
            )
            .await
            .map_err(failed)?;
        let amount = pending
            .as_str()
            .and_then(|s| s.parse::<u128>().ok())
            .filter(|n| *n > 0)
            .ok_or_else(|| LeaveError::NothingPending(entry.payout.clone()))?;
        let tx = self.sender(sub, route);
        self.funded(&tx, sub).await?;
        let end = self
            .send(
                &tx,
                "claim",
                "claim_payout",
                json!({"account_id": entry.payout}),
                format!("{idem}:{amount}"),
            )
            .await?;
        if !end.success {
            return Err(Self::refused("claim_payout", &end));
        }
        Ok(end.hash)
    }

    /// Exits whose deadline has passed and whose `finalize_exit` is due, sent without asking:
    /// when Tor is down they wait. At most one try per note a minute.
    pub async fn finalize_due(&self) -> Vec<(u32, Result<Paid, LeaveError>)> {
        let now = self.clock.now_secs();
        let due: Vec<(String, Left)> = self
            .exits
            .pending()
            .into_iter()
            .filter(|(_, l)| {
                l.deadline_secs
                    .is_some_and(|d| now >= d + DEADLINE_SLACK_SECS)
            })
            .collect();
        let mut out = Vec::new();
        for (commitment, left) in due {
            {
                let mut tried = self
                    .finalize_tried
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if tried
                    .get(&commitment)
                    .is_some_and(|at| now.saturating_sub(*at) < 60)
                {
                    continue;
                }
                tried.insert(commitment.clone(), now);
            }
            out.push((
                left.note_id,
                self.finalize_note(left.note_id, None, None).await,
            ));
        }
        out
    }
}
