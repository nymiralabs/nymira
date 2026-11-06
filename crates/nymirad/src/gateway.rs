//! The local gateway: an OpenAI chat request in, the lease, the encrypted exchange through the
//! operator, the checks on what came back, and the settlement out.
//!
//! One lease per note. A lease serves requests until it has been idle for `idle_secs` or cannot
//! pay for another; then it is retired and, once the operator has settled it, the wallet installs
//! the next state and the lease record is closed.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use futures_util::StreamExt;
use nymira_near_codec::{chain_id, contract_address, Network};
use nymira_operator::charge::ChargeParams;
use nymira_operator::clock::Clock;
use nymira_operator::proxy::{parse_chat, ModelSpec, ProxyConfig};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use zkapi_client::config::{ClientConfig, ClientProofMode};
use zkapi_client::wallet::Wallet;
use zkapi_types::wire::CurvePointWire;
use zkapi_types::Felt252;

pub mod api;
pub mod close;
pub mod revoke;
pub mod watch;

use crate::agents::Scope;
use crate::attest::{Attestor, ModelIdentity, ReportRequest, Trust};
use crate::e2ee::{self, ClientKeys};
use crate::lease::{self, Attempt, Lease, LeaseClient, LeaseError, Pinned};
use crate::openai::ApiError;
use crate::pins::{self, Pins, Tripwire};
use crate::records::{LeaseRecord, Records};
use crate::rpc::{client, client_with_identity, send_error, Rpc, RpcError, Transport};
use crate::secrets::{NoteEntry, NoteStatus, NoteStore};
use crate::setsize;
use crate::sizing;
use crate::snapshot;
use crate::submitter::SubmitterStore;
use crate::walletvault::WalletVault;

pub const DAY_SECS: u64 = 86_400;
/// How long a verified attestation report is trusted before a fresh one is fetched.
const ATTESTATION_TTL_SECS: u64 = 300;
/// The wallet's own near-expiry margin for settlement.
const SETTLEMENT_WINDOW_SECS: u64 = 21_905;
/// How long a `require_tee` answer waits for the operator to read its signature.
const HOLD_SECS: std::time::Duration = std::time::Duration::from_secs(120);
/// Raw bytes kept per exchange for the signature check.
const EXCHANGE_CAP: usize = 4 << 20;

pub struct GatewayConfig {
    pub home: PathBuf,
    pub operator_url: String,
    /// NEAR AI's public gateway, for the attestation report and the model list.
    pub gateway_url: String,
    pub vault: String,
    pub network: Network,
    /// Directory with `request.pk` and the other proof files.
    pub setup_dir: String,
    /// SHA-256 (hex) of `request.pk`, compiled into a release build.
    pub proving_key_sha256: String,
    /// SHA-256 (hex) of `withdrawal.pk`, compiled into a release build: the close and exit
    /// proofs are made with it.
    pub withdrawal_key_sha256: String,
    pub models: Vec<ModelSpec>,
    pub rounds: u32,
    /// Seconds of silence after which a lease is retired.
    pub idle_secs: u64,
    /// Percent of a lease's remaining bound one request may use: 90, or 70 if the first-day
    /// overspend rule triggers (it did not in the first-day checks).
    pub share_pct: u128,
    /// A new lease waits a random time up to this many seconds before anything goes out, then
    /// opens its Tor circuit while the proof is built (`net::prewarm`), so lease timing does not
    /// follow the moment the user asked. 0 turns both off; over a direct transport they are off
    /// anyway.
    pub prewarm_max_secs: u64,
    /// Where the encrypted backup of the whole state goes after every retire.
    pub backup: Option<crate::backup::Target>,
}

#[derive(Default, Clone)]
pub struct ChatOptions {
    /// `X-Nymira-Max-Lease-Usd`: 0.5 or more opens the large class.
    pub max_lease_usd: Option<f64>,
    /// Which notes this request may draw on. The HTTP front door sets it from the token.
    pub scope: Scope,
    /// `X-Nymira-Approval-Id`: the retry of a request the owner has been asked to approve.
    pub approval_id: Option<String>,
    /// `X-Nymira-Max-Cost-Usd`, in micro-USD: the most this call may cost.
    pub max_cost_micro: Option<u128>,
}

struct Exchange {
    chat_id: Option<String>,
    model: String,
    request: Vec<u8>,
    response: Vec<u8>,
    prompt: u64,
    completion: u64,
}

struct Session {
    note_key: String,
    wallet: Wallet,
    lease: Lease,
    /// This conversation's connection: one SOCKS identity, so one Tor circuit, for the lease and
    /// everything sent on it.
    http: reqwest::Client,
    keys: Arc<ClientKeys>,
    identities: HashMap<String, ModelIdentity>,
    spent_micro: u128,
    reserved_micro: u128,
    last_used: u64,
    exchanges: Vec<Exchange>,
    record: LeaseRecord,
    retiring: bool,
    /// An exchange failed after the operator may have billed it: usage is unknown.
    incomplete: bool,
    /// Requests that have been admitted and not yet finished.
    inflight: u32,
    /// The agent whose budget note this lease is on; its day counters follow the lease.
    agent: Option<String>,
    /// The operator's charge was more than a step above the recomputed one.
    overcharged: bool,
}

/// What an agent request carries out of its admission checks.
struct Admitted {
    /// The most output tokens the call may ask for under `max_call_usd`.
    cap: u64,
    /// None when the call is only being quoted.
    hold: Option<crate::limits::CallHold>,
    /// The call's sized input and worst-case cost (micro-USD), for the approval check.
    input_tokens: u64,
    worst: u128,
}

/// What `preflight` found.
struct Pre {
    model: String,
    spec: ModelSpec,
    caller_max: Option<u64>,
    admitted: Option<Admitted>,
    needs_approval: bool,
}

/// At most this many requests run at once on one note's lease; a third gets 409 `busy`.
const MAX_INFLIGHT: u32 = 2;

/// What a finished lease came to.
#[derive(Debug)]
pub struct Settled {
    pub lease_id: String,
    pub charge: u128,
    pub expected: u128,
    pub trust: Trust,
    pub warnings: Vec<String>,
}

pub enum Outcome {
    Json(Value, String),
    Stream(Body, String),
}

/// What the proof's snapshot and the set-size count saw, kept until the lease opens.
struct Seen {
    root: String,
    leaf_count: u32,
    block: u64,
    sets: Result<setsize::Raw, String>,
}

pub struct Gateway {
    cfg: GatewayConfig,
    transport: Transport,
    rpc: Rpc,
    /// A second, independent provider for the set-size count.
    rpc_second: Option<Rpc>,
    pins: Pins,
    tripwire: Tripwire,
    notes: NoteStore,
    vault: WalletVault,
    records: Records,
    attestor: Arc<dyn Attestor>,
    clock: Arc<dyn Clock>,
    sessions: Mutex<HashMap<String, Session>>,
    identities: Mutex<HashMap<String, (ModelIdentity, u64)>>,
    /// Notes whose lease is being settled: they stay busy until the wallet is sealed again.
    settling: Mutex<std::collections::HashSet<String>>,
    open_lock: Mutex<()>,
    /// Close and exit transactions: the submitters' keys, the sender's database and settings.
    submitters: SubmitterStore,
    tx_store: nymira_operator::store::Store,
    tx_config: nymira_operator::tx::TxConfig,
    /// What left, with the exit deadlines.
    exits: crate::exits::ExitBook,
    /// Holds whose release found the lock busy (see `close::Hold`).
    stuck_holds: std::sync::Mutex<Vec<String>>,
    /// When `finalize_exit` was last tried for a note (Unix seconds).
    finalize_tried: std::sync::Mutex<HashMap<String, u64>>,
    /// The agents' rate windows and day totals.
    limits: crate::limits::Limits,
    /// Agent requests held for the owner's approval, in memory only.
    approvals: crate::approvals::Approvals,
    /// Where an approval's signing key is read from; the node and second RPC when unset.
    key_rpcs: std::sync::Mutex<Option<Vec<Rpc>>>,
    /// The expiry guard's switches and what it has tried (see `watch`).
    guard: crate::expiry::GuardConfig,
    guard_tries: std::sync::Mutex<HashMap<String, watch::Tries>>,
    notices: crate::expiry::NoticeBook,
    /// The operator's emergency notice as last read, and when it was read (Unix seconds).
    emergency: std::sync::Mutex<(Option<String>, u64)>,
    /// Budget notes whose owner has started a revocation: no agent request is served on them.
    revoking: std::sync::Mutex<std::collections::HashSet<String>>,
    /// Answers kept for agents' idempotency keys, in memory only.
    idem: crate::idempotency::Idempotency,
    /// Budget notes the operator overcharged, until the owner acknowledges.
    charge_warnings: crate::chargewarn::ChargeWarnings,
}

fn felt_point(v: &Value) -> Option<CurvePointWire> {
    Some(CurvePointWire {
        x: Felt252::from_hex(v["x"].as_str()?).ok()?,
        y: Felt252::from_hex(v["y"].as_str()?).ok()?,
    })
}

