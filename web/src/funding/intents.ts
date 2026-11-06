// Funding from another chain through 1Click, direct path (mirrors crates/nymirad/src/intents.rs).
// 1Click is called from this page without an API key, so it sees the user's IP address, and the
// page holds no intermediate wallet: the user pays the deposit address from their own wallet.
//
//   1. vault pins and room under the TVL cap are checked
//   2. the note secret is made, stored sealed and backed up (flow.ts), before any live quote
//   3. an exact-output quote to NEAR USDC for denomination plus note fee is requested, with the
//      vault as recipient and `{commitment, withdraw_to}` as `customRecipientMsg`, and checked
//   4. the vault is checked again, then the deposit address is shown
//   5. 1Click's status says SUCCESS and names the destination transaction; the note is accepted
//      only from the `deposit` event in that transaction's receipts
import { PROFILE } from '../profile';
import type { RpcClient, TxStatus } from '../chain/rpc';
import { pendingPayout } from '../chain/leave';
import {
  bookingEvents,
  depositEvents,
  matchDeposit,
  treeComplete,
  type Booking,
} from '../chain/receipt';
import { openJson, sealJson, type Sealed } from '../crypto/aead';
import { requireKey } from '../crypto/wrap';
import { getSetting, putSetting } from '../store/db';
import type { NoteRecord } from '../store/types';
import { isAccountId } from '../wallet/client';
import { finalize, notePrice, vaultGate, type Acceptance } from './flow';
import { buildRequest } from './oneclick';
import { checkQuote, validateQuote } from './quote-check';
import type { QuoteAsset } from '../profiles/types';

/** The account that signs the destination transaction on the live Intents path. */
export const INTENTS_SIGNER = 'intents.near';

export const PRIVACY =
  'On this path 1Click and the Intents explorer pair your source wallet with this deposit. The proofs are designed to keep the vault and the operator from tying your spending to this deposit, but while few notes are open, and without Tor, your spending can still be matched to it by time and network address. Wait a while between depositing and your first chat.';

export const QUOTE_ONLY =
  'Quote only. Intents has no testnet, so no cross-chain payment is made here. Fund with testnet USDC from the Circle faucet.';

export const TRUST_NOTE =
  'The signature shows only that 1Click issued this quote. 1Click and its solvers hold your funds while they are in transit, and the refund address is the only way back.';

/** Documented 1Click fee for keyless use, in basis points of the amount delivered. */
export const KEYLESS_FEE_BPS = 25;

/** Shown while a payment is on its way or after it failed (the same words as nymirad). */
export const ROUTE_FALLBACK =
  'Route screening can delay or block delivery. For a future deposit, fund with NEAR USDC directly; do not pay twice while this attempt is unresolved.';

export type Json = Record<string, unknown>;

export interface Attempt {
  /** The id of the pending note this attempt funds. */
  noteId: string;
  source: string;
  refund: string;
  request: Json;
  quote: Json;
  createdAt: number;
  /** The destination transaction, once 1Click names it. */
  hash?: string;
  /** What 1Click says it delivered, in USDC base units, when it says. */
  amount?: string;
}

const attemptAad = (noteId: string) =>
  `nymira:v1:intents:${PROFILE.network}:${PROFILE.vaultAccount}:${noteId}`;

/** The pending funding is sealed under the wrapping key: the deposit address names the order. */
export async function saveAttempt(attempt: Attempt): Promise<void> {
  await putSetting(
    `intents:${attempt.noteId}`,
    await sealJson(requireKey(), attempt, attemptAad(attempt.noteId)),
  );
}

export async function loadAttempt(noteId: string): Promise<Attempt | null> {
  const sealed = await getSetting<Sealed>(`intents:${noteId}`);
  return sealed ? openJson<Attempt>(requireKey(), sealed, attemptAad(noteId)) : null;
}

export const intentsMessage = (commitment: string, payout: string) =>
  JSON.stringify({ commitment, withdraw_to: payout });

