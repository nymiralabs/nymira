// The funding flow, step by step. Each step stores its result before the next one can start, so a
// closed tab never leaves money without its secret:
//
//   prepareNote     secret and commitment made in the worker, sealed, stored (phase prepared)
//   backup + test   an encrypted backup file is built, downloaded, chosen again and decrypted
//                   (phase backed_up). No transfer is offered before this.
//   sendDeposit     vault re-checked against the pins, phase sending stored, then the wallet gets
//                   `ft_transfer_call`; phase sent stored with the transaction hash
//   confirmDeposit  acceptance read from the `deposit` event of the user's own receipts, never
//                   from `note_status`; genesis state sealed and stored (phase accepted)
import { PROFILE } from '../profile';
import { RpcError, type RpcClient, type TxStatus } from '../chain/rpc';
import {
  depositEvents,
  hasReceiptTree,
  matchDeposit,
  txHashOf,
  type DepositEvent,
} from '../chain/receipt';
import { checkVault } from '../chain/vault';
import { getNote, listNotes, putNote } from '../store/db';
import { noteAad, type NoteRecord } from '../store/types';
import { callWorker } from '../worker/client';
import { requireKey } from '../crypto/wrap';
import type { FunctionCallAction } from '../wallet/protocol';
import { raceWallet, type RaceOptions } from '../wallet/watch';
import { findDeposit, scanStart, type FoundDeposit } from './watch';

/** 100 Tgas: the token's own work, the vault's `ft_on_transfer` and the refund callback. */
export const TRANSFER_GAS = '100000000000000';

export interface DepositPlan {
  /** Denomination in base units: one of `config().denominations`. */
  denomination: string;
  /** The payout account. Equal to `sender` unless the user chose another. */
  payout: string;
  sender: string;
}

/** The price of a note: the denomination plus the non-refundable note fee from `config()`. */
export function notePrice(denomination: string): bigint {
  const pins = PROFILE.pins;
  if (!pins) throw new Error('this build has no vault pins');
  return BigInt(denomination) + BigInt(pins.config.note_fee);
}

export function depositMessage(commitment: string, payout: string, sender: string): string {
  // The default payout (the sending account) is left out of the message: nothing to tamper with.
  return JSON.stringify(payout === sender ? { commitment } : { commitment, withdraw_to: payout });
}

export function transferAction(record: NoteRecord): FunctionCallAction {
  return {
    type: 'FunctionCall',
    params: {
      methodName: 'ft_transfer_call',
      args: {
        receiver_id: record.vault,
        amount: notePrice(record.denomination).toString(),
        msg: depositMessage(record.commitment, record.payout, record.sender),
      },
      gas: TRANSFER_GAS,
      deposit: '1',
    },
  };
}

export async function prepareNote(plan: DepositPlan): Promise<NoteRecord> {
  const pins = PROFILE.pins;
  if (!pins) throw new Error('this build has no vault pins');
  if (!pins.config.denominations.includes(plan.denomination)) {
    throw new Error('that denomination is not offered by the vault');
  }
  const id = crypto.randomUUID();
  const aad = noteAad(PROFILE.network, PROFILE.vaultAccount, id);
  const { commitment, sealed } = await callWorker({ op: 'generate', key: requireKey(), aad });
  const now = Date.now();
  const record: NoteRecord = {
    id,
    network: PROFILE.network,
    vault: PROFILE.vaultAccount,
    token: PROFILE.tokenAccount,
    denomination: plan.denomination,
    payout: plan.payout,
    payoutExplicit: plan.payout !== plan.sender,
    sender: plan.sender,
    commitment,
    phase: 'prepared',
    createdAt: now,
    updatedAt: now,
    kind: 'pending',
    sealed,
  };
  await putNote(record);
  return record;
}

async function update(record: NoteRecord, patch: Partial<NoteRecord>): Promise<NoteRecord> {
  const next = { ...record, ...patch, updatedAt: Date.now() };
  await putNote(next);
  return next;
}

