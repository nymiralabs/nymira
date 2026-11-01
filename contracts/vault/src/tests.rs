//! Unit tests: the contract's methods called as a client would, under `testing_env!` (the real
//! `alt_bn128` and Poseidon code run here). Sandbox tests are in `tests/`.

use near_sdk::json_types::{Base64VecU8, U128};
use near_sdk::test_utils::{get_logs, VMContextBuilder};
use near_sdk::{env, testing_env, AccountId, IntoStorageKey, NearToken};
use nymira_testkit::proofs::{canonical_verifying_key, signing_key, Kind};
use zkapi_core::{Felt252, MerkleTree, MERKLE_DEPTH};

use super::*;

const VAULT: &str = "vault.test";
const TOKEN: &str = "usdc.test";
const NOTE_FEE: u128 = 50_000;
const ONE: u128 = 1_000_000;
const FIVE: u128 = 5_000_000;
const DAY: u64 = 86_400;
/// 2026-11-02 00:00:00 UTC.
const T0: u64 = 1_793_577_600;

fn context(predecessor: &str, secs: u64) -> VMContextBuilder {
    let mut builder = VMContextBuilder::new();
    builder
        .current_account_id(VAULT.parse().unwrap())
        .predecessor_account_id(predecessor.parse().unwrap())
        .block_timestamp(secs * 1_000_000_000);
    builder
}

fn at(secs: u64) {
    testing_env!(context(TOKEN, secs).build());
}

fn key(seed: u64) -> PublicKey {
    let point = signing_key(seed);
    PublicKey {
        x: hex::encode32(&point.x.0),
        y: hex::encode32(&point.y.0),
    }
}

#[derive(Clone)]
struct Params {
    operator_pk: PublicKey,
    clearance_pk: PublicKey,
    denominations: Vec<u128>,
    tvl_cap: u128,
    request_vk: Vec<u8>,
    withdrawal_vk: Vec<u8>,
    note_fee: u128,
    exit_delay_secs: u64,
}

impl Default for Params {
    fn default() -> Self {
        Self {
            operator_pk: key(1),
            clearance_pk: key(2),
            denominations: vec![ONE, FIVE],
            tvl_cap: 100_000_000,
            request_vk: canonical_verifying_key(Kind::Request),
            withdrawal_vk: canonical_verifying_key(Kind::Withdrawal),
            note_fee: NOTE_FEE,
            exit_delay_secs: 7 * DAY,
        }
    }
}

fn account(name: &str) -> AccountId {
    name.parse().unwrap()
}

fn try_new(p: Params) -> Vault {
    at(T0);
    Vault::new(
        p.operator_pk,
        p.clearance_pk,
        account(TOKEN),
        p.denominations.into_iter().map(U128).collect(),
        U128(p.tvl_cap),
        Base64VecU8(p.request_vk),
        Base64VecU8(p.withdrawal_vk),
        account("treasury.test"),
        U128(p.note_fee),
        p.exit_delay_secs,
        30 * DAY,
        vec![U128(100_000), U128(500_000)],
        U128(1_000),
        U128(2_000),
        1_000,
        account("owner.test"),
        account("operator.test"),
    )
}

fn vault() -> Vault {
    try_new(Params::default())
}

/// A vault whose cap admits hundreds of notes.
fn roomy_vault() -> Vault {
    try_new(Params {
        tvl_cap: 10u128.pow(12),
        ..Params::default()
    })
}

fn commitment(n: u64) -> String {
    hex::encode32(&Felt252::from_u64(n).0)
}

