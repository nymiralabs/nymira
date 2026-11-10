// The live checks behind the Status screen and the documentation's "check live" buttons. Every
// check is one read of something the app already reads: the operator's public status, the vault
// compared with the pins in this bundle, each RPC endpoint on its own, and this bundle's manifest.
// Nothing runs until the reader asks, because each read shows the reader's address to that host.
import { checkVault, type VaultCheck } from '../chain/vault';
import { RpcClient } from '../chain/rpc';
import { OperatorClient } from '../chat/operator';
import type { Profile } from '../profiles/types';

export type Level = 'ok' | 'warn' | 'fail';

export interface Check {
  id: 'operator' | 'vault' | 'rpc' | 'release';
  title: string;
  level: Level;
  /** One sentence a reader can act on. */
  summary: string;
  /** Label and value pairs for the details. */
  facts: [string, string][];
  /** How long the reads took, in milliseconds. */
  ms?: number;
}

export interface Report {
  at: number;
  checks: Check[];
  overall: Level;
  headline: string;
}

/** The operator's heartbeat is due every 30 minutes; a gap beyond an hour is a warning. */
export const HEARTBEAT_WARN_SECS = 3600;

export function age(secs: number): string {
  if (!Number.isFinite(secs) || secs < 0) return 'unknown';
  if (secs < 90) return `${Math.round(secs)} s`;
  if (secs < 5400) return `${Math.round(secs / 60)} min`;
  if (secs < 129600) return `${(secs / 3600).toFixed(1)} h`;
  return `${Math.round(secs / 86400)} d`;
}

const usd = (micro: bigint | string | number, places = 2) =>
  `${(Number(micro) / 1e6).toFixed(places)} USDC`;
const short = (hex: string) => (hex.length > 20 ? `${hex.slice(0, 10)}…${hex.slice(-8)}` : hex);

type Json = Record<string, unknown>;

/** Grades the operator's status object. Pure, so a test can feed it any shape. */
export function gradeOperator(status: Json, nowSecs: number): Omit<Check, 'ms'> {
  const heartbeat = Number(status.heartbeat_age_secs);
  const watchers = Array.isArray(status.watchers) ? (status.watchers as Json[]) : [];
  const budget = (status.daily_budget ?? {}) as Json;
  const admission = (status.admission ?? {}) as Json;
  const notice = typeof status.emergency_notice === 'string' ? status.emergency_notice : null;
  const problems: string[] = [];
  if (status.degraded === true) problems.push('the operator reports a degraded state');
  if (status.recovery_mode === true) problems.push('it is in recovery mode');
  if (status.replica !== undefined && status.replica !== 'ok') {
    problems.push(`its second machine reports "${String(status.replica)}"`);
  }
  if (Number.isFinite(heartbeat) && heartbeat > HEARTBEAT_WARN_SECS) {
    problems.push(`the last heartbeat is ${age(heartbeat)} old`);
  }
  if (notice) problems.push(`it posted a notice: ${notice}`);
  const facts: [string, string][] = [
    ['Last heartbeat', Number.isFinite(heartbeat) ? `${age(heartbeat)} ago` : 'unknown'],
    ['Open notes', String(status.open_notes ?? 'unknown')],
    ['Notes from outside the team', String(status.external_notes ?? 'unknown')],
    [
      'Watchers',
      watchers.length
        ? watchers
            .map((w) => `${String(w.id)} polled ${age(nowSecs - Number(w.last_poll_ts))} ago`)
            .join(', ')
        : 'none reported',
    ],
    ['Load', admission.load_pct === undefined ? 'unknown' : `${String(admission.load_pct)}%`],
    [
      'Provider budget today',
      budget.limit === undefined
        ? 'unknown'
        : `${usd(String(budget.used ?? 0), 3)} of ${usd(String(budget.limit), 3)}`,
    ],
  ];
  return {
    id: 'operator',
    title: 'Operator',
    level: problems.length ? 'warn' : 'ok',
    summary: problems.length
      ? `Reachable, but ${problems.join('; ')}.`
      : 'Reachable and healthy. It signed this answer with a fresh nonce.',
    facts,
  };
}

export function gradeVault(check: VaultCheck, account: string): Omit<Check, 'ms'> {
  const base = { id: 'vault' as const, title: 'Vault' };
  switch (check.status) {
    case 'ok':
      return {
        ...base,
        level: 'ok',
        summary:
          'The code hash, the verifying keys, the config and the access keys on chain equal what this app pins.',
        facts: [
          ['Account', account],
          ['Open deposits', usd(check.limits.currentTvl)],
          ['Cap on open deposits', usd(check.limits.tvlCap)],
        ],
      };
    case 'mismatch':
      return {
        ...base,
        level: 'fail',
        summary: `The chain differs from what this app pins: ${check.diffs.join(', ')}. Funding and chat stay off until a new release.`,
        facts: [['Account', account], ...check.diffs.map((d): [string, string] => ['Differs', d])],
      };
    case 'unpinned':
      return {
        ...base,
        level: 'warn',
        summary: 'This build pins no vault, so there is nothing to compare with.',
        facts: [['Account', account]],
      };
    default:
      return {
        ...base,
        level: 'warn',
        summary: 'The vault could not be read, so it could not be compared.',
        facts: [
          ['Account', account],
          ['Error', check.detail],
        ],
      };
  }
}

export interface EndpointRead {
  url: string;
  ok: boolean;
  ms: number;
  height?: number;
  blockAgeSecs?: number;
  error?: string;
}