fn unavailable(code: &str, message: impl Into<String>) -> ApiError {
    let mut e = ApiError::from_operator(503, code, &message.into(), None);
    e.code = code.into();
    e
}

fn lease_error(e: LeaseError) -> ApiError {
    match e {
        LeaseError::Operator(o) => {
            ApiError::from_operator(o.status, &o.code, &o.message, o.retry_after_secs)
        }
        LeaseError::Gate(g) => unavailable("operator_untrusted", g.to_string()),
        LeaseError::Emergency(n) => unavailable(
            "operator_emergency",
            format!("the operator reports an emergency ({n}); no new lease is opened. Leave with `nymirad exit --note ID` for each note"),
        ),
        LeaseError::TorDown(m) => unavailable("tor_down", format!("Tor is down: {m}")),
        LeaseError::TorRefused(m) => {
            unavailable("tor_refused", format!("the other side refuses Tor: {m}"))
        }
        other => unavailable("operator_unreachable", other.to_string()),
    }
}

/// Why a note was passed over: `Reason` goes into the 402, `Fatal` ends the request.
enum Skip {
    Reason(String),
    Fatal(ApiError),
}

/// A failed call to a service nymirad reaches through Tor, with the Tor causes told apart.
fn net_error(code: &str, e: RpcError) -> ApiError {
    match e {
        RpcError::TorDown(m) => unavailable("tor_down", format!("Tor is down: {m}")),
        RpcError::TorRefused(m) => {
            unavailable("tor_refused", format!("the other side refuses Tor: {m}"))
        }
        other => unavailable(code, other.to_string()),
    }
}

fn micro_ceil(nano: u128) -> u128 {
    nano.div_ceil(1000)
}

impl Gateway {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        cfg: GatewayConfig,
        transport: Transport,
        rpc: Rpc,
        pins: Pins,
        notes: NoteStore,
        passphrase: &str,
        attestor: Arc<dyn Attestor>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, String> {
        check_proving_key(&cfg.setup_dir, &cfg.proving_key_sha256)?;
        check_withdrawal_key(&cfg.setup_dir, &cfg.withdrawal_key_sha256)?;
        let vault = WalletVault::new(&cfg.home, passphrase, cfg.rounds);
        vault.seal_leftovers().map_err(|e| e.to_string())?;
        let submitters =
            SubmitterStore::open(&cfg.home.join("submitters.sealed"), passphrase, cfg.rounds)
                .map_err(|e| e.to_string())?;
        let tx_store = nymira_operator::store::Store::open(&cfg.home.join("tx.db"))
            .map_err(|e| e.to_string())?;
        Ok(Self {
            submitters,
            tx_store,
            tx_config: Default::default(),
            exits: crate::exits::ExitBook::new(&cfg.home),
            stuck_holds: Default::default(),
            finalize_tried: Default::default(),
            limits: crate::limits::Limits::new(&cfg.home),
            approvals: Default::default(),
            key_rpcs: Default::default(),
            guard: Default::default(),
            guard_tries: Default::default(),
            notices: crate::expiry::NoticeBook::new(&cfg.home),
            emergency: Default::default(),
            revoking: Default::default(),
            idem: Default::default(),
            charge_warnings: crate::chargewarn::ChargeWarnings::new(&cfg.home),
            records: Records::new(&cfg.home),
            transport,
            rpc,
            rpc_second: None,
            pins,
            tripwire: Tripwire::default(),
            notes,
            vault,
            attestor,
            clock,
            sessions: Mutex::new(HashMap::new()),
            identities: Mutex::new(HashMap::new()),
            settling: Mutex::new(Default::default()),
            open_lock: Mutex::new(()),
            cfg,
        })
    }

    /// Waits and retries of the close and exit transactions (tests shorten them).
    pub fn with_tx_config(mut self, cfg: nymira_operator::tx::TxConfig) -> Self {
        self.tx_config = cfg;
        self
    }

    /// The second RPC provider the set size is read from as well; the lower figure is kept.
    pub fn with_second_rpc(mut self, rpc: Option<Rpc>) -> Self {
        self.rpc_second = rpc;
        self
    }

    /// The submitter accounts, for the CLI that adds and lists them.
    pub fn submitters(&self) -> &SubmitterStore {
        &self.submitters
    }

    pub fn vault_account(&self) -> &str {
        &self.cfg.vault
    }

    pub fn records(&self) -> Vec<LeaseRecord> {
        self.records.list()
    }

    pub fn approvals(&self) -> &crate::approvals::Approvals {
        &self.approvals
    }

    pub fn now_secs(&self) -> u64 {
        self.clock.now_secs()
    }

    /// The endpoints an approval's signing key is checked on (tests point them at a fake node).
    pub fn set_key_rpcs(&self, rpcs: Vec<Rpc>) {
        *self.key_rpcs.lock().unwrap_or_else(|e| e.into_inner()) = Some(rpcs);
    }

    pub fn key_rpcs(&self) -> Vec<Rpc> {
        let set = self
            .key_rpcs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        set.unwrap_or_else(|| {
            let mut rpcs = vec![self.rpc.clone()];
            rpcs.extend(self.rpc_second.clone());
            rpcs
        })
    }

    /// What a signed policy is checked against: the pinned models and the smallest bound class.
    pub fn policy_context(&self) -> crate::policy::Context {
        crate::policy::Context::new(
            self.cfg.models.iter().map(|m| m.id.clone()).collect(),
            &self.bound_classes(),
        )
    }

    /// What the lease records show was charged on the note today (UTC): the least today's total
    /// can be, whatever a restored counter file says.
    fn charged_in_records(&self, note_id: u32, now: u64) -> u128 {
        crate::limits::charges_on(&self.records.list(), note_id, now / DAY_SECS)
    }

    /// What settled leases charged today on the agent's budget note, in micro-USD.
    pub fn agent_spend_today(&self, agent: &str, note: &str) -> u128 {
        let now = self.clock.now_secs();
        let note_id = self
            .notes
            .list()
            .ok()
            .and_then(|n| n.into_iter().find(|n| n.commitment == note))
            .and_then(|n| n.note_id);
        self.limits.charged_today(agent, now, &|| {
            note_id.map_or(0, |id| self.charged_in_records(id, now))
        })
    }

    /// An agent request's checks that need no lease: the rate limit, then the call's cost
    /// against `max_call_usd`, then the day's reservation. Owner requests pass untouched.
    fn admit(
        &self,
        opts: &ChatOptions,
        request: &Value,
        spec: &ModelSpec,
        caller_max: Option<u64>,
        dry: bool,
    ) -> Result<Option<Admitted>, ApiError> {
        let Scope::Agent { name, note, policy } = &opts.scope else {
            return Ok(None);
        };
        let now = self.clock.now_secs();
        let note_id = self
            .notes
            .list()
            .ok()
            .and_then(|n| n.into_iter().find(|n| &n.commitment == note))
            .and_then(|n| n.note_id);
        let floor = || note_id.map_or(0, |id| self.charged_in_records(id, now));
        // A quote looks at the counters and changes none.
        if dry {
            self.limits
                .peek_rate(name, now, policy.rate_limit_per_min, &floor)
        } else {
            self.limits
                .rate(name, now, policy.rate_limit_per_min, &floor)
        }
        .map_err(ApiError::policy_rate_limit)?;

        // The input is sized against a throwaway key: the ciphertext has the same length under
        // any key, and nothing has to be attested or sent for this check.
        let throwaway = hex::decode(ClientKeys::generate().public_hex())
            .ok()
            .and_then(|k| <[u8; 32]>::try_from(k).ok())
            .expect("a public key is 32 bytes");
        let cfg = ProxyConfig {
            models: self.cfg.models.clone(),
            ..ProxyConfig::default()
        };
        let sealed = e2ee::seal_request(request, &throwaway, Some(1))
            .map_err(|e| ApiError::invalid_request(e.to_string()))?;
        let input = parse_chat(
            &serde_json::to_vec(&sealed).expect("json"),
            &cfg,
            &Default::default(),
        )
        .map_err(|e| ApiError::invalid_request(e.message))?
        .input_tokens;
        // The call's own cost cap can only lower the policy's limit.
        let limit = crate::limits::micro_of(policy.max_call_usd)
            .min(opts.max_cost_micro.unwrap_or(u128::MAX));
        let cap = sizing::max_tokens(limit, 100, spec, u128::from(input))
            .ok_or_else(ApiError::call_limit)?;
        if caller_max.is_some_and(|c| c > cap) {
            return Err(ApiError::call_limit());
        }
        let worst = micro_ceil(sizing::cost_nano(spec, input, caller_max.unwrap_or(cap)));
        let bound = lease::choose_bound(&self.bound_classes(), opts.max_lease_usd).unwrap_or(0);
        let max_daily = crate::limits::micro_of(policy.max_daily_usd);
        let hold = if dry {
            if !self
                .limits
                .has_room(name, now, worst, bound, max_daily, &floor)
            {
                return Err(ApiError::daily_limit());
            }
            None
        } else {
            Some(
                self.limits
                    .reserve_call(name, now, worst, bound, max_daily, &floor)
                    .ok_or_else(ApiError::daily_limit)?,
            )
        };
        Ok(Some(Admitted {
            cap,
            hold,
            input_tokens: input,
            worst,
        }))
    }

