//! `nymira-vault`: USDC deposits as notes in a Poseidon Merkle tree.
//!
//! `new()`, deposits through `ft_on_transfer`, `sync_root`, mutual close, payouts, leaf retirement
//! and the views.

mod events;
mod pins;
mod tree;
mod word;
mod zero_hashes;

use near_sdk::json_types::{Base64VecU8, U128, U64};
use near_sdk::serde_json::json;
use near_sdk::store::{Lazy, LookupMap, LookupSet, Vector};
use near_sdk::{
    assert_one_yocto, env, near, require, AccountId, BorshStorageKey, Gas, NearToken,
    PanicOnDefault, Promise, PromiseError, PromiseOrValue,
};
use nymira_near_codec::{
    chain_id, check_signing_key, contract_address, destination, public_input_from_wire, Network,
    Proof, VerifyingKey, SCALAR_MODULUS_BE,
};
use zkapi_core::Felt252;

#[cfg(all(feature = "mainnet", feature = "dev-setup"))]
compile_error!("the mainnet profile must not enable dev-setup");
#[cfg(all(feature = "judge", feature = "dev-setup"))]
compile_error!("the public judge vault and testnet bundle must not enable dev-setup");
#[cfg(all(feature = "mainnet", feature = "test-intents-account"))]
compile_error!("the mainnet profile must not enable test-intents-account");

#[cfg(feature = "mainnet")]
const NETWORK: Network = Network::Mainnet;
#[cfg(not(feature = "mainnet"))]
const NETWORK: Network = Network::Testnet;

/// Layout version of the state struct.
const STATE_VERSION: u32 = 1;
/// Liquid NEAR under which a deposit is returned (exits, callbacks and registrations need NEAR).
const RESERVE: u128 = 2_000_000_000_000_000_000_000_000;
/// 0.03 USDC.
const CLOSE_FEE: u128 = 30_000;
/// The account whose deposits take the Intents branch; a payout account may never be it. Sandbox
/// builds (`test-intents-account`) name the open-source Verifier fixture's account instead. On the
/// testnet vault `intents.near` does not exist, so the branch stays dormant there.
#[cfg(feature = "test-intents-account")]
const INTENTS_ACCOUNT: &str = "intents.test.near";
#[cfg(not(feature = "test-intents-account"))]
const INTENTS_ACCOUNT: &str = "intents.near";
const ONE_DAY_SECS: u64 = 86_400;
/// `sync_root` inserts when this long has passed since the last insert...
const SYNC_WAIT_NS: u64 = 30_000_000_000;
/// ...or when this many leaves wait.
const SYNC_BATCH: u32 = 16;
/// At most this many leaves per `sync_root` call. Measured on the sandbox (2026-11-01): a Poseidon
/// hash costs about 1.8 Tgas, so 120 leaves burnt 287 Tgas, and the worst alignment of a batch is
/// about `n + 47` hashes. 80 leaves stay under 250 Tgas, which is 300 Tgas with the 1.2 margin.
const MAX_SYNC_LEAVES: u32 = 80;
/// zkapi's protocol version (`zkapi_types::PROTOCOL_VERSION`, checked by a test).
const PROTOCOL_VERSION: u16 = 2;
/// 0.00125 NEAR: what USDC keeps to register an account (`storage_balance_bounds` minimum).
const REGISTRATION_DEPOSIT: u128 = 1_250_000_000_000_000_000_000;
/// Fixed gas of each payout leg and of its callback, never the caller's remaining gas. Placeholders
/// with margin over the mock token; ticket #22 measures them on the real USDC.
const GAS_STORAGE_DEPOSIT: Gas = Gas::from_tgas(10);
const GAS_FT_TRANSFER: Gas = Gas::from_tgas(20);
const GAS_CALLBACK: Gas = Gas::from_tgas(10);
/// A payout method needs its two legs and callback, plus its own work.
const MIN_GAS_CLAIM: Gas = Gas::from_tgas(50);
/// `mutual_close` also verifies a Groth16 proof (about 40 Tgas with parsing).
const MIN_GAS_MUTUAL_CLOSE: Gas = Gas::from_tgas(110);
/// Page size cap of `get_leaves`, `note_records` and the other paged views.
const MAX_PAGE: u32 = 100;
/// At most this many notes per `claim_expired_batch` call.
const MAX_CLAIM_BATCH: u32 = 50;
const NS_PER_SEC: u64 = 1_000_000_000;
/// A proposed TVL cap or parameter change applies no earlier than this after the proposal...
const PROPOSAL_DELAY_NS: u64 = 48 * 3_600 * NS_PER_SEC;
/// ...and lapses when it is still not applied this long after it became applicable.
const PROPOSAL_LAPSE_NS: u64 = 7 * ONE_DAY_SECS * NS_PER_SEC;
/// `slot_rate` is parts per million of the lease bound per minute.
const MAX_SLOT_RATE: u32 = 1_000_000;

/// Collection prefixes: the enum index is the prefix byte. Append only, never reorder.
#[derive(BorshStorageKey)]
#[near]
enum StorageKey {
    Notes,
    Commitments,
    KnownRoots,
    RequestVk,
    WithdrawalVk,
    Nullifiers,
    PendingPayouts,
    Exits,
    Flagged,
}

#[near(serializers = [borsh, json])]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Status {
    Active,
    PendingExit,
    Closed,
}

#[near(serializers = [borsh])]
struct Note {
    commitment: [u8; 32],
    /// The denomination, the circuit's note deposit.
    deposit_amount: u128,
    /// Unix seconds.
    expiry: u64,
    status: Status,
    withdraw_to: AccountId,
    /// `note_leaf(note_id, commitment, deposit_amount, expiry)`, computed at deposit.
    leaf: [u8; 32],
    leaf_retired: bool,
    /// Block time (nanoseconds) of the last `request_exit` or `defeat_exit`; 0 if none.
    last_exit_ns: u64,
}

impl Note {
    /// The leaf the tree holds for this note: zero once retired.
    fn tree_leaf(&self) -> [u8; 32] {
        if self.leaf_retired {
            [0; 32]
        } else {
            self.leaf
        }
    }
}

/// A pending exit: what `finalize_exit` pays once the deadline has passed.
#[near(serializers = [borsh])]
struct ExitRecord {
    /// N_k, the escaped state's nullifier.
    nullifier: [u8; 32],
    /// N_{k+1}, the successor nullifier a defeat must be a request proof at.
    next_nullifier: [u8; 32],
    final_balance: u128,
    conceded: u128,
    /// Block time (nanoseconds).
    deadline: u64,
}

/// The lease parameters the vault owner may change (with a 48-hour delay). The exit delay, the
/// note lifetime and the note fee are not among them: TVL accounting relies on a fixed note fee.
#[near(serializers = [borsh, json])]
#[derive(Clone)]
pub struct LeaseParams {
    pub bound_classes: Vec<U128>,
    pub charge_step: U128,
    pub min_charge: U128,
    pub slot_rate: u32,
}

impl LeaseParams {
    fn check(&self) {
        require!(!self.bound_classes.is_empty(), "no bound classes");
        for (i, class) in self.bound_classes.iter().enumerate() {
            require!(class.0 > 0, "a bound class is zero");
            require!(
                !self.bound_classes[..i].contains(class),
                "repeated bound class"
            );
        }
        require!(self.charge_step.0 > 0, "charge step is zero");
        require!(self.min_charge.0 > 0, "minimum charge is zero");
        require!(self.slot_rate <= MAX_SLOT_RATE, "slot rate above 100%");
    }
}

/// A proposed TVL cap, waiting for its 48 hours.
#[near(serializers = [borsh])]
struct PendingCap {
    value: u128,
    /// Block time (nanoseconds).
    proposed_at: u64,
}

