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
export async function scriptTree(wallet: FundingWallet, denomination = '1000000') {
  zk();
  const call = wallet.calls.at(-1)!;
  const commitment = (JSON.parse(call.args.msg as string) as { commitment: string }).commitment;
  const d = wallet.lastDeposit as { note_id: number; expiry: number };
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
