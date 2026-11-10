// Leaving a note: `closeNote` (the operator's clearance, then `mutual_close`), `requestExit`
// (`request_exit`, no clearance), `finalizeExit` (`finalize_exit` after the deadline) and
// `claimPayout`. It follows `nymirad`'s `gateway/close.rs` step for step, with the browser's
// differences:
//   - the transactions are signed by the user's wallet, not by a submitter account, so the same
//     wallet account is on chain next to the note ID (the close screen says so);
//   - a proof is made only after the wallet account has been seen to hold NEAR for gas;
//   - every proof is recorded in the sealed payload and the note's public `leave` state before it
//     is sent, so a crash or a reload cannot lose track of a proof that left;
//   - times come from where the verifier takes them: the operator's signed status time for the
//     ownership proof, the chain's final block time for the proofs the vault judges and for the
//     exit deadline.
import { checkVault } from '../chain/vault';
import { deadlineSecs, exitRequest, pendingPayout } from '../chain/leave';
import { readNoteRecord } from '../chain/tree';
import type { RpcClient } from '../chain/rpc';
import {
  OperatorClient,
  OperatorError,
  OperatorUnreachable,
  describeOperatorError,
} from '../chat/operator';
import { GateError, verifyStatus, type SignatureCheck } from '../chat/gate';
import type { Profile } from '../profiles/types';
import { patchNote } from '../store/db';
import { noteAad, type LeaveInfo, type NoteRecord } from '../store/types';
import type { FunctionCallAction } from '../wallet/protocol';
import { raceWallet, type RaceOptions } from '../wallet/watch';
import { callWorker } from '../worker/client';
import type { LeaseConfigInput } from '../worker/protocol';
import { callArgs, clearanceBody, planParts, ExitError } from '../note/exit';
import type { NoteView } from '../note/lease';
import { txHashOf } from '../chain/receipt';
import type { TxStatus } from '../chain/rpc';

/** 100 Tgas: `mutual_close` needs 80 and the payouts 30 (the same figure `nymirad` attaches). */
export const GAS = '100000000000000';
/**
 * A wallet account with less than 0.02 NEAR is treated as having none (`nymirad`'s figure). A call
 * prepays its attached gas: 100 Tgas at the minimum gas price (0.0001 NEAR per Tgas) is 0.01
 * NEAR, refunded when unused but needed up front; the margin covers a higher gas price.
 */
export const MIN_NEAR_YOCTO = 20_000_000_000_000_000_000_000n;

export interface LeaveDeps {
  profile: Profile;
  rpc: RpcClient;
  operator: OperatorClient;
  wallet: {
    send(receiver: string, action: FunctionCallAction): Promise<unknown>;
    connected: { accountId: string } | null;
  };
  signatureCheck: SignatureCheck;
  /** `withdrawal_words` of the web-checks module: the 16 statement words as a JSON array of hex. */
  words: (publicInputsJson: string) => string;
  getKey: () => CryptoKey;
  /** Milliseconds. */
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
  /** How many times, and how far apart, the vault is asked whether a send took effect. */
  polls?: number;
  pollMs?: number;
  /** How the app waits for a wallet that may never answer (src/wallet/watch.ts). */
  race?: RaceOptions;
}

export class LeaveError extends Error {
  constructor(
    readonly kind: string,
    message: string,
  ) {
    super(message);
    this.name = 'LeaveError';
  }
}

export type Step = (text: string) => void;

const nowMs = (d: LeaveDeps) => (d.now ?? Date.now)();
const nowSecs = (d: LeaveDeps) => Math.floor(nowMs(d) / 1000);
const sleep = (d: LeaveDeps, ms: number) =>
  d.sleep ? d.sleep(ms) : new Promise<void>((r) => setTimeout(r, ms));

const aadOf = (n: NoteRecord) => noteAad(n.network, n.vault, n.id);

function configOf(d: LeaveDeps): LeaseConfigInput {
  const pins = d.profile.pins;
  if (!pins) throw new LeaveError('no_pins', 'This build has no vault pins.');
  return {
    chainId: pins.config.chain_id,
    operatorPk: pins.config.operator_pk,
    clearancePk: pins.config.clearance_pk,
    vault: d.profile.vaultAccount,
  };
}

async function save(
  d: LeaveDeps,
  note: NoteRecord,
  patch: Partial<NoteRecord>,
): Promise<NoteRecord> {
  return patchNote(note, patch, nowMs(d));
}

const leaveOf = (d: LeaveDeps, state: LeaveInfo['state'], more: Partial<LeaveInfo> = {}) => ({
  state,
  at: nowSecs(d),
  ...more,
});