/// Proposed lease parameters, waiting for their 48 hours.
#[near(serializers = [borsh])]
struct PendingParams {
    value: LeaseParams,
    /// Block time (nanoseconds).
    proposed_at: u64,
}

/// A verifying key in the `alt_bn128` host byte format.
#[near(serializers = [borsh])]
struct StoredKey {
    alpha_g1: [u8; 64],
    beta_g2: [u8; 128],
    gamma_g2: [u8; 128],
    delta_g2: [u8; 128],
    ic: Vec<[u8; 64]>,
}

impl From<VerifyingKey> for StoredKey {
    fn from(key: VerifyingKey) -> Self {
        Self {
            alpha_g1: key.alpha_g1,
            beta_g2: key.beta_g2,
            gamma_g2: key.gamma_g2,
            delta_g2: key.delta_g2,
            ic: key.ic,
        }
    }
}

impl From<&StoredKey> for VerifyingKey {
    fn from(key: &StoredKey) -> Self {
        Self {
            alpha_g1: key.alpha_g1,
            beta_g2: key.beta_g2,
            gamma_g2: key.gamma_g2,
            delta_g2: key.delta_g2,
            ic: key.ic.clone(),
        }
    }
}

/// A Baby Jubjub public key as two big-endian field words, `0x` and 64 hex digits each.
#[near(serializers = [json])]
#[derive(Clone)]
pub struct PublicKey {
    pub x: String,
    pub y: String,
}

#[near(serializers = [borsh])]
#[derive(Clone, Copy)]
struct Point {
    x: [u8; 32],
    y: [u8; 32],
}

impl Point {
    fn parse(key: &PublicKey) -> Point {
        let point = Point {
            x: word::decode32(&key.x).unwrap_or_else(|| env::panic_str("bad key coordinate")),
            y: word::decode32(&key.y).unwrap_or_else(|| env::panic_str("bad key coordinate")),
        };
        require!(
            check_signing_key(&point.x, &point.y).is_ok(),
            "key is off the curve, the identity or outside the subgroup"
        );
        point
    }

    fn json(&self) -> PublicKey {
        PublicKey {
            x: word::encode32(&self.x),
            y: word::encode32(&self.y),
        }
    }
}

#[near(contract_state)]
#[derive(PanicOnDefault)]
pub struct Vault {
    version: u32,
    /// The state key (`operator_pk`) and the clearance key.
    operator_pk: Point,
    clearance_pk: Point,
    token: AccountId,
    treasury: AccountId,
    owner: AccountId,
    operator_account: AccountId,
    denominations: Vec<u128>,
    note_fee: u128,
    tvl_cap: u128,
    exit_delay_secs: u64,
    note_ttl: u64,
    bound_classes: Vec<u128>,
    charge_step: u128,
    min_charge: u128,
    slot_rate: u32,
    request_vk: Lazy<StoredKey>,
    withdrawal_vk: Lazy<StoredKey>,
    /// Note ID is the index.
    notes: Vector<Note>,
    /// Commitment to note ID.
    commitments: LookupMap<[u8; 32], u32>,
    /// Sum of the full deposits (denomination plus note fee) of active and pending-exit notes.
    tvl: u128,
    /// Notes with status `Active`.
    open_notes: u32,
    /// Note fees (and later close fees and spent balances) owed to the treasury account.
    treasury_balance: u128,
    /// Leaves in the tree: notes `0..leaf_count` are inserted.
    leaf_count: u32,
    /// `filled[t]` for `t` in `1..32` (index 0 unused), as zkapi's `Frontier`.
    frontier: [[u8; 32]; 32],
    current_root: [u8; 32],
    /// Raised by every `retire_closed_leaf`: the operator drops stale roots when it changes.
    retirement_epoch: u64,
    known_roots: LookupSet<[u8; 32]>,
    /// Block time of the last insert (nanoseconds).
    last_sync_ns: u64,
    /// Spent nullifiers as canonical big-endian words.
    nullifiers: LookupSet<[u8; 32]>,
    /// Failed payouts by payout account: one record per account.
    pending_payouts: LookupMap<AccountId, u128>,
    /// Pending exits by note ID.
    exits: LookupMap<u32, ExitRecord>,
    /// The note IDs in `exits`, ascending, so that a page boundary survives a deletion.
    // shortcut: a sorted Vec is read on every call; open exits are bounded by the TVL cap
    // (hundreds), upgrade to an ordered map if a mainnet cap admits thousands of notes.
    exit_ids: Vec<u32>,
    /// Nullifiers of escaped states whose exit was defeated: never usable again.
    flagged: LookupSet<[u8; 32]>,
    /// At most one pending proposal of each kind; a new one replaces it and restarts the 48 hours.
    pending_tvl_cap: Option<PendingCap>,
    pending_params: Option<PendingParams>,
}

/// Positions of the withdrawal statement's 16 public inputs.
mod input {
    pub const PROTOCOL_VERSION: usize = 0;
    pub const CHAIN_ID: usize = 1;
    pub const CONTRACT_ADDRESS: usize = 2;
    pub const NOTE_LEAF: usize = 3;
    pub const STATE_KEY_X: usize = 4;
    pub const STATE_KEY_Y: usize = 5;
    pub const CLEARANCE_KEY_X: usize = 6;
    pub const CLEARANCE_KEY_Y: usize = 7;
    pub const NOTE_FIELDS: usize = 8;
    pub const FINAL_BALANCE: usize = 9;
    pub const DESTINATION: usize = 10;
    pub const WITHDRAWAL_NULLIFIER: usize = 11;
    pub const KIND: usize = 12;
    pub const NEXT_NULLIFIER: usize = 13;
    pub const CONCEDED: usize = 14;
    pub const VALID_UNTIL: usize = 15;
    pub const COUNT: usize = 16;
}

/// Positions of the request statement's 12 public inputs.
mod request_input {
    pub const PROTOCOL_VERSION: usize = 0;
    pub const CHAIN_ID: usize = 1;
    pub const CONTRACT_ADDRESS: usize = 2;
    pub const STATE_KEY_X: usize = 4;
    pub const STATE_KEY_Y: usize = 5;
    pub const SOLVENCY_BOUND: usize = 7;
    pub const REQUEST_NULLIFIER: usize = 8;
    pub const COUNT: usize = 12;
}

/// The value of a big-endian word that must fit in `bytes` bytes.
fn small(word: &[u8; 32], bytes: usize) -> Option<u128> {
    let (high, low) = word.split_at(32 - bytes);
    if high.iter().any(|b| *b != 0) {
        return None;
    }
    Some(low.iter().fold(0u128, |n, b| n << 8 | u128::from(*b)))
}

/// The deposit message.
#[derive(near_sdk::serde::Deserialize)]
#[serde(crate = "near_sdk::serde", deny_unknown_fields)]
struct DepositMsg {
    commitment: String,
    withdraw_to: Option<AccountId>,
}

/// What the message of an Intents deposit says.
#[derive(Debug, PartialEq, Eq)]
pub enum IntentsMsg {
    /// No valid payout account: the amount cannot be credited to anyone. The commitment is kept
    /// only when it decodes, never the raw message.
    Bad { commitment: Option<[u8; 32]> },
    /// A valid payout account; `commitment` is `None` when it does not decode.
    Valid {
        commitment: Option<[u8; 32]>,
        payout: AccountId,
    },
}

