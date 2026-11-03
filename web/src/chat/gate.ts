// What must hold before a proof leaves the device (`nymirad/src/gate.rs`): the operator's
// `/v1/status` is signed by the vault's state key, its lease parameters equal the pinned ones, and
// the time and puzzle fields come from it. Fields of the status this build does not know are
// ignored. The signature check is injected: in the app it is the zkapi Schnorr verifier in WASM.
import type { PublicKeyPin, VaultConfigPin } from '../profiles/types';

/** A status nonce older than this is not used (the operator's slots are 5 s, its puzzle window 30). */
export const MAX_STATUS_AGE_SECS = 30;
/** `request_time` is the operator's signed time rounded down to this grid. */
export const TIME_GRID_SECS = 60;

export interface StatusNonce {
  nonce: string;
  host: string;
  ts: number;
}

export type Refusal =
  | { kind: 'no_nonce' }
  | { kind: 'bad_signature' }
  | { kind: 'stale'; ageSecs: number }
  | { kind: 'malformed'; field: string }
  | { kind: 'params'; fields: string[] };

export function describeRefusal(r: Refusal): string {
  switch (r.kind) {
    case 'no_nonce':
      return 'The operator sent no signed nonce (its signer is unreachable).';
    case 'bad_signature':
      return 'The operator’s nonce signature does not verify against the vault’s state key. Nothing was sent.';
    case 'stale':
      return `The operator’s nonce is ${r.ageSecs} s away from this device’s clock. Check that the clock is right.`;
    case 'malformed':
      return `The operator’s status is malformed (${r.field}).`;
    case 'params':
      return `The operator’s lease parameters differ from the vault’s (${r.fields.join(', ')}). It is refused.`;
  }
}

export class GateError extends Error {
  constructor(readonly refusal: Refusal) {
    super(describeRefusal(refusal));
    this.name = 'GateError';
  }
}

export type SignatureCheck = (
  pk: PublicKeyPin,
  nonce: StatusNonce,
  signatureJson: string,
) => boolean;

type Json = Record<string, unknown>;

export function verifyStatus(
  status: Json,
  operatorPk: PublicKeyPin,
  nowSecs: number,
  check: SignatureCheck,
): StatusNonce {
  const slot = status.nonce as Json | null | undefined;
  if (!slot || !status.nonce_sig) throw new GateError({ kind: 'no_nonce' });
  const { nonce, host, ts } = slot;
  if (typeof nonce !== 'string') throw new GateError({ kind: 'malformed', field: 'nonce' });
  if (typeof host !== 'string') throw new GateError({ kind: 'malformed', field: 'host' });
  if (typeof ts !== 'number' || !Number.isSafeInteger(ts)) {
    throw new GateError({ kind: 'malformed', field: 'ts' });
  }
  const slotValue: StatusNonce = { nonce, host, ts };
  let valid = false;
  try {
    valid = check(operatorPk, slotValue, JSON.stringify(status.nonce_sig));
  } catch {
    valid = false;
  }
  if (!valid) throw new GateError({ kind: 'bad_signature' });
  const age = Math.max(0, nowSecs - ts);
  if (age > MAX_STATUS_AGE_SECS || ts > nowSecs + MAX_STATUS_AGE_SECS) {
    throw new GateError({ kind: 'stale', ageSecs: age });
  }
  return slotValue;
}

const PARAM_FIELDS = ['bound_classes', 'charge_step', 'min_charge', 'slot_rate'] as const;

const spell = (v: unknown): string[] =>
  (Array.isArray(v) ? v : [v]).map((x) => String(x).replaceAll('"', ''));

/** The lease parameters in `status` against the pinned `config()` fields. */
export function checkParams(status: Json, pinned: VaultConfigPin): void {
  const pinnedRecord = pinned as unknown as Json;
  const differing = PARAM_FIELDS.filter((f) => {
    const live = status[f];
    if (live === null || live === undefined) return true;
    const a = spell(live);
    const b = spell(pinnedRecord[f]);
    return a.length !== b.length || a.some((x, i) => x !== b[i]);
  });
  if (differing.length > 0) throw new GateError({ kind: 'params', fields: [...differing] });
}

/** `request_time` for a proof: the operator's slot time rounded down to the grid. */
export const requestTime = (n: StatusNonce): number =>
  Math.floor(n.ts / TIME_GRID_SECS) * TIME_GRID_SECS;

/** The puzzle difficulty the operator asks for, 0 when none. */
export function puzzleDifficulty(status: Json): number {
  const admission = status.admission as Json | undefined;
  const d = admission?.puzzle_difficulty;
  return typeof d === 'number' && d > 0 ? Math.min(d, 32) : 0;
}
