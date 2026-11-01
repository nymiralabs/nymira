//! `claim_expired` and `claim_expired_batch`.

// The promises these calls return are scheduled when they drop, which is what the tests read.
#![allow(unused_must_use)]

use near_sdk::test_utils::get_created_receipts;
use nymira_testkit::proofs::WithdrawalKindId;

use super::close::{commitment_of_note, funded, gen, note, receipts, ALICE, CHARGE};
use super::exit::{
    assert_refused, ctx, finalize, fixture, inject, pending, refusal, request_exit, EXIT_AT, NS,
};
use super::*;

/// The last second of note 0's lifetime: 30 days after `T0`, rounded to the end of a UTC day.
fn expiry() -> u64 {
    note().expiry
}

/// The first second a note with expiry `expiry` can be claimed.
fn claimable_at(expiry: u64) -> u64 {
    expiry + 7 * DAY
}

fn claim(v: &mut Vault, secs: u64, id: u32) {
    testing_env!(ctx("anyone.test", secs * NS).build());
    v.claim_expired(id);
}

fn batch(v: &mut Vault, secs: u64, from: u32, limit: u32) -> Vec<u32> {
    testing_env!(ctx("anyone.test", secs * NS).build());
    v.claim_expired_batch(from, limit)
}

#[test]
fn a_claim_waits_for_the_expiry_plus_the_exit_delay_to_the_second() {
    let mut v = funded();
    let at = claimable_at(expiry());
    for secs in [T0 + 31 * DAY, expiry(), at - 1] {
        assert_refused(
            refusal(|| claim(&mut v, secs, 0)),
            "the note cannot be claimed yet",
        );
    }
    assert_eq!(v.note_status(commitment_of_note()), Some(Status::Active));
    // Not a nanosecond early, then exactly on the second.
    testing_env!(ctx("anyone.test", at * NS - 1).build());
    assert_refused(refusal(|| v.claim_expired(0)), "cannot be claimed yet");
    claim(&mut v, at, 0);
    assert_eq!(v.note_status(commitment_of_note()), Some(Status::Closed));
}

