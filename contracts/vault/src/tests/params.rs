//! Parameter views, the TVL cap and lease parameter proposals, and the heartbeat.

#![allow(unused_must_use)]

use super::exit::{assert_refused, refusal};
use super::*;

const OWNER: &str = "owner.test";
const OPERATOR: &str = "operator.test";
const HOUR: u64 = 3_600;
const NS: u64 = 1_000_000_000;
const WAIT: u64 = 48 * HOUR;

/// A call from `caller` at `ns` with `yocto` attached.
fn as_caller(caller: &str, ns: u64, yocto: u128) {
    let mut builder = context(caller, 0);
    builder
        .block_timestamp(ns)
        .attached_deposit(NearToken::from_yoctonear(yocto));
    testing_env!(builder.build());
}

fn propose_cap(v: &mut Vault, secs: u64, cap: u128) {
    as_caller(OWNER, secs * NS, 1);
    v.propose_tvl_cap(U128(cap));
}

fn apply_cap(v: &mut Vault, ns: u64) {
    as_caller(OWNER, ns, 1);
    v.apply_tvl_cap();
}

fn new_params() -> LeaseParams {
    LeaseParams {
        bound_classes: vec![U128(200_000), U128(1_000_000)],
        charge_step: U128(2_000),
        min_charge: U128(4_000),
        slot_rate: 2_000,
    }
}

fn propose_params(v: &mut Vault, secs: u64, params: LeaseParams) {
    as_caller(OWNER, secs * NS, 1);
    v.propose_params(params);
}

fn apply_params(v: &mut Vault, ns: u64) {
    as_caller(OWNER, ns, 1);
    v.apply_params();
}

// Views

#[test]
fn the_parameter_views_and_config_return_the_deploy_values() {
    let v = vault();
    assert_eq!(v.bound_classes(), vec![U128(100_000), U128(500_000)]);
    assert_eq!(v.charge_step().0, 1_000);
    assert_eq!(v.min_charge().0, 2_000);
    assert_eq!(v.slot_rate(), 1_000);
    assert_eq!(v.operator_account(), account(OPERATOR));
    let c = v.config();
    assert_eq!(c.bound_classes, v.bound_classes());
    assert_eq!(c.charge_step, v.charge_step());
    assert_eq!(c.min_charge, v.min_charge());
    assert_eq!(c.slot_rate, v.slot_rate());
    assert_eq!(c.operator_account, v.operator_account());
    assert_eq!(c.owner, account(OWNER));
    assert_eq!(c.close_fee.0, 30_000);
    assert_eq!(c.tvl_cap.0, 100_000_000);
    assert_eq!(c.exit_delay_secs, 7 * DAY);
    assert_eq!(c.note_ttl, 30 * DAY);
}

#[test]
fn new_refuses_nonsense_lease_parameters() {
    for (classes, step, min, rate, message) in [
        (vec![], 1_000, 2_000, 1_000, "no bound classes"),
        (vec![0], 1_000, 2_000, 1_000, "a bound class is zero"),
        (vec![5, 5], 1_000, 2_000, 1_000, "repeated bound class"),
        (vec![5], 0, 2_000, 1_000, "charge step is zero"),
        (vec![5], 1_000, 0, 1_000, "minimum charge is zero"),
        (vec![5], 1_000, 2_000, 1_000_001, "slot rate above 100%"),
    ] {
        let lease = LeaseParams {
            bound_classes: classes.into_iter().map(U128).collect(),
            charge_step: U128(step),
            min_charge: U128(min),
            slot_rate: rate,
        };
        assert_refused(refusal(|| lease.check()), message);
    }
}

// TVL cap