/** Notes of this build's vault, newest first. */
export async function loadNotes(): Promise<NoteRecord[]> {
  const all = await listNotes();
  return all
    .filter((n) => n.network === PROFILE.network && n.vault === PROFILE.vaultAccount)
    .sort((a, b) => b.createdAt - a.createdAt);
}

/** The note records a backup should hold: every note of this vault that holds a secret. */
export async function backupScope(): Promise<NoteRecord[]> {
  return (await loadNotes()).filter((n) => n.phase !== 'rejected');
}

export async function makeBackupFile(passphrase: string): Promise<Uint8Array> {
  return callWorker({
    op: 'makeBackup',
    key: requireKey(),
    records: await backupScope(),
    passphrase,
    now: Date.now(),
    scope: { network: PROFILE.network, vault: PROFILE.vaultAccount },
  });
}

/** Decrypts the chosen file and checks it holds this note's secret; stores `backed_up` on success. */
export async function testRestore(
  record: NoteRecord,
  bytes: Uint8Array,
  passphrase: string,
): Promise<NoteRecord> {
  const check = await callWorker({
    op: 'testBackup',
    key: requireKey(),
    bytes,
    passphrase,
    record,
  });
  if (!check.matches) throw new Error('the file does not hold this note: download a new backup');
  return update(record, { phase: 'backed_up' });
}

export type SendBlock = { ok: true } | { ok: false; reason: string };

/** The checks that must pass right before a transfer goes to the wallet. */
export async function preDepositChecks(rpc: RpcClient, record: NoteRecord): Promise<SendBlock> {
  if (record.phase !== 'backed_up') {
    return { ok: false, reason: 'the backup has not been downloaded and test-restored' };
  }
  const check = await checkVault(rpc, PROFILE.vaultAccount, PROFILE.pins);
  if (check.status === 'unpinned') return { ok: false, reason: 'this build has no vault pins' };
  if (check.status === 'mismatch') {
    return { ok: false, reason: `the vault changed: ${check.diffs.join(', ')}` };
  }
  if (check.status === 'unreachable') {
    return { ok: false, reason: `the vault could not be checked: ${check.detail}` };
  }
  if (check.limits.currentTvl + notePrice(record.denomination) > check.limits.tvlCap) {
    return { ok: false, reason: 'the vault is full: its deposit cap would be exceeded' };
  }
  return { ok: true };
}

export interface SendResult {
  record: NoteRecord;
  outcome: unknown;
}

export async function sendDeposit(
  rpc: RpcClient,
  record: NoteRecord,
  wallet: { send(receiver: string, action: FunctionCallAction): Promise<unknown> },
): Promise<SendResult> {
  const block = await preDepositChecks(rpc, record);
  if (!block.ok) throw new Error(block.reason);
  // Stored before the wallet is asked: after a crash the note is found again from its receipt.
  const sending = await update(record, { phase: 'sending' });
  const outcome = await wallet.send(record.token, transferAction(record));
  const hash = txHashOf(outcome) ?? undefined;
  const sent = await update(sending, { phase: 'sent', ...(hash ? { txHash: hash } : {}) });
  return { record: sent, outcome };
}

/** The wallet never answered and the chain showed nothing: the app stops waiting. */
export class WalletSilentError extends Error {
  constructor() {
    super(
      'The wallet did not answer and the vault shows no deposit of this note. If you approved in the wallet, the transfer may still land: open the Notes tab and look the deposit up by its transaction hash (from your wallet’s history or the explorer).',
    );
  }
}

export interface WatchedSend extends SendResult {
  /** Set when the chain, not the wallet, showed the deposit. */
  found?: FoundDeposit;
}

/**
 * Like `sendDeposit`, but the app does not depend on the wallet returning. While it waits it reads
 * the vault for this note's leaf; when the leaf appears it goes on without the wallet. A refusal
 * counts only after a grace period in which the chain shows nothing (a closed window may still
 * have sent). A wallet that never answers ends in `WalletSilentError`.
 */