/** The wording for a failure of the operator or the chain, never raw JSON. */
export function describe(e: unknown): string {
  if (e instanceof LeaveError || e instanceof ExitError) return e.message;
  if (e instanceof OperatorError || e instanceof OperatorUnreachable)
    return describeOperatorError(e);
  if (e instanceof GateError) return e.message;
  return e instanceof Error ? e.message : String(e);
}

export async function noteView(d: LeaveDeps, note: NoteRecord): Promise<NoteView> {
  return callWorker({ op: 'readNote', key: d.getKey(), aad: aadOf(note), sealed: note.sealed });
}

/** A note with a lease open or unsettled cannot leave by a close; an exit still can (it concedes the bound). */
async function checkIdle(d: LeaveDeps, note: NoteRecord, forExit: boolean): Promise<NoteView> {
  const view = await noteView(d, note);
  if (view.active?.token !== undefined)
    throw new LeaveError('lease_open', 'A lease is open on this note. End the conversation first.');
  if (view.pending && !forExit)
    throw new LeaveError(
      'lease_pending',
      'An earlier lease on this note is not settled yet. Settle it first.',
    );
  return view;
}

/** The wallet account must be able to pay gas before any proof exists: a half-started exit helps no one. */
async function needNear(d: LeaveDeps): Promise<string> {
  const account = d.wallet.connected?.accountId;
  if (!account) throw new LeaveError('no_wallet', 'Connect a wallet account first.');
  const amount = BigInt((await d.rpc.viewAccount(account)).amount);
  if (amount < MIN_NEAR_YOCTO) {
    throw new LeaveError(
      'no_near',
      `${account} holds too little NEAR to pay gas (it needs about 0.02 NEAR). Connect an account that has some; any account can send this, and the payout still goes to your payout account.`,
    );
  }
  return account;
}

function failureOf(outcome: unknown): string | null {
  const status = (outcome as TxStatus | null | undefined)?.status as
    { Failure?: unknown } | undefined;
  return status && typeof status === 'object' && 'Failure' in status
    ? JSON.stringify(status.Failure)
    : null;
}

const call = (methodName: string, args: unknown): FunctionCallAction => ({
  type: 'FunctionCall',
  params: { methodName, args, gas: GAS, deposit: '0' },
});

async function waitFor(d: LeaveDeps, check: () => Promise<boolean>): Promise<boolean> {
  const polls = d.polls ?? 15;
  for (let i = 0; i < polls; i++) {
    if (await check().catch(() => false)) return true;
    if (i < polls - 1) await sleep(d, d.pollMs ?? 2000);
  }
  return false;
}

const isClosed = async (d: LeaveDeps, note: NoteRecord) =>
  (await readNoteRecord(d.rpc, d.profile.vaultAccount, note.noteId!))?.status === 'Closed';

/** A close or exit works on a note that was accepted and has a note ID. */
function need(note: NoteRecord): number {
  if (note.kind !== 'state' || note.phase !== 'accepted' || note.noteId === undefined)
    throw new LeaveError('not_open', 'This note is not an open, accepted note.');
  return note.noteId;
}

// ---- close ----

export interface Closed {
  note: NoteRecord;
  txHash: string;
  via: 'wallet' | 'operator';
  /** The vault shows the note closed. `false` after the operator took a close that has not landed yet. */
  confirmed: boolean;
}

/**
 * Closes a note to its payout account. The operator signs a clearance for an ownership proof; the
 * close proof is valid for 3 hours; `mutual_close` goes out from the user's wallet (no close fee) or
 * through the operator's `POST /v1/close` (the operator sends it and takes the close fee).
 */
