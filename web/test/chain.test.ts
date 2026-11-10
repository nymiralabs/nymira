import { describe, expect, it } from 'vitest';
import { NetworkError, RpcClient, RpcError } from '../src/chain/rpc';
import { depositEvents, hasReceiptTree, matchDeposit, txHashOf } from '../src/chain/receipt';
import { canonical, checkVault, diffVault, type VaultSnapshot } from '../src/chain/vault';
import { accountFingerprint } from '../src/chain/token';
import fixture from '../test-fixtures/mock-vault.json';

const commitment = '0x' + 'ab'.repeat(32);
const event = (over: Record<string, unknown> = {}) =>
  'EVENT_JSON:' +
  JSON.stringify({
    standard: 'nymira',
    version: '1.0.0',
    event: 'deposit',
    data: {
      note_id: 4,
      commitment,
      denomination: '1000000',
      expiry: 1_795_000_000,
      withdraw_to: 'alice.test',
      ...over,
    },
  });
const tree = (logs: string[], executor = 'vault.test') => ({
  transaction: { hash: 'H'.repeat(44), signer_id: 'alice.test' },
  receipts_outcome: [
    { outcome: { executor_id: 'usdc.test', logs: [] } },
    { outcome: { executor_id: executor, logs, status: { SuccessValue: '' } as unknown } },
  ],
});

describe('deposit events', () => {
  it('reads the event of the vault and nothing else', () => {
    expect(depositEvents(tree([event()]), 'vault.test')).toEqual([
      {
        noteId: 4,
        commitment,
        denomination: '1000000',
        expiry: 1_795_000_000,
        withdrawTo: 'alice.test',
      },
    ]);
    // The same line emitted by another contract is not the vault's event.
    expect(depositEvents(tree([event()], 'evil.test'), 'vault.test')).toEqual([]);
    expect(depositEvents(tree(['EVENT_JSON:{not json', 'plain log']), 'vault.test')).toEqual([]);
  });

  it('does not accept a deposit log from a failed vault receipt', () => {
    const failed = tree([event()]);
    failed.receipts_outcome[1]!.outcome.status = { Failure: { error: 'failed' } };
    expect(depositEvents(failed, 'vault.test')).toEqual([]);
    failed.receipts_outcome[1]!.outcome.status = undefined;
    expect(depositEvents(failed, 'vault.test')).toEqual([]);
  });

  it('compares commitment, amount and payout account', () => {
    const events = depositEvents(tree([event()]), 'vault.test');
    const expected = { commitment, denomination: '1000000', payout: 'alice.test' };
    expect(matchDeposit(events, expected).ok).toBe(true);
    expect(matchDeposit(events, { ...expected, payout: 'mallory.test' })).toMatchObject({
      ok: false,
      reason: 'mismatch',
      mismatched: ['payout account'],
    });
    expect(matchDeposit(events, { ...expected, denomination: '5000000' })).toMatchObject({
      mismatched: ['amount'],
    });
    expect(matchDeposit(events, { ...expected, commitment: '0x' + 'cd'.repeat(32) })).toMatchObject(
      {
        mismatched: ['commitment'],
      },
    );
    expect(matchDeposit([], expected)).toEqual({ ok: false, reason: 'no_event' });
  });

  it('finds the hash and tells a receipt tree from a bare outcome', () => {
    expect(txHashOf(tree([]))).toBe('H'.repeat(44));
    expect(hasReceiptTree(tree([]))).toBe(true);
    expect(hasReceiptTree({ transaction: { hash: 'x' } })).toBe(false);
    expect(txHashOf(null)).toBeNull();
  });
});

describe('vault pins', () => {
  const pins = fixture.pins as unknown as Parameters<typeof diffVault>[0];
  const live = (): VaultSnapshot => ({
    codeHash: pins.codeHash,
    vkHashes: { ...pins.vkHashes },
    config: JSON.parse(JSON.stringify(pins.config)),
    accessKeys: [],
  });

  it('equal state has no differences, whatever the key order', () => {
    expect(diffVault(pins, live())).toEqual([]);
    const reordered = live();
    reordered.config = Object.fromEntries(Object.entries(reordered.config).reverse());
    expect(diffVault(pins, reordered)).toEqual([]);
  });

  it('names each kind of difference', () => {
    const a = live();
    a.codeHash = 'other';
    expect(diffVault(pins, a)).toEqual(['the vault code hash']);
    const b = live();
    (b.vkHashes as { request: string }).request = '0x00';
    expect(diffVault(pins, b)).toEqual(['the verifying key hashes']);
    const c = live();
    c.accessKeys = [{ public_key: 'ed25519:K', permission: 'FullAccess' }];
    expect(diffVault(pins, c)).toEqual(['the access keys']);
    for (const field of [
      'operator_pk',
      'clearance_pk',
      'bound_classes',
      'charge_step',
      'min_charge',
      'slot_rate',
      'note_fee',
    ]) {
      const d = live();
      d.config[field] = 'changed';
      expect(diffVault(pins, d)).toEqual([`config: ${field}`]);
    }
  });

  it('a field the pins do not know is a difference too', () => {
    const e = live();
    e.config.new_field = '1';
    expect(diffVault(pins, e)).toEqual(['config: new_field']);
  });

  it('compares numbers and numeric strings as the same value', () => {
    expect(canonical({ a: 300 })).toBe(canonical({ a: '300' }));
  });
});

