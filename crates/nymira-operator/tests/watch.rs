//! The credit watcher: alarms, the minting stop and the ledger comparison.

mod common;

use common::*;
use nymira_operator::budget::{BudgetConfig, CreditConfig};
use nymira_operator::provider::ProviderError;

fn hackathon(credit: u128) -> Setup {
    Setup {
        credit,
        tune: Box::new(|c| {
            c.budget = BudgetConfig::hackathon();
            c.credit = CreditConfig::hackathon();
        }),
        ..Setup::default()
    }
}

/// One watcher tick after the five-minute interval.
async fn five_minutes(rig: &Rig) {
    rig.clock.advance_secs(301);
    rig.op.tick().await;
}

fn keys(rig: &Rig) -> Vec<String> {
    rig.sink.sent().into_iter().map(|a| a.key).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_credit_alarm_fires_under_the_threshold_once_and_clears_when_credit_returns() {
    let rig = Rig::new(hackathon(900_000)).await;
    assert_eq!(rig.op.minting_stop(), None);
    assert!(keys(&rig).is_empty());

    rig.provider.set_balance(450_000); // under 0.50 USD, over the 0.20 USD stop
    five_minutes(&rig).await;
    // The drop is also spend the ledger never booked.
    assert_eq!(keys(&rig), ["credit_low", "credit_drift"]);
    assert_eq!(rig.op.minting_stop(), None);
    assert!(rig.sink.sent()[0].body.contains("0.450000 USD"));
    assert!(rig.sink.sent()[0].body.contains("claim_treasury"));
    five_minutes(&rig).await;
    assert_eq!(
        keys(&rig).len(),
        2,
        "same condition, inside the repeat window"
    );

    rig.provider.set_balance(2_000_000);
    five_minutes(&rig).await;
    rig.provider.set_balance(100_000);
    five_minutes(&rig).await;
    assert_eq!(
        keys(&rig),
        [
            "credit_low",
            "credit_drift",
            "credit_low",
            "credit_stop",
            "credit_drift"
        ]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn minting_stops_under_the_stop_threshold_and_resumes_after_a_top_up() {
    let rig = Rig::new(hackathon(900_000)).await;
    rig.provider.set_balance(150_000);
    five_minutes(&rig).await;
    assert_eq!(rig.op.minting_stop(), Some("credit"));
    let (s, a, _) = rig.lease(&rig.fresh(1)).await;
    assert_eq!(
        (s, a["error"]["code"].as_str()),
        (429, Some("operator_capacity"))
    );
    assert!(rig.provider.keys().is_empty(), "no key was minted");

    rig.provider.set_balance(1_000_000);
    five_minutes(&rig).await;
    assert_eq!(rig.op.minting_stop(), None);
    let (s, body, _) = rig.lease(&rig.fresh(2)).await;
    assert_eq!(s, 201, "{body}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_default_thresholds_are_three_dollars_and_one_dollar() {
    let rig = Rig::new(Setup {
        credit: 2_500_000,
        ..Setup::default()
    })
    .await;
    assert_eq!(rig.op.minting_stop(), None);
    five_minutes(&rig).await;
    assert_eq!(keys(&rig), ["credit_low"], "2.50 USD is under 3 and over 1");
    rig.provider.set_balance(900_000);
    five_minutes(&rig).await;
    assert_eq!(rig.op.minting_stop(), Some("credit"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reading_below_the_ledger_is_an_alarm_and_a_higher_one_is_not() {
    let rig = Rig::new(Setup::default()).await; // 100 USD, default thresholds
    rig.provider.set_balance(100_000_000 + 500_000); // a top-up the ledger did not know
    five_minutes(&rig).await;
    assert!(keys(&rig).is_empty());

    rig.provider.set_balance(100_000_000 + 500_000 - 15_000); // inside the 0.02 USD drift
    five_minutes(&rig).await;
    assert!(keys(&rig).is_empty());

    rig.provider.set_balance(99_000_000); // 1.5 USD gone that nothing booked
    five_minutes(&rig).await;
    assert_eq!(keys(&rig), ["credit_drift"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_balance_that_cannot_be_read_three_times_is_an_alarm_and_the_old_figure_stays() {
    let rig = Rig::new(hackathon(900_000)).await;
    rig.provider
        .set_balance_error(Some(ProviderError::Unavailable("down".into())));
    for _ in 0..2 {
        five_minutes(&rig).await;
    }
    assert!(keys(&rig).is_empty());
    five_minutes(&rig).await;
    assert_eq!(keys(&rig), ["credit_unreadable"]);
    assert_eq!(rig.op.minting_stop(), None, "the last reading was fine");
    rig.provider.set_balance_error(None);
    five_minutes(&rig).await;
    assert_eq!(keys(&rig).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_balance_is_read_every_five_minutes_and_not_on_every_tick() {
    let rig = Rig::new(hackathon(900_000)).await;
    rig.provider.set_balance(100_000);
    rig.clock.advance_secs(200);
    rig.op.tick().await;
    assert_eq!(rig.op.minting_stop(), None, "not due yet");
    rig.clock.advance_secs(101);
    rig.op.tick().await;
    assert_eq!(rig.op.minting_stop(), Some("credit"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_start_under_the_stop_threshold_pauses_minting_at_once() {
    let rig = Rig::new(hackathon(100_000)).await;
    assert_eq!(rig.op.minting_stop(), Some("credit"));
    assert_eq!(keys(&rig), ["credit_low", "credit_stop"]);
}