async function closeNoteRun(
  d: LeaveDeps,
  start: NoteRecord,
  opts: { via: 'wallet' | 'operator'; step?: Step },
): Promise<Closed> {
  const step = opts.step ?? (() => undefined);
  let note = start;
  const noteId = need(note);
  if (note.leave?.state === 'closed')
    throw new LeaveError('closed', 'This note is already closed.');
  if (note.leave?.state === 'exiting' || note.leave?.state === 'exited')
    throw new LeaveError('exiting', 'This note is in an exit. Finalize the exit instead.');
  await checkIdle(d, note, false);
  if (opts.via === 'wallet') await needNear(d);

  // A close sent earlier may have landed after the answer was lost.
  if (await isClosed(d, note).catch(() => false)) {
    note = await save(d, note, { leave: leaveOf(d, 'closed', { via: opts.via }) });
    return { note, txHash: note.leave?.txHash ?? '', via: opts.via, confirmed: true };
  }

  step('Checking the operator’s signed status…');
  let ownershipNow: number;
  try {
    const status = await d.operator.status();
    const nonce = verifyStatus(
      status,
      d.profile.pins!.config.operator_pk,
      nowSecs(d),
      d.signatureCheck,
    );
    if (status.emergency_notice) {
      throw new LeaveError(
        'emergency',
        `The operator posted an emergency notice (${String(status.emergency_notice)}) and signs no clearance. Request an exit instead.`,
      );
    }
    ownershipNow = nonce.ts;
  } catch (e) {
    throw new LeaveError('operator', describe(e));
  }

  step('Making the ownership proof (about 10 seconds)…');
  const own = await callWorker({
    op: 'prepareWithdrawal',
    key: d.getKey(),
    aad: aadOf(note),
    sealed: note.sealed,
    input: { config: configOf(d), kind: 'ownership', now: ownershipNow },
  });
  // Recorded before it leaves: a lock and a released proof are in the sealed payload.
  note = await save(d, note, {
    sealed: own.sealed,
    leave: leaveOf(d, 'closing', { via: opts.via }),
  });

  step('Asking the operator for a clearance…');
  let clearance: string;
  try {
    clearance = await d.operator.clearance(clearanceBody(own.planJson));
  } catch (e) {
    throw new LeaveError(
      e instanceof OperatorError ? e.code || 'operator' : 'operator',
      describe(e),
    );
  }

  step('Making the close proof (about 10 seconds)…');
  const chainNow = await d.rpc.finalBlockTime();
  const close = await callWorker({
    op: 'prepareWithdrawal',
    key: d.getKey(),
    aad: aadOf(note),
    sealed: note.sealed,
    input: { config: configOf(d), kind: 'mutual', now: chainNow, payout: note.payout, clearance },
  });
  note = await save(d, note, { sealed: close.sealed });
  const args = callArgs(close.planJson, d.words(planParts(close.planJson).publicInputs));

  let txHash = '';
  if (opts.via === 'operator') {
    step('Handing the close to the operator…');
    try {
      txHash = await d.operator.close(args.proof, args.inputs);
    } catch (e) {
      throw new LeaveError('operator', describe(e));
    }
  } else {
    step('Waiting for your wallet to sign the close…');
    // The app does not depend on the wallet returning: while it waits it asks the vault whether
    // the note closed. A refusal counts only when the vault shows nothing after a grace period.
    const raced = await raceWallet(
      d.wallet.send(d.profile.vaultAccount, call('mutual_close', args)),
      async () => ((await isClosed(d, note)) ? true : null),
      d.race,
    );
    if (raced.by === 'rejected') {
      throw new LeaveError('wallet', `The wallet did not send the close: ${describe(raced.error)}`);
    }
    if (raced.by === 'timeout') {
      note = await save(d, note, { leave: leaveOf(d, 'closing', { via: opts.via }) });
      throw new LeaveError(
        'wallet_silent',
        'The wallet did not answer with a confirmed transaction, and the app could not confirm whether the note closed. If you approved in the wallet, the close may still land: use “Ask the vault whether the close landed” on this note, or the Notes tab to look it up by its transaction hash.',
      );
    }
    const outcome = raced.by === 'wallet' ? raced.outcome : null;
    txHash = txHashOf(outcome) ?? '';
    // A failed payout receipt marks the whole transaction failed although the vault closed the
    // note: the vault, not the outcome, says whether it closed.
    const failure = failureOf(outcome);
    if (failure && !(await isClosed(d, note).catch(() => false))) {
      throw new LeaveError('refused', `The vault refused the close: ${failure}`);
    }
  }

  step('Checking that the vault shows the note closed…');
  const confirmed = await waitFor(d, () => isClosed(d, note));
  if (confirmed) {
    note = await save(d, note, {
      leave: leaveOf(d, 'closed', { via: opts.via, ...(txHash ? { txHash } : {}) }),
    });
  } else if (txHash) {
    note = await save(d, note, { leave: leaveOf(d, 'closing', { via: opts.via, txHash }) });
  }
  return { note, txHash, via: opts.via, confirmed };
}

/** Asks the vault again whether a close that was sent has landed. */
export async function checkClosed(d: LeaveDeps, note: NoteRecord): Promise<NoteRecord> {
  need(note);
  if (await isClosed(d, note)) {
    return save(d, note, {
      leave: leaveOf(d, 'closed', {
        ...(note.leave?.via ? { via: note.leave.via } : {}),
        ...(note.leave?.txHash ? { txHash: note.leave.txHash } : {}),
      }),
    });
  }
  return note;
}