/// Parses an Intents deposit message; total, it never panics. A payout account must be present
/// and may not be the vault, the token or the Intents account.
pub fn intents_msg(msg: &str, vault: &AccountId, token: &AccountId) -> IntentsMsg {
    let Ok(parsed) = near_sdk::serde_json::from_str::<DepositMsg>(msg) else {
        return IntentsMsg::Bad { commitment: None };
    };
    let commitment = word::decode32(&parsed.commitment);
    match parsed.withdraw_to {
        Some(payout)
            if payout != *vault && payout != *token && payout.as_str() != INTENTS_ACCOUNT =>
        {
            IntentsMsg::Valid { commitment, payout }
        }
        _ => IntentsMsg::Bad { commitment },
    }
}

/// For the fuzz target in `fuzz/`.
#[doc(hidden)]
pub mod fuzzing {
    pub fn intents_msg(msg: &str) -> bool {
        let vault = "vault.test".parse().unwrap();
        let token = "usdc.test".parse().unwrap();
        matches!(
            super::intents_msg(msg, &vault, &token),
            super::IntentsMsg::Valid { .. }
        )
    }
}

#[near(serializers = [json])]
pub struct Config {
    pub operator_pk: PublicKey,
    pub clearance_pk: PublicKey,
    pub operator_account: AccountId,
    pub token: AccountId,
    pub treasury: AccountId,
    pub owner: AccountId,
    pub chain_id: U64,
    pub denominations: Vec<U128>,
    pub note_fee: U128,
    pub close_fee: U128,
    pub bound_classes: Vec<U128>,
    pub charge_step: U128,
    pub min_charge: U128,
    pub slot_rate: u32,
    pub tvl_cap: U128,
    pub current_tvl: U128,
    pub exit_delay_secs: u64,
    pub note_ttl: u64,
}

#[near(serializers = [json])]
pub struct NoteRecord {
    pub note_id: u32,
    pub deposit_amount: U128,
    pub expiry: u64,
    pub status: Status,
    pub leaf_retired: bool,
}

/// A pending exit as `exit_requests` lists it.
#[near(serializers = [json])]
pub struct ExitRequest {
    pub note_id: u32,
    pub nullifier: String,
    pub next_nullifier: String,
    pub final_balance: U128,
    pub conceded: U128,
    /// Block time in nanoseconds.
    pub deadline: U64,
}

#[near(serializers = [json])]
pub struct VkHashes {
    pub request: String,
    pub withdrawal: String,
}