    pub fn models_json(&self) -> Value {
        crate::openai::models(&self.cfg.models)
    }

    /// At start-up: the pinned models against the live list. Differences are warnings.
    pub async fn check_models(&self) -> Vec<String> {
        let http = match client(&self.transport) {
            Ok(h) => h,
            Err(e) => return vec![format!("the live model list could not be read: {e}")],
        };
        match crate::models::fetch_live(&http, &self.cfg.gateway_url).await {
            Ok(live) => crate::models::compare(&self.cfg.models, &live),
            Err(e) => vec![format!("the live model list could not be read: {e}")],
        }
    }

    fn charge_params(&self) -> Option<ChargeParams> {
        let c = &self.pins.config;
        Some(ChargeParams {
            step: c["charge_step"].as_str()?.parse().ok()?,
            min_charge: c["min_charge"].as_str()?.parse().ok()?,
            slot_rate: u32::try_from(c["slot_rate"].as_u64()?).ok()?,
        })
    }

    fn bound_classes(&self) -> Vec<u128> {
        self.pins.config["bound_classes"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().and_then(|s| s.parse().ok()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn wallet_config(
        &self,
        state_dir: &std::path::Path,
        bound: u128,
    ) -> Result<ClientConfig, ApiError> {
        let key = |f: &str| {
            felt_point(&self.pins.config[f]).ok_or_else(|| {
                unavailable("pins_malformed", format!("the pinned {f} is malformed"))
            })
        };
        Ok(ClientConfig {
            protocol_version: 2,
            chain_id: chain_id(self.cfg.network),
            contract_address: Felt252(contract_address(&self.cfg.vault)),
            request_charge_cap: bound,
            policy_charge_cap: bound,
            policy_enabled: false,
            server_url: self.cfg.operator_url.clone(),
            state_dir: state_dir.to_string_lossy().into_owned(),
            proof_mode: ClientProofMode::Groth16 {
                setup_dir: self.cfg.setup_dir.clone(),
            },
            state_signing_key: key("operator_pk")?,
            clearance_signing_key: key("clearance_pk")?,
            settlement_window_seconds: SETTLEMENT_WINDOW_SECS,
        })
    }

    /// A free note that can pay for a `bound` lease, its wallet unsealed. `Err` carries the
    /// reason, for the 402.
    async fn pick_note(
        &self,
        bound: u128,
        busy: &[String],
        scope: &Scope,
    ) -> Result<(NoteEntry, Wallet), ApiError> {
        let notes = self
            .notes
            .list()
            .map_err(|e| unavailable("notes_unreadable", e.to_string()))?;
        let now = self.clock.now_secs();
        let accepted: Vec<_> = notes
            .into_iter()
            .filter(|n| {
                n.status == NoteStatus::Accepted
                    && n.note_id.is_some()
                    && scope.allows(&n.commitment)
            })
            .collect();
        if accepted.is_empty() {
            return Err(if scope.is_agent() {
                // The budget note holds nothing yet, whatever the owner's other notes hold.
                ApiError::budget_exhausted()
            } else {
                ApiError::no_funds("no accepted note: deposit one with `nymirad deposit` first")
            });
        }
        let mut reasons = Vec::new();
        for entry in accepted {
            let (id, expiry) = (
                entry.note_id.expect("filtered"),
                entry.expiry_ns.unwrap_or(0),
            );
            if busy.contains(&entry.commitment) {
                reasons.push(format!("note {id} has an open lease (keep two notes so one can retire while the other serves)"));
                continue;
            }
            if self.guard.enabled && crate::expiry::lease_blocked(expiry, now) {
                reasons.push(format!("note {id} expires within 5 days"));
                continue;
            }
            match self.unsealed_wallet(&entry, bound).await {
                Ok(wallet) => {
                    // An exit lock means a close or exit began: no new request on this note.
                    if wallet.exit_lock().ok().flatten().is_some() {
                        if scope.is_agent() {
                            drop(wallet);
                            let _ = self.vault.checkin(&entry.commitment);
                            return Err(ApiError::note_expiring());
                        }
                        reasons.push(format!("note {id} is closing or exiting"));
                        drop(wallet);
                        let _ = self.vault.checkin(&entry.commitment);
                        continue;
                    }
                    let balance = wallet.state().map_or(0, |s| s.current_balance);
                    if balance < bound {
                        if scope.is_agent() {
                            drop(wallet);
                            let _ = self.vault.checkin(&entry.commitment);
                            return Err(ApiError::budget_exhausted());
                        }
                        reasons.push(format!(
                            "note {id} has {balance} micro-USD left, under the {bound} lease bound"
                        ));
                        drop(wallet);
                        let _ = self.vault.checkin(&entry.commitment);
                        continue;
                    }
                    return Ok((entry, wallet));
                }
                Err(Skip::Reason(r)) => {
                    reasons.push(format!("note {id}: {r}"));
                    let _ = self.vault.checkin(&entry.commitment);
                }
                Err(Skip::Fatal(e)) => {
                    let _ = self.vault.checkin(&entry.commitment);
                    return Err(e);
                }
            }
        }
        Err(ApiError::no_funds(reasons.join("; ")))
    }

    /// The note's wallet, unsealed. A request left pending by an earlier run (a crash, a lost
    /// answer) is resolved first: if the operator has settled it the wallet installs the next
    /// state, otherwise the note waits.
    async fn unsealed_wallet(&self, entry: &NoteEntry, bound: u128) -> Result<Wallet, Skip> {
        let mut wallet = self.open_wallet(entry, bound)?;
        if wallet.has_pending_request() {
            match crate::rpc::recover_pending_over(&self.transport, &mut wallet).await {
                Ok(Some(_)) if !wallet.has_pending_request() => {}
                _ => {
                    return Err(Skip::Reason(
                        "an earlier lease is not settled yet; it is resolved when the operator finishes it".into(),
                    ))
                }
            }
        }
        Ok(wallet)
    }

    /// The note's wallet, unsealed, with its state in place and nothing resolved.
    fn open_wallet(&self, entry: &NoteEntry, bound: u128) -> Result<Wallet, Skip> {
        let fatal = |code: &str, m: String| Skip::Fatal(unavailable(code, m));
        let dir = self
            .vault
            .checkout(&entry.commitment)
            .map_err(|e| fatal("wallet_unreadable", e.to_string()))?;
        let cfg = self.wallet_config(&dir, bound).map_err(Skip::Fatal)?;
        let mut wallet = Wallet::new(cfg).map_err(|e| fatal("wallet_unreadable", e.to_string()))?;
        if wallet.state().is_none() {
            let secret =
                Felt252::from_hex(&entry.secret).map_err(|e| fatal("notes_unreadable", e))?;
            let amount = entry
                .deposit_amount
                .as_deref()
                .and_then(|a| a.parse().ok())
                .unwrap_or(0);
            wallet
                .confirm_deposit(
                    secret,
                    entry.note_id.expect("filtered"),
                    amount,
                    entry.expiry_ns.unwrap_or(0),
                )
                .map_err(|e| fatal("wallet_unreadable", e.to_string()))?;
        }
        Ok(wallet)
    }

    /// The model's verified identity: from the cache, or from a fresh report with a new nonce.
    async fn attested(&self, model: &str) -> Result<ModelIdentity, ApiError> {
        let now = self.clock.now_secs();
        if let Some((id, at)) = self.identities.lock().await.get(model) {
            if now.saturating_sub(*at) < ATTESTATION_TTL_SECS {
                return Ok(id.clone());
            }
        }
        let id = self.model_identity(model).await?;
        self.identities
            .lock()
            .await
            .insert(model.to_string(), (id.clone(), now));
        Ok(id)
    }

    async fn model_identity(&self, model: &str) -> Result<ModelIdentity, ApiError> {
        // Every attempt has its own identity, nonce and circuit: the attestation fetch is not on a
        // lease's circuit, and a destination that turns away some Tor exits (NVIDIA's service
        // answers 403) gets another exit. Every client the verifier builds (gateway, Intel,
        // NVIDIA) goes through the same proxy.
        let tries = if self.transport.is_direct() {
            1
        } else {
            crate::net::CIRCUIT_TRIES
        };
        crate::net::on_fresh_circuits(tries, |identity, _| async move {
            let nonce = hex::encode(rand::random::<[u8; 32]>());
            let http = client_with_identity(&self.transport, &identity)
                .map_err(|e| net_error("attestation_unreachable", e))?;
            let socks_url = self
                .transport
                .socks_url(&identity)
                .map_err(|e| net_error("attestation_unreachable", e))?;
            // The operator's signed time, on this attempt's own circuit (not the lease's), before
            // any evidence is fetched. A verifier that reads it never falls back to a clock of
            // its own: no signed time, no attestation.
            let operator_time = if self.attestor.wants_operator_time() {
                let client = LeaseClient::new(http.clone(), &self.cfg.operator_url);
                let t = lease::signed_time(
                    &client,
                    &self.pins.config["operator_pk"],
                    self.clock.now_secs(),
                )
                .await
                .map_err(|e| {
                    unavailable(
                        "attestation_failed",
                        format!("the operator's signed time is not available: {e}"),
                    )
                })?;
                Some(t)
            } else {
                None
            };
            self.attestor
                .verify_model(&ReportRequest {
                    gateway_url: &self.cfg.gateway_url,
                    model,
                    nonce: &nonce,
                    http: &http,
                    socks_url: socks_url.as_deref(),
                    operator_time,
                })
                .await
                .map_err(|e| unavailable("attestation_failed", e))
        })
        .await
    }

    /// The random delay, then an empty connection to the operator on the conversation's identity.
    /// The connection is made while the proof is built; its result does not matter.
    async fn prewarm(&self, identity: &str) {
        let (Some(socks), true) = (self.transport.socks_addr(), self.cfg.prewarm_max_secs > 0)
        else {
            return;
        };
        let delay_ms = rand::random::<u64>() % (self.cfg.prewarm_max_secs * 1000 + 1);
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        let Ok(url) = reqwest::Url::parse(&self.cfg.operator_url) else {
            return;
        };
        let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
            return;
        };
        let (host, identity) = (host.to_string(), identity.to_string());
        tokio::spawn(async move {
            let _ = crate::net::prewarm(&socks, &identity, &host, port).await;
        });
    }

    /// Opens a lease on a free note.
    async fn open_session(
        &self,
        opts: ChatOptions,
        busy: Vec<String>,
    ) -> Result<Session, ApiError> {
        if let Err(diffs) = self.tripwire.check_against(&self.pins, &self.rpc).await {
            return Err(unavailable(
                "vault_pins_changed",
                format!("no new lease: {}", pins::warnings(&diffs).join(" ")),
            ));
        }
        check_proving_key(&self.cfg.setup_dir, &self.cfg.proving_key_sha256)
            .map_err(|e| unavailable("proving_key_changed", e))?;
        check_withdrawal_key(&self.cfg.setup_dir, &self.cfg.withdrawal_key_sha256)
            .map_err(|e| unavailable("proving_key_changed", e))?;
        let classes = self.bound_classes();
        let bound = lease::choose_bound(&classes, opts.max_lease_usd).ok_or_else(|| {
            unavailable(
                "bound_class_missing",
                "the vault lists no 0.10 USDC lease class",
            )
        })?;
        // A new conversation, a new circuit. With Tor not ready this fails before a note is touched.
        let identity = crate::net::identity();
        let http = client_with_identity(&self.transport, &identity)
            .map_err(|e| net_error("operator_unreachable", e))?;
        self.prewarm(&identity).await;
        let lease_client = LeaseClient::new(http.clone(), &self.cfg.operator_url);
        let (entry, wallet) = self.pick_note(bound, &busy, &opts.scope).await?;
        let (note_id, note_key) = (entry.note_id.expect("picked"), entry.commitment.clone());
        let wallet = Mutex::new(wallet);
        let seen: std::sync::Mutex<Option<Seen>> = std::sync::Mutex::new(None);
        let pinned = Pinned {
            operator_pk: &self.pins.config["operator_pk"],
            config: &self.pins.config,
        };
        let opened = lease::open_lease(
            &lease_client,
            &pinned,
            || self.clock.now_secs(),
            |attempt, request_time| {
                let (wallet, seen) = (&wallet, &seen);
                async move {
                    let snap = snapshot::take(&self.rpc, &self.cfg.vault)
                        .await
                        .map_err(|e| e.to_string())?;
                    if snap.tree.leaf(note_id) == Felt252::ZERO {
                        return Err("the note's leaf is not in the tree yet (wait for the next sync_root) or was retired".into());
                    }
                    // The set is counted from the block the proof's tree came from. A failure
                    // here is kept for the record; it never stops the lease.
                    let sets = setsize::read(
                        &self.rpc,
                        self.rpc_second.as_ref(),
                        &self.cfg.vault,
                        &snap.block_hash,
                        snap.leaf_count,
                        request_time,
                    )
                    .await
                    .map_err(|e| e.to_string());
                    *seen.lock().unwrap_or_else(|e| e.into_inner()) = Some(Seen {
                        root: format!("0x{}", hex::encode(snap.root.0)),
                        leaf_count: snap.leaf_count,
                        block: snap.block_height,
                        sets,
                    });
                    let mut w = wallet.lock().await;
                    if attempt == Attempt::Rebuild {
                        lease::set_aside_stale(&w)?;
                    }
                    lease::prepare(
                        &mut w,
                        snap.root,
                        snap.tree.siblings(note_id).to_vec(),
                        bound,
                        request_time,
                    )
                }
            },
        )
        .await;
        let mut wallet = wallet.into_inner();
        let lease = match opened {
            Ok(l) => l,
            Err(e) => {
                // A refusal means the operator did not bind the nullifier: the proof is set aside
                // and the state can prove again. Any other failure (a timeout, a lost answer)
                // leaves the request journaled, so the next run resolves it with the operator
                // instead of risking a second proof on a spent nullifier.
                if matches!(&e, LeaseError::Operator(_)) && wallet.has_pending_request() {
                    let _ = lease::set_aside_stale(&wallet);
                }
                drop(wallet);
                let _ = self.vault.checkin(&note_key);
                return Err(match e {
                    LeaseError::Build(m) => ApiError::no_funds(m),
                    other => lease_error(other),
                });
            }
        };
        let seen = seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
            .unwrap_or_else(|| Seen {
                root: String::new(),
                leaf_count: 0,
                block: 0,
                sets: Err("no snapshot".into()),
            });
        // The operator's team list can only lower the count; if it cannot be read the pinned
        // list alone is removed.
        let operator_team = lease_client
            .status()
            .await
            .ok()
            .and_then(|s| setsize::operator_team(&s));
        let mut warnings = Vec::new();
        let set_size = match &seen.sets {
            Ok(raw) => Some(setsize::evaluate(
                raw,
                &setsize::pinned_team(),
                operator_team.as_deref(),
            )),
            Err(why) => {
                warnings.push(format!("set size unavailable: {why}"));
                None
            }
        };
        let (root, leaf_count, snapshot_block) = (seen.root, seen.leaf_count, seen.block);
        let now = self.clock.now_secs();
        let record = LeaseRecord {
            lease_id: lease.id.clone(),
            note_id,
            root,
            leaf_count,
            snapshot_block,
            bound_class: bound.to_string(),
            opened_at: now,
            closed_at: None,
            charge: None,
            expected_charge: None,
            model: String::new(),
            sampling: json!({}),
            requests: 0,
            message_count: 0,
            message_bytes: 0,
            prompt_tokens: 0,
            completion_tokens: 0,
            trust: None,
            warnings,
            set_size,
        };
        let _ = self.records.write(&record);
        let agent = match &opts.scope {
            Scope::Agent { name, .. } => Some(name.clone()),
            Scope::Owner { .. } => None,
        };
        if let Some(name) = &agent {
            self.limits
                .lease_opened(name, now, bound, &|| self.charged_in_records(note_id, now));
        }
        self.back_up();
        // Stop the wallet from using this state again before the lease settles.
        let _ = &mut wallet;
        Ok(Session {
            note_key,
            wallet,
            lease,
            http,
            keys: Arc::new(ClientKeys::generate()),
            identities: HashMap::new(),
            spent_micro: 0,
            reserved_micro: 0,
            last_used: now,
            exchanges: Vec::new(),
            record,
            retiring: false,
            incomplete: false,
            inflight: 0,
            agent,
            overcharged: false,
        })
    }

    /// The note key of a session that can take this request, opening a lease if none can.
    async fn session_for(&self, opts: ChatOptions) -> Result<String, ApiError> {
        let _one_at_a_time = self.open_lock.lock().await;
        let want = lease::choose_bound(&self.bound_classes(), opts.max_lease_usd);
        let busy: Vec<String> = {
            let sessions = self.sessions.lock().await;
            if let Some(s) = sessions.values().find(|s| {
                let bound: u128 = s.record.bound_class.parse().unwrap_or(0);
                !s.retiring
                    && Some(bound) == want
                    && s.spent_micro + s.reserved_micro < bound / 2
                    && opts.scope.allows(&s.note_key)
            }) {
                return Ok(s.note_key.clone());
            }
            let mut keys: Vec<String> = sessions.keys().cloned().collect();
            keys.extend(self.settling.lock().await.iter().cloned());
            keys
        };
        let session = self.open_session(opts, busy).await?;
        let key = session.note_key.clone();
        self.sessions.lock().await.insert(key.clone(), session);
        Ok(key)
    }

    /// The checks that need no lease, in the order the spec lists them: the note itself (a
    /// revocation, a close, the last days, an unacknowledged charge warning), the model, the
    /// rate, the call's cost against `max_call_usd` and any cost cap, the day's reservation,
    /// then the owner's approval. Chat and the quote route both run this; with `dry` set no
    /// counter moves and nothing is held, and a call that needs approval is reported in `Pre`
    /// instead of being held.
    fn preflight(&self, opts: &ChatOptions, request: &Value, dry: bool) -> Result<Pre, ApiError> {
        if let Scope::Agent { note, policy, .. } = &opts.scope {
            if let Some(why) = self.note_refusal(note) {
                return Err(why);
            }
            if self.charge_warnings.active(note) {
                return Err(unavailable(
                    "charge_warning",
                    "the operator overcharged this note; the owner has to acknowledge it",
                ));
            }
            if !request["model"]
                .as_str()
                .is_some_and(|m| policy.allowed_models.iter().any(|a| a == m))
            {
                return Err(ApiError::model_not_allowed());
            }
        }
        let model = request["model"]
            .as_str()
            .ok_or_else(|| ApiError::invalid_request("model must be a string"))?
            .to_string();
        let spec = self
            .cfg
            .models
            .iter()
            .find(|m| m.id == model)
            .cloned()
            .ok_or_else(|| ApiError::invalid_request(format!("the model {model} is not served")))?;
        let caller_max = request["max_tokens"]
            .as_u64()
            .or_else(|| request["max_completion_tokens"].as_u64());
        let admitted = self.admit(opts, request, &spec, caller_max, dry)?;
        // Above the confirmation amount the call waits for the owner; this comes after the day's
        // reservation and before the balance check. An early return frees the reservation.
        let mut needs_approval = false;
        if let (Scope::Agent { name, policy, .. }, Some(a)) = (&opts.scope, &admitted) {
            if a.worst > crate::limits::micro_of(policy.confirm_above_usd) {
                if dry {
                    needs_approval = true;
                } else {
                    self.approvals.gate(
                        name,
                        request,
                        opts.approval_id.as_deref(),
                        a.input_tokens,
                        a.worst,
                        self.clock.now_secs(),
                    )?;
                }
            }
        }
        Ok(Pre {
            model,
            spec,
            caller_max,
            admitted,
            needs_approval,
        })
    }

    pub async fn chat(
        self: &Arc<Self>,
        request: Value,
        opts: ChatOptions,
    ) -> Result<Outcome, ApiError> {
        self.chat_checked(request, opts).await.map(|(o, _)| o)
    }

    /// `chat`, and the signature kind of the answer when `require_tee` held it back until it
    /// was verified.
    pub async fn chat_checked(
        self: &Arc<Self>,
        request: Value,
        mut opts: ChatOptions,
    ) -> Result<(Outcome, Option<Trust>), ApiError> {
        opts.max_lease_usd = opts.scope.lease_request(opts.max_lease_usd);
        let tee = matches!(&opts.scope, Scope::Agent { policy, .. } if policy.require_tee);
        let Pre {
            model,
            spec,
            caller_max,
            admitted,
            ..
        } = self.preflight(&opts, &request, false)?;
        let stream = request["stream"].as_bool().unwrap_or(false);
        let (agent_cap, hold) = match admitted {
            Some(a) => (Some(a.cap), a.hold),
            None => (None, None),
        };

        if let Some(why) = self.emergency_refusal() {
            return Err(why);
        }
        // Attest the model before a lease is opened or anything is encrypted.
        let identity = self.attested(&model).await?;
        let key = self.session_for(opts).await?;
        if let Some(s) = self.sessions.lock().await.get_mut(&key) {
            s.identities
                .entry(model.clone())
                .or_insert_with(|| identity.clone());
        }

        // Size the request with the operator's own parser, so the two agree.
        let (keys, lease, remaining, http) = {
            let sessions = self.sessions.lock().await;
            let s = sessions
                .get(&key)
                .ok_or_else(|| unavailable("lease_gone", "the lease ended"))?;
            let bound: u128 = s.record.bound_class.parse().unwrap_or(0);
            (
                s.keys.clone(),
                s.lease.clone(),
                bound.saturating_sub(s.spent_micro + s.reserved_micro),
                s.http.clone(),
            )
        };
        let cfg = ProxyConfig {
            models: self.cfg.models.clone(),
            ..ProxyConfig::default()
        };
        let sealed = |max| {
            e2ee::seal_request(&request, &identity.model_pub, max)
                .map_err(|e| ApiError::invalid_request(e.to_string()))
        };
        let probe = parse_chat(
            &serde_json::to_vec(&sealed(Some(1))?).expect("json"),
            &cfg,
            &Default::default(),
        )
        .map_err(|e| ApiError::invalid_request(e.message))?;
        let ours = sizing::max_tokens(remaining, self.cfg.share_pct, &spec, u128::from(probe.input_tokens))
            .ok_or_else(|| ApiError::no_funds("the lease cannot pay for this prompt and an answer; start a new conversation or shorten it"))?;
        // The per-call limit only lowers the cap, never the lease's bound.
        let max = caller_max
            .map_or(ours, |c| c.min(ours))
            .min(agent_cap.unwrap_or(u64::MAX));
        let parsed = parse_chat(
            &serde_json::to_vec(&sealed(Some(max))?).expect("json"),
            &cfg,
            &Default::default(),
        )
        .map_err(|e| ApiError::invalid_request(e.message))?;
        let body = parsed.body;
        let reserve = micro_ceil(sizing::cost_nano(&spec, parsed.input_tokens, max));
        {
            // Checked and taken under one lock, so requests that run at once cannot each claim
            // the same remaining bound.
            let mut sessions = self.sessions.lock().await;
            let Some(s) = sessions.get_mut(&key) else {
                return Err(unavailable("lease_gone", "the lease ended"));
            };
            if agent_cap.is_some() && s.inflight >= MAX_INFLIGHT {
                return Err(ApiError::busy());
            }
            let bound: u128 = s.record.bound_class.parse().unwrap_or(0);
            if s.spent_micro + s.reserved_micro + reserve > bound * self.cfg.share_pct / 100 {
                return Err(ApiError::no_funds(
                    "the lease is busy with other requests and cannot also pay for this one; retry in a moment",
                ));
            }
            s.reserved_micro += reserve;
            s.inflight += 1;
            s.last_used = self.clock.now_secs();
        }

        let release = |this: &Arc<Self>| {
            let (this, key) = (this.clone(), key.clone());
            async move {
                if let Some(s) = this.sessions.lock().await.get_mut(&key) {
                    s.reserved_micro = s.reserved_micro.saturating_sub(reserve);
                    s.inflight = s.inflight.saturating_sub(1);
                }
            }
        };
        if let Err(e) = self.transport.ensure_ready() {
            release(self).await;
            return Err(net_error("operator_unreachable", e));
        }
        let mut builder = http
            .post(format!("{}/v1/chat/completions", self.cfg.operator_url))
            .header("content-type", "application/json")
            .header("x-nymira-lease-id", &lease.id)
            .header(lease::TOKEN_HEADER, lease.token.expose());
        for (name, value) in e2ee::request_headers(&keys.public_hex(), &identity.model_pub) {
            builder = builder.header(name, value);
        }
        let sent = builder.body(body.clone()).send().await;
        let response = match sent {
            Ok(r) => r,
            Err(e) => {
                release(self).await;
                return Err(net_error("operator_unreachable", send_error(e)));
            }
        };
        if !response.status().is_success() {
            let status = response.status().as_u16();
            let retry = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse().ok());
            let err: Value = response.json().await.unwrap_or(Value::Null);
            release(self).await;
            if crate::net::refuses_tor(status)
                && err["error"]["code"].as_str().unwrap_or("").is_empty()
            {
                return Err(net_error(
                    "operator_unreachable",
                    RpcError::TorRefused(format!("status {status}")),
                ));
            }
            return Err(ApiError::from_operator(
                status,
                err["error"]["code"].as_str().unwrap_or(""),
                err["error"]["message"].as_str().unwrap_or("no message"),
                err["error"]["retry_after_secs"].as_u64().or(retry),
            ));
        }

        let ctx = Finish {
            gw: self.clone(),
            key: key.clone(),
            model: model.clone(),
            spec,
            request_body: body,
            reserve,
            message_count: request["messages"].as_array().map_or(0, Vec::len) as u32,
            message_bytes: request["messages"]
                .as_array()
                .map(|m| {
                    m.iter()
                        .map(|x| x["content"].as_str().map_or(0, str::len) as u64)
                        .sum()
                })
                .unwrap_or(0),
            sampling: sampling_of(&request),
            _hold: hold,
        };
        if stream {
            let upstream = response.bytes_stream();
            let state = StreamState {
                upstream: Box::pin(upstream),
                keys,
                ctx: Some(ctx),
                pending: String::new(),
                raw: Vec::new(),
                usage: None,
                chat_id: None,
                out: Vec::new(),
            };
            if tee {
                let text = drain(state)
                    .await
                    .map_err(|e| unavailable("provider_error", e))?;
                let trust = self.verify_held(&key, chat_id_of_sse(&text)).await;
                return held(trust, Outcome::Stream(Body::from(text), lease.id));
            }
            let stream = futures_util::stream::unfold(state, next_chunk);
            return Ok((Outcome::Stream(Body::from_stream(stream), lease.id), None));
        }
        let raw = match response.bytes().await {
            Ok(b) => b,
            Err(e) => {
                ctx.failed().await;
                return Err(unavailable(
                    "operator_unreachable",
                    e.without_url().to_string(),
                ));
            }
        };
        let mut answer: Value = match serde_json::from_slice(&raw) {
            Ok(v) => v,
            Err(_) => {
                ctx.failed().await;
                return Err(unavailable("provider_error", "the answer is not JSON"));
            }
        };
        if let Err(e) = e2ee::open_response(&keys, &mut answer) {
            ctx.failed().await;
            return Err(unavailable("decrypt_failed", e.to_string()));
        }
        let usage = (
            answer["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
            answer["usage"]["completion_tokens"].as_u64().unwrap_or(0),
        );
        if answer["usage"].is_null() {
            ctx.failed().await;
            return Err(unavailable("provider_error", "the answer carries no usage"));
        }
        let chat_id = answer["id"].as_str().map(String::from);
        ctx.done(raw.to_vec(), chat_id.clone(), usage).await;
        if tee {
            let trust = self.verify_held(&key, chat_id).await;
            return held(trust, Outcome::Json(answer, lease.id));
        }
        Ok((Outcome::Json(answer, lease.id), None))
    }

    /// `require_tee`: the signatures can only be read once the lease is closed, so this closes
    /// it, waits for the operator to store them, and checks the one for `chat_id` against the
    /// exact bytes exchanged. The attestation and the key binding were checked before anything
    /// was encrypted (`attested`), and the answer opened with that key.
    async fn verify_held(&self, key: &str, chat_id: Option<String>) -> Trust {
        let not = |why: &str| Trust::NotVerified(why.to_string());
        let Some(id) = chat_id else {
            return not("the answer has no chat id");
        };
        let found = {
            let sessions = self.sessions.lock().await;
            sessions.get(key).and_then(|s| {
                let x = s
                    .exchanges
                    .iter()
                    .rev()
                    .find(|x| x.chat_id.as_deref() == Some(&id))?;
                let identity = s.identities.get(&x.model)?.clone();
                Some((
                    s.lease.clone(),
                    s.http.clone(),
                    identity,
                    x.model.clone(),
                    x.request.clone(),
                    x.response.clone(),
                ))
            })
        };
        let Some((lease, http, identity, model, request, response)) = found else {
            return not("the exchange or its verified attestation is missing");
        };
        if let Err(e) = self.retire(key).await {
            return Trust::NotVerified(format!(
                "the lease could not be closed to read the signature: {}",
                e.message
            ));
        }
        let client = LeaseClient::new(http, &self.cfg.operator_url);
        let deadline = tokio::time::Instant::now() + HOLD_SECS;
        loop {
            // A failed read is tried again until the deadline.
            if let Ok(status) = client.lease_status(&lease).await {
                if let Some(list) = status["provider_evidence"]["signatures"].as_array() {
                    return match list.iter().find(|e| e["chat_id"] == id.as_str()) {
                        Some(e) => self.attestor.verify_signature(
                            &identity,
                            &e["signature"],
                            &model,
                            &request,
                            &response,
                        ),
                        None => Trust::NotVerified(format!("no signature for chat {id}")),
                    };
                }
            }
            if tokio::time::Instant::now() >= deadline {
                return not("the signature was not available in time");
            }
            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    }

    /// Ends the lease on a note: no more requests, the operator settles.
    pub async fn retire(&self, note_key: &str) -> Result<(), ApiError> {
        let (lease, http) = {
            let mut sessions = self.sessions.lock().await;
            let Some(s) = sessions.get_mut(note_key) else {
                return Ok(());
            };
            if s.retiring {
                return Ok(());
            }
            s.retiring = true;
            (s.lease.clone(), s.http.clone())
        };
        let lease_client = LeaseClient::new(http, &self.cfg.operator_url);
        let retired = match self.transport.ensure_ready() {
            Ok(()) => lease_client.retire(&lease).await,
            Err(RpcError::TorDown(m)) => Err(LeaseError::TorDown(m)),
            Err(other) => Err(LeaseError::Transport(other.to_string())),
        };
        if let Err(e) = retired {
            // Not retired: the next pass tries again.
            if let Some(s) = self.sessions.lock().await.get_mut(note_key) {
                s.retiring = false;
            }
            return Err(lease_error(e));
        }
        Ok(())
    }

    /// Retires leases idle for `idle_secs`, or all of them when `all`.
    pub async fn reap(&self, all: bool) {
        let now = self.clock.now_secs();
        let due: Vec<String> = self
            .sessions
            .lock()
            .await
            .values()
            .filter(|s| {
                !s.retiring && (all || now.saturating_sub(s.last_used) >= self.cfg.idle_secs)
            })
            .map(|s| s.note_key.clone())
            .collect();
        for key in due {
            let _ = self.retire(&key).await;
        }
    }

    /// Settles every retired lease the operator has finished: installs the next state, checks
    /// signatures and the charge, closes the record, seals the wallet and writes the backup.
    pub async fn settle(&self) -> Vec<Settled> {
        let keys: Vec<String> = self
            .sessions
            .lock()
            .await
            .values()
            .filter(|s| s.retiring)
            .map(|s| s.note_key.clone())
            .collect();
        let mut done = Vec::new();
        for key in keys {
            let Some(mut session) = self.sessions.lock().await.remove(&key) else {
                continue;
            };
            self.settling.lock().await.insert(key.clone());
            match self.finish_lease(&mut session).await {
                Some(settled) => {
                    if let Some(agent) = &session.agent {
                        let bound = session.record.bound_class.parse().unwrap_or(0);
                        let now = self.clock.now_secs();
                        let note_id = session.record.note_id;
                        self.limits.settled(agent, now, bound, settled.charge, &|| {
                            self.charged_in_records(note_id, now)
                        });
                    }
                    if session.overcharged && session.agent.is_some() {
                        self.warn_overcharge(&key);
                    }
                    drop(session.wallet);
                    let _ = self.vault.checkin(&key);
                    self.back_up();
                    done.push(settled);
                }
                None => {
                    self.sessions.lock().await.insert(key.clone(), session);
                }
            }
            self.settling.lock().await.remove(&key);
        }
        done
    }

    async fn finish_lease(&self, s: &mut Session) -> Option<Settled> {
        // The lease's own client: the same circuit the lease used.
        s.wallet.set_http_client(Some(s.http.clone()));
        let response = s.wallet.recover_pending().await.ok().flatten()?;
        let charge = response.charge_applied;
        let mut warnings = Vec::new();

        // Signatures: the operator fetched them before it revoked the key.
        let status = LeaseClient::new(s.http.clone(), &self.cfg.operator_url)
            .lease_status(&s.lease)
            .await
            .ok();
        let signatures: Vec<Value> = status
            .as_ref()
            .and_then(|v| v["provider_evidence"]["signatures"].as_array().cloned())
            .unwrap_or_default();
        let mut trust = Trust::ModelSigned;
        if s.exchanges.is_empty() {
            trust = Trust::NotVerified("no chat on this lease".into());
        }
        for x in &s.exchanges {
            let one = match (&x.chat_id, s.identities.get(&x.model)) {
                (Some(id), Some(identity)) => signatures
                    .iter()
                    .find(|e| e["chat_id"] == id.as_str())
                    .map(|e| {
                        self.attestor.verify_signature(
                            identity,
                            &e["signature"],
                            &x.model,
                            &x.request,
                            &x.response,
                        )
                    })
                    .unwrap_or_else(|| Trust::NotVerified(format!("no signature for chat {id}"))),
                _ => Trust::NotVerified("an exchange without a chat id".into()),
            };
            trust = match (trust, one) {
                (Trust::NotVerified(r), _) | (_, Trust::NotVerified(r)) => Trust::NotVerified(r),
                (Trust::GatewaySigned, _) | (_, Trust::GatewaySigned) => Trust::GatewaySigned,
                _ => Trust::ModelSigned,
            };
        }
        if let Trust::NotVerified(reason) = &trust {
            warnings.push(format!("not verified: {reason}"));
        }
        s.overcharged = false;

        // The charge, recomputed from the usage the signed answers reported.
        let bound: u128 = s.record.bound_class.parse().unwrap_or(0);
        let cost: u128 = s
            .exchanges
            .iter()
            .filter_map(|x| {
                self.cfg
                    .models
                    .iter()
                    .find(|m| m.id == x.model)
                    .map(|m| sizing::cost_nano(m, x.prompt, x.completion))
            })
            .sum();
        // The local clock, not a figure the operator reports: it was checked against the
        // operator's within 30 s when the lease opened.
        let held = self.clock.now_secs().saturating_sub(s.lease.issued_at);
        let expected = match (self.charge_params(), s.incomplete) {
            (Some(p), false) => {
                let check = sizing::check_charge_cost(bound, cost, held, p, charge);
                s.overcharged = check.warning.is_some();
                warnings.extend(check.warning);
                check.expected
            }
            (None, _) => {
                warnings.push(
                    "the pinned lease parameters are unreadable: the charge was not checked".into(),
                );
                0
            }
            (_, true) => {
                warnings.push("an exchange failed after the operator may have billed it: the charge was not checked".into());
                0
            }
        };

        s.record.closed_at = Some(self.clock.now_secs());
        s.record.charge = Some(charge.to_string());
        s.record.expected_charge = Some(expected.to_string());
        s.record.trust = Some(trust.clone());
        s.record.warnings = warnings.clone();
        let _ = self.records.write(&s.record);
        Some(Settled {
            lease_id: s.lease.id.clone(),
            charge,
            expected,
            trust,
            warnings,
        })
    }

    /// Marks a budget note whose lease was overcharged: new agent requests stop on it until
    /// the owner runs `nymirad agent ack-warning`.
    fn warn_overcharge(&self, note: &str) {
        if let Err(e) = self.charge_warnings.raise(note) {
            eprintln!("warning: the charge warning was not saved: {e}");
        }
        let agent = crate::agents::Registry::at(&self.cfg.home)
            .load()
            .ok()
            .and_then(|b| b.for_note(note).map(|a| a.name.clone()))
            .unwrap_or_default();
        eprintln!(
            "warning: the operator charged agent {agent}'s budget note more than one step above the recomputed charge. New requests from that agent stop until you run `nymirad agent ack-warning --name {agent}`."
        );
    }

    /// The owner has seen the warning on `note`; the agent can send requests again.
    pub fn ack_charge_warning(&self, note: &str) -> std::io::Result<bool> {
        self.charge_warnings.ack(note)
    }

    /// Writes the backup after a state change. A failure is a warning: the money and the state
    /// are already where they were.
    fn back_up(&self) {
        if let Err(e) = self.write_backup() {
            eprintln!("warning: the backup was not written: {e}");
        }
    }

    /// The whole state, encrypted to the backup recipient and written to every place: the notes
    /// and wallets in the clear inside the encryption (a restore on a new machine has no sealing
    /// passphrase to rely on), the submitters, the exit book and the lease records. Returns the
    /// files written; empty when no backup is set up.
    pub fn write_backup(&self) -> Result<Vec<PathBuf>, crate::backup::BackupError> {
        use crate::backup::{BackupError, StateBackup, STATE_FILE, VERSION};
        let Some(target) = &self.cfg.backup else {
            return Ok(Vec::new());
        };
        let state_err = |e: &dyn std::fmt::Display| BackupError::State(e.to_string());
        let mut state = StateBackup {
            version: VERSION,
            taken_at: self.clock.now_secs(),
            notes: self.notes.list().map_err(|e| state_err(&e))?,
            funding: self.notes.funding_attempts().map_err(|e| state_err(&e))?,
            wallets: self.vault.export_all().map_err(|e| state_err(&e))?,
            submitters: Some(
                String::from_utf8_lossy(&self.submitters.export_json().map_err(|e| state_err(&e))?)
                    .into_owned(),
            ),
            ..Default::default()
        };
        // agents.json holds only token hashes; a restore without it would turn budget notes
        // into ordinary ones.
        for name in [
            crate::exits::FILE,
            crate::records::FILE,
            crate::agents::FILE,
            crate::chargewarn::FILE,
            "config.json",
        ] {
            state.add_file(&self.cfg.home, name);
        }
        // Signed policies travel with their versions in agents.json.
        if let Ok(book) = crate::agents::Registry::at(&self.cfg.home).load() {
            for a in &book.agents {
                state.add_file(&self.cfg.home, &crate::policy::file_name(&a.name));
            }
        }
        crate::backup::write_backups(target, &state.to_bytes()?, STATE_FILE)
    }

    /// Background loop: retire idle leases and settle finished ones.
    pub fn spawn_background(self: &Arc<Self>, every: std::time::Duration) {
        let gw = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                gw.reap(false).await;
                for settled in gw.settle().await {
                    for line in settled_lines(&settled) {
                        eprintln!("{line}");
                    }
                }
                for event in gw.guard_tick().await {
                    eprintln!("{event}");
                }
                for (id, result) in gw.finalize_due().await {
                    match result {
                        Ok(p) => eprintln!("note {id}: exit finalized ({})", p.tx_hash),
                        Err(e) => eprintln!("warning: finalize_exit for note {id} waits: {e}"),
                    }
                }
            }
        });
    }
}

