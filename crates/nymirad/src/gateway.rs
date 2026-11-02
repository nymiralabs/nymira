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

use crate::attest::{Attestor, ModelIdentity, ReportRequest, Trust};
use crate::e2ee::{self, ClientKeys};
use crate::lease::{self, Attempt, Lease, LeaseClient, LeaseError, Pinned};
use crate::openai::ApiError;
use crate::pins::{self, Pins, Tripwire};
use crate::records::{LeaseRecord, Records};
use crate::rpc::{client, client_with_identity, send_error, Rpc, RpcError, Transport};
use crate::secrets::{NoteEntry, NoteStatus, NoteStore};
use crate::sizing;
use crate::snapshot;
use crate::walletvault::WalletVault;

pub const DAY_SECS: u64 = 86_400;
/// How long a verified attestation report is trusted before a fresh one is fetched.
const ATTESTATION_TTL_SECS: u64 = 300;
/// The wallet's own near-expiry margin for settlement.
const SETTLEMENT_WINDOW_SECS: u64 = 21_905;
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
    pub models: Vec<ModelSpec>,
    pub rounds: u32,
    /// Seconds of silence after which a lease is retired.
    pub idle_secs: u64,
    /// Percent of a lease's remaining bound one request may use: 90, or 70 if the first-day
    /// overspend rule triggers (it did not, see `docs/day-1-checks.md`).
    pub share_pct: u128,
    /// A new lease waits a random time up to this many seconds before anything goes out, and
    /// opens its Tor circuit meanwhile (`net::prewarm`), so lease timing does not follow the
    /// moment the user asked. 0 turns both off; over a direct transport they are off anyway.
    pub prewarm_max_secs: u64,
    pub backup: Option<(age::x25519::Recipient, Vec<PathBuf>)>,
}

#[derive(Default, Clone, Copy)]
pub struct ChatOptions {
    /// `X-Nymira-Max-Lease-Usd`: 0.5 or more opens the large class.
    pub max_lease_usd: Option<f64>,
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
}

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

