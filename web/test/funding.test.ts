import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { expiryCalendar } from '../src/backup/ics';
import { depositMessage, notePrice, transferAction } from '../src/funding/flow';
import {
  CACHE_MS,
  MAINNET_USDC_ASSET,
  MIN_GAP_MS,
  QuoteBook,
  dryQuotesEnabled,
  intentsPaths,
  intentsPathsEnabled,
  quoteBody,
} from '../src/funding/oneclick';
import { ed25519 } from '@noble/curves/ed25519.js';
import { base58Encode, quoteHash } from '../src/funding/quote-check';
import { profile as mainnet } from '../src/profiles/mainnet';
import { profile as mock } from '../src/profiles/mock';
import { profile as testnet } from '../src/profiles/testnet';
import { formatDate, formatUnits } from '../src/ui/format';
import type { NoteRecord } from '../src/store/types';

const commitment = '0x' + 'ab'.repeat(32);

describe('deposit message', () => {
  it('leaves withdraw_to out when the payout is the sending account', () => {
    expect(depositMessage(commitment, 'alice.test', 'alice.test')).toBe(
      JSON.stringify({ commitment }),
    );
    expect(depositMessage(commitment, 'bob.test', 'alice.test')).toBe(
      JSON.stringify({ commitment, withdraw_to: 'bob.test' }),
    );
  });

  it('prices a note as denomination plus the pinned fee and sends ft_transfer_call with 1 yocto', () => {
    expect(notePrice('1000000')).toBe(1_050_000n);
    expect(notePrice('5000000')).toBe(5_050_000n);
    const record = {
      vault: 'vault.test',
      token: 'usdc.test',
      denomination: '1000000',
      commitment,
      payout: 'alice.test',
      sender: 'alice.test',
    } as NoteRecord;
    const action = transferAction(record);
    expect(action.params.methodName).toBe('ft_transfer_call');
    expect(action.params.deposit).toBe('1');
    expect(action.params.args).toEqual({
      receiver_id: 'vault.test',
      amount: '1050000',
      msg: JSON.stringify({ commitment }),
    });
  });
});

describe('calendar file', () => {
  const ics = expiryCalendar(1_795_046_400, 'uid-1', Date.UTC(2026, 10, 3));

  it('is dated by the expiry day and carries the 5-day and 3-day alarms', () => {
    // 1_795_046_400 is 2026-11-19T00:00:00Z, the end of 2026-11-18 (UTC).
    expect(ics).toContain('DTSTART;VALUE=DATE:20261118');
    expect(ics).toContain('TRIGGER:-P5D');
    expect(ics).toContain('TRIGGER:-P3D');
    expect(formatDate(1_795_046_400)).toBe('2026-11-18 UTC');
  });

  it('holds no note data, vault, account or product name', () => {
    const lower = ics.toLowerCase();
    for (const word of [
      'nymira',
      'vault',
      'usdc',
      'commitment',
      'note id',
      'alice',
      'testnet',
      '1000000',
    ]) {
      expect(lower).not.toContain(word);
    }
  });
});

/** A dry quote signed with the mock build's test key. */
function signedAnswer(request: Record<string, unknown>) {
  const response: Record<string, unknown> = {
    timestamp: '2026-11-06T00:00:00.000Z',
    quoteRequest: request,
    quote: {
      amountIn: '1065586',
      amountInFormatted: '1.07',
      amountInUsd: '1.07',
      minAmountIn: '1065586',
      amountOut: request.amount,
      amountOutFormatted: '1.05',
      amountOutUsd: '1.05',
      minAmountOut: request.amount,
      timeEstimate: 45,
    },
  };
  const message = new TextEncoder().encode(quoteHash(response));
  response.signature =
    'ed25519:' + base58Encode(ed25519.sign(message, new Uint8Array(32).fill(44)));
  return response;
}

