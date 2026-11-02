//! A lease after admission: retirement (by request, idle or at the cap), the drain, the key's
//! deactivation, the usage read, settlement and the signed next state.
//!
//! Statuses: `active` -> `retiring` (no new requests, in-flight ones draining) -> `revoked` (key
//! deactivated, usage not yet readable) -> `retired` (signed usage record stored) -> `settled`
//! (charge fixed, next state signed, nullifier `finalized`). Every move is a compare-and-set on
//! the status, and `tick` is safe to run at any time and any number of times.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use serde_json::{json, Value};
use zkapi_types::wire::{ApiRequestV2, RecoveryResponseV2, RequestResponseV2};

use crate::charge::{charge, ChargeParams};
use crate::error::{ApiError, ApiResult, Code};
use crate::operator::{Operator, LEASE_MAX_SECS, SETTLE_AFTER_SECS};
use crate::provider::ProviderError;
use crate::signer::{SignRequest, SignerError};
use crate::store::{self, LeaseRow};
use crate::token;
use crate::usage::{SignedUsage, UsageRecord};

/// Longest any provider or signer call may hold the lifecycle lock.
const CALL_TIMEOUT: Duration = Duration::from_secs(10);

async fn timed<T>(
    call: impl std::future::Future<Output = Result<T, ProviderError>>,
) -> Result<T, ProviderError> {
    tokio::time::timeout(CALL_TIMEOUT, call)
        .await
        .unwrap_or_else(|_| Err(ProviderError::Unavailable("timed out".into())))
}

/// A lease with no request for this long is retired.
pub const IDLE_SECS: u64 = 5 * 60;
/// Requests in flight get this long to drain before they are cancelled.
pub const DRAIN_SECS: u64 = 120;
/// Usage is read at least this long after the key is deactivated.
pub const USAGE_WAIT_SECS: u64 = 10;
/// A lease whose usage cannot be read settles at its bound after this long.
pub const UNREAD_SETTLES_AFTER_SECS: u64 = 6 * 60 * 60;

/// The lease takes no new requests: it is retiring.
#[derive(Debug)]
pub struct LeaseClosing;

/// Held while a request runs under a lease; dropping it ends the request.
pub struct InflightGuard {
    inflight: Arc<Mutex<std::collections::HashMap<String, u32>>>,
    lease_id: String,
}

impl Drop for InflightGuard {
    fn drop(&mut self) {
        let mut map = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(n) = map.get_mut(&self.lease_id) {
            *n = n.saturating_sub(1);
        }
    }
}

fn db(_: rusqlite::Error) -> ApiError {
    ApiError::new(Code::ReplicaUnreachable, "the operator's store failed").retry_after(5)
}

fn api_status(row: &LeaseRow) -> &'static str {
    match row.status.as_str() {
        "provisioning" => "provisioning",
        "active" => "active",
        "settled" => "finalized",
        _ => "retiring",
    }
}

impl Operator {
    /// Starts a request under a lease; refused once the lease is closing. The proxy path
    /// calls this before it forwards anything.
    pub fn begin_request(&self, lease_id: &str) -> Result<InflightGuard, LeaseClosing> {
        // The in-flight lock is held across the check and the increment, so a retirement cannot
        // slip between them: it either sees this request or this request sees it closing.
        let mut inflight = self.inflight.lock().unwrap_or_else(|e| e.into_inner());
        if self
            .closing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(lease_id)
        {
            return Err(LeaseClosing);
        }
        *inflight.entry(lease_id.to_string()).or_insert(0) += 1;
        Ok(InflightGuard {
            inflight: self.inflight.clone(),
            lease_id: lease_id.to_string(),
        })
    }

    /// The first request proxied under a lease counts as delivery; every request is activity.
    ///
    /// A failed write is an error the proxy must act on: a delivery that is not recorded would
    /// let `expire` close a used lease for nothing.
    pub async fn mark_delivered(&self, lease_id: &str) -> ApiResult<()> {
        let (id, now) = (lease_id.to_string(), self.clock.now_secs());
        let first = self
            .store
            .run(move |c| store::mark_delivered(c, &id, now))
            .await
            .map_err(db)?;
        if first {
            // The second watcher needs it to raise a concession; a miss is sent again later.
            let _ = self
                .replica
                .record(crate::replog::kinds::DELIVERED, lease_id, None)
                .await;
        }
        Ok(())
    }

