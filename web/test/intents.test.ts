import { ed25519 } from '@noble/curves/ed25519.js';
import { describe, expect, it, vi } from 'vitest';
import { RpcError, type RpcClient } from '../src/chain/rpc';
import type { Booking } from '../src/chain/receipt';
import {
  INTENTS_SIGNER,
  QUOTE_ONLY,
  ROUTE_FALLBACK,
  confirmIntents,
  beforeAddress,
  GAS_NEAR_MIN,
  explainBooking,
  fetchStatus,
  intentsMessage,
  payoutProblem,
  requestQuote,
  twoStepProblem,
  statusText,
  type Attempt,
} from '../src/funding/intents';
import { claimPayout, type LeaveDeps } from '../src/leave/flow';
import type { FunctionCallAction } from '../src/wallet/protocol';

// Sealing a note needs the wallet core in a worker; the acceptance rules are what these tests read.
vi.mock('../src/funding/flow', async (original) => ({
  ...(await original<typeof import('../src/funding/flow')>()),
  finalize: async (record: unknown, event: unknown, source: unknown) => ({
    status: 'accepted',
    record,
    event,
    source,
  }),
}));
import { CACHE_MS, MAINNET_USDC_ASSET, QuoteBook, quoteBody } from '../src/funding/oneclick';
import { base58Encode, checkQuote, quoteHash, validateTwoStep } from '../src/funding/quote-check';
import { transferAction } from '../src/funding/flow';
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
    expect(testnet.oneClick.intentsPath).toBe('none');
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

describe('what the vault booked', () => {
  const line = (event: string, data: Json) =>
    'EVENT_JSON:' + JSON.stringify({ standard: 'nymira', version: '1.0.0', event, data });
  const deposit = line('deposit', {
    note_id: 3,
    commitment,
    denomination: '1000000',
    expiry: 1_795_000_000,
    withdraw_to: 'fresh.near',
  });
  const tx = (logs: string[], status: unknown = { SuccessValue: '' }) => ({
    final_execution_status: 'FINAL',
    transaction: { hash: 'H', signer_id: INTENTS_SIGNER },
    transaction_outcome: { id: 'a', outcome: { receipt_ids: ['b'] } },
    receipts_outcome: [{ id: 'b', outcome: { executor_id: PROFILE.vaultAccount, logs, status } }],
  });
  /** A regular node that has forgotten the transaction, and an archival one that has it. */
  const split = (archive: unknown, pendingAmount = '0') => {
    const asked: string[] = [];
    const regular = {
      txStatus: async () => {
        throw new RpcError('unknown transaction', 'UNKNOWN_TRANSACTION');
      },
      view: async () => {
        asked.push('regular');
        return '0';
      },
    } as unknown as RpcClient;
    const archival = {
      txStatus: async () => archive,
      view: async (_a: string, method: string) => {
        asked.push(`archival:${method}`);
        return pendingAmount;
      },
    } as unknown as RpcClient;
    return { regular, archival, asked };
  };

  it('SUCCESS with no deposit event confirms no note', async () => {
    const status = { status: 'SUCCESS', swapDetails: { nearTxHashes: ['H'] } };
    const progress = await fetchStatus(
      { quote: { quote: { depositAddress: 'a' } } } as unknown as Attempt,
      async () => new Response(JSON.stringify(status)),
    );
    expect(progress).toMatchObject({ state: 'success', hash: 'H' });
    const result = await confirmIntents(fakeRpc({ txStatus: tx([]) }), note, 'H');
    expect(result).toMatchObject({ status: 'pending' });
  });

  it('the archival endpoint confirms the note and pending_payout finds the excess', async () => {
    const excess = line('payout_pending', {
      account: 'fresh.near',
      amount: '500000',
      reason: 'intents_excess',
      commitment,
    });
    const { regular, archival, asked } = split(tx([deposit, excess]), '500000');
    const result = await confirmIntents(regular, note, 'H', {
      archival,
      delivered: '1550000',
    });
    expect(result).toMatchObject({
      status: 'accepted',
      pending: '500000',
      bookings: [{ event: 'payout_pending', amount: '500000', account: 'fresh.near' }],
    });
    expect(asked).toEqual(['archival:pending_payout']);
  });

  it('without an archival endpoint the note stays pending', async () => {
    const { regular } = split(tx([deposit]));
    expect(await confirmIntents(regular, note, 'H')).toMatchObject({ status: 'pending' });
  });

  it('a payout_pending event for the payout account is booked, matched by amount without a commitment', async () => {
    const waiting = line('payout_pending', {
      account: 'fresh.near',
      amount: '1050000',
      reason: 'transfer_failed',
    });
    const result = await confirmIntents(fakeRpc({ txStatus: tx([waiting]) }), note, 'H');
    expect(result).toMatchObject({
      status: 'booked',
      bookings: [{ event: 'payout_pending', amount: '1050000' }],
    });
    // Another account's payout, another amount, a failed receipt: none of them is ours.
    for (const other of [
      line('payout_pending', {
        account: 'mallory.near',
        amount: '1050000',
        reason: 'transfer_failed',
      }),
      line('payout_pending', { account: 'fresh.near', amount: '7', reason: 'transfer_failed' }),
    ]) {
      expect(await confirmIntents(fakeRpc({ txStatus: tx([other]) }), note, 'H')).toMatchObject({
        status: 'pending',
      });
    }
    const failed = tx([waiting], { Failure: {} });
    expect(await confirmIntents(fakeRpc({ txStatus: failed }), note, 'H')).toMatchObject({
      status: 'pending',
    });
  });

  it('explains dust and unclaimed; unclaimed without a commitment matches by amount', async () => {
    const dust = line('dust', { amount: '10000', reason: 'below_note_fee', commitment });
    const unclaimed = line('unclaimed', { amount: '1050000', reason: 'intents_bad_msg' });
    const d = await confirmIntents(fakeRpc({ txStatus: tx([dust]) }), note, 'H', {
      delivered: '10000',
    });
    expect(d).toMatchObject({ status: 'booked', bookings: [{ event: 'dust' }] });
    const u = await confirmIntents(fakeRpc({ txStatus: tx([unclaimed]) }), note, 'H');
    expect(u).toMatchObject({ status: 'booked', bookings: [{ event: 'unclaimed' }] });
    if (u.status !== 'booked') throw new Error('not booked');
    expect(explainBooking(u.bookings[0]!)).toContain(
      'could not read the message or found no valid payout account, so the amount went to the treasury balance',
    );
    expect(explainBooking((d as { bookings: Booking[] }).bookings[0]!)).toContain(
      'cannot be claimed',
    );
    // A dust amount at or above the note fee is not dust.
    const big = line('dust', { amount: '50000', reason: 'below_note_fee', commitment });
    expect(await confirmIntents(fakeRpc({ txStatus: tx([big]) }), note, 'H')).toMatchObject({
      status: 'pending',
    });
  });

  it('claim_payout is sent for the payout account', async () => {
    const sent: { receiver: string; action: FunctionCallAction }[] = [];
    const deps = {
      profile: PROFILE,
      rpc: {
        view: async () => '500000',
        viewAccount: async () => ({ amount: '1' + '0'.repeat(24) }),
      },
      wallet: {
        connected: { accountId: 'anyone.near' },
        send: async (receiver: string, action: FunctionCallAction) => {
          sent.push({ receiver, action });
          return { status: { SuccessValue: '' } };
        },
      },
    } as unknown as LeaveDeps;
    const claimed = await claimPayout(deps, note);
    expect(claimed.amount).toBe(500000n);
    expect(sent).toHaveLength(1);
    expect(sent[0]!.receiver).toBe(PROFILE.vaultAccount);
    expect(sent[0]!.action.params).toMatchObject({
      methodName: 'claim_payout',
      args: { account_id: 'fresh.near' },
    });
  });

  it('say what a refund, a failure and a passed deadline mean', () => {
    const later = NOW + 1;
    expect(statusText({ state: 'failed', status: 'REFUNDED' }, NOW, NOW)).toContain(
      'refunded the payment to your refund address',
    );
    expect(statusText({ state: 'failed', status: 'FAILED' }, NOW, NOW)).toContain('order failed');
    expect(statusText({ state: 'pending' }, NOW, later)).toContain('deadline has passed');
    expect(statusText({ state: 'pending' }, later, NOW)).toBe('Waiting for your payment.');
    expect(statusText({ state: 'failed', status: 'FAILED' }, NOW, NOW)).toContain(ROUTE_FALLBACK);
  });
});