describe('1Click dry quotes', () => {
  const asset = mock.oneClick.shortlist[0]!;

  it('are profile flags: dry quotes on testnet and mock, Intents paths on mainnet and the mock build only', () => {
    expect(dryQuotesEnabled(testnet)).toBe(true);
    expect(dryQuotesEnabled(mock)).toBe(true);
    expect(dryQuotesEnabled(mainnet)).toBe(false);
    expect(intentsPathsEnabled(testnet)).toBe(false);
    expect(intentsPathsEnabled(mock)).toBe(true);
    expect(intentsPathsEnabled(mainnet)).toBe(true);
  });

  const withPath = (p: typeof mainnet, intentsPath: typeof mainnet.oneClick.intentsPath) => ({
    ...p,
    oneClick: { ...p.oneClick, intentsPath },
  });

  it('offer on mainnet only the configured Intents path: two-step by decision, or none', () => {
    expect(mainnet.oneClick.intentsPath).toBe('two-step');
    expect(intentsPaths(mainnet)).toEqual(['two-step']);
    expect(intentsPaths(withPath(mainnet, 'direct'))).toEqual(['direct']);
    expect(intentsPaths(withPath(mainnet, 'none'))).toEqual([]);
    expect(intentsPaths(mock)).toEqual(['direct', 'two-step']);
  });

  it('ignore the Intents setting on the testnet profile, which stays quote-only', () => {
    for (const path of ['direct', 'two-step', 'both'] as const) {
      expect(intentsPaths(withPath(testnet, path))).toEqual([]);
    }
  });

  it('read the Intents path from the compiled profile and from nothing else', () => {
    const src = (f: string) => readFileSync(new URL(`../src/${f}`, import.meta.url), 'utf8');
    const code = src('ui/Intents.tsx') + src('funding/oneclick.ts');
    for (const sink of ['location', 'localStorage', 'sessionStorage', 'searchParams'])
      expect(code).not.toContain(sink);
  });

  it('send dry:true with a dummy refund address and no note data', () => {
    const body = quoteBody(asset, 1_050_000n, 0);
    expect(body).toMatchObject({
      dry: true,
      swapType: 'EXACT_OUTPUT',
      amount: '1050000',
      destinationAsset: MAINNET_USDC_ASSET,
      refundTo: asset.dummyRefund,
    });
    const text = JSON.stringify(body);
    expect(text).not.toContain('alice');
    expect(text).not.toContain('vault');
  });

  it('go out one per 5 seconds and are cached', async () => {
    let now = Date.parse('2026-11-06T00:00:00Z');
    const requestTimes: number[] = [];
    const book = new QuoteBook(
      'https://oneclick.test',
      mock.oneClick.signerKey,
      async (_url, init) => {
        requestTimes.push(now);
        return new Response(JSON.stringify(signedAnswer(JSON.parse(String(init?.body)))));
      },
      () => now,
      async (ms) => {
        now += ms;
      },
    );
    const second = mock.oneClick.shortlist[1]!;
    const [a, b] = await Promise.all([
      book.quote(asset, 1_050_000n),
      book.quote(second, 1_050_000n),
    ]);
    expect('quote' in a && 'quote' in b).toBe(true);
    expect(requestTimes[1]! - requestTimes[0]!).toBeGreaterThanOrEqual(MIN_GAP_MS);
    // Within a minute the same quote comes from the cache.
    await book.quote(asset, 1_050_000n);
    expect(requestTimes).toHaveLength(2);
    now += CACHE_MS + 1;
    await book.quote(asset, 1_050_000n);
    expect(requestTimes).toHaveLength(3);
  });

  it('report an error row when 1Click is unreachable', async () => {
    const book = new QuoteBook('https://oneclick.test', mock.oneClick.signerKey, async () => {
      throw new TypeError('offline');
    });
    expect(await book.quote(asset, 1n)).toEqual({
      label: asset.label,
      error: 'could not reach 1Click',
    });
  });
});

describe('amounts', () => {
  it('formats base units', () => {
    expect(formatUnits(1_050_000n, 6)).toBe('1.05');
    expect(formatUnits('5000000', 6)).toBe('5.00');
    expect(formatUnits(50_000n, 6)).toBe('0.05');
  });
});

describe('restoring a backup over a note the browser already holds', () => {
  const rec = (id: string, updatedAt: number, leave?: object) =>
    ({
      id,
      updatedAt,
      ...(leave ? { leave } : {}),
    }) as unknown as import('../src/store/types').NoteRecord;

  it('keeps the newer state, takes a newer backup and an unknown note, and never replaces a note that is leaving', async () => {
    const { mergeRestored } = await import('../src/store/merge');
    const held = [rec('a', 200), rec('b', 100), rec('c', 100, { phase: 'exit' })];
    const incoming = [rec('a', 100), rec('b', 300), rec('c', 900), rec('d', 50)];
    const { take, skipped } = mergeRestored(held, incoming);
    expect(take.map((n) => n.id).sort()).toEqual(['b', 'd']);
    expect(skipped.map((n) => n.id).sort()).toEqual(['a', 'c']);
  });
});

describe('looking a deposit up by a typed hash', () => {
  it('leaves the note pending when the hash is another transaction of the account, instead of marking it rejected', async () => {
    const { confirmDeposit } = await import('../src/funding/flow');
    const record = {
      id: 'n1',
      commitment: '0x' + '11'.repeat(32),
      denomination: '1000000',
      payout: 'alice.testnet',
      sender: 'alice.testnet',
      phase: 'pending',
    } as unknown as import('../src/store/types').NoteRecord;
    // A complete transaction of the same account that holds no `deposit` event (a storage
    // registration, say): it proves nothing about this note.
    const rpc = {
      txStatus: async () => ({
        final_execution_status: 'FINAL',
        status: { SuccessValue: '' },
        transaction: { signer_id: 'alice.testnet' },
        receipts_outcome: [],
      }),
    } as unknown as import('../src/chain/rpc').RpcClient;
    const result = await confirmDeposit(rpc, record, null, { waitMs: 0, sleep: async () => {} });
    expect(result.status).toBe('pending');
  });
});
