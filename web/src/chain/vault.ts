// The vault tripwire. The bundle carries the vault's code hash, `vk_hashes()`, every `config()`
// value except the TVL cap and the current TVL, and the access-key list. The app compares them
// with the chain at load and before every deposit; any difference stops funding (and chat, in the
// later ticket) and shows what differs.
import type { VaultConfigPin, VaultPins } from '../profiles/types';
import type { RpcClient } from './rpc';

export interface VaultSnapshot {
  codeHash: string;
  vkHashes: unknown;
  config: Record<string, unknown>;
  accessKeys: { public_key: string; permission: unknown }[];
}

/** The live `config()` fields the pins do not cover. */
export interface VaultLimits {
  tvlCap: bigint;
  currentTvl: bigint;
}

export async function readVault(
  rpc: RpcClient,
  vault: string,
): Promise<{ snapshot: VaultSnapshot; limits: VaultLimits }> {
  const [account, vkHashes, config, keys] = await Promise.all([
    rpc.viewAccount(vault),
    rpc.view(vault, 'vk_hashes'),
    rpc.view<Record<string, unknown>>(vault, 'config'),
    rpc.accessKeyList(vault),
  ]);
  const { tvl_cap, current_tvl, ...pinned } = config;
  return {
    snapshot: {
      codeHash: account.code_hash,
      vkHashes,
      config: pinned,
      accessKeys: keys.keys.map((k) => ({
        public_key: k.public_key,
        permission: k.access_key.permission,
      })),
    },
    limits: { tvlCap: BigInt(String(tvl_cap ?? 0)), currentTvl: BigInt(String(current_tvl ?? 0)) },
  };
}

/** Stable JSON: object keys sorted, numbers and numeric strings both compared as strings. */
export function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  if (value !== null && typeof value === 'object') {
    const entries = Object.entries(value as Record<string, unknown>).sort(([a], [b]) =>
      a < b ? -1 : a > b ? 1 : 0,
    );
    return `{${entries.map(([k, v]) => `${JSON.stringify(k)}:${canonical(v)}`).join(',')}}`;
  }
  return JSON.stringify(String(value));
}

/** The differences between what the bundle pins and what the chain says. Empty means equal. */
export function diffVault(pins: VaultPins, snapshot: VaultSnapshot): string[] {
  const diffs: string[] = [];
  if (snapshot.codeHash !== pins.codeHash) diffs.push('the vault code hash');
  if (canonical(snapshot.vkHashes) !== canonical(pins.vkHashes)) {
    diffs.push('the verifying key hashes');
  }
  const configPin: VaultConfigPin = pins.config;
  const pinned = configPin as unknown as Record<string, unknown>;
  const live = snapshot.config;
  const names = new Set([...Object.keys(pinned), ...Object.keys(live)]);
  for (const name of [...names].sort()) {
    if (canonical(pinned[name]) !== canonical(live[name])) diffs.push(`config: ${name}`);
  }
  const sortKeys = (keys: { public_key: string; permission: unknown }[]) =>
    canonical([...keys].sort((a, b) => (a.public_key < b.public_key ? -1 : 1)));
  if (sortKeys(snapshot.accessKeys) !== sortKeys(pins.accessKeys)) diffs.push('the access keys');
  return diffs;
}

export type VaultCheck =
  | { status: 'ok'; limits: VaultLimits; checkedAt: number }
  | { status: 'unpinned' }
  | { status: 'mismatch'; diffs: string[] }
  | { status: 'unreachable'; detail: string };

export async function checkVault(
  rpc: RpcClient,
  vault: string,
  pins: VaultPins | null,
  now: () => number = Date.now,
): Promise<VaultCheck> {
  if (!pins) return { status: 'unpinned' };
  try {
    const { snapshot, limits } = await readVault(rpc, vault);
    const diffs = diffVault(pins, snapshot);
    return diffs.length === 0
      ? { status: 'ok', limits, checkedAt: now() }
      : { status: 'mismatch', diffs };
  } catch (error) {
    return {
      status: 'unreachable',
      detail: error instanceof Error ? error.message : String(error),
    };
  }
}
