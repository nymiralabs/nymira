import { ed25519 } from '@noble/curves/ed25519.js';
import { describe, expect, it } from 'vitest';
import type { RpcClient } from '../src/chain/rpc';
import {
  INTENTS_SIGNER,
  QUOTE_ONLY,
  confirmIntents,
  beforeAddress,
  fetchStatus,
  intentsMessage,
  payoutProblem,
  requestQuote,
  type Attempt,
} from '../src/funding/intents';
import { CACHE_MS, MAINNET_USDC_ASSET, QuoteBook, quoteBody } from '../src/funding/oneclick';
import { base58Encode, checkQuote, quoteHash } from '../src/funding/quote-check';
import { PROFILE } from '../src/profile';
import type { NoteRecord } from '../src/store/types';
import { profile as testnet } from '../src/profiles/testnet';

type Json = Record<string, unknown>;
const SEED = new Uint8Array(32).fill(44);
const KEY = PROFILE.oneClick.signerKey;
const NOW = Date.parse('2026-11-06T12:00:00Z');
const asset = PROFILE.oneClick.shortlist[0]!;
const pins = PROFILE.pins!;
const commitment = '0x' + 'ab'.repeat(32);

/** What a 1Click that signs with the test key answers to `request`. */
function answer(request: Json, over: { quote?: Json; request?: Json; skipSign?: boolean } = {}) {
  const amount = BigInt(request.amount as string);
  const quote: Json = {
    amountIn: String(amount + 15_586n),
    amountInFormatted: '1.065586',
    amountInUsd: '1.07',
    minAmountIn: String(amount + 15_586n),
    amountOut: request.amount,
    amountOutFormatted: '1.05',
    amountOutUsd: '1.05',
    minAmountOut: request.amount,
    ...(request.dry === false
      ? { depositAddress: 'fake-deposit-1', deadline: request.deadline }
      : {}),
    ...over.quote,
  };
  const response: Json = {
    timestamp: '2026-11-06T12:00:00.000Z',
    quoteRequest: { ...request, ...over.request },
    quote,
  };
  response.signature = over.skipSign
    ? 'ed25519:' + base58Encode(new Uint8Array(64))
    : 'ed25519:' + base58Encode(ed25519.sign(new TextEncoder().encode(quoteHash(response)), SEED));
  return response;
}

const reply = (body: unknown) => new Response(JSON.stringify(body), { status: 201 });
const bodyOf = (init?: RequestInit) => JSON.parse(String(init?.body)) as Json;

describe('quote checks', () => {
  const sent = quoteBody(asset, 1_050_000n, NOW);
  const check =
    (response: Json, now = NOW) =>
    () =>
      checkQuote(response, sent, KEY, now);

  it('accept a signed quote that answers the request', () => {
    expect(check(answer(sent))).not.toThrow();
  });

  it('stop on a wrong amountOut', () => {
    expect(check(answer(sent, { quote: { amountOut: '1000000' } }))).toThrow('amountOut');
  });

  it('stop on a wrong recipient', () => {
    expect(check(answer(sent, { request: { recipient: 'mallory.near' } }))).toThrow(
      'request field recipient',
    );
  });

  it('stop on a changed customRecipientMsg', () => {
    const changed = answer(sent, { request: { customRecipientMsg: '{"commitment":"0x02"}' } });
    expect(check(changed)).toThrow('request field customRecipientMsg');
  });

  it('stop on a bad signature', () => {
    expect(check(answer(sent, { skipSign: true }))).toThrow('signature does not verify');
    const tampered = answer(sent);
    (tampered.quote as Json).amountIn = '1';
    expect(check(tampered)).toThrow('signature does not verify');
    const unsigned = answer(sent);
    delete unsigned.signature;
    expect(check(unsigned)).toThrow('no signature');
  });

  it('stop on an expired deadline', () => {
    expect(check(answer(sent), NOW + 3_600_001)).toThrow('expired');
  });

  it('hash in the SDK format', () => {
    expect(base58Encode(Uint8Array.of(0, 0, 1))).toBe('112');
    expect(quoteHash(answer(sent))).toMatch(/^[1-9A-HJ-NP-Za-km-z]{43,44}$/);
  });
});

