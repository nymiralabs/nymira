//! How much one request may spend, and whether the operator charged what the rules say.
//!
//! Amounts: the lease bound and the charge are micro-USD; prices and costs are nano-USD.

use nymira_operator::charge::{charge, ChargeParams};
use nymira_operator::proxy::ModelSpec;

/// Share of the lease bound a single request may use, in percent. 70 replaces 90 if the first-day
/// overspend rule (day-1-checks.md) triggers.
pub const BOUND_SHARE_PCT: u128 = 90;

/// `(bound x share - estimated input cost) / output price`, in tokens. Hidden reasoning tokens
/// are part of the completion count the provider bills, so they are inside this cap. `None` when
/// the bound cannot pay for the prompt and a single output token.
pub fn max_tokens(
    bound_micro: u128,
    share_pct: u128,
    model: &ModelSpec,
    input_tokens: u128,
) -> Option<u64> {
    let budget = bound_micro * 1_000 * share_pct / 100;
    let input_cost = input_tokens * u128::from(model.input_nano_per_token);
    let room = budget.checked_sub(input_cost)?;
    let tokens = room / u128::from(model.output_nano_per_token.max(1));
    u64::try_from(tokens).ok().filter(|t| *t > 0)
}

/// What the charge check found.
#[derive(Debug, PartialEq, Eq)]
pub struct ChargeCheck {
    pub expected: u128,
    pub reported: u128,
    /// `Some` when the operator charged more than one charge step above `expected`.
    pub warning: Option<String>,
}

/// Cost in nano-USD of `prompt` and `completion` tokens (reasoning tokens are in `completion`).
pub fn cost_nano(model: &ModelSpec, prompt: u64, completion: u64) -> u128 {
    u128::from(prompt) * u128::from(model.input_nano_per_token)
        + u128::from(completion) * u128::from(model.output_nano_per_token)
}

/// Recomputes the charge from signed usage and the pinned values.
pub fn check_charge(
    bound_micro: u128,
    model: &ModelSpec,
    prompt_tokens: u64,
    completion_tokens: u64,
    held_secs: u64,
    params: ChargeParams,
    reported: u128,
) -> ChargeCheck {
    let cost = cost_nano(model, prompt_tokens, completion_tokens);
    check_charge_cost(bound_micro, cost, held_secs, params, reported)
}

/// The same from a cost already summed over models. `held_secs` is an upper bound (the operator
/// counts to the moment it deactivates the key, which the client cannot see).
pub fn check_charge_cost(
    bound_micro: u128,
    cost_nano_usd: u128,
    held_secs: u64,
    params: ChargeParams,
    reported: u128,
) -> ChargeCheck {
    let expected = charge(bound_micro, cost_nano_usd, held_secs, params);
    let warning = (reported > expected + params.step).then(|| {
        format!(
            "the operator charged {reported} micro-USD; the signed usage and the vault's rules give {expected} (more than one step of {} above)",
            params.step
        )
    });
    ChargeCheck {
        expected,
        reported,
        warning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glm() -> ModelSpec {
        ModelSpec {
            id: "z-ai/glm-5.3-flash".into(),
            input_nano_per_token: 80,
            output_nano_per_token: 500,
            output_cap_tokens: 4096,
        }
    }

    const P: ChargeParams = ChargeParams {
        step: 1_000,
        min_charge: 2_000,
        slot_rate: 1_000,
    };

    #[test]
    fn max_tokens_follows_the_formula() {
        // 0.10 USDC = 100_000 micro = 100_000_000 nano; 90% = 90_000_000; 1_000 input tokens
        // cost 80_000; (90_000_000 - 80_000) / 500 = 179_840.
        assert_eq!(max_tokens(100_000, 90, &glm(), 1_000), Some(179_840));
        // The 70% factor leaves less.
        assert_eq!(max_tokens(100_000, 70, &glm(), 1_000), Some(139_840));
    }

    #[test]
    fn a_prompt_the_bound_cannot_pay_gives_none() {
        assert_eq!(max_tokens(100_000, 90, &glm(), 2_000_000), None);
        // Exactly the budget leaves no output token.
        assert_eq!(max_tokens(100_000, 90, &glm(), 1_125_000), None);
    }

    #[test]
    fn the_charge_check_accepts_the_operators_own_number() {
        // 14 x 80 + 5 x 500 = 3_620 nano -> minimum charge applies.
        let c = check_charge(100_000, &glm(), 14, 5, 0, P, 2_000);
        assert_eq!((c.expected, c.warning), (2_000, None));
    }

    #[test]
    fn one_step_above_is_tolerated_two_is_not() {
        let ok = check_charge(100_000, &glm(), 14, 5, 0, P, 3_000);
        assert_eq!(ok.warning, None);
        let bad = check_charge(100_000, &glm(), 14, 5, 0, P, 3_001);
        assert!(bad.warning.unwrap().contains("charged 3001"));
    }

    #[test]
    fn inflated_usage_shows_up_as_a_charge_above_the_rules() {
        // Real usage 100 + 100 tokens; the operator charges as if the answer were far longer.
        let c = check_charge(100_000, &glm(), 100, 100, 0, P, 60_000);
        assert!(c.warning.is_some());
    }
}
