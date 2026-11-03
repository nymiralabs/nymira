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
/// hours of chain time (see the module docs). The seconds per block are measured
/// from each jump, because a loaded machine makes blocks longer than the nominal
/// 0.3 s and a jump sized from 0.3 s then lands past the target.
pub async fn advance_to(worker: &Worker<Sandbox>, target: u64) -> Result<()> {
    let mut seconds_per_block = CHAIN_SECONDS_PER_BLOCK;
    let mut probed = false;
    loop {
        let now = now_seconds(worker).await;
        if now == target {
            return Ok(());
        }
        if now > target {
            return Err(format!("the sandbox is already past {target} (at {now})").into());
        }
        let remaining = (target - now) as f64;
        // The first jump is a probe of at most 200 blocks: it measures the real block time.
        let blocks = if probed {
            // Aim for 90% of the way, then step one block at a time near the target.
            ((0.9 * remaining / seconds_per_block) as u64)
                .saturating_sub(5)
                .max(1)
        } else {
            ((remaining / seconds_per_block) as u64)
                .saturating_sub(5)
                .clamp(1, 200)
        }
        .min(50_000);
        let before = worker.view_block().await?.timestamp() as f64 / 1e9;
        worker.fast_forward(blocks).await?;
        let after = worker.view_block().await?.timestamp() as f64 / 1e9;
        if blocks >= 20 && after > before {
            seconds_per_block = (after - before) / blocks as f64;
            probed = true;
        }
    }
}

/// Like [`advance_to`], but a sandbox that is already at or past `target` is fine: the caller
/// only needs the clock to have reached it.
pub async fn advance_past(worker: &Worker<Sandbox>, target: u64) -> Result<()> {
    if now_seconds(worker).await >= target {
        return Ok(());
    }
    advance_to(worker, target).await
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