describe('dry quote source list', () => {
  const book = (fetchImpl: typeof fetch, clock = { t: NOW }) =>
    new QuoteBook(
      PROFILE.oneClick.baseUrl,
      KEY,
      fetchImpl,
      () => clock.t,
      async (ms) => {
        clock.t += ms;
      },
    );

  it('carry placeholders only: no commitment, payout or wallet address', async () => {
    let body: Json = {};
    await book(async (_url, init) => {
      body = bodyOf(init);
      return reply(answer(body));
    }).quote(asset, 1_050_000n);
    expect(body).toMatchObject({
      dry: true,
      destinationAsset: MAINNET_USDC_ASSET,
      recipient: 'nymira.near',
      refundTo: asset.dummyRefund,
    });
    expect(JSON.parse(body.customRecipientMsg as string)).toEqual({
      commitment: '0x' + '01'.repeat(32),
      withdraw_to: 'nymira.near',
    });
  });

  it('list a source only for a signed quote that echoes the request', async () => {
    const good = await book(async (_u, init) => reply(answer(bodyOf(init)))).quote(
      asset,
      1_050_000n,
    );
    expect('quote' in good).toBe(true);
    const unsigned = await book(async (_u, init) =>
      reply(answer(bodyOf(init), { skipSign: true })),
    ).quote(asset, 1_050_000n);
    expect(unsigned).toMatchObject({ error: 'the quote signature does not verify' });
    const elsewhere = await book(async (_u, init) =>
      reply(answer(bodyOf(init), { request: { recipient: 'example.near' } })),
    ).quote(asset, 1_050_000n);
    expect(elsewhere).toMatchObject({ error: expect.stringContaining('recipient') });
  });

  it('do not list a source 1Click refuses, such as an origin minimum above the payment', async () => {
    const refused = await book(async () => new Response('{}', { status: 400 })).quote(
      asset,
      1_050_000n,
    );
    expect(refused).toEqual({ label: asset.label, error: '1Click answered 400' });
  });

  it('are cached for a few minutes', async () => {
    let calls = 0;
    const clock = { t: NOW };
    const b = book(async (_u, init) => {
      calls++;
      return reply(answer(bodyOf(init)));
    }, clock);
    await b.quote(asset, 1_050_000n);
    await b.quote(asset, 1_050_000n);
    expect(calls).toBe(1);
    clock.t += CACHE_MS + 1;
    await b.quote(asset, 1_050_000n);
    expect(calls).toBe(2);
  });

  it('name the mainnet asset and nymira.near on a testnet build, which offers no payment', () => {
    expect(testnet.oneClick.intentsPaths).toBe(false);
    expect(testnet.oneClick.signerKey).toMatch(/^ed25519:/);
    expect(quoteBody(testnet.oneClick.shortlist[0]!, 1n, NOW)).toMatchObject({
      destinationAsset: MAINNET_USDC_ASSET,
      recipient: 'nymira.near',
    });
    expect(QUOTE_ONLY).toBe(
      'Quote only. Intents has no testnet, so no cross-chain payment is made here. Fund with testnet USDC from the Circle faucet.',
    );
  });
});

describe('payout account', () => {
  it('may not be the vault, the token or intents.near', () => {
    for (const bad of [PROFILE.vaultAccount, PROFILE.tokenAccount, 'intents.near', 'Not An Id']) {
      expect(payoutProblem(bad)).not.toBeNull();
    }
    expect(payoutProblem('fresh-payout.near')).toBeNull();
  });

  it('is always written into the message as withdraw_to', () => {
    expect(JSON.parse(intentsMessage(commitment, 'fresh.near'))).toEqual({
      commitment,
      withdraw_to: 'fresh.near',
    });
  });
});

/** A node that answers the vault reads from the bundle's own pins. */
function fakeRpc(over: { config?: Json; codeHash?: string; txStatus?: unknown } = {}) {
  const config = { ...pins.config, tvl_cap: '100000000', current_tvl: '0', ...over.config };
  return {
    viewAccount: async () => ({ code_hash: over.codeHash ?? pins.codeHash }),
    view: async (_a: string, method: string) => (method === 'config' ? config : pins.vkHashes),
    accessKeyList: async () => ({
      keys: pins.accessKeys.map((k) => ({
        public_key: k.public_key,
        access_key: { permission: k.permission },
      })),
    }),
    txStatus: async () => over.txStatus,
  } as unknown as RpcClient;
}

const note = {
  id: 'n1',
  vault: PROFILE.vaultAccount,
  denomination: '1000000',
  payout: 'fresh.near',
  commitment,
  phase: 'backed_up',
} as NoteRecord;