#[test]
fn pending_proposals_show_in_views_until_they_apply() {
    let mut v = vault();
    assert!(v.pending_tvl_cap().is_none());
    assert!(v.pending_params().is_none());
    propose_cap(&mut v, 1_000, 7 * ONE);
    propose_params(&mut v, 2_000, new_params());
    at(T0);
    let cap = v.pending_tvl_cap().expect("a cap proposal");
    assert_eq!(cap.tvl_cap.0, 7 * ONE);
    assert_eq!(cap.effective_at.0, (1_000 + WAIT) * NS);
    assert_eq!(cap.lapses_at.0, (1_000 + WAIT + 7 * DAY) * NS);
    let params = v.pending_params().expect("a params proposal");
    assert_eq!(params.params.min_charge.0, 4_000);
    assert_eq!(params.effective_at.0, (2_000 + WAIT) * NS);
    // The applied values do not move before they apply.
    assert_eq!(v.config().tvl_cap.0, 100_000_000);
    apply_cap(&mut v, (1_000 + WAIT) * NS);
    apply_params(&mut v, (2_000 + WAIT) * NS);
    assert!(v.pending_tvl_cap().is_none());
    assert!(v.pending_params().is_none());
}

#[test]
fn a_tvl_cap_applies_after_exactly_48_hours() {
    let mut v = vault();
    propose_cap(&mut v, T0, 7 * ONE);
    let logs = get_logs();
    assert!(logs[0].contains(r#""event":"tvl_cap_proposed""#));
    assert!(logs[0].contains(r#""tvl_cap":"7000000""#));
    assert!(logs[0].contains(&format!(r#""effective_at":"{}""#, (T0 + WAIT) * NS)));
    // One second early, and one nanosecond early: refused, the cap is unchanged.
    for ns in [(T0 + WAIT - 1) * NS, (T0 + WAIT) * NS - 1] {
        assert_refused(
            refusal(|| apply_cap(&mut v, ns)),
            "the 48-hour wait is not over",
        );
    }
    assert_eq!(v.config().tvl_cap.0, 100_000_000);
    apply_cap(&mut v, (T0 + WAIT) * NS);
    assert_eq!(v.config().tvl_cap.0, 7 * ONE);
    let logs = get_logs();
    assert!(logs[0].contains(r#""event":"tvl_cap_applied""#));
    assert!(logs[0].contains(r#""tvl_cap":"7000000""#));
    assert!(logs[0].contains(&format!(r#""effective_at":"{}""#, (T0 + WAIT) * NS)));
    // `apply` cleared the record.
    assert_refused(
        refusal(|| apply_cap(&mut v, (T0 + WAIT) * NS + 1)),
        "no pending proposal",
    );
}

#[test]
fn the_new_cap_governs_deposits() {
    let mut v = vault();
    propose_cap(&mut v, T0, 2 * (ONE + NOTE_FEE));
    apply_cap(&mut v, (T0 + WAIT) * NS);
    at(T0 + WAIT);
    for n in 1..=2 {
        assert_eq!(deposit(&mut v, "alice.test", ONE + NOTE_FEE, &msg(n)), 0);
    }
    assert_eq!(
        deposit(&mut v, "alice.test", ONE + NOTE_FEE, &msg(3)),
        ONE + NOTE_FEE
    );
    // A cap below the current TVL is allowed: it only stops new deposits.
    propose_cap(&mut v, T0 + WAIT, ONE);
    apply_cap(&mut v, (T0 + 2 * WAIT) * NS);
    assert_eq!(v.config().tvl_cap.0, ONE);
    assert_eq!(v.config().current_tvl.0, 2 * (ONE + NOTE_FEE));
}

#[test]
fn a_second_proposal_replaces_the_first_and_restarts_the_wait() {
    let mut v = vault();
    propose_cap(&mut v, T0, 5 * ONE);
    propose_cap(&mut v, T0 + DAY, 9 * ONE);
    // 48 hours after the first one: too early for the second.
    assert_refused(
        refusal(|| apply_cap(&mut v, (T0 + WAIT) * NS)),
        "the 48-hour wait is not over",
    );
    apply_cap(&mut v, (T0 + DAY + WAIT) * NS);
    assert_eq!(v.config().tvl_cap.0, 9 * ONE);
}

#[test]
fn a_proposal_lapses_seven_days_after_it_becomes_applicable() {
    let mut v = vault();
    propose_cap(&mut v, T0, 5 * ONE);
    let last = T0 + WAIT + 7 * DAY;
    assert_refused(
        refusal(|| apply_cap(&mut v, (last + 1) * NS)),
        "the proposal lapsed",
    );
    assert_eq!(v.config().tvl_cap.0, 100_000_000);
    // At exactly seven days it still applies.
    apply_cap(&mut v, last * NS);
    assert_eq!(v.config().tvl_cap.0, 5 * ONE);
    // A lapsed one can be proposed again.
    propose_cap(&mut v, last + 10, 3 * ONE);
    apply_cap(&mut v, (last + 10 + WAIT) * NS);
    assert_eq!(v.config().tvl_cap.0, 3 * ONE);
}

#[test]
fn lease_parameters_over_the_limits_are_refused() {
    let mut p = new_params();
    p.bound_classes = (1..=5).map(|n| U128(n * 1_000_000)).collect();
    let text = refusal(|| p.check());
    assert_refused(text, "more than four bound classes");
    let mut p = new_params();
    p.min_charge = U128(300_000);
    let text = refusal(|| p.check());
    assert_refused(text, "minimum charge above the smallest bound class");
}

#[test]
fn a_zero_cap_is_refused() {
    let mut v = vault();
    assert_refused(refusal(|| propose_cap(&mut v, T0, 0)), "tvl cap is zero");
    assert_refused(
        refusal(|| apply_cap(&mut v, (T0 + WAIT) * NS)),
        "no pending proposal",
    );
}

// Lease parameters

#[test]
fn lease_parameters_apply_after_48_hours_and_touch_nothing_else() {
    let mut v = vault();
    let before = v.config();
    propose_params(&mut v, T0, new_params());
    let logs = get_logs();
    assert!(logs[0].contains(r#""event":"params_proposed""#));
    assert!(logs[0].contains(r#""bound_classes":["200000","1000000"]"#));
    assert!(logs[0].contains(&format!(r#""effective_at":"{}""#, (T0 + WAIT) * NS)));
    for ns in [(T0 + WAIT - 1) * NS, (T0 + WAIT) * NS - 1] {
        assert_refused(
            refusal(|| apply_params(&mut v, ns)),
            "the 48-hour wait is not over",
        );
    }
    assert_eq!(v.bound_classes(), before.bound_classes);
    apply_params(&mut v, (T0 + WAIT) * NS);
    let after = v.config();
    assert_eq!(after.bound_classes, new_params().bound_classes);
    assert_eq!(after.charge_step.0, 2_000);
    assert_eq!(after.min_charge.0, 4_000);
    assert_eq!(after.slot_rate, 2_000);
    // The exit delay, the note lifetime, the fee and every pinned key are not parameters.
    assert_eq!(after.exit_delay_secs, before.exit_delay_secs);
    assert_eq!(after.note_ttl, before.note_ttl);
    assert_eq!(after.note_fee, before.note_fee);
    assert_eq!(after.denominations, before.denominations);
    assert_eq!(after.tvl_cap, before.tvl_cap);
    let logs = get_logs();
    assert!(logs[0].contains(r#""event":"params_applied""#));
    assert!(logs[0].contains(r#""slot_rate":2000"#));
    assert_refused(
        refusal(|| apply_params(&mut v, (T0 + WAIT) * NS + 1)),
        "no pending proposal",
    );
}

#[test]
fn a_second_params_proposal_restarts_the_wait_and_old_ones_lapse() {
    let mut v = vault();
    propose_params(&mut v, T0, new_params());
    let mut other = new_params();
    other.slot_rate = 3_000;
    propose_params(&mut v, T0 + DAY, other);
    assert_refused(
        refusal(|| apply_params(&mut v, (T0 + WAIT) * NS)),
        "the 48-hour wait is not over",
    );
    assert_refused(
        refusal(|| apply_params(&mut v, (T0 + DAY + WAIT + 7 * DAY + 1) * NS)),
        "the proposal lapsed",
    );
    apply_params(&mut v, (T0 + DAY + WAIT + 7 * DAY) * NS);
    assert_eq!(v.slot_rate(), 3_000);
}

#[test]
fn bad_parameters_are_refused_at_proposal_time() {
    let mut v = vault();
    let mut bad = new_params();
    bad.bound_classes = vec![];
    assert_refused(
        refusal(|| propose_params(&mut v, T0, bad)),
        "no bound classes",
    );
    assert_refused(
        refusal(|| apply_params(&mut v, (T0 + WAIT) * NS)),
        "no pending proposal",
    );
}

#[test]
fn the_two_proposal_kinds_are_independent() {
    let mut v = vault();
    propose_cap(&mut v, T0, 5 * ONE);
    propose_params(&mut v, T0 + HOUR, new_params());
    apply_params(&mut v, (T0 + HOUR + WAIT) * NS);
    apply_cap(&mut v, (T0 + HOUR + WAIT) * NS);
    assert_eq!(v.config().tvl_cap.0, 5 * ONE);
    assert_eq!(v.slot_rate(), 2_000);
}

// Who may call

#[test]
fn only_the_owner_with_one_yocto_can_propose_or_apply() {
    let mut v = vault();
    propose_cap(&mut v, T0, 5 * ONE);
    propose_params(&mut v, T0, new_params());
    let late = (T0 + WAIT) * NS;
    for caller in [OPERATOR, "alice.test", "treasury.test", VAULT, TOKEN] {
        as_caller(caller, late, 1);
        assert_refused(
            refusal(|| v.propose_tvl_cap(U128(ONE))),
            "only the vault owner",
        );
        assert_refused(refusal(|| v.apply_tvl_cap()), "only the vault owner");
        assert_refused(
            refusal(|| v.propose_params(new_params())),
            "only the vault owner",
        );
        assert_refused(refusal(|| v.apply_params()), "only the vault owner");
    }
    // The owner without a yoctoNEAR, or with more than one.
    for yocto in [0, 2] {
        as_caller(OWNER, late, yocto);
        assert_refused(
            refusal(|| v.propose_tvl_cap(U128(ONE))),
            "Requires attached deposit of exactly 1 yoctoNEAR",
        );
        assert_refused(refusal(|| v.apply_tvl_cap()), "exactly 1 yoctoNEAR");
        assert_refused(
            refusal(|| v.propose_params(new_params())),
            "exactly 1 yoctoNEAR",
        );
        assert_refused(refusal(|| v.apply_params()), "exactly 1 yoctoNEAR");
    }
    // Nothing moved.
    assert_eq!(v.config().tvl_cap.0, 100_000_000);
    apply_cap(&mut v, late);
    apply_params(&mut v, late);
}

// Heartbeat

#[test]
fn heartbeat_emits_the_block_time_for_the_operator_only() {
    let mut v = vault();
    as_caller(OPERATOR, (T0 + 77) * NS + 5, 0);
    v.heartbeat();
    let logs = get_logs();
    assert_eq!(logs.len(), 1);
    assert!(logs[0].contains(r#""event":"heartbeat""#));
    assert!(logs[0].contains(&format!(r#""ts":"{}""#, (T0 + 77) * NS + 5)));
    for caller in [OWNER, "alice.test", "treasury.test", VAULT, TOKEN] {
        as_caller(caller, (T0 + 80) * NS, 0);
        assert_refused(refusal(|| v.heartbeat()), "only the operator account");
        assert!(get_logs().is_empty());
    }
}
