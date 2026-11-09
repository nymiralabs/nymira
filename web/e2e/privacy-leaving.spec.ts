// The privacy panel's link sentence, reuse check and leaving lists (issue #53), against the mock
// stack. The vault is the fake RPC with scripted views (see chat-support.ts and leave-support.ts).
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { installTestWallet } from '@testkit';
import type { BrowserContext, Page } from '@playwright/test';
import {
  endConversation,
  expect,
  fundNote,
  openChat,
  say,
  scriptTree,
  scriptVault,
  startConversation,
  test,
} from './chat-support';
import { LeaveWallet, openClose, showActive } from './leave-support';
import { backUpAndTest, openFund, prepare, rpcRequests, scriptChain, workdir } from './support';

test.describe.configure({ timeout: 420_000 });

const shared = JSON.parse(
  readFileSync(new URL('../../shared/privacy-strings.json', import.meta.url), 'utf8'),
) as Record<string, string>;

const MARKUP =
  '<img src=x onerror="window.__pwned=1"><a href="https://evil.test/">open me</a> <b>bold</b>';

const leasePosts = (stack: { seen: { method: string; path: string }[] }) =>
  stack.seen.filter((r) => r.method === 'POST' && r.path === '/v2/openrouter/leases').length;

async function lease(page: Page, text: string) {
  await startConversation(page);
  await say(page, text);
  await endConversation(page);
}

/** The panel from the notes screen, which works whatever state the note is in. */
async function openFromNotes(page: Page) {
  await page.getByTestId('nav-notes').click();
  await page.getByTestId('notes-privacy').click();
  await expect(page.getByTestId('privacy-screen')).toBeVisible();
}

async function fundForLeaving(page: Page, context: BrowserContext, name: string) {
  const wallet = new LeaveWallet();
  await scriptChain(wallet);
  await installTestWallet(context, wallet);
  await openFund(page);
  await prepare(page);
  await backUpAndTest(page, join(workdir(), `${name}.nymira-backup`));
  await page.getByTestId('send').click();
  await expect(page.getByTestId('accepted')).toBeVisible();
  await scriptTree(wallet);
  await showActive(wallet, wallet.lastDeposit.expiry as number);
  return wallet;
}

async function expectOpenSentence(page: Page) {
  const sentence = page.getByTestId('link-sentence');
  await expect(sentence).toHaveText(shared.link_open!);
  await expect(sentence).toHaveAttribute('data-left', 'false');
}

async function expectLeftSentence(page: Page) {
  const sentence = page.getByTestId('link-sentence');
  await expect(sentence).toHaveText(shared.link_left!);
  await expect(sentence).toHaveAttribute('data-left', 'true');
  // The no-link sentence is gone, not shown beside it.
  await expect(page.getByTestId('privacy-screen')).not.toContainText('designed to leave no');
}

test('reuse check: finalized as spent, a resent request refused with the reason as given, nothing resent after 300 seconds', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  const { wallet } = await fundNote(page, context);
  await scriptVault(wallet, [{ status: 'Active' }, { status: 'Active' }, { status: 'Active' }]);
  await openChat(page);
  await lease(page, 'one lease');
  const nullifier = [...stack.leases.values()][0]!.nullifier;

  await page.getByTestId('chat-privacy').click();
  await expect(page.getByTestId('privacy-screen')).toBeVisible();
  await expectOpenSentence(page);
  await expect(page.getByTestId('reuse-rule')).toHaveText(shared.reuse_rule!);

  stack.set({ refusal: MARKUP });
  const posts = leasePosts(stack);
  await page.getByTestId('reuse-check').click();
  // The evidence: what the operator's route reports, shown as spent.
  await expect(page.getByTestId('reuse-status')).toHaveText(shared.reuse_finalized!);
  expect(
    stack.seen.some((r) => r.method === 'GET' && r.path === `/v2/nullifiers/${nullifier}`),
  ).toBe(true);
  // The stored request is inside its 300 seconds, so it went again and was refused.
  await expect(page.getByTestId('reuse-refused')).toContainText('the operator refuses');
  await expect(page.getByTestId('reuse-refused')).toContainText('nullifier_seen');
  expect(leasePosts(stack)).toBe(posts + 1);
  // The reason is in the labelled box, as given, as plain text.
  const says = page.getByTestId('operator-says');
  await expect(says).toContainText(shared.reuse_says!);
  await expect(page.getByTestId('operator-says-text')).toHaveText(MARKUP);
  await expect(says.locator('a, img, b')).toHaveCount(0);
  expect(await page.evaluate(() => (window as unknown as { __pwned?: number }).__pwned)).toBe(
    undefined,
  );
  await expect(page.getByTestId('reuse')).toContainText(shared.reuse_read_reason!);

  // Neither the nullifier nor the vault's nullifier view went to a public node.
  const rpc = JSON.stringify(await rpcRequests());
  expect(rpc).not.toContain(nullifier);
  expect(rpc).not.toContain('is_nullifier_used');

  // After 300 seconds the proof is too old to say anything: only the status is shown.
  await page.clock.setFixedTime(Date.now() + 301_000);
  await page.getByTestId('reuse-check').click();
  await expect(page.getByTestId('reuse-not-sent')).toHaveText(shared.reuse_too_old!);
  await expect(page.getByTestId('reuse-status')).toHaveText(shared.reuse_finalized!);
  await expect(page.getByTestId('operator-says')).toHaveCount(0);
  expect(leasePosts(stack)).toBe(posts + 1);
});

