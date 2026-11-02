import { describe, expect, it } from 'vitest';
import { expiryCalendar } from '../src/backup/ics';
import { depositMessage, notePrice, transferAction } from '../src/funding/flow';
import {
  CACHE_MS,
  MAINNET_USDC_ASSET,
  MIN_GAP_MS,
  QuoteBook,
  dryQuotesEnabled,
  intentsPathsEnabled,
  quoteBody,
} from '../src/funding/oneclick';
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

describe('1Click dry quotes', () => {
  const asset = mock.oneClick.shortlist[0]!;

  it('are profile flags: dry quotes on testnet and mock, Intents paths on mainnet only', () => {
    expect(dryQuotesEnabled(testnet)).toBe(true);
    expect(dryQuotesEnabled(mock)).toBe(true);
    expect(dryQuotesEnabled(mainnet)).toBe(false);
    expect(intentsPathsEnabled(testnet)).toBe(false);
    expect(intentsPathsEnabled(mock)).toBe(false);
    expect(intentsPathsEnabled(mainnet)).toBe(true);
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
    expect(text).not.toContain('commitment');
    expect(text).not.toContain('alice');
    expect(text).not.toContain('vault');
  });

  it('go out one per 5 seconds and are cached', async () => {
    let now = 0;
    const requestTimes: number[] = [];
    const book = new QuoteBook(
      'https://oneclick.test',
      async () => {
        requestTimes.push(now);
        return new Response(
          JSON.stringify({ quote: { amountInFormatted: '1.07', timeEstimate: 45 } }),
        );
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
    const book = new QuoteBook('https://oneclick.test', async () => {
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
