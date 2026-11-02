//! The provider watcher: credit, prices and the per-lease cost check. It raises alarms and
//! decides when key minting stops.
//!
//! Minting stops while any of these holds: the credit is under the stop threshold, a pinned
//! model's live price differs from its pin, or a lease's reported provider cost came out above
//! what the proxy estimated from the pinned prices (the last one lasts until the operator is
//! restarted: a person looks at it first).

use std::collections::HashSet;

use crate::operator::Operator;

#[derive(Default)]
pub(crate) struct Stops {
    credit: bool,
    prices: HashSet<String>,
    cost: bool,
    balance_failures: u32,
    model_failures: u32,
}

/// Failed reads in a row before the "unreadable" alarm.
const FAILURES_BEFORE_ALARM: u32 = 3;
/// A lease cost may exceed the estimate by this share plus [`COST_SLACK_NANO`] before it counts.
const COST_SLACK_PERCENT: u128 = 1;
const COST_SLACK_NANO: u128 = 1_000;

fn usd(micro: u128) -> String {
    format!("{}.{:06} USD", micro / 1_000_000, micro % 1_000_000)
}

impl Operator {
    /// Why new leases are paused, if they are.
    pub fn minting_stop(&self) -> Option<&'static str> {
        let stops = self.stops.lock().unwrap_or_else(|e| e.into_inner());
        if stops.credit {
            Some("credit")
        } else if !stops.prices.is_empty() {
            Some("price")
        } else if stops.cost {
            Some("cost")
        } else {
            None
        }
    }

    /// Compares a fresh reading with what the ledger expected and with the thresholds.
    pub(crate) async fn judge_credit(&self, expected: u128, balance: u128) {
        let cfg = self.config().credit;
        self.stops
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .balance_failures = 0;
        self.alarms.resolve("credit_unreadable");
        if balance < cfg.alarm_below {
            self.alarms
                .raise(
                    "credit_low",
                    "NEAR AI credit is low",
                    &format!(
                        "The organisation has {} left; the alarm is under {}. Top it up the same day: by hand from the NEAR AI dashboard on testnet, from the treasury after claim_treasury on a mainnet vault.",
                        usd(balance),
                        usd(cfg.alarm_below)
                    ),
                )
                .await;
        } else {
            self.alarms.resolve("credit_low");
        }
        let stop = balance < cfg.stop_below;
        self.stops.lock().unwrap_or_else(|e| e.into_inner()).credit = stop;
        if stop {
            self.alarms
                .raise(
                    "credit_stop",
                    "NEAR AI credit: new leases are paused",
                    &format!(
                        "The organisation has {} left, under the stop threshold of {}. No key is minted until the credit is topped up.",
                        usd(balance),
                        usd(cfg.stop_below)
                    ),
                )
                .await;
        } else {
            self.alarms.resolve("credit_stop");
        }
        // Lower than the ledger expected means spend the ledger does not know about.
        if expected > balance && expected - balance > cfg.drift {
            self.alarms
                .raise(
                    "credit_drift",
                    "NEAR AI credit is below the operator's ledger",
                    &format!(
                        "The provider reports {}; the ledger expected {}. Something spent {} the operator did not book.",
                        usd(balance),
                        usd(expected),
                        usd(expected - balance)
                    ),
                )
                .await;
        } else {
            self.alarms.resolve("credit_drift");
        }
    }

    pub(crate) async fn balance_unreadable(&self) {
        let failures = {
            let mut stops = self.stops.lock().unwrap_or_else(|e| e.into_inner());
            stops.balance_failures += 1;
            stops.balance_failures
        };
        if failures >= FAILURES_BEFORE_ALARM {
            self.alarms
                .raise(
                    "credit_unreadable",
                    "NEAR AI credit cannot be read",
                    &format!("{failures} balance reads in a row failed. The ledger keeps counting from the last reading."),
                )
                .await;
        }
    }

    pub(crate) async fn models_unreadable(&self) {
        let failures = {
            let mut stops = self.stops.lock().unwrap_or_else(|e| e.into_inner());
            stops.model_failures += 1;
            stops.model_failures
        };
        if failures >= FAILURES_BEFORE_ALARM {
            self.alarms
                .raise(
                    "models_unreadable",
                    "NEAR AI model list cannot be read",
                    &format!("{failures} reads of the live model list in a row failed; the pinned prices are not being checked."),
                )
                .await;
        }
    }

    /// `blocked`: pinned models missing, not attested, or priced differently. `priced`: the ones
    /// whose price differs, which also stop minting.
    pub(crate) async fn judge_models(&self, blocked: &HashSet<String>, priced: HashSet<String>) {
        {
            let mut stops = self.stops.lock().unwrap_or_else(|e| e.into_inner());
            stops.model_failures = 0;
            stops.prices = priced.clone();
        }
        self.alarms.resolve("models_unreadable");
        if blocked.is_empty() {
            self.alarms.resolve("model_mismatch");
            return;
        }
        let mut ids: Vec<&String> = blocked.iter().collect();
        ids.sort();
        let list = ids
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        let note = if priced.is_empty() {
            "The proxy refuses these models; leases still mint."
        } else {
            "A live price differs from its pin, so no key is minted until the pin or the price changes."
        };
        self.alarms
            .raise(
                "model_mismatch",
                "NEAR AI models differ from the pinned table",
                &format!("Not matching the pin (missing, not attested, or another price): {list}. {note}"),
            )
            .await;
    }

    /// A lease's provider cost (nano-USD) against the proxy's estimate from the pinned prices.
    /// A lease the spend book does not know (after a restart) has no estimate and is skipped.
    pub(crate) async fn judge_lease_cost(&self, lease_id: &str, provider_nano: u128) {
        let known = self
            .spend
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .knows(lease_id);
        if !known {
            return;
        }
        let estimate_nano = self.spent_estimate(lease_id) * 1000;
        if provider_nano <= estimate_nano * (100 + COST_SLACK_PERCENT) / 100 + COST_SLACK_NANO {
            return;
        }
        self.stops.lock().unwrap_or_else(|e| e.into_inner()).cost = true;
        self.alarms
            .raise(
                "lease_cost_mismatch",
                "A lease cost more than the pinned prices explain",
                &format!(
                    "The provider billed {provider_nano} nano-USD for lease {lease_id}; the estimate from the pinned prices is {estimate_nano}. New leases stay paused until the operator is restarted."
                ),
            )
            .await;
    }
}
