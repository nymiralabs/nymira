// The lease arithmetic the operator and the vault use, in BigInt. `charge` and `maxTokens` follow
// `nymira-operator/src/charge.rs` and `nymirad/src/sizing.rs` line for line; the unit tests hold
// the same hand-computed cases. Amounts: bound and charge are micro-USD, prices and costs nano-USD.

export interface ChargeParams {
  step: bigint;
  minCharge: bigint;
  /** Parts per million of the bound, per minute held. */
  slotRate: bigint;
}

export interface ModelSpec {
  id: string;
  inputNano: bigint;
  outputNano: bigint;
}

/** Share of the bound one request may use, in percent: 70 if the first-day overspend rule triggers. */
export const BOUND_SHARE_PCT = 90n;

const SLOT_DENOMINATOR = 60_000_000n;

const ceilDiv = (a: bigint, b: bigint): bigint => (a + b - 1n) / b;

/** `min(bound, max(min_charge, round_up_to_step(usage x 1.03 + slot fee)))`. */
export function charge(bound: bigint, costNano: bigint, heldSecs: bigint, p: ChargeParams): bigint {
  const numerator = costNano * 103n * 600n + bound * p.slotRate * heldSecs;
  const units = ceilDiv(numerator, SLOT_DENOMINATOR * p.step) * p.step;
  const floor = units > p.minCharge ? units : p.minCharge;
  return floor < bound ? floor : bound;
}

export function costNano(model: ModelSpec, prompt: bigint, completion: bigint): bigint {
  return prompt * model.inputNano + completion * model.outputNano;
}

export interface ChargeCheck {
  expected: bigint;
  reported: bigint;
  /** True when the operator charged more than one charge step above `expected`. */
  overcharged: boolean;
}

export function checkCharge(
  bound: bigint,
  cost: bigint,
  heldSecs: bigint,
  p: ChargeParams,
  reported: bigint,
): ChargeCheck {
  const expected = charge(bound, cost, heldSecs, p);
  return { expected, reported, overcharged: reported > expected + p.step };
}

/**
 * `(bound x share - estimated input cost) / output price`, in tokens; reasoning tokens are part of
 * the completion count, so they are inside the cap. `null` when the bound cannot pay for the
 * prompt and one output token.
 */
export function maxTokens(
  boundMicro: bigint,
  sharePct: bigint,
  model: ModelSpec,
  inputTokens: bigint,
): number | null {
  const budget = (boundMicro * 1000n * sharePct) / 100n;
  const room = budget - inputTokens * model.inputNano;
  if (room < 0n) return null;
  const tokens = room / (model.outputNano > 0n ? model.outputNano : 1n);
  return tokens > 0n && tokens <= BigInt(Number.MAX_SAFE_INTEGER) ? Number(tokens) : null;
}