/// What the user is told when a lease settles: whether the signature held (the answer has gone
/// to the app long before the signature can be fetched, so this is where a "not verified" or an
/// overcharge shows) and every warning.
pub fn settled_lines(settled: &Settled) -> Vec<String> {
    let mut lines = vec![format!(
        "lease {}: {}, charged {} (expected {})",
        settled.lease_id,
        settled.trust.label(),
        settled.charge,
        settled.expected
    )];
    lines.extend(
        settled
            .warnings
            .iter()
            .map(|w| format!("warning: lease {}: {w}", settled.lease_id)),
    );
    lines
}

fn sampling_of(request: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for k in [
        "temperature",
        "top_p",
        "frequency_penalty",
        "presence_penalty",
        "seed",
        "stream",
    ] {
        if let Some(v) = request.get(k) {
            out.insert(k.into(), v.clone());
        }
    }
    Value::Object(out)
}

/// The proving key's SHA-256 against the value pinned in this build.
pub fn check_proving_key(setup_dir: &str, expected_hex: &str) -> Result<(), String> {
    check_key_file(setup_dir, "request.pk", expected_hex)
}

/// The same for the withdrawal key, which the close and exit proofs use.
pub fn check_withdrawal_key(setup_dir: &str, expected_hex: &str) -> Result<(), String> {
    check_key_file(setup_dir, "withdrawal.pk", expected_hex)
}

