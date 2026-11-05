import { describe, expect, it } from 'vitest';
import { raceWallet } from '../src/wallet/watch';

const fast = { pollMs: 5, graceMs: 40, maxMs: 120 };
const never = () => new Promise<never>(() => undefined);
const after = <T>(ms: number, value: T) => new Promise<T>((r) => setTimeout(() => r(value), ms));

describe('raceWallet', () => {
  it('returns the wallet outcome when the wallet answers first', async () => {
    const r = await raceWallet(after(10, { hash: 'h' }), async () => null, fast);
    expect(r).toEqual({ by: 'wallet', outcome: { hash: 'h' } });
  });

  it('finishes from the chain when the wallet never returns', async () => {
    let polls = 0;
    const r = await raceWallet(never(), async () => (++polls >= 3 ? 'seen' : null), fast);
    expect(r).toEqual({ by: 'chain', found: 'seen' });
    expect(polls).toBe(3);
  });

  it('prefers the chain over a late rejection: the closed window had sent', async () => {
    let seen = false;
    setTimeout(() => (seen = true), 20);
    const r = await raceWallet(
      after(10, null).then(() => Promise.reject(new Error('User rejected the transaction'))),
      async () => (seen ? 'landed' : null),
      fast,
    );
    expect(r).toEqual({ by: 'chain', found: 'landed' });
  });

  it('calls a request rejected only after the grace period shows nothing', async () => {
    const started = Date.now();
    const r = await raceWallet(
      Promise.reject(new Error('User rejected the transaction')),
      async () => null,
      fast,
    );
    expect(r.by).toBe('rejected');
    expect(r.by === 'rejected' && r.error.message).toBe('User rejected the transaction');
    expect(Date.now() - started).toBeGreaterThanOrEqual(fast.graceMs - 5);
  });

  it('ends in a time-out when the wallet is silent and the chain shows nothing', async () => {
    const r = await raceWallet(never(), async () => null, fast);
    expect(r).toEqual({ by: 'timeout' });
  });

  it('keeps going when the chain read fails', async () => {
    let polls = 0;
    const r = await raceWallet(
      never(),
      async () => {
        if (++polls < 3) throw new Error('rpc down');
        return 'ok';
      },
      fast,
    );
    expect(r).toEqual({ by: 'chain', found: 'ok' });
  });
});
