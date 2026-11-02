//! The vault watcher: answers stale exits, completes expiries and exits, retires closed leaves
//! and keeps the tree moving.
//!
//! It reads the vault through two or more independent providers. An exit counts once two of them
//! report it (all of them after ten failed answers in an hour), and only then does its pair of
//! nullifiers join the operator's blocked set and does any gas go out. The watcher holds two
//! keys behind [`VaultCalls`]: one for exit answers with its own balance, one for upkeep. It has
//! no way to send `mutual_close`: that call belongs to the closer.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine;
use serde_json::{json, Value};
use zkapi_core::MerkleTree;
use zkapi_types::wire::ApiRequestV2;
use zkapi_types::Felt252;

use crate::alarm::Alarms;
use crate::chain::{ExitEntry, NoteRecord, VaultView};
use crate::clock::Clock;
use crate::store::{self, Store};
use crate::tx::{NearTx, TxRequest};

const TGAS: u64 = 1_000_000_000_000;
/// Measured on the sandbox (`docs/measurements.md`) times 1.2, rounded up; never above 300 Tgas.
pub const GAS_DEFEAT: u64 = 46 * TGAS;
pub const GAS_RAISE: u64 = 46 * TGAS;
/// The vault wants 30 Tgas for a payout; the call burns about 10.
pub const GAS_FINALIZE: u64 = 40 * TGAS;
pub const GAS_CLAIM: u64 = 30 * TGAS;
/// Not measured for 50 notes: the most one call may carry.
pub const GAS_CLAIM_BATCH: u64 = 299 * TGAS;
/// 202 Tgas for 80 leaves, times 1.2.
pub const GAS_SYNC: u64 = 250 * TGAS;
/// Worst case 94 hashes, about 176 Tgas, times 1.2.
pub const GAS_RETIRE: u64 = 220 * TGAS;
pub const SYNC_LEAVES: u32 = 80;
pub const CLAIM_BATCH: u32 = 50;
const PAGE: u32 = 100;
const YOCTO_PER_NEAR: u128 = 1_000_000_000_000_000_000_000_000;
/// Failed answers in an hour that raise an alarm, and that make the watcher wait for every
/// provider to agree.
const FAILED_ALARM: usize = 3;
const FAILED_DEGRADE: usize = 10;
/// New exit requests within [`WAVE_WINDOW_SECS`] that raise an alarm.
const WAVE_SIZE: usize = 3;
const WAVE_WINDOW_SECS: u64 = 3_600;
/// Providers further apart than this many blocks disagree.
const MAX_BLOCK_GAP: u64 = 10;
const HOUR: u64 = 3_600;
/// A call that failed is not repeated for this long.
const RETRY_SECS: u64 = 60;
/// After an answer went out, the same note is left alone this long.
const SUCCESS_PAUSE_SECS: u64 = 30;
/// Closed leaves retired in one poll; each retirement needs a fresh read of the tree.
const RETIRES_PER_POLL: usize = 5;

/// Which key sends a call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    /// `defeat_exit` and `raise_concession`, from the exit-answer account.
    Answer,
    /// `sync_root`, `retire_closed_leaf`, `claim_expired*` and `finalize_exit`.
    Upkeep,
}

#[async_trait]
pub trait VaultCalls: Send + Sync {
    /// Sends the call and returns once it is final.
    async fn call(
        &self,
        key: Key,
        method: &'static str,
        args: Value,
        gas: u64,
    ) -> Result<(), String>;
    /// The exit-answer account's balance in yoctoNEAR.
    async fn answer_balance(&self) -> Result<u128, String>;
}

/// Sends through two [`NearTx`] senders, one per key.
pub struct NearCalls {
    pub answer: Arc<NearTx>,
    pub upkeep: Arc<NearTx>,
}

#[async_trait]
impl VaultCalls for NearCalls {
    async fn call(
        &self,
        key: Key,
        method: &'static str,
        args: Value,
        gas: u64,
    ) -> Result<(), String> {
        let (tx, kind) = match key {
            Key::Answer => (&self.answer, "answer"),
            Key::Upkeep => (&self.upkeep, "upkeep"),
        };
        let end = tx
            .send(TxRequest {
                kind,
                method,
                args: args.to_string().into_bytes(),
                gas,
                idem_key: None,
            })
            .await
            .map_err(|e| e.to_string())?;
        if end.success {
            Ok(())
        } else {
            Err(end.detail)
        }
    }

