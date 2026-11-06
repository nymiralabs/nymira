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
import type { RpcClient } from '../chain/rpc';
import { depositEvents, matchDeposit, treeComplete } from '../chain/receipt';
import { putSetting } from '../store/db';
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

export type Json = Record<string, unknown>;

export interface Attempt {
  /** The id of the pending note this attempt funds. */
  noteId: string;
  source: string;
  refund: string;
  request: Json;
  quote: Json;
  createdAt: number;
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
  await putSetting(`intents:${note.id}`, attempt);
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
  { state: 'pending' } | { state: 'failed'; status: string } | { state: 'success'; hash: string };

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
      return { state: 'success', hash: hashes[0] };
    }
    default:
      throw new Error('1Click gave an unknown status');
  }
}

/**
 * Accepts the note from the `deposit` event in the destination transaction, read at final
 * finality once every receipt of the tree is present. Note ID, commitment, amount and payout
 * account come from the event; one that differs leaves the note pending, never accepted.
 */
export async function confirmIntents(
  rpc: RpcClient,
  note: NoteRecord,
  hash: string,
): Promise<Acceptance> {
  const pending = (reason: string): Acceptance => ({ status: 'pending', reason });
  let status;
  try {
    status = await rpc.txStatus(hash, INTENTS_SIGNER);
  } catch {
    return pending('the destination transaction could not be read from the RPC provider yet');
  }
  const final = (status as { final_execution_status?: string }).final_execution_status;
  if ((final !== undefined && final !== 'FINAL') || !treeComplete(status)) {
    return pending('the destination transaction is not final yet');
  }
  const ours = depositEvents(status, note.vault).filter(
    (e) => e.commitment === note.commitment.toLowerCase(),
  );
  if (ours.length !== 1) {
    return pending('the destination transaction has no unique deposit event for this note');
  }
  // A receipt that logged the event and then failed accepted nothing.
  const logged = status.receipts_outcome?.find(
    (r) =>
      r.outcome?.executor_id === note.vault &&
      r.outcome.logs?.some((l) => l.toLowerCase().includes(note.commitment.toLowerCase())),
  );
  const result = logged?.outcome?.status as Json | undefined;
  if (!result || !('SuccessValue' in result || 'SuccessReceiptId' in result)) {
    return pending('the vault receipt with the deposit event did not succeed');
  }
  const match = matchDeposit(ours, {
    commitment: note.commitment,
    denomination: note.denomination,
    payout: note.payout,
  });
  if (!match.ok) {
    return pending(
      `the deposit event differs from the request (${(match.mismatched ?? []).join(', ')}): the note is not accepted`,
    );
  }
  return finalize({ ...note, txHash: hash }, match.event, 'rpc');
}