pub struct Gateway {
    cfg: GatewayConfig,
    transport: Transport,
    rpc: Rpc,
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
        let vault = WalletVault::new(&cfg.home, passphrase, cfg.rounds);
        vault.seal_leftovers().map_err(|e| e.to_string())?;
        Ok(Self {
            records: Records::new(&cfg.home),
            transport,
            rpc,
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

    pub fn records(&self) -> Vec<LeaseRecord> {
        self.records.list()
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
    ) -> Result<(NoteEntry, Wallet), ApiError> {
        let notes = self
            .notes
            .list()
            .map_err(|e| unavailable("notes_unreadable", e.to_string()))?;
        let now = self.clock.now_secs();
        let accepted: Vec<_> = notes
            .into_iter()
            .filter(|n| n.status == NoteStatus::Accepted && n.note_id.is_some())
            .collect();
        if accepted.is_empty() {
            return Err(ApiError::no_funds(
                "no accepted note: deposit one with `nymirad deposit` first",
            ));
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
            if expiry.saturating_sub(now) < 5 * DAY_SECS {
                reasons.push(format!("note {id} expires within 5 days"));
                continue;
            }
            match self.unsealed_wallet(&entry, bound).await {
                Ok(wallet) => {
                    let balance = wallet.state().map_or(0, |s| s.current_balance);
                    if balance < bound {
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
        if wallet.has_pending_request() {
            match wallet.recover_pending().await {
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
        let nonce = hex::encode(rand::random::<[u8; 32]>());
        // Its own identity: the attestation fetch is not on a lease's circuit. Every client the
        // verifier builds (gateway, Intel, NVIDIA) goes through the same proxy.
        let identity = crate::net::identity();
        let http = client_with_identity(&self.transport, &identity)
            .map_err(|e| net_error("attestation_unreachable", e))?;
        let socks_url = self
            .transport
            .socks_url(&identity)
            .map_err(|e| net_error("attestation_unreachable", e))?;
        self.attestor
            .verify_model(&ReportRequest {
                gateway_url: &self.cfg.gateway_url,
                model,
                nonce: &nonce,
                http: &http,
                socks_url: socks_url.as_deref(),
            })
            .await
            .map_err(|e| unavailable("attestation_failed", e))
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
        let (entry, wallet) = self.pick_note(bound, &busy).await?;
        let (note_id, note_key) = (entry.note_id.expect("picked"), entry.commitment.clone());
        let wallet = Mutex::new(wallet);
        let seen: std::sync::Mutex<Option<(String, u32, u64)>> = std::sync::Mutex::new(None);
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
                    *seen.lock().unwrap_or_else(|e| e.into_inner()) = Some((
                        format!("0x{}", hex::encode(snap.root.0)),
                        snap.leaf_count,
                        snap.block_height,
                    ));
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
        let (root, leaf_count, snapshot_block) = seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .unwrap_or_default();
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
            warnings: vec![],
        };
        let _ = self.records.write(&record);
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
                !s.retiring && Some(bound) == want && s.spent_micro + s.reserved_micro < bound / 2
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

    pub async fn chat(
        self: &Arc<Self>,
        request: Value,
        opts: ChatOptions,
    ) -> Result<Outcome, ApiError> {
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
        let stream = request["stream"].as_bool().unwrap_or(false);
        let caller_max = request["max_tokens"]
            .as_u64()
            .or_else(|| request["max_completion_tokens"].as_u64());

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
        let max = caller_max.map_or(ours, |c| c.min(ours));
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
            let bound: u128 = s.record.bound_class.parse().unwrap_or(0);
            if s.spent_micro + s.reserved_micro + reserve > bound * self.cfg.share_pct / 100 {
                return Err(ApiError::no_funds(
                    "the lease is busy with other requests and cannot also pay for this one; retry in a moment",
                ));
            }
            s.reserved_micro += reserve;
            s.last_used = self.clock.now_secs();
        }

        self.transport
            .ensure_ready()
            .map_err(|e| net_error("operator_unreachable", e))?;
        let sent = http
            .post(format!("{}/v1/chat/completions", self.cfg.operator_url))
            .header("content-type", "application/json")
            .header("x-nymira-lease-id", &lease.id)
            .header(lease::TOKEN_HEADER, lease.token.expose())
            .header("x-signing-algo", "ed25519")
            .header("x-client-pub-key", keys.public_hex())
            .header("x-model-pub-key", hex::encode(identity.model_pub))
            .header("x-encryption-version", "2")
            .body(body.clone())
            .send()
            .await;
        let release = |this: &Arc<Self>| {
            let (this, key) = (this.clone(), key.clone());
            async move {
                if let Some(s) = this.sessions.lock().await.get_mut(&key) {
                    s.reserved_micro = s.reserved_micro.saturating_sub(reserve);
                }
            }
        };
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
            let stream = futures_util::stream::unfold(state, next_chunk);
            return Ok(Outcome::Stream(Body::from_stream(stream), lease.id));
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
        ctx.done(raw.to_vec(), chat_id, usage).await;
        Ok(Outcome::Json(answer, lease.id))
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

    /// The whole state, encrypted to the backup recipient: sealed notes and wallets, and the
    /// lease records.
    fn back_up(&self) {
        let Some((recipient, places)) = &self.cfg.backup else {
            return;
        };
        let mut files = std::collections::BTreeMap::new();
        let mut add = |rel: String, path: PathBuf| {
            if let Ok(bytes) = std::fs::read(path) {
                use base64::Engine;
                files.insert(rel, base64::engine::general_purpose::STANDARD.encode(bytes));
            }
        };
        add("notes.sealed".into(), self.cfg.home.join("notes.sealed"));
        add(
            crate::records::FILE.into(),
            self.cfg.home.join(crate::records::FILE),
        );
        if let Ok(dir) = std::fs::read_dir(self.cfg.home.join("wallets")) {
            for e in dir.flatten() {
                add(
                    format!("wallets/{}", e.file_name().to_string_lossy()),
                    e.path(),
                );
            }
        }
        let plain = serde_json::to_vec(&files).expect("files serialize");
        let _ = crate::backup::write_backups(recipient, &plain, places, "state.age");
    }

    /// Background loop: retire idle leases and settle finished ones.
    pub fn spawn_background(self: &Arc<Self>, every: std::time::Duration) {
        let gw = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(every).await;
                gw.reap(false).await;
                gw.settle().await;
            }
        });
    }
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
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(std::path::Path::new(setup_dir).join("request.pk"))
        .map_err(|e| format!("the proving key could not be read: {e}"))?;
    let have = hex::encode(Sha256::digest(&bytes));
    if have.eq_ignore_ascii_case(expected_hex) {
        Ok(())
    } else {
        Err(format!(
            "the proving key does not match the pinned hash (found {have})"
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
}

impl Finish {
    /// The exchange failed after the operator may have billed it: free the reservation and note
    /// that the usage is unknown, so the charge is not judged against a partial picture.
    async fn failed(self) {
        let mut sessions = self.gw.sessions.lock().await;
        if let Some(s) = sessions.get_mut(&self.key) {
            s.reserved_micro = s.reserved_micro.saturating_sub(self.reserve);
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
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let request: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return ApiError::invalid_request("the body is not JSON").into_response(),
    };
    let opts = ChatOptions {
        max_lease_usd: headers
            .get("x-nymira-max-lease-usd")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse().ok()),
    };
    match gw.chat(request, opts).await {
        Err(e) => e.into_response(),
        Ok(Outcome::Json(v, lease)) => (
            StatusCode::OK,
            [("x-nymira-lease-id", lease)],
            axum::Json(v),
        )
            .into_response(),
        Ok(Outcome::Stream(body, lease)) => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .header("x-nymira-lease-id", lease)
            .body(body)
            .expect("static response"),
    }
}

pub async fn models_handler(State(gw): State<Arc<Gateway>>) -> axum::Json<Value> {
    axum::Json(gw.models_json())
}
