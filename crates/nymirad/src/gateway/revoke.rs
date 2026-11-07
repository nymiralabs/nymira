//! The owner's side of a budget note: what each agent has spent, and revocation.
//!
//! Revoking needs nothing from the agent. In order: the agent's token is disabled, the open lease
//! is retired and settled, a clearance is asked for and the note is closed to the owner's payout
//! account. Only if the operator gives no clearance is an exit filed; `finalize_exit` follows
//! after the window like any other exit. From the first step on, no agent request is served on
//! the note (`note_expiring`, or `token_disabled` at the front door).

use std::time::Duration;

use serde_json::{json, Value};

use super::close::{Ask, CloseOptions, Closed, Exiting, LeaveError};
use super::*;
use crate::agents::{Book, Registry};

/// What revoking tried, and how long it waits for the lease to settle.
#[derive(Clone, Debug)]
pub struct RevokeOptions {
    /// How the close goes out (delay, submitter family, `via_operator`).
    pub close: CloseOptions,
    /// The lease is looked at this many times, `settle_every` apart, before giving up.
    pub settle_polls: u32,
    pub settle_every: Duration,
}

impl Default for RevokeOptions {
    fn default() -> Self {
        Self {
            close: CloseOptions::default(),
            settle_polls: 150,
            settle_every: Duration::from_secs(2),
        }
    }
}

#[derive(Debug)]
pub enum Revoked {
    /// The operator cleared the close.
    Closed(Closed),
    /// The operator gave no clearance (its answer is the second field), so an exit was filed.
    Exiting(Exiting, String),
}

/// The limits of a policy, as the owner's view and the balance route show them.
pub(super) fn policy_json(p: &crate::policy::Policy) -> Value {
    json!({
        "allowed_models": p.allowed_models,
        "max_lease_usd": p.max_lease_usd,
        "max_call_usd": p.max_call_usd,
        "max_daily_usd": p.max_daily_usd,
        "confirm_above_usd": p.confirm_above_usd,
        "rate_limit_per_min": p.rate_limit_per_min,
        "require_tee": p.require_tee,
    })
}

impl Gateway {
    /// Why an agent request on `note` must not be served, if it must not: a revocation has
    /// started, a close or exit has begun, or the note is in the last days before it expires.
    pub(super) fn note_refusal(&self, note: &str) -> Option<ApiError> {
        if self.is_moved(note) {
            return Some(ApiError::note_moved());
        }
        if self
            .revoking
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(note)
        {
            return Some(ApiError::note_expiring());
        }
        let entry = self
            .notes
            .list()
            .ok()?
            .into_iter()
            .find(|n| n.commitment == note)?;
        let leaving = matches!(entry.status, NoteStatus::Exiting | NoteStatus::Closed);
        let short = self.guard.enabled
            && entry
                .expiry_ns
                .is_some_and(|e| crate::expiry::lease_blocked(e, self.clock.now_secs()));
        (leaving || short).then(ApiError::note_expiring)
    }

    /// Whether the budget note runs on the agent's machine. An unreadable registry counts as
    /// moved: this copy then refuses rather than risk a second spender.
    pub(super) fn is_moved(&self, note: &str) -> bool {
        Registry::at(&self.cfg.home)
            .load()
            .map_or(true, |b| b.for_note(note).is_some_and(|a| a.moved))
    }

    /// The note's balance as of the last settled lease: what was deposited, less every charge
    /// recorded. A lease still open is not counted.
    pub(super) fn note_balance(&self, entry: &NoteEntry) -> Option<u128> {
        let deposit: u128 = entry.deposit_amount.as_deref()?.parse().ok()?;
        let id = entry.note_id?;
        let charged: u128 = self
            .records
            .list()
            .iter()
            .filter(|r| r.note_id == id && r.closed_at.is_some())
            .filter_map(|r| r.charge.as_deref()?.parse::<u128>().ok())
            .sum();
        Some(deposit.saturating_sub(charged))
    }