fn msg(n: u64) -> String {
    format!(r#"{{"commitment":"{}"}}"#, commitment(n))
}

/// A `ft_on_transfer` call from the token; returns the unused amount.
fn deposit(v: &mut Vault, sender: &str, amount: u128, msg: &str) -> u128 {
    v.ft_on_transfer(account(sender), U128(amount), msg.to_owned())
        .0
}

fn deposit_ok(v: &mut Vault, n: u64) {
    // A fresh environment at the current time: the token calls, and the log buffer is empty.
    at(env::block_timestamp() / 1_000_000_000);
    assert_eq!(deposit(v, "alice.test", ONE + NOTE_FEE, &msg(n)), 0);
}

/// `current_tvl` and `open_notes` equal what the notes say.
fn assert_counters(v: &Vault) {
    let mut tvl = 0;
    let mut open = 0;
    for record in v.note_records(0, u32::MAX) {
        if record.status != Status::Closed {
            tvl += record.deposit_amount.0 + NOTE_FEE;
        }
        if record.status == Status::Active {
            open += 1;
        }
    }
    assert_eq!(v.config().current_tvl.0, tvl);
    assert_eq!(v.open_notes(), open);
}

fn leaves(v: &Vault) -> Vec<Felt252> {
    let mut out = Vec::new();
    let mut from = 0;
    loop {
        let page = v.get_leaves(from, 100);
        if page.is_empty() {
            return out;
        }
        from += page.len() as u32;
        out.extend(
            page.iter()
                .map(|leaf| Felt252(hex::decode32(leaf).unwrap())),
        );
    }
}

fn client_root(v: &Vault) -> String {
    hex::encode32(&MerkleTree::from_leaves(&leaves(v)).root().0)
}

// new()

#[test]
fn new_stores_the_parameters_and_reports_the_pins() {
    let v = vault();
    let config = v.config();
    assert_eq!(config.token, account(TOKEN));
    assert_eq!(config.treasury, account("treasury.test"));
    assert_eq!(config.owner, account("owner.test"));
    assert_eq!(config.operator_account, account("operator.test"));
    assert_eq!(config.chain_id.0, chain_id(Network::Testnet));
    assert_eq!(config.denominations, vec![U128(ONE), U128(FIVE)]);
    assert_eq!(config.note_fee.0, NOTE_FEE);
    assert_eq!(config.close_fee.0, 30_000);
    assert_eq!(config.tvl_cap.0, 100_000_000);
    assert_eq!(config.current_tvl.0, 0);
    assert_eq!(config.exit_delay_secs, 7 * DAY);
    assert_eq!(config.note_ttl, 30 * DAY);
    assert_eq!(config.operator_pk.x, key(1).x);
    assert_eq!(config.clearance_pk.y, key(2).y);
    assert_eq!(v.leaf_count(), 0);
    let hashes = v.vk_hashes();
    assert_eq!(
        hashes.request,
        "0x38348b670fd21c8c622075f84f5c4a246f54081e09886b0a3c86c89edc01e1c8"
    );
    assert_eq!(
        hashes.withdrawal,
        "0xc14844d3af2b29303852e7e47e642830552e0c73df3ebbed8f0d27c316be9ffa"
    );
}

#[test]
fn the_dev_pins_are_the_hashes_of_the_dev_keys() {
    at(T0);
    assert_eq!(
        env::keccak256_array(canonical_verifying_key(Kind::Request)),
        pins::PINS.request
    );
    assert_eq!(
        env::keccak256_array(canonical_verifying_key(Kind::Withdrawal)),
        pins::PINS.withdrawal
    );
}

/// Offset of the first IC element in the canonical key: alpha (32), beta, gamma and delta (64
/// each) and the 8-byte length of the IC list.
const IC_START: usize = 32 + 3 * 64 + 8;

#[test]
#[should_panic(expected = "request verifying key does not match the compiled hash")]
fn new_panics_when_a_byte_of_the_request_ic_changes() {
    let mut p = Params::default();
    p.request_vk[IC_START + 5] ^= 1;
    try_new(p);
}

#[test]
#[should_panic(expected = "withdrawal verifying key does not match the compiled hash")]
fn new_panics_when_a_byte_of_the_withdrawal_ic_changes() {
    let mut p = Params::default();
    p.withdrawal_vk[IC_START + 5] ^= 1;
    try_new(p);
}

#[test]
#[should_panic(expected = "request verifying key does not match the compiled hash")]
fn new_panics_when_the_request_key_is_swapped_for_the_withdrawal_key() {
    let mut p = Params::default();
    p.request_vk = p.withdrawal_vk.clone();
    try_new(p);
}

#[test]
#[should_panic(expected = "no denominations")]
fn new_panics_without_denominations() {
    try_new(Params {
        denominations: vec![],
        ..Params::default()
    });
}

#[test]
#[should_panic(expected = "repeated denomination")]
fn new_panics_on_repeated_denominations() {
    try_new(Params {
        denominations: vec![ONE, FIVE, ONE],
        ..Params::default()
    });
}

#[test]
#[should_panic(expected = "a denomination is zero")]
fn new_panics_on_a_zero_denomination() {
    try_new(Params {
        denominations: vec![0, ONE],
        ..Params::default()
    });
}

#[test]
#[should_panic(expected = "note fee is zero")]
fn new_panics_on_a_zero_note_fee() {
    try_new(Params {
        note_fee: 0,
        ..Params::default()
    });
}

#[test]
#[should_panic(expected = "exit delay under one day")]
fn new_panics_on_an_exit_delay_under_a_day() {
    try_new(Params {
        exit_delay_secs: DAY - 1,
        ..Params::default()
    });
}

#[test]
fn new_accepts_an_exit_delay_of_exactly_a_day() {
    try_new(Params {
        exit_delay_secs: DAY,
        ..Params::default()
    });
}

#[test]
#[should_panic(expected = "off the curve")]
fn new_panics_on_a_key_off_the_curve() {
    let mut bent = key(1);
    bent.y = hex::encode32(&Felt252::from_u64(3).0);
    try_new(Params {
        operator_pk: bent,
        ..Params::default()
    });
}

#[test]
#[should_panic(expected = "off the curve")]
fn new_panics_on_the_identity_key() {
    try_new(Params {
        clearance_pk: PublicKey {
            x: commitment(0),
            y: commitment(1),
        },
        ..Params::default()
    });
}

#[test]
#[should_panic(expected = "operator and clearance keys are the same")]
fn new_panics_when_both_keys_are_equal() {
    try_new(Params {
        clearance_pk: key(1),
        ..Params::default()
    });
}

#[test]
fn storage_prefixes_are_pinned() {
    // Changing one of these moves live state: append new keys, never reorder.
    assert_eq!(StorageKey::Notes.into_storage_key(), vec![0]);
    assert_eq!(StorageKey::Commitments.into_storage_key(), vec![1]);
    assert_eq!(StorageKey::KnownRoots.into_storage_key(), vec![2]);
    assert_eq!(StorageKey::RequestVk.into_storage_key(), vec![3]);
    assert_eq!(StorageKey::WithdrawalVk.into_storage_key(), vec![4]);
}

#[test]
fn state_carries_a_version() {
    assert_eq!(vault().version, STATE_VERSION);
}

// Deposits

#[test]
fn a_valid_deposit_stores_the_note_and_emits_deposit() {
    let mut v = vault();
    at(T0 + 10);
    assert_eq!(deposit(&mut v, "alice.test", ONE + NOTE_FEE, &msg(7)), 0);
    let logs = get_logs();
    assert_eq!(logs.len(), 1);
    let expiry = end_of_utc_day(T0 + 10 + 30 * DAY).unwrap();
    let event: near_sdk::serde_json::Value =
        near_sdk::serde_json::from_str(logs[0].strip_prefix("EVENT_JSON:").unwrap()).unwrap();
    assert_eq!(
        event,
        near_sdk::serde_json::json!({
            "standard": "nymira",
            "version": "1.0.0",
            "event": "deposit",
            "data": {
                "note_id": 0,
                "commitment": commitment(7),
                "denomination": "1000000",
                "expiry": expiry,
                "withdraw_to": "alice.test",
            },
        })
    );
    let records = v.note_records(0, 10);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].note_id, 0);
    assert_eq!(records[0].deposit_amount.0, ONE);
    assert_eq!(records[0].expiry, expiry);
    assert_eq!(records[0].status, Status::Active);
    assert!(!records[0].leaf_retired);
    assert_eq!(v.note_status(commitment(7)), Some(Status::Active));
    assert_eq!(v.treasury_balance().0, NOTE_FEE);
    assert_eq!(v.open_notes(), 1);
    assert_eq!(v.config().current_tvl.0, ONE + NOTE_FEE);
    assert_counters(&v);
}

