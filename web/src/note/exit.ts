// Making the exit proofs of a note: the kind-2 ownership proof that asks the operator for a
// clearance, the kind-1 proof of a mutual close, and the kind-0 proof of `request_exit`. Like the
// lease code it runs in the note worker (a plain secret is in memory here) and, with a Node-loaded
// WASM, in the unit tests. The rules are the wallet core's `exit_lock.json` rules of the Rust
// client (zkapi `wallet.rs`), kept in the sealed payload:
//
//   - the lock is written before a proof is returned, and every proof is appended to `released`
//     before it leaves the worker, so a proof that left is always on record;
//   - while a lock exists the note makes no request proof (`prepareLease` refuses);
//   - a mutual lock may switch to escape; an escape lock goes back to mutual only while no escape
//     proof was released;
//   - an escape concedes the state's `bound_at_state`: 0 when no request proof was ever made at it,
//     the stored bound otherwise (the wallet core records the bound before a request leaves);
//   - one ownership proof is live at a time: a retry resends the kept one.
import type { Sealed } from '../crypto/aead';
import { open, seal, walletConfigJson, type ExitLock, type LeaseConfig } from './lease';
import { splitObject } from './rawjson';

/** `EXIT_PROOF_VALIDITY_SECONDS` of the wallet core: a close or exit proof lives 3 hours. */
export const EXIT_PROOF_SECS = 10_800;
/** The operator accepts an ownership proof that ends within 3,300 s (3,600 less 300 for clock skew). */
export const OWNERSHIP_PROOF_SECS = 3_300;
/** A kept ownership proof is resent only while it stays valid this long. */
export const OWNERSHIP_RESEND_MARGIN_SECS = 5;

export interface WithdrawalWasm {
  /** `browser_prepare_withdrawal` with the withdrawal proving key already loaded. */
  prepareWithdrawal(configJson: string, stateJson: string, argsJson: string): string;
}

export type WithdrawalKind = 'ownership' | 'mutual' | 'escape';

export interface WithdrawalInput {
  config: LeaseConfig;
  kind: WithdrawalKind;
  /**
   * Unix seconds. For an ownership proof the operator's signed status time (so a wrong local
   * clock cannot push the proof past the operator's 3,300 s window); for the other kinds the
   * chain's final block time (the time the vault judges `valid_until` by).
   */
  now: number;
  /** The payout account's 20-byte destination, `0x` and 40 hex digits (mutual and escape). */
  destination?: string;
  /** The operator's `ClearanceResponseV2` as JSON text (mutual). */
  clearance?: string;
}

export interface PreparedWithdrawal {
  /** The note's new sealed payload: the lock and the released proof are recorded in it. */
  sealed: Sealed;
  /** `WithdrawalPlan` as the wallet core wrote it: `{mode, public_inputs, proof}` (JSON text). */
  planJson: string;
  validUntil: number;
  /** An escape's concession as a decimal string; `0` for the other kinds. */
  conceded: string;
  /** An ownership proof from an earlier try, resent instead of making a second one. */
  reused: boolean;
}

export class ExitError extends Error {
  constructor(
    readonly kind: string,
    message: string,
  ) {
    super(message);
    this.name = 'ExitError';
  }
}

/** The lock for `mode`, or an `ExitError` when the note's history forbids it. */
export function lockFor(current: ExitLock | undefined, mode: ExitLock['mode'], now: number) {
  if (!current) return { mode, since: now, released: [] } satisfies ExitLock;
  if (current.mode === mode) return current;
  if (mode === 'mutual' && current.released.some((p) => p.kind === 0)) {
    throw new ExitError(
      'escape_released',
      'An exit request was already made for this note, so it can only leave by that exit.',
    );
  }
  return { ...current, mode };
}