fn check_key_file(setup_dir: &str, file: &str, expected_hex: &str) -> Result<(), String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(std::path::Path::new(setup_dir).join(file))
        .map_err(|e| format!("the proving key {file} could not be read: {e}"))?;
    let have = hex::encode(Sha256::digest(&bytes));
    if have.eq_ignore_ascii_case(expected_hex) {
        Ok(())
    } else {
        Err(format!(
            "the proving key {file} does not match the pinned hash (found {have})"
        ))
    }
}

/// Bookkeeping after an exchange.
struct Finish {
    gw: Arc<Gateway>,
    key: String,
    model: String,
    spec: ModelSpec,
    request_body: Vec<u8>,
    reserve: u128,
    message_count: u32,
    message_bytes: u64,
    sampling: Value,
    /// An agent's call stays in the day's reservation until it finishes or is dropped.
    _hold: Option<crate::limits::CallHold>,
}

impl Finish {
    /// The exchange failed after the operator may have billed it: free the reservation and note
    /// that the usage is unknown, so the charge is not judged against a partial picture.
    async fn failed(self) {
        let mut sessions = self.gw.sessions.lock().await;
        if let Some(s) = sessions.get_mut(&self.key) {
            s.reserved_micro = s.reserved_micro.saturating_sub(self.reserve);
            s.inflight = s.inflight.saturating_sub(1);
            s.incomplete = true;
        }
    }

