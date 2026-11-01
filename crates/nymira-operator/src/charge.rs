//! The charge of a lease: `min(bound, max(min_charge, round_up_to_step(usage x 1.03 + slot fee)))`.
//!
//! Amounts are micro-USD (USDC base units); usage arrives in nano-USD. Everything is integer
//! arithmetic with one rounding, up, at the end.

/// The vault's lease parameters, as `config()` returns them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChargeParams {
    pub step: u128,
    pub min_charge: u128,
    /// Parts per million of the bound, per minute held.
    pub slot_rate: u32,
}

/// Seconds the usage figure and the slot fee are expressed in: 6e7 = 1e6 (parts) x 60 (seconds).
const SLOT_DENOMINATOR: u128 = 60_000_000;

/// The charge of a lease that was used (or ran its course) and whose measured usage is
/// `cost_nano_usd`, held for `held_secs`. A lease closed with `expire` before any request pays 0
/// and never reaches this function.
pub fn charge(bound: u128, cost_nano_usd: u128, held_secs: u64, p: ChargeParams) -> u128 {
    // total_micro = cost_nano x 1.03 / 1000 + bound x slot_rate x held / 6e7, over the common
    // denominator 6e7 (cost_nano x 103 / 1e5 = cost_nano x 103 x 600 / 6e7).
    let numerator =
        cost_nano_usd * 103 * 600 + bound * u128::from(p.slot_rate) * u128::from(held_secs);
    let units = numerator.div_ceil(SLOT_DENOMINATOR * p.step) * p.step;
    units.max(p.min_charge).min(bound)
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: ChargeParams = ChargeParams {
        step: 1_000,
        min_charge: 2_000,
        slot_rate: 1_000,
    };

    #[test]
    fn hand_computed_cases() {
        // 0.04 USD of usage: 41_200 micro with the 3% margin; 60 s of 0.1% per minute of 0.10 is
        // 100 micro; 41_300 rounds up to 42_000.
        assert_eq!(charge(100_000, 40_000_000, 60, P), 42_000);
        // 10_000 micro x 1.03 = 10_300 rounds up to 11_000.
        assert_eq!(charge(100_000, 10_000_000, 0, P), 11_000);
        // Slot fee alone: 600 s at 0.1% per minute of 0.10 USDC = 1_000 micro.
        assert_eq!(
            charge(100_000, 0, 600, P),
            2_000,
            "minimum applies above 1_000"
        );
        // An exact multiple of the step does not round up: 600 s of 0.1% per minute of 0.50.
        assert_eq!(charge(500_000, 0, 600, P), 5_000);
        assert_eq!(charge(100_000, 8_000_000, 60, P), 9_000);
    }

    #[test]
    fn the_minimum_applies_to_an_unused_lease_that_ran_its_course() {
        assert_eq!(charge(100_000, 0, 0, P), 2_000);
        assert_eq!(charge(100_000, 0, 120, P), 2_000);
    }

    #[test]
    fn the_bound_caps_the_charge() {
        assert_eq!(charge(100_000, 500_000_000, 600, P), 100_000);
        assert_eq!(charge(100_000, u128::from(u64::MAX), 600, P), 100_000);
        // A minimum above the bound still cannot exceed it.
        assert_eq!(charge(1_500, 0, 0, P), 1_500);
    }

    #[test]
    fn a_finer_step_rounds_less() {
        let fine = ChargeParams { step: 1, ..P };
        assert_eq!(charge(100_000, 40_000_000, 60, fine), 41_300);
    }
}