// ---- exit ----

export interface Exiting {
  note: NoteRecord;
  txHash?: string;
  /** What the exit concedes (micro-USDC): 0 when no request left the state, the stored bound otherwise. */
  conceded: string;
  /** Unix seconds after which `finalize_exit` works. */
  deadline: number;
}

/**
 * Files `request_exit` for the latest state: concession 0 when no request left the state, the stored
 * bound otherwise. No request is made on the note once the exit proof is made.
 */
async function requestExitRun(
  d: LeaveDeps,
  start: NoteRecord,
  opts: { step?: Step } = {},
): Promise<Exiting> {
  const step = opts.step ?? (() => undefined);
  let note = start;
  const noteId = need(note);
  if (note.leave?.state === 'closed')
    throw new LeaveError('closed', 'This note is already closed.');
  await checkIdle(d, note, true);

  // Filed already (a lost answer, a second try, another device): report it, make no second proof.
  const filed = await exitRequest(d.rpc, d.profile.vaultAccount, noteId);
  if (filed) {
    const deadline = deadlineSecs(filed);
    note = await save(d, note, { leave: leaveOf(d, 'exiting', { deadline }) });
    return { note, conceded: filed.conceded, deadline };
  }

  await needNear(d);
  step('Making the exit proof (about 10 seconds)…');
  const chainNow = await d.rpc.finalBlockTime();
  const proof = await callWorker({
    op: 'prepareWithdrawal',
    key: d.getKey(),
    aad: aadOf(note),
    sealed: note.sealed,
    input: { config: configOf(d), kind: 'escape', now: chainNow, payout: note.payout },
  });
  // From here the proof may leave: the lock is in the sealed payload and no request is made on this note.
  note = await save(d, note, { sealed: proof.sealed, leave: leaveOf(d, 'exiting') });
  const args = callArgs(proof.planJson, d.words(planParts(proof.planJson).publicInputs));

  step('Waiting for your wallet to sign the exit request…');
  let outcome: unknown;
  try {
    outcome = await d.wallet.send(d.profile.vaultAccount, call('request_exit', args));
  } catch (e) {
    throw new LeaveError('wallet', `The wallet did not send the exit request: ${describe(e)}`);
  }
  const txHash = txHashOf(outcome) ?? undefined;
  const failure = failureOf(outcome);

  step('Reading the exit deadline from the vault…');
  let row = await exitRequest(d.rpc, d.profile.vaultAccount, noteId).catch(() => undefined);
  if (!row) {
    await waitFor(d, async () => {
      row = await exitRequest(d.rpc, d.profile.vaultAccount, noteId);
      return row !== undefined;
    });
  }
  if (!row) {
    throw new LeaveError(
      failure ? 'refused' : 'not_listed',
      failure
        ? `The vault refused the exit request: ${failure}`
        : 'The exit request was sent, but the vault does not list it yet. Check again in a moment; do not send another.',
    );
  }
  const deadline = deadlineSecs(row);
  note = await save(d, note, {
    leave: leaveOf(d, 'exiting', { deadline, ...(txHash ? { txHash } : {}) }),
  });
  return { note, ...(txHash ? { txHash } : {}), conceded: row.conceded, deadline };
}

/** Reads a filed exit from the vault (a restored backup, or a request whose answer was lost). */
export async function refreshExit(d: LeaveDeps, note: NoteRecord): Promise<NoteRecord> {
  const noteId = need(note);
  const row = await exitRequest(d.rpc, d.profile.vaultAccount, noteId);
  if (row) return save(d, note, { leave: leaveOf(d, 'exiting', { deadline: deadlineSecs(row) }) });
  if (note.leave?.state === 'exiting' && (await isClosed(d, note))) {
    return save(d, note, { leave: leaveOf(d, 'exited', { via: note.leave.via }) });
  }
  return note;
}

export interface Finalized {
  note: NoteRecord;
  txHash?: string;
}