    async fn done(self, mut raw: Vec<u8>, chat_id: Option<String>, usage: (u64, u64)) {
        raw.truncate(EXCHANGE_CAP);
        let actual = micro_ceil(sizing::cost_nano(&self.spec, usage.0, usage.1));
        let mut sessions = self.gw.sessions.lock().await;
        let Some(s) = sessions.get_mut(&self.key) else {
            return;
        };
        s.reserved_micro = s.reserved_micro.saturating_sub(self.reserve);
        s.inflight = s.inflight.saturating_sub(1);
        s.spent_micro += actual;
        s.last_used = self.gw.clock.now_secs();
        s.exchanges.push(Exchange {
            chat_id,
            model: self.model.clone(),
            request: self.request_body,
            response: raw,
            prompt: usage.0,
            completion: usage.1,
        });
        let r = &mut s.record;
        r.model = self.model;
        r.sampling = self.sampling;
        r.requests += 1;
        r.message_count += self.message_count;
        r.message_bytes += self.message_bytes;
        r.prompt_tokens += usage.0;
        r.completion_tokens += usage.1;
        let _ = self.gw.records.write(r);
    }
}

type Upstream =
    std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Bytes, reqwest::Error>> + Send>>;

struct StreamState {
    upstream: Upstream,
    keys: Arc<ClientKeys>,
    ctx: Option<Finish>,
    pending: String,
    raw: Vec<u8>,
    usage: Option<(u64, u64)>,
    chat_id: Option<String>,
    out: Vec<Bytes>,
}

