//! Time control for tests.
//!
//! **Sandbox.** `fast_forward(n)` advances the chain by `n` blocks. Measured on
//! near-sandbox 0.3.16 (2026-11-01): one block is about 0.3 s of chain time
//! ([`CHAIN_SECONDS_PER_BLOCK`]) and about 11 ms of wall time, whatever the
//! configured block delay. So 30 s of chain time is about 100 blocks (about 1 s),
//! 48 h about 576,000 blocks (about 1.8 h), 7 days about 2 million (about 6 h)
//! and 30 days about 8.6 million (about 27 h). [`advance_to`] lands exactly on a
//! chosen second and is the sandbox tool for the 30-second rule.
//!
//! **Exact time.** The 48-hour, 7-day and 30-day rules are tested at their
//! boundaries under `testing_env!` ([`testing_env_at`]), where
//! `block_timestamp` is whatever the test sets. That environment runs the real
//! `alt_bn128` host functions, so proofs verify there. A note's expiry is
//! rounded to the end of a UTC day ([`end_of_utc_day`]); tests that need an
//! expired note set the time past that expiry instead of using a short TTL.

use near_sdk::test_utils::VMContextBuilder;
use near_sdk::testing_env;
use near_workspaces::network::Sandbox;
use near_workspaces::Worker;

pub const THIRTY_SECONDS: u64 = 30;
pub const FORTY_EIGHT_HOURS: u64 = 48 * 3600;
pub const SEVEN_DAYS: u64 = 7 * 86_400;
pub const THIRTY_DAYS: u64 = 30 * 86_400;

/// Chain seconds per sandbox block, as measured (see the module docs).
pub const CHAIN_SECONDS_PER_BLOCK: f64 = 0.3;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// The sandbox's latest block time in Unix seconds (`block_timestamp / 10^9`,
/// rounded down), the unit every circuit-facing time uses.
pub async fn now_seconds(worker: &Worker<Sandbox>) -> u64 {
    worker.view_block().await.expect("latest block").timestamp() / 1_000_000_000
}

/// Advances the sandbox until `now_seconds` equals `target` (never beyond it).
/// Steps whole blocks, so the final block lands within 0.3 s after the start of
/// `target`. Errors if `target` is already in the past. Practical up to a few
/// hours of chain time (see the module docs).
pub async fn advance_to(worker: &Worker<Sandbox>, target: u64) -> Result<()> {
    loop {
        let now = now_seconds(worker).await;
        if now == target {
            return Ok(());
        }
        if now > target {
            return Err(format!("the sandbox is already past {target} (at {now})").into());
        }
        let remaining = (target - now) as f64;
        // Jump most of the way, then step one block at a time near the target.
        let blocks = ((remaining / CHAIN_SECONDS_PER_BLOCK) as u64)
            .saturating_sub(5)
            .max(1);
        worker.fast_forward(blocks.min(50_000)).await?;
    }
}

/// A `testing_env!` whose `block_timestamp` is `unix_seconds`, exactly.
pub fn testing_env_at(unix_seconds: u64) {
    testing_env!(VMContextBuilder::new()
        .block_timestamp(unix_seconds * 1_000_000_000)
        .build());
}

/// The last second of the UTC day that contains `unix_seconds`: how the vault
/// rounds a note's expiry.
pub fn end_of_utc_day(unix_seconds: u64) -> u64 {
    (unix_seconds / 86_400 + 1) * 86_400 - 1
}
