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
use crate::signer::SignRequest;
use crate::store::{self, LeaseRow};
use crate::token;
use crate::usage::{SignedUsage, UsageRecord};

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
        if self
            .closing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(lease_id)
        {
            return Err(LeaseClosing);
        }
        *self
            .inflight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(lease_id.to_string())
            .or_insert(0) += 1;
        Ok(InflightGuard {
            inflight: self.inflight.clone(),
            lease_id: lease_id.to_string(),
        })
    }

    /// The first request proxied under a lease counts as delivery; every request is activity.
    pub async fn mark_delivered(&self, lease_id: &str) {
        let (id, now) = (lease_id.to_string(), self.clock.now_secs());
        let _ = self
            .store
            .run(move |c| store::mark_delivered(c, &id, now))
            .await;
    }

    async fn authorized(&self, lease_id: &str, token: Option<&str>) -> ApiResult<LeaseRow> {
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
        if row.delivered {
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
        let _ = self.provider.revoke(lease_id).await;
        let id = lease_id.to_string();
        self.store
            .run(move |c| store::roll_back(c, &id))
            .await
            .map_err(db)?;
        self.ledger.release(lease_id);
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
        let lease_id = row.lease_id.clone();
        let signed = self
            .store
            .run(move |c| store::signed_state(c, &lease_id))
            .await
            .map_err(db)?;
        let recovery = match signed {
            Some((_, _, json)) => RecoveryResponseV2 {
                status: "finalized".into(),
                nullifier_status: "finalized".into(),
                request_response: Some(
                    serde_json::from_str::<RequestResponseV2>(&json)
                        .map_err(|_| db(rusqlite::Error::InvalidQuery))?,
                ),
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
        };
        Ok(serde_json::to_value(recovery).expect("recovery serialises"))
    }

    /// Runs every due lifecycle step once.
    pub async fn tick(&self) {
        let _step = self.lifecycle.lock().await;
        let now = self.clock.now_secs();
        let Ok(leases) = self.store.run(|c| store::all_leases(c)).await else {
            return;
        };
        for lease in leases {
            match lease.status.as_str() {
                "active" => self.tick_active(&lease, now).await,
                "retiring" => self.tick_retiring(&lease, now).await,
                "revoked" => self.tick_revoked(&lease, now).await,
                "retired" => self.settle(&lease, now).await,
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
        if let Ok(Some(evidence)) = self.provider.evidence(&lease.lease_id).await {
            self.evidence
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(lease.lease_id.clone(), evidence);
        }
        match self.provider.revoke(&lease.lease_id).await {
            // An unknown key is as good as deactivated.
            Ok(()) | Err(ProviderError::Rejected(_)) => {}
            Err(ProviderError::Unavailable(_)) => return,
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
        let cost = match self.provider.usage(&lease.lease_id).await {
            Ok(usage) => Some(usage.cost_nano_usd.to_string()),
            Err(_) if now >= lease.retiring_at.unwrap_or(now) + UNREAD_SETTLES_AFTER_SECS => None,
            Err(_) => return,
        };
        let signed = self.minter.sign(UsageRecord {
            lease_id: lease.lease_id.clone(),
            cost_nano_usd: cost,
            read_at: now,
        });
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
            ChargeParams {
                step: config.charge_step,
                min_charge: config.min_charge,
                slot_rate: config.slot_rate,
            }
        };
        let held = lease
            .revoked_at
            .unwrap_or(0)
            .saturating_sub(lease.issued_at.unwrap_or(0));
        let (amount, basis) = match usage
            .record
            .cost_nano_usd
            .as_deref()
            .map(str::parse::<u128>)
        {
            Some(Ok(cost)) => (charge(lease.bound, cost, held, params), "measured"),
            _ => (lease.bound, "worst_case"),
        };
        let request = SignRequest {
            lease_id: lease.lease_id.clone(),
            charge: amount.to_string(),
            usage,
        };
        let Ok(signed) = self.signer.sign_next(request).await else {
            return;
        };
        if signed.charge_applied != amount {
            return;
        }
        let (id, basis) = (lease.lease_id.clone(), basis);
        let settled = self
            .store
            .run(move |c| store::settle(c, &id, amount, basis, now))
            .await;
        if settled.is_ok_and(|s| s) {
            self.ledger.settle(&lease.lease_id, amount, now);
            self.closing
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&lease.lease_id);
            self.issued
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&lease.nullifier);
        }
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