impl Drop for StreamState {
    fn drop(&mut self) {
        // The client disconnected before the stream ended: the operator may still bill it.
        if let Some(ctx) = self.ctx.take() {
            tokio::spawn(async move { ctx.failed().await });
        }
    }
}

/// Reads a streamed answer to its end without passing any of it on.
async fn drain(mut st: StreamState) -> Result<Vec<u8>, String> {
    let mut text = Vec::new();
    loop {
        match next_chunk(st).await {
            Some((Ok(chunk), next)) => {
                text.extend_from_slice(&chunk);
                st = next;
            }
            Some((Err(e), _)) => return Err(e.to_string()),
            None => return Ok(text),
        }
    }
}

/// The chat id the first event of a decrypted stream carries.
fn chat_id_of_sse(text: &[u8]) -> Option<String> {
    String::from_utf8_lossy(text)
        .lines()
        .filter_map(|l| l.strip_prefix("data:"))
        .filter_map(|d| serde_json::from_str::<Value>(d.trim()).ok())
        .find_map(|v| v["id"].as_str().map(String::from))
}

/// The answer with its signature kind if it verified; otherwise `not_verified` and no content.
fn held(trust: Trust, answer: Outcome) -> Result<(Outcome, Option<Trust>), ApiError> {
    match trust {
        Trust::NotVerified(why) => Err(ApiError::not_verified(&why)),
        ok => Ok((answer, Some(ok))),
    }
}