/** Sends `finalize_exit` once the chain's time is past the deadline. */
async function finalizeExitRun(
  d: LeaveDeps,
  start: NoteRecord,
  opts: { step?: Step } = {},
): Promise<Finalized> {
  const step = opts.step ?? (() => undefined);
  let note = start;
  const noteId = need(note);
  if (note.leave?.state !== 'exiting')
    throw new LeaveError('no_exit', 'This note has no exit filed.');
  const row = await exitRequest(d.rpc, d.profile.vaultAccount, noteId);
  if (!row) {
    // Gone from the list: finalized already (by this app or by anyone else).
    if (await isClosed(d, note)) {
      note = await save(d, note, { leave: leaveOf(d, 'exited') });
      return { note };
    }
    throw new LeaveError('no_exit', 'The vault lists no exit for this note.');
  }
  const deadline = deadlineSecs(row);
  const chainNow = await d.rpc.finalBlockTime();
  if (chainNow < deadline) {
    throw new LeaveError(
      'not_yet',
      `The exit window is open until ${new Date(deadline * 1000).toISOString()} (the chain’s clock). Come back after that.`,
    );
  }
  await needNear(d);
  step('Waiting for your wallet to sign finalize_exit…');
  let outcome: unknown;
  try {
    outcome = await d.wallet.send(
      d.profile.vaultAccount,
      call('finalize_exit', { note_id: noteId }),
    );
  } catch (e) {
    throw new LeaveError('wallet', `The wallet did not send finalize_exit: ${describe(e)}`);
  }
  const txHash = txHashOf(outcome) ?? undefined;
  const failure = failureOf(outcome);
  step('Checking that the exit is finalized…');
  const gone = await waitFor(
    d,
    async () => (await exitRequest(d.rpc, d.profile.vaultAccount, noteId)) === undefined,
  );
  if (!gone) {
    throw new LeaveError(
      failure ? 'refused' : 'not_listed',
      failure
        ? `The vault refused finalize_exit: ${failure}`
        : 'finalize_exit was sent, but the exit is still listed. Check again in a moment.',
    );
  }
  note = await save(d, note, {
    leave: leaveOf(d, 'exited', { ...(txHash ? { txHash } : {}) }),
  });
  return { note, ...(txHash ? { txHash } : {}) };
}

// ---- payout ----

export interface Claimed {
  amount: bigint;
  txHash?: string;
}

/** What waits for the payout account (reads `pending_payout`, which shows the account to the RPC provider). */
export async function pendingFor(d: LeaveDeps, note: NoteRecord): Promise<bigint> {
  return pendingPayout(d.rpc, d.profile.vaultAccount, note.payout);
}

/** Collects a pending payout of the note's payout account. */
async function claimPayoutRun(
  d: LeaveDeps,
  note: NoteRecord,
  opts: { step?: Step } = {},
): Promise<Claimed> {
  const step = opts.step ?? (() => undefined);
  step('Reading the pending payout (the RPC provider sees the payout account)…');
  const amount = await pendingFor(d, note);
  if (amount === 0n)
    throw new LeaveError('nothing_pending', `Nothing is pending for ${note.payout}.`);
  await needNear(d);
  step('Waiting for your wallet to sign claim_payout…');
  let outcome: unknown;
  try {
    outcome = await d.wallet.send(
      d.profile.vaultAccount,
      call('claim_payout', { account_id: note.payout }),
    );
  } catch (e) {
    throw new LeaveError('wallet', `The wallet did not send claim_payout: ${describe(e)}`);
  }
  const txHash = txHashOf(outcome) ?? undefined;
  const failure = failureOf(outcome);
  if (failure) throw new LeaveError('refused', `The vault refused claim_payout: ${failure}`);
  return { amount, ...(txHash ? { txHash } : {}) };
}

/** The tripwire's verdict, for the screen: leaving is allowed even when it is not `ok`. */
export async function vaultVerdict(d: LeaveDeps) {
  return checkVault(d.rpc, d.profile.vaultAccount, d.profile.pins);
}

// One close, exit, finalize or claim runs on a note at a time: a second click, or the expiry
// guard starting while the user already has one going, is refused instead of racing it.
const running = new Set<string>();
async function exclusive<T>(id: string, run: () => Promise<T>): Promise<T> {
  if (running.has(id))
    throw new LeaveError('busy', 'Another close or exit of this note is already running.');
  running.add(id);
  try {
    return await run();
  } finally {
    running.delete(id);
  }
}

export const closeNote = (d: LeaveDeps, note: NoteRecord, o: Parameters<typeof closeNoteRun>[2]) =>
  exclusive(note.id, () => closeNoteRun(d, note, o));
export const requestExit = (d: LeaveDeps, note: NoteRecord, o?: { step?: Step }) =>
  exclusive(note.id, () => requestExitRun(d, note, o));
export const finalizeExit = (d: LeaveDeps, note: NoteRecord, o?: { step?: Step }) =>
  exclusive(note.id, () => finalizeExitRun(d, note, o));
export const claimPayout = (d: LeaveDeps, note: NoteRecord, o?: { step?: Step }) =>
  exclusive(note.id, () => claimPayoutRun(d, note, o));