    pub(crate) async fn authorized(
        &self,
        lease_id: &str,
        token: Option<&str>,
    ) -> ApiResult<LeaseRow> {
        let id = lease_id.to_string();
        let row = self
            .store
            .run(move |c| store::lease(c, &id))
            .await
            .map_err(db)?;
        let row = row.ok_or_else(|| ApiError::new(Code::LeaseNotFound, "unknown lease"))?;
        if !token.is_some_and(|t| token::matches(&row.token_hash, t)) {
            return Err(ApiError::new(
                Code::LeaseTokenInvalid,
                "missing or wrong lease token",
            ));
        }
        Ok(row)
    }

    fn lease_value(&self, row: &LeaseRow) -> Value {
        let issued = row.issued_at.unwrap_or(0);
        let mut v = json!({
            "status": api_status(row),
            "client_request_id": row.client_request_id,
            "issued_at": issued,
            "expires_at": row.expires_at.unwrap_or(issued + LEASE_MAX_SECS),
            "settle_after": issued + SETTLE_AFTER_SECS,
            "spending_limit_usd": row.bound as f64 / 1_000_000.0,
        });
        if let Some(evidence) = row
            .evidence_json
            .as_deref()
            .and_then(|j| serde_json::from_str::<Value>(j).ok())
        {
            v["provider_evidence"] = evidence;
        }
        if let Some(usage) = row
            .usage_json
            .as_deref()
            .and_then(|j| serde_json::from_str::<SignedUsage>(j).ok())
            .and_then(|u| u.record.cost_nano_usd)
            .and_then(|c| c.parse::<u128>().ok())
        {
            v["usage_usd"] = (usage as f64 / 1e9).into();
        }
        if let Some(charge) = row.charge {
            v["charge_applied"] = charge.to_string().into();
            v["charge_basis"] = row.charge_basis.clone().into();
        } else {
            let from = row.retiring_at.unwrap_or(issued + LEASE_MAX_SECS);
            v["settlement_deadline"] = (from + UNREAD_SETTLES_AFTER_SECS).into();
        }
        v
    }

    /// `GET /v2/openrouter/leases/{id}`: the lease's status, for its holder.
    pub async fn lease_status(&self, lease_id: &str, token: Option<&str>) -> ApiResult<Value> {
        Ok(self.lease_value(&self.authorized(lease_id, token).await?))
    }

    /// `POST /v2/openrouter/leases/{id}`: retire. Settlement follows once the key's usage can be
    /// read; poll the lease or `GET /v2/requests/{client_request_id}`.
    pub async fn retire(
        &self,
        lease_id: &str,
        token: Option<&str>,
    ) -> ApiResult<(StatusCode, Value)> {
        self.authorized(lease_id, token).await?;
        let _step = self.lifecycle.lock().await;
        let id = lease_id.to_string();
        let row = self
            .store
            .run(move |c| store::lease(c, &id))
            .await
            .map_err(db)?;
        let row = row.ok_or_else(|| ApiError::new(Code::LeaseNotFound, "unknown lease"))?;
        if row.status != "active" {
            return Err(ApiError::new(
                Code::LeaseEnded,
                "the lease is already retired",
            ));
        }
        self.begin_retire(&row).await?;
        let id = lease_id.to_string();
        let row = self
            .store
            .run(move |c| store::lease(c, &id))
            .await
            .map_err(db)?;
        Ok((
            StatusCode::OK,
            self.lease_value(&row.expect("lease exists")),
        ))
    }

    async fn begin_retire(&self, row: &LeaseRow) -> ApiResult<()> {
        self.closing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(row.lease_id.clone());
        let (id, now) = (row.lease_id.clone(), self.clock.now_secs());
        self.store
            .run(move |c| store::advance(c, &id, "active", "retiring", "retiring_at", now))
            .await
            .map_err(db)?;
        Ok(())
    }