/// Decrypts a streamed answer line by line as it arrives.
async fn next_chunk(mut st: StreamState) -> Option<(Result<Bytes, std::io::Error>, StreamState)> {
    loop {
        if !st.out.is_empty() {
            let chunk = st.out.remove(0);
            return Some((Ok(chunk), st));
        }
        match st.upstream.next().await {
            Some(Ok(bytes)) => {
                if st.raw.len() < EXCHANGE_CAP {
                    st.raw.extend_from_slice(&bytes);
                }
                st.pending.push_str(&String::from_utf8_lossy(&bytes));
                while let Some(at) = st.pending.find('\n') {
                    let line: String = st.pending.drain(..=at).collect();
                    let line = line.trim_end_matches(['\n', '\r']);
                    match e2ee::open_sse_line(&st.keys, line) {
                        Ok(opened) => {
                            if let Some(data) = opened.strip_prefix("data:") {
                                if let Ok(v) = serde_json::from_str::<Value>(data.trim()) {
                                    if st.chat_id.is_none() {
                                        st.chat_id = v["id"].as_str().map(String::from);
                                    }
                                    if v["usage"].is_object() {
                                        st.usage = Some((
                                            v["usage"]["prompt_tokens"].as_u64().unwrap_or(0),
                                            v["usage"]["completion_tokens"].as_u64().unwrap_or(0),
                                        ));
                                    }
                                }
                            }
                            st.out.push(Bytes::from(format!("{opened}\n")));
                        }
                        Err(e) => {
                            // Never pass ciphertext on as if it were an answer.
                            st.pending.clear();
                            st.out.clear();
                            let err = std::io::Error::other(e.to_string());
                            if let Some(ctx) = st.ctx.take() {
                                ctx.failed().await;
                            }
                            return Some((Err(err), st));
                        }
                    }
                }
            }
            Some(Err(e)) => {
                if let Some(ctx) = st.ctx.take() {
                    ctx.failed().await;
                }
                return Some((Err(std::io::Error::other(e.without_url().to_string())), st));
            }
            None => {
                if let Some(ctx) = st.ctx.take() {
                    match st.usage {
                        Some(usage) => {
                            ctx.done(std::mem::take(&mut st.raw), st.chat_id.take(), usage)
                                .await
                        }
                        None => ctx.failed().await,
                    }
                }
                return None;
            }
        }
    }
}

// ---- HTTP ----

pub async fn chat_handler(
    State(gw): State<Arc<Gateway>>,
    axum::Extension(scope): axum::Extension<Scope>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return ApiError::invalid_request("the body is not JSON").into_response(),
    };
    // An agent sees reasons from the spec's table and fixed texts only.
    let agent = scope.is_agent();
    let fail = move |e: ApiError| if agent { e.typed() } else { e }.into_response();
    let note = match &scope {
        Scope::Agent { note, .. } => note.clone(),
        Scope::Owner { .. } => String::new(),
    };
    let opts = match api::options(&headers, scope) {
        Ok(o) => o,
        Err(e) => return fail(e),
    };
    let Some(raw_key) = headers.get(crate::idempotency::HEADER).filter(|_| agent) else {
        return match gw.chat_checked(request, opts).await {
            Ok((outcome, trust)) => answer(outcome, trust.as_ref().map(Trust::label), false),
            Err(e) => fail(e),
        };
    };
    let Some(raw_key) = raw_key
        .to_str()
        .ok()
        .filter(|k| crate::idempotency::valid_key(k))
    else {
        return fail(ApiError::invalid_request(
            "Idempotency-Key is 1 to 128 printable characters",
        ));
    };
    if request["stream"].as_bool().unwrap_or(false) {
        return fail(ApiError::invalid_request(
            "an Idempotency-Key cannot be used with stream: true",
        ));
    }
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let key =
        crate::idempotency::entry_key(&crate::agents::hash_token(bearer), &note, raw_key, &request);
    let cell = gw.idem.slot(&key, gw.now_secs());
    let mut first: Option<Response> = None;
    let slot = &mut first;
    let kept = cell
        .get_or_init(|| async {
            match gw.chat_checked(request.clone(), opts.clone()).await {
                Ok((Outcome::Json(value, lease), trust)) => {
                    let kept = crate::idempotency::Stored {
                        answer: value,
                        lease,
                        signature_kind: trust.as_ref().map(Trust::label),
                    };
                    *slot = Some(replay(&kept, false));
                    Some(kept)
                }
                Ok((outcome, trust)) => {
                    *slot = Some(answer(outcome, trust.as_ref().map(Trust::label), false));
                    None
                }
                Err(e) => {
                    *slot = Some(fail(e));
                    None
                }
            }
        })
        .await;
    match (first, kept) {
        (Some(response), _) => {
            if kept.is_none() {
                gw.idem.forget(&key);
            }
            response
        }
        // A retry that waited for the first request and got its answer.
        (None, Some(kept)) => replay(kept, true),
        // The request this one waited for was refused or failed: this one runs on its own.
        (None, None) => match gw.chat_checked(request, opts).await {
            Ok((outcome, trust)) => answer(outcome, trust.as_ref().map(Trust::label), false),
            Err(e) => fail(e),
        },
    }
}

fn replay(kept: &crate::idempotency::Stored, again: bool) -> Response {
    let mut response = (
        StatusCode::OK,
        [("x-nymira-lease-id", kept.lease.clone())],
        axum::Json(kept.answer.clone()),
    )
        .into_response();
    if let Some(kind) = kept.signature_kind {
        response.headers_mut().insert(
            "x-nymira-signature-kind",
            axum::http::HeaderValue::from_static(kind),
        );
    }
    if again {
        response.headers_mut().insert(
            "x-nymira-idempotent-replay",
            axum::http::HeaderValue::from_static("true"),
        );
    }
    response
}

fn answer(outcome: Outcome, kind: Option<&'static str>, again: bool) -> Response {
    match outcome {
        Outcome::Json(v, lease) => replay(
            &crate::idempotency::Stored {
                answer: v,
                lease,
                signature_kind: kind,
            },
            again,
        ),
        Outcome::Stream(body, lease) => {
            let mut response = Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .header("x-nymira-lease-id", lease)
                .body(body)
                .expect("static response");
            if let Some(kind) = kind {
                response.headers_mut().insert(
                    "x-nymira-signature-kind",
                    axum::http::HeaderValue::from_static(kind),
                );
            }
            response
        }
    }
}

pub async fn models_handler(
    State(gw): State<Arc<Gateway>>,
    axum::Extension(scope): axum::Extension<Scope>,
) -> axum::Json<Value> {
    let mut list = gw.models_json();
    if let (Scope::Agent { policy, .. }, Some(data)) = (&scope, list["data"].as_array_mut()) {
        data.retain(|m| policy.allowed_models.iter().any(|a| m["id"] == a.as_str()));
    }
    axum::Json(list)
}

#[cfg(test)]
mod settled_tests {
    use super::*;

    #[test]
    fn a_changed_withdrawal_key_is_refused_like_a_changed_proving_key() {
        use sha2::{Digest, Sha256};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("withdrawal.pk");
        std::fs::write(&path, b"withdrawal key bytes").unwrap();
        let pinned = hex::encode(Sha256::digest(b"withdrawal key bytes"));
        let dir_str = dir.path().to_str().unwrap();
        assert!(check_withdrawal_key(dir_str, &pinned).is_ok());
        std::fs::write(&path, b"withdrawal key bytes!").unwrap();
        let err = check_withdrawal_key(dir_str, &pinned).unwrap_err();
        assert!(
            err.contains("withdrawal.pk") && err.contains("pinned"),
            "{err}"
        );
        // A key file that is missing is an error too, not a pass.
        std::fs::remove_file(&path).unwrap();
        assert!(check_withdrawal_key(dir_str, &pinned).is_err());
    }

    #[test]
    fn a_settled_lease_tells_the_trust_label_the_charge_and_each_warning() {
        let lines = settled_lines(&Settled {
            lease_id: "l1".into(),
            charge: 5_000,
            expected: 2_000,
            trust: Trust::NotVerified("no signature".into()),
            warnings: vec!["the operator charged more than one step over".into()],
        });
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].contains("l1") && lines[0].contains("not-verified"),
            "{lines:?}"
        );
        assert!(lines[0].contains("5000") && lines[0].contains("2000"));
        assert!(lines[1].starts_with("warning: lease l1:"));
    }
}
