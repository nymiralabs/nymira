// Everything that touches a note's wallet state during a chat: making the lease request proof,
// installing the operator's signed successor, and keeping the lease records. It runs in the note
// worker (src/worker) and, with a Node-loaded WASM, in the unit tests. A plain secret is in memory
// here and never leaves: callers get sealed payloads, the request body and public figures.
//
// A sealed payload of an accepted note is `{state, extra}`. `state` is zkapi's BrowserNoteState as
// JSON text and `extra` is JSON text of `NoteExtra`: the pending request's journal, the lease in
// progress with its token, and the records of finished leases. Both stay text because the wallet
// core writes the u64 chain id as bare digits (see rawjson.ts).
import { openJson, sealJson, type Sealed } from '../crypto/aead';
import { splitObject } from './rawjson';

/** What a wallet core call needs from the vault (`config()` plus the mapped contract address). */
export interface LeaseConfig {
  chainId: string;
  contractAddress: string;
  operatorPk: { x: string; y: string };
  clearancePk: { x: string; y: string };
}

/** The zkapi-browser surface the lease path uses. */
export interface LeaseWasm {
  /** `BrowserRequestProver.prepare_request` of a prover loaded from the checked key. */
  prepareRequest(configJson: string, stateJson: string, argsJson: string): string;
  /** `BrowserNoteTree.from_state(snapshot).path(noteId)`: a `BrowserTreePath` as JSON text. */
  treePath(snapshotJson: string, noteId: number): string;
  completeResponse(configJson: string, argsJson: string): string;
}

export interface TreeSnapshot {
  /** The vault's `currentRoot()` at the snapshot block, 0x plus 64 hex digits. */
  root: string;
  nextNoteId: number;
  /** `get_leaves` from 0, 0x plus 64 hex digits each. */
  leaves: string[];
  /** The final block the views were read at (hash). */
  block: string;
}

export interface SamplingSettings {
  temperature?: number;
  top_p?: number;
  stream: boolean;
}

/** Metadata of one lease, never message content. */
export interface LeaseRecord {
  leaseId: string;
  /** The root the proof used and the number of leaves under it. */
  root: string;
  leafCount: number;
  snapshotBlock: string;
  /** Bound class in micro-USDC, as a decimal string. */
  boundClass: string;
  openedAt: number;
  closedAt?: number;
  /** Micro-USDC. `charge` is what the operator applied, `expectedCharge` what the rules give. */
  charge?: string;
  expectedCharge?: string;
  model: string;
  sampling: SamplingSettings;
  requests: number;
  messageCount: number;
  messageBytes: number;
  promptTokens: number;
  completionTokens: number;
  /** `model_signed`, `gateway_signed` or `not_verified` (with the reason in `warnings`). */
  trust?: string;
  warnings: string[];
}

/** The lease in progress. `token` is the lease's credential (sent as `X-Nymira-Lease-Token`). */
export interface ActiveLease {
  record: LeaseRecord;
  token?: string;
  clientRequestId: string;
  issuedAt?: number;
  expiresAt?: number;
}

export interface NoteExtra {
  /** The journal of a prepared request that has no installed response yet (JSON text). */
  journal?: string;
  active?: ActiveLease;
  leases: LeaseRecord[];
}

interface Payload {
  state: string;
  extra?: string;
}

export const emptyExtra = (): NoteExtra => ({ leases: [] });

export const parseExtra = (text: string | undefined): NoteExtra =>
  text ? (JSON.parse(text) as NoteExtra) : emptyExtra();

/** The wallet core's config JSON; `bound` is the one solvency bound of every proof at a state. */
export function walletConfigJson(c: LeaseConfig, bound: string): string {
  return (
    `{"protocol_version":2,"chain_id":${c.chainId},"contract_address":${JSON.stringify(c.contractAddress)},` +
    `"request_charge_cap":${bound},"policy_charge_cap":${bound},"policy_enabled":false,` +
    `"state_signing_key":${JSON.stringify(c.operatorPk)},"clearance_signing_key":${JSON.stringify(c.clearancePk)}}`
  );
}

async function open(key: CryptoKey, aad: string, sealed: Sealed) {
  const payload = await openJson<Payload>(key, sealed, aad);
  if (typeof payload.state !== 'string') throw new Error('this note has no wallet state yet');
  return { state: payload.state, extra: parseExtra(payload.extra) };
}

const seal = (key: CryptoKey, aad: string, state: string, extra: NoteExtra) =>
  sealJson(key, { state, extra: JSON.stringify(extra) } satisfies Payload, aad);

export interface PrepareInput {
  config: LeaseConfig;
  noteId: number;
  bound: string;
  tree: TreeSnapshot;
  requestTime: number;
  clientRequestId: string;
  createdAtMs: number;
  openedAt: number;
}

export interface PreparedLease {
  /** The note's new sealed payload: the state now records the bound (C2) and the journal is kept. */
  sealed: Sealed;
  /** The request body for `POST /v2/openrouter/leases`, as the wallet core wrote it. */
  requestJson: string;
}

/** The payload every lease request carries (`OpenRouterLeaseAuthorization::default()`). */
export const LEASE_PAYLOAD = '{"mode":"openrouter_ephemeral_lease","version":1}';

/**
 * Proves a lease request at `bound`. The returned sealed payload must be stored before the request
 * leaves the page: it records the bound (so a later exit concedes the right amount) and the
 * journal that the response is completed against.
 */