#[near]
impl Vault {
    /// Starts the vault. The verifying keys must hash to the compiled pins; the `alt_bn128` form is
    /// derived here from those bytes.
    #[init]
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        operator_pk: PublicKey,
        clearance_pk: PublicKey,
        token: AccountId,
        denominations: Vec<U128>,
        tvl_cap: U128,
        request_vk: Base64VecU8,
        withdrawal_vk: Base64VecU8,
        treasury: AccountId,
        note_fee: U128,
        exit_delay_secs: u64,
        note_ttl: u64,
        bound_classes: Vec<U128>,
        charge_step: U128,
        min_charge: U128,
        slot_rate: u32,
        owner: AccountId,
        operator_account: AccountId,
    ) -> Self {
        let denominations: Vec<u128> = denominations.into_iter().map(|d| d.0).collect();
        require!(!denominations.is_empty(), "no denominations");
        require!(
            denominations.iter().all(|d| *d > 0),
            "a denomination is zero"
        );
        for (i, d) in denominations.iter().enumerate() {
            require!(!denominations[..i].contains(d), "repeated denomination");
        }
        require!(note_fee.0 > 0, "note fee is zero");
        require!(exit_delay_secs >= ONE_DAY_SECS, "exit delay under one day");
        let lease = LeaseParams {
            bound_classes,
            charge_step,
            min_charge,
            slot_rate,
        };
        lease.check();
        let operator_pk = Point::parse(&operator_pk);
        let clearance_pk = Point::parse(&clearance_pk);
        require!(
            operator_pk.x != clearance_pk.x || operator_pk.y != clearance_pk.y,
            "operator and clearance keys are the same"
        );

        // The pin first: a key that is not the compiled one never reaches the parser.
        require!(
            env::keccak256_array(&request_vk.0) == pins::PINS.request,
            "request verifying key does not match the compiled hash"
        );
        require!(
            env::keccak256_array(&withdrawal_vk.0) == pins::PINS.withdrawal,
            "withdrawal verifying key does not match the compiled hash"
        );
        let request_key = VerifyingKey::from_canonical(&request_vk.0)
            .unwrap_or_else(|_| env::panic_str("request verifying key is malformed"));
        let withdrawal_key = VerifyingKey::from_canonical(&withdrawal_vk.0)
            .unwrap_or_else(|_| env::panic_str("withdrawal verifying key is malformed"));

        let empty_root = zero_hashes::ZERO_HASHES[32];
        let mut known_roots = LookupSet::new(StorageKey::KnownRoots);
        known_roots.insert(empty_root);
        Self {
            version: STATE_VERSION,
            operator_pk,
            clearance_pk,
            token,
            treasury,
            owner,
            operator_account,
            denominations,
            note_fee: note_fee.0,
            tvl_cap: tvl_cap.0,
            exit_delay_secs,
            note_ttl,
            bound_classes: lease.bound_classes.into_iter().map(|b| b.0).collect(),
            charge_step: lease.charge_step.0,
            min_charge: lease.min_charge.0,
            slot_rate: lease.slot_rate,
            request_vk: Lazy::new(StorageKey::RequestVk, request_key.into()),
            withdrawal_vk: Lazy::new(StorageKey::WithdrawalVk, withdrawal_key.into()),
            notes: Vector::new(StorageKey::Notes),
            commitments: LookupMap::new(StorageKey::Commitments),
            tvl: 0,
            open_notes: 0,
            treasury_balance: 0,
            leaf_count: 0,
            frontier: [[0; 32]; 32],
            current_root: empty_root,
            retirement_epoch: 0,
            known_roots,
            last_sync_ns: env::block_timestamp(),
            nullifiers: LookupSet::new(StorageKey::Nullifiers),
            pending_payouts: LookupMap::new(StorageKey::PendingPayouts),
            exits: LookupMap::new(StorageKey::Exits),
            exit_ids: Vec::new(),
            flagged: LookupSet::new(StorageKey::Flagged),
            pending_tvl_cap: None,
            pending_params: None,
        }
    }

    /// NEP-141 receiver. `msg` is `{"commitment": "0x..", "withdraw_to": "<account>"?}`. Never
    /// panics on bad input: the whole amount is returned unused. Creates no promise. Deposits from
    /// the Intents account are never returned (a refund to it would get stuck): every outcome
    /// ends in a note, a pending payout, dust or the unclaimed treasury bucket.
    pub fn ft_on_transfer(&mut self, sender_id: AccountId, amount: U128, msg: String) -> U128 {
        if env::predecessor_account_id() != self.token {
            return amount;
        }
        if sender_id.as_str() == INTENTS_ACCOUNT {
            self.intents_deposit(amount.0, &msg);
            return U128(0);
        }
        match self.deposit(sender_id, amount.0, &msg) {
            Some(()) => U128(0),
            None => amount,
        }
    }

    /// Inserts waiting notes' leaves into the tree. Anyone may call it. Does nothing when no leaf
    /// waits, or when under 30 seconds have passed since the last insert and fewer than 16 leaves
    /// wait. Inserts at most `min(max, MAX_SYNC_LEAVES)` leaves and returns how many.
    pub fn sync_root(&mut self, max: u32) -> u32 {
        let waiting = self.notes.len() - self.leaf_count;
        let now = env::block_timestamp();
        if waiting == 0
            || (waiting < SYNC_BATCH && now.saturating_sub(self.last_sync_ns) < SYNC_WAIT_NS)
        {
            return 0;
        }
        let count = waiting.min(max).min(MAX_SYNC_LEAVES);
        if count == 0 {
            return 0;
        }
        let first = self.leaf_count;
        // The leaf left of the first insert; index 0 has none.
        let mut previous = match first {
            0 => [0; 32],
            _ => self.notes.get(first - 1).expect("note exists").tree_leaf(),
        };
        for index in first..first + count {
            let note = self.notes.get(index).expect("note exists");
            if note.status == Status::Closed && !note.leaf_retired {
                self.notes.get_mut(index).expect("note exists").leaf_retired = true;
            }
            let leaf = self.notes.get(index).expect("note exists").tree_leaf();
            tree::append(&mut self.frontier, index, &previous, &leaf);
            previous = leaf;
        }
        self.leaf_count = first + count;
        self.current_root = tree::root(&self.frontier, self.leaf_count, &previous);
        self.known_roots.insert(self.current_root);
        self.last_sync_ns = now;
        events::emit(
            "sync_root",
            near_sdk::serde_json::json!({
                "first_index": first,
                "last_index": first + count - 1,
                "root": word::encode32(&self.current_root),
            }),
        );
        count
    }

    /// Closes a note with the operator's clearance: `proof` is the 256-byte wire proof of a kind-1
    /// withdrawal and `inputs` its 16 public input words (32 bytes each, big-endian). Pays the
    /// stored payout account `final_balance` (less the close fee when the operator submits it).
    pub fn mutual_close(
        &mut self,
        proof: Base64VecU8,
        inputs: Vec<Base64VecU8>,
    ) -> PromiseOrValue<()> {
        require!(
            env::prepaid_gas() >= MIN_GAS_MUTUAL_CLOSE,
            "not enough gas for the close and its payout"
        );
        let (words, scalars) = parse_inputs(&inputs, input::COUNT);
        let word = |index: usize| &words[index];
        require!(small(word(input::KIND), 1) == Some(1), "not a mutual close");
        let (note_id, final_balance, _) = self.check_withdrawal(&words);
        let nullifier = *word(input::WITHDRAWAL_NULLIFIER);
        require!(
            !self.nullifiers.contains(&nullifier),
            "nullifier already used"
        );
        require!(!self.flagged.contains(&nullifier), "nullifier is flagged");
        verify_proof(&VerifyingKey::from(&*self.withdrawal_vk), &scalars, &proof);

        // Effects before the promise.
        let deposit = self.notes.get(note_id).expect("note exists").deposit_amount;
        self.nullifiers.insert(nullifier);
        self.notes.get_mut(note_id).expect("note exists").status = Status::Closed;
        // `note_fee` is fixed at `new()`, so this is what the deposit added to `tvl`.
        self.tvl -= deposit + self.note_fee;
        self.open_notes -= 1;
        let close_fee = if env::predecessor_account_id() == self.operator_account {
            CLOSE_FEE.min(final_balance)
        } else {
            0
        };
        let payout = final_balance - close_fee;
        self.treasury_balance += deposit - final_balance + close_fee;
        events::emit(
            "mutual_close",
            json!({
                "note_id": note_id,
                "amount_paid": payout.to_string(),
                "close_fee": close_fee.to_string(),
            }),
        );
        self.pay_closed_note(note_id, payout)
    }

    /// Files an exit with a kind-0 withdrawal proof for the latest state the caller holds, without
    /// the operator's clearance: `proof` is the 256-byte wire proof and `inputs` its 16 public
    /// input words. Spends N_k and its successor, parks the note in `PendingExit` and records what
    /// `finalize_exit` pays once the deadline has passed. Makes no USDC call.
    pub fn request_exit(&mut self, proof: Base64VecU8, inputs: Vec<Base64VecU8>) {
        let (words, scalars) = parse_inputs(&inputs, input::COUNT);
        let word = |index: usize| &words[index];
        require!(
            small(word(input::KIND), 1) == Some(0),
            "not an exit request"
        );
        let (note_id, final_balance, deposit) = self.check_withdrawal(&words);
        let conceded = small(word(input::CONCEDED), 16)
            .unwrap_or_else(|| env::panic_str("concession out of range"));
        require!(
            final_balance
                .checked_add(conceded)
                .is_some_and(|b| b <= deposit),
            "balance and concession above the deposit"
        );
        let nullifier = *word(input::WITHDRAWAL_NULLIFIER);
        let next_nullifier = *word(input::NEXT_NULLIFIER);
        require!(next_nullifier != nullifier, "successor nullifier repeats");
        for n in [&nullifier, &next_nullifier] {
            require!(!self.nullifiers.contains(n), "nullifier already used");
            require!(!self.flagged.contains(n), "nullifier is flagged");
        }
        verify_proof(&VerifyingKey::from(&*self.withdrawal_vk), &scalars, &proof);

        let now = env::block_timestamp();
        let deadline = now + self.exit_delay_secs * 1_000_000_000;
        self.nullifiers.insert(nullifier);
        self.nullifiers.insert(next_nullifier);
        let note = self.notes.get_mut(note_id).expect("note exists");
        note.status = Status::PendingExit;
        note.last_exit_ns = now;
        self.open_notes -= 1;
        self.add_exit(
            note_id,
            ExitRecord {
                nullifier,
                next_nullifier,
                final_balance,
                conceded,
                deadline,
            },
        );
        events::emit(
            "request_exit",
            json!({
                "note_id": note_id,
                "nullifier": word::encode32(&nullifier),
                "next_nullifier": word::encode32(&next_nullifier),
                "final_balance": final_balance.to_string(),
                "conceded": conceded.to_string(),
                "deadline": deadline.to_string(),
            }),
        );
    }

    /// Ends a pending exit before its deadline with a request proof at the successor nullifier:
    /// the user spent past the escaped state. Anyone may call it. Frees both nullifiers, flags N_k
    /// for good and returns the note to `Active`.
    pub fn defeat_exit(&mut self, note_id: u32, proof: Base64VecU8, inputs: Vec<Base64VecU8>) {
        let record = self.open_exit(note_id);
        self.check_request_proof(
            &proof,
            &inputs,
            &record.next_nullifier,
            "not a request at the successor nullifier",
        );
        self.remove_exit(note_id);
        self.nullifiers.remove(&record.nullifier);
        self.nullifiers.remove(&record.next_nullifier);
        self.flagged.insert(record.nullifier);
        let note = self.notes.get_mut(note_id).expect("note exists");
        note.status = Status::Active;
        note.last_exit_ns = env::block_timestamp();
        self.open_notes += 1;
        events::emit(
            "defeat_exit",
            json!({ "note_id": note_id, "flagged_nullifier": word::encode32(&record.nullifier) }),
        );
    }

    /// Lowers a pending exit's payout before its deadline with a request proof at the escaped
    /// nullifier: a lease was in flight. Anyone may call it. The concession becomes
    /// `min(amount, solvency bound, the escaped state's balance)` and must grow. The deadline stays.
    pub fn raise_concession(
        &mut self,
        note_id: u32,
        proof: Base64VecU8,
        inputs: Vec<Base64VecU8>,
        amount: U128,
    ) {
        let record = self.open_exit(note_id);
        let bound = self.check_request_proof(
            &proof,
            &inputs,
            &record.nullifier,
            "not a request at the escaped nullifier",
        );
        let balance = record.final_balance + record.conceded;
        let conceded = amount.0.min(bound).min(balance);
        require!(conceded > record.conceded, "the concession does not grow");
        let record = self.exits.get_mut(&note_id).expect("exit exists");
        record.conceded = conceded;
        record.final_balance = balance - conceded;
        events::emit(
            "raise_concession",
            json!({ "note_id": note_id, "conceded": conceded.to_string() }),
        );
    }

    /// Pays a pending exit's record once its deadline has passed, with no proof. Anyone may call
    /// it. The payout account gets `final_balance`; the rest of the deposit goes to the treasury.
    pub fn finalize_exit(&mut self, note_id: u32) -> PromiseOrValue<()> {
        require!(
            env::prepaid_gas() >= MIN_GAS_CLAIM,
            "not enough gas for the payout"
        );
        let record = self.pending_exit(note_id);
        require!(
            env::block_timestamp() >= record.deadline,
            "the exit window is still open"
        );
        let deposit = self.notes.get(note_id).expect("note exists").deposit_amount;
        let payout = record.final_balance;
        self.remove_exit(note_id);
        self.notes.get_mut(note_id).expect("note exists").status = Status::Closed;
        self.tvl -= deposit + self.note_fee;
        self.treasury_balance += deposit - payout;
        events::emit(
            "finalize_exit",
            json!({ "note_id": note_id, "amount_paid": payout.to_string() }),
        );
        self.pay_closed_note(note_id, payout)
    }

    /// Pending exits with note ID at or above `from_note_id`, at most `limit` (capped at 100), in
    /// note ID order: the next page starts at the last ID plus one.
    pub fn exit_requests(&self, from_note_id: u32, limit: u32) -> Vec<ExitRequest> {
        let first = self.exit_ids.partition_point(|id| *id < from_note_id);
        self.exit_ids[first..]
            .iter()
            .take(limit.min(MAX_PAGE) as usize)
            .map(|note_id| (note_id, self.exits.get(note_id).expect("exit exists")))
            .map(|(note_id, record)| ExitRequest {
                note_id: *note_id,
                nullifier: word::encode32(&record.nullifier),
                next_nullifier: word::encode32(&record.next_nullifier),
                final_balance: U128(record.final_balance),
                conceded: U128(record.conceded),
                deadline: U64(record.deadline),
            })
            .collect()
    }

    /// Sweeps an `Active` note to the treasury once `max(expiry, last exit activity)` plus the
    /// exit delay has passed. Anyone may call it. Books the denomination to the treasury balance
    /// and closes the note; no USDC call.
    pub fn claim_expired(&mut self, note_id: u32) {
        let note = self
            .notes
            .get(note_id)
            .unwrap_or_else(|| env::panic_str("unknown note"));
        require!(note.status == Status::Active, "note is not active");
        require!(self.claimable(note), "the note cannot be claimed yet");
        self.sweep(&[note_id]);
    }

    /// `claim_expired` for notes `from..from + limit` (limit capped at 50): notes that are not
    /// claimable yet are skipped. Returns the swept note IDs; a call that sweeps none emits nothing.
    pub fn claim_expired_batch(&mut self, from: u32, limit: u32) -> Vec<u32> {
        let end = from
            .saturating_add(limit.min(MAX_CLAIM_BATCH))
            .min(self.notes.len());
        let ids: Vec<u32> = (from..end)
            .filter(|id| {
                let note = self.notes.get(*id).expect("note exists");
                note.status == Status::Active && self.claimable(note)
            })
            .collect();
        if !ids.is_empty() {
            self.sweep(&ids);
        }
        ids
    }

    /// Zeroes the leaf of a closed note that `sync_root` has already inserted, so no root can open
    /// a lease for it. Anyone may call it. `siblings` are the leaf's path siblings bottom up as
    /// `0x` words, only as many as the frontier slot holding the leaf is high (the highest bit
    /// where `note_id` and `leaf_count` differ; none when the note is the last inserted one). They
    /// must rebuild that slot from the stored leaf, and the rewritten slot gives the new root.
    pub fn retire_closed_leaf(&mut self, note_id: u32, siblings: Vec<String>) {
        let note = self
            .notes
            .get(note_id)
            .unwrap_or_else(|| env::panic_str("unknown note"));
        require!(note.status == Status::Closed, "note is not closed");
        require!(!note.leaf_retired, "leaf already retired");
        require!(note_id < self.leaf_count, "leaf is not in the tree yet");
        let leaf = note.leaf;
        let level = tree::retire_level(note_id, self.leaf_count);
        require!(siblings.len() == level, "wrong number of siblings");
        if level > 0 {
            let siblings: Vec<[u8; 32]> = siblings
                .iter()
                .map(|s| {
                    word::decode32(s)
                        .filter(|w| *w < SCALAR_MODULUS_BE)
                        .unwrap_or_else(|| env::panic_str("bad sibling"))
                })
                .collect();
            let (before, after) = tree::path_nodes(note_id, &leaf, &siblings);
            require!(
                before == self.frontier[level],
                "siblings do not match the tree"
            );
            self.frontier[level] = after;
        }
        self.notes
            .get_mut(note_id)
            .expect("note exists")
            .leaf_retired = true;
        let last = self
            .notes
            .get(self.leaf_count - 1)
            .expect("note exists")
            .tree_leaf();
        self.current_root = tree::root(&self.frontier, self.leaf_count, &last);
        self.known_roots.insert(self.current_root);
        self.retirement_epoch += 1;
        events::emit("retire_closed_leaf", json!({ "note_id": note_id }));
    }

    /// Pays the pending payout of `account_id`, to that account only. Anyone may call it.
    pub fn claim_payout(&mut self, account_id: AccountId) -> Promise {
        require!(
            env::prepaid_gas() >= MIN_GAS_CLAIM,
            "not enough gas for the payout"
        );
        let amount = self
            .pending_payouts
            .remove(&account_id)
            .unwrap_or_else(|| env::panic_str("nothing pending for this account"));
        self.payout(&account_id, amount)
            .then(Self::callback().on_claim_payout(account_id, U128(amount)))
    }

    /// Pays the whole treasury balance to the treasury account, to that account only. Anyone may
    /// call it.
    pub fn claim_treasury(&mut self) -> Promise {
        require!(
            env::prepaid_gas() >= MIN_GAS_CLAIM,
            "not enough gas for the payout"
        );
        let amount = std::mem::take(&mut self.treasury_balance);
        require!(amount > 0, "the treasury balance is empty");
        let treasury = self.treasury.clone();
        self.payout(&treasury, amount)
            .then(Self::callback().on_claim_treasury(U128(amount)))
    }

    /// A close payout finished. Only the promise status counts: the returned bytes are never read.
    #[private]
    pub fn on_close_payout(
        &mut self,
        account: AccountId,
        amount: U128,
        note_id: u32,
        commitment: String,
    ) {
        if payout_failed() {
            self.credit_pending(
                account,
                amount.0,
                "transfer_failed",
                Some((note_id, commitment)),
            );
        }
    }

    #[private]
    pub fn on_claim_payout(&mut self, account: AccountId, amount: U128) {
        if payout_failed() {
            // Restored whatever its size: the record may have been re-created meanwhile.
            let current = self.pending_payouts.get(&account).copied().unwrap_or(0);
            self.pending_payouts.insert(account, current + amount.0);
        } else {
            events::emit(
                "payout_claimed",
                json!({ "account": account, "amount": amount.0.to_string() }),
            );
        }
    }

    #[private]
    pub fn on_claim_treasury(&mut self, amount: U128) {
        if payout_failed() {
            self.treasury_balance += amount.0;
        } else {
            events::emit(
                "treasury_claimed",
                json!({ "amount": amount.0.to_string() }),
            );
        }
    }

    /// What `account_id` can collect with `claim_payout`.
    pub fn pending_payout(&self, account_id: AccountId) -> U128 {
        U128(self.pending_payouts.get(&account_id).copied().unwrap_or(0))
    }

    /// Whether the nullifier (`0x` and 64 hex digits) has been spent.
    pub fn is_nullifier_used(&self, nullifier: String) -> bool {
        word::decode32(&nullifier).is_some_and(|n| self.nullifiers.contains(&n))
    }

    pub fn current_root(&self) -> String {
        word::encode32(&self.current_root)
    }

    pub fn is_known_root(&self, root: String) -> bool {
        word::decode32(&root).is_some_and(|root| self.known_roots.contains(&root))
    }

    /// Leaves `from..from + limit` (limit capped at 100) of the inserted tree, zero once retired.
    pub fn get_leaves(&self, from: u32, limit: u32) -> Vec<String> {
        let end = from
            .saturating_add(limit.min(MAX_PAGE))
            .min(self.leaf_count);
        (from..end)
            .map(|id| word::encode32(&self.notes.get(id).expect("note exists").tree_leaf()))
            .collect()
    }

    /// How many leaves `retire_closed_leaf` has zeroed in the inserted tree.
    pub fn retirement_epoch(&self) -> U64 {
        U64(self.retirement_epoch)
    }

    pub fn leaf_count(&self) -> u32 {
        self.leaf_count
    }

    /// Notes with status `Active`, expired or not.
    pub fn open_notes(&self) -> u32 {
        self.open_notes
    }

    /// The status of the note with this commitment.
    pub fn note_status(&self, commitment: String) -> Option<Status> {
        let commitment =
            word::decode32(&commitment).unwrap_or_else(|| env::panic_str("bad commitment"));
        let id = self.commitments.get(&commitment)?;
        Some(self.notes.get(*id).expect("note exists").status)
    }

    /// Public fields of notes `from..from + limit` (limit capped at 100): nothing else.
    pub fn note_records(&self, from: u32, limit: u32) -> Vec<NoteRecord> {
        let end = from
            .saturating_add(limit.min(MAX_PAGE))
            .min(self.notes.len());
        (from..end)
            .map(|note_id| {
                let note = self.notes.get(note_id).expect("note exists");
                NoteRecord {
                    note_id,
                    deposit_amount: U128(note.deposit_amount),
                    expiry: note.expiry,
                    status: note.status,
                    leaf_retired: note.leaf_retired,
                }
            })
            .collect()
    }

    /// NEAR the vault can spend: balance minus the storage it must keep covered.
    pub fn liquid_near(&self) -> U128 {
        let locked = u128::from(env::storage_usage()) * env::storage_byte_cost().as_yoctonear();
        U128(env::account_balance().as_yoctonear().saturating_sub(locked))
    }

    pub fn treasury_balance(&self) -> U128 {
        U128(self.treasury_balance)
    }

    /// The keccak256 hashes of the two verifying keys this build accepts.
    pub fn vk_hashes(&self) -> VkHashes {
        VkHashes {
            request: word::encode32(&pins::PINS.request),
            withdrawal: word::encode32(&pins::PINS.withdrawal),
        }
    }

    /// Proposes a new TVL cap; it applies 48 hours later. Vault owner only, with one yoctoNEAR.
    /// A second proposal replaces the first and restarts the 48 hours.
    #[payable]
    pub fn propose_tvl_cap(&mut self, tvl_cap: U128) {
        self.owner_call();
        require!(tvl_cap.0 > 0, "tvl cap is zero");
        let now = env::block_timestamp();
        self.pending_tvl_cap = Some(PendingCap {
            value: tvl_cap.0,
            proposed_at: now,
        });
        events::emit(
            "tvl_cap_proposed",
            json!({
                "tvl_cap": tvl_cap.0.to_string(),
                "effective_at": (now + PROPOSAL_DELAY_NS).to_string(),
            }),
        );
    }

    /// Applies the pending TVL cap once 48 hours have passed and before it lapses (7 days after
    /// it became applicable). Vault owner only, with one yoctoNEAR.
    #[payable]
    pub fn apply_tvl_cap(&mut self) {
        self.owner_call();
        let pending = self
            .pending_tvl_cap
            .as_ref()
            .unwrap_or_else(|| env::panic_str("no pending proposal"));
        let effective_at = pending.proposed_at + PROPOSAL_DELAY_NS;
        check_applicable(effective_at);
        let cap = pending.value;
        self.pending_tvl_cap = None;
        self.tvl_cap = cap;
        events::emit(
            "tvl_cap_applied",
            json!({ "tvl_cap": cap.to_string(), "effective_at": effective_at.to_string() }),
        );
    }

    /// Proposes new lease parameters (bound classes, charge step, minimum charge, slot rate); they
    /// apply 48 hours later. Vault owner only, with one yoctoNEAR.
    #[payable]
    pub fn propose_params(&mut self, params: LeaseParams) {
        self.owner_call();
        params.check();
        let now = env::block_timestamp();
        events::emit(
            "params_proposed",
            json!({
                "params": params,
                "effective_at": (now + PROPOSAL_DELAY_NS).to_string(),
            }),
        );
        self.pending_params = Some(PendingParams {
            value: params,
            proposed_at: now,
        });
    }

    /// Applies the pending lease parameters, under the same window as `apply_tvl_cap`.
    #[payable]
    pub fn apply_params(&mut self) {
        self.owner_call();
        let pending = self
            .pending_params
            .as_ref()
            .unwrap_or_else(|| env::panic_str("no pending proposal"));
        let effective_at = pending.proposed_at + PROPOSAL_DELAY_NS;
        check_applicable(effective_at);
        let params = self.pending_params.take().expect("checked above").value;
        self.bound_classes = params.bound_classes.iter().map(|b| b.0).collect();
        self.charge_step = params.charge_step.0;
        self.min_charge = params.min_charge.0;
        self.slot_rate = params.slot_rate;
        events::emit(
            "params_applied",
            json!({ "params": params, "effective_at": effective_at.to_string() }),
        );
    }

    /// Proof of life: emits the block time, only for the operator account.
    pub fn heartbeat(&mut self) {
        require!(
            env::predecessor_account_id() == self.operator_account,
            "only the operator account"
        );
        events::emit(
            "heartbeat",
            json!({ "ts": env::block_timestamp().to_string() }),
        );
    }

    /// The account that takes the close fee and sends heartbeats; fixed at `new()`.
    pub fn operator_account(&self) -> AccountId {
        self.operator_account.clone()
    }

    pub fn bound_classes(&self) -> Vec<U128> {
        self.bound_classes.iter().copied().map(U128).collect()
    }

    pub fn charge_step(&self) -> U128 {
        U128(self.charge_step)
    }

    pub fn min_charge(&self) -> U128 {
        U128(self.min_charge)
    }

    pub fn slot_rate(&self) -> u32 {
        self.slot_rate
    }

    pub fn config(&self) -> Config {
        Config {
            operator_pk: self.operator_pk.json(),
            clearance_pk: self.clearance_pk.json(),
            operator_account: self.operator_account.clone(),
            token: self.token.clone(),
            treasury: self.treasury.clone(),
            owner: self.owner.clone(),
            chain_id: U64(chain_id(NETWORK)),
            denominations: self.denominations.iter().copied().map(U128).collect(),
            note_fee: U128(self.note_fee),
            close_fee: U128(CLOSE_FEE),
            bound_classes: self.bound_classes.iter().copied().map(U128).collect(),
            charge_step: U128(self.charge_step),
            min_charge: U128(self.min_charge),
            slot_rate: self.slot_rate,
            tvl_cap: U128(self.tvl_cap),
            current_tvl: U128(self.tvl),
            exit_delay_secs: self.exit_delay_secs,
            note_ttl: self.note_ttl,
        }
    }
}