test('leaving and observer lists, a long refusal is cut, no Intents line for a note funded from an account', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  const { wallet } = await fundNote(page, context);
  await scriptVault(wallet, [{ status: 'Active' }]);
  await openChat(page);
  await lease(page, 'cut');
  await page.getByTestId('chat-privacy').click();
  await expect(page.getByTestId('privacy-screen')).toBeVisible();

  const leaving = page.getByTestId('leaving');
  for (const key of [
    'leave_close',
    'leave_clearance',
    'leave_exit',
    'leave_answer',
    'leave_sender',
  ]) {
    await expect(leaving).toContainText(shared[key]!);
  }
  await expect(page.getByTestId('exit-purpose')).toHaveText(shared.leave_exit_purpose!);

  const observers = page.getByTestId('observers');
  await expect(page.getByTestId('observer')).toHaveCount(9);
  for (const key of [
    'who_chain',
    'who_operator',
    'who_entry',
    'who_second',
    'who_model',
    'who_near_ai',
    'who_rpc',
    'who_host',
    'who_attest',
    'who_certs',
  ]) {
    await expect(observers).toContainText(shared[key]!);
  }
  // This note was funded from the user's own account, so no 1Click line.
  await expect(observers).not.toContainText('1Click');
  // The landing drawing says the same words.
  await page.getByTestId('privacy-back').click();
  await page.getByRole('button', { name: 'Nymira', exact: true }).click();
  const desc = await page.getByTestId('who-desc').innerHTML();
  for (const key of ['who_operator', 'who_model', 'who_chain']) {
    expect(desc.replaceAll('&amp;', '&').replaceAll('&#39;', "'")).toContain(
      shared[key]!.replaceAll('&', '&'),
    );
  }

  // A long refusal is cut to 300 characters.
  stack.set({ refusal: 'x'.repeat(2000) });
  // The landing has no screen nav: the app opens from its button.
  await page.getByTestId('open-app').click();
  await openChat(page);
  await page.getByTestId('chat-privacy').click();
  await page.getByTestId('reuse-check').click();
  const text = await page.getByTestId('operator-says-text').innerText();
  expect(text.length).toBeLessThanOrEqual(301);
  expect(text.endsWith('…')).toBe(true);
});

test('the sentence changes after a close', async ({ context, page, stack }) => {
  stack.reset();
  await fundForLeaving(page, context, 'leaving-close');
  await openFromNotes(page);
  await expectOpenSentence(page);
  await page.getByTestId('privacy-back').click();

  await openClose(page);
  await page.getByTestId('close-wallet').click();
  await expect(page.getByTestId('leave-state')).toHaveText('closed', { timeout: 240_000 });

  await openFromNotes(page);
  await expectLeftSentence(page);
});

test('the sentence changes after a clearance', async ({ context, page, stack }) => {
  stack.reset();
  const wallet = await fundForLeaving(page, context, 'leaving-clearance');
  await openFromNotes(page);
  await expectOpenSentence(page);
  await page.getByTestId('privacy-back').click();

  // The operator signs the clearance; the user then says no to the close in the wallet.
  wallet.closeMode = 'cancel';
  await openClose(page);
  await page.getByTestId('close-wallet').click();
  await expect(page.getByTestId('leave-state')).toHaveText('close started, not confirmed', {
    timeout: 240_000,
  });
  expect(stack.clearances).toHaveLength(1);
  expect(wallet.vaultCalls).toHaveLength(0);

  await openFromNotes(page);
  await expectLeftSentence(page);
});

test('the sentence changes after an exit, and the reuse check then resends nothing', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  await fundForLeaving(page, context, 'leaving-exit');
  await openChat(page);
  await lease(page, 'before the exit');
  await openFromNotes(page);
  await expectOpenSentence(page);
  await page.getByTestId('privacy-back').click();

  await openClose(page);
  await page.getByTestId('exit-request').click();
  await expect(page.getByTestId('exit-deadline')).toBeVisible({ timeout: 240_000 });

  await openFromNotes(page);
  await expectLeftSentence(page);
  const posts = leasePosts(stack);
  await page.getByTestId('reuse-check').click();
  await expect(page.getByTestId('reuse-status')).toHaveText(shared.reuse_finalized!);
  await expect(page.getByTestId('reuse-not-sent')).toHaveText(shared.reuse_left!);
  await expect(page.getByTestId('operator-says')).toHaveCount(0);
  expect(leasePosts(stack)).toBe(posts);
});
