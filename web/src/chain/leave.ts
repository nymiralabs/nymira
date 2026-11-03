// What the close and exit screens read from the vault. Every read goes to a public RPC provider,
// which sees the IP address and the call:
//   exit_requests   a page of the vault's pending exits from note 0 (nothing names this note)
//   note_records    the public record of this one note (its note ID is in the request)
//   pending_payout  the payout account (it is in the request, and it is the account that is paid)
import type { RpcClient } from './rpc';

const PAGE = 100;

/** `exit_requests` rows as the vault serializes them: U128 and U64 are decimal strings. */
export interface ExitRow {
  note_id: number;
  nullifier: string;
  next_nullifier: string;
  final_balance: string;
  conceded: string;
  /** Block time in nanoseconds. */
  deadline: string;
}

/** The pending exit of `noteId`, found by paging the whole list from 0. */
export async function exitRequest(
  rpc: RpcClient,
  vault: string,
  noteId: number,
): Promise<ExitRow | undefined> {
  for (let from = 0; ;) {
    const rows = await rpc.view<ExitRow[]>(vault, 'exit_requests', {
      from_note_id: from,
      limit: PAGE,
    });
    const found = rows.find((r) => r.note_id === noteId);
    if (found) return found;
    const last = rows.at(-1);
    if (rows.length < PAGE || !last) return undefined;
    from = last.note_id + 1;
  }
}

/** The exit's deadline in Unix seconds. */
export function deadlineSecs(row: ExitRow): number {
  if (!/^\d+$/.test(row.deadline)) throw new Error('the exit’s deadline is unreadable');
  return Number(BigInt(row.deadline) / 1_000_000_000n);
}

export async function pendingPayout(
  rpc: RpcClient,
  vault: string,
  account: string,
): Promise<bigint> {
  const amount = await rpc.view<string>(vault, 'pending_payout', { account_id: account });
  return BigInt(amount ?? '0');
}
