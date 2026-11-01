//! Lease admission: the cheap checks, the verification pool, the budget, the nullifier
//! compare-and-set and the reservation order (disk, replica, provider key).

use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use lru::LruCache;
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use zkapi_proof::compact::CompactSigner;
use zkapi_types::wire::{ApiRequestV2, OpenRouterLeaseAuthorization};
use zkapi_types::{canonical_payload_hash, Felt252, PROTOCOL_VERSION};

use crate::budget::{BudgetConfig, Ledger, Refusal};
use crate::chain::{VaultConfig, VaultView};
use crate::clock::Clock;
use crate::error::{ApiError, ApiResult, Code};
use crate::json::{only_keys, parse_strict};
use crate::provider::ProviderAdapter;
use crate::puzzle;
use crate::replica::{Replica, ReplicaEntry};
use crate::signer::SignerApi;
use crate::status::{StatusSigner, SLOT_SECS};
use crate::store::{self, Cas, NewLease, Store};
use crate::token;
use crate::usage::Minter;
use crate::verify::{point_checks, ProofVerifier};

/// A request proof is valid for 300 seconds, with 30 seconds of clock skew either way.
pub const PROOF_LIFE_SECS: u64 = 300;
pub const PROOF_SKEW_SECS: u64 = 30;
/// A replaced root stays acceptable this long.
pub const ROOT_GRACE_MS: u64 = 60_000;
/// No lease lasts longer than this.
pub const LEASE_MAX_SECS: u64 = 600;
/// Usage is read at least 10 seconds after the key is deactivated; settlement waits for it.
pub const SETTLE_AFTER_SECS: u64 = LEASE_MAX_SECS + 10;
pub const LEASE_BODY_LIMIT: usize = 64 * 1024;
pub const MAX_CLIENT_REQUEST_ID: usize = 36;
pub const MAX_PAYLOAD: usize = 2 * 1024;

/// Where the operator runs. Only the sandbox may start without a replica.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Profile {
    Sandbox,
    Testnet,
    Mainnet,
}

#[derive(Clone)]
pub struct Config {
    pub profile: Profile,
    pub vault_account: String,
    pub chain_id: u64,
    pub contract_address: Felt252,
    /// Host the status nonce is signed for: the one clients dial.
    pub host: String,
    /// The one browser origin CORS admits.
    pub web_origin: String,
    pub budget: BudgetConfig,
    /// Team note ids, also published as a file in the repository.
    pub team_note_ids: Vec<u32>,
    pub max_verifications: usize,
    pub attempts_cap: usize,
    pub attempts_max: u32,
    pub attempts_window_secs: u64,
    pub puzzle_difficulty: u32,
    pub puzzle_load_pct: u32,
    pub config_refresh_secs: u64,
    pub chain_refresh_ms: u64,
    pub balance_refresh_secs: u64,
    pub mint_timeout_secs: u64,
}

impl Config {
    pub fn new(
        vault_account: &str,
        chain_id: u64,
        contract_address: Felt252,
        web_origin: &str,
    ) -> Self {
        Self {
            profile: Profile::Sandbox,
            vault_account: vault_account.into(),
            chain_id,
            contract_address,
            host: "localhost".into(),
            web_origin: web_origin.into(),
            budget: BudgetConfig::default(),
            team_note_ids: Vec::new(),
            max_verifications: 4,
            attempts_cap: 65_536,
            attempts_max: 3,
            attempts_window_secs: 300,
            puzzle_difficulty: 14,
            puzzle_load_pct: 50,
            config_refresh_secs: 300,
            chain_refresh_ms: 1_000,
            balance_refresh_secs: 300,
            mint_timeout_secs: 10,
        }
    }
}

/// Reads the team's note ids from the JSON file published in the repository (`[0, 1, ...]`).
pub fn load_team_note_ids(path: &std::path::Path) -> Result<Vec<u32>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

#[derive(Debug, thiserror::Error)]
pub enum StartError {
    #[error("vault read failed: {0}")]
    Vault(String),
    #[error("the vault's operator key is not this operator's state key")]
    KeyMismatch,
    #[error("the vault's chain id differs from the configured one")]
    ChainMismatch,
    #[error("a testnet or mainnet vault needs replication before the first signature, which is not built")]
    NoReplication,
    #[error("store: {0}")]
    Store(String),
    #[error("provider: {0}")]
    Provider(String),
}

