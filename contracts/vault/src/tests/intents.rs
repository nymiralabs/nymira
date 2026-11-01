//! The `intents.near` branch of `ft_on_transfer`: every path ends in a record, none panics.

#![allow(unused_must_use)]

use near_sdk::PromiseResult;

use super::close::callback_env;
use super::*;

const INTENTS: &str = "intents.near";
const BOB: &str = "bob.test";

fn intents_msg(n: u64, payout: &str) -> String {
    format!(
        r#"{{"commitment":"{}","withdraw_to":"{payout}"}}"#,
        commitment(n)
    )
}

/// A transfer from the token with the Intents account as sender; returns the unused amount.
fn from_intents(v: &mut Vault, amount: u128, msg: &str) -> u128 {
    at(T0);
    v.ft_on_transfer(account(INTENTS), U128(amount), msg.to_owned())
        .0
}

fn pending_of(v: &Vault, who: &str) -> u128 {
    v.pending_payout(account(who)).0
}

fn last_event() -> String {
    get_logs().pop().expect("an event")
}

#[test]
fn a_valid_intents_deposit_writes_the_note() {
    let mut v = vault();
    assert_eq!(
        from_intents(&mut v, ONE + NOTE_FEE, &intents_msg(1, BOB)),
        0
    );
    assert_eq!(v.note_records(0, 10).len(), 1);
    assert_eq!(v.notes.get(0).unwrap().withdraw_to, account(BOB));
    assert_eq!(pending_of(&v, BOB), 0);
    assert_eq!(v.treasury_balance().0, NOTE_FEE);
    assert!(get_logs()[0].contains(r#""event":"deposit""#));
    assert_counters(&v);
}

#[test]
fn the_excess_is_a_pending_payout_or_dust() {
    let mut v = vault();
    // 70_000 over the price is at least the note fee: a new pending record.
    assert_eq!(
        from_intents(&mut v, ONE + NOTE_FEE + 70_000, &intents_msg(1, BOB)),
        0
    );
    assert_eq!(pending_of(&v, BOB), 70_000);
    let logs = get_logs();
    assert!(logs[1].contains(r#""event":"payout_pending""#));
    assert!(logs[1].contains(r#""reason":"intents_excess""#));
    assert!(logs[1].contains(r#""note_id":0"#));
    // Under the note fee with no record: dust for the treasury, with its own event.
    let mut v = vault();
    from_intents(&mut v, ONE + NOTE_FEE + 12_000, &intents_msg(1, BOB));
    assert_eq!(pending_of(&v, BOB), 0);
    assert_eq!(v.treasury_balance().0, NOTE_FEE + 12_000);
    let event = last_event();
    assert!(event.contains(r#""event":"dust""#));
    assert!(event.contains(r#""reason":"below_note_fee""#));
    // With a record, a small excess just adds to it.
    let mut v = vault();
    from_intents(&mut v, ONE + NOTE_FEE + 70_000, &intents_msg(1, BOB));
    from_intents(&mut v, ONE + NOTE_FEE + 12_000, &intents_msg(2, BOB));
    assert_eq!(pending_of(&v, BOB), 82_000);
    assert_eq!(v.treasury_balance().0, 2 * NOTE_FEE);
}

#[test]
fn the_larger_denomination_that_fits_is_taken() {
    // 1 and 5 USDC: an amount that fits both buys the 5 USDC note.
    let mut v = vault();
    from_intents(&mut v, FIVE + NOTE_FEE + 100_000, &intents_msg(1, BOB));
    assert_eq!(v.notes.get(0).unwrap().deposit_amount, FIVE);
    assert_eq!(pending_of(&v, BOB), 100_000);
    // One base unit short of the large price: the small note and a large excess.
    let mut v = vault();
    from_intents(&mut v, FIVE + NOTE_FEE - 1, &intents_msg(1, BOB));
    assert_eq!(v.notes.get(0).unwrap().deposit_amount, ONE);
    assert_eq!(pending_of(&v, BOB), FIVE - ONE - 1);
    // Exactly the large price.
    let mut v = vault();
    from_intents(&mut v, FIVE + NOTE_FEE, &intents_msg(1, BOB));
    assert_eq!(v.notes.get(0).unwrap().deposit_amount, FIVE);
    assert_eq!(pending_of(&v, BOB), 0);
}

#[test]
fn refused_deposits_come_back_as_a_pending_payout_never_to_intents() {
    let mut v = vault();
    // Short amount.
    assert_eq!(
        from_intents(&mut v, ONE + NOTE_FEE - 1, &intents_msg(1, BOB)),
        0
    );
    assert_eq!(pending_of(&v, BOB), ONE + NOTE_FEE - 1);
    // Repeated commitment.
    from_intents(&mut v, ONE + NOTE_FEE, &intents_msg(2, "alice.test"));
    from_intents(&mut v, ONE + NOTE_FEE, &intents_msg(2, BOB));
    assert_eq!(pending_of(&v, BOB), 2 * (ONE + NOTE_FEE) - 1);
    // Commitment at the modulus, and one that is not a word at all.
    for bad in [
        format!(
            r#"{{"commitment":"{}","withdraw_to":"{BOB}"}}"#,
            word::encode32(&SCALAR_MODULUS_BE)
        ),
        format!(r#"{{"commitment":"0x12","withdraw_to":"{BOB}"}}"#),
    ] {
        let before = pending_of(&v, BOB);
        assert_eq!(from_intents(&mut v, ONE + NOTE_FEE, &bad), 0);
        assert_eq!(pending_of(&v, BOB), before + ONE + NOTE_FEE);
    }
    // A full cap.
    let mut v = try_new(Params {
        tvl_cap: ONE + NOTE_FEE,
        ..Params::default()
    });
    from_intents(&mut v, ONE + NOTE_FEE, &intents_msg(1, "alice.test"));
    from_intents(&mut v, ONE + NOTE_FEE, &intents_msg(2, BOB));
    assert_eq!(v.note_records(0, 10).len(), 1);
    assert_eq!(pending_of(&v, BOB), ONE + NOTE_FEE);
    assert_counters(&v);
}

#[test]
fn a_refused_amount_under_the_note_fee_is_dust() {
    let mut v = vault();
    from_intents(&mut v, 10, &intents_msg(1, BOB));
    assert_eq!(pending_of(&v, BOB), 0);
    assert_eq!(v.treasury_balance().0, 10);
    assert!(last_event().contains(r#""event":"dust""#));
}

#[test]
fn a_bad_message_goes_to_the_unclaimed_bucket_without_echoing_it() {
    let secret = "SECRET-MARK";
    let bad_msgs = [
        "not json".to_owned(),
        String::new(),
        format!(r#"{{"commitment":"{}"}}"#, commitment(1)),
        format!(
            r#"{{"commitment":"{}","withdraw_to":"{secret}"}}"#,
            commitment(1)
        ),
        format!(
            r#"{{"commitment":"{}","withdraw_to":"vault.test"}}"#,
            commitment(1)
        ),
        format!(
            r#"{{"commitment":"{}","withdraw_to":"usdc.test"}}"#,
            commitment(1)
        ),
        format!(
            r#"{{"commitment":"{}","withdraw_to":"{INTENTS}"}}"#,
            commitment(1)
        ),
        format!(
            r#"{{"commitment":"{}","withdraw_to":"{BOB}","{secret}":1}}"#,
            commitment(1)
        ),
        format!(r#"{{"withdraw_to":"{BOB}"}}"#),
    ];
    let mut v = vault();
    let mut total = 0;
    for (i, msg) in bad_msgs.iter().enumerate() {
        let amount = ONE + NOTE_FEE + i as u128;
        total += amount;
        at(T0);
        assert_eq!(from_intents(&mut v, amount, msg), 0, "{msg}");
        let logs = get_logs();
        assert_eq!(logs.len(), 1, "{msg}");
        assert!(logs[0].contains(r#""event":"unclaimed""#), "{msg}");
        assert!(logs[0].contains(r#""reason":"intents_bad_msg""#));
        assert!(!logs[0].contains(secret), "the raw message is echoed");
        assert!(!logs[0].contains("not json"));
    }
    assert_eq!(v.treasury_balance().0, total);
    assert!(v.note_records(0, 10).is_empty());
    assert_eq!(pending_of(&v, BOB), 0);
}

#[test]
fn only_the_token_creates_records() {
    let mut v = vault();
    // A fake token calling with `sender_id = intents.near`: returned, nothing written.
    testing_env!(context("fake.test", T0).build());
    let amount = v
        .ft_on_transfer(account(INTENTS), U128(ONE + NOTE_FEE), intents_msg(1, BOB))
        .0;
    assert_eq!(amount, ONE + NOTE_FEE);
    assert!(v.note_records(0, 10).is_empty());
    assert_eq!(pending_of(&v, BOB), 0);
    assert_eq!(v.treasury_balance().0, 0);
    assert!(get_logs().is_empty());
}

#[test]
fn a_normal_sender_is_still_refused_the_old_way() {
    let mut v = vault();
    at(T0);
    // A bad denomination from anyone else comes back as before, no record.
    assert_eq!(deposit(&mut v, "alice.test", ONE, &msg(1)), ONE);
    assert_eq!(v.treasury_balance().0, 0);
    // And a payout account that is the Intents account is refused for them.
    let bad = intents_msg(1, INTENTS);
    assert_eq!(
        deposit(&mut v, "alice.test", ONE + NOTE_FEE, &bad),
        ONE + NOTE_FEE
    );
}

#[test]
fn claim_payout_pays_an_intents_pending_record_and_empties_the_view() {
    let mut v = vault();
    from_intents(&mut v, ONE + NOTE_FEE + 70_000, &intents_msg(1, BOB));
    assert_eq!(pending_of(&v, BOB), 70_000);
    testing_env!(context("anyone.test", T0).build());
    drop(v.claim_payout(account(BOB)));
    assert_eq!(pending_of(&v, BOB), 0);
}

#[test]
fn many_pending_accounts_and_failures_keep_every_amount_claimable() {
    let mut v = roomy_vault();
    for i in 0..150u64 {
        let who = format!("user{i}.test");
        from_intents(
            &mut v,
            ONE + NOTE_FEE + 60_000 + i as u128,
            &intents_msg(i + 1, &who),
        );
    }
    for i in 0..150u128 {
        assert_eq!(pending_of(&v, &format!("user{i}.test")), 60_000 + i);
    }
    // A failed close payout adds to an existing record.
    callback_env(VAULT, PromiseResult::Failed);
    v.on_close_payout(account("user7.test"), U128(1_000), 7, commitment(8));
    assert_eq!(pending_of(&v, "user7.test"), 60_007 + 1_000);
    // A failed `claim_payout` restores the amount it took.
    testing_env!(context("anyone.test", T0).build());
    drop(v.claim_payout(account("user9.test")));
    assert_eq!(pending_of(&v, "user9.test"), 0);
    callback_env(VAULT, PromiseResult::Failed);
    v.on_claim_payout(account("user9.test"), U128(60_009));
    assert_eq!(pending_of(&v, "user9.test"), 60_009);
}

#[test]
fn random_messages_never_panic_and_conserve_every_unit() {
    let mut seed = 0x1234_5678_9abc_def0u64;
    let mut random = move |bound: u64| {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (seed >> 33) % bound
    };
    let pieces = [
        r#"{"commitment":""#,
        r#"","withdraw_to":""#,
        r#"bob.test"#,
        r#"vault.test"#,
        r#"intents.near"#,
        r#"usdc.test"#,
        "0x",
        "00",
        "ff",
        r#""}"#,
        "{",
        "}",
        "\u{0}",
        "é",
        r#"","x":1}"#,
    ];
    let mut v = roomy_vault();
    let mut sent = 0u128;
    for _ in 0..400 {
        let mut text = String::new();
        for _ in 0..random(7) {
            text.push_str(pieces[random(pieces.len() as u64) as usize]);
        }
        // Half the time, a well-formed message with a random commitment.
        if random(2) == 0 {
            text = intents_msg(random(40) + 1, BOB);
        }
        let amount = u128::from(random(6_500_000));
        sent += amount;
        assert_eq!(from_intents(&mut v, amount, &text), 0, "{text:?}");
    }
    let notes: u128 = (0..v.notes.len())
        .map(|id| v.notes.get(id).unwrap().deposit_amount)
        .sum();
    // Every unit sent is a note deposit, a fee, dust, unclaimed or a pending payout.
    assert_eq!(notes + v.treasury_balance().0 + pending_of(&v, BOB), sent);
    assert_counters(&v);
}