    /// Each agent's spend today (UTC), its note's balance and expiry, and the policy in force.
    /// Public values only: no token, no secret.
    pub fn agent_overview(&self, book: &Book) -> Value {
        let notes = self.notes.list().unwrap_or_default();
        let ctx = self.policy_context();
        let revoking = self
            .revoking
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        let rows: Vec<Value> = book
            .agents
            .iter()
            .map(|a| {
                let entry = notes.iter().find(|n| n.commitment == a.note);
                let policy =
                    crate::policy::in_force(&self.cfg.home, a, &ctx).map(|p| policy_json(&p));
                json!({
                    "agent": a.name,
                    "token": if a.disabled { "disabled" } else { "active" },
                    "revoking": revoking.contains(&a.note),
                    "note_id": entry.and_then(|n| n.note_id),
                    "note_status": entry.map(|n| format!("{:?}", n.status)),
                    "expires_at": entry.and_then(|n| n.expiry_ns),
                    "balance_micro": entry.and_then(|n| self.note_balance(n)).map(|b| b.to_string()),
                    "spent_today_micro": self.agent_spend_today(&a.name, &a.note).to_string(),
                    "charge_warning": self.charge_warnings.active(&a.note),
                    "policy_version": a.policy_version,
                    "policy": policy,
                })
            })
            .collect();
        Value::Array(rows)
    }

    /// Revokes the budget note of the agent called `name`. Safe to run again after a failure:
    /// the token stays disabled and the note stays shut to the agent.
    pub async fn revoke_agent(
        &self,
        name: &str,
        opts: RevokeOptions,
        ask: Option<&dyn Ask>,
    ) -> Result<Revoked, LeaveError> {
        let registry = Registry::at(&self.cfg.home);
        let book = registry.load().map_err(LeaveError::Failed)?;
        let agent = book
            .agents
            .iter()
            .find(|a| a.name == name)
            .ok_or_else(|| LeaveError::Failed(format!("no agent named {name}")))?;
        let key = agent.note.clone();
        let entry = self
            .notes
            .list()
            .map_err(|e| LeaveError::Failed(e.to_string()))?
            .into_iter()
            .find(|n| n.commitment == key)
            .ok_or_else(|| LeaveError::Failed(format!("the budget note of {name} is not here")))?;
        let id = entry.note_id.ok_or(LeaveError::NoNote(0))?;

        // 1. Shut the agent out.
        registry.disable(name).map_err(LeaveError::Failed)?;
        self.revoking
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(key.clone());

        // 2. Retire the open lease and wait for the operator to settle it.
        if self.sessions.lock().await.contains_key(&key) {
            self.retire(&key)
                .await
                .map_err(|e| LeaveError::Failed(e.message))?;
            for _ in 0..opts.settle_polls {
                self.settle().await;
                if !self.sessions.lock().await.contains_key(&key) {
                    break;
                }
                tokio::time::sleep(opts.settle_every).await;
            }
            if self.sessions.lock().await.contains_key(&key) {
                return Err(LeaveError::Failed(format!(
                    "the lease on note {id} has not settled yet; run the command again"
                )));
            }
        }

        // 3. Ask for a clearance and close. 4. No clearance: file an exit.
        let family = opts.close.family.clone();
        let mut why = String::from("the note was already exiting");
        if entry.status == NoteStatus::Accepted {
            match self.close_note(id, opts.close, ask).await {
                Ok(closed) => return Ok(Revoked::Closed(closed)),
                Err(LeaveError::Refused(answer)) => why = answer,
                Err(other) => return Err(other),
            }
        }
        match self.exit_note(id, family.as_deref(), ask).await {
            Ok(exit) => Ok(Revoked::Exiting(exit, why)),
            Err(e) => Err(LeaveError::Failed(format!(
                "{e} (after the operator's refusal: {why})"
            ))),
        }
    }
}
