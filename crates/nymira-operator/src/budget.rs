//! Provider credit, the daily provider budget and the key-minting token bucket.
//!
//! Amounts are micro-USD (USDC base units), the unit of lease bounds and charges.
//!
//! Live credit is the last provider reading minus the charges settled since. Admission needs
//! `live credit - open bounds - reserve >= new bound`. The daily budget counts the day's settled
//! charges plus the cost reserved by in-flight requests (never lease bounds) and admits a new
//! lease only while that sum is below the limit.

use std::collections::HashMap;
use std::sync::Mutex;

pub const SECS_PER_DAY: u64 = 86_400;

/// Why admission said no, with the wait the client should honour.
#[derive(Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The daily provider budget is used up: wait for the next UTC midnight.
    DailyBudget { retry_after_secs: u64 },
    /// Credit (after open bounds and the reserve) cannot cover the bound, or the 0.50 class
    /// would exceed its share.
    Credit,
    /// The key-minting bucket is empty.
    Minting,
}

#[derive(Clone, Copy, Debug)]
pub struct BudgetConfig {
    /// Daily provider budget in micro-USD (default 0.25 USD).
    pub daily_limit: u128,
    /// Credit kept back from admission (1 USD).
    pub reserve: u128,
    /// Bounds at or above this are capped at `1 / large_share_divisor` of live credit.
    pub large_class_from: u128,
    pub large_share_divisor: u128,
    pub bucket_capacity: u32,
    pub bucket_refill_per_sec: f64,
}

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            daily_limit: 250_000,
            reserve: 1_000_000,
            large_class_from: 500_000,
            large_share_divisor: 5,
            bucket_capacity: 10,
            bucket_refill_per_sec: 1.0,
        }
    }
}

impl BudgetConfig {
    /// The hackathon deployment: 1 USD of provider credit in total, so a 0.20 USD reserve and
    /// 0.05 USD a day (about 0.70 USD over the 14 days). The 0.50 class stays closed until live
    /// credit reaches 2.50 USD, whatever the config.
    pub fn hackathon() -> Self {
        Self {
            daily_limit: 50_000,
            reserve: 200_000,
            ..Self::default()
        }
    }
}

/// Credit thresholds in micro-USD for the watcher: an alarm under `alarm_below`, no new leases
/// under `stop_below`, and an alarm when a reading is `drift` under what the ledger expected.
#[derive(Clone, Copy, Debug)]
pub struct CreditConfig {
    pub alarm_below: u128,
    pub stop_below: u128,
    pub drift: u128,
}

impl Default for CreditConfig {
    fn default() -> Self {
        Self {
            alarm_below: 3_000_000,
            stop_below: 1_000_000,
            drift: 20_000,
        }
    }
}

impl CreditConfig {
    /// The hackathon deployment (1 USD of credit in total): alarm at 0.50 USD, stop at 0.20 USD.
    pub fn hackathon() -> Self {
        Self {
            alarm_below: 500_000,
            stop_below: 200_000,
            ..Self::default()
        }
    }
}

struct Inner {
    /// Provider balance at the last reading, less charges settled since.
    credit: u128,
    open_bounds: HashMap<String, u128>,
    day: u64,
    day_settled: u128,
    inflight: HashMap<String, u128>,
    tokens: f64,
    bucket_ms: u64,
}

pub struct Ledger {
    cfg: BudgetConfig,
    inner: Mutex<Inner>,
}

impl Ledger {
    pub fn new(cfg: BudgetConfig, credit: u128, now_ms: u64) -> Self {
        Self {
            cfg,
            inner: Mutex::new(Inner {
                credit,
                open_bounds: HashMap::new(),
                day: now_ms / 1000 / SECS_PER_DAY,
                day_settled: 0,
                inflight: HashMap::new(),
                tokens: f64::from(cfg.bucket_capacity),
                bucket_ms: now_ms,
            }),
        }
    }

