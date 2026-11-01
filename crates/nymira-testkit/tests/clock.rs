//! Self-test of sandbox time control and of the exact-time helper.

use near_sdk::env;
use nymira_testkit::clock::{
    advance_to, end_of_utc_day, now_seconds, testing_env_at, FORTY_EIGHT_HOURS, SEVEN_DAYS,
    THIRTY_DAYS, THIRTY_SECONDS,
};

#[test]
fn rule_lengths_in_seconds() {
    assert_eq!(THIRTY_SECONDS, 30);
    assert_eq!(FORTY_EIGHT_HOURS, 48 * 3600);
    assert_eq!(SEVEN_DAYS, 7 * 86_400);
    assert_eq!(THIRTY_DAYS, 30 * 86_400);
}

#[test]
fn end_of_utc_day_is_the_last_second_of_the_day() {
    // 2026-11-01T13:00:00Z
    assert_eq!(end_of_utc_day(1_793_538_000), 1_793_577_599);
    assert_eq!(end_of_utc_day(1_793_577_599), 1_793_577_599);
    assert_eq!(end_of_utc_day(1_793_577_600), 1_793_663_999);
}

#[test]
fn testing_env_at_sets_the_block_timestamp_exactly() {
    let limit = 1_800_000_000;
    testing_env_at(limit - 1);
    assert_eq!(env::block_timestamp() / 1_000_000_000, limit - 1);
    testing_env_at(limit);
    assert_eq!(env::block_timestamp(), limit * 1_000_000_000);
}

#[tokio::test]
async fn sandbox_lands_one_second_before_and_at_a_limit() {
    let worker = near_workspaces::sandbox().await.unwrap();
    let start = now_seconds(&worker).await;
    let limit = start + THIRTY_SECONDS;
    advance_to(&worker, limit - 1).await.unwrap();
    assert_eq!(now_seconds(&worker).await, limit - 1, "one second before the limit");
    advance_to(&worker, limit).await.unwrap();
    assert_eq!(now_seconds(&worker).await, limit, "at the limit");
}