describe('vault check', () => {
  it('a build without pins is unpinned and never asks the chain', async () => {
    const rpc = new RpcClient(['https://a.test'], {
      fetchImpl: async () => {
        throw new Error('must not be called');
      },
    });
    expect(await checkVault(rpc, 'vault.test', null)).toEqual({ status: 'unpinned' });
  });

  it('an unreachable chain is reported, not treated as a match', async () => {
    const rpc = new RpcClient(['https://a.test'], {
      fetchImpl: async () => new Response('down', { status: 503 }),
    });
    const pins = fixture.pins as unknown as Parameters<typeof diffVault>[0];
    expect((await checkVault(rpc, 'vault.test', pins)).status).toBe('unreachable');
  });
});

describe('rpc client', () => {
  const ok = (result: unknown) => new Response(JSON.stringify({ jsonrpc: '2.0', id: 'x', result }));

  it('falls back to the next endpoint after a network failure, and rests the failed one', async () => {
    const seen: string[] = [];
    let now = 0;
    const client = new RpcClient(['https://a.test', 'https://b.test'], {
      now: () => now,
      fetchImpl: async (url) => {
        seen.push(String(url));
        if (String(url).startsWith('https://a.test')) throw new TypeError('blocked by CORS');
        return ok('fine');
      },
    });
    expect(await client.call('status', [])).toBe('fine');
    expect(await client.call('status', [])).toBe('fine');
    // First call tried a then b; the second skipped a (cooling down).
    expect(seen).toEqual(['https://a.test', 'https://b.test', 'https://b.test']);
    now = 60_000;
    await client.call('status', []);
    expect(seen.at(-2)).toBe('https://a.test');
  });

  it('does not retry a JSON-RPC error on another endpoint, and reports total failure', async () => {
    const urls: string[] = [];
    const client = new RpcClient(['https://a.test', 'https://b.test'], {
      fetchImpl: async (url) => {
        urls.push(String(url));
        return new Response(
          JSON.stringify({
            error: { name: 'HANDLER_ERROR', cause: { name: 'UNKNOWN_TRANSACTION' }, message: 'no' },
          }),
        );
      },
    });
    await expect(client.call('tx', [])).rejects.toMatchObject({ code: 'UNKNOWN_TRANSACTION' });
    expect(urls).toHaveLength(1);

    const down = new RpcClient(['https://a.test', 'https://b.test'], {
      fetchImpl: async () => new Response('x', { status: 429 }),
    });
    await expect(down.call('status', [])).rejects.toBeInstanceOf(NetworkError);
    expect(RpcError.name).toBe('RpcError');
  });

  it('a view returns the contract’s JSON and a panic is an error', async () => {
    const bytes = [...new TextEncoder().encode('{"total":"1"}')];
    const good = new RpcClient(['https://a.test'], {
      fetchImpl: async () => ok({ result: bytes }),
    });
    expect(await good.view('usdc.test', 'storage_balance_of', { account_id: 'a' })).toEqual({
      total: '1',
    });
    const nul = new RpcClient(['https://a.test'], {
      fetchImpl: async () => ok({ result: [...new TextEncoder().encode('null')] }),
    });
    expect(await nul.view('usdc.test', 'storage_balance_of', {})).toBeNull();
    const bad = new RpcClient(['https://a.test'], {
      fetchImpl: async () => ok({ error: 'panicked' }),
    });
    await expect(bad.view('x.test', 'm')).rejects.toBeInstanceOf(RpcError);
  });

  it('fingerprints are stable and differ per account', async () => {
    expect(await accountFingerprint('bob.test')).toBe(await accountFingerprint('bob.test'));
    expect(await accountFingerprint('bob.test')).not.toBe(await accountFingerprint('b0b.test'));
    expect(await accountFingerprint('bob.test')).toMatch(/^[0-9a-f]{4} [0-9a-f]{4} [0-9a-f]{4}$/);
  });
});