#[test]
fn a_claim_books_the_denomination_to_the_treasury_without_a_usdc_call() {
    let mut v = funded();
    claim(&mut v, claimable_at(expiry()), 0);
    assert!(get_created_receipts().is_empty());
    assert_eq!(v.treasury_balance().0, NOTE_FEE + ONE);
    assert_eq!(v.open_notes(), 0);
    assert_eq!(v.config().current_tvl.0, 0);
    let logs = get_logs();
    assert_eq!(logs.len(), 1);
    assert!(logs[0].contains(r#""event":"claim_expired""#));
    assert!(logs[0].contains(r#""note_ids":[0]"#));
    assert_counters(&v);
    // A second claim, and a note that does not exist.
    assert_refused(
        refusal(|| claim(&mut v, claimable_at(expiry()) + 1, 0)),
        "note is not active",
    );
    assert_refused(
        refusal(|| claim(&mut v, claimable_at(expiry()), 9)),
        "unknown note",
    );
}

#[test]
fn a_batch_skips_what_is_not_claimable_and_emits_one_event() {
    let mut v = roomy_vault();
    // Notes 0 and 1 expire on one day, note 2 two days later, note 3 is pending.
    for (i, day) in [0u64, 0, 2, 0].into_iter().enumerate() {
        at(T0 + day * DAY);
        assert_eq!(
            deposit(&mut v, "alice.test", ONE + NOTE_FEE, &msg(i as u64 + 1)),
            0
        );
    }
    inject(&mut v, 3, 500_000, 0, u64::MAX);
    let first = v.notes.get(0).unwrap().expiry;
    assert!(v.notes.get(2).unwrap().expiry > first);
    let swept = batch(&mut v, claimable_at(first), 0, 10);
    assert_eq!(swept, vec![0, 1]);
    let logs = get_logs();
    assert_eq!(logs.len(), 1, "one event for the whole batch");
    assert!(logs[0].contains(r#""note_ids":[0,1]"#));
    assert_eq!(v.notes.get(2).unwrap().status, Status::Active);
    assert_eq!(v.notes.get(3).unwrap().status, Status::PendingExit);
    assert_eq!(v.treasury_balance().0, 4 * NOTE_FEE + 2 * ONE);
    assert_counters(&v);
    // Nothing left to sweep: no event, an empty result.
    assert!(batch(&mut v, claimable_at(first), 0, 10).is_empty());
    assert!(get_logs().is_empty());
    // Later the third note follows, the pending one still never.
    let second = v.notes.get(2).unwrap().expiry;
    assert_eq!(batch(&mut v, claimable_at(second), 0, 10), vec![2]);
    assert_eq!(v.notes.get(3).unwrap().status, Status::PendingExit);
}

#[test]
fn a_batch_clamps_its_limit_to_fifty_and_starts_at_from() {
    let mut v = roomy_vault();
    for n in 1..=60 {
        deposit_ok(&mut v, n);
    }
    let at = claimable_at(v.notes.get(0).unwrap().expiry);
    assert_eq!(batch(&mut v, at, 0, 1_000).len(), 50);
    assert_eq!(get_logs().len(), 1);
    assert_eq!(batch(&mut v, at, 55, 1).len(), 1);
    assert_eq!(batch(&mut v, at, 50, u32::MAX).len(), 9);
    assert!(batch(&mut v, at, 60, 10).is_empty());
    assert!(batch(&mut v, at, u32::MAX, u32::MAX).is_empty());
    assert_eq!(v.open_notes(), 0);
    assert_counters(&v);
}

#[test]
fn a_pending_exit_blocks_both_calls_however_late() {
    let mut v = pending();
    let late = claimable_at(expiry()) + 30 * DAY;
    assert_refused(refusal(|| claim(&mut v, late, 0)), "note is not active");
    assert!(batch(&mut v, late, 0, 10).is_empty());
    assert_eq!(
        v.note_status(commitment_of_note()),
        Some(Status::PendingExit)
    );
}

#[test]
fn recent_exit_activity_pushes_the_claim_back_by_a_whole_window() {
    // A defeat at `last` leaves the note `Active` with `last_exit_ns = last`.
    let mut v = funded();
    let last = (claimable_at(expiry()) + DAY) * NS;
    v.notes.get_mut(0).unwrap().last_exit_ns = last;
    let before = (claimable_at(expiry()) + DAY + 7 * DAY) - 1;
    assert_refused(
        refusal(|| claim(&mut v, before, 0)),
        "the note cannot be claimed yet",
    );
    assert!(batch(&mut v, before, 0, 10).is_empty());
    claim(&mut v, claimable_at(expiry()) + DAY + 7 * DAY, 0);
    assert_eq!(v.note_status(commitment_of_note()), Some(Status::Closed));
}

#[test]
fn a_defeat_stamps_the_activity_that_delays_the_claim() {
    let f = fixture();
    let mut v = pending();
    testing_env!(ctx(ALICE, EXIT_AT * NS + 9).build());
    let (proof, inputs) = super::exit::args(&f.defeat[0]);
    v.defeat_exit(0, proof, inputs);
    assert_eq!(v.notes.get(0).unwrap().last_exit_ns, EXIT_AT * NS + 9);
    // The expiry still dominates this early, so the claim date is the expiry's.
    assert_refused(
        refusal(|| claim(&mut v, claimable_at(expiry()) - 1, 0)),
        "cannot be claimed yet",
    );
}

#[test]
fn an_expired_note_can_still_exit_and_finalize() {
    let g = gen();
    let s1 = fixture().states[1].clone();
    let after_expiry = expiry() + DAY;
    let escape = g
        .withdrawal(
            &s1,
            WithdrawalKindId::Escape,
            ALICE,
            0,
            after_expiry + 3_600,
        )
        .unwrap();
    let mut v = funded();
    request_exit(&mut v, after_expiry * NS, &escape);
    assert_eq!(
        v.note_status(commitment_of_note()),
        Some(Status::PendingExit)
    );
    // While the exit is pending nobody can sweep it, even past expiry plus a window.
    assert_refused(
        refusal(|| claim(&mut v, claimable_at(expiry()) + DAY, 0)),
        "note is not active",
    );
    let deadline = after_expiry * NS + 7 * DAY * NS;
    drop(finalize(&mut v, deadline, 0));
    assert_eq!(v.note_status(commitment_of_note()), Some(Status::Closed));
    assert!(receipts().contains(&format!(r#""amount":"{}""#, ONE - CHARGE)));
}

#[test]
fn a_swept_note_cannot_exit_and_claim_treasury_pays_the_booked_amount() {
    let f = fixture();
    let mut v = funded();
    claim(&mut v, claimable_at(expiry()), 0);
    // An older, unconsumed state: s0.
    testing_env!(ctx("anyone.test", claimable_at(expiry()) * NS).build());
    let (proof, inputs) = super::exit::args(&f.escape[0]);
    assert_refused(
        refusal(|| v.request_exit(proof, inputs)),
        "note is not active",
    );
    testing_env!(ctx("anyone.test", claimable_at(expiry()) * NS).build());
    drop(v.claim_treasury());
    let all = receipts();
    assert!(all.contains("usdc.test ft_transfer"), "{all}");
    assert!(all.contains(&format!(r#""amount":"{}""#, NOTE_FEE + ONE)));
    assert!(all.contains(r#""receiver_id":"treasury.test""#));
    assert_eq!(v.treasury_balance().0, 0);
}

#[test]
fn retirement_works_on_a_swept_note() {
    let mut v = funded();
    assert_eq!(sync(&mut v, T0 + 30, 10), 1);
    claim(&mut v, claimable_at(expiry()), 0);
    let siblings = siblings_of(&v, 0);
    testing_env!(ctx("anyone.test", claimable_at(expiry()) * NS).build());
    v.retire_closed_leaf(0, siblings);
    assert_eq!(v.get_leaves(0, 1), vec![commitment(0)]);
    assert_eq!(client_root(&v), v.current_root());
    // A note swept before `sync_root` reached it is inserted as a zero leaf.
    let mut v = funded();
    claim(&mut v, claimable_at(expiry()), 0);
    assert_eq!(sync(&mut v, claimable_at(expiry()) + 1, 10), 1);
    assert_eq!(v.get_leaves(0, 1), vec![commitment(0)]);
    assert_eq!(client_root(&v), v.current_root());
}
