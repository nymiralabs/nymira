import { readFileSync } from 'node:fs';
import { beforeAll, describe, expect, it } from 'vitest';
import {
  browser_generate_deposit,
  browser_note_leaf_from_fields,
  initSync,
} from '../src/wasm/zkapi-browser/zkapi_browser.js';
import { pad32 } from '../src/note/core';
import { findDeposit, type DepositProbeRpc } from '../src/funding/watch';

beforeAll(() => {
  initSync({
    module: readFileSync(
      new URL('../src/wasm/zkapi-browser/zkapi_browser_bg.wasm', import.meta.url),
    ),
  });
});

const leafOf = async (noteId: number, commitment: string, amount: string, expiry: number) =>
  pad32(JSON.parse(browser_note_leaf_from_fields(noteId, commitment, amount, String(expiry))));

interface Row {
  note_id: number;
  deposit_amount: string;
  expiry: number;
  status: string;
}

// A vault with the given notes; `synced` is how many leaves its tree holds.
function vault(rows: Row[], leaves: string[]): DepositProbeRpc & { calls: string[] } {
  const calls: string[] = [];
  return {
    calls,
    async view<T>(_account: string, method: string, args: unknown) {
      calls.push(method);
      const { from, limit } = args as { from: number; limit: number };
      if (method === 'note_records') return rows.slice(from, from + limit) as T;
      if (method === 'get_leaves') return leaves.slice(from, from + limit) as T;
      throw new Error(`unexpected view ${method}`);
    },
  };
}

describe('findDeposit', () => {
  let mine = '';
  let other = '';
  let record = { commitment: '', denomination: '1000000' };
  beforeAll(() => {
    mine = JSON.parse(browser_generate_deposit()).registration_commitment as string;
    other = JSON.parse(browser_generate_deposit()).registration_commitment as string;
    record = { commitment: pad32(mine), denomination: '1000000' };
  });
  const row = (id: number, over: Partial<Row> = {}): Row => ({
    note_id: id,
    deposit_amount: '1000000',
    expiry: 1_900_000_000 + id,
    status: 'Active',
    ...over,
  });

  it('finds the note whose leaf is made from this commitment, among others of the same size', async () => {
    const rows = [row(0), row(1), row(2)];
    const leaves = [
      await leafOf(0, pad32(other), '1000000', rows[0]!.expiry),
      await leafOf(1, pad32(mine), '1000000', rows[1]!.expiry),
      await leafOf(2, pad32(other), '1000000', rows[2]!.expiry),
    ];
    const found = await findDeposit(vault(rows, leaves), 'vault.test', record, 0, leafOf);
    expect(found).toEqual({ noteId: 1, denomination: '1000000', expiry: rows[1]!.expiry });
  });

  it('shows nothing for a note whose leaf the tree does not hold yet', async () => {
    expect(await findDeposit(vault([row(0)], []), 'vault.test', record, 0, leafOf)).toBeNull();
  });

  it('shows nothing when no leaf matches, and skips other sizes and closed notes', async () => {
    const rows = [row(0, { deposit_amount: '5000000' }), row(1, { status: 'Closed' }), row(2)];
    const leaves = await Promise.all(
      rows.map((r) => leafOf(r.note_id, pad32(other), r.deposit_amount, r.expiry)),
    );
    const rpc = vault(rows, leaves);
    expect(await findDeposit(rpc, 'vault.test', record, 0, leafOf)).toBeNull();
    // One leaf read: for the one candidate of the right size that is still open.
    expect(rpc.calls.filter((m) => m === 'get_leaves')).toHaveLength(1);
  });

  it('starts at the given note id', async () => {
    const rows = [row(0), row(1)];
    const leaves = [
      await leafOf(0, pad32(mine), '1000000', rows[0]!.expiry),
      await leafOf(1, pad32(other), '1000000', rows[1]!.expiry),
    ];
    expect(await findDeposit(vault(rows, leaves), 'vault.test', record, 1, leafOf)).toBeNull();
  });
});