/** Why this account may not be the payout of an Intents deposit; null when it may. */
export function payoutProblem(payout: string): string | null {
  if (!isAccountId(payout)) return 'That is not a valid NEAR account id.';
  if ([PROFILE.vaultAccount, PROFILE.tokenAccount, INTENTS_SIGNER].includes(payout)) {
    return 'That account cannot be a payout: it is the vault, the token or the Intents contract.';
  }
  return null;
}

async function post(url: string, body: Json, fetchImpl: typeof fetch): Promise<Json> {
  const response = await fetchImpl(url, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify(body),
    credentials: 'omit',
    referrerPolicy: 'no-referrer',
  });
  if (!response.ok) throw new Error(`1Click answered ${response.status} to the quote request`);
  return (await response.json()) as Json;
}

/**
 * The live quote for a backed-up pending note. The vault is checked first; the answer is checked
 * for signature, echo of the request, recipient, message, amount and deadline, and for a deposit
 * address. The attempt is kept in IndexedDB on this device.
 */
export async function requestQuote(
  rpc: RpcClient,
  note: NoteRecord,
  asset: QuoteAsset,
  refund: string,
  fetchImpl: typeof fetch = (...a) => fetch(...a),
  now: () => number = Date.now,
): Promise<Attempt> {
  if (note.phase !== 'backed_up') throw new Error('the backup has not been tested yet');
  if (refund.trim() === '') throw new Error('a refund address on the source chain is required');
  const price = notePrice(note.denomination);
  const gate = await vaultGate(rpc, price);
  if (!gate.ok) throw new Error(gate.reason);
  const request = buildRequest({
    dry: false,
    assetId: asset.assetId,
    amount: price,
    refund: refund.trim(),
    recipient: PROFILE.vaultAccount,
    message: intentsMessage(note.commitment, note.payout),
    now: now(),
  });
  const quote = await post(`${PROFILE.oneClick.baseUrl}/v0/quote`, request, fetchImpl);
  checkQuote(quote, request, PROFILE.oneClick.signerKey, now());
  const address = (quote.quote as Json | undefined)?.depositAddress;
  if (typeof address !== 'string' || address === '') {
    throw new Error('the quote has no deposit address');
  }
  const attempt: Attempt = {
    noteId: note.id,
    source: asset.label,
    refund: refund.trim(),
    request,
    quote,
    createdAt: now(),
  };
  await saveAttempt(attempt);
  return attempt;
}

/** The last checks before the deposit address is shown: the vault, then the quote's deadline. */
export async function beforeAddress(
  rpc: RpcClient,
  attempt: Attempt,
  now: () => number = Date.now,
): Promise<void> {
  const gate = await vaultGate(rpc, BigInt(attempt.request.amount as string));
  if (!gate.ok) throw new Error(gate.reason);
  validateQuote(attempt.quote, attempt.request, now());
}

export type Progress =
  | { state: 'pending' }
  | { state: 'failed'; status: string }
  | { state: 'success'; hash: string; amount?: string };

/** 1Click's status for the order. `SUCCESS` only supplies the destination transaction hash. */
export async function fetchStatus(
  attempt: Attempt,
  fetchImpl: typeof fetch = (...a) => fetch(...a),
): Promise<Progress> {
  const q = attempt.quote.quote as Json;
  const params = new URLSearchParams({ depositAddress: String(q.depositAddress) });
  if (typeof q.depositMemo === 'string' && q.depositMemo !== '') {
    params.set('depositMemo', q.depositMemo);
  }
  const response = await fetchImpl(`${PROFILE.oneClick.baseUrl}/v0/status?${params}`, {
    credentials: 'omit',
    referrerPolicy: 'no-referrer',
  });
  if (!response.ok) throw new Error(`1Click answered ${response.status} to the status request`);
  const status = (await response.json()) as Json;
  switch (status.status) {
    case 'KNOWN_DEPOSIT_TX':
    case 'PENDING_DEPOSIT':
    case 'INCOMPLETE_DEPOSIT':
    case 'PROCESSING':
      return { state: 'pending' };
    case 'REFUNDED':
    case 'FAILED':
      return { state: 'failed', status: status.status };
    case 'SUCCESS': {
      const hashes = (status.swapDetails as Json | undefined)?.nearTxHashes;
      if (!Array.isArray(hashes) || hashes.length !== 1 || typeof hashes[0] !== 'string') {
        throw new Error('1Click gave no single NEAR destination transaction');
      }
      const out = (status.swapDetails as Json).amountOut;
      if (out === undefined || out === null) return { state: 'success', hash: hashes[0] };
      if (typeof out !== 'string' || !/^[1-9]\d*$/.test(out)) {
        throw new Error('1Click gave an invalid settled amount');
      }
      return { state: 'success', hash: hashes[0], amount: out };
    }
    default:
      throw new Error('1Click gave an unknown status');
  }
}