export function gradeRpc(reads: EndpointRead[]): Omit<Check, 'ms'> {
  const up = reads.filter((r) => r.ok);
  const stale = up.filter((r) => (r.blockAgeSecs ?? 0) > 120);
  const level: Level =
    up.length === 0 ? 'fail' : up.length < reads.length || stale.length ? 'warn' : 'ok';
  return {
    id: 'rpc',
    title: 'NEAR RPC',
    level,
    summary:
      level === 'fail'
        ? 'No RPC endpoint answered. The app cannot read the chain.'
        : level === 'warn'
          ? `${up.length} of ${reads.length} endpoints answered${stale.length ? ', and a block is more than two minutes old' : ''}.`
          : `All ${reads.length} endpoints answered with a recent block.`,
    facts: reads.map((r): [string, string] => [
      new URL(r.url).host,
      r.ok
        ? `block ${r.height} · ${age(r.blockAgeSecs ?? 0)} old · ${r.ms} ms`
        : `no answer (${r.error ?? 'failed'})`,
    ]),
  };
}

export function gradeRelease(
  manifest: { version?: unknown; bundle_hash?: unknown } | null,
): Omit<Check, 'ms'> {
  if (!manifest || typeof manifest.bundle_hash !== 'string') {
    return {
      id: 'release',
      title: 'This release',
      level: 'warn',
      summary: 'No manifest could be read from this host, so the bundle hash cannot be shown.',
      facts: [],
    };
  }
  return {
    id: 'release',
    title: 'This release',
    level: 'ok',
    summary:
      'The host serves a manifest. It reports this hash; it does not prove it. Check it with `nymirad verify-web`.',
    facts: [
      ['Version', String(manifest.version ?? 'unknown')],
      ['Bundle hash', short(manifest.bundle_hash)],
    ],
  };
}

export function overallOf(checks: Pick<Check, 'level'>[]): { overall: Level; headline: string } {
  if (checks.some((c) => c.level === 'fail')) {
    return { overall: 'fail', headline: 'Something is wrong. Read the red line before you fund.' };
  }
  if (checks.some((c) => c.level === 'warn')) {
    return { overall: 'warn', headline: 'Working, with something to know.' };
  }
  return { overall: 'ok', headline: 'Everything checked is as pinned.' };
}

const timed = async <T>(f: () => Promise<T>): Promise<[T, number]> => {
  const t = performance.now();
  const value = await f();
  return [value, Math.round(performance.now() - t)];
};

async function readEndpoint(url: string): Promise<EndpointRead> {
  const t = performance.now();
  try {
    const r = await new RpcClient([url], { timeoutMs: 6000 }).call<{
      sync_info: { latest_block_height: number; latest_block_time: string };
    }>('status', []);
    return {
      url,
      ok: true,
      ms: Math.round(performance.now() - t),
      height: r.sync_info.latest_block_height,
      blockAgeSecs: Math.max(0, (Date.now() - Date.parse(r.sync_info.latest_block_time)) / 1000),
    };
  } catch (e) {
    return {
      url,
      ok: false,
      ms: Math.round(performance.now() - t),
      error: e instanceof Error ? e.message.slice(0, 80) : 'failed',
    };
  }
}

/** Runs every check. Each one fails on its own; none throws. */
export async function runChecks(profile: Profile, vaultRpc: RpcClient): Promise<Report> {
  const operator = new OperatorClient(profile.operatorUrl);
  const [opResult, vaultResult, rpcResult, relResult] = await Promise.all([
    timed(async (): Promise<Omit<Check, 'ms'>> => {
      try {
        return gradeOperator(await operator.status(), Date.now() / 1000);
      } catch (e) {
        return {
          id: 'operator',
          title: 'Operator',
          level: 'fail',
          summary:
            "The operator did not answer. It runs on a home machine and can be offline. From an address other than the project's own, the browser may also refuse the answer.",
          facts: [['Error', e instanceof Error ? e.message.slice(0, 100) : 'failed']],
        };
      }
    }),
    timed(async () =>
      gradeVault(
        await checkVault(vaultRpc, profile.vaultAccount, profile.pins),
        profile.vaultAccount,
      ),
    ),
    timed(async () => gradeRpc(await Promise.all(profile.rpcUrls.map(readEndpoint)))),
    timed(async () => {
      try {
        const r = await fetch('/manifest.json', { cache: 'no-store', credentials: 'omit' });
        return gradeRelease(r.ok ? ((await r.json()) as Json) : null);
      } catch {
        return gradeRelease(null);
      }
    }),
  ]);
  const checks: Check[] = [opResult, vaultResult, rpcResult, relResult].map(([c, ms]) => ({
    ...c,
    ms,
  }));
  return { at: Date.now(), checks, ...overallOf(checks) };
}

/** A plain-text copy of a report, for a bug report. It holds no secret. */
export function reportText(report: Report, profile: Profile): string {
  const lines = [
    `Nymira status, ${new Date(report.at).toISOString()}`,
    `Network: ${profile.network}`,
    `Overall: ${report.overall}. ${report.headline}`,
    '',
  ];
  for (const c of report.checks) {
    lines.push(`[${c.level}] ${c.title}: ${c.summary}`);
    for (const [k, v] of c.facts) lines.push(`    ${k}: ${v}`);
  }
  return lines.join('\n');
}

/** Only the vault comparison, for the documentation's "compare with the chain" button. */
export async function checkVaultOnly(profile: Profile, vaultRpc: RpcClient): Promise<Check> {
  const [c, ms] = await timed(async () =>
    gradeVault(
      await checkVault(vaultRpc, profile.vaultAccount, profile.pins),
      profile.vaultAccount,
    ),
  );
  return { ...c, ms };
}
