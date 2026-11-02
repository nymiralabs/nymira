// The vault's note tree for request proofs: `get_leaves` from 0 (at most 100 per call), the root
// and the leaf count, all read at one final block from public RPC with fallback. The wallet core
// rebuilds the tree from the leaves and refuses it unless it reproduces `current_root` (error
// `tree_root_mismatch`), so a lying endpoint cannot hand the proof a tree the vault does not hold.
import type { TreeSnapshot } from '../note/lease';
import type { RpcClient } from './rpc';

const PAGE = 100;

export async function readSnapshot(rpc: RpcClient, vault: string): Promise<TreeSnapshot> {
  const block = await rpc.finalBlockHash();
  const [root, count] = await Promise.all([
    rpc.view<string>(vault, 'current_root', {}, block),
    rpc.view<number>(vault, 'leaf_count', {}, block),
  ]);
  const leaves: string[] = [];
  for (let from = 0; from < count; from += PAGE) {
    leaves.push(...(await rpc.view<string[]>(vault, 'get_leaves', { from, limit: PAGE }, block)));
  }
  if (leaves.length !== count) throw new Error('the vault returned fewer leaves than it counts');
  return { root, nextNoteId: count, leaves, block };
}

export interface PublicNoteRecord {
  note_id: number;
  deposit_amount: string;
  expiry: number;
  status: string;
}

/** The note's stored deposit, read from `note_records` (public fields only). */
export async function readNoteRecord(
  rpc: RpcClient,
  vault: string,
  noteId: number,
): Promise<PublicNoteRecord | undefined> {
  const rows = await rpc.view<PublicNoteRecord[]>(vault, 'note_records', {
    from: noteId,
    limit: 1,
  });
  return rows.find((r) => r.note_id === noteId);
}