impl Vault {
    /// The checks every vault owner method starts with: one yoctoNEAR (so only a full-access key
    /// can call) and the owner as caller.
    fn owner_call(&self) {
        assert_one_yocto();
        require!(
            env::predecessor_account_id() == self.owner,
            "only the vault owner"
        );
    }

    /// The vault's own callback, with its fixed gas and no share of leftover gas.
    fn callback() -> VaultExt {
        Self::ext(env::current_account_id())
            .with_static_gas(GAS_CALLBACK)
            .with_unused_gas_weight(0)
    }

    /// The checks a kind-0 and a kind-1 withdrawal proof share, before the nullifiers and the
    /// pairing: protocol version, chain, contract, both keys, the note (`Active`, its leaf and
    /// fields), the balance, the destination and `valid_until`. Returns the note ID, the proof's
    /// `final_balance` and the note's deposit.
    fn check_withdrawal(&self, words: &[[u8; 32]]) -> (u32, u128, u128) {
        let word = |index: usize| &words[index];
        require!(
            small(word(input::PROTOCOL_VERSION), 2) == Some(u128::from(PROTOCOL_VERSION)),
            "wrong protocol version"
        );
        require!(
            small(word(input::CHAIN_ID), 8) == Some(u128::from(chain_id(NETWORK))),
            "wrong chain id"
        );
        require!(
            *word(input::CONTRACT_ADDRESS) == contract_address(env::current_account_id().as_str()),
            "wrong contract address"
        );
        require!(
            *word(input::STATE_KEY_X) == self.operator_pk.x
                && *word(input::STATE_KEY_Y) == self.operator_pk.y,
            "wrong state key"
        );
        require!(
            *word(input::CLEARANCE_KEY_X) == self.clearance_pk.x
                && *word(input::CLEARANCE_KEY_Y) == self.clearance_pk.y,
            "wrong clearance key"
        );
        let (note_id, deposit_amount, expiry) =
            zkapi_core::v2::unpack_note_fields(&Felt252(*word(input::NOTE_FIELDS)))
                .unwrap_or_else(|| env::panic_str("invalid noteFields"));
        let note = self
            .notes
            .get(note_id)
            .unwrap_or_else(|| env::panic_str("unknown note"));
        require!(note.status == Status::Active, "note is not active");
        require!(*word(input::NOTE_LEAF) == note.leaf, "wrong note leaf");
        require!(
            deposit_amount == note.deposit_amount && expiry == note.expiry,
            "note fields do not match the stored note"
        );
        let final_balance = small(word(input::FINAL_BALANCE), 16)
            .unwrap_or_else(|| env::panic_str("final balance out of range"));
        require!(
            final_balance <= note.deposit_amount,
            "final balance above the deposit"
        );
        require!(
            word(input::DESTINATION)[..12].iter().all(|b| *b == 0)
                && word(input::DESTINATION)[12..] == destination(note.withdraw_to.as_str()),
            "destination is not the payout account"
        );
        let valid_until = small(word(input::VALID_UNTIL), 8)
            .unwrap_or_else(|| env::panic_str("valid_until out of range"));
        require!(
            u128::from(env::block_timestamp() / 1_000_000_000) <= valid_until,
            "proof expired"
        );
        (note_id, final_balance, note.deposit_amount)
    }

