// Finds a deposit on chain without the wallet's help and without a transaction hash. Used while a
// wallet that never returns keeps the Fund screen waiting (src/wallet/watch.ts).
//
// The vault stores, for each note, a leaf made from the note's id, its commitment, amount and
// expiry. The commitment is not readable from a view, and the app does not call `note_status`
// because that names the commitment to the RPC provider. So the probe reads only public fields:
// notes of this denomination that were added since the send began, then it computes the leaf each
// of them would have if it were this note and compares it with the leaf the vault holds. A note of
// someone else cannot match: the leaf binds the commitment.
import { pad32 } from '../note/core';

export interface DepositProbeRpc {
  view<T>(account: string, method: string, args?: unknown): Promise<T>;
}

export interface FoundDeposit {
  noteId: number;
  denomination: string;
  expiry: number;
}

export type LeafOf = (
  noteId: number,
  commitment: string,
  amount: string,
  expiry: number,
) => Promise<string>;

const PAGE = 100;
const MAX_PAGES = 20;

interface Row {
  note_id: number;
  deposit_amount: string;
  expiry: number;
  status: string;
}

/** The note count the scan starts from: every deposit after this call has a higher note id. */
export async function scanStart(rpc: DepositProbeRpc, vault: string): Promise<number> {
  // Notes the tree does not hold yet are not in `leaf_count`; starting there still covers them.
  return rpc.view<number>(vault, 'leaf_count', {});
}

export async function findDeposit(
  rpc: DepositProbeRpc,
  vault: string,
  record: { commitment: string; denomination: string },
  from: number,
  leafOf: LeafOf,
): Promise<FoundDeposit | null> {
  const commitment = pad32(record.commitment);
  for (let page = 0, start = from; page < MAX_PAGES; page++, start += PAGE) {
    const rows = await rpc.view<Row[]>(vault, 'note_records', { from: start, limit: PAGE });
    for (const row of rows) {
      if (row.status !== 'Active' || row.deposit_amount !== record.denomination) continue;
      const [held] = await rpc.view<string[]>(vault, 'get_leaves', { from: row.note_id, limit: 1 });
      // The tree does not hold this note yet (`sync_root` has not run): look again later.
      if (held === undefined) continue;
      const mine = await leafOf(row.note_id, commitment, record.denomination, row.expiry);
      if (pad32(held) === mine) {
        return { noteId: row.note_id, denomination: record.denomination, expiry: row.expiry };
      }
    }
    if (rows.length < PAGE) break;
  }
  return null;
}
