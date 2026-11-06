// Helpers for the chat tests: a funded note on the scripted chain, the vault tree views the app
// reads, and drivers for the chat screen.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { expect, test as base, type BrowserContext, type Page } from '@playwright/test';
import {
  browser_note_leaf_from_fields,
  browser_tree_path_from_leaves,
  initSync,
} from '../src/wasm/zkapi-browser-dev/zkapi_browser.js';
import { MockStack } from './mock-operator';
import {
  FundingWallet,
  RPC,
  VAULT,
  backUpAndTest,
  newFundingWallet,
  openFund,
  prepare,
  readBackup,
  workdir,
  control,
} from './support';

let wasmReady = false;
function zk() {
  if (wasmReady) return;
  initSync({
    module: readFileSync(
      new URL('../src/wasm/zkapi-browser-dev/zkapi_browser_bg.wasm', import.meta.url),
    ),
  });
  wasmReady = true;
}

/** One mock operator and gateway per Playwright worker. */
export const test = base.extend<object, { stack: MockStack }>({
  stack: [
    // eslint-disable-next-line no-empty-pattern
    async ({}, use) => {
      const stack = new MockStack();
      await stack.start();
      await use(stack);
      await stack.stop();
    },
    { scope: 'worker' },
  ],
});
export { expect };

/** Scripts the vault's tree views for a single deposited note (what the app reads for a proof). */
export async function scriptTree(
  wallet: FundingWallet | { deposit: { commitment: string; note_id: number; expiry: number } },
  denomination = '1000000',
) {
  zk();
  // A deposit that did not come from the test wallet (the Intents path) is passed as `deposit`.
  const d =
    'deposit' in wallet
      ? wallet.deposit
      : (wallet.lastDeposit as { note_id: number; expiry: number });
  const commitment =
    'deposit' in wallet
      ? wallet.deposit.commitment
      : (JSON.parse(wallet.calls.at(-1)!.args.msg as string) as { commitment: string }).commitment;
  const leaf = JSON.parse(
    browser_note_leaf_from_fields(d.note_id, commitment, denomination, String(d.expiry)),
  ) as string;
  const path = JSON.parse(browser_tree_path_from_leaves(JSON.stringify([leaf]), 0)) as {
    active_root: string;
  };
  const set = (method: string, result: unknown) =>
    control({
      cmd: 'set_view',
      account: VAULT,
      method,
      args: 'any',
      result: { json: result },
    });
  await set('current_root', path.active_root);
  await set('leaf_count', 1);
  await set('get_leaves', [leaf]);
  return { leaf, root: path.active_root };
}

export interface VaultRow {
  status: 'Active' | 'Closed' | 'PendingExit';
  /** Unix seconds; the default is a month ahead. */
  expiry?: number;
}

/**
 * Scripts the vault's tree and `note_records` as one new state from the next block on: row `i` is
 * note `i`, and the note at `own` is the one the app holds. Earlier states stay readable at their
 * own block, so a lease keeps the figure it was made with while "now" reads the new state.
 */
export async function scriptVault(
  wallet: FundingWallet,
  rows: VaultRow[],
  own = 0,
  denomination = '1000000',
) {
  zk();
  const { leaf: ownLeaf } = await scriptTree(wallet, denomination);
  const far = Math.floor(Date.now() / 1000) + 30 * 86_400;
  const leaves = rows.map((_, id) =>
    id === own
      ? ownLeaf
      : (JSON.parse(
          browser_note_leaf_from_fields(
            id,
            '0x' + (id + 1).toString(16).padStart(64, '0'),
            denomination,
            String(far),
          ),
        ) as string),
  );
  const path = JSON.parse(browser_tree_path_from_leaves(JSON.stringify(leaves), own)) as {
    active_root: string;
  };
  await control({ cmd: 'advance_blocks', n: 1, seconds_per_block: 1 });
  const set = (method: string, args: unknown, result: unknown) =>
    control({ cmd: 'set_view', account: VAULT, method, args, result: { json: result } });
  await set('current_root', 'any', path.active_root);
  await set('leaf_count', 'any', rows.length);
  await set('get_leaves', 'any', leaves);
  const records = rows.map((r, note_id) => ({
    note_id,
    deposit_amount: denomination,
    expiry: r.expiry ?? far,
    status: r.status,
    leaf_retired: r.status === 'Closed',
  }));
  await set('note_records', { json: { from: 0, limit: rows.length } }, records);
  return { root: path.active_root };
}

/** Funds one 1 USDC note through the UI and scripts its tree. Leaves the page on the Fund screen. */
export async function fundNote(page: Page, context: BrowserContext) {
  const wallet = await newFundingWallet(context);
  await openFund(page);
  await prepare(page);
  const file = await backUpAndTest(page, join(workdir(), 'fund.nymira-backup'));
  await page.getByTestId('send').click();
  await expect(page.getByTestId('accepted')).toBeVisible();
  const tree = await scriptTree(wallet);
  return { wallet, tree, backupFile: file };
}

export async function openChat(page: Page) {
  await page.getByTestId('nav-chat').click();
  await expect(page.getByTestId('chat-screen')).toBeVisible();
  await expect(page.getByTestId('prover-status')).toContainText('Proving key loaded and checked');
}

export async function startConversation(page: Page) {
  await page.getByTestId('chat-use-note').click();
  await expect(page.getByTestId('chat-input')).toBeVisible();
}

export async function say(page: Page, text: string) {
  const before = await page.getByTestId('conversation').locator('li').count();
  await page.getByTestId('chat-input').fill(text);
  await page.getByTestId('chat-send').click();
  await expect(page.getByTestId('conversation').locator('li')).toHaveCount(before + 2, {
    timeout: 180_000,
  });
}

export async function endConversation(page: Page) {
  await page.getByTestId('chat-end').click();
  await expect(page.getByTestId('settled')).toBeVisible({ timeout: 120_000 });
}

export { RPC, readBackup };