describe('two-step path', () => {
  const account = 'alice.near';
  const twoStepRequest = () => {
    const request: Json = { ...quoteBody(asset, 1_050_000n, NOW), dry: false, recipient: account };
    delete request.customRecipientMsg;
    return request;
  };

  it('asks 1Click to pay the user account with no custom message', async () => {
    let sent: Json = {};
    // Storing the attempt fails without IndexedDB, after the checks; the request is what counts.
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
      account,
    ).catch(() => undefined);
    expect(sent).toMatchObject({ dry: false, amount: '1050000', recipient: account });
    expect('customRecipientMsg' in sent).toBe(false);
  });

  it('requires that recipient in the quote', () => {
    const request = twoStepRequest();
    expect(() => validateTwoStep(answer(request), account, PROFILE.vaultAccount)).not.toThrow();
    const check = (over: Json) => () =>
      validateTwoStep(answer(request, { request: over }), account, PROFILE.vaultAccount);
    expect(check({ recipient: PROFILE.vaultAccount })).toThrow('must not name the vault');
    expect(check({ recipient: 'mallory.near' })).toThrow('does not pay your own');
    expect(check({ customRecipientMsg: '{"commitment":"0x01"}' })).toThrow('no custom message');
  });

  it('is not offered to an account without NEAR or without a USDC registration', async () => {
    const node = (near: string | null, registered: string[]) =>
      ({
        viewAccount: async () => {
          if (near === null) throw new RpcError('no such account', 'UNKNOWN_ACCOUNT');
          return { amount: near };
        },
        view: async (_t: string, _m: string, args: { account_id: string }) =>
          registered.includes(args.account_id) ? { total: '1' } : null,
      }) as unknown as RpcClient;
    const both = [account, PROFILE.vaultAccount];
    const enough = GAS_NEAR_MIN.toString();
    expect(await twoStepProblem(node(enough, both), account)).toBeNull();
    expect(await twoStepProblem(node('0', both), account)).toContain('too little NEAR');
    expect(await twoStepProblem(node(null, both), account)).toContain('too little NEAR');
    expect(await twoStepProblem(node(String(GAS_NEAR_MIN - 1n), both), account)).toContain(
      'too little NEAR',
    );
    expect(await twoStepProblem(node(enough, [PROFILE.vaultAccount]), account)).toContain(
      `${account} is not registered`,
    );
    expect(await twoStepProblem(node(enough, [account]), account)).toContain(
      `${PROFILE.vaultAccount} is not registered`,
    );
  });

  it('writes no withdraw_to when the payout account is the sender', () => {
    const own = { ...note, sender: account, payout: account };
    const message = (n: NoteRecord) =>
      JSON.parse((transferAction(n).params.args as { msg: string }).msg) as Json;
    expect(message(own)).toEqual({ commitment });
    expect(message({ ...own, payout: 'fresh.near' })).toEqual({
      commitment,
      withdraw_to: 'fresh.near',
    });
  });
});