export async function sendDepositWatched(
  rpc: RpcClient,
  record: NoteRecord,
  wallet: { send(receiver: string, action: FunctionCallAction): Promise<unknown> },
  race: RaceOptions = {},
  leafOf: (n: number, c: string, a: string, e: number) => Promise<string> = (
    noteId,
    commitment,
    amount,
    expiry,
  ) => callWorker({ op: 'noteLeaf', noteId, commitment, amount, expiry }),
): Promise<WatchedSend> {
  const block = await preDepositChecks(rpc, record);
  if (!block.ok) throw new Error(block.reason);
  const sending = await update(record, { phase: 'sending' });
  const from = await scanStart(rpc, record.vault).catch(() => 0);
  const raced = await raceWallet(
    wallet.send(record.token, transferAction(record)),
    () => findDeposit(rpc, record.vault, record, from, leafOf),
    race,
  );
  if (raced.by === 'rejected') throw raced.error;
  if (raced.by === 'timeout') throw new WalletSilentError();
  if (raced.by === 'chain') {
    return {
      record: await update(sending, { phase: 'sent' }),
      outcome: null,
      found: raced.found,
    };
  }
  const hash = txHashOf(raced.outcome) ?? undefined;
  const sent = await update(sending, { phase: 'sent', ...(hash ? { txHash: hash } : {}) });
  return { record: sent, outcome: raced.outcome };
}

/** Accepts a note the chain showed (no receipt): same sealing as a read receipt. */
export async function acceptFound(record: NoteRecord, found: FoundDeposit): Promise<Acceptance> {
  return finalize(
    record,
    {
      noteId: found.noteId,
      commitment: record.commitment,
      denomination: found.denomination,
      expiry: found.expiry,
      withdrawTo: record.payout,
    },
    'chain',
  );
}

export type AcceptanceSource = 'wallet' | 'rpc' | 'chain';

export type Acceptance =
  | { status: 'accepted'; record: NoteRecord; event: DepositEvent; source: AcceptanceSource }
  | { status: 'rejected'; record: NoteRecord; reason: string }
  | { status: 'pending'; reason: string };

export interface ConfirmOptions {
  /** How long to wait for a transaction the RPC does not know yet. */
  waitMs?: number;
  sleep?: (ms: number) => Promise<void>;
  /** Called when the app is about to ask the RPC provider, so the screen can say so. */
  onRpcRead?: () => void;
}

const delay = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

/**
 * Reads acceptance from the receipts of the user's own transaction. A receipt tree from the wallet
 * is used when it has the event; otherwise the RPC provider is asked for the transaction (and the
 * screen says what that shows it). `note_status` is never called.
 */