/**
 * What 1Click's status means, in plain words. `deadline` is the quote's, in milliseconds; a
 * payment 1Click has not delivered by then is not coming through this quote.
 */
export function statusText(result: Progress, deadline: number, now: number): string {
  if (result.state === 'failed') {
    return result.status === 'REFUNDED'
      ? `1Click refunded the payment to your refund address on the source chain. Nothing reached the vault and this note is not funded. ${ROUTE_FALLBACK}`
      : `1Click reports that the order failed. If you paid, the refund goes to your refund address on the source chain; look there. The note is not funded. ${ROUTE_FALLBACK}`;
  }
  return Number.isFinite(deadline) && now > deadline
    ? `The quote's deadline has passed and 1Click has not delivered. Do not pay for this quote now. If you already paid, 1Click refunds to your refund address; it may take a while. ${ROUTE_FALLBACK}`
    : 'Waiting for your payment.';
}

/** Plain words for what the vault booked instead of a note. */
export function explainBooking(b: Booking): string {
  const what = `${b.amount} USDC base units`;
  switch (b.event) {
    case 'dust':
      return `Dust: ${what} (${b.reason}) went to the treasury balance. This amount cannot be claimed.`;
    case 'unclaimed':
      return `Unclaimed: ${what}. The vault could not read the message or found no valid payout account, so the amount went to the treasury balance. No note was confirmed.`;
    default:
      return `Waiting for payout: ${what} for ${b.account} (${b.reason}). Claim it with claim_payout; any wallet with a little NEAR can send it, and the money goes to the payout account.`;
  }
}

/**
 * The vault's bookings that belong to this delivery (mirrors nymirad). An event names this
 * delivery by commitment or, with none, by amount: what 1Click delivered, less the note price
 * when a note was accepted. More than one is ambiguous and throws.
 */
export function matchBookings(
  events: Booking[],
  note: NoteRecord,
  acceptedId: number | null,
  delivered: bigint,
): Booking[] {
  const fee = BigInt(PROFILE.pins?.config.note_fee ?? '0');
  const expected = acceptedId === null ? delivered : delivered - BigInt(note.denomination) - fee;
  const mine = events.filter((e) => {
    const value = BigInt(e.amount);
    if (value === 0n) return false;
    if (e.event === 'payout_pending' && e.account !== note.payout) return false;
    const named =
      e.commitment === undefined
        ? value === expected
        : e.commitment === note.commitment.toLowerCase();
    if (!named) return false;
    if (e.noteId !== undefined && acceptedId !== null && e.noteId !== acceptedId) return false;
    switch (e.event) {
      case 'payout_pending':
        return ['below_note_fee', 'transfer_failed', 'intents_excess'].includes(e.reason);
      case 'dust':
        return e.reason === 'below_note_fee' && value < fee;
      default:
        return e.reason === 'intents_bad_msg' && acceptedId === null;
    }
  });
  if (mine.length > 1) throw new Error('ambiguous vault delivery events; the note is not accepted');
  return mine;
}

export type IntentsResult =
  | (Extract<Acceptance, { status: 'accepted' }> & { bookings: Booking[]; pending?: string })
  | { status: 'booked'; bookings: Booking[]; pending?: string }
  | { status: 'pending'; reason: string; pending?: string };