    fn inner(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn roll_day(inner: &mut Inner, now_secs: u64) {
        let day = now_secs / SECS_PER_DAY;
        if day != inner.day {
            inner.day = day;
            inner.day_settled = 0;
        }
    }

    /// A fresh provider reading replaces the corrected figure.
    pub fn set_balance(&self, balance: u128) {
        self.inner().credit = balance;
    }

    /// Checks every admission rule and, if they pass, books `bound` as open under `lease_id`.
    pub fn admit(&self, lease_id: &str, bound: u128, now_ms: u64) -> Result<(), Refusal> {
        let mut inner = self.inner();
        let now_secs = now_ms / 1000;
        Self::roll_day(&mut inner, now_secs);
        let used = inner.day_settled + inner.inflight.values().sum::<u128>();
        if used >= self.cfg.daily_limit {
            return Err(Refusal::DailyBudget {
                retry_after_secs: (inner.day + 1) * SECS_PER_DAY - now_secs,
            });
        }
        let open: u128 = inner.open_bounds.values().sum();
        let free = inner
            .credit
            .saturating_sub(open)
            .saturating_sub(self.cfg.reserve);
        if free < bound {
            return Err(Refusal::Credit);
        }
        if bound >= self.cfg.large_class_from && bound * self.cfg.large_share_divisor > inner.credit
        {
            return Err(Refusal::Credit);
        }
        let elapsed = now_ms.saturating_sub(inner.bucket_ms) as f64 / 1000.0;
        inner.tokens = (inner.tokens + elapsed * self.cfg.bucket_refill_per_sec)
            .min(f64::from(self.cfg.bucket_capacity));
        inner.bucket_ms = now_ms;
        if inner.tokens < 1.0 {
            return Err(Refusal::Minting);
        }
        inner.tokens -= 1.0;
        inner.open_bounds.insert(lease_id.to_string(), bound);
        Ok(())
    }

    /// The lease ended without a charge (rolled back or expired unused).
    pub fn release(&self, lease_id: &str) {
        let mut inner = self.inner();
        inner.open_bounds.remove(lease_id);
        inner.inflight.remove(lease_id);
    }

    /// Re-books a lease found at start-up.
    pub fn restore(&self, lease_id: &str, bound: u128) {
        self.inner().open_bounds.insert(lease_id.to_string(), bound);
    }

    /// Sets the cost reserved by the lease's in-flight requests.
    pub fn set_inflight(&self, lease_id: &str, cost: u128) {
        self.inner().inflight.insert(lease_id.to_string(), cost);
    }

    /// Settlement: the open bound and the in-flight reservation give way to the charge, which
    /// now counts against the day and against live credit.
    pub fn settle(&self, lease_id: &str, charge: u128, now_secs: u64) {
        let mut inner = self.inner();
        Self::roll_day(&mut inner, now_secs);
        inner.open_bounds.remove(lease_id);
        inner.inflight.remove(lease_id);
        inner.day_settled += charge;
        inner.credit = inner.credit.saturating_sub(charge);
    }

    /// Charges settled earlier today, read back from the store after a restart.
    pub fn restore_settled(&self, amount: u128, now_secs: u64) {
        let mut inner = self.inner();
        Self::roll_day(&mut inner, now_secs);
        inner.day_settled += amount;
    }

    pub fn live_credit(&self) -> u128 {
        self.inner().credit
    }

    pub fn open_bounds(&self) -> u128 {
        self.inner().open_bounds.values().sum()
    }

    /// `(limit, used, resets_ts)` for `/v1/status`.
    pub fn daily(&self, now_secs: u64) -> (u128, u128, u64) {
        let mut inner = self.inner();
        Self::roll_day(&mut inner, now_secs);
        let used = inner.day_settled + inner.inflight.values().sum::<u128>();
        (self.cfg.daily_limit, used, (inner.day + 1) * SECS_PER_DAY)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 1_000 * SECS_PER_DAY; // a UTC midnight, in seconds
    fn at(secs: u64) -> u64 {
        (DAY + secs) * 1000
    }

    fn ledger(credit: u128) -> Ledger {
        Ledger::new(BudgetConfig::default(), credit, at(0))
    }

    #[test]
    fn credit_minus_open_bounds_minus_reserve_must_cover_the_bound() {
        let l = ledger(1_300_000);
        // free = 1.30 - 1.00 = 0.30
        assert_eq!(l.admit("a", 100_000, at(0)), Ok(()));
        assert_eq!(l.admit("b", 100_000, at(0)), Ok(()));
        assert_eq!(l.admit("c", 100_000, at(0)), Ok(()));
        assert_eq!(l.admit("d", 100_000, at(0)), Err(Refusal::Credit));
        l.release("a");
        assert_eq!(l.admit("d", 100_000, at(0)), Ok(()));
    }

    #[test]
    fn the_large_class_is_capped_at_a_fifth_of_live_credit() {
        let l = ledger(2_400_000); // a fifth is 0.48: 0.50 is too much although credit is free
        assert_eq!(l.admit("a", 500_000, at(0)), Err(Refusal::Credit));
        l.set_balance(2_500_000);
        assert_eq!(l.admit("a", 500_000, at(0)), Ok(()));
    }

    #[test]
    fn the_daily_budget_counts_settled_charges_and_inflight_cost_not_bounds() {
        let l = ledger(5_000_000);
        // A 0.50 lease opens under a 0.25 budget: bounds are not counted.
        assert_eq!(l.admit("big", 500_000, at(10)), Ok(()));
        assert_eq!(l.daily(DAY + 10).1, 0);
        // In-flight request cost counts.
        l.set_inflight("big", 200_000);
        assert_eq!(l.daily(DAY + 10).1, 200_000);
        assert_eq!(l.admit("b", 100_000, at(10)), Ok(()));
        // Settlement moves it from reserved cost to the charge (the actual figure, not the bound).
        l.settle("big", 260_000, DAY + 20);
        assert_eq!(l.daily(DAY + 20).1, 260_000);
        assert_eq!(
            l.admit("c", 100_000, at(20)),
            Err(Refusal::DailyBudget {
                retry_after_secs: SECS_PER_DAY - 20
            })
        );
        // Midnight resets it.
        assert_eq!(l.daily(DAY + SECS_PER_DAY).1, 0);
        assert_eq!(l.admit("c", 100_000, at(SECS_PER_DAY)), Ok(()));
        assert_eq!(l.daily(DAY + 20).2, DAY + SECS_PER_DAY);
    }

    #[test]
    fn settlement_lowers_live_credit_by_the_charge() {
        let l = ledger(5_000_000);
        l.admit("a", 100_000, at(0)).unwrap();
        l.settle("a", 2_000, DAY);
        assert_eq!(l.live_credit(), 4_998_000);
        assert_eq!(l.open_bounds(), 0);
    }

    #[test]
    fn the_hackathon_profile_admits_leases_on_one_dollar_of_credit() {
        let l = Ledger::new(BudgetConfig::hackathon(), 1_000_000, at(0));
        // 1.00 - 0.20 reserve leaves 0.80: eight 0.10 bounds, not a ninth.
        for n in 0..8 {
            assert_eq!(
                l.admit(&format!("l{n}"), 100_000, at(0)),
                Ok(()),
                "lease {n}"
            );
        }
        assert_eq!(l.admit("l8", 100_000, at(0)), Err(Refusal::Credit));
        // The 0.50 class needs 2.50 USD of live credit.
        let big = Ledger::new(BudgetConfig::hackathon(), 1_000_000, at(0));
        assert_eq!(big.admit("b", 500_000, at(0)), Err(Refusal::Credit));
        // The default profile admits nothing on 1 USD: the reserve eats it all.
        let default = ledger(1_000_000);
        assert_eq!(default.admit("a", 100_000, at(0)), Err(Refusal::Credit));
    }

    #[test]
    fn the_minting_bucket_refills_over_time() {
        let cfg = BudgetConfig {
            bucket_capacity: 2,
            bucket_refill_per_sec: 0.5,
            ..Default::default()
        };
        let l = Ledger::new(cfg, 100_000_000, at(0));
        assert_eq!(l.admit("a", 100_000, at(0)), Ok(()));
        assert_eq!(l.admit("b", 100_000, at(0)), Ok(()));
        assert_eq!(l.admit("c", 100_000, at(0)), Err(Refusal::Minting));
        assert_eq!(l.admit("c", 100_000, at(2)), Ok(()));
    }
}