export async function confirmDeposit(
  rpc: RpcClient,
  record: NoteRecord,
  walletOutcome: unknown,
  options: ConfirmOptions = {},
): Promise<Acceptance> {
  const expected = {
    commitment: record.commitment,
    denomination: record.denomination,
    payout: record.payout,
  };
  const check = (status: TxStatus, source: AcceptanceSource) => {
    const signer = status.transaction?.signer_id;
    if (signer !== undefined && signer !== record.sender) {
      return { ok: false as const, reason: 'the transaction was signed by another account' };
    }
    const events = depositEvents(status, record.vault);
    const match = matchDeposit(events, expected);
    return match.ok
      ? { ok: true as const, event: match.event, source }
      : { ok: false as const, reason: describe(match) };
  };

  // 1. The wallet's own receipt tree, when it has the event.
  if (hasReceiptTree(walletOutcome)) {
    const fromWallet = check(walletOutcome, 'wallet');
    if (fromWallet.ok) return finalize(record, fromWallet.event, 'wallet');
    if (fromWallet.reason.startsWith('the event does not match')) {
      return {
        status: 'rejected',
        record: await update(record, { phase: 'rejected' }),
        reason: fromWallet.reason,
      };
    }
  }

  // 2. The RPC provider, by transaction hash.
  const hash = txHashOf(walletOutcome) ?? record.txHash;
  if (!hash) {
    return {
      status: 'pending',
      reason: 'the wallet gave no transaction hash: enter it to look the note up',
    };
  }
  // A hash typed by the user (nothing came from the wallet in this flow) can be any transaction
  // of the account. A transaction that is not this deposit says nothing about the note, which
  // may well be funded under another hash: such a lookup never marks the note rejected.
  const supplied = walletOutcome == null;
  options.onRpcRead?.();
  const deadline = Date.now() + (options.waitMs ?? 60_000);
  const sleep = options.sleep ?? delay;
  for (;;) {
    try {
      const status = await rpc.txStatus(hash, record.sender);
      const found = check(status, 'rpc');
      if (found.ok) return finalize({ ...record, txHash: hash }, found.event, 'rpc');
      if (supplied) {
        return {
          status: 'pending',
          reason: `${found.reason}: this transaction does not show the deposit of this note; check the hash and try again`,
        };
      }
      if (
        found.reason.startsWith('the event does not match') ||
        found.reason.startsWith('the transaction was signed')
      ) {
        return {
          status: 'rejected',
          record: await update(record, { phase: 'rejected', txHash: hash }),
          reason: found.reason,
        };
      }
      // A complete outcome with no deposit event: the vault refused and the token refunded.
      return {
        status: 'rejected',
        record: await update(record, { phase: 'rejected', txHash: hash }),
        reason: 'the vault did not accept the deposit; the token refunds a refused transfer',
      };
    } catch (error) {
      const unknown = error instanceof RpcError && error.code === 'UNKNOWN_TRANSACTION';
      const network = !(error instanceof RpcError);
      if (!(unknown || network) || Date.now() >= deadline) {
        return {
          status: 'pending',
          reason: unknown
            ? 'the RPC provider does not know this transaction (public testnet RPC keeps about a day)'
            : 'the RPC provider could not be reached',
        };
      }
      await sleep(1500);
    }
  }
}

function describe(match: ReturnType<typeof matchDeposit>): string {
  if (match.ok) return '';
  if (match.reason === 'no_event') return 'no deposit event in the receipts';
  return `the event does not match what was requested: ${(match.mismatched ?? []).join(', ')}`;
}

async function finalize(
  record: NoteRecord,
  event: DepositEvent,
  source: AcceptanceSource,
): Promise<Acceptance> {
  const pins = PROFILE.pins;
  if (!pins) throw new Error('this build has no vault pins');
  const sealed = await callWorker({
    op: 'confirm',
    key: requireKey(),
    aad: noteAad(record.network, record.vault, record.id),
    pending: record.sealed,
    input: {
      vault: record.vault,
      config: {
        chainId: pins.config.chain_id,
        operatorPk: pins.config.operator_pk,
        clearancePk: pins.config.clearance_pk,
        maxBound: [...pins.config.bound_classes]
          .map(BigInt)
          .reduce((a, b) => (a > b ? a : b))
          .toString(),
      },
      noteId: event.noteId,
      amount: event.denomination,
      expiry: event.expiry,
    },
  });
  const next = await update(record, {
    phase: 'accepted',
    kind: 'state',
    sealed,
    noteId: event.noteId,
    expiry: event.expiry,
  });
  return { status: 'accepted', record: next, event, source };
}

/** Records that were interrupted between storing the secret and reading the receipt. */
export async function interruptedNotes(): Promise<NoteRecord[]> {
  return (await loadNotes()).filter((n) => n.phase === 'sending' || n.phase === 'sent');
}

/** Sets the transaction hash a user found in their wallet or the explorer, for a lost record. */
export async function attachHash(id: string, hash: string): Promise<NoteRecord> {
  const record = await getNote(id);
  if (!record) throw new Error('unknown note');
  if (!/^[1-9A-HJ-NP-Za-km-z]{43,44}$/.test(hash))
    throw new Error('that is not a transaction hash');
  return update(record, { txHash: hash });
}