#[test]
fn the_payout_account_defaults_to_the_sender_and_can_be_named() {
    let mut v = vault();
    deposit_ok(&mut v, 1);
    let named = format!(
        r#"{{"commitment":"{}","withdraw_to":"fresh.test"}}"#,
        commitment(2)
    );
    assert_eq!(deposit(&mut v, "alice.test", FIVE + NOTE_FEE, &named), 0);
    assert_eq!(v.notes.get(0).unwrap().withdraw_to, account("alice.test"));
    assert_eq!(v.notes.get(1).unwrap().withdraw_to, account("fresh.test"));
    assert_eq!(v.notes.get(1).unwrap().deposit_amount, FIVE);
}

#[test]
fn note_ids_are_sequential_and_the_leaf_is_the_pinned_formula() {
    let mut v = vault();
    for n in 1..=3 {
        deposit_ok(&mut v, n);
    }
    for id in 0..3u32 {
        let note = v.notes.get(id).unwrap();
        let expected = zkapi_core::v2::note_leaf(id, &Felt252(note.commitment), ONE, note.expiry);
        assert_eq!(note.leaf, expected.0);
    }
}

#[test]
fn the_leaf_matches_the_published_vector() {
    // zkapi's own vector: registration of secret 42, note 7, deposit 5_000_000, expiry
    // 4_000_000_000 (leaf_vectors_match_the_specification in zkapi-core).
    let registration = zkapi_core::v2::registration_commitment(&Felt252::from_u64(42));
    assert_eq!(
        hex::encode32(&registration.0),
        "0x26538304fc53474b13823162f0b9667c1538c7360828d3a1a2e8ffa019a2f8cc"
    );
    assert_eq!(
        hex::encode32(&zkapi_core::v2::note_fields(7, 5_000_000, 4_000_000_000).0),
        "0x0000000000000007000000000000000000000000004c4b4000000000ee6b2800"
    );
}

