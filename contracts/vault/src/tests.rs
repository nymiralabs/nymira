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
        x: word::encode32(&point.x.0),
        y: word::encode32(&point.y.0),
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
    // A fresh chain: the mock storage outlives `testing_env!`.
    near_sdk::mock::with_mocked_blockchain(|b| drop(b.take_storage()));
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
    word::encode32(&Felt252::from_u64(n).0)
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
    assert_counters(v);
}

/// `current_tvl` and `open_notes` equal what the notes say.
fn assert_counters(v: &Vault) {
    let mut tvl = 0;
    let mut open = 0;
    let records = (0..v.notes.len())
        .step_by(100)
        .flat_map(|from| v.note_records(from, 100));
    for record in records {
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
                .map(|leaf| Felt252(word::decode32(leaf).unwrap())),
        );
    }
}

fn client_root(v: &Vault) -> String {
    word::encode32(&MerkleTree::from_leaves(&leaves(v)).root().0)
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
    bent.y = word::encode32(&Felt252::from_u64(3).0);
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
fn the_leaf_formula_matches_the_published_vector() {
    // zkapi's own vector: registration of secret 42, note 7, deposit 5_000_000, expiry
    // 4_000_000_000 (leaf_vectors_match_the_specification in zkapi-core).
    let registration = zkapi_core::v2::registration_commitment(&Felt252::from_u64(42));
    assert_eq!(
        word::encode32(&registration.0),
        "0x26538304fc53474b13823162f0b9667c1538c7360828d3a1a2e8ffa019a2f8cc"
    );
    assert_eq!(
        word::encode32(&zkapi_core::v2::note_fields(7, 5_000_000, 4_000_000_000).0),
        "0x0000000000000007000000000000000000000000004c4b4000000000ee6b2800"
    );
    assert_eq!(
        word::encode32(&zkapi_core::v2::note_leaf(7, &registration, 5_000_000, 4_000_000_000).0),
        "0x1698e82280376fc3506cb7ebf7278db1764d0183aea5fd4d3b475c644b61395f"
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
fn an_amount_missing_the_note_fee_is_returned() {
    assert_refused(&mut vault(), TOKEN, "alice.test", ONE, &msg(1));
}

#[test]
fn an_amount_above_denomination_plus_fee_is_returned() {
    assert_refused(
        &mut vault(),
        TOKEN,
        "alice.test",
        ONE + NOTE_FEE + 1,
        &msg(1),
    );
}

#[test]
fn a_zero_amount_is_returned() {
    assert_refused(&mut vault(), TOKEN, "alice.test", 0, &msg(1));
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
    let modulus = word::encode32(&SCALAR_MODULUS_BE);
    let mut below = SCALAR_MODULUS_BE;
    below[31] -= 1;
    for bad in [modulus, word::encode32(&[0xff; 32])] {
        let m = format!(r#"{{"commitment":"{bad}"}}"#);
        assert_refused(&mut v, TOKEN, "alice.test", ONE + NOTE_FEE, &m);
    }
    // The largest field element is fine.
    let m = format!(r#"{{"commitment":"{}"}}"#, word::encode32(&below));
    assert_eq!(deposit(&mut v, "alice.test", ONE + NOTE_FEE, &m), 0);
}

#[test]
fn a_repeated_commitment_is_returned() {
    let mut v = vault();
    deposit_ok(&mut v, 1);
    assert_refused(&mut v, TOKEN, "bob.test", ONE + NOTE_FEE, &msg(1));
    assert_refused(&mut v, TOKEN, "bob.test", FIVE + NOTE_FEE, &msg(1));
}

fn assert_named_payout_refused(payout: &str) {
    let m = format!(
        r#"{{"commitment":"{}","withdraw_to":"{payout}"}}"#,
        commitment(1)
    );
    assert_refused(&mut vault(), TOKEN, "alice.test", ONE + NOTE_FEE, &m);
}

#[test]
fn a_payout_account_equal_to_the_vault_is_returned() {
    assert_named_payout_refused(VAULT);
}

#[test]
fn a_payout_account_equal_to_the_token_is_returned() {
    assert_named_payout_refused(TOKEN);
}

#[test]
fn a_payout_account_equal_to_intents_is_returned() {
    assert_named_payout_refused("intents.near");
}

#[test]
fn a_sender_that_cannot_be_paid_is_returned_when_no_payout_is_named() {
    let mut v = vault();
    // `intents.near` is not refused: its deposits take the Intents branch (see `tests::intents`).
    for sender in [VAULT, TOKEN] {
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
    let empty_root = word::encode32(&zero_hashes::ZERO_HASHES[32]);
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
        assert_eq!(v.current_root(), word::encode32(&reference.root().0));
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

// retire_closed_leaf

/// The siblings `retire_closed_leaf` needs for note `id`, as a client reads them off its rebuild.
fn siblings_of(v: &Vault, id: u32) -> Vec<String> {
    let level = tree::retire_level(id, v.leaf_count);
    let tree = MerkleTree::from_leaves(&leaves(v));
    tree.get_siblings(id)[..level]
        .iter()
        .map(|s| word::encode32(&s.0))
        .collect()
}

fn retire(v: &mut Vault, id: u32) {
    let siblings = siblings_of(v, id);
    testing_env!(context("anyone.test", T0 + 1_000).build());
    v.retire_closed_leaf(id, siblings);
}

/// Deposits and syncs `n` notes.
fn synced(n: u64) -> Vault {
    let mut v = roomy_vault();
    for i in 1..=n {
        deposit_ok(&mut v, i);
    }
    assert_eq!(sync(&mut v, T0 + 30, 120), n as u32);
    v
}

/// What `mutual_close` does to the note and the counters, without the proof.
fn close_in_place(v: &mut Vault, id: u32) {
    let note = v.notes.get_mut(id).unwrap();
    let deposit = note.deposit_amount;
    let was = note.status;
    note.status = Status::Closed;
    if was == Status::Active {
        v.open_notes -= 1;
    }
    if was != Status::Closed {
        v.tvl -= deposit + NOTE_FEE;
    }
}

#[test]
fn a_retirement_zeroes_the_leaf_and_moves_the_root() {
    let mut v = synced(5);
    close_in_place(&mut v, 1);
    let before = v.current_root();
    retire(&mut v, 1);
    assert_ne!(v.current_root(), before);
    assert_eq!(client_root(&v), v.current_root());
    assert_eq!(v.get_leaves(1, 1), vec![commitment(0)]);
    assert!(v.note_records(1, 1)[0].leaf_retired);
    assert_eq!(v.retirement_epoch(), U64(1));
    assert!(v.is_known_root(before));
    assert!(v.is_known_root(v.current_root()));
    let logs = get_logs();
    assert_eq!(logs.len(), 1);
    assert!(logs[0].contains(r#""event":"retire_closed_leaf""#));
    assert!(logs[0].contains(r#""note_id":1"#));
}

fn assert_retire_refused(v: &mut Vault, id: u32, siblings: Vec<String>, message: &str) {
    let root = v.current_root();
    let epoch = v.retirement_epoch();
    testing_env!(context("anyone.test", T0 + 1_000).build());
    let err = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        v.retire_closed_leaf(id, siblings)
    }))
    .expect_err("must refuse");
    let text = err
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| err.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default();
    assert!(text.contains(message), "{text}");
    assert_eq!(v.current_root(), root);
    assert_eq!(v.retirement_epoch(), epoch);
}

#[test]
fn retirement_refuses_a_note_that_is_not_closed() {
    let mut v = synced(5);
    let siblings = siblings_of(&v, 1);
    assert_retire_refused(&mut v, 1, siblings.clone(), "note is not closed");
    v.notes.get_mut(1).unwrap().status = Status::PendingExit;
    assert_retire_refused(&mut v, 1, siblings, "note is not closed");
}

#[test]
fn retirement_refuses_an_unknown_note_and_one_not_yet_in_the_tree() {
    let mut v = synced(4);
    assert_retire_refused(&mut v, 9, vec![], "unknown note");
    deposit_ok(&mut v, 99);
    close_in_place(&mut v, 4);
    assert_retire_refused(&mut v, 4, vec![], "leaf is not in the tree yet");
}

#[test]
fn retirement_refuses_a_second_retirement_and_a_leaf_zeroed_at_sync() {
    let mut v = synced(5);
    close_in_place(&mut v, 1);
    retire(&mut v, 1);
    let siblings = siblings_of(&v, 1);
    assert_retire_refused(&mut v, 1, siblings, "leaf already retired");
    // Closed before `sync_root` reached it: zeroed there, nothing left to retire.
    deposit_ok(&mut v, 50);
    close_in_place(&mut v, 5);
    assert_eq!(sync(&mut v, T0 + 100, 120), 1);
    assert_retire_refused(&mut v, 5, vec![], "leaf already retired");
}

#[test]
fn retirement_refuses_siblings_that_do_not_match_the_tree() {
    let mut v = synced(6);
    close_in_place(&mut v, 0);
    let good = siblings_of(&v, 0);
    assert_eq!(good.len(), 2);
    assert_retire_refused(&mut v, 0, vec![], "wrong number of siblings");
    let mut long = good.clone();
    long.push(good[0].clone());
    assert_retire_refused(&mut v, 0, long, "wrong number of siblings");
    for position in 0..good.len() {
        let mut wrong = good.clone();
        wrong[position] = commitment(777);
        assert_retire_refused(&mut v, 0, wrong, "siblings do not match the tree");
    }
    let mut swapped = good.clone();
    swapped.swap(0, 1);
    assert_retire_refused(&mut v, 0, swapped, "siblings do not match the tree");
    let mut malformed = good.clone();
    malformed[0] = "0x12".to_owned();
    assert_retire_refused(&mut v, 0, malformed, "bad sibling");
    let mut above_modulus = good;
    above_modulus[0] = word::encode32(&SCALAR_MODULUS_BE);
    assert_retire_refused(&mut v, 0, above_modulus, "bad sibling");
}

#[test]
fn siblings_of_another_leaf_do_not_retire_this_one() {
    let mut v = synced(8);
    close_in_place(&mut v, 2);
    let other = siblings_of(&v, 5);
    assert_retire_refused(&mut v, 2, other, "siblings do not match the tree");
}

/// Retire `index` of `first` synced notes, then sync `more` new ones and compare with a rebuild.
fn retire_then_sync(first: u64, index: u32, more: u64) {
    let mut v = synced(first);
    close_in_place(&mut v, index);
    retire(&mut v, index);
    assert_eq!(client_root(&v), v.current_root(), "{first} {index}");
    for n in 0..more {
        deposit_ok(&mut v, 1_000 + n);
    }
    sync(&mut v, T0 + 10_000, 120);
    assert_eq!(v.leaf_count(), (first + more) as u32);
    assert_eq!(client_root(&v), v.current_root(), "{first} {index} {more}");
}

#[test]
fn a_retired_leaf_in_every_frontier_position_gives_the_client_root_after_sync() {
    // (inserted, retired index): index 0, odd, even, the last inserted (odd and even count),
    // and the powers of two.
    let cases = [
        (1, 0),
        (2, 0),
        (5, 0),
        (6, 3),
        (7, 4),
        (8, 7),
        (9, 8),
        (9, 4),
        (9, 2),
        (9, 1),
        (16, 15),
        (17, 16),
        (17, 8),
        (17, 4),
        (20, 15),
        (20, 19),
        (21, 20),
        (33, 32),
        (33, 16),
    ];
    for (first, index) in cases {
        for more in [1, 2, 3, 8, 13] {
            retire_then_sync(first, index, more);
        }
    }
}

#[test]
fn retire_and_sync_root_interleaved_keep_the_client_root() {
    let mut v = roomy_vault();
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut random = move |bound: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound
    };
    let mut next = 1;
    let mut clock = T0;
    let mut retired = 0;
    for _ in 0..90 {
        match random(4) {
            0 | 1 => {
                for _ in 0..=random(5) {
                    deposit_ok(&mut v, next);
                    next += 1;
                }
            }
            2 => {
                clock += 30;
                sync(&mut v, clock, random(9) as u32 + 1);
            }
            _ => {
                let inserted: Vec<u32> = (0..v.leaf_count)
                    .filter(|id| !v.notes.get(*id).unwrap().leaf_retired)
                    .collect();
                if inserted.is_empty() {
                    continue;
                }
                let id = inserted[random(inserted.len() as u64) as usize];
                close_in_place(&mut v, id);
                retire(&mut v, id);
                retired += 1;
            }
        }
        assert_eq!(client_root(&v), v.current_root());
    }
    assert!(retired > 5 && v.leaf_count() > 20);
    assert_eq!(v.retirement_epoch(), U64(retired));
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
        word::encode32(&v.notes.get(5).unwrap().leaf)
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
    assert_eq!(word::decode32(&word::encode32(&word)), Some(word));
    assert_eq!(
        word::decode32(&word::encode32(&word).to_uppercase().replace("0X", "0x")),
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
        assert_eq!(word::decode32(bad), None, "{bad}");
    }
}

// Mutual close

mod exit;
mod expiry;
mod intents;
mod params;

// The promises these calls return are scheduled when they drop, which is what the tests read.
#[allow(unused_must_use)]
mod close {
    use std::sync::OnceLock;

    use near_sdk::test_utils::get_created_receipts;
    use near_sdk::{Gas, PromiseResult};
    use nymira_near_codec::Network;
    use nymira_testkit::proofs::{withdrawal_index as w, Bundle, ProofGen, WithdrawalKindId};
    use zkapi_proof::dev::{DevNote, DevRequest, DevState};

    use super::*;

    pub(super) const ALICE: &str = "alice.test";
    pub(super) const OPERATOR: &str = "operator.test";
    pub(super) const CLOSE_AT: u64 = T0 + 60;
    pub(super) const VALID_UNTIL: u64 = T0 + 3_600;
    pub(super) const CHARGE: u128 = 20_000;

    pub(super) fn gen() -> &'static ProofGen {
        static GEN: OnceLock<ProofGen> = OnceLock::new();
        GEN.get_or_init(|| ProofGen::new(Network::Testnet, VAULT).unwrap())
    }

    /// Note 0 as the vault assigns it when alice deposits 1 USDC at `T0`.
    pub(super) fn note_with(secret: u64, note_id: u32) -> DevNote {
        DevNote {
            secret: Felt252::from_u64(secret),
            note_id,
            deposit_amount: ONE,
            expiry: end_of_utc_day(T0 + 30 * DAY).unwrap(),
        }
    }

    pub(super) fn note() -> DevNote {
        note_with(42, 0)
    }

    /// The state after one request that charged `CHARGE`.
    pub(super) fn state(note: &DevNote) -> DevState {
        state_with(gen(), note)
    }

    pub(super) fn state_with(gen: &ProofGen, note: &DevNote) -> DevState {
        let s0 = note.genesis(Felt252::from_u64(7));
        gen.next_state(
            &s0,
            &DevRequest {
                request_time: T0 + 10,
                solvency_bound: 100_000,
                request_context: Felt252::from_u64(99),
                rerandomization: Felt252::from_u64(9),
            },
            CHARGE,
            &Felt252::from_u64(17),
        )
        .unwrap()
    }

    /// A kind-1 proof of note 0's state, paying `ALICE`, valid until `VALID_UNTIL`.
    pub(super) fn bundle() -> &'static Bundle {
        static BUNDLE: OnceLock<Bundle> = OnceLock::new();
        BUNDLE.get_or_init(|| {
            gen()
                .withdrawal(
                    &state(&note()),
                    WithdrawalKindId::MutualClose,
                    ALICE,
                    0,
                    VALID_UNTIL,
                )
                .unwrap()
        })
    }

    /// A vault holding note 0, deposited by alice at `T0`.
    pub(super) fn funded() -> Vault {
        let mut v = vault();
        at(T0);
        let msg = format!(
            r#"{{"commitment":"{}"}}"#,
            word::encode32(&note().registration().0)
        );
        assert_eq!(deposit(&mut v, ALICE, ONE + NOTE_FEE, &msg), 0);
        v
    }

    pub(super) fn call(
        v: &mut Vault,
        caller: &str,
        secs: u64,
        bundle: &Bundle,
    ) -> PromiseOrValue<()> {
        testing_env!(context(caller, secs).build());
        v.mutual_close(
            Base64VecU8(bundle.proof_wire.clone()),
            bundle
                .inputs_wire
                .iter()
                .map(|word| Base64VecU8(word.to_vec()))
                .collect(),
        )
    }

    pub(super) fn close(v: &mut Vault, caller: &str, bundle: &Bundle) -> PromiseOrValue<()> {
        call(v, caller, CLOSE_AT, bundle)
    }

    /// One line per created receipt action: receiver, method, args, deposit, gas, gas weight.
    pub(super) fn receipts() -> String {
        use near_sdk::mock::MockAction;
        let mut out = String::new();
        for receipt in get_created_receipts() {
            for action in &receipt.actions {
                if let MockAction::FunctionCallWeight {
                    method_name,
                    args,
                    attached_deposit,
                    prepaid_gas,
                    gas_weight,
                    ..
                } = action
                {
                    out.push_str(&format!(
                        "{} {} {} deposit={} gas={} weight={}\n",
                        receipt.receiver_id,
                        String::from_utf8_lossy(method_name),
                        String::from_utf8_lossy(args),
                        attached_deposit.as_yoctonear(),
                        prepaid_gas.as_gas(),
                        gas_weight.0
                    ));
                }
            }
        }
        out
    }

    #[test]
    fn a_user_close_pays_the_whole_balance_and_books_the_spent_part() {
        let mut v = funded();
        let result = close(&mut v, ALICE, bundle());
        assert!(matches!(result, PromiseOrValue::Promise(_)));
        // The promise is scheduled when dropped.
        drop(result);
        let logs = get_logs();
        assert!(logs[0].contains(r#""event":"mutual_close""#));
        assert!(logs[0].contains(&format!(r#""amount_paid":"{}""#, ONE - CHARGE)));
        assert!(logs[0].contains(r#""close_fee":"0""#));
        let all = receipts();
        let lines: Vec<&str> = all.lines().collect();
        assert_eq!(lines.len(), 3, "{all}");
        // Registration first (0.00125 NEAR, registration only), then the transfer, then the
        // callback; every leg has fixed gas and no leftover-gas weight.
        assert!(lines[0].starts_with("usdc.test storage_deposit"));
        assert!(lines[0].contains(r#""registration_only":true"#));
        assert!(lines[0].contains(r#""account_id":"alice.test""#));
        assert!(lines[0].contains(&format!(
            "deposit={REGISTRATION_DEPOSIT} gas={}",
            6_000_000_000_000u64
        )));
        assert!(lines[1].starts_with("usdc.test ft_transfer"));
        assert!(lines[1].contains(&format!(r#""amount":"{}""#, ONE - CHARGE)));
        assert!(lines[1].contains("deposit=1 gas=7000000000000"));
        assert!(lines[2].starts_with("vault.test on_close_payout"));
        assert!(lines[2].contains("gas=6000000000000 weight=0"));
        assert_eq!(v.note_status(commitment_of_note()), Some(Status::Closed));
        assert_eq!(v.open_notes(), 0);
        assert_eq!(v.config().current_tvl.0, 0);
        // The note fee at deposit plus the spent part.
        assert_eq!(v.treasury_balance().0, NOTE_FEE + CHARGE);
        assert!(v.is_nullifier_used(word::encode32(
            &bundle().inputs_wire[w::WITHDRAWAL_NULLIFIER]
        )));
        assert_counters(&v);
    }

    pub(super) fn commitment_of_note() -> String {
        word::encode32(&note().registration().0)
    }

    #[test]
    fn the_operator_close_takes_the_close_fee_from_the_payout() {
        let mut v = funded();
        let _ = close(&mut v, OPERATOR, bundle());
        let logs = get_logs();
        assert!(logs[0].contains(&format!(r#""amount_paid":"{}""#, ONE - CHARGE - 30_000)));
        assert!(logs[0].contains(r#""close_fee":"30000""#));
        // Payout plus close fee plus treasury share is the deposit.
        assert_eq!(
            (ONE - CHARGE - 30_000) + 30_000 + (v.treasury_balance().0 - NOTE_FEE - 30_000),
            ONE
        );
        assert_eq!(v.treasury_balance().0, NOTE_FEE + CHARGE + 30_000);
    }

    #[test]
    fn a_fully_spent_note_closes_without_a_transfer() {
        let n = note();
        let s0 = n.genesis(Felt252::from_u64(7));
        let spent = gen()
            .next_state(
                &s0,
                &DevRequest {
                    request_time: T0 + 10,
                    solvency_bound: ONE,
                    request_context: Felt252::from_u64(99),
                    rerandomization: Felt252::from_u64(9),
                },
                ONE,
                &Felt252::from_u64(17),
            )
            .unwrap();
        let b = gen()
            .withdrawal(&spent, WithdrawalKindId::MutualClose, ALICE, 0, VALID_UNTIL)
            .unwrap();
        let mut v = funded();
        let result = close(&mut v, OPERATOR, &b);
        assert!(matches!(result, PromiseOrValue::Value(())));
        assert!(!receipts().contains("ft_transfer"));
        assert!(get_logs()[0].contains(r#""amount_paid":"0""#));
        assert!(get_logs()[0].contains(r#""close_fee":"0""#));
        assert_eq!(v.treasury_balance().0, NOTE_FEE + ONE);
    }

    #[test]
    fn the_close_fee_never_exceeds_the_balance() {
        let n = note();
        let s0 = n.genesis(Felt252::from_u64(7));
        let left = 10_000u128;
        let spent = gen()
            .next_state(
                &s0,
                &DevRequest {
                    request_time: T0 + 10,
                    solvency_bound: ONE,
                    request_context: Felt252::from_u64(99),
                    rerandomization: Felt252::from_u64(9),
                },
                ONE - left,
                &Felt252::from_u64(17),
            )
            .unwrap();
        let b = gen()
            .withdrawal(&spent, WithdrawalKindId::MutualClose, ALICE, 0, VALID_UNTIL)
            .unwrap();
        let mut v = funded();
        let result = close(&mut v, OPERATOR, &b);
        assert!(matches!(result, PromiseOrValue::Value(())));
        assert!(get_logs()[0].contains(r#""close_fee":"10000""#));
        assert_eq!(v.treasury_balance().0, NOTE_FEE + ONE);
    }

    fn assert_close_refused(message: &str, edit: impl FnOnce(&mut Vault) -> Bundle) {
        let mut v = funded();
        let b = edit(&mut v);
        let status = v.note_status(commitment_of_note());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = close(&mut v, ALICE, &b);
        }));
        let error = result.expect_err("the close must panic");
        let text = error
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| error.downcast_ref::<&str>().map(|s| s.to_string()))
            .unwrap_or_default();
        assert!(text.contains(message), "expected {message:?}, got {text:?}");
        // A refused close changes nothing.
        assert_eq!(v.note_status(commitment_of_note()), status);
        assert_eq!(v.treasury_balance().0, NOTE_FEE);
        if status == Some(Status::Active) {
            assert_counters(&v);
        }
    }

    fn forged(index: usize, word: [u8; 32]) -> Bundle {
        bundle().with_input(index, word)
    }

    fn felt(value: u128) -> [u8; 32] {
        Felt252::from_u128(value).0
    }

    #[test]
    fn a_kind_other_than_one_is_refused() {
        assert_close_refused("not a mutual close", |_| {
            gen()
                .withdrawal(
                    &state(&note()),
                    WithdrawalKindId::Escape,
                    ALICE,
                    0,
                    VALID_UNTIL,
                )
                .unwrap()
        });
        assert_close_refused("not a mutual close", |_| forged(w::KIND, felt(2)));
    }

    #[test]
    fn a_wrong_protocol_version_is_refused() {
        assert_close_refused("wrong protocol version", |_| {
            forged(w::PROTOCOL_VERSION, felt(3))
        });
    }

    #[test]
    fn a_wrong_chain_id_is_refused() {
        assert_close_refused("wrong chain id", |_| {
            forged(w::CHAIN_ID, felt(u128::from(chain_id(Network::Mainnet))))
        });
    }

    #[test]
    fn a_proof_made_for_another_vault_is_refused() {
        assert_close_refused("wrong contract address", |_| {
            let other = ProofGen::new(Network::Testnet, "other.test").unwrap();
            other
                .withdrawal(
                    &state_with(&other, &note()),
                    WithdrawalKindId::MutualClose,
                    ALICE,
                    0,
                    VALID_UNTIL,
                )
                .unwrap()
        });
    }

    #[test]
    fn a_wrong_state_key_is_refused() {
        let other = signing_key(9);
        assert_close_refused("wrong state key", |_| forged(w::STATE_KEY_X, other.x.0));
        assert_close_refused("wrong state key", |_| forged(w::STATE_KEY_Y, other.y.0));
    }

    #[test]
    fn a_wrong_clearance_key_x_is_refused() {
        assert_close_refused("wrong clearance key", |_| {
            forged(w::CLEARANCE_KEY_X, signing_key(9).x.0)
        });
    }

    #[test]
    fn a_wrong_clearance_key_y_is_refused() {
        assert_close_refused("wrong clearance key", |_| {
            forged(w::CLEARANCE_KEY_Y, signing_key(9).y.0)
        });
    }

    #[test]
    fn a_proof_signed_with_a_different_clearance_key_is_refused() {
        assert_close_refused("wrong clearance key", |_| {
            gen()
                .withdrawal_with_clearance_seed(&state(&note()), ALICE, 0, VALID_UNTIL, 9)
                .unwrap()
        });
    }

    #[test]
    fn a_closed_note_cannot_close_again() {
        assert_close_refused("note is not active", |v| {
            v.notes.get_mut(0).unwrap().status = Status::Closed;
            bundle().clone()
        });
    }

    #[test]
    fn a_swept_note_cannot_close() {
        // A note swept by `claim_expired` is `Closed` like any other.
        assert_close_refused("note is not active", |v| {
            let note = v.notes.get_mut(0).unwrap();
            note.status = Status::Closed;
            note.leaf_retired = true;
            bundle().clone()
        });
    }

    #[test]
    fn a_pending_exit_note_cannot_close() {
        assert_close_refused("note is not active", |v| {
            v.notes.get_mut(0).unwrap().status = Status::PendingExit;
            bundle().clone()
        });
    }

    #[test]
    fn a_proof_for_an_unknown_note_is_refused() {
        assert_close_refused("unknown note", |_| {
            let mut fields = zkapi_core::v2::note_fields(5, ONE, note().expiry);
            fields.0[31] ^= 0;
            forged(w::NOTE_FIELDS, fields.0)
        });
    }

    #[test]
    fn forged_note_fields_are_refused() {
        // At or above 2^224.
        assert_close_refused("invalid noteFields", |_| {
            let mut word = bundle().inputs_wire[w::NOTE_FIELDS];
            word[3] = 1;
            forged(w::NOTE_FIELDS, word)
        });
        // Same id, another expiry.
        assert_close_refused("note fields do not match", |_| {
            forged(
                w::NOTE_FIELDS,
                zkapi_core::v2::note_fields(0, ONE, note().expiry + 1).0,
            )
        });
    }

    #[test]
    fn the_right_leaf_with_a_wrong_deposit_is_refused() {
        assert_close_refused("note fields do not match", |_| {
            forged(
                w::NOTE_FIELDS,
                zkapi_core::v2::note_fields(0, ONE + 1, note().expiry).0,
            )
        });
    }

    #[test]
    fn the_right_note_id_with_a_wrong_leaf_is_refused() {
        // A valid proof of another note (another secret) that has the same id.
        assert_close_refused("wrong note leaf", |_| {
            let other = note_with(43, 0);
            gen()
                .withdrawal(
                    &state(&other),
                    WithdrawalKindId::MutualClose,
                    ALICE,
                    0,
                    VALID_UNTIL,
                )
                .unwrap()
        });
    }

    #[test]
    fn a_final_balance_above_the_deposit_is_refused() {
        assert_close_refused("final balance above the deposit", |_| {
            forged(w::FINAL_BALANCE, felt(ONE + 1))
        });
    }

    #[test]
    fn a_final_balance_that_is_not_a_u128_is_refused() {
        assert_close_refused("final balance out of range", |_| {
            let mut word = [0u8; 32];
            word[0] = 1;
            forged(w::FINAL_BALANCE, word)
        });
    }

    #[test]
    fn a_destination_other_than_the_payout_account_is_refused() {
        assert_close_refused("destination is not the payout account", |_| {
            gen()
                .withdrawal(
                    &state(&note()),
                    WithdrawalKindId::MutualClose,
                    "mallory.test",
                    0,
                    VALID_UNTIL,
                )
                .unwrap()
        });
    }

    #[test]
    fn valid_until_is_inclusive() {
        let mut v = funded();
        let _ = call(&mut v, ALICE, VALID_UNTIL, bundle());
        assert_eq!(v.note_status(commitment_of_note()), Some(Status::Closed));
    }

    #[test]
    fn one_second_after_valid_until_is_refused() {
        let mut v = funded();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = call(&mut v, ALICE, VALID_UNTIL + 1, bundle());
        }));
        assert!(result.is_err());
        assert_eq!(v.note_status(commitment_of_note()), Some(Status::Active));
    }

    #[test]
    fn a_public_input_at_the_field_modulus_is_refused() {
        assert_close_refused("not a canonical field element", |_| {
            forged(w::VALID_UNTIL, SCALAR_MODULUS_BE)
        });
    }

    #[test]
    fn the_wrong_number_of_inputs_is_refused() {
        let mut v = funded();
        testing_env!(context(ALICE, CLOSE_AT).build());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            v.mutual_close(
                Base64VecU8(bundle().proof_wire.clone()),
                bundle()
                    .inputs_wire
                    .iter()
                    .take(15)
                    .map(|w| Base64VecU8(w.to_vec()))
                    .collect(),
            )
        }));
        assert!(result.is_err());
    }

    #[test]
    fn a_proof_that_does_not_match_the_inputs_is_refused() {
        // A lower final balance with the proof of the higher one.
        assert_close_refused("invalid proof", |_| {
            forged(w::FINAL_BALANCE, felt(ONE - CHARGE - 1))
        });
    }

    #[test]
    fn a_spent_nullifier_is_refused_even_with_a_fresh_proof() {
        let mut v = funded();
        let _ = close(&mut v, ALICE, bundle());
        // Reopen the note so only the nullifier stands in the way, then present another proof
        // of the same state (a different random proof).
        v.notes.get_mut(0).unwrap().status = Status::Active;
        let again = gen()
            .withdrawal(
                &state(&note()),
                WithdrawalKindId::MutualClose,
                ALICE,
                0,
                VALID_UNTIL,
            )
            .unwrap();
        assert_ne!(again.proof_wire, bundle().proof_wire);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _ = close(&mut v, ALICE, &again);
        }));
        assert!(result.is_err());
        assert!(v.is_nullifier_used(word::encode32(&again.inputs_wire[w::WITHDRAWAL_NULLIFIER])));
    }

    #[test]
    #[should_panic(expected = "not enough gas")]
    fn too_little_prepaid_gas_panics_before_any_write() {
        let mut v = funded();
        let mut c = context(ALICE, CLOSE_AT);
        c.prepaid_gas(Gas::from_tgas(79));
        testing_env!(c.build());
        v.mutual_close(
            Base64VecU8(bundle().proof_wire.clone()),
            bundle()
                .inputs_wire
                .iter()
                .map(|w| Base64VecU8(w.to_vec()))
                .collect(),
        );
    }

    #[test]
    fn the_nullifier_view_refuses_garbage() {
        let v = funded();
        assert!(!v.is_nullifier_used("garbage".to_owned()));
        assert!(!v.is_nullifier_used(commitment(77)));
    }

    #[test]
    fn the_protocol_version_constant_is_zkapis() {
        assert_eq!(PROTOCOL_VERSION, zkapi_types::PROTOCOL_VERSION);
    }

    // Payout callbacks and claims

    pub(super) fn callback_env(predecessor: &str, result: PromiseResult) {
        testing_env!(
            context(predecessor, CLOSE_AT).build(),
            near_sdk::test_vm_config(),
            near_sdk::RuntimeFeesConfig::test(),
            Default::default(),
            vec![result]
        );
    }

    pub(super) fn account_id(name: &str) -> AccountId {
        name.parse().unwrap()
    }

    #[test]
    fn a_successful_transfer_with_junk_bytes_creates_no_pending_payout() {
        let mut v = funded();
        callback_env(
            VAULT,
            PromiseResult::Successful(b"\xde\xad\xbe\xefjunk".to_vec()),
        );
        v.on_close_payout(account_id(ALICE), U128(900_000), 0, commitment_of_note());
        assert_eq!(v.pending_payout(account_id(ALICE)).0, 0);
        assert!(get_logs().is_empty());
    }

    #[test]
    fn a_failed_transfer_creates_exactly_one_pending_payout() {
        let mut v = funded();
        callback_env(VAULT, PromiseResult::Failed);
        v.on_close_payout(account_id(ALICE), U128(900_000), 0, commitment_of_note());
        assert_eq!(v.pending_payout(account_id(ALICE)).0, 900_000);
        let logs = get_logs();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].contains(r#""event":"payout_pending""#));
        assert!(logs[0].contains(r#""reason":"transfer_failed""#));
        assert!(logs[0].contains(r#""note_id":0"#));
    }

    #[test]
    fn a_later_failed_amount_adds_to_the_record_with_no_floor() {
        let mut v = funded();
        callback_env(VAULT, PromiseResult::Failed);
        v.on_close_payout(account_id(ALICE), U128(900_000), 0, commitment_of_note());
        callback_env(VAULT, PromiseResult::Failed);
        v.on_close_payout(account_id(ALICE), U128(7), 0, commitment_of_note());
        assert_eq!(v.pending_payout(account_id(ALICE)).0, 900_007);
    }

    #[test]
    fn a_failed_amount_below_the_note_fee_for_a_new_account_is_dust() {
        let mut v = funded();
        let before = v.treasury_balance().0;
        callback_env(VAULT, PromiseResult::Failed);
        v.on_close_payout(
            account_id(ALICE),
            U128(NOTE_FEE - 1),
            0,
            commitment_of_note(),
        );
        assert_eq!(v.pending_payout(account_id(ALICE)).0, 0);
        assert_eq!(v.treasury_balance().0, before + NOTE_FEE - 1);
        let logs = get_logs();
        assert!(logs[0].contains(r#""event":"dust""#));
        assert!(logs[0].contains(r#""reason":"below_note_fee""#));
    }

    #[test]
    fn a_failed_amount_of_exactly_the_note_fee_is_a_record() {
        let mut v = funded();
        callback_env(VAULT, PromiseResult::Failed);
        v.on_close_payout(account_id(ALICE), U128(NOTE_FEE), 0, commitment_of_note());
        assert_eq!(v.pending_payout(account_id(ALICE)).0, NOTE_FEE);
    }

    #[test]
    fn many_pending_accounts_do_not_block_a_new_one() {
        let mut v = funded();
        for n in 0..200 {
            callback_env(VAULT, PromiseResult::Failed);
            v.on_close_payout(
                account_id(&format!("user{n}.test")),
                U128(NOTE_FEE),
                0,
                commitment_of_note(),
            );
        }
        callback_env(VAULT, PromiseResult::Failed);
        v.on_close_payout(account_id(ALICE), U128(900_000), 0, commitment_of_note());
        assert_eq!(v.pending_payout(account_id(ALICE)).0, 900_000);
        assert_eq!(v.pending_payout(account_id("user7.test")).0, NOTE_FEE);
    }

    fn with_pending(amount: u128) -> Vault {
        let mut v = funded();
        callback_env(VAULT, PromiseResult::Failed);
        v.on_close_payout(account_id(ALICE), U128(amount), 0, commitment_of_note());
        v
    }

    #[test]
    fn anyone_can_claim_a_payout_and_it_goes_to_that_account_only() {
        let mut v = with_pending(900_000);
        testing_env!(context("stranger.test", CLOSE_AT).build());
        v.claim_payout(account_id(ALICE));
        assert_eq!(v.pending_payout(account_id(ALICE)).0, 0);
        let all = receipts();
        assert!(all.contains("ft_transfer") && all.contains("storage_deposit"));
        assert!(all.contains("alice.test"));
        assert!(!all.contains("stranger.test"));
        assert!(all.contains("on_claim_payout"));
    }

    #[test]
    #[should_panic(expected = "nothing pending")]
    fn claiming_with_nothing_pending_panics() {
        let mut v = funded();
        testing_env!(context("stranger.test", CLOSE_AT).build());
        v.claim_payout(account_id(ALICE));
    }

    #[test]
    #[should_panic(expected = "not enough gas")]
    fn claim_payout_with_too_little_gas_panics_before_any_write() {
        let mut v = with_pending(900_000);
        let mut c = context("stranger.test", CLOSE_AT);
        c.prepaid_gas(Gas::from_tgas(49));
        testing_env!(c.build());
        v.claim_payout(account_id(ALICE));
    }

    #[test]
    fn a_successful_claim_emits_payout_claimed() {
        let mut v = with_pending(900_000);
        testing_env!(context("stranger.test", CLOSE_AT).build());
        v.claim_payout(account_id(ALICE));
        callback_env(VAULT, PromiseResult::Successful(vec![]));
        v.on_claim_payout(account_id(ALICE), U128(900_000));
        assert_eq!(v.pending_payout(account_id(ALICE)).0, 0);
        assert!(get_logs()[0].contains(r#""event":"payout_claimed""#));
    }

    #[test]
    fn a_failed_claim_restores_the_amount_even_after_a_new_record_appeared() {
        let mut v = with_pending(900_000);
        testing_env!(context("stranger.test", CLOSE_AT).build());
        v.claim_payout(account_id(ALICE));
        // While the claim is in flight, a new failed payout re-creates a record.
        callback_env(VAULT, PromiseResult::Failed);
        v.on_close_payout(account_id(ALICE), U128(NOTE_FEE), 0, commitment_of_note());
        callback_env(VAULT, PromiseResult::Failed);
        v.on_claim_payout(account_id(ALICE), U128(900_000));
        assert_eq!(v.pending_payout(account_id(ALICE)).0, 900_000 + NOTE_FEE);
    }

    #[test]
    fn claim_treasury_pays_the_treasury_account_only_and_zeroes_the_balance() {
        let mut v = funded();
        assert_eq!(v.treasury_balance().0, NOTE_FEE);
        testing_env!(context("stranger.test", CLOSE_AT).build());
        v.claim_treasury();
        assert_eq!(v.treasury_balance().0, 0);
        let all = receipts();
        assert!(all.contains("treasury.test"));
        assert!(!all.contains("stranger.test"));
        assert!(all.contains("on_claim_treasury"));
    }

    #[test]
    #[should_panic(expected = "the treasury balance is empty")]
    fn claim_treasury_on_an_empty_balance_panics() {
        let mut v = vault();
        testing_env!(context("stranger.test", CLOSE_AT).build());
        v.claim_treasury();
    }

    #[test]
    #[should_panic(expected = "not enough gas")]
    fn claim_treasury_with_too_little_gas_panics_before_any_write() {
        let mut v = funded();
        let mut c = context("stranger.test", CLOSE_AT);
        c.prepaid_gas(Gas::from_tgas(49));
        testing_env!(c.build());
        v.claim_treasury();
    }

    #[test]
    fn a_failed_treasury_transfer_restores_the_balance_with_fees_that_accrued() {
        let mut v = funded();
        testing_env!(context("stranger.test", CLOSE_AT).build());
        v.claim_treasury();
        // A fee accrues while the transfer is in flight.
        at(T0 + 5);
        let msg = format!(r#"{{"commitment":"{}"}}"#, commitment(900));
        assert_eq!(deposit(&mut v, ALICE, ONE + NOTE_FEE, &msg), 0);
        callback_env(VAULT, PromiseResult::Failed);
        v.on_claim_treasury(U128(NOTE_FEE));
        assert_eq!(v.treasury_balance().0, 2 * NOTE_FEE);
    }

    #[test]
    fn a_successful_treasury_claim_emits_treasury_claimed() {
        let mut v = funded();
        testing_env!(context("stranger.test", CLOSE_AT).build());
        v.claim_treasury();
        callback_env(VAULT, PromiseResult::Successful(vec![]));
        v.on_claim_treasury(U128(NOTE_FEE));
        assert!(get_logs()[0].contains(r#""event":"treasury_claimed""#));
        assert_eq!(v.treasury_balance().0, 0);
    }
}
