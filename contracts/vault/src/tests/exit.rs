//! The exit game: `request_exit`, `defeat_exit`, `raise_concession`, `finalize_exit` and
//! `exit_requests`, with real proofs from the development setup.

// The promises these calls return are scheduled when they drop, which is what the tests read.
#![allow(unused_must_use)]

use std::sync::OnceLock;

use near_sdk::test_utils::get_created_receipts;
use near_sdk::{Gas, PromiseResult};
use nymira_testkit::proofs::{withdrawal_index as w, Bundle, WithdrawalKindId};
use zkapi_proof::dev::{tree_with, DevRequest, DevState};

use super::close::{
    account_id, bundle as close_bundle, commitment_of_note, funded, gen, note, receipts, ALICE,
    CHARGE, OPERATOR, VALID_UNTIL,
};
use super::*;

pub(super) const EXIT_AT: u64 = T0 + 60;
pub(super) const NS: u64 = 1_000_000_000;
pub(super) const DEADLINE: u64 = EXIT_AT * NS + 7 * DAY * NS;
/// The state after three charges: s1 (the escaped state) and its successors.
pub(super) const BALANCE_1: u128 = ONE - CHARGE;

pub(super) struct Fixture {
    pub(super) states: Vec<DevState>,
    /// Kind-0 proofs of s0, s1 and s2, paying `ALICE`, no concession.
    pub(super) escape: [Bundle; 3],
    /// Request proofs at the successor nullifier of s1 and of s2: they defeat the exit of s1, s2.
    pub(super) defeat: [Bundle; 2],
    /// Request proofs at s1's nullifier with solvency bounds 100_000, 50_000 and 30_000.
    pub(super) raise: [Bundle; 3],
}

fn request(bound: u128) -> DevRequest {
    DevRequest {
        request_time: T0 + 10,
        solvency_bound: bound,
        request_context: Felt252::from_u64(99),
        rerandomization: Felt252::from_u64(9),
    }
}

pub(super) fn fixture() -> &'static Fixture {
    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    FIXTURE.get_or_init(|| {
        let g = gen();
        let n = note();
        let tree = tree_with(&[&n]);
        let s0 = n.genesis(Felt252::from_u64(7));
        let s1 = g
            .next_state(&s0, &request(100_000), CHARGE, &Felt252::from_u64(17))
            .unwrap();
        let s2 = g
            .next_state(&s1, &request(100_000), 30_000, &Felt252::from_u64(18))
            .unwrap();
        let s3 = g
            .next_state(&s2, &request(100_000), 10_000, &Felt252::from_u64(19))
            .unwrap();
        assert_eq!(s1.successor_nullifier(), s2.nullifier());
        assert_eq!(s2.successor_nullifier(), s3.nullifier());
        let escape = |s: &DevState| {
            g.withdrawal(s, WithdrawalKindId::Escape, ALICE, 0, VALID_UNTIL)
                .unwrap()
        };
        let at = |s: &DevState, bound: u128| g.request(s, &tree, &request(bound)).unwrap();
        Fixture {
            escape: [escape(&s0), escape(&s1), escape(&s2)],
            defeat: [at(&s2, 100_000), at(&s3, 100_000)],
            raise: [at(&s1, 100_000), at(&s1, 50_000), at(&s1, 30_000)],
            states: vec![s0, s1, s2, s3],
        }
    })
}

pub(super) fn args(b: &Bundle) -> (Base64VecU8, Vec<Base64VecU8>) {
    (
        Base64VecU8(b.proof_wire.clone()),
        b.inputs_wire
            .iter()
            .map(|word| Base64VecU8(word.to_vec()))
            .collect(),
    )
}

pub(super) fn ctx(predecessor: &str, ns: u64) -> VMContextBuilder {
    let mut builder = context(predecessor, 0);
    builder.block_timestamp(ns);
    builder
}

pub(super) fn request_exit(v: &mut Vault, ns: u64, b: &Bundle) {
    testing_env!(ctx("anyone.test", ns).build());
    let (proof, inputs) = args(b);
    v.request_exit(proof, inputs);
}

pub(super) fn defeat(v: &mut Vault, caller: &str, ns: u64, id: u32, b: &Bundle) {
    testing_env!(ctx(caller, ns).build());
    let (proof, inputs) = args(b);
    v.defeat_exit(id, proof, inputs);
}

pub(super) fn raise(v: &mut Vault, ns: u64, id: u32, b: &Bundle, amount: u128) {
    testing_env!(ctx("anyone.test", ns).build());
    let (proof, inputs) = args(b);
    v.raise_concession(id, proof, inputs, U128(amount));
}

pub(super) fn finalize(v: &mut Vault, ns: u64, id: u32) -> PromiseOrValue<()> {
    testing_env!(ctx("anyone.test", ns).build());
    v.finalize_exit(id)
}

/// The text of the panic `f` raises.
pub(super) fn refusal(f: impl FnOnce()) -> String {
    let error =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).expect_err("the call must panic");
    error
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| error.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_default()
}

pub(super) fn assert_refused(text: String, message: &str) {
    assert!(text.contains(message), "expected {message:?}, got {text:?}");
}

/// A vault with note 0 in `PendingExit` through the real `request_exit` of s1.
pub(super) fn pending() -> Vault {
    let mut v = funded();
    request_exit(&mut v, EXIT_AT * NS, &fixture().escape[1]);
    v
}