/**
 * Reads the destination transaction from the regular endpoints, then from `archival` when they
 * return nothing or an unfinished tree. Returns the status and the client that answered.
 */
type Destination = { status: TxStatus; client: RpcClient } | { reason: string };
async function readDestination(
  rpc: RpcClient,
  archival: RpcClient | undefined,
  hash: string,
): Promise<Destination> {
  let reason = 'the destination transaction could not be read from the RPC provider yet';
  for (const client of archival && archival !== rpc ? [rpc, archival] : [rpc]) {
    try {
      const status = await client.txStatus(hash, INTENTS_SIGNER);
      const final = (status as { final_execution_status?: string }).final_execution_status;
      if ((final !== undefined && final !== 'FINAL') || !treeComplete(status)) {
        reason = 'the destination transaction is not final yet';
        continue;
      }
      return { status, client };
    } catch {
      // Try the archival endpoint next.
    }
  }
  return { reason };
}

/**
 * Accepts the note from the `deposit` event in the destination transaction, read at final
 * finality once every receipt of the tree is present. Note ID, commitment, amount and payout
 * account come from the event; one that differs leaves the note pending, never accepted. A
 * delivery the vault booked without a note (`payout_pending`, `dust`, `unclaimed`) comes back as
 * `booked`; `SUCCESS` with no event at all confirms nothing. `delivered` is what 1Click settled.
 * Where there is something to explain, `pending_payout` of the payout account is read from the
 * endpoint that gave the receipt (that read shows the account to the RPC provider).
 */
export async function confirmIntents(
  rpc: RpcClient,
  note: NoteRecord,
  hash: string,
  options: { archival?: RpcClient; delivered?: string } = {},
): Promise<IntentsResult> {
  const pending = (reason: string, extra: { pending?: string } = {}): IntentsResult => ({
    status: 'pending',
    reason,
    ...extra,
  });
  const read = await readDestination(rpc, options.archival, hash);
  if ('reason' in read) return pending(read.reason);
  const { status, client } = read;
  const ours = depositEvents(status, note.vault).filter(
    (e) => e.commitment === note.commitment.toLowerCase(),
  );
  let bookings: Booking[];
  try {
    bookings = matchBookings(
      bookingEvents(status, note.vault),
      note,
      ours[0]?.noteId ?? null,
      BigInt(options.delivered ?? notePrice(note.denomination)),
    );
  } catch (e) {
    return pending(e instanceof Error ? e.message : String(e));
  }
  const waiting =
    ours.length !== 1 || bookings.length > 0
      ? await pendingPayout(client, note.vault, note.payout).then(
          (v) => ({ pending: v.toString() }),
          () => ({}),
        )
      : {};
  if (ours.length === 0 && bookings.length > 0) return { status: 'booked', bookings, ...waiting };
  if (ours.length !== 1) {
    return pending(
      'the destination transaction has no unique deposit event for this note',
      waiting,
    );
  }
  // A receipt that logged the event and then failed accepted nothing.
  const logged = status.receipts_outcome?.find(
    (r) =>
      r.outcome?.executor_id === note.vault &&
      r.outcome.logs?.some((l) => l.toLowerCase().includes(note.commitment.toLowerCase())),
  );
  const result = logged?.outcome?.status as Json | undefined;
  if (!result || !('SuccessValue' in result || 'SuccessReceiptId' in result)) {
    return pending('the vault receipt with the deposit event did not succeed', waiting);
  }
  const match = matchDeposit(ours, {
    commitment: note.commitment,
    denomination: note.denomination,
    payout: note.payout,
  });
  if (!match.ok) {
    return pending(
      `the deposit event differs from the request (${(match.mismatched ?? []).join(', ')}): the note is not accepted`,
      waiting,
    );
  }
  const accepted = await finalize({ ...note, txHash: hash }, match.event, 'rpc');
  return accepted.status === 'accepted'
    ? { ...accepted, bookings, ...waiting }
    : pending('the note could not be stored', waiting);
}