describe('live quote', () => {
  const never = (async () => {
    throw new Error('1Click must not be called');
  }) as typeof fetch;
  const quote = (rpc: RpcClient, refund = '0xRefund', n = note) =>
    requestQuote(rpc, n, asset, refund, never, () => NOW);

  it('is exact-output for denomination plus fee, vault as recipient, with the expected message', async () => {
    let sent: Json = {};
    // IndexedDB is absent here, so storing the attempt fails after the checks; the request is what counts.
    await requestQuote(
      fakeRpc(),
      note,
      asset,
      '0xRefund',
      async (_u, init) => {
        sent = bodyOf(init);
        return reply(answer(sent));
      },
      () => NOW,
    ).catch(() => undefined);
    expect(sent).toMatchObject({
      dry: false,
      swapType: 'EXACT_OUTPUT',
      amount: '1050000',
      recipient: PROFILE.vaultAccount,
      refundTo: '0xRefund',
      customRecipientMsg: intentsMessage(commitment, 'fresh.near'),
    });
  });

  it('is not requested when the vault differs from the pins', async () => {
    await expect(quote(fakeRpc({ codeHash: 'other' }))).rejects.toThrow('the vault changed');
    await expect(quote(fakeRpc({ config: { note_fee: '1' } }))).rejects.toThrow('config: note_fee');
  });

  it('is not requested when the TVL cap has no room', async () => {
    const full = fakeRpc({ config: { tvl_cap: '1000000', current_tvl: '0' } });
    await expect(quote(full)).rejects.toThrow('vault is full');
  });

  it('is never shown as payable after a wrong answer: one stop per case', async () => {
    const ask = (over: Parameters<typeof answer>[1]) =>
      requestQuote(
        fakeRpc(),
        note,
        asset,
        '0xRefund',
        async (_u, init) => reply(answer(bodyOf(init), over)),
        () => NOW,
      );
    await expect(ask({ quote: { amountOut: '1000000' } })).rejects.toThrow('amountOut');
    await expect(ask({ request: { recipient: 'mallory.near' } })).rejects.toThrow('recipient');
    await expect(ask({ request: { customRecipientMsg: '{}' } })).rejects.toThrow(
      'customRecipientMsg',
    );
    await expect(ask({ skipSign: true })).rejects.toThrow('signature');
    const noAddress = await ask({ quote: { depositAddress: '' } }).catch((e) => e);
    expect(String(noAddress)).toMatch(/signature|deposit address/);
  });

  it('is checked again before the address is shown: the vault, then the deadline', async () => {
    const request = { ...quoteBody(asset, 1_050_000n, NOW), dry: false };
    const attempt = { request, quote: answer(request) } as unknown as Attempt;
    await expect(beforeAddress(fakeRpc({ codeHash: 'other' }), attempt, () => NOW)).rejects.toThrow(
      'the vault changed',
    );
    await expect(beforeAddress(fakeRpc(), attempt, () => NOW + 7_200_000)).rejects.toThrow(
      'expired',
    );
  });

  it('needs a refund address and a tested backup', async () => {
    await expect(quote(fakeRpc(), ' ')).rejects.toThrow('refund address');
    await expect(quote(fakeRpc(), 'x', { ...note, phase: 'prepared' })).rejects.toThrow('backup');
  });
});

describe('status and receipt', () => {
  const attempt = { quote: { quote: { depositAddress: 'addr' } } } as unknown as Attempt;
  const status = (body: unknown) => async () => new Response(JSON.stringify(body));

  it('maps 1Click statuses; SUCCESS only supplies the destination hash', async () => {
    expect(await fetchStatus(attempt, status({ status: 'PROCESSING' }))).toEqual({
      state: 'pending',
    });
    expect(await fetchStatus(attempt, status({ status: 'REFUNDED' }))).toEqual({
      state: 'failed',
      status: 'REFUNDED',
    });
    const success = { status: 'SUCCESS', swapDetails: { nearTxHashes: ['H'] } };
    expect(await fetchStatus(attempt, status(success))).toEqual({ state: 'success', hash: 'H' });
    const none = { status: 'SUCCESS', swapDetails: { nearTxHashes: [] } };
    await expect(fetchStatus(attempt, status(none))).rejects.toThrow('single NEAR');
  });

  const event = (over: Json = {}) =>
    'EVENT_JSON:' +
    JSON.stringify({
      standard: 'nymira',
      version: '1.0.0',
      event: 'deposit',
      data: {
        note_id: 3,
        commitment,
        denomination: '1000000',
        expiry: 1_795_000_000,
        withdraw_to: 'fresh.near',
        ...over,
      },
    });
  const tx = (logs: string[], over: Json = {}) => ({
    final_execution_status: 'FINAL',
    transaction: { hash: 'H', signer_id: INTENTS_SIGNER },
    transaction_outcome: { id: 'a', outcome: { receipt_ids: ['b'] } },
    receipts_outcome: [
      {
        id: 'b',
        outcome: { executor_id: PROFILE.vaultAccount, logs, status: { SuccessValue: '' } },
      },
    ],
    ...over,
  });
  const run = (s: unknown) => confirmIntents(fakeRpc({ txStatus: s }), note, 'H');

  it('leave the note pending when the tree is incomplete or not final', async () => {
    const open = { id: 'a', outcome: { receipt_ids: ['missing'] } };
    expect(await run(tx([event()], { transaction_outcome: open }))).toMatchObject({
      status: 'pending',
    });
    expect(await run(tx([event()], { final_execution_status: 'INCLUDED' }))).toMatchObject({
      status: 'pending',
    });
  });

  it('leave the note pending when the event differs, is missing or is not unique', async () => {
    expect(await run(tx([event({ withdraw_to: 'mallory.near' })]))).toMatchObject({
      status: 'pending',
      reason: expect.stringContaining('payout account'),
    });
    expect(await run(tx([event({ denomination: '5000000' })]))).toMatchObject({
      reason: expect.stringContaining('amount'),
    });
    expect(await run(tx([]))).toMatchObject({ status: 'pending' });
    expect(await run(tx([event(), event({ note_id: 4 })]))).toMatchObject({ status: 'pending' });
  });
});
