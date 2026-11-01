//! `nymira-vault`: USDC deposits as notes in a Poseidon Merkle tree.
//!
//! `new()`, deposits through `ft_on_transfer`, `sync_root` and the tree views.

mod events;
mod pins;
mod tree;
mod word;
mod zero_hashes;

use near_sdk::json_types::{Base64VecU8, U128, U64};
use near_sdk::serde_json::json;
use near_sdk::store::{Lazy, LookupMap, LookupSet, Vector};
use near_sdk::{
    env, near, require, AccountId, BorshStorageKey, Gas, NearToken, PanicOnDefault, Promise,
    PromiseError, PromiseOrValue,
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
/// Deposits from this account would need the Intents branch; a payout account may never be it.
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
    known_roots: LookupSet<[u8; 32]>,
    /// Block time of the last insert (nanoseconds).
    last_sync_ns: u64,
    /// Spent nullifiers as canonical big-endian words.
    nullifiers: LookupSet<[u8; 32]>,
    /// Failed payouts by payout account: one record per account.
    pending_payouts: LookupMap<AccountId, u128>,
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
    pub const VALID_UNTIL: usize = 15;
    pub const COUNT: usize = 16;
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
            bound_classes: bound_classes.into_iter().map(|b| b.0).collect(),
            charge_step: charge_step.0,
            min_charge: min_charge.0,
            slot_rate,
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
            known_roots,
            last_sync_ns: env::block_timestamp(),
            nullifiers: LookupSet::new(StorageKey::Nullifiers),
            pending_payouts: LookupMap::new(StorageKey::PendingPayouts),
        }
    }

    /// NEP-141 receiver. `msg` is `{"commitment": "0x..", "withdraw_to": "<account>"?}`. Never
    /// panics on bad input: the whole amount is returned unused. Creates no promise.
    pub fn ft_on_transfer(&mut self, sender_id: AccountId, amount: U128, msg: String) -> U128 {
        if env::predecessor_account_id() != self.token {
            return amount;
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
        let (words, scalars) = parse_inputs(&inputs);
        let word = |index: usize| &words[index];
        require!(small(word(input::KIND), 1) == Some(1), "not a mutual close");
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
        let nullifier = *word(input::WITHDRAWAL_NULLIFIER);
        require!(
            !self.nullifiers.contains(&nullifier),
            "nullifier already used"
        );
        let key = VerifyingKey::from(&*self.withdrawal_vk);
        let proof =
            Proof::from_zkapi_wire(&proof.0).unwrap_or_else(|_| env::panic_str("malformed proof"));
        require!(
            nymira_verifier::verify(&key, &scalars, &proof) == Ok(true),
            "invalid proof"
        );

        // Effects before the promise.
        let account = note.withdraw_to.clone();
        let commitment = note.commitment;
        let deposit = note.deposit_amount;
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
        if payout == 0 {
            return PromiseOrValue::Value(());
        }
        PromiseOrValue::Promise(self.payout(&account, payout).then(
            Self::callback().on_close_payout(
                account,
                U128(payout),
                note_id,
                word::encode32(&commitment),
            ),
        ))
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
            self.credit_pending(account, amount.0, Some((note_id, commitment)));
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
    /// The vault's own callback, with its fixed gas and no share of leftover gas.
    fn callback() -> VaultExt {
        Self::ext(env::current_account_id())
            .with_static_gas(GAS_CALLBACK)
            .with_unused_gas_weight(0)
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

    /// A failed close payout: added to the account's record, or a new record when it is at least
    /// the note fee, otherwise dust for the treasury.
    fn credit_pending(&mut self, account: AccountId, amount: u128, note: Option<(u32, String)>) {
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
                "reason": "transfer_failed",
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
        if commitment >= SCALAR_MODULUS_BE || self.commitments.contains_key(&commitment) {
            return None;
        }
        let payout = msg.withdraw_to.unwrap_or(sender_id);
        if payout == env::current_account_id()
            || payout == self.token
            || payout.as_str() == INTENTS_ACCOUNT
        {
            return None;
        }
        if self.tvl.checked_add(amount)? > self.tvl_cap {
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
        });
        self.commitments.insert(commitment, note_id);
        self.tvl += amount;
        self.open_notes += 1;
        self.treasury_balance += note_fee;
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
        Some(())
    }
}

/// Parses the 16 public input words: the big-endian words, and the little-endian scalars the
/// verifier takes. Refuses a wrong count or length and any word at or above the field modulus.
fn parse_inputs(inputs: &[Base64VecU8]) -> (Vec<[u8; 32]>, Vec<[u8; 32]>) {
    require!(
        inputs.len() == input::COUNT,
        "wrong number of public inputs"
    );
    let mut words = Vec::with_capacity(input::COUNT);
    let mut scalars = Vec::with_capacity(input::COUNT);
    for item in inputs {
        let scalar = public_input_from_wire(&item.0)
            .unwrap_or_else(|_| env::panic_str("public input is not a canonical field element"));
        words.push(<[u8; 32]>::try_from(item.0.as_slice()).expect("32 bytes"));
        scalars.push(scalar);
    }
    (words, scalars)
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
