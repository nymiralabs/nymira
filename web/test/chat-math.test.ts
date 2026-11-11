import { describe, expect, it } from 'vitest';
import { charge, checkCharge, costNano, maxTokens, type ChargeParams } from '../src/chat/charge';
import { MODELS, modelById, chooseBound } from '../src/chat/models';
import { solvePuzzle, leadingZeroBits } from '../src/chat/puzzle';
import {
  GateError,
  checkParams,
  puzzleDifficulty,
  requestTime,
  verifyStatus,
} from '../src/chat/gate';
import { sha256 } from '@noble/hashes/sha2.js';
import type { VaultConfigPin } from '../src/profiles/types';

const P: ChargeParams = { step: 1000n, minCharge: 2000n, slotRate: 1000n };
const glm = modelById('z-ai/glm-5.3-flash')!;

describe('charge (the same cases as nymira-operator charge.rs)', () => {
  it('hand computed cases', () => {
    expect(charge(100_000n, 40_000_000n, 60n, P)).toBe(42_000n);
    expect(charge(100_000n, 10_000_000n, 0n, P)).toBe(11_000n);
    expect(charge(100_000n, 0n, 600n, P)).toBe(2_000n);
    expect(charge(500_000n, 0n, 600n, P)).toBe(5_000n);
    expect(charge(100_000n, 8_000_000n, 60n, P)).toBe(9_000n);
    expect(charge(100_000n, 0n, 0n, P)).toBe(2_000n);
    expect(charge(100_000n, 500_000_000n, 600n, P)).toBe(100_000n);
    expect(charge(1_500n, 0n, 0n, P)).toBe(1_500n);
    expect(charge(100_000n, 40_000_000n, 60n, { ...P, step: 1n })).toBe(41_300n);
  });

  it('warns only when the charge is more than one step above the rules', () => {
    const cost = costNano(glm, 14n, 5n);
    expect(checkCharge(100_000n, cost, 0n, P, 3_000n).overcharged).toBe(false);
    expect(checkCharge(100_000n, cost, 0n, P, 3_001n).overcharged).toBe(true);
    expect(checkCharge(100_000n, costNano(glm, 100n, 100n), 0n, P, 60_000n).overcharged).toBe(true);
  });
});

describe('maxTokens (nymirad sizing.rs)', () => {
  it('follows (bound x share - input cost) / output price', () => {
    expect(maxTokens(100_000n, 90n, glm, 1_000n)).toBe(179_840);
    expect(maxTokens(100_000n, 70n, glm, 1_000n)).toBe(139_840);
  });
  it('is null when the bound cannot pay for the prompt and one token', () => {
    expect(maxTokens(100_000n, 90n, glm, 2_000_000n)).toBeNull();
    expect(maxTokens(100_000n, 90n, glm, 1_125_000n)).toBeNull();
  });
});

describe('models', () => {
  it('lists the four pinned models and picks the class', () => {
    expect(MODELS.map((m) => m.id)).toContain('z-ai/glm-5.3-flash');
    expect(chooseBound([100_000n, 500_000n], false)).toBe(100_000n);
    expect(chooseBound([100_000n, 500_000n], true)).toBe(500_000n);
    expect(chooseBound([500_000n], false)).toBeNull();
  });
});

describe('hashcash', () => {
  it('solves at the asked difficulty and binds to the nonce', () => {
    const value = solvePuzzle('aabb', 10);
    const [nonce, counter] = value.split(':');
    expect(nonce).toBe('aabb');
    expect(
      leadingZeroBits(sha256(new TextEncoder().encode(`aabb:${counter}`))),
    ).toBeGreaterThanOrEqual(10);
    expect(leadingZeroBits(new Uint8Array([0, 0b0010_0000]))).toBe(10);
  });
});

describe('status gate', () => {
  const pk = { x: '0x1', y: '0x2' };
  const NOW = 1_800_000_000;
  const status = (ts: number) => ({
    nonce: { nonce: 'aabb', host: 'op.test', ts },
    nonce_sig: { r: 1 },
    bound_classes: ['100000', '500000'],
    charge_step: '1000',
    min_charge: '2000',
    slot_rate: 1000,
    future_field: { anything: 1 },
    admission: { puzzle_difficulty: 8 },
  });
  const pinned = {
    bound_classes: ['100000', '500000'],
    charge_step: '1000',
    min_charge: '2000',
    slot_rate: 1000,
  } as unknown as VaultConfigPin;

  it('accepts a verified fresh nonce and ignores unknown fields', () => {
    const n = verifyStatus(status(NOW - 3), pk, NOW, () => true);
    expect(n.host).toBe('op.test');
    expect(() => checkParams(status(NOW), pinned)).not.toThrow();
    expect(puzzleDifficulty(status(NOW))).toBe(8);
  });

  it('refuses a bad signature, an old nonce and a missing one', () => {
    const kind = (f: () => unknown) => {
      try {
        f();
      } catch (e) {
        return (e as GateError).refusal.kind;
      }
      return 'none';
    };
    expect(kind(() => verifyStatus(status(NOW), pk, NOW, () => false))).toBe('bad_signature');
    expect(kind(() => verifyStatus(status(NOW - 31), pk, NOW, () => true))).toBe('stale');
    expect(kind(() => verifyStatus({ ...status(NOW), nonce: null }, pk, NOW, () => true))).toBe(
      'no_nonce',
    );
    expect(
      kind(() =>
        verifyStatus(status(NOW), pk, NOW, () => {
          throw new Error('bad json');
        }),
      ),
    ).toBe('bad_signature');
  });

  it('names each changed parameter', () => {
    const live = { ...status(NOW), charge_step: '500', slot_rate: 2000, min_charge: null };
    try {
      checkParams(live, pinned);
      throw new Error('should refuse');
    } catch (e) {
      expect((e as GateError).refusal).toEqual({
        kind: 'params',
        fields: ['charge_step', 'min_charge', 'slot_rate'],
      });
    }
  });

  it('puts request_time on the 60 s grid', () => {
    expect(requestTime({ nonce: 'a', host: 'h', ts: 1_800_000_119 })).toBe(1_800_000_060);
  });
});

describe('estimateInputTokens (operator proxy.rs counts roles too)', () => {
  it('counts the role bytes and a rounding margin on top of the content', async () => {
    const { estimateInputTokens } = await import('../src/chat/session');
    expect(estimateInputTokens([{ role: 'user', content: 'hi' }])).toBe(2n + 4n + 20n);
    expect(
      estimateInputTokens([
        { role: 'user', content: 'hi' },
        { role: 'assistant', content: 'ok' },
      ]),
    ).toBe(2n + 4n + 2n + 9n + 20n);
  });
});

describe('Typed answer duration', () => {
  it('is about 11 ms a character, between 0.4 s and 3.5 s', async () => {
    const { typeDuration } = await import('../src/ui/Typed');
    expect(typeDuration(0)).toBe(400);
    expect(typeDuration(100)).toBe(1100);
    expect(typeDuration(100_000)).toBe(3500);
  });
});