    /// `POST /v2/openrouter/leases/{id}/expire`: closes a lease no request was proxied under.
    /// The charge is 0, the nullifier is unbound again and the stored proof is deleted.
    pub async fn expire(&self, lease_id: &str, token: Option<&str>) -> ApiResult<Value> {
        self.authorized(lease_id, token).await?;
        let _step = self.lifecycle.lock().await;
        let id = lease_id.to_string();
        let row = self
            .store
            .run(move |c| store::lease(c, &id))
            .await
            .map_err(db)?;
        let row = row.ok_or_else(|| ApiError::new(Code::LeaseNotFound, "unknown lease"))?;
        // Stop new requests first, then look: a request already running counts as delivery.
        self.closing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(lease_id.to_string());
        let running = self
            .inflight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(lease_id)
            .copied()
            .unwrap_or(0);
        if row.delivered || running > 0 {
            self.closing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(lease_id);
            return Err(ApiError::new(
                Code::LeaseUsed,
                "a request was proxied under this lease",
            ));
        }
        if row.status != "active" {
            return Err(ApiError::new(
                Code::LeaseEnded,
                "the lease is already retired",
            ));
        }
        let _ = timed(self.provider.revoke(lease_id)).await;
        let id = lease_id.to_string();
        self.store
            .run(move |c| store::roll_back(c, &id))
            .await
            .map_err(db)?;
        // Without this the replica would bring the lease back after a restore and settle it at
        // its bound, charging a user who cancelled.
        let _ = self
            .replica
            .record(crate::replog::kinds::ROLLED_BACK, lease_id, None)
            .await;
        self.ledger.release(lease_id);
        self.spend
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .forget(lease_id);
        self.closing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(lease_id);
        self.issued
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&row.nullifier);
        Ok(json!({
            "status": "cancelled", "client_request_id": row.client_request_id,
            "charge_applied": "0", "charge_basis": "unissued",
        }))
    }

    /// `GET /v2/requests/{client_request_id}`: the charge and the signed next state once they
    /// exist. Never a key. A request this operator never saw is a 404, which the wallet reads
    /// as "nothing yet".
    pub async fn request_status(&self, client_request_id: &str) -> ApiResult<Value> {
        let id = client_request_id.to_string();
        let row = self
            .store
            .run(move |c| store::lease_by_client_request_id(c, &id))
            .await
            .map_err(db)?;
        let row = row.ok_or_else(|| ApiError::new(Code::LeaseNotFound, "unknown request"))?;
        let recovery = self.recovery_of(&row).await?;
        Ok(serde_json::to_value(recovery).expect("recovery serialises"))
    }

    /// What a lease says about its nullifier: the signed next state once it exists, otherwise
    /// the time by which the operator settles. Never a key.
    pub(crate) async fn recovery_of(&self, row: &LeaseRow) -> ApiResult<RecoveryResponseV2> {
        let lease_id = row.lease_id.clone();
        let signed = self
            .store
            .run(move |c| store::signed_state(c, &lease_id))
            .await
            .map_err(db)?;
        Ok(match signed {
            Some((_, _, json)) => RecoveryResponseV2 {
                status: "finalized".into(),
                nullifier_status: "finalized".into(),
                request_response: Some(
                    serde_json::from_str::<RequestResponseV2>(&json)
                        .map_err(|_| db(rusqlite::Error::InvalidQuery))?,
                ),
                settlement_deadline: None,
            },
            // Settled at the bound while the second machine was away: nothing was signed, and
            // nothing will be. The client leaves this note by exit.
            None if row.status == "settled" => RecoveryResponseV2 {
                status: "finalized".into(),
                nullifier_status: "finalized".into(),
                request_response: None,
                settlement_deadline: None,
            },
            None => RecoveryResponseV2 {
                status: "pending".into(),
                nullifier_status: "reserved".into(),
                request_response: None,
                settlement_deadline: Some(
                    row.retiring_at
                        .unwrap_or(row.issued_at.unwrap_or(0) + LEASE_MAX_SECS)
                        + UNREAD_SETTLES_AFTER_SECS,
                ),
            },
        })
    }

    /// A replica that is diverged or unreachable raises an alarm; one that is back clears it.
    async fn alarm_on_replica_health(&self) {
        for (health, what) in [
            ("diverged", "its log and ours disagree; nothing is signed until a person looks"),
            ("unreachable", "no signature can leave while the second machine does not answer"),
        ] {
            let key = format!("replica_{health}");
            if self.replica.health() == health {
                self.alarms
                    .raise(&key, &format!("Replica {health}"), what)
                    .await;
            } else {
                self.alarms.resolve(&key);
            }
        }
    }

    /// Runs every due lifecycle step once.
    pub async fn tick(&self) {
        let _step = self.lifecycle.lock().await;
        let now = self.clock.now_secs();
        // The replica is compared with every few seconds: this delivers the backlog of status
        // facts and notices a disk that fell behind it.
        let sync_due = {
            let mut at = self.synced_at.lock().unwrap_or_else(|e| e.into_inner());
            let due = now >= *at + crate::operator::SYNC_EVERY_SECS;
            if due {
                *at = now;
            }
            due
        };
        if sync_due || self.replica.in_recovery() {
            let _ = self.restore_from_replica().await;
            self.alarm_on_replica_health().await;
        }
        let purge_due = {
            let mut at = self.purged_at.lock().unwrap_or_else(|e| e.into_inner());
            let due = now >= *at + crate::operator::PURGE_EVERY_SECS;
            if due {
                *at = now;
            }
            due
        };
        if purge_due {
            let _ = self
                .store
                .run(move |c| store::purge_old(c, now, crate::replog::RETENTION_SECS))
                .await;
        }
        let balance_due = self.clock.now_ms().saturating_sub(self.balance_read_ms())
            >= self.config().balance_refresh_secs * 1000;
        if balance_due {
            self.refresh_balance().await;
            self.provider.housekeeping().await;
        }
        let checked = *self
            .models_checked_ms
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self.clock.now_ms().saturating_sub(checked) >= crate::proxy::MODEL_CHECK_EVERY_MS {
            self.check_models().await;
        }
        let Ok(leases) = self.store.run(|c| store::all_leases(c)).await else {
            return;
        };
        for lease in leases {
            match lease.status.as_str() {
                "active" => self.tick_active(&lease, now).await,
                "retiring" => self.tick_retiring(&lease, now).await,
                "revoked" => self.tick_revoked(&lease, now).await,
                "retired" => self.settle(&lease, now).await,
                crate::restore::RESTORED => self.settle_restored(&lease, now).await,
                _ => {}
            }
        }
    }

    async fn tick_active(&self, lease: &LeaseRow, now: u64) {
        let issued = lease.issued_at.unwrap_or(now);
        let idle_since = lease.last_activity.unwrap_or(issued);
        if now >= issued + LEASE_MAX_SECS || now >= idle_since + IDLE_SECS {
            let _ = self.begin_retire(lease).await;
        }
    }

    async fn tick_retiring(&self, lease: &LeaseRow, now: u64) {
        let running = self
            .inflight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&lease.lease_id)
            .copied()
            .unwrap_or(0);
        // Requests still running get their deadline; then they are cancelled and the lease moves on.
        if running > 0 && now < lease.retiring_at.unwrap_or(now) + DRAIN_SECS {
            return;
        }
        if running > 0 {
            // Out of patience: abort what is still running; the lease moves on next tick.
            self.cancel_requests(&lease.lease_id);
            self.inflight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&lease.lease_id);
        }
        let algo = self
            .algos
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&lease.lease_id)
            .cloned()
            .unwrap_or_else(|| "ed25519".into());
        let query = crate::provider::EvidenceQuery {
            nonce: token::random_hex(32),
            signing_algo: algo,
        };
        debug_assert!(
            crate::proxy::valid_nonce(&query.nonce)
                && crate::proxy::valid_signing_algo(&query.signing_algo)
        );
        if let Ok(Some(evidence)) = timed(self.provider.evidence(&lease.lease_id, &query)).await {
            let (id, json) = (lease.lease_id.clone(), evidence.to_string());
            let _ = self
                .store
                .run(move |c| store::store_evidence(c, &id, &json))
                .await;
        }
        match timed(self.provider.revoke(&lease.lease_id)).await {
            // Only a key the provider does not know is as good as deactivated.
            Ok(()) | Err(ProviderError::NotFound(_)) => {}
            // Any other failure is retried, until the six hours are up: then the lease moves on and
            // settles at its bound; the key is capped by its spend limit and expires by itself.
            Err(_) if now < lease.retiring_at.unwrap_or(now) + UNREAD_SETTLES_AFTER_SECS => return,
            Err(_) => {}
        }
        let id = lease.lease_id.clone();
        let _ = self
            .store
            .run(move |c| store::advance(c, &id, "retiring", "revoked", "revoked_at", now))
            .await;
    }

    async fn tick_revoked(&self, lease: &LeaseRow, now: u64) {
        if now < lease.revoked_at.unwrap_or(now) + USAGE_WAIT_SECS {
            return;
        }
        let cost = match timed(self.provider.usage(&lease.lease_id)).await {
            Ok(usage) => {
                self.judge_lease_cost(&lease.lease_id, usage.cost_nano_usd)
                    .await;
                Some(usage.cost_nano_usd.to_string())
            }
            Err(_) if now >= lease.retiring_at.unwrap_or(now) + UNREAD_SETTLES_AFTER_SECS => None,
            Err(_) => return,
        };
        let Ok(signed) = self
            .minter
            .sign(UsageRecord {
                lease_id: lease.lease_id.clone(),
                cost_nano_usd: cost,
                read_at: now,
            })
            .await
        else {
            return;
        };
        let (id, json) = (
            lease.lease_id.clone(),
            serde_json::to_string(&signed).expect("usage serialises"),
        );
        let _ = self
            .store
            .run(move |c| store::store_usage(c, &id, &json))
            .await;
    }

    async fn settle(&self, lease: &LeaseRow, now: u64) {
        let Some(usage) = lease
            .usage_json
            .as_deref()
            .and_then(|j| serde_json::from_str::<SignedUsage>(j).ok())
        else {
            return;
        };
        let params = {
            let cache = self.cache.lock().await;
            let Some(config) = cache.config.as_ref() else {
                return;
            };
            ChargeParams::from(config)
        };
        let held = lease.held_secs();
        let (mut amount, mut basis) = match usage
            .record
            .cost_nano_usd
            .as_deref()
            .map(str::parse::<u128>)
        {
            Some(Ok(cost)) => (charge(lease.bound, cost, held, params), "measured"),
            _ => (lease.bound, "worst_case"),
        };
        // A state already signed fixes the charge, whatever the parameters say now.
        let id = lease.lease_id.clone();
        if let Ok(Some((_, signed_charge, _))) =
            self.store.run(move |c| store::signed_state(c, &id)).await
        {
            if signed_charge != amount {
                amount = signed_charge;
                basis = if usage.record.cost_nano_usd.is_some() {
                    "measured"
                } else {
                    "worst_case"
                };
            }
        }
        let request = SignRequest {
            lease_id: lease.lease_id.clone(),
            charge: amount.to_string(),
            usage,
        };
        // A machine that is still restoring signs nothing.
        if self.replica.in_recovery() {
            return;
        }
        match tokio::time::timeout(CALL_TIMEOUT, self.signer.sign_next(request)).await {
            Ok(Ok(signed)) if signed.charge_applied == amount => {}
            // The second machine has been away for six hours: the lease is settled at its bound
            // and no next state is signed. The user leaves this note by exit.
            // A link that answers nothing times the call out; that is the same thing.
            Ok(Err(SignerError::ReplicaUnreachable)) | Err(_)
                if now >= lease.retiring_at.unwrap_or(now) + UNREAD_SETTLES_AFTER_SECS =>
            {
                amount = lease.bound;
                basis = "link_down";
            }
            _ => return,
        }
        let (id, basis) = (lease.lease_id.clone(), basis);
        let settled = self
            .store
            .run(move |c| store::settle(c, &id, amount, basis, now))
            .await;
        if settled.is_ok_and(|s| s) {
            let fact =
                serde_json::json!({ "charge": amount.to_string(), "basis": basis }).to_string();
            let _ = self
                .replica
                .record(crate::replog::kinds::SETTLED, &lease.lease_id, Some(&fact))
                .await;
            self.ledger.settle(&lease.lease_id, amount, now);
            self.closing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&lease.lease_id);
            self.spend
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .forget(&lease.lease_id);
            self.algos
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&lease.lease_id);
            self.issued
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&lease.nullifier);
        }
    }

    /// A lease that came back from the replica has no key and no token: six hours after the
    /// restore it is settled at its bound, with no next state. (An honest client settles it
    /// sooner through `POST /v2/reconcile`.)
    async fn settle_restored(&self, lease: &LeaseRow, now: u64) {
        if self.replica.in_recovery() {
            return;
        }
        // A lease with no stamp was restored by a start-up that stopped half way: the clock
        // starts now, and its provider key (which nobody holds a record of) goes.
        let Some(stamp) = lease.retiring_at else {
            let id = lease.lease_id.clone();
            let _ = self.provider.revoke(&id).await;
            let _ = self
                .store
                .run(move |c| {
                    c.execute(
                        "UPDATE leases SET retiring_at = ?2 WHERE lease_id = ?1 AND retiring_at IS NULL",
                        rusqlite::params![id, now],
                    )
                    .map(|_| ())
                })
                .await;
            return;
        };
        if now < stamp + UNREAD_SETTLES_AFTER_SECS {
            return;
        }
        let _ = self.settle_at_bound(lease, "recovered").await;
    }

    /// Settles a restored lease at its bound and tells the second machine.
    pub(crate) async fn settle_at_bound(&self, lease: &LeaseRow, basis: &'static str) -> bool {
        let (id, now) = (lease.lease_id.clone(), self.clock.now_secs());
        let done = self
            .store
            .run(move |c| crate::restore::settle_restored(c, &id, basis, now))
            .await
            .unwrap_or(false);
        if done {
            self.ledger.settle(&lease.lease_id, lease.bound, now);
            self.closing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&lease.lease_id);
            let fact = json!({ "charge": lease.bound.to_string(), "basis": basis }).to_string();
            let _ = self
                .replica
                .record(crate::replog::kinds::SETTLED, &lease.lease_id, Some(&fact))
                .await;
        }
        done
    }

    /// `POST /v2/reconcile`: an honest client that lost a lease to a restore proves it holds the
    /// request proof, and the lease is settled at its bound. No next state is signed; the client
    /// leaves the note by exit.
    pub async fn reconcile(&self, body: &[u8]) -> ApiResult<Value> {
        {
            let now = self.clock.now_secs();
            let mut recent = self.reconciles.lock().unwrap_or_else(|e| e.into_inner());
            while recent.front().is_some_and(|t| *t + 60 <= now) {
                recent.pop_front();
            }
            if recent.len() >= crate::operator::RECONCILES_PER_MINUTE {
                let wait = recent.front().map_or(1, |t| (*t + 60).saturating_sub(now).max(1));
                return Err(ApiError::new(
                    Code::OperatorCapacity,
                    "too many reconcile calls this minute",
                )
                .retry_after(wait));
            }
            recent.push_back(now);
        }
        let value = crate::json::parse_strict(body)
            .map_err(|e| ApiError::new(Code::MalformedRequest, e.to_string()))?;
        let (Some(lease_id), request) = (value["lease_id"].as_str(), &value["request"]) else {
            return Err(ApiError::new(
                Code::MalformedRequest,
                "lease_id and request are required",
            ));
        };
        let canonical = crate::operator::parse_request(request.to_string().as_bytes())?.1;
        let id = lease_id.to_string();
        let (lease, stored) = self
            .store
            .run(move |c| Ok((store::lease(c, &id)?, store::request_json(c, &id)?)))
            .await
            .map_err(db)?;
        let (Some(lease), Some(stored)) = (lease, stored) else {
            return Err(ApiError::new(Code::LeaseNotFound, "unknown lease"));
        };
        // The proof the client holds is the one on file: that is who it is.
        if serde_json::from_str::<Value>(&stored).ok().as_ref() != Some(&canonical) {
            return Err(ApiError::new(
                Code::ProofInvalid,
                "the request does not match the one on file",
            ));
        }
        if lease.status != crate::restore::RESTORED {
            return Err(ApiError::new(
                Code::LeaseNotFound,
                "the lease is not waiting for reconciliation",
            ));
        }
        if !self.settle_at_bound(&lease, "reconciled").await {
            return Err(ApiError::new(Code::ReplicaUnreachable, "try again").retry_after(5));
        }
        Ok(json!({
            "status": "finalized",
            "nullifier_status": "finalized",
            "charge_applied": lease.bound.to_string(),
            "next_state": Value::Null,
        }))
    }

    /// Runs [`Operator::tick`] every second for as long as the process lives.
    pub fn spawn_background(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let operator = self.clone();
        tokio::spawn(async move {
            loop {
                operator.tick().await;
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        })
    }
}

/// The request proof stored with a lease, for the watcher and for tests.
pub async fn stored_request(operator: &Operator, lease_id: &str) -> Option<ApiRequestV2> {
    let id = lease_id.to_string();
    let json = operator
        .store
        .run(move |c| store::request_json(c, &id))
        .await
        .ok()??;
    serde_json::from_str(&json).ok()
}
