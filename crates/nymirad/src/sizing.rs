//! How much one request may spend, and whether the operator charged what the rules say.
//!
//! Amounts: the lease bound and the charge are micro-USD; prices and costs are nano-USD.

use nymira_operator::charge::{charge, ChargeParams};
use nymira_operator::proxy::ModelSpec;

/// Share of the lease bound a single request may use, in percent. 70 replaces 90 if the first-day
/// overspend rule (day-1-checks.md) triggers.
pub const BOUND_SHARE_PCT: u128 = 90;

/// Upper bound on the prompt's token count from its size: a token is at least one byte, and
/// English text averages about four, so bytes / 2 stays above the real count for any usual text.
/// shortcut: a tokenizer would size the input tighter; it only matters for very long prompts.
pub fn estimate_input_tokens(message_bytes: &[usize]) -> u128 {
    message_bytes
        .iter()
        .map(|b| (*b as u128).div_ceil(2) + 4)
        .sum::<u128>()
        + 16
}

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
    let cost_nano = u128::from(prompt_tokens) * u128::from(model.input_nano_per_token)
        + u128::from(completion_tokens) * u128::from(model.output_nano_per_token);
    let expected = charge(bound_micro, cost_nano, held_secs, params);
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
    fn the_input_estimate_never_undercounts_plain_text() {
        let text = "hello world, this is a short prompt";
        let real_upper = text.split_whitespace().count() as u128 * 2;
        assert!(estimate_input_tokens(&[text.len()]) >= real_upper);
        assert_eq!(estimate_input_tokens(&[]), 16);
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