    /// Whether the exit window after the note's expiry and after its last exit activity is over.
    fn claimable(&self, note: &Note) -> bool {
        let ready = note
            .expiry
            .saturating_mul(NS_PER_SEC)
            .max(note.last_exit_ns)
            .saturating_add(self.exit_delay_secs.saturating_mul(NS_PER_SEC));
        env::block_timestamp() >= ready
    }

    /// Closes `Active` notes and books their denominations to the treasury, with one event.
    fn sweep(&mut self, ids: &[u32]) {
        let note_fee = self.note_fee;
        for id in ids {
            let note = self.notes.get_mut(*id).expect("note exists");
            note.status = Status::Closed;
            let deposit = note.deposit_amount;
            // `note_fee` is fixed at `new()`, so this is what the deposit added to `tvl`.
            self.tvl -= deposit + note_fee;
            self.open_notes -= 1;
            self.treasury_balance += deposit;
        }
        events::emit("claim_expired", json!({ "note_ids": ids }));
    }

    fn add_exit(&mut self, note_id: u32, record: ExitRecord) {
        let at = self
            .exit_ids
            .binary_search(&note_id)
            .expect_err("one exit per note");
        self.exit_ids.insert(at, note_id);
        self.exits.insert(note_id, record);
    }

    fn remove_exit(&mut self, note_id: u32) {
        let at = self.exit_ids.binary_search(&note_id).expect("exit exists");
        self.exit_ids.remove(at);
        self.exits.remove(&note_id);
    }