pub(super) fn only_exit(v: &Vault) -> ExitRequest {
    let mut all = v.exit_requests(0, 100);
    assert_eq!(all.len(), 1);
    all.remove(0)
}

fn word_of(felt: Felt252) -> String {
    word::encode32(&felt.0)
}

/// A pending exit written straight into the state (no proof), for accounting and paging tests.
pub(super) fn inject(v: &mut Vault, id: u32, final_balance: u128, conceded: u128, deadline: u64) {
    let note = v.notes.get_mut(id).unwrap();
    assert_eq!(note.status, Status::Active);
    note.status = Status::PendingExit;
    v.open_notes -= 1;
    v.add_exit(
        id,
        ExitRecord {
            nullifier: [id as u8 + 1; 32],
            next_nullifier: [id as u8 + 101; 32],
            final_balance,
            conceded,
            deadline,
        },
    );
}

// request_exit

#[test]
fn request_exit_records_the_exit_and_spends_both_nullifiers() {
    let f = fixture();
    let v = pending();
    let exit = only_exit(&v);
    assert_eq!(exit.note_id, 0);
    assert_eq!(exit.nullifier, word_of(f.states[1].nullifier()));
    assert_eq!(
        exit.next_nullifier,
        word_of(f.states[1].successor_nullifier())
    );
    assert_eq!(exit.final_balance.0, BALANCE_1);
    assert_eq!(exit.conceded.0, 0);
    assert_eq!(exit.deadline.0, DEADLINE);
    assert_eq!(
        v.note_status(commitment_of_note()),
        Some(Status::PendingExit)
    );
    assert!(v.is_nullifier_used(exit.nullifier.clone()));
    assert!(v.is_nullifier_used(exit.next_nullifier.clone()));
    // Pending exit notes stay in the TVL and leave `open_notes`.
    assert_eq!(v.open_notes(), 0);
    assert_eq!(v.config().current_tvl.0, ONE + NOTE_FEE);
    assert_eq!(v.notes.get(0).unwrap().last_exit_ns, EXIT_AT * NS);
    let logs = get_logs();
    assert_eq!(logs.len(), 1);
    assert!(logs[0].contains(r#""event":"request_exit""#));
    assert!(logs[0].contains(&format!(r#""deadline":"{DEADLINE}""#)));
    assert!(logs[0].contains(&format!(r#""final_balance":"{BALANCE_1}""#)));
    assert!(logs[0].contains(r#""conceded":"0""#));
    assert_counters(&v);
}

#[test]
fn request_exit_makes_no_usdc_call() {
    let v = pending();
    assert!(get_created_receipts().is_empty());
    assert_eq!(v.treasury_balance().0, NOTE_FEE);
}

/// A funded vault, and the text `request_exit` panics with for `b`.
fn exit_refusal(v: &mut Vault, ns: u64, b: &Bundle) -> String {
    refusal(|| request_exit(v, ns, b))
}

fn assert_exit_refused(message: &str, b: &Bundle) {
    let mut v = funded();
    assert_refused(exit_refusal(&mut v, EXIT_AT * NS, b), message);
    assert_eq!(v.note_status(commitment_of_note()), Some(Status::Active));
    assert!(v.exit_ids.is_empty());
    assert_counters(&v);
}

fn forged(b: &Bundle, index: usize, word: [u8; 32]) -> Bundle {
    b.with_input(index, word)
}

#[test]
fn request_exit_refuses_a_wrong_clearance_key() {
    let e = &fixture().escape[1];
    let other = Felt252::from_u64(5).0;
    assert_exit_refused("wrong clearance key", &forged(e, w::CLEARANCE_KEY_X, other));
    assert_exit_refused("wrong clearance key", &forged(e, w::CLEARANCE_KEY_Y, other));
}

#[test]
fn request_exit_refuses_a_wrong_state_key_and_a_wrong_kind() {
    let e = &fixture().escape[1];
    assert_exit_refused(
        "wrong state key",
        &forged(e, w::STATE_KEY_X, Felt252::from_u64(5).0),
    );
    // A mutual close proof is not an exit request, and the other way round.
    assert_exit_refused("not an exit request", close_bundle());
    assert_exit_refused(
        "not an exit request",
        &forged(e, w::KIND, Felt252::from_u64(2).0),
    );
}

#[test]
fn request_exit_refuses_forged_note_data() {
    let e = &fixture().escape[1];
    let n = note();
    let fields = |id: u32, deposit: u128| zkapi_types::pack_note_fields(id, deposit, n.expiry).0;
    // A proof for note 1, which does not exist.
    assert_exit_refused("unknown note", &forged(e, w::NOTE_FIELDS, fields(1, ONE)));
    // The right note ID with a wrong deposit or a wrong leaf.
    assert_exit_refused(
        "note fields do not match",
        &forged(e, w::NOTE_FIELDS, fields(0, FIVE)),
    );
    assert_exit_refused(
        "wrong note leaf",
        &forged(e, w::NOTE_LEAF, Felt252::from_u64(5).0),
    );
    assert_exit_refused(
        "invalid proof",
        &forged(e, w::FINAL_BALANCE, Felt252::from_u128(BALANCE_1 + 1).0),
    );
}

#[test]
fn request_exit_refuses_a_balance_and_concession_above_the_deposit() {
    let e = &fixture().escape[1];
    assert_exit_refused(
        "balance and concession above the deposit",
        &forged(e, w::CONCEDED, Felt252::from_u128(ONE).0),
    );
    let mut huge = [0u8; 32];
    huge[0] = 1;
    assert_exit_refused("concession out of range", &forged(e, w::CONCEDED, huge));
}

#[test]
fn request_exit_refuses_a_repeating_successor_nullifier() {
    let e = &fixture().escape[1];
    let n = e.inputs_wire[w::WITHDRAWAL_NULLIFIER];
    assert_exit_refused(
        "successor nullifier repeats",
        &forged(e, w::NEXT_NULLIFIER, n),
    );
}

#[test]
fn request_exit_checks_valid_until_in_seconds() {
    let e = &fixture().escape[1];
    let mut v = funded();
    assert_refused(
        exit_refusal(&mut v, (VALID_UNTIL + 1) * NS, e),
        "proof expired",
    );
    // Accepted at the last second, even a fraction of a second into it.
    request_exit(&mut v, VALID_UNTIL * NS + NS - 1, e);
    assert_eq!(
        v.note_status(commitment_of_note()),
        Some(Status::PendingExit)
    );
}

#[test]
fn a_second_request_exit_is_refused_whatever_its_nullifiers() {
    let f = fixture();
    let mut v = pending();
    for b in [&f.escape[1], &f.escape[2], &f.escape[0]] {
        assert_refused(exit_refusal(&mut v, EXIT_AT * NS, b), "note is not active");
    }
    assert_eq!(v.exit_ids.len(), 1);
}

#[test]
fn request_exit_refuses_a_nullifier_that_is_spent_or_flagged() {
    let f = fixture();
    let mut v = funded();
    let n1 = f.states[1].nullifier().0;
    v.nullifiers.insert(f.states[1].successor_nullifier().0);
    assert_refused(
        exit_refusal(&mut v, EXIT_AT * NS, &f.escape[1]),
        "nullifier already used",
    );
    v.nullifiers.remove(&f.states[1].successor_nullifier().0);
    v.flagged.insert(n1);
    assert_refused(
        exit_refusal(&mut v, EXIT_AT * NS, &f.escape[1]),
        "nullifier is flagged",
    );
    // The successor alone being flagged is enough too.
    v.flagged.remove(&n1);
    v.flagged.insert(f.states[1].successor_nullifier().0);
    assert_refused(
        exit_refusal(&mut v, EXIT_AT * NS, &f.escape[1]),
        "nullifier is flagged",
    );
    assert!(v.exit_ids.is_empty());
}

#[test]
fn request_exit_refuses_a_closed_note_with_an_older_state() {
    let f = fixture();
    // Closed by a mutual close (the proof is of s1; s0's nullifier is still unused).
    let mut v = funded();
    testing_env!(context(ALICE, T0 + 60).build());
    let (proof, inputs) = args(close_bundle());
    drop(v.mutual_close(proof, inputs));
    assert_refused(
        exit_refusal(&mut v, EXIT_AT * NS, &f.escape[0]),
        "note is not active",
    );
    // Closed by `finalize_exit`.
    let mut v = pending();
    drop(finalize(&mut v, DEADLINE, 0));
    assert_refused(
        exit_refusal(&mut v, DEADLINE, &f.escape[0]),
        "note is not active",
    );
    assert_refused(
        exit_refusal(&mut v, DEADLINE, &f.escape[2]),
        "note is not active",
    );
    // Closed by `claim_expired`.
    let mut v = funded();
    let swept = (v.notes.get(0).unwrap().expiry + 7 * DAY) * NS;
    testing_env!(ctx("anyone.test", swept).build());
    v.claim_expired(0);
    assert_refused(
        exit_refusal(&mut v, swept, &f.escape[0]),
        "note is not active",
    );
}

// exit_requests

#[test]
fn exit_requests_pages_by_note_id_and_survives_a_deletion_between_pages() {
    let mut v = roomy_vault();
    for n in 1..=5 {
        deposit_ok(&mut v, n);
    }
    for id in 0..5 {
        inject(&mut v, id, 500_000, 0, 0);
    }
    let first = v.exit_requests(0, 2);
    assert_eq!(first.iter().map(|e| e.note_id).collect::<Vec<_>>(), [0, 1]);
    // Finalizing note 1 (and defeating nothing) between the two pages removes a record.
    drop(finalize(&mut v, 1, 1));
    let second = v.exit_requests(first.last().unwrap().note_id + 1, 2);
    assert_eq!(second.iter().map(|e| e.note_id).collect::<Vec<_>>(), [2, 3]);
    let third = v.exit_requests(4, 2);
    assert_eq!(third.iter().map(|e| e.note_id).collect::<Vec<_>>(), [4]);
    // Deleting the record the next page starts at skips nothing either.
    drop(finalize(&mut v, 2, 2));
    assert_eq!(
        v.exit_requests(2, 10)
            .iter()
            .map(|e| e.note_id)
            .collect::<Vec<_>>(),
        [3, 4]
    );
}

#[test]
fn exit_requests_caps_the_limit_at_one_hundred() {
    let mut v = roomy_vault();
    for n in 1..=105 {
        deposit_ok(&mut v, n);
    }
    for id in 0..105 {
        inject(&mut v, id, 1, 0, 0);
    }
    assert_eq!(v.exit_requests(0, 1_000).len(), 100);
    assert_eq!(v.exit_requests(100, 1_000).len(), 5);
    assert!(v.exit_requests(105, 10).is_empty());
    assert!(v.exit_requests(u32::MAX, u32::MAX).is_empty());
}

// defeat_exit

#[test]
fn defeat_frees_the_nullifiers_flags_the_escaped_one_and_reopens_the_note() {
    let f = fixture();
    let mut v = pending();
    let before = only_exit(&v);
    // Anyone, not only the operator, can defeat.
    defeat(&mut v, "stranger.test", EXIT_AT * NS + 5, 0, &f.defeat[0]);
    assert_eq!(v.note_status(commitment_of_note()), Some(Status::Active));
    assert!(v.exit_requests(0, 10).is_empty());
    assert!(!v.is_nullifier_used(before.nullifier.clone()));
    assert!(!v.is_nullifier_used(before.next_nullifier.clone()));
    assert!(v.flagged.contains(&f.states[1].nullifier().0));
    assert!(!v.flagged.contains(&f.states[1].successor_nullifier().0));
    assert_eq!(v.open_notes(), 1);
    assert_eq!(v.config().current_tvl.0, ONE + NOTE_FEE);
    assert_eq!(v.notes.get(0).unwrap().last_exit_ns, EXIT_AT * NS + 5);
    let logs = get_logs();
    assert!(logs[0].contains(r#""event":"defeat_exit""#));
    assert!(logs[0].contains(&format!(r#""flagged_nullifier":"{}""#, before.nullifier)));
    assert_counters(&v);
}

#[test]
fn after_a_defeat_the_old_state_is_dead_and_a_newer_one_can_exit() {
    let f = fixture();
    let mut v = pending();
    defeat(&mut v, ALICE, EXIT_AT * NS + 1, 0, &f.defeat[0]);
    // The escaped state cannot be filed again; s0 shares no nullifier but is older, still open.
    assert_refused(
        exit_refusal(&mut v, EXIT_AT * NS + 2, &f.escape[1]),
        "nullifier is flagged",
    );
    // A newer state works, with its own pair of nullifiers.
    request_exit(&mut v, EXIT_AT * NS + 3, &f.escape[2]);
    let exit = only_exit(&v);
    assert_eq!(exit.final_balance.0, BALANCE_1 - 30_000);
    assert_eq!(exit.deadline.0, EXIT_AT * NS + 3 + 7 * DAY * NS);
}

#[test]
fn defeat_needs_a_proof_at_the_successor_nullifier() {
    let f = fixture();
    let mut v = pending();
    // A proof at N_k (the escaped nullifier itself) cannot defeat.
    for b in &f.raise {
        assert_refused(
            refusal(|| defeat(&mut v, ALICE, EXIT_AT * NS, 0, b)),
            "not a request at the successor nullifier",
        );
    }
    // A proof at the successor of the next state is no help either.
    assert_refused(
        refusal(|| defeat(&mut v, ALICE, EXIT_AT * NS, 0, &f.defeat[1])),
        "not a request at the successor nullifier",
    );
    assert_eq!(
        v.note_status(commitment_of_note()),
        Some(Status::PendingExit)
    );
    assert_eq!(v.exit_ids.len(), 1);
}

#[test]
fn defeat_and_raise_check_only_version_chain_contract_and_state_key() {
    let f = fixture();
    let mut v = pending();
    let other = Felt252::from_u64(5).0;
    for (index, message) in [
        (request_input::PROTOCOL_VERSION, "wrong protocol version"),
        (request_input::CHAIN_ID, "wrong chain id"),
        (request_input::CONTRACT_ADDRESS, "wrong contract address"),
        (request_input::STATE_KEY_X, "wrong state key"),
        (request_input::STATE_KEY_Y, "wrong state key"),
    ] {
        let d = f.defeat[0].with_input(index, other);
        assert_refused(
            refusal(|| defeat(&mut v, ALICE, EXIT_AT * NS, 0, &d)),
            message,
        );
        let b = f.raise[0].with_input(index, other);
        assert_refused(refusal(|| raise(&mut v, EXIT_AT * NS, 0, &b, 10)), message);
    }
    // Any other input is bound by the proof: a forged root fails the pairing, not a root check.
    let d = f.defeat[0].with_input(3, other);
    assert_refused(
        refusal(|| defeat(&mut v, ALICE, EXIT_AT * NS, 0, &d)),
        "invalid proof",
    );
}

#[test]
fn defeat_and_raise_refuse_a_note_without_a_pending_exit() {
    let f = fixture();
    let mut v = funded();
    assert_refused(
        refusal(|| defeat(&mut v, ALICE, EXIT_AT * NS, 0, &f.defeat[0])),
        "no pending exit",
    );
    assert_refused(
        refusal(|| raise(&mut v, EXIT_AT * NS, 0, &f.raise[0], 10)),
        "no pending exit",
    );
    assert_refused(
        refusal(|| defeat(&mut v, ALICE, EXIT_AT * NS, 7, &f.defeat[0])),
        "unknown note",
    );
    // An exit already finalized.
    let mut v = pending();
    drop(finalize(&mut v, DEADLINE, 0));
    assert_refused(
        refusal(|| defeat(&mut v, ALICE, DEADLINE, 0, &f.defeat[0])),
        "no pending exit",
    );
    assert_refused(
        refusal(|| raise(&mut v, DEADLINE, 0, &f.raise[0], 10)),
        "no pending exit",
    );
}

#[test]
fn proofs_against_hundreds_of_older_roots_defeat_and_raise() {
    // The vault keeps no root check for either call: proofs made against trees of different
    // sizes (and so different roots) all verify.
    let f = fixture();
    let g = gen();
    let n = note();
    for extra in [0u64, 150, 400] {
        let others: Vec<_> = (0..extra)
            .map(|i| {
                let mut other = n.clone();
                other.note_id = 1 + i as u32;
                other.secret = Felt252::from_u64(1_000 + i);
                other
            })
            .collect();
        let mut refs: Vec<&_> = vec![&n];
        refs.extend(others.iter());
        let tree = tree_with(&refs);
        let defeat_proof = g.request(&f.states[2], &tree, &request(100_000)).unwrap();
        let raise_proof = g.request(&f.states[1], &tree, &request(50_000)).unwrap();
        let mut v = pending();
        raise(&mut v, EXIT_AT * NS, 0, &raise_proof, 40_000);
        defeat(&mut v, ALICE, EXIT_AT * NS, 0, &defeat_proof);
        assert_eq!(v.note_status(commitment_of_note()), Some(Status::Active));
    }
}

// raise_concession

#[test]
fn raise_concession_takes_the_least_of_amount_bound_and_balance() {
    let f = fixture();
    let cases = [
        // (proof, amount, expected conceded)
        (0, 20_000u128, 20_000u128),
        (0, 999_999_999, 100_000),
        (1, 999_999_999, 50_000),
        (2, 999_999_999, 30_000),
        (1, 10, 10),
    ];
    for (proof, amount, expected) in cases {
        let mut v = pending();
        raise(&mut v, EXIT_AT * NS + 9, 0, &f.raise[proof], amount);
        let exit = only_exit(&v);
        assert_eq!(exit.conceded.0, expected, "{proof} {amount}");
        assert_eq!(exit.final_balance.0, BALANCE_1 - expected);
        assert_eq!(
            exit.deadline.0, DEADLINE,
            "a raise never moves the deadline"
        );
        let logs = get_logs();
        assert!(logs[0].contains(r#""event":"raise_concession""#));
        assert!(logs[0].contains(&format!(r#""conceded":"{expected}""#)));
        // Last exit activity is set by `request_exit` and `defeat_exit`, not by a raise.
        assert_eq!(v.notes.get(0).unwrap().last_exit_ns, EXIT_AT * NS);
    }
}

#[test]
fn a_raise_is_capped_by_the_escaped_balance() {
    let f = fixture();
    let mut v = funded();
    // A state whose balance is below the proof's bound: concede the balance, pay nothing.
    inject(&mut v, 0, 5_000, 0, DEADLINE);
    v.exits.get_mut(&0).unwrap().nullifier = f.states[1].nullifier().0;
    raise(&mut v, EXIT_AT * NS, 0, &f.raise[0], 999_999);
    let exit = only_exit(&v);
    assert_eq!((exit.conceded.0, exit.final_balance.0), (5_000, 0));
}

#[test]
fn the_concession_only_grows() {
    let f = fixture();
    let mut v = pending();
    raise(&mut v, EXIT_AT * NS, 0, &f.raise[0], 40_000);
    // The same, a smaller amount and a smaller-bound proof after a larger one change nothing.
    for (proof, amount) in [(0, 40_000), (0, 10_000), (2, 999_999), (1, 40_000)] {
        assert_refused(
            refusal(|| raise(&mut v, EXIT_AT * NS, 0, &f.raise[proof], amount)),
            "the concession does not grow",
        );
    }
    assert_eq!(only_exit(&v).conceded.0, 40_000);
    // Growing is fine, in steps.
    raise(&mut v, EXIT_AT * NS, 0, &f.raise[0], 70_000);
    raise(&mut v, EXIT_AT * NS, 0, &f.raise[0], 999_999);
    let exit = only_exit(&v);
    assert_eq!(
        (exit.conceded.0, exit.final_balance.0),
        (100_000, BALANCE_1 - 100_000)
    );
    assert_refused(
        refusal(|| raise(&mut v, EXIT_AT * NS, 0, &f.raise[0], 999_999)),
        "the concession does not grow",
    );
    // A zero amount never grows anything.
    let mut v = pending();
    assert_refused(
        refusal(|| raise(&mut v, EXIT_AT * NS, 0, &f.raise[0], 0)),
        "the concession does not grow",
    );
}

#[test]
fn raise_needs_the_escaped_nullifier_not_the_successor() {
    let f = fixture();
    let mut v = pending();
    assert_refused(
        refusal(|| raise(&mut v, EXIT_AT * NS, 0, &f.defeat[0], 50_000)),
        "not a request at the escaped nullifier",
    );
    assert_eq!(only_exit(&v).conceded.0, 0);
}

// Deadline

#[test]
fn defeat_and_raise_stop_at_the_deadline_and_finalize_starts_there() {
    let f = fixture();
    let mut v = pending();
    // One nanosecond before: the window is open for both, finalize is not.
    assert_refused(
        refusal(|| {
            finalize(&mut v, DEADLINE - 1, 0);
        }),
        "the exit window is still open",
    );
    raise(&mut v, DEADLINE - 1, 0, &f.raise[0], 10_000);
    // At the deadline: defeat and raise are over.
    assert_refused(
        refusal(|| defeat(&mut v, ALICE, DEADLINE, 0, &f.defeat[0])),
        "the exit window is over",
    );
    assert_refused(
        refusal(|| raise(&mut v, DEADLINE, 0, &f.raise[0], 50_000)),
        "the exit window is over",
    );
    drop(finalize(&mut v, DEADLINE, 0));
    assert_eq!(v.note_status(commitment_of_note()), Some(Status::Closed));
    // One second before the deadline defeat still works.
    let mut v = pending();
    defeat(&mut v, ALICE, DEADLINE - NS, 0, &f.defeat[0]);
    assert_eq!(v.note_status(commitment_of_note()), Some(Status::Active));
}

// finalize_exit

#[test]
fn finalize_pays_the_record_and_books_the_rest_to_the_treasury() {
    let f = fixture();
    let mut v = pending();
    raise(&mut v, EXIT_AT * NS, 0, &f.raise[1], 50_000);
    let paid = BALANCE_1 - 50_000;
    let result = finalize(&mut v, DEADLINE, 0);
    assert!(matches!(result, PromiseOrValue::Promise(_)));
    drop(result);
    let logs = get_logs();
    assert!(logs[0].contains(r#""event":"finalize_exit""#));
    assert!(logs[0].contains(&format!(r#""amount_paid":"{paid}""#)));
    let all = receipts();
    let lines: Vec<&str> = all.lines().collect();
    assert_eq!(lines.len(), 3, "{all}");
    assert!(lines[0].starts_with("usdc.test storage_deposit"));
    assert!(lines[0].contains(r#""account_id":"alice.test""#));
    assert!(lines[1].starts_with("usdc.test ft_transfer"));
    assert!(lines[1].contains(&format!(r#""amount":"{paid}""#)));
    assert!(lines[2].starts_with("vault.test on_close_payout"));
    assert_eq!(v.note_status(commitment_of_note()), Some(Status::Closed));
    assert!(v.exit_requests(0, 10).is_empty());
    assert_eq!(v.config().current_tvl.0, 0);
    assert_eq!(v.open_notes(), 0);
    assert_eq!(v.treasury_balance().0, NOTE_FEE + ONE - paid);
    assert_counters(&v);
    // A second call fails.
    assert_refused(
        refusal(|| {
            finalize(&mut v, DEADLINE + 1, 0);
        }),
        "no pending exit",
    );
}

#[test]
fn finalize_with_nothing_left_to_pay_makes_no_transfer_and_still_closes() {
    let mut v = funded();
    inject(&mut v, 0, 0, ONE, 5);
    let result = finalize(&mut v, 5, 0);
    assert!(matches!(result, PromiseOrValue::Value(())));
    drop(result);
    assert!(get_created_receipts().is_empty());
    assert_eq!(v.note_status(commitment_of_note()), Some(Status::Closed));
    assert_eq!(v.treasury_balance().0, NOTE_FEE + ONE);
}

#[test]
fn finalize_needs_the_gas_for_its_payout() {
    let mut v = pending();
    testing_env!(ctx(ALICE, DEADLINE).prepaid_gas(Gas::from_tgas(29)).build());
    assert_refused(
        refusal(|| {
            v.finalize_exit(0);
        }),
        "not enough gas for the payout",
    );
    assert_eq!(
        v.note_status(commitment_of_note()),
        Some(Status::PendingExit)
    );
    // The least gas that passes is measured with the real token (issue #22).
    at(T0);
}

#[test]
fn finalize_is_refused_whole_when_the_vault_is_short_of_near() {
    let mut v = pending();
    let usage = env::storage_usage();
    let storage = u128::from(usage) * env::storage_byte_cost().as_yoctonear();
    let mut c = ctx(ALICE, DEADLINE);
    c.storage_usage(usage)
        .account_balance(NearToken::from_yoctonear(storage + PAYOUT_RESERVE - 1));
    testing_env!(c.build());
    assert_refused(
        refusal(|| {
            v.finalize_exit(0);
        }),
        "the vault is short of NEAR for the payout",
    );
    assert_eq!(
        v.note_status(commitment_of_note()),
        Some(Status::PendingExit)
    );
    let mut c = ctx(ALICE, DEADLINE);
    c.storage_usage(usage)
        .account_balance(NearToken::from_yoctonear(storage + PAYOUT_RESERVE));
    testing_env!(c.build());
    drop(v.finalize_exit(0));
    assert_eq!(v.note_status(commitment_of_note()), Some(Status::Closed));
}

#[test]
fn a_failed_finalize_payout_becomes_a_pending_payout() {
    let mut v = pending();
    drop(finalize(&mut v, DEADLINE, 0));
    testing_env!(
        ctx(VAULT, DEADLINE).build(),
        near_sdk::test_vm_config(),
        near_sdk::RuntimeFeesConfig::test(),
        Default::default(),
        vec![PromiseResult::Failed]
    );
    v.on_close_payout(account_id(ALICE), U128(BALANCE_1), 0, commitment_of_note());
    assert_eq!(v.pending_payout(account_id(ALICE)).0, BALANCE_1);
}

#[test]
fn a_retirement_works_after_finalize_and_an_exit_before_sync_gives_a_zero_leaf() {
    // Synced first, then exit, finalize, retire.
    let mut v = funded();
    assert_eq!(sync(&mut v, T0 + 30, 10), 1);
    request_exit(&mut v, EXIT_AT * NS, &fixture().escape[1]);
    drop(finalize(&mut v, DEADLINE, 0));
    let siblings = siblings_of(&v, 0);
    testing_env!(context("anyone.test", T0 + 8 * DAY).build());
    v.retire_closed_leaf(0, siblings);
    assert_eq!(v.get_leaves(0, 1), vec![commitment(0)]);
    assert_eq!(client_root(&v), v.current_root());
    // Exit and finalize before `sync_root` reaches the leaf: inserted as a zero leaf.
    let mut v = pending();
    drop(finalize(&mut v, DEADLINE, 0));
    assert_eq!(sync(&mut v, T0 + 8 * DAY, 10), 1);
    assert_eq!(v.get_leaves(0, 1), vec![commitment(0)]);
    assert!(v.note_records(0, 1)[0].leaf_retired);
    assert_eq!(client_root(&v), v.current_root());
}

#[test]
fn a_missed_heartbeat_opens_no_exit() {
    // The operator sends no heartbeat from deployment on, and time runs to the last second before
    // the note's claim date. Every permissionless call that could act on silence runs; none moves
    // the note, opens an exit or sweeps it. The vault only ever reads time for the claim rule, so
    // the check is that the claim rule alone decides: one second later the same call succeeds.
    let mut v = funded();
    let claim_date = note().expiry + 7 * DAY;
    testing_env!(ctx("anyone.test", (claim_date - 1) * NS).build());
    v.sync_root(10);
    assert!(v.claim_expired_batch(0, 10).is_empty());
    assert_refused(refusal(|| v.claim_expired(0)), "cannot be claimed yet");
    assert_eq!(v.note_status(commitment_of_note()), Some(Status::Active));
    assert!(v.exit_requests(0, 10).is_empty());
    assert_eq!(v.open_notes(), 1);
    // A late heartbeat does not change what silence did not do.
    testing_env!(ctx(OPERATOR, (claim_date - 1) * NS).build());
    v.heartbeat();
    assert_eq!(v.note_status(commitment_of_note()), Some(Status::Active));
    // And the check is live: at the claim date the same silence is what the claim rule sweeps.
    testing_env!(ctx("anyone.test", claim_date * NS).build());
    assert_eq!(v.claim_expired_batch(0, 10), vec![0]);
}

#[test]
fn a_withholding_operator_leaves_the_user_at_least_the_balance_minus_the_bound() {
    // The user escapes with s1 (balance B_k). The operator withholds the successor and answers
    // only with a request at s1 whose bound is b: the most it can take is b.
    let f = fixture();
    for (proof, bound) in [(0usize, 100_000u128), (1, 50_000), (2, 30_000)] {
        let mut v = pending();
        raise(&mut v, EXIT_AT * NS, 0, &f.raise[proof], u128::MAX);
        let exit = only_exit(&v);
        assert!(exit.final_balance.0 >= BALANCE_1 - bound);
        assert_eq!(exit.conceded.0, bound);
        drop(finalize(&mut v, DEADLINE, 0));
        assert_eq!(
            v.treasury_balance().0,
            NOTE_FEE + ONE - exit.final_balance.0
        );
    }
}

// The legal and illegal orderings

/// One step of the random walk: what it did, if it was accepted.
fn step(v: &mut Vault, choice: u64, amount: u128, now: u64) -> Option<&'static str> {
    let f = fixture();
    let pick = |n: u64| (choice / 8 % n) as usize;
    let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match choice % 8 {
        0 => {
            request_exit(v, now, &f.escape[pick(3)]);
            "request_exit"
        }
        1 => {
            let b = [&f.defeat[0], &f.defeat[1], &f.raise[0]][pick(3)];
            defeat(v, OPERATOR, now, 0, b);
            "defeat_exit"
        }
        2 => {
            let b = [&f.raise[0], &f.raise[1], &f.raise[2], &f.defeat[0]][pick(4)];
            raise(v, now, 0, b, amount);
            "raise_concession"
        }
        3 => {
            drop(finalize(v, now, 0));
            "finalize_exit"
        }
        4 => {
            testing_env!(ctx(if pick(2) == 0 { ALICE } else { OPERATOR }, now).build());
            let (proof, inputs) = args(close_bundle());
            drop(v.mutual_close(proof, inputs));
            "mutual_close"
        }
        5 => {
            testing_env!(ctx("anyone.test", now).build());
            // A sync with nothing waiting is not a step: it inserts nothing and changes nothing.
            assert!(v.sync_root(10) > 0, "nothing waiting");
            "sync_root"
        }
        6 => {
            testing_env!(ctx("anyone.test", now).build());
            v.claim_expired(pick(2) as u32);
            "claim_expired"
        }
        _ => {
            let id = pick(2) as u32;
            let siblings = siblings_of(v, id);
            testing_env!(ctx("anyone.test", now).build());
            v.retire_closed_leaf(id, siblings);
            "retire_closed_leaf"
        }
    }));
    run.ok()
}

/// Whether the model allows `call` on note `id` in this state, from the rules alone.
fn legal(v: &Vault, call: &str, id: u32, now: u64) -> bool {
    let note = v.notes.get(id).unwrap();
    let exit = v.exit_requests(0, 1).first().map(|e| e.deadline.0);
    let claim_at = note
        .expiry
        .max(note.last_exit_ns / NS)
        .saturating_add(7 * DAY);
    match call {
        "request_exit" | "mutual_close" => note.status == Status::Active,
        "defeat_exit" | "raise_concession" => {
            note.status == Status::PendingExit && exit.is_some_and(|d| now < d)
        }
        "finalize_exit" => note.status == Status::PendingExit && exit.is_some_and(|d| now >= d),
        "claim_expired" => note.status == Status::Active && now / NS >= claim_at,
        "retire_closed_leaf" => note.status == Status::Closed && !note.leaf_retired,
        _ => true,
    }
}

#[test]
fn random_orderings_keep_the_accounting_and_reject_the_illegal_ones() {
    let mut accepted = std::collections::BTreeMap::<&str, u32>::new();
    let mut rejected = std::collections::BTreeMap::<&str, u32>::new();
    let mut seed = 0x2545_f491_4f6c_dd1du64;
    let mut random = move |bound: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound
    };
    for _ in 0..120 {
        let mut v = funded();
        // A second plain note: the sweep and the leaf retirement reach more than one note.
        at(T0);
        assert_eq!(deposit(&mut v, ALICE, ONE + NOTE_FEE, &msg(2)), 0);
        let mut now = EXIT_AT * NS;
        // What the vault holds in USDC under this model: deposits in, payouts out.
        let mut usdc = 2 * (ONE + NOTE_FEE);
        for _ in 0..14 {
            now += match random(10) {
                0..=6 => random(300) * NS,
                7 | 8 => random(9) * DAY * NS,
                // Past the note's expiry plus the exit delay.
                _ => 40 * DAY * NS,
            };
            let before = v.exit_requests(0, 1).first().map(|e| e.final_balance.0);
            let statuses: Vec<_> = (0..2).map(|i| v.notes.get(i).unwrap().status).collect();
            let treasury = v.treasury_balance().0;
            let leaves = v.leaf_count;
            let (choice, amount) = (random(1 << 20), u128::from(random(150_000)));
            // The target note of the step: the choice picks note 0 or 1 for claims and retirements.
            let id = (choice / 8 % 2) as u32;
            let call = [
                "request_exit",
                "defeat_exit",
                "raise_concession",
                "finalize_exit",
                "mutual_close",
                "sync_root",
                "claim_expired",
                "retire_closed_leaf",
            ][(choice % 8) as usize];
            let id = if matches!(call, "claim_expired" | "retire_closed_leaf") {
                id
            } else {
                0
            };
            let allowed = legal(&v, call, id, now);
            let Some(done) = step(&mut v, choice, amount, now) else {
                // A refused call changes nothing.
                *rejected.entry(call).or_default() += 1;
                let after: Vec<_> = (0..2).map(|i| v.notes.get(i).unwrap().status).collect();
                assert_eq!(after, statuses);
                assert_eq!(v.treasury_balance().0, treasury);
                assert_eq!(v.leaf_count, leaves);
                continue;
            };
            assert_eq!(done, call);
            assert!(
                allowed,
                "{done} on note {id} was accepted where the rules forbid it"
            );
            *accepted.entry(done).or_default() += 1;
            let delta = v.treasury_balance().0 - treasury;
            match done {
                "finalize_exit" => {
                    let paid = before.expect("a record was there");
                    assert_eq!(
                        paid + delta,
                        ONE,
                        "payout plus treasury share is the deposit"
                    );
                    usdc -= paid;
                }
                "mutual_close" => {
                    let fee = if get_logs()
                        .iter()
                        .any(|l| l.contains(r#""close_fee":"30000""#))
                    {
                        30_000
                    } else {
                        0
                    };
                    let paid = BALANCE_1 - fee;
                    assert_eq!(paid + delta, ONE, "payout plus close fee plus share");
                    usdc -= paid;
                }
                "claim_expired" => assert_eq!(delta, ONE, "the whole denomination is swept"),
                _ => assert_eq!(delta, 0, "{done} moves no money"),
            }
            // Solvency, exactly: what the vault holds is what its notes and the treasury book.
            let held: u128 = (0..v.notes.len())
                .map(|id| v.notes.get(id).unwrap())
                .filter(|n| n.status != Status::Closed)
                .map(|n| n.deposit_amount)
                .sum();
            assert_eq!(held + v.treasury_balance().0, usdc);
            assert_counters(&v);
            let pending = v.notes.get(0).unwrap().status == Status::PendingExit;
            assert_eq!(!v.exit_requests(0, 10).is_empty(), pending);
            if let Some(exit) = v.exit_requests(0, 1).first() {
                assert!(exit.final_balance.0 + exit.conceded.0 <= ONE);
            }
        }
    }
    // The walk reached every kind of step, and the rules refused each of them somewhere.
    for call in [
        "request_exit",
        "defeat_exit",
        "raise_concession",
        "finalize_exit",
        "mutual_close",
        "sync_root",
        "claim_expired",
        "retire_closed_leaf",
    ] {
        assert!(
            accepted.get(call).copied().unwrap_or(0) > 0,
            "{call} never accepted: {accepted:?}"
        );
        assert!(
            rejected.get(call).copied().unwrap_or(0) > 0,
            "{call} never refused: {rejected:?}"
        );
    }
}

#[test]
fn a_defeated_state_cannot_be_closed_either() {
    let f = fixture();
    let mut v = pending();
    defeat(&mut v, ALICE, EXIT_AT * NS, 0, &f.defeat[0]);
    // The mutual-close proof is of s1, whose nullifier the defeat flagged.
    testing_env!(ctx(ALICE, EXIT_AT * NS + NS).build());
    let (proof, inputs) = args(close_bundle());
    assert_refused(
        refusal(|| {
            v.mutual_close(proof, inputs);
        }),
        "nullifier is flagged",
    );
    assert_eq!(v.note_status(commitment_of_note()), Some(Status::Active));
}