export async function prepareLease(
  wasm: LeaseWasm,
  key: CryptoKey,
  aad: string,
  sealed: Sealed,
  input: PrepareInput,
): Promise<PreparedLease> {
  const { state, extra } = await open(key, aad, sealed);
  if (extra.active?.token !== undefined) throw new Error('a lease is already open on this note');
  const snapshot = JSON.stringify({
    root: input.tree.root,
    next_note_id: input.tree.nextNoteId,
    leaves: input.tree.leaves,
  });
  const path = JSON.parse(wasm.treePath(snapshot, input.noteId)) as {
    active_root: string;
    siblings: string[];
  };
  const args = JSON.stringify({
    payload: LEASE_PAYLOAD,
    active_root: path.active_root,
    merkle_siblings: path.siblings,
    client_request_id: input.clientRequestId,
    request_time: input.requestTime,
    created_at_ms: input.createdAtMs,
  });
  const prepared = splitObject(
    wasm.prepareRequest(walletConfigJson(input.config, input.bound), state, args),
  );
  const { request, journal, state: next } = prepared;
  if (!request || !journal || !next) throw new Error('the wallet core returned an unexpected shape');
  const record: LeaseRecord = {
    leaseId: '',
    root: path.active_root,
    leafCount: input.tree.nextNoteId,
    snapshotBlock: input.tree.block,
    boundClass: input.bound,
    openedAt: input.openedAt,
    model: '',
    sampling: { stream: false },
    requests: 0,
    messageCount: 0,
    messageBytes: 0,
    promptTokens: 0,
    completionTokens: 0,
    warnings: [],
  };
  const active: ActiveLease = { record, clientRequestId: input.clientRequestId };
  return {
    sealed: await seal(key, aad, next, { ...extra, journal, active }),
    requestJson: request,
  };
}

/** Records the operator's answer to the lease request: the lease id and token. */
export async function leaseOpened(
  key: CryptoKey,
  aad: string,
  sealed: Sealed,
  lease: { leaseId: string; token: string; issuedAt: number; expiresAt: number },
): Promise<Sealed> {
  const { state, extra } = await open(key, aad, sealed);
  if (!extra.active) throw new Error('no lease request is pending on this note');
  extra.active = {
    ...extra.active,
    token: lease.token,
    issuedAt: lease.issuedAt,
    expiresAt: lease.expiresAt,
    record: { ...extra.active.record, leaseId: lease.leaseId },
  };
  return seal(key, aad, state, extra);
}

export type ExchangeUse = Pick<
  LeaseRecord,
  'model' | 'sampling' | 'messageCount' | 'messageBytes' | 'promptTokens' | 'completionTokens'
>;

/** Adds one exchange's metadata (never its content) to the lease in progress. */
export async function recordExchange(
  key: CryptoKey,
  aad: string,
  sealed: Sealed,
  use: ExchangeUse,
): Promise<Sealed> {
  const { state, extra } = await open(key, aad, sealed);
  const a = extra.active;
  if (!a) throw new Error('no lease is open on this note');
  const r = a.record;
  a.record = {
    ...r,
    model: use.model,
    sampling: use.sampling,
    requests: r.requests + 1,
    messageCount: r.messageCount + use.messageCount,
    messageBytes: r.messageBytes + use.messageBytes,
    promptTokens: r.promptTokens + use.promptTokens,
    completionTokens: r.completionTokens + use.completionTokens,
  };
  return seal(key, aad, state, extra);
}

export interface Settlement {
  /** `RequestResponseV2` as the operator sent it (JSON text). */
  responseJson: string;
  closedAt: number;
  expectedCharge: string;
  trust: string;
  warnings: string[];
}

export interface Completed {
  sealed: Sealed;
  charge: string;
  record: LeaseRecord;
}

/**
 * Installs the operator's signed successor state after checks S1 to S6 of the wallet core, and
 * moves the lease from "in progress" to the finished records.
 */
export async function completeLease(
  wasm: LeaseWasm,
  key: CryptoKey,
  aad: string,
  sealed: Sealed,
  config: LeaseConfig,
  s: Settlement,
): Promise<Completed> {
  const { state, extra } = await open(key, aad, sealed);
  if (!extra.journal || !extra.active) throw new Error('no lease request is pending on this note');
  const bound = extra.active.record.boundClass;
  const next = wasm.completeResponse(
    walletConfigJson(config, bound),
    `{"state":${state},"journal":${extra.journal},"response":${s.responseJson}}`,
  );
  const charge = String(
    (JSON.parse(s.responseJson) as { charge_applied: number | string }).charge_applied,
  );
  const record: LeaseRecord = {
    ...extra.active.record,
    closedAt: s.closedAt,
    charge,
    expectedCharge: s.expectedCharge,
    trust: s.trust,
    warnings: s.warnings,
  };
  const out: NoteExtra = { leases: [...extra.leases, record] };
  return { sealed: await seal(key, aad, next, out), charge, record };
}

/** Sets a refused request aside (`root_stale`: the operator refused it before binding the nullifier). */
export async function discardPending(
  key: CryptoKey,
  aad: string,
  sealed: Sealed,
): Promise<Sealed> {
  const { state, extra } = await open(key, aad, sealed);
  const { journal: _journal, active: _active, ...rest } = extra;
  void _journal;
  void _active;
  return seal(key, aad, state, { ...rest, leases: extra.leases });
}

/** What the page may know about a note: public figures from the state and the lease records. */
export interface NoteView {
  balance: string;
  noteId: number;
  expiry: number;
  boundAtState: string;
  pending: boolean;
  active?: ActiveLease;
  leases: LeaseRecord[];
}

export async function readNote(key: CryptoKey, aad: string, sealed: Sealed): Promise<NoteView> {
  const { state, extra } = await open(key, aad, sealed);
  const s = splitObject(state);
  return {
    balance: s.current_balance ?? '0',
    noteId: Number(s.note_id),
    expiry: Number(s.expiry_ts),
    boundAtState: s.bound_at_state ?? '0',
    pending: extra.journal !== undefined,
    active: extra.active,
    leases: extra.leases,
  };
}