    /// The exit record of a note in `PendingExit`.
    fn pending_exit(&self, note_id: u32) -> ExitRecord {
        let note = self
            .notes
            .get(note_id)
            .unwrap_or_else(|| env::panic_str("unknown note"));
        require!(note.status == Status::PendingExit, "no pending exit");
        let record = self
            .exits
            .get(&note_id)
            .unwrap_or_else(|| env::panic_str("no pending exit"));
        ExitRecord { ..*record }
    }

    /// The exit record of a note whose exit window is still open.
    fn open_exit(&self, note_id: u32) -> ExitRecord {
        let record = self.pending_exit(note_id);
        require!(
            env::block_timestamp() < record.deadline,
            "the exit window is over"
        );
        record
    }

    /// Checks a request proof against a pending exit: protocol version, chain, contract, the state
    /// key (not the clearance key, which request proofs lack), the nullifier it must be at, then
    /// the pairing. No root, request time or tag check: old evidence stays usable. Returns the
    /// proof's solvency bound.
    fn check_request_proof(
        &self,
        proof: &Base64VecU8,
        inputs: &[Base64VecU8],
        expected_nullifier: &[u8; 32],
        mismatch: &str,
    ) -> u128 {
        let (words, scalars) = parse_inputs(inputs, request_input::COUNT);
        let word = |index: usize| &words[index];
        require!(
            small(word(request_input::PROTOCOL_VERSION), 2) == Some(u128::from(PROTOCOL_VERSION)),
            "wrong protocol version"
        );
        require!(
            small(word(request_input::CHAIN_ID), 8) == Some(u128::from(chain_id(NETWORK))),
            "wrong chain id"
        );
        require!(
            *word(request_input::CONTRACT_ADDRESS)
                == contract_address(env::current_account_id().as_str()),
            "wrong contract address"
        );
        require!(
            *word(request_input::STATE_KEY_X) == self.operator_pk.x
                && *word(request_input::STATE_KEY_Y) == self.operator_pk.y,
            "wrong state key"
        );
        require!(
            word(request_input::REQUEST_NULLIFIER) == expected_nullifier,
            mismatch
        );
        let bound = small(word(request_input::SOLVENCY_BOUND), 16)
            .unwrap_or_else(|| env::panic_str("solvency bound out of range"));
        verify_proof(&VerifyingKey::from(&*self.request_vk), &scalars, proof);
        bound
    }

    /// Pays the closed note's payout account `payout` (nothing when it is 0).
    fn pay_closed_note(&mut self, note_id: u32, payout: u128) -> PromiseOrValue<()> {
        if payout == 0 {
            return PromiseOrValue::Value(());
        }
        let note = self.notes.get(note_id).expect("note exists");
        let account = note.withdraw_to.clone();
        let commitment = word::encode32(&note.commitment);
        PromiseOrValue::Promise(
            self.payout(&account, payout)
                .then(Self::callback().on_close_payout(account, U128(payout), note_id, commitment)),
        )
    }

    /// Registers `to` with USDC (registration only, the NEAR comes back when it is already
    /// registered), then transfers `amount` in a second receipt. Each leg has fixed gas.
    fn payout(&self, to: &AccountId, amount: u128) -> Promise {
        let register = Promise::new(self.token.clone()).function_call(
            "storage_deposit".to_owned(),
            json!({ "account_id": to, "registration_only": true })
                .to_string()
                .into_bytes(),
            NearToken::from_yoctonear(REGISTRATION_DEPOSIT),
            GAS_STORAGE_DEPOSIT,
        );
        let transfer = Promise::new(self.token.clone()).function_call(
            "ft_transfer".to_owned(),
            json!({ "receiver_id": to, "amount": amount.to_string() })
                .to_string()
                .into_bytes(),
            NearToken::from_yoctonear(1),
            GAS_FT_TRANSFER,
        );
        register.then(transfer)
    }