#[test]
fn an_overflowing_expiry_is_a_refusal_not_a_panic() {
    assert_eq!(end_of_utc_day(u64::MAX), None);
    let mut v = vault();
    v.note_ttl = u64::MAX;
    assert_eq!(
        deposit(&mut v, "alice.test", ONE + NOTE_FEE, &msg(1)),
        ONE + NOTE_FEE
    );
}

#[test]
fn expiry_is_the_end_of_the_utc_day_thirty_days_on() {
    assert_eq!(end_of_utc_day(0), Some(DAY - 1));
    assert_eq!(end_of_utc_day(DAY - 1), Some(DAY - 1));
    assert_eq!(end_of_utc_day(DAY), Some(2 * DAY - 1));
    let mut v = vault();
    // The last second of a day, and the first second of the next: different expiries.
    for (n, now) in [(1u64, T0 + DAY - 1), (2, T0 + DAY)] {
        at(now);
        assert_eq!(deposit(&mut v, "alice.test", ONE + NOTE_FEE, &msg(n)), 0);
    }
    assert_eq!(v.note_records(0, 1)[0].expiry, T0 + 31 * DAY - 1);
    assert_eq!(v.note_records(1, 1)[0].expiry, T0 + 32 * DAY - 1);
}

/// One refusal: the whole amount comes back and nothing changes.
fn assert_refused(v: &mut Vault, predecessor: &str, sender: &str, amount: u128, msg: &str) {
    let notes = v.note_records(0, u32::MAX).len();
    let treasury = v.treasury_balance().0;
    testing_env!(context(predecessor, T0).build());
    assert_eq!(deposit(v, sender, amount, msg), amount);
    assert!(get_logs().is_empty());
    assert_eq!(v.note_records(0, u32::MAX).len(), notes);
    assert_eq!(v.treasury_balance().0, treasury);
    assert_counters(v);
}

#[test]
fn a_call_from_another_token_is_returned() {
    assert_refused(
        &mut vault(),
        "fake.test",
        "alice.test",
        ONE + NOTE_FEE,
        &msg(1),
    );
}

#[test]
fn a_wrong_amount_is_returned() {
    let mut v = vault();
    assert_refused(&mut v, TOKEN, "alice.test", ONE, &msg(1));
    assert_refused(&mut v, TOKEN, "alice.test", ONE + NOTE_FEE + 1, &msg(1));
    assert_refused(&mut v, TOKEN, "alice.test", 0, &msg(1));
}

#[test]
fn a_bad_denomination_is_returned() {
    // 2 USDC plus the fee: a well-formed total for a denomination the vault does not have.
    assert_refused(
        &mut vault(),
        TOKEN,
        "alice.test",
        2 * ONE + NOTE_FEE,
        &msg(1),
    );
}