#[derive(Default)]
pub(crate) struct Cache {
    pub(crate) config: Option<VaultConfig>,
    config_at_ms: u64,
    /// Set by `params_applied`; cleared by the next successful read of `config()`.
    dirty: bool,
    root: String,
    /// Root -> the millisecond we saw it replaced (`None` while current).
    roots: HashMap<String, Option<u64>>,
    epoch: u64,
    refreshed_ms: u64,
    open_notes: u32,
    team_open: u32,
}

struct Attempt {
    count: u32,
    first_secs: u64,
}

#[derive(Default)]
struct Load {
    slot: u64,
    peak_now: u32,
    peak_prev: u32,
}

pub struct Operator {
    cfg: Config,
    pub(crate) clock: Arc<dyn Clock>,
    vault: Arc<dyn VaultView>,
    pub(crate) provider: Arc<dyn ProviderAdapter>,
    replica: Arc<dyn Replica>,
    verifier: Arc<dyn ProofVerifier>,
    pub(crate) store: Store,
    pub(crate) ledger: Ledger,
    status: StatusSigner,
    state_pk: zkapi_types::wire::CurvePointWire,
    pub(crate) cache: tokio::sync::Mutex<Cache>,
    balance_at_ms: Mutex<u64>,
    /// Held across the compare-and-set, the replica acknowledgement and the key mint.
    admission: tokio::sync::Mutex<()>,
    attempts: Mutex<LruCache<String, Attempt>>,
    verify_slots: Arc<Semaphore>,
    load: Mutex<Load>,
    /// Issued 201 bodies by nullifier, so a duplicate POST returns the same lease. Memory only:
    /// the token is stored hashed and cannot be rebuilt.
    pub(crate) issued: Mutex<HashMap<String, (Value, Value)>>,
    pub(crate) signer: Arc<dyn SignerApi>,
    pub(crate) minter: Minter,
    /// Requests in flight per lease, and the leases that take no new ones.
    pub(crate) inflight: Arc<Mutex<HashMap<String, u32>>>,
    pub(crate) closing: Mutex<HashSet<String>>,
    /// Serialises lifecycle steps (retire, expire, tick).
    pub(crate) lifecycle: tokio::sync::Mutex<()>,
    pub(crate) evidence: Mutex<HashMap<String, Value>>,
}

pub struct Parts {
    pub cfg: Config,
    pub clock: Arc<dyn Clock>,
    pub vault: Arc<dyn VaultView>,
    pub provider: Arc<dyn ProviderAdapter>,
    pub replica: Arc<dyn Replica>,
    pub verifier: Arc<dyn ProofVerifier>,
    pub store: Store,
    pub state_signer: CompactSigner,
    pub signer: Arc<dyn SignerApi>,
    pub minter: Minter,
}

/// `0x` and 64 lowercase hex digits, the vault's spelling of a word (`Felt252::to_hex` trims).
pub fn felt_hex(f: &Felt252) -> String {
    format!("0x{}", hex::encode(f.as_bytes()))
}

impl Operator {
    /// Reads the vault, checks that it names this operator's key, sweeps provider keys that have
    /// no record and settles unacknowledged reservations.
    pub async fn start(parts: Parts) -> Result<Arc<Self>, StartError> {
        let Parts {
            cfg,
            clock,
            vault,
            provider,
            replica,
            verifier,
            store,
            state_signer,
            signer,
            minter,
        } = parts;
        if cfg.profile != Profile::Sandbox {
            return Err(StartError::NoReplication);
        }
        let state_pk = state_signer.public_key();
        let credit = provider
            .balance()
            .await
            .map_err(|e| StartError::Provider(e.to_string()))?;
        let now_ms = clock.now_ms();
        let operator = Arc::new(Self {
            ledger: Ledger::new(cfg.budget, credit, now_ms),
            status: StatusSigner::new(state_signer, cfg.host.clone()),
            attempts: Mutex::new(LruCache::new(
                NonZeroUsize::new(cfg.attempts_cap.max(1)).expect("non-zero"),
            )),
            verify_slots: Arc::new(Semaphore::new(cfg.max_verifications)),
            cfg,
            clock,
            vault,
            provider,
            replica,
            verifier,
            store,
            state_pk,
            cache: tokio::sync::Mutex::new(Cache::default()),
            balance_at_ms: Mutex::new(now_ms),
            admission: tokio::sync::Mutex::new(()),
            load: Mutex::new(Load::default()),
            issued: Mutex::new(HashMap::new()),
            signer,
            minter,
            inflight: Arc::new(Mutex::new(HashMap::new())),
            closing: Mutex::new(HashSet::new()),
            lifecycle: tokio::sync::Mutex::new(()),
            evidence: Mutex::new(HashMap::new()),
        });
        operator
            .refresh_chain(true)
            .await
            .map_err(|e| StartError::Vault(e.message))?;
        {
            let cache = operator.cache.lock().await;
            let config = cache.config.as_ref().expect("loaded by refresh");
            let pk = &operator.state_pk;
            if config.operator_pk.x != felt_hex(&pk.x) || config.operator_pk.y != felt_hex(&pk.y) {
                return Err(StartError::KeyMismatch);
            }
            if config.chain_id != u128::from(operator.cfg.chain_id) {
                return Err(StartError::ChainMismatch);
            }
        }
        operator.recover().await?;
        Ok(operator)
    }

