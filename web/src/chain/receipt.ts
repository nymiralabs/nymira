// Acceptance of a deposit is read from the `deposit` event in the receipts of the user's own
// transaction, never from a `note_status` call. The vault emits (contracts/vault, `open_note`):
//   EVENT_JSON:{"standard":"nymira","version":"1.0.0","event":"deposit","data":{
//     "note_id":<u32>,"commitment":"0x..","denomination":"<base units>","expiry":<unix s>,
//     "withdraw_to":"<account>"}}
import type { TxStatus } from './rpc';

export interface DepositEvent {
  noteId: number;
  commitment: string;
  denomination: string;
  expiry: number;
  withdrawTo: string;
}

const PREFIX = 'EVENT_JSON:';

/** Every `deposit` event of the `nymira` standard emitted by `vault` anywhere in the outcome. */
export function depositEvents(status: TxStatus, vault: string): DepositEvent[] {
  const found: DepositEvent[] = [];
  for (const receipt of status.receipts_outcome ?? []) {
    const outcome = receipt.outcome;
    const result = outcome?.status;
    if (
      !outcome ||
      outcome.executor_id !== vault ||
      typeof result !== 'object' ||
      result === null ||
      !('SuccessValue' in result || 'SuccessReceiptId' in result)
    )
      continue;
    for (const line of outcome.logs ?? []) {
      if (!line.startsWith(PREFIX)) continue;
      let body: unknown;
      try {
        body = JSON.parse(line.slice(PREFIX.length));
      } catch {
        continue;
      }
      const event = parseDeposit(body);
      if (event) found.push(event);
    }
  }
  return found;
}

function parseDeposit(body: unknown): DepositEvent | null {
  if (typeof body !== 'object' || body === null) return null;
  const e = body as { standard?: unknown; event?: unknown; data?: unknown };
  if (e.standard !== 'nymira' || e.event !== 'deposit') return null;
  const d = e.data as Record<string, unknown> | undefined;
  if (!d) return null;
  const { note_id, commitment, denomination, expiry, withdraw_to } = d;
  if (
    typeof note_id !== 'number' ||
    !Number.isInteger(note_id) ||
    typeof commitment !== 'string' ||
    typeof denomination !== 'string' ||
    typeof expiry !== 'number' ||
    typeof withdraw_to !== 'string'
  ) {
    return null;
  }
  return {
    noteId: note_id,
    commitment: commitment.toLowerCase(),
    denomination,
    expiry,
    withdrawTo: withdraw_to,
  };
}

/** What the vault booked instead of (or beside) a note, from a successful receipt. */
export interface Booking {
  event: 'payout_pending' | 'dust' | 'unclaimed';
  /** `payout_pending` only: the account the amount waits for. */
  account?: string;
  /** USDC base units, digits only. */
  amount: string;
  reason: string;
  noteId?: number;
  commitment?: string;
}

const BOOKINGS = ['payout_pending', 'dust', 'unclaimed'];

/** `payout_pending`, `dust` and `unclaimed` events the vault logged in receipts that succeeded. */
export function bookingEvents(status: TxStatus, vault: string): Booking[] {
  const found: Booking[] = [];
  for (const receipt of status.receipts_outcome ?? []) {
    const outcome = receipt.outcome;
    const result = outcome?.status as Record<string, unknown> | undefined;
    if (outcome?.executor_id !== vault || !result) continue;
    if (!('SuccessValue' in result || 'SuccessReceiptId' in result)) continue;
    for (const line of outcome.logs ?? []) {
      if (!line.startsWith(PREFIX)) continue;
      let body: { standard?: unknown; event?: unknown; data?: Record<string, unknown> };
      try {
        body = JSON.parse(line.slice(PREFIX.length));
      } catch {
        continue;
      }
      const d = body?.data;
      if (body?.standard !== 'nymira' || !BOOKINGS.includes(body.event as string) || !d) continue;
      if (typeof d.amount !== 'string' || !/^\d+$/.test(d.amount) || typeof d.reason !== 'string') {
        continue;
      }
      found.push({
        event: body.event as Booking['event'],
        amount: d.amount,
        reason: d.reason,
        ...(typeof d.account === 'string' ? { account: d.account } : {}),
        ...(typeof d.note_id === 'number' ? { noteId: d.note_id } : {}),
        ...(typeof d.commitment === 'string' ? { commitment: d.commitment.toLowerCase() } : {}),
      });
    }
  }
  return found;
}

export interface ExpectedDeposit {
  commitment: string;
  denomination: string;
  payout: string;
}

export type MatchResult =
  | { ok: true; event: DepositEvent }
  | { ok: false; reason: 'no_event' | 'mismatch'; mismatched?: string[] };

/**
 * The event must carry our commitment, our denomination and our payout account. A matching
 * commitment with another payout or amount is reported as a mismatch (a tampered page or wallet),
 * never accepted.
 */
export function matchDeposit(events: DepositEvent[], expected: ExpectedDeposit): MatchResult {
  if (events.length === 0) return { ok: false, reason: 'no_event' };
  const commitment = expected.commitment.toLowerCase();
  const ours = events.filter((e) => e.commitment === commitment);
  if (ours.length === 0) return { ok: false, reason: 'mismatch', mismatched: ['commitment'] };
  const event = ours[0] as DepositEvent;
  const wrong: string[] = [];
  if (event.denomination !== expected.denomination) wrong.push('amount');
  if (event.withdrawTo !== expected.payout) wrong.push('payout account');
  return wrong.length === 0
    ? { ok: true, event }
    : { ok: false, reason: 'mismatch', mismatched: wrong };
}

/** The transaction hash in a wallet's outcome, when it gave one. */
export function txHashOf(outcome: unknown): string | null {
  const o = outcome as TxStatus | null | undefined;
  return o?.transaction?.hash ?? o?.transaction_outcome?.id ?? null;
}

/** A wallet outcome is "the full receipt tree" when it carries receipt outcomes at all. */
export function hasReceiptTree(outcome: unknown): outcome is TxStatus {
  const o = outcome as TxStatus | null | undefined;
  return Array.isArray(o?.receipts_outcome) && o.receipts_outcome.length > 0;
}

/** Every receipt a receipt of the outcome points to is in the outcome: the whole tree is there. */
export function treeComplete(status: TxStatus): boolean {
  const all = status.receipts_outcome ?? [];
  const ids = new Set(all.map((r) => r.id));
  return [status.transaction_outcome, ...all].every((r) =>
    (r?.outcome?.receipt_ids ?? []).every((id) => ids.has(id)),
  );
}