    /// An amount owed to `account`: added to its record, or a new record when it is at least the
    /// note fee, otherwise dust for the treasury.
    fn credit_pending(
        &mut self,
        account: AccountId,
        amount: u128,
        reason: &str,
        note: Option<(u32, String)>,
    ) {
        let (note_id, commitment) = note.unzip();
        let note_fee = self.note_fee;
        let existing = self.pending_payouts.get(&account).copied();
        if existing.is_none() && amount < note_fee {
            self.treasury_balance += amount;
            events::emit(
                "dust",
                json!({
                    "amount": amount.to_string(),
                    "reason": "below_note_fee",
                    "note_id": note_id,
                    "commitment": commitment,
                }),
            );
            return;
        }
        self.pending_payouts
            .insert(account.clone(), existing.unwrap_or(0) + amount);
        events::emit(
            "payout_pending",
            json!({
                "account": account,
                "amount": amount.to_string(),
                "reason": reason,
                "note_id": note_id,
                "commitment": commitment,
            }),
        );
    }

    /// A valid deposit stores a note and emits `deposit`; any refusal returns `None` and changes
    /// nothing.
    fn deposit(&mut self, sender_id: AccountId, amount: u128, msg: &str) -> Option<()> {
        let note_fee = self.note_fee;
        let denomination = self
            .denominations
            .iter()
            .copied()
            .find(|d| d.checked_add(note_fee) == Some(amount))?;
        let msg: DepositMsg = near_sdk::serde_json::from_str(msg).ok()?;
        let commitment = word::decode32(&msg.commitment)?;
        let payout = msg.withdraw_to.unwrap_or(sender_id);
        self.open_note(denomination, commitment, payout).map(|_| ())
    }

    /// The Intents branch: parses into a result and writes a record on every path, never
    /// panicking. The note takes the larger denomination whose price fits `amount`; the excess
    /// and any refused amount become a pending payout for the payout account (dust below the note
    /// fee); an unparseable message or an invalid payout account goes to the unclaimed bucket.
    fn intents_deposit(&mut self, amount: u128, msg: &str) {
        let (commitment, payout) = match intents_msg(msg, &env::current_account_id(), &self.token) {
            IntentsMsg::Bad { commitment } => {
                self.treasury_balance += amount;
                events::emit(
                    "unclaimed",
                    json!({
                        "amount": amount.to_string(),
                        "reason": "intents_bad_msg",
                        "commitment": commitment.map(|c| word::encode32(&c)),
                    }),
                );
                return;
            }
            IntentsMsg::Valid { commitment, payout } => (commitment, payout),
        };
        let note_fee = self.note_fee;
        let denomination = self
            .denominations
            .iter()
            .copied()
            .filter(|d| d.checked_add(note_fee).is_some_and(|price| price <= amount))
            .max();
        let opened = denomination
            .zip(commitment)
            .and_then(|(d, c)| self.open_note(d, c, payout.clone()).map(|id| (d, c, id)));
        match opened {
            Some((denomination, commitment, note_id)) => {
                let excess = amount - (denomination + note_fee);
                if excess > 0 {
                    let note = Some((note_id, word::encode32(&commitment)));
                    self.credit_pending(payout, excess, "intents_excess", note);
                }
            }
            // Short amount, repeated or malformed commitment, full cap, low NEAR: all of it is
            // owed to the payout account.
            None => self.credit_pending(payout, amount, "intents_excess", None),
        }
    }

    /// Stores a note for `denomination` and emits `deposit`, returning its ID; any refusal
    /// returns `None` and changes nothing. The caller has matched the amount to the denomination.
    fn open_note(
        &mut self,
        denomination: u128,
        commitment: [u8; 32],
        payout: AccountId,
    ) -> Option<u32> {
        let price = denomination.checked_add(self.note_fee)?;
        if commitment >= SCALAR_MODULUS_BE || self.commitments.contains_key(&commitment) {
            return None;
        }
        if payout == env::current_account_id()
            || payout == self.token
            || payout.as_str() == INTENTS_ACCOUNT
        {
            return None;
        }
        if self.tvl.checked_add(price)? > self.tvl_cap {
            return None;
        }
        if self.liquid_near().0 < RESERVE {
            return None;
        }
        let note_id = self.notes.len();
        if note_id == u32::MAX {
            return None;
        }

        let now_secs = env::block_timestamp() / 1_000_000_000;
        let expiry = end_of_utc_day(now_secs.checked_add(self.note_ttl)?)?;
        let leaf = zkapi_core::v2::note_leaf(note_id, &Felt252(commitment), denomination, expiry).0;
        self.notes.push(Note {
            commitment,
            deposit_amount: denomination,
            expiry,
            status: Status::Active,
            withdraw_to: payout.clone(),
            leaf,
            leaf_retired: false,
            last_exit_ns: 0,
        });
        self.commitments.insert(commitment, note_id);
        self.tvl += price;
        self.open_notes += 1;
        self.treasury_balance += self.note_fee;
        events::emit(
            "deposit",
            near_sdk::serde_json::json!({
                "note_id": note_id,
                "commitment": word::encode32(&commitment),
                "denomination": denomination.to_string(),
                "expiry": expiry,
                "withdraw_to": payout,
            }),
        );
        Some(note_id)
    }
}

/// Parses `count` public input words: the big-endian words, and the little-endian scalars the
/// verifier takes. Refuses a wrong count or length and any word at or above the field modulus.
fn parse_inputs(inputs: &[Base64VecU8], count: usize) -> (Vec<[u8; 32]>, Vec<[u8; 32]>) {
    require!(inputs.len() == count, "wrong number of public inputs");
    let mut words = Vec::with_capacity(count);
    let mut scalars = Vec::with_capacity(count);
    for item in inputs {
        let scalar = public_input_from_wire(&item.0)
            .unwrap_or_else(|_| env::panic_str("public input is not a canonical field element"));
        words.push(<[u8; 32]>::try_from(item.0.as_slice()).expect("32 bytes"));
        scalars.push(scalar);
    }
    (words, scalars)
}

/// A proposal is applicable from `effective_at` and lapses 7 days later.
fn check_applicable(effective_at: u64) {
    let now = env::block_timestamp();
    require!(now >= effective_at, "the 48-hour wait is not over");
    require!(
        now <= effective_at.saturating_add(PROPOSAL_LAPSE_NS),
        "the proposal lapsed"
    );
}

/// Panics unless the wire proof verifies against `key` for the scalars.
fn verify_proof(key: &VerifyingKey, scalars: &[[u8; 32]], proof: &Base64VecU8) {
    let proof =
        Proof::from_zkapi_wire(&proof.0).unwrap_or_else(|_| env::panic_str("malformed proof"));
    require!(
        nymira_verifier::verify(key, scalars, &proof) == Ok(true),
        "invalid proof"
    );
}

/// A payout failed only when the promise it follows failed. The returned bytes are never read
/// (a token may return anything), so this asks for none.
fn payout_failed() -> bool {
    matches!(env::promise_result_checked(0, 0), Err(PromiseError::Failed))
}

/// The last second of the UTC day that contains `unix_seconds`.
fn end_of_utc_day(unix_seconds: u64) -> Option<u64> {
    Some(
        (unix_seconds / ONE_DAY_SECS)
            .checked_add(1)?
            .checked_mul(ONE_DAY_SECS)?
            - 1,
    )
}

#[cfg(test)]
mod tests;
