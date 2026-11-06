// The anonymity set of a lease, counted in the browser from the vault's `note_records` at the
// final block the proof's tree was read at. The operator's counts are never used; its team list
// can only remove notes from the count. The rule is the one in `crates/nymirad/src/setsize.rs`
// (note IDs below the leaf count whose status is `Active` and whose expiry is at or after the
// request time), and `shared/set-size-cases.json` is run through both.
import type { RpcClient } from '../chain/rpc';
import { fill } from './strings';

/** The vault's page cap for `note_records`. */
const PAGE = 100;

interface Row {
  note_id: number;
  expiry: number;
  status: string;
}

/** The note IDs one provider lists as able to pay, read at `block`. */
export async function countable(
  rpc: RpcClient,
  vault: string,
  block: string,
  leafCount: number,
  requestTime: number,
): Promise<Set<number>> {
  const out = new Set<number>();
  for (let from = 0; from < leafCount;) {
    const limit = Math.min(PAGE, leafCount - from);
    const page = await rpc.view<Row[]>(vault, 'note_records', { from, limit }, block);
    if (!Array.isArray(page) || page.length === 0 || page.length > limit) {
      throw new Error('the vault returned a malformed note_records');
    }
    page.forEach((row, i) => {
      if (row.note_id !== from + i || typeof row.expiry !== 'number') {
        throw new Error('the vault returned a malformed note_records');
      }
      if (row.status === 'Active' && row.expiry >= requestTime) out.add(row.note_id);
    });
    from += page.length;
  }
  return out;
}

/** What the providers listed, before the team list is taken out. */
export interface Raw {
  /** One set per provider that answered; at least one. */
  sets: Set<number>[];
  /** Why there is no second set, if there is none. */
  oneProvider: string | null;
}

/**
 * Reads the sets at `block` from every provider in parallel. A provider that fails is kept as the
 * reason there is only one set; all failing throws.
 */
export async function read(
  providers: RpcClient[],
  vault: string,
  block: string,
  leafCount: number,
  requestTime: number,
): Promise<Raw> {
  const results = await Promise.allSettled(
    providers.map((p) => countable(p, vault, block, leafCount, requestTime)),
  );
  const sets: Set<number>[] = [];
  const failures: string[] = [];
  for (const r of results) {
    if (r.status === 'fulfilled') sets.push(r.value);
    else failures.push(r.reason instanceof Error ? r.reason.message : String(r.reason));
  }
  if (sets.length === 0) throw new Error(failures[0] ?? 'no RPC provider is configured');
  let oneProvider: string | null = null;
  if (sets.length === 1) {
    oneProvider =
      providers.length < 2 ? 'no second provider is configured' : (failures[0] ?? 'unknown');
  }
  return { sets, oneProvider };
}

/** The same rule at the current final block of the first provider. */
export async function readNow(
  providers: RpcClient[],
  vault: string,
  requestTime: number,
): Promise<Raw> {
  const block = await providers[0]!.finalBlockHash();
  const leafCount = await providers[0]!.view<number>(vault, 'leaf_count', {}, block);
  if (typeof leafCount !== 'number') throw new Error('the vault returned a malformed leaf_count');
  return read(providers, vault, block, leafCount, requestTime);
}

/** The figure kept with a lease record. Records written before it existed have none. */
export interface SetSize {
  /** Notes that could have paid, minus the team. The lower provider's figure. */
  external: number;
  /** The same count before the team list is taken out. */
  total: number;
  /** Team IDs only the pinned list has, and only the operator's list has. */
  pinnedOnly: number[];
  operatorOnly: number[];
  /** The operator's list as it was read, or `null` when it gave none. */
  operatorTeam: number[] | null;
  /** (external, total) of each provider, when the two differ. */
  providersDiffer: [[number, number], [number, number]] | null;
  /** Why only one provider was read, if so. */
  oneProvider: string | null;
}

/** What a lease record holds when the count could not be made. */
export interface SetSizeUnavailable {
  unavailable: string;
}

export const isFigure = (s: SetSize | SetSizeUnavailable | undefined): s is SetSize =>
  s !== undefined && 'external' in s;

/** The IDs removed from the external count: the pinned list plus the operator's. */
export const teamOf = (pinned: number[], operator: number[] | null): Set<number> =>
  new Set([...pinned, ...(operator ?? [])]);

const sorted = (ids: Iterable<number>) => [...ids].sort((a, b) => a - b);

/** Applies the team lists to what the providers listed. The operator's list can only lower N. */
export function evaluate(raw: Raw, pinned: number[], operator: number[] | null): SetSize {
  const pin = new Set(pinned);
  const op = operator === null ? null : new Set(operator);
  const team = teamOf(pinned, operator);
  const figures = raw.sets.map((s): [number, number] => [
    [...s].filter((id) => !team.has(id)).length,
    s.size,
  ]);
  const [external, total] = figures.reduce((lo, f) =>
    f[0] < lo[0] || (f[0] === lo[0] && f[1] < lo[1]) ? f : lo,
  );
  const [a, b] = raw.sets;
  const differ =
    a !== undefined && b !== undefined && (a.size !== b.size || [...a].some((id) => !b.has(id)));
  return {
    external,
    total,
    pinnedOnly: op ? sorted([...pin].filter((id) => !op.has(id))) : [],
    operatorOnly: op ? sorted([...op].filter((id) => !pin.has(id))) : [],
    operatorTeam: op ? sorted(op) : null,
    providersDiffer: differ ? [figures[0]!, figures[1]!] : null,
    oneProvider: raw.oneProvider,
  };
}

/** "set size: N (external), total T", the line `nymirad status` prints. */
export const headline = (s: SetSize): string =>
  `${fill('set_size', { n: s.external })}, ${fill('set_total', { n: s.total })}`;

/** What differs between the lists and the providers, one sentence each. */
export function differences(s: SetSize, announceMissingList = true): string[] {
  const ids = (v: number[]) => v.join(', ');
  const out: string[] = [];
  if (s.pinnedOnly.length > 0) out.push(fill('team_pinned_only', { ids: ids(s.pinnedOnly) }));
  if (s.operatorOnly.length > 0) {
    out.push(fill('team_operator_only', { ids: ids(s.operatorOnly) }));
  }
  if (s.operatorTeam === null && announceMissingList) out.push(fill('operator_list_missing', {}));
  if (s.providersDiffer) {
    const [a, b] = s.providersDiffer;
    out.push(fill('providers_differ', { a: a[0], b: b[0] }));
  }
  if (s.oneProvider) out.push(fill('one_provider', { why: s.oneProvider }));
  return out;
}
