import { expect, it } from 'vitest';
import { charge, costNano } from '../src/chat/charge';
import { MODELS } from '../src/chat/models';
import { profile as testnet } from '../src/profiles/testnet';

// The worked examples on the documentation page, with the testnet vault's parameters.
const c = testnet.pins!.config;
const params = {
  step: BigInt(c.charge_step),
  minCharge: BigInt(c.min_charge),
  slotRate: BigInt(c.slot_rate),
};
const glm = MODELS[0]!;

it('a short chat pays the minimum charge', () => {
  expect(charge(100_000n, costNano(glm, 1_200n, 300n), 90n, params)).toBe(2_000n);
});

it('a longer chat rounds up to the next step', () => {
  expect(charge(100_000n, costNano(glm, 20_000n, 4_000n), 300n, params)).toBe(5_000n);
  expect(charge(100_000n, costNano(glm, 120_000n, 8_000n), 600n, params)).toBe(16_000n);
});

it('the vault parameters the page states', () => {
  expect(params).toEqual({ step: 1_000n, minCharge: 2_000n, slotRate: 1_000n });
  expect(c.bound_classes).toEqual(['100000', '500000']);
  expect(c.note_fee).toBe('50000');
  expect(c.close_fee).toBe('30000');
  expect(c.exit_delay_secs).toBe(604800);
  expect(c.note_ttl).toBe(2592000);
});