    /// Start-up: provisioning reservations are rolled back and their keys revoked; unacknowledged
    /// active ones are resent or rolled back; keys with no record are revoked.
    async fn recover(&self) -> Result<(), StartError> {
        let leases = self
            .store
            .run(|c| store::all_leases(c))
            .await
            .map_err(|e| StartError::Store(e.to_string()))?;
        let known: HashSet<String> = leases.iter().map(|l| l.lease_id.clone()).collect();
        for lease in &leases {
            if lease.status == "settled" {
                continue;
            }
            let keep = if lease.status == "provisioning" {
                false
            } else if lease.replicated {
                true
            } else {
                self.replicate(&lease.lease_id).await.is_ok()
            };
            if keep {
                self.ledger.restore(&lease.lease_id, lease.bound);
                if lease.status != "active" {
                    self.closing
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .insert(lease.lease_id.clone());
                }
                continue;
            }
            let _ = self.provider.revoke(&lease.lease_id).await;
            let id = lease.lease_id.clone();
            self.store
                .run(move |c| store::roll_back(c, &id))
                .await
                .map_err(|e| StartError::Store(e.to_string()))?;
        }
        let day_start =
            self.clock.now_secs() / crate::budget::SECS_PER_DAY * crate::budget::SECS_PER_DAY;
        let settled = self
            .store
            .run(move |c| store::settled_since(c, day_start))
            .await
            .map_err(|e| StartError::Store(e.to_string()))?;
        self.ledger.restore_settled(settled, self.clock.now_secs());
        let listed = self
            .provider
            .list()
            .await
            .map_err(|e| StartError::Provider(e.to_string()))?;
        for lease_id in listed.into_iter().filter(|l| !known.contains(l)) {
            let _ = self.provider.revoke(&lease_id).await;
        }
        Ok(())
    }