    async fn answer_balance(&self) -> Result<u128, String> {
        self.answer.balance().await.map_err(|e| e.to_string())
    }
}

/// One call a [`FakeCalls`] saw.
#[derive(Clone, Debug)]
pub struct SeenCall {
    pub key: Key,
    pub method: &'static str,
    pub args: Value,
    pub gas: u64,
}

/// Records calls; chosen methods can be made to fail.
pub struct FakeCalls {
    seen: Mutex<Vec<SeenCall>>,
    failing: Mutex<Vec<&'static str>>,
    balance: Mutex<u128>,
}

impl Default for FakeCalls {
    fn default() -> Self {
        Self {
            seen: Mutex::new(Vec::new()),
            failing: Mutex::new(Vec::new()),
            balance: Mutex::new(10 * YOCTO_PER_NEAR),
        }
    }
}

impl FakeCalls {
    pub fn seen(&self) -> Vec<SeenCall> {
        self.seen.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn fail(&self, methods: &[&'static str]) {
        *self.failing.lock().unwrap_or_else(|e| e.into_inner()) = methods.to_vec();
    }

    pub fn set_balance(&self, yocto: u128) {
        *self.balance.lock().unwrap_or_else(|e| e.into_inner()) = yocto;
    }
}

#[async_trait]
impl VaultCalls for FakeCalls {
    async fn call(
        &self,
        key: Key,
        method: &'static str,
        args: Value,
        gas: u64,
    ) -> Result<(), String> {
        self.seen
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(SeenCall {
                key,
                method,
                args,
                gas,
            });
        if self
            .failing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains(&method)
        {
            return Err("the vault refused".into());
        }
        Ok(())
    }

    async fn answer_balance(&self) -> Result<u128, String> {
        Ok(*self.balance.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

#[derive(Clone, Debug)]
pub struct WatcherConfig {
    /// Providers that must report an exit before it counts: 2 on testnet, 3 on mainnet.
    pub quorum: usize,
    /// Exit-answer balance under which an alarm goes out.
    pub balance_alarm: u128,
    /// Vault liquid NEAR under which an alarm goes out: the vault's own floor (2 NEAR, under it
    /// deposits stop) plus 1 NEAR of warning.
    pub vault_liquid_alarm: u128,
    /// A note a batch did not sweep is looked at again after this long.
    pub claim_retry_secs: u64,
    /// Least time between two `sync_root` calls.
    pub sync_every_secs: u64,
}

impl Default for WatcherConfig {
    fn default() -> Self {
        Self {
            quorum: 2,
            balance_alarm: 2 * YOCTO_PER_NEAR,
            vault_liquid_alarm: 3 * YOCTO_PER_NEAR,
            claim_retry_secs: HOUR,
            sync_every_secs: 30,
        }
    }
}

#[derive(Default)]
struct State {
    /// When answers failed, newest last, within the hour.
    failed: VecDeque<u64>,
    /// Earliest time a call for a note or an exit may be sent again.
    not_before: HashMap<(&'static str, u32), u64>,
    last_sync: u64,
    last_poll: Option<u64>,
    /// Exits already counted toward a wave, by note and nullifier.
    seen_exits: HashSet<(u32, String)>,
    /// When new exits were first confirmed, within the hour.
    wave: VecDeque<u64>,
    /// Polls in a row in which the providers disagreed.
    disagreed: u32,
}

pub struct Watcher {
    providers: Vec<Arc<dyn VaultView>>,
    calls: Arc<dyn VaultCalls>,
    store: Store,
    clock: Arc<dyn Clock>,
    alarms: Arc<Alarms>,
    cfg: WatcherConfig,
    state: Mutex<State>,
    /// The name under which the last poll is written to the database, when another process
    /// (the public API) shows it.
    id: Option<String>,
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// The `proof` and `inputs` arguments the vault takes from a stored request proof.
fn request_args(request: &ApiRequestV2) -> (String, Vec<String>) {
    let inputs = request
        .public_inputs
        .to_field_elements()
        .iter()
        .map(|f| b64(f.as_bytes()))
        .collect();
    (request.proof.proof.clone(), inputs)
}

fn felt_from_hex(text: &str) -> Option<Felt252> {
    let bytes = hex::decode(text.trim_start_matches("0x")).ok()?;
    Some(Felt252(bytes.try_into().ok()?))
}

impl Watcher {
    pub fn new(
        providers: Vec<Arc<dyn VaultView>>,
        calls: Arc<dyn VaultCalls>,
        store: Store,
        clock: Arc<dyn Clock>,
        alarms: Arc<Alarms>,
        cfg: WatcherConfig,
    ) -> Result<Self, String> {
        if providers.len() < cfg.quorum.max(1) {
            return Err(format!(
                "{} providers given, {} needed",
                providers.len(),
                cfg.quorum
            ));
        }
        Ok(Self {
            providers,
            calls,
            store,
            clock,
            alarms,
            cfg,
            state: Mutex::new(State::default()),
            id: None,
        })
    }

    pub fn with_id(mut self, id: &str) -> Self {
        self.id = Some(id.into());
        self
    }

    /// The time of the last poll that reached enough providers, for `/v1/status`.
    pub fn last_poll(&self) -> Option<u64> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .last_poll
    }

    fn failed_this_hour(&self) -> usize {
        let now = self.clock.now_secs();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while state.failed.front().is_some_and(|at| now >= at + HOUR) {
            state.failed.pop_front();
        }
        state.failed.len()
    }

    fn due(&self, what: &'static str, id: u32) -> bool {
        let now = self.clock.now_secs();
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .not_before
            .get(&(what, id))
            .is_none_or(|at| now >= *at)
    }

    fn wait(&self, what: &'static str, id: u32) {
        self.wait_for(what, id, RETRY_SECS);
    }

    fn wait_for(&self, what: &'static str, id: u32, secs: u64) {
        let at = self.clock.now_secs() + secs;
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .not_before
            .insert((what, id), at);
    }

    /// Sends an exit answer and counts it when it fails.
    async fn answer(&self, method: &'static str, note_id: u32, args: Value, gas: u64) {
        match self.calls.call(Key::Answer, method, args, gas).await {
            // Give the final view time to show the answer before the same call goes out again.
            Ok(()) => self.wait_for("answer", note_id, SUCCESS_PAUSE_SECS),
            Err(_) => {
                self.wait("answer", note_id);
                let now = self.clock.now_secs();
                self.state
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .failed
                    .push_back(now);
                if self.failed_this_hour() >= FAILED_ALARM {
                    self.alarms
                        .raise(
                            "answers_failing",
                            "Exit answers are failing",
                            &format!(
                                "{} exit answers failed in the last hour (last: {method} for note {note_id}).",
                                self.failed_this_hour()
                            ),
                        )
                        .await;
                }
            }
        }
    }

    /// Runs every duty once.
    pub async fn poll(&self) {
        let now = self.clock.now_secs();
        let confirmed = self.confirmed_exits().await;
        if let Some(exits) = &confirmed {
            self.state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .last_poll = Some(now);
            if let Some(id) = self.id.clone() {
                let _ = self
                    .store
                    .run(move |c| store::set_watcher_poll(c, &id, now))
                    .await;
            }
            self.check_wave(exits).await;
            for exit in exits {
                self.import(exit).await;
            }
            for exit in exits {
                self.answer_exit(exit).await;
            }
        }
        if self.failed_this_hour() < FAILED_ALARM {
            self.alarms.resolve("answers_failing");
        }
        self.check_balance().await;
        self.check_vault_liquid().await;
        self.check_providers().await;
        let Some(view) = self.providers.first() else {
            return;
        };
        if let Ok(records) = self.records(view.as_ref()).await {
            self.retire_closed(view.as_ref(), &records).await;
            self.claim_expired(&records).await;
            self.sync_root(view.as_ref(), &records).await;
        }
    }

    /// Counts exits not seen before; a burst of them gets a person's attention.
    async fn check_wave(&self, exits: &[ExitEntry]) {
        let now = self.clock.now_secs();
        let size = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            for exit in exits {
                if state
                    .seen_exits
                    .insert((exit.note_id, exit.nullifier.to_ascii_lowercase()))
                {
                    state.wave.push_back(now);
                }
            }
            while state
                .wave
                .front()
                .is_some_and(|at| now >= at + WAVE_WINDOW_SECS)
            {
                state.wave.pop_front();
            }
            state.wave.len()
        };
        if size >= WAVE_SIZE {
            self.alarms
                .raise(
                    "exit_wave",
                    "A wave of exit requests",
                    &format!("{size} new exit requests in the last hour; look at them."),
                )
                .await;
        } else {
            self.alarms.resolve("exit_wave");
        }
    }

    /// Providers more than ten blocks apart, or showing different leaf counts at one block, in
    /// two polls in a row. A few blocks of drift is normal and says nothing.
    async fn check_providers(&self) {
        let mut heads = Vec::new();
        for provider in &self.providers {
            if let Ok(head) = provider.head().await {
                heads.push(head);
            }
        }
        let differ = heads.iter().any(|a| {
            heads.iter().any(|b| {
                a.height.abs_diff(b.height) > MAX_BLOCK_GAP
                    || (a.height == b.height && a.leaf_count != b.leaf_count)
            })
        });
        let streak = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.disagreed = if differ { state.disagreed + 1 } else { 0 };
            state.disagreed
        };
        if streak >= 2 {
            self.alarms
                .raise(
                    "providers_disagree",
                    "RPC providers disagree",
                    &format!(
                        "The providers have not agreed for {streak} polls in a row; heads (height, leaves): {}.",
                        heads
                            .iter()
                            .map(|h| format!("({}, {})", h.height, h.leaf_count))
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                )
                .await;
        } else if !differ {
            self.alarms.resolve("providers_disagree");
        }
    }

    /// Exits reported by enough providers: two, or all of them while answers keep failing.
    async fn confirmed_exits(&self) -> Option<Vec<ExitEntry>> {
        let mut reports: Vec<Vec<ExitEntry>> = Vec::new();
        for provider in &self.providers {
            if let Ok(exits) = provider.exits().await {
                reports.push(exits);
            }
        }
        let degraded = self.failed_this_hour() >= FAILED_DEGRADE;
        if reports.len() < self.cfg.quorum {
            return None;
        }
        // While answers keep failing, every provider that answers must show the exit.
        let needed = if degraded {
            reports.len()
        } else {
            self.cfg.quorum
        };
        let mut merged: Vec<(ExitEntry, usize)> = Vec::new();
        for report in &reports {
            for exit in report {
                let key = (exit.note_id, exit.nullifier.to_ascii_lowercase());
                match merged.iter_mut().find(|(e, _)| {
                    (e.note_id, e.nullifier.to_ascii_lowercase()) == key
                        && e.next_nullifier.eq_ignore_ascii_case(&exit.next_nullifier)
                }) {
                    Some((e, n)) => {
                        *n += 1;
                        // The most conceded any provider shows: a raise is skipped if one has it.
                        e.conceded = e.conceded.max(exit.conceded);
                    }
                    None => merged.push((exit.clone(), 1)),
                }
            }
        }
        Some(
            merged
                .into_iter()
                .filter(|(_, n)| *n >= needed)
                .map(|(e, _)| e)
                .collect(),
        )
    }

    /// The pair of nullifiers joins the blocked set: refused for leases, successor signatures and
    /// clearances from now on.
    async fn import(&self, exit: &ExitEntry) {
        let now = self.clock.now_secs();
        for n in [&exit.nullifier, &exit.next_nullifier] {
            let n = n.to_ascii_lowercase();
            let _ = self
                .store
                .run(move |c| store::import_exit(c, &n, now))
                .await;
        }
    }

    async fn request_at(&self, nullifier: &str) -> Option<(store::LeaseRow, ApiRequestV2)> {
        let n = nullifier.to_ascii_lowercase();
        let (lease, json) = self
            .store
            .run(move |c| {
                let lease = store::lease_by_nullifier(c, &n)?;
                let json = match &lease {
                    Some(l) => store::request_json(c, &l.lease_id)?,
                    None => None,
                };
                Ok((lease, json))
            })
            .await
            .ok()?;
        let request: ApiRequestV2 = serde_json::from_str(&json?).ok()?;
        Some((lease?, request))
    }

    async fn answer_exit(&self, exit: &ExitEntry) {
        let now_ns = u128::from(self.clock.now_secs()) * 1_000_000_000;
        if !self.due("answer", exit.note_id) {
            return;
        }
        if now_ns >= exit.deadline {
            self.finalize(exit).await;
            return;
        }
        // A provider that shows the note out of its exit (closed, swept, defeated) stops the
        // call; one that cannot say does not.
        for provider in &self.providers {
            if let Ok(Some(note)) = provider.note(exit.note_id).await {
                if note.status != "PendingExit" {
                    return;
                }
            }
        }
        if let Some((lease, request)) = self.request_at(&exit.next_nullifier).await {
            if lease.status != "provisioning" {
                let (proof, inputs) = request_args(&request);
                self.answer(
                    "defeat_exit",
                    exit.note_id,
                    json!({ "note_id": exit.note_id, "proof": proof, "inputs": inputs }),
                    GAS_DEFEAT,
                )
                .await;
                return;
            }
        }
        if let Some((lease, request)) = self.request_at(&exit.nullifier).await {
            if !lease.delivered {
                return;
            }
            // The vault takes the least of the amount, the proof's bound and the balance, and
            // wants the result above what is conceded: skip a call that cannot raise it.
            let balance = exit.final_balance + exit.conceded;
            if lease.bound.min(balance) <= exit.conceded {
                return;
            }
            let (proof, inputs) = request_args(&request);
            self.answer(
                "raise_concession",
                exit.note_id,
                json!({
                    "note_id": exit.note_id, "proof": proof, "inputs": inputs,
                    "amount": lease.bound.to_string(),
                }),
                GAS_RAISE,
            )
            .await;
        }
    }

    /// An undefeated exit past its deadline is paid by anyone.
    async fn finalize(&self, exit: &ExitEntry) {
        match self
            .calls
            .call(
                Key::Upkeep,
                "finalize_exit",
                json!({ "note_id": exit.note_id }),
                GAS_FINALIZE,
            )
            .await
        {
            Ok(()) => {}
            Err(_) => self.wait("answer", exit.note_id),
        }
    }

    async fn check_balance(&self) {
        match self.calls.answer_balance().await {
            Ok(balance) if balance < self.cfg.balance_alarm => {
                self.alarms
                    .raise(
                        "answer_balance_low",
                        "The exit-answer account is running low",
                        &format!(
                            "The exit-answer account holds {}.{:03} NEAR; top it up before exits need answers.",
                            balance / YOCTO_PER_NEAR,
                            balance % YOCTO_PER_NEAR / (YOCTO_PER_NEAR / 1_000)
                        ),
                    )
                    .await;
            }
            Ok(_) => self.alarms.resolve("answer_balance_low"),
            Err(_) => {}
        }
    }

    async fn check_vault_liquid(&self) {
        let Some(view) = self.providers.first() else {
            return;
        };
        match view.liquid_near().await {
            Ok(liquid) if liquid < self.cfg.vault_liquid_alarm => {
                self.alarms
                    .raise(
                        "vault_liquid_low",
                        "The vault is running low on NEAR",
                        &format!(
                            "The vault can spend {}.{:03} NEAR after its storage. Under 2 NEAR it refuses deposits and a failed payout may not be recorded; top it up.",
                            liquid / YOCTO_PER_NEAR,
                            liquid % YOCTO_PER_NEAR / (YOCTO_PER_NEAR / 1_000)
                        ),
                    )
                    .await;
            }
            Ok(_) => self.alarms.resolve("vault_liquid_low"),
            Err(_) => {}
        }
    }

    /// Every note record, paged.
    async fn records(&self, view: &dyn VaultView) -> Result<Vec<NoteRecord>, ()> {
        let mut all = Vec::new();
        loop {
            let page = view
                .note_records(all.len() as u32, PAGE)
                .await
                .map_err(|_| ())?;
            let full = page.len() as u32 >= PAGE;
            all.extend(page);
            if !full {
                return Ok(all);
            }
        }
    }

    /// Zeroes the leaf of every closed note the tree still holds.
    async fn retire_closed(&self, view: &dyn VaultView, records: &[NoteRecord]) {
        let Ok(leaf_count) = view.leaf_count().await else {
            return;
        };
        let todo = records
            .iter()
            .filter(|r| r.status == "Closed" && !r.leaf_retired && r.note_id < leaf_count)
            .filter(|r| self.due("retire", r.note_id))
            .take(RETIRES_PER_POLL);
        for record in todo {
            let Some(siblings) = self.siblings(view, record.note_id, leaf_count).await else {
                return;
            };
            let sent = self
                .calls
                .call(
                    Key::Upkeep,
                    "retire_closed_leaf",
                    json!({ "note_id": record.note_id, "siblings": siblings }),
                    GAS_RETIRE,
                )
                .await;
            if sent.is_err() {
                self.wait("retire", record.note_id);
                return;
            }
        }
    }

    /// The siblings `retire_closed_leaf` wants, bottom up, as the tree stands now.
    async fn siblings(
        &self,
        view: &dyn VaultView,
        note_id: u32,
        leaf_count: u32,
    ) -> Option<Vec<String>> {
        let mut leaves: Vec<Felt252> = Vec::with_capacity(leaf_count as usize);
        while (leaves.len() as u32) < leaf_count {
            let page = view.leaves(leaves.len() as u32, PAGE).await.ok()?;
            if page.is_empty() {
                return None;
            }
            for leaf in page {
                leaves.push(felt_from_hex(&leaf)?);
            }
        }
        leaves.truncate(leaf_count as usize);
        let level = (31 - (note_id ^ leaf_count).leading_zeros()) as usize;
        Some(
            MerkleTree::from_leaves(&leaves).get_siblings(note_id)[..level]
                .iter()
                .map(|s| format!("0x{}", hex::encode(s.as_bytes())))
                .collect(),
        )
    }

    /// Sweeps notes past expiry plus the exit window. The vault skips a note whose last exit
    /// activity is more recent, so the watcher asks again only after a while.
    async fn claim_expired(&self, records: &[NoteRecord]) {
        let now = self.clock.now_secs();
        let Some(delay) = self.exit_delay().await else {
            return;
        };
        let mut candidates: Vec<u32> = records
            .iter()
            .filter(|r| r.status == "Active" && now >= r.expiry.saturating_add(delay))
            .map(|r| r.note_id)
            .filter(|id| self.due("claim", *id))
            .collect();
        candidates.sort_unstable();
        while let Some(&from) = candidates.first() {
            let window: Vec<u32> = candidates
                .iter()
                .copied()
                .take_while(|id| *id < from + CLAIM_BATCH)
                .collect();
            candidates.drain(..window.len());
            let batch = self
                .calls
                .call(
                    Key::Upkeep,
                    "claim_expired_batch",
                    json!({ "from": from, "limit": CLAIM_BATCH }),
                    GAS_CLAIM_BATCH,
                )
                .await;
            if batch.is_err() {
                // One note must not block the rest.
                for id in &window {
                    let _ = self
                        .calls
                        .call(
                            Key::Upkeep,
                            "claim_expired",
                            json!({ "note_id": id }),
                            GAS_CLAIM,
                        )
                        .await;
                }
            }
            let again = self.clock.now_secs() + self.cfg.claim_retry_secs;
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            for id in window {
                state.not_before.insert(("claim", id), again);
            }
        }
    }

    async fn exit_delay(&self) -> Option<u64> {
        for provider in &self.providers {
            if let Ok(config) = provider.config().await {
                return Some(config.exit_delay_secs);
            }
        }
        None
    }

    /// Inserts waiting leaves into the tree.
    async fn sync_root(&self, view: &dyn VaultView, records: &[NoteRecord]) {
        let Ok(leaf_count) = view.leaf_count().await else {
            return;
        };
        if (records.len() as u32) <= leaf_count {
            return;
        }
        let now = self.clock.now_secs();
        {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            if now < state.last_sync + self.cfg.sync_every_secs {
                return;
            }
            state.last_sync = now;
        }
        let _ = self
            .calls
            .call(
                Key::Upkeep,
                "sync_root",
                json!({ "max": SYNC_LEAVES }),
                GAS_SYNC,
            )
            .await;
    }

    /// Runs [`Watcher::poll`] every ten seconds for as long as the process lives.
    pub fn spawn_background(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        let watcher = self.clone();
        tokio::spawn(async move {
            loop {
                watcher.poll().await;
                tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            }
        })
    }
}