#[test]
fn malformed_json_is_returned() {
    let mut v = vault();
    for bad in [
        "",
        "not json",
        "{",
        "{}",
        r#"{"commitment":5}"#,
        r#"{"commitment":"0x12"}"#,
        r#"{"commitment":"0xzz00000000000000000000000000000000000000000000000000000000000000"}"#,
        r#"{"withdraw_to":"a.test"}"#,
        r#"{"commitment":"0000000000000000000000000000000000000000000000000000000000000001"}"#,
    ] {
        assert_refused(&mut v, TOKEN, "alice.test", ONE + NOTE_FEE, bad);
    }
    // An unknown field, a payout string that is no account id.
    let extra = format!(r#"{{"commitment":"{}","memo":"x"}}"#, commitment(1));
    assert_refused(&mut v, TOKEN, "alice.test", ONE + NOTE_FEE, &extra);
    let bad_account = format!(
        r#"{{"commitment":"{}","withdraw_to":"Not An Account"}}"#,
        commitment(1)
    );
    assert_refused(&mut v, TOKEN, "alice.test", ONE + NOTE_FEE, &bad_account);
}

#[test]
fn an_out_of_range_commitment_is_returned() {
    let mut v = vault();
    let modulus = hex::encode32(&SCALAR_MODULUS_BE);
    let mut below = SCALAR_MODULUS_BE;
    below[31] -= 1;
    for bad in [modulus, hex::encode32(&[0xff; 32])] {
        let m = format!(r#"{{"commitment":"{bad}"}}"#);
        assert_refused(&mut v, TOKEN, "alice.test", ONE + NOTE_FEE, &m);
    }
    // The largest field element is fine.
    let m = format!(r#"{{"commitment":"{}"}}"#, hex::encode32(&below));
    assert_eq!(deposit(&mut v, "alice.test", ONE + NOTE_FEE, &m), 0);
}

#[test]
fn a_repeated_commitment_is_returned() {
    let mut v = vault();
    deposit_ok(&mut v, 1);
    assert_refused(&mut v, TOKEN, "bob.test", ONE + NOTE_FEE, &msg(1));
    assert_refused(&mut v, TOKEN, "bob.test", FIVE + NOTE_FEE, &msg(1));
}

#[test]
fn a_payout_account_the_vault_cannot_pay_is_returned() {
    let mut v = vault();
    for payout in [VAULT, TOKEN, "intents.near"] {
        let m = format!(
            r#"{{"commitment":"{}","withdraw_to":"{payout}"}}"#,
            commitment(1)
        );
        assert_refused(&mut v, TOKEN, "alice.test", ONE + NOTE_FEE, &m);
    }
    // The same accounts as the sender, with no payout named.
    for sender in [VAULT, TOKEN, "intents.near"] {
        assert_refused(&mut v, TOKEN, sender, ONE + NOTE_FEE, &msg(1));
    }
}

#[test]
fn the_tvl_cap_is_a_cap_on_full_deposits() {
    let mut v = try_new(Params {
        tvl_cap: 3 * (ONE + NOTE_FEE),
        ..Params::default()
    });
    for n in 1..=3 {
        deposit_ok(&mut v, n);
    }
    assert_refused(&mut v, TOKEN, "alice.test", ONE + NOTE_FEE, &msg(4));
    assert_eq!(v.config().current_tvl.0, 3 * (ONE + NOTE_FEE));
    assert_eq!(v.treasury_balance().0, 3 * NOTE_FEE);
}

#[test]
fn the_tvl_cap_admits_a_deposit_that_lands_exactly_on_it() {
    let mut v = try_new(Params {
        tvl_cap: FIVE + NOTE_FEE,
        ..Params::default()
    });
    assert_eq!(deposit(&mut v, "alice.test", FIVE + NOTE_FEE, &msg(1)), 0);
    assert_eq!(v.config().current_tvl.0, FIVE + NOTE_FEE);
    assert_refused(&mut v, TOKEN, "alice.test", ONE + NOTE_FEE, &msg(2));
}

#[test]
fn a_hundred_dollar_cap_holds_ninety_five_notes_of_one_usdc() {
    let mut v = vault();
    for n in 1..=95 {
        deposit_ok(&mut v, n);
    }
    assert_refused(&mut v, TOKEN, "alice.test", ONE + NOTE_FEE, &msg(96));
    assert_eq!(v.config().current_tvl.0, 99_750_000);
    assert_counters(&v);
}

#[test]
fn a_deposit_below_the_near_reserve_is_returned() {
    let mut v = vault();
    // Liquid NEAR is balance minus storage_usage * byte cost.
    let usage = env::storage_usage();
    let byte_cost = env::storage_byte_cost().as_yoctonear();
    let storage = u128::from(usage) * byte_cost;
    for (balance, accepted) in [
        (storage + RESERVE - 1, false),
        (storage + RESERVE, true),
        (storage, false),
        (0, false),
    ] {
        let mut builder = context(TOKEN, T0);
        builder
            .storage_usage(usage)
            .account_balance(NearToken::from_yoctonear(balance));
        testing_env!(builder.build());
        let before = v.note_records(0, u32::MAX).len();
        let n = before as u64 + 100;
        let unused = deposit(&mut v, "alice.test", ONE + NOTE_FEE, &msg(n));
        assert_eq!(unused == 0, accepted, "balance {balance}");
        assert_eq!(
            v.note_records(0, u32::MAX).len(),
            before + usize::from(accepted)
        );
    }
    assert_counters(&v);
}

#[test]
fn liquid_near_is_balance_minus_locked_storage() {
    let v = vault();
    let mut builder = context(TOKEN, T0);
    builder
        .storage_usage(1_000)
        .account_balance(NearToken::from_near(10));
    testing_env!(builder.build());
    let byte_cost = env::storage_byte_cost().as_yoctonear();
    assert_eq!(
        v.liquid_near().0,
        NearToken::from_near(10).as_yoctonear() - 1_000 * byte_cost
    );
    let mut broke = context(TOKEN, T0);
    broke
        .storage_usage(1_000_000_000)
        .account_balance(NearToken::from_near(1));
    testing_env!(broke.build());
    assert_eq!(v.liquid_near().0, 0);
}

#[test]
fn ft_on_transfer_creates_no_promise() {
    let mut v = vault();
    deposit_ok(&mut v, 1);
    assert_refused(&mut v, TOKEN, "alice.test", 1, &msg(2));
    testing_env!(context(TOKEN, T0).build());
    deposit_ok(&mut v, 3);
    assert!(near_sdk::test_utils::get_created_receipts().is_empty());
}

// sync_root

fn sync(v: &mut Vault, at_secs: u64, max: u32) -> u32 {
    testing_env!(context("anyone.test", at_secs).build());
    v.sync_root(max)
}

#[test]
fn sync_root_with_nothing_waiting_does_nothing() {
    let mut v = vault();
    assert_eq!(sync(&mut v, T0 + 3_600, 120), 0);
    assert!(get_logs().is_empty());
    let empty_root = hex::encode32(&zero_hashes::ZERO_HASHES[32]);
    assert_eq!(v.current_root(), empty_root);
    assert!(v.is_known_root(empty_root));
    assert_eq!(client_root(&v), v.current_root());
}

#[test]
fn sync_root_waits_for_thirty_seconds_or_sixteen_leaves() {
    let mut v = vault();
    for n in 1..=15 {
        deposit_ok(&mut v, n);
    }
    // 29 seconds after the vault started: too early.
    assert_eq!(sync(&mut v, T0 + 29, 120), 0);
    assert_eq!(v.leaf_count(), 0);
    // 30 seconds: inserts all 15.
    assert_eq!(sync(&mut v, T0 + 30, 120), 15);
    assert_eq!(v.leaf_count(), 15);
    // The clock restarts at the insert.
    deposit_ok(&mut v, 16);
    assert_eq!(sync(&mut v, T0 + 59, 120), 0);
    assert_eq!(sync(&mut v, T0 + 60, 120), 1);
}

#[test]
fn sixteen_waiting_leaves_sync_at_once() {
    let mut v = vault();
    for n in 1..=16 {
        deposit_ok(&mut v, n);
    }
    assert_eq!(sync(&mut v, T0, 120), 16);
    assert_eq!(client_root(&v), v.current_root());
}

#[test]
fn sync_root_inserts_at_most_the_cap_and_at_most_max() {
    let mut v = roomy_vault();
    for n in 1..=100 {
        deposit_ok(&mut v, n);
    }
    assert_eq!(sync(&mut v, T0, 0), 0);
    assert_eq!(sync(&mut v, T0, 5), 5);
    assert_eq!(sync(&mut v, T0, 1_000), MAX_SYNC_LEAVES);
    assert_eq!(v.leaf_count(), 5 + MAX_SYNC_LEAVES);
    // 15 waiting and no 30 seconds yet.
    assert_eq!(sync(&mut v, T0 + 1, 1_000), 0);
    assert_eq!(sync(&mut v, T0 + 30, 1_000), 100 - 5 - MAX_SYNC_LEAVES);
    assert_eq!(client_root(&v), v.current_root());
}

#[test]
fn a_full_batch_emits_one_event_and_one_log() {
    let mut v = roomy_vault();
    for n in 1..=u64::from(MAX_SYNC_LEAVES) {
        deposit_ok(&mut v, n);
    }
    assert_eq!(sync(&mut v, T0, 120), MAX_SYNC_LEAVES);
    let logs = get_logs();
    assert_eq!(logs.len(), 1);
    assert!(logs[0].len() < 16 * 1024);
    let event: near_sdk::serde_json::Value =
        near_sdk::serde_json::from_str(logs[0].strip_prefix("EVENT_JSON:").unwrap()).unwrap();
    assert_eq!(
        event,
        near_sdk::serde_json::json!({
            "standard": "nymira",
            "version": "1.0.0",
            "event": "sync_root",
            "data": {"first_index": 0, "last_index": MAX_SYNC_LEAVES - 1, "root": v.current_root()},
        })
    );
}

#[test]
fn every_root_stays_known_and_matches_a_client_rebuild() {
    let mut v = vault();
    let mut roots = vec![v.current_root()];
    let mut next = 1;
    let mut now = T0;
    // Batch sizes that cross the powers of two and leave the tree at odd and even sizes.
    for batch in [1u64, 1, 2, 3, 1, 5, 8, 13, 1, 20] {
        for _ in 0..batch {
            deposit_ok(&mut v, next);
            next += 1;
        }
        now += 30;
        assert_eq!(sync(&mut v, now, 120) as u64, batch);
        assert_eq!(
            client_root(&v),
            v.current_root(),
            "after {} leaves",
            next - 1
        );
        roots.push(v.current_root());
    }
    for root in &roots {
        assert!(v.is_known_root(root.clone()));
    }
    assert_eq!(
        roots.iter().collect::<std::collections::HashSet<_>>().len(),
        roots.len()
    );
    assert!(!v.is_known_root(commitment(12_345)));
    assert!(!v.is_known_root("garbage".to_owned()));
    assert_counters(&v);
}

#[test]
fn the_zero_hash_constants_match_zkapi_core() {
    let zeros = zkapi_core::v2::zero_hashes();
    for (level, expected) in zeros.iter().enumerate() {
        assert_eq!(zero_hashes::ZERO_HASHES[level], expected.0, "level {level}");
    }
    assert_eq!(zero_hashes::ZERO_HASHES.len(), MERKLE_DEPTH + 1);
}

#[test]
fn the_tree_matches_zkapi_cores_frontier_at_every_size() {
    let mut v = vault();
    let mut reference = zkapi_core::merkle::Frontier::new();
    let mut now = T0;
    for n in 1..=40u64 {
        deposit_ok(&mut v, n);
        now += 30;
        assert_eq!(sync(&mut v, now, 120), 1);
        reference
            .append(Felt252(v.notes.get(n as u32 - 1).unwrap().leaf))
            .unwrap();
        assert_eq!(v.current_root(), hex::encode32(&reference.root().0));
    }
}

#[test]
fn a_note_closed_before_sync_root_reaches_it_is_a_zero_leaf() {
    let mut v = vault();
    for n in 1..=5 {
        deposit_ok(&mut v, n);
    }
    // Close note 2 the way a later close would.
    v.notes.get_mut(2).unwrap().status = Status::Closed;
    assert_eq!(sync(&mut v, T0 + 30, 120), 5);
    let leaves = v.get_leaves(0, 100);
    assert_eq!(leaves.len(), 5);
    assert_eq!(leaves[2], commitment(0));
    assert_ne!(leaves[1], commitment(0));
    assert!(v.note_records(2, 1)[0].leaf_retired);
    assert!(!v.note_records(1, 1)[0].leaf_retired);
    assert_eq!(client_root(&v), v.current_root());
}

#[test]
fn a_zero_leaf_next_to_an_odd_index_still_gives_the_client_root() {
    let mut v = vault();
    for n in 1..=4 {
        deposit_ok(&mut v, n);
    }
    v.notes.get_mut(0).unwrap().status = Status::Closed;
    v.notes.get_mut(3).unwrap().status = Status::Closed;
    assert_eq!(sync(&mut v, T0 + 30, 2), 2);
    assert_eq!(client_root(&v), v.current_root());
    assert_eq!(sync(&mut v, T0 + 60, 120), 2);
    assert_eq!(client_root(&v), v.current_root());
}

// Views

#[test]
fn get_leaves_pages_and_caps_the_limit() {
    let mut v = roomy_vault();
    for n in 1..=130 {
        deposit_ok(&mut v, n);
    }
    sync(&mut v, T0, 120);
    sync(&mut v, T0 + 30, 120);
    assert_eq!(v.leaf_count(), 130);
    assert_eq!(v.get_leaves(0, 1_000).len(), 100);
    assert_eq!(v.get_leaves(100, 1_000).len(), 30);
    assert_eq!(v.get_leaves(130, 10).len(), 0);
    assert_eq!(v.get_leaves(u32::MAX, u32::MAX).len(), 0);
    assert_eq!(v.get_leaves(5, 3).len(), 3);
    assert_eq!(
        v.get_leaves(5, 1)[0],
        hex::encode32(&v.notes.get(5).unwrap().leaf)
    );
}

#[test]
fn get_leaves_shows_only_inserted_leaves() {
    let mut v = vault();
    for n in 1..=3 {
        deposit_ok(&mut v, n);
    }
    assert!(v.get_leaves(0, 10).is_empty());
    assert_eq!(v.leaf_count(), 0);
    assert_eq!(v.note_records(0, 10).len(), 3);
}

#[test]
fn note_records_has_the_five_public_fields_and_caps_the_limit() {
    let mut v = roomy_vault();
    for n in 1..=105 {
        deposit_ok(&mut v, n);
    }
    assert_eq!(v.note_records(0, 1_000).len(), 100);
    assert_eq!(v.note_records(100, 1_000).len(), 5);
    assert_eq!(v.note_records(105, 1).len(), 0);
    let value = near_sdk::serde_json::to_value(&v.note_records(3, 1)[0]).unwrap();
    let mut fields: Vec<_> = value.as_object().unwrap().keys().cloned().collect();
    fields.sort();
    assert_eq!(
        fields,
        [
            "deposit_amount",
            "expiry",
            "leaf_retired",
            "note_id",
            "status"
        ]
    );
    assert_eq!(value["note_id"], 3);
    assert_eq!(value["deposit_amount"], "1000000");
    assert_eq!(value["status"], "Active");
}

#[test]
fn note_status_by_commitment() {
    let mut v = vault();
    deposit_ok(&mut v, 1);
    assert_eq!(v.note_status(commitment(1)), Some(Status::Active));
    assert_eq!(v.note_status(commitment(2)), None);
    v.notes.get_mut(0).unwrap().status = Status::PendingExit;
    assert_eq!(v.note_status(commitment(1)), Some(Status::PendingExit));
}

#[test]
fn open_notes_counts_active_notes() {
    let mut v = vault();
    for n in 1..=3 {
        deposit_ok(&mut v, n);
    }
    assert_eq!(v.open_notes(), 3);
}

#[test]
fn hex_words_round_trip_and_refuse_bad_text() {
    let word: [u8; 32] = core::array::from_fn(|i| (i * 7) as u8);
    assert_eq!(hex::decode32(&hex::encode32(&word)), Some(word));
    assert_eq!(
        hex::decode32(&hex::encode32(&word).to_uppercase().replace("0X", "0x")),
        Some(word)
    );
    for bad in [
        "",
        "0x",
        "0x00",
        &format!("0x{}", "g".repeat(64)),
        &format!("1x{}", "0".repeat(64)),
        &"0".repeat(66),
    ] {
        assert_eq!(hex::decode32(bad), None, "{bad}");
    }
}