    async fn replicate(&self, lease_id: &str) -> Result<(), ApiError> {
        let id = lease_id.to_string();
        let (lease, request) = self
            .store
            .run(move |c| Ok((store::lease(c, &id)?, store::request_json(c, &id)?)))
            .await
            .map_err(db_error)?;
        let (Some(lease), Some(request_json)) = (lease, request) else {
            return Err(ApiError::new(Code::ReplicaUnreachable, "no such lease"));
        };
        self.replica
            .append(&ReplicaEntry {
                nullifier: lease.nullifier,
                lease_id: lease.lease_id.clone(),
                request_json,
            })
            .await
            .map_err(|_| {
                ApiError::new(Code::ReplicaUnreachable, "the replica did not acknowledge")
                    .retry_after(5)
            })?;
        let id = lease.lease_id;
        self.store
            .run(move |c| store::mark_replicated(c, &id))
            .await
            .map_err(db_error)
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn ledger(&self) -> &Ledger {
        &self.ledger
    }

    pub fn status_signer(&self) -> &StatusSigner {
        &self.status
    }

    pub fn clock(&self) -> &Arc<dyn Clock> {
        &self.clock
    }

    /// The vault applied new parameters: refuse leases until `config()` is read again.
    pub async fn params_applied(&self) {
        self.cache.lock().await.dirty = true;
        let _ = self.refresh_chain(true).await;
    }

    /// Reads the provider's balance and corrects the ledger.
    pub async fn refresh_balance(&self) {
        let read =
            tokio::time::timeout(std::time::Duration::from_secs(5), self.provider.balance()).await;
        if let Ok(Ok(balance)) = read {
            self.ledger.set_balance(balance);
            *self.balance_at_ms.lock().unwrap_or_else(|e| e.into_inner()) = self.clock.now_ms();
        }
    }

    /// Reads the vault when the cache is old or `force`d. A failed read keeps the cache and
    /// reports the error; leases are refused while `config()` is dirty or too old.
    pub async fn refresh_chain(&self, force: bool) -> ApiResult<()> {
        let now_ms = self.clock.now_ms();
        let mut cache = self.cache.lock().await;
        let config_due = cache.config.is_none()
            || cache.dirty
            || now_ms.saturating_sub(cache.config_at_ms) >= self.cfg.config_refresh_secs * 1000;
        if !force
            && !config_due
            && now_ms.saturating_sub(cache.refreshed_ms) < self.cfg.chain_refresh_ms
        {
            return Ok(());
        }
        let unreachable = |e: crate::chain::ChainError| {
            {
                let _ = e; // the detail can name an RPC URL; clients get a fixed message
                ApiError::new(Code::OperatorCapacity, "the vault could not be read").retry_after(5)
            }
        };
        let root = self
            .vault
            .current_root()
            .await
            .map_err(unreachable)?
            .to_ascii_lowercase();
        let epoch = self.vault.retirement_epoch().await.map_err(unreachable)?;
        let open_notes = self.vault.open_notes().await.map_err(unreachable)?;
        let exits = self.vault.exits().await.map_err(unreachable)?;
        let team_open = if self.cfg.team_note_ids.is_empty() {
            0
        } else {
            self.vault
                .open_among(&self.cfg.team_note_ids)
                .await
                .map_err(unreachable)?
        };
        if config_due || force {
            cache.config = Some(self.vault.config().await.map_err(unreachable)?);
            cache.config_at_ms = now_ms;
            cache.dirty = false;
        }
        if epoch != cache.epoch {
            // The retirement epoch moved: closed notes must not open leases on an old root.
            // Nothing is kept for grace: the old root of a closed note must not outlive the epoch.
            cache.roots.clear();
            if !cache.root.is_empty() && cache.root == root {
                let current = cache.root.clone();
                cache.roots.insert(current, None);
            }
            cache.epoch = epoch;
        }
        if root != cache.root {
            let old = std::mem::replace(&mut cache.root, root.clone());
            if cache.roots.contains_key(&old) {
                cache.roots.insert(old, Some(now_ms));
            }
            cache.roots.insert(root, None);
        }
        cache.open_notes = open_notes;
        cache.team_open = team_open;
        cache.refreshed_ms = now_ms;
        drop(cache);
        for exit in exits {
            for n in [exit.nullifier, exit.next_nullifier] {
                let _ = self.import_exit(&n.to_ascii_lowercase()).await;
            }
        }
        Ok(())
    }

    fn root_accepted(&self, cache: &Cache, root: &str, now_ms: u64) -> bool {
        match cache.roots.get(root) {
            Some(None) => true,
            Some(Some(replaced)) => now_ms.saturating_sub(*replaced) <= ROOT_GRACE_MS,
            None => false,
        }
    }

    /// Records a nullifier seen in a pending exit. `Err(state)` when the row is in another
    /// state (a lease holds it: the watcher answers that exit).
    pub async fn import_exit(&self, nullifier: &str) -> Result<(), String> {
        self.claim(nullifier, store::state::EXIT).await
    }

    /// Reserves a nullifier for a clearance. `Err(state)` when a lease, an exit or another
    /// clearance holds it.
    pub async fn reserve_clearance(&self, nullifier: &str) -> Result<(), String> {
        self.claim(nullifier, store::state::CLEARANCE_RESERVED)
            .await
    }

    async fn claim(&self, nullifier: &str, to: &'static str) -> Result<(), String> {
        let _guard = self.admission.lock().await;
        if to == store::state::EXIT {
            // Whatever the row says, the signer must know this nullifier is in an exit.
            let n = nullifier.to_string();
            self.store
                .run(move |c| store::record_exit_seen(c, &n))
                .await
                .map_err(|e| e.to_string())?;
        }
        let (n, now) = (nullifier.to_string(), self.clock.now_secs());
        match self
            .store
            .run(move |c| store::claim_nullifier(c, &n, to, now))
            .await
        {
            Ok(Cas::Done) => Ok(()),
            Ok(Cas::Conflict(row)) => Err(row.state),
            Err(e) => Err(e.to_string()),
        }
    }

    fn note_load(&self, held: u32) {
        let slot = self.clock.now_secs() / SLOT_SECS;
        let mut load = self.load.lock().unwrap_or_else(|e| e.into_inner());
        Self::rotate(&mut load, slot);
        load.peak_now = load.peak_now.max(held);
    }

    fn rotate(load: &mut Load, slot: u64) {
        if slot == load.slot {
            return;
        }
        load.peak_prev = if slot == load.slot + 1 {
            load.peak_now
        } else {
            0
        };
        load.peak_now = 0;
        load.slot = slot;
    }

    /// Verification load: the busiest moment of the last two slots as a percentage of the pool.
    pub fn load_pct(&self) -> u32 {
        let slot = self.clock.now_secs() / SLOT_SECS;
        let mut load = self.load.lock().unwrap_or_else(|e| e.into_inner());
        Self::rotate(&mut load, slot);
        let held = (self.cfg.max_verifications - self.verify_slots.available_permits()) as u32;
        let peak = load.peak_now.max(load.peak_prev).max(held);
        peak * 100 / self.cfg.max_verifications as u32
    }

    pub fn puzzle_difficulty(&self) -> u32 {
        if self.load_pct() > self.cfg.puzzle_load_pct {
            self.cfg.puzzle_difficulty
        } else {
            0
        }
    }

    /// The `/v1/status` document.
    pub async fn status_json(&self) -> Value {
        let _ = self.refresh_chain(false).await;
        let now = self.clock.now_secs();
        let cache = self.cache.lock().await;
        let config = cache.config.clone().expect("loaded at start");
        let slot = self.status.current(now);
        let (limit, used, resets_ts) = self.ledger.daily(now);
        json!({
            "version": 1,
            "operator_pk": { "x": config.operator_pk.x, "y": config.operator_pk.y },
            "clearance_pk": { "x": config.clearance_pk.x, "y": config.clearance_pk.y },
            "vault": self.cfg.vault_account,
            "note_fee": config.note_fee.to_string(),
            "close_fee": config.close_fee.to_string(),
            "bound_classes": config.bound_classes.iter().map(|b| b.to_string()).collect::<Vec<_>>(),
            "charge_step": config.charge_step.to_string(),
            "min_charge": config.min_charge.to_string(),
            "slot_rate": config.slot_rate,
            "open_notes": cache.open_notes,
            "external_notes": cache.open_notes.saturating_sub(cache.team_open),
            "team_note_ids": self.cfg.team_note_ids,
            "nonce": { "nonce": slot.nonce, "host": slot.host, "ts": slot.ts },
            "nonce_sig": slot.sig,
            "heartbeat_age_secs": Value::Null,
            "watchers": [],
            "degraded": false,
            "admission": {
                "load_pct": self.load_pct(),
                "retry_after_secs": RETRY_HINT_SECS,
                "puzzle_difficulty": self.puzzle_difficulty(),
            },
            "daily_budget": {
                "limit": limit.to_string(),
                "used": used.to_string(),
                "resets_ts": resets_ts,
            },
            "recovery_mode": false,
            "emergency_notice": Value::Null,
        })
    }

    fn count_attempt(&self, nullifier: &str) -> ApiResult<()> {
        let now = self.clock.now_secs();
        let mut attempts = self.attempts.lock().unwrap_or_else(|e| e.into_inner());
        let window = self.cfg.attempts_window_secs;
        let entry = attempts.get_or_insert_mut(nullifier.to_string(), || Attempt {
            count: 0,
            first_secs: now,
        });
        if now.saturating_sub(entry.first_secs) >= window {
            *entry = Attempt {
                count: 0,
                first_secs: now,
            };
        }
        if entry.count >= self.cfg.attempts_max {
            let wait = (entry.first_secs + window).saturating_sub(now).max(1);
            return Err(ApiError::new(
                Code::NullifierRateLimited,
                "too many attempts for this nullifier",
            )
            .retry_after(wait));
        }
        entry.count += 1;
        Ok(())
    }

    /// Entries in the attempt counter (tests check it stays at its cap).
    pub fn attempt_entries(&self) -> usize {
        self.attempts
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
    }

    /// `POST /v2/openrouter/leases`: the whole admission path. `puzzle` is the
    /// `X-Nymira-Puzzle` header.
    pub async fn open_lease(
        &self,
        body: &[u8],
        puzzle: Option<&str>,
    ) -> ApiResult<(StatusCode, Value)> {
        if body.len() > LEASE_BODY_LIMIT {
            return Err(ApiError::new(
                Code::BodyTooLarge,
                "request body over 64 KiB",
            ));
        }
        let now = self.clock.now_secs();
        let difficulty = self.puzzle_difficulty();
        if difficulty > 0 {
            let valid = self.status.recent_nonces(now);
            if !puzzle.is_some_and(|p| puzzle::check(p, difficulty, &valid)) {
                return Err(ApiError::new(
                    Code::PuzzleRequired,
                    format!("solve the hashcash puzzle at difficulty {difficulty}"),
                )
                .retry_after(RETRY_HINT_SECS));
            }
        }
        let (request, canonical) = parse_request(body)?;
        let inputs = &request.public_inputs;
        if inputs.protocol_version != PROTOCOL_VERSION {
            return Err(ApiError::new(
                Code::UnsupportedVersion,
                "protocol version not served",
            ));
        }
        if !point_checks(inputs) {
            return Err(ApiError::new(
                Code::ProofInvalid,
                "a curve point is invalid",
            ));
        }
        let nullifier = felt_hex(&inputs.request_nullifier);
        self.count_attempt(&nullifier)?;

        // Cheap checks, all before Groth16.
        if inputs.chain_id != self.cfg.chain_id
            || inputs.contract_address != self.cfg.contract_address
            || inputs.state_signing_key_x != self.state_pk.x
            || inputs.state_signing_key_y != self.state_pk.y
        {
            return Err(ApiError::new(
                Code::ProofInvalid,
                "public inputs name another deployment",
            ));
        }
        self.refresh_chain(false).await?;
        let root = felt_hex(&inputs.active_root);
        let now_ms = self.clock.now_ms();
        {
            let cache = self.cache.lock().await;
            if cache.dirty {
                return Err(ApiError::new(
                    Code::OperatorCapacity,
                    "vault parameters changed; re-reading them",
                )
                .retry_after(5));
            }
            let config = cache.config.as_ref().expect("loaded");
            if !config.bound_classes.contains(&inputs.solvency_bound) {
                return Err(ApiError::new(
                    Code::BoundClassInvalid,
                    "bound is not one of the vault's classes",
                ));
            }
            let (t, life) = (inputs.request_time, PROOF_LIFE_SECS + PROOF_SKEW_SECS);
            if t > now + PROOF_SKEW_SECS || t + life < now {
                return Err(ApiError::new(
                    Code::ProofExpired,
                    "request proof is outside its validity",
                ));
            }
            if !self.root_accepted(&cache, &root, now_ms) {
                return Err(ApiError::new(
                    Code::RootStale,
                    "root unknown or replaced too long ago",
                ));
            }
        }
        let bound = inputs.solvency_bound;
        let proof_ends = inputs.request_time + PROOF_LIFE_SECS;
        let existing = {
            let n = nullifier.clone();
            self.store
                .run(move |c| store::nullifier_row(c, &n))
                .await
                .map_err(db_error)?
        };
        if let Some(row) = existing {
            if row.state == store::state::RESERVED
                && row.client_request_id.as_deref() == Some(request.client_request_id.as_str())
            {
                // A repeat of a request still being served: wait for it to finish.
                drop(self.admission.lock().await);
            }
            let n = nullifier.clone();
            let row = self
                .store
                .run(move |c| store::nullifier_row(c, &n))
                .await
                .map_err(db_error)?;
            if let Some(row) = row {
                return self.seen(&nullifier, &row, &request, &canonical);
            }
            // The request it waited on rolled back: the nullifier is unbound, go on.
        }

        // Groth16, at most `max_verifications` at once, no queue.
        let permit = self.verify_slots.clone().try_acquire_owned().map_err(|_| {
            ApiError::new(Code::OperatorCapacity, "verification pool full").retry_after(1)
        })?;
        self.note_load((self.cfg.max_verifications - self.verify_slots.available_permits()) as u32);
        let verifier = self.verifier.clone();
        let (public, proof) = (request.public_inputs.clone(), request.proof.clone());
        let ok = tokio::task::spawn_blocking(move || {
            let ok = verifier.verify(&public, &proof);
            drop(permit);
            ok
        })
        .await
        .map_err(|_| ApiError::new(Code::ProofInvalid, "verification failed"))?;
        if !ok {
            return Err(ApiError::new(
                Code::ProofInvalid,
                "Groth16 verification failed",
            ));
        }

        let _admission = self.admission.lock().await;
        self.maybe_refresh_balance().await;
        let lease_id = token::random_hex(16);
        if let Err(refusal) = self.ledger.admit(&lease_id, bound, self.clock.now_ms()) {
            return Err(refusal_error(refusal, proof_ends, self.clock.now_secs()));
        }
        let (lease_token, token_hash) = token::generate();
        let reserved = {
            let lease = NewLease {
                nullifier: nullifier.clone(),
                client_request_id: request.client_request_id.clone(),
                lease_id: lease_id.clone(),
                token_hash,
                bound,
                request_json: canonical.to_string(),
                reserved_at: self.clock.now_secs(),
            };
            self.store
                .run(move |c| store::reserve_lease(c, &lease))
                .await
                .map_err(db_error)
        };
        match reserved {
            Ok(Cas::Done) => {}
            Ok(Cas::Conflict(row)) => {
                self.ledger.release(&lease_id);
                return self.seen(&nullifier, &row, &request, &canonical);
            }
            Err(e) => {
                self.ledger.release(&lease_id);
                return Err(e);
            }
        }
        if let Err(e) = self.replicate(&lease_id).await {
            self.abort(&lease_id).await;
            return Err(e);
        }
        let minted = tokio::time::timeout(
            std::time::Duration::from_secs(self.cfg.mint_timeout_secs),
            self.provider.mint(&lease_id, bound),
        )
        .await;
        let minted = match minted {
            Ok(Ok(minted)) => minted,
            Ok(Err(e)) => {
                self.abort(&lease_id).await;
                let _ = e; // upstream text stays out of the response
                return Err(ApiError::new(
                    Code::ProviderError,
                    "the provider could not mint a key",
                ));
            }
            Err(_) => {
                self.abort(&lease_id).await;
                return Err(ApiError::new(
                    Code::ProviderTimeout,
                    "the provider did not mint in time",
                ));
            }
        };
        let issued_at = self.clock.now_secs();
        let expires_at = issued_at + LEASE_MAX_SECS;
        {
            let (id, key) = (lease_id.clone(), minted.key_id.clone());
            let activated = self
                .store
                .run(move |c| store::activate_lease(c, &id, &key, issued_at, expires_at))
                .await;
            if let Err(e) = activated {
                self.abort(&lease_id).await;
                return Err(db_error(e));
            }
        }
        let body = json!({
            "status": "active",
            "client_request_id": request.client_request_id,
            "issued_at": issued_at,
            "expires_at": expires_at,
            "settle_after": issued_at + SETTLE_AFTER_SECS,
            "spending_limit_usd": bound as f64 / 1_000_000.0,
            "lease_id": lease_id,
            "lease_token": lease_token,
            "bound": bound.to_string(),
        });
        {
            let mut issued = self.issued.lock().unwrap_or_else(|e| e.into_inner());
            // Entries die with their lease, so the map stays small.
            issued.retain(|_, (_, b)| b["expires_at"].as_u64().is_some_and(|t| t > issued_at));
            issued.insert(nullifier, (canonical, body.clone()));
        }
        Ok((StatusCode::CREATED, body))
    }

    /// The nullifier is bound already: an exit, or a lease. A byte-identical repeat of the request
    /// that holds it gets the same lease back.
    fn seen(
        &self,
        nullifier: &str,
        row: &store::NullifierRow,
        request: &ApiRequestV2,
        canonical: &Value,
    ) -> ApiResult<(StatusCode, Value)> {
        if row.state == store::state::EXIT {
            return Err(ApiError::new(
                Code::NullifierInExit,
                "nullifier is in a pending or finished exit",
            ));
        }
        if row.state == store::state::RESERVED
            && row.client_request_id.as_deref() == Some(request.client_request_id.as_str())
        {
            let issued = self.issued.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((original, body)) = issued.get(nullifier) {
                if original == canonical {
                    return Ok((StatusCode::CREATED, body.clone()));
                }
            }
        }
        Err(ApiError::new(
            Code::NullifierSeen,
            "nullifier already used or reserved",
        ))
    }

    /// Undoes a reservation that never became a lease: key revoked, nullifier unbound.
    async fn abort(&self, lease_id: &str) {
        let _ = self.provider.revoke(lease_id).await;
        let id = lease_id.to_string();
        let _ = self.store.run(move |c| store::roll_back(c, &id)).await;
        self.ledger.release(lease_id);
    }

    async fn maybe_refresh_balance(&self) {
        let due = {
            let at = *self.balance_at_ms.lock().unwrap_or_else(|e| e.into_inner());
            self.clock.now_ms().saturating_sub(at) >= self.cfg.balance_refresh_secs * 1000
        };
        if due {
            self.refresh_balance().await;
        }
    }
}

/// Seconds a refused client should wait: shorter than the proof's remaining life.
const RETRY_HINT_SECS: u64 = 30;

fn refusal_error(refusal: Refusal, proof_ends: u64, now: u64) -> ApiError {
    let short = proof_ends
        .saturating_sub(now)
        .saturating_sub(5)
        .clamp(1, RETRY_HINT_SECS);
    match refusal {
        Refusal::DailyBudget { retry_after_secs } => ApiError::new(
            Code::OperatorCapacity,
            "the daily provider budget is used up",
        )
        .retry_after(retry_after_secs),
        Refusal::Credit => ApiError::new(
            Code::OperatorCapacity,
            "not enough provider credit for this bound",
        )
        .retry_after(short),
        Refusal::Minting => ApiError::new(Code::OperatorCapacity, "key minting is rate limited")
            .retry_after(short.min(5)),
    }
}

fn db_error(_: rusqlite::Error) -> ApiError {
    ApiError::new(Code::ReplicaUnreachable, "the operator's store failed").retry_after(5)
}

const REQUEST_KEYS: [&str; 5] = [
    "client_request_id",
    "payload",
    "payload_hash",
    "public_inputs",
    "proof",
];
const INPUT_KEYS: [&str; 12] = [
    "protocol_version",
    "chain_id",
    "contract_address",
    "active_root",
    "state_signing_key_x",
    "state_signing_key_y",
    "request_time",
    "solvency_bound",
    "request_nullifier",
    "authorization_tag",
    "anonymous_commitment_x",
    "anonymous_commitment_y",
];

fn malformed(message: impl Into<String>) -> ApiError {
    ApiError::new(Code::MalformedRequest, message)
}

/// Strict parse of a lease request: valid JSON, no duplicate or unknown keys, bounded fields,
/// a prompt-free payload whose hash matches. Returns the request and its canonical JSON.
fn parse_request(body: &[u8]) -> ApiResult<(ApiRequestV2, Value)> {
    let value = parse_strict(body).map_err(malformed)?;
    if !only_keys(&value, &REQUEST_KEYS)
        || !only_keys(&value["public_inputs"], &INPUT_KEYS)
        || !only_keys(&value["proof"], &["backend", "proof"])
    {
        return Err(malformed("unknown field"));
    }
    let request: ApiRequestV2 =
        serde_json::from_value(value.clone()).map_err(|e| malformed(e.to_string()))?;
    if request.client_request_id.is_empty()
        || request.client_request_id.chars().count() > MAX_CLIENT_REQUEST_ID
    {
        return Err(malformed("client_request_id must be 1 to 36 characters"));
    }
    if request.payload.len() > MAX_PAYLOAD {
        return Err(malformed("payload over 2 KiB"));
    }
    serde_json::from_str::<OpenRouterLeaseAuthorization>(&request.payload)
        .map_err(|_| malformed("payload is not a lease authorization"))?;
    if canonical_payload_hash(request.payload.as_bytes()) != request.payload_hash {
        return Err(malformed("payload_hash does not match the payload"));
    }
    Ok((request, value))
}