export async function prepareWithdrawal(
  wasm: WithdrawalWasm,
  key: CryptoKey,
  aad: string,
  sealed: Sealed,
  input: WithdrawalInput,
): Promise<PreparedWithdrawal> {
  const { state, extra } = await open(key, aad, sealed);
  if (extra.active?.token !== undefined)
    throw new ExitError('lease_open', 'A lease is open on this note. End the conversation first.');
  if (extra.journal !== undefined && input.kind !== 'escape') {
    throw new ExitError(
      'lease_pending',
      'An earlier lease on this note is not settled yet. Settle it first.',
    );
  }
  const fields = splitObject(state);
  const bound = fields.bound_at_state ?? '0';
  if (!/^\d+$/.test(bound)) throw new ExitError('bad_state', 'The note state has no valid bound.');
  const mode = input.kind === 'escape' ? 'escape' : 'mutual';
  const lock = lockFor(extra.exit, mode, input.now);
  const config = walletConfigJson(input.config, '0');
  const store = async (released: ExitLock['released']) =>
    seal(key, aad, state, { ...extra, exit: { ...lock, released } });

  if (input.kind === 'ownership') {
    const live = lock.released
      .filter((p) => p.kind === 2 && p.ownership !== undefined)
      .sort((a, b) => b.validUntil - a.validUntil)[0];
    if (live && live.validUntil > input.now + OWNERSHIP_RESEND_MARGIN_SECS) {
      return {
        sealed: await store(lock.released),
        planJson: live.ownership!,
        validUntil: live.validUntil,
        conceded: '0',
        reused: true,
      };
    }
    if (live && live.validUntil >= input.now) {
      throw new ExitError(
        'ownership_still_valid',
        'An ownership proof of this note is still valid for a few seconds. Try again in a moment.',
      );
    }
  }

  const validUntil =
    input.now + (input.kind === 'ownership' ? OWNERSHIP_PROOF_SECS : EXIT_PROOF_SECS);
  const parts = [`"mode":${JSON.stringify(input.kind)}`, `"valid_until":${validUntil}`];
  if (input.kind !== 'ownership') {
    if (!input.destination || !/^0x[0-9a-f]{40}$/.test(input.destination))
      throw new ExitError('no_destination', 'The payout account has no valid destination.');
    parts.push(`"destination":${JSON.stringify(input.destination)}`);
  }
  if (input.kind === 'escape') parts.push(`"conceded":${bound}`);
  if (input.kind === 'mutual') {
    if (!input.clearance) throw new ExitError('no_clearance', 'A mutual close needs a clearance.');
    parts.push(`"clearance":${input.clearance}`);
  }
  const planJson = wasm.prepareWithdrawal(config, state, `{${parts.join(',')}}`);
  const released = [
    ...lock.released,
    {
      kind: ({ escape: 0, mutual: 1, ownership: 2 } as const)[input.kind],
      validUntil,
      conceded: input.kind === 'escape' ? bound : '0',
      ...(input.kind === 'ownership' ? { ownership: planJson } : {}),
    },
  ];
  return {
    sealed: await store(released),
    planJson,
    validUntil,
    conceded: input.kind === 'escape' ? bound : '0',
    reused: false,
  };
}

/** What the page needs to send a plan: the proof text and the statement as JSON text. */
export function planParts(planJson: string): {
  proof: string;
  publicInputs: string;
  nullifier: string;
} {
  const plan = splitObject(planJson);
  const proofObject = splitObject(plan.proof!);
  const publicInputs = plan.public_inputs!;
  return {
    proof: JSON.parse(proofObject.proof!) as string,
    publicInputs,
    nullifier: splitObject(publicInputs).withdrawal_nullifier!,
  };
}

/** `{"withdrawal_nullifier":..,"ownership_proof":{"public_inputs":..,"proof":..}}`: the clearance request. */
export function clearanceBody(planJson: string): string {
  const plan = splitObject(planJson);
  const { nullifier, publicInputs } = planParts(planJson);
  return `{"withdrawal_nullifier":${nullifier},"ownership_proof":{"public_inputs":${publicInputs},"proof":${plan.proof!}}}`;
}

const b64 = (hex: string): string => {
  const bytes = new Uint8Array(hex.length / 2);
  for (let i = 0; i < bytes.length; i++) bytes[i] = parseInt(hex.slice(i * 2, i * 2 + 2), 16);
  let s = '';
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
};

/**
 * The call arguments `mutual_close`, `request_exit` and `POST /v1/close` take: the wire proof and
 * the 16 statement words, each as base64 of 32 big-endian bytes. `wordsHex` is the JSON array that
 * `withdrawal_words` (web-checks) returns.
 */
export function callArgs(planJson: string, wordsHex: string): { proof: string; inputs: string[] } {
  const words = JSON.parse(wordsHex) as string[];
  if (words.length !== 16 || words.some((w) => !/^[0-9a-f]{64}$/.test(w)))
    throw new ExitError('bad_words', 'The statement words are malformed.');
  return { proof: planParts(planJson).proof, inputs: words.map(b64) };
}
