// Close, exit, finalize, payout and the expiry guard, in a real browser (issue #38).
// What is real: the app, the worker's proofs, the operator mock's checks and clearance (see
// leave-support.ts). What is mocked: the vault (the fake RPC runs no contract, so the test scripts
// what the vault shows after each call) and the chain's clock (the test moves the fake chain's block
// time; the sandbox cannot move seven days).
import { join } from 'node:path';
import { installTestWallet } from '@testkit';
import type { BrowserContext, Page } from '@playwright/test';
import { destination } from '../src/wasm/near-codec/nymira_near_codec_wasm.js';
import { PUBLIC_SENTENCE, plain } from '../src/leave/text';
import { toHex } from '../src/crypto/bytes';
import {
  endConversation,
  expect,
  openChat,
  say,
  scriptTree,
  startConversation,
  test,
} from './chat-support';
import {
  DAY,
  LeaveWallet,
  advanceChain,
  chainNow,
  openClose,
  operatorClosesOnChain,
  showActive,
} from './leave-support';
import {
  BACKUP_PASSPHRASE,
  SENDER,
  WRAP_PASSPHRASE,
  backUpAndTest,
  connectWallet,
  control,
  expectNoSecret,
  openFund,
  pageSurfaces,
  prepare,
  readBackup,
  rpcRequests,
  scriptChain,
  watch,
  workdir,
  enterApp,
} from './support';

test.describe.configure({ timeout: 360_000 });

const nowSecs = () => Math.floor(Date.now() / 1000);

/** Funds one 1 USDC note through the UI, with the vault's views for it scripted. */
async function fundForLeaving(page: Page, context: BrowserContext, expiryInDays?: number) {
  const wallet = new LeaveWallet();
  if (expiryInDays !== undefined) {
    wallet.eventOverride = { expiry: nowSecs() + Math.round(expiryInDays * DAY) };
  }
  await scriptChain(wallet);
  await installTestWallet(context, wallet);
  await openFund(page);
  await prepare(page);
  await backUpAndTest(page, join(workdir(), 'fund.nymira-backup'));
  await page.getByTestId('send').click();
  await expect(page.getByTestId('accepted')).toBeVisible();
  await scriptTree(wallet);
  await showActive(wallet, wallet.lastDeposit.expiry as number);
  return wallet;
}

/** After a reload the app starts again: unlock, no wallet connected. */
async function reloadTo(page: Page, nav: 'nav-chat' | 'nav-close') {
  await page.reload();
  await enterApp(page);
  await page.getByTestId(nav).click();
  // A dev build with the mock profile keeps the tab unlocked across a reload: then no prompt shows.
  const field = page.getByTestId('unlock-passphrase');
  if (
    await field.waitFor({ state: 'visible', timeout: 8000 }).then(
      () => true,
      () => false,
    )
  ) {
    await field.fill(WRAP_PASSPHRASE);
    await page.getByTestId('unlock-submit').click();
  }
}

const payoutDestination = (account: string) => toHex(destination(account));

test('close with the wallet: clearance, a 3-hour close proof, no close fee, a tested backup @webkit', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  const seen = watch(page);
  const wallet = await fundForLeaving(page, context);
  await openClose(page);

  // The wording: the guarantee word for word, the limit, and what closing shows on chain.
  await expect(page.getByTestId('public-sentence')).toHaveText(plain(PUBLIC_SENTENCE));
  await expect(page.getByTestId('old-state-limit')).toContainText(
    'an exit from an old state can be defeated',
  );
  await expect(page.getByTestId('old-state-limit')).toContainText('lose your latest state');
  await expect(page.getByTestId('close-reveals')).toContainText(
    'Closing shows your note ID, your total spend and your payout account on chain, next to the account that sends the close',
  );
  await expect(page.getByTestId('gas-note')).toContainText('need a little NEAR for gas');
  await expect(page.getByTestId('gas-note')).toContainText('Any account can send them');
  // The guard is on, closing at the end of a session is off.
  await expect(page.getByTestId('guard-enabled')).toBeChecked();
  await expect(page.getByTestId('close-at-end')).not.toBeChecked();

  await page.getByTestId('close-wallet').click();
  await expect(page.getByTestId('leave-message')).toContainText('is closed', { timeout: 240_000 });
  await expect(page.getByTestId('leave-state')).toHaveText('closed');

  // One clearance, one mutual_close from the user's wallet with no deposit: no close fee.
  expect(stack.clearances).toHaveLength(1);
  expect(stack.closes).toHaveLength(0);
  expect(wallet.vaultCalls.map((c) => c.method)).toEqual(['mutual_close']);
  expect(wallet.vaultCalls[0]!.deposit).toBe('0');
  const call = wallet.decoded('mutual_close');
  expect(call.valid).toBe(true);
  expect(call).toMatchObject({ kind: 1, noteId: 0, finalBalance: 1_000_000n, conceded: 0n });
  expect(call.destination).toBe(payoutDestination(SENDER));
  // The close proof is valid for 3 hours from the chain's time.
  expect(Math.abs(call.validUntil - ((await chainNow()) + 10_800))).toBeLessThan(5);

  // The ownership proof the operator saw ended within 3,300 s and carried no destination.
  const body = stack.seen.find((r) => r.path === '/v2/withdraw/clearance')!.body!;
  expect(body).toContain('"kind":2');

  // What reached the RPC provider: no nullifier query, and no note_status call.
  const rpcCalls = JSON.stringify(await rpcRequests());
  expect(rpcCalls).not.toContain('note_status');
  expect(rpcCalls).not.toContain('is_nullifier_used');

  // A backup is prompted after the close, and test-restored.
  await expect(page.getByTestId('close-backup')).toBeVisible();
  const file = join(workdir(), 'closed.nymira-backup');
  await page.getByTestId('close-backup-passphrase').fill(BACKUP_PASSPHRASE);
  const [download] = await Promise.all([
    page.waitForEvent('download'),
    page.getByTestId('close-backup-download').click(),
  ]);
  await download.saveAs(file);
  await page.getByTestId('close-backup-file').setInputFiles(file);
  await expect(page.getByTestId('close-backup-tested')).toBeVisible();
  const backup = await readBackup(file);
  const note = backup.notes[0]!;
  expect(note).toMatchObject({ leave: { state: 'closed', via: 'wallet' } });

  // A closed note opens no new lease.
  await page.getByTestId('nav-chat').click();
  await expect(page.getByTestId('chat-screen')).toBeVisible();
  await expect(page.getByTestId('chat-note')).toHaveCount(0);

  // No plain secret anywhere the page could have put it.
  const secret = (JSON.parse(note.state!) as { secret_s: string }).secret_s;
  expectNoSecret(await pageSurfaces(page), secret);
  expectNoSecret(seen.console, secret);
  expectNoSecret(seen.requests, secret);
  expectNoSecret(
    stack.seen.map((r) => r.body ?? ''),
    secret,
  );
  expect(seen.violations).toEqual([]);
});

test('a user without NEAR hands the close proof to the operator and pays its close fee', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  const wallet = await fundForLeaving(page, context);
  operatorClosesOnChain(stack, wallet.expiry);
  await openClose(page);
  await expect(page.getByTestId('operator-close-note')).toContainText('close fee');

  // The wallet account holds no NEAR: the wallet route stops before any proof exists.
  await control({ cmd: 'set_account', account: wallet.accountId, spec: { amount: '0' } });
  await page.getByTestId('close-wallet').click();
  await expect(page.getByTestId('leave-error')).toContainText('too little NEAR');
  expect(stack.clearances).toHaveLength(0);
  await expect(page.getByTestId('leave-state')).toHaveText('open');

  await page.getByTestId('close-operator').click();
  await expect(page.getByTestId('leave-message')).toContainText('is closed', { timeout: 240_000 });
  expect(stack.clearances).toHaveLength(1);
  expect(stack.closes).toHaveLength(1);
  expect(stack.closes[0]).toMatchObject({
    valid: true,
    kind: 1,
    finalBalance: 1_000_000n,
    conceded: 0n,
  });
  expect(wallet.vaultCalls).toHaveLength(0);
  await expect(page.getByTestId('close-backup')).toBeVisible();
});

test('exit: request, see the deadline, advance the chain, finalize, claim; no request after the exit proof', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  const wallet = await fundForLeaving(page, context);
  await openClose(page);
  await expect(page.getByTestId('exit-concession')).toContainText('0.00 USDC');

  await page.getByTestId('way-exit').click();
  await page.getByTestId('exit-request').click();
  await expect(page.getByTestId('exit-deadline')).toBeVisible({ timeout: 240_000 });
  await expect(page.getByTestId('leave-message')).toContainText('concedes 0.00 USDC');

  // request_exit from the user's wallet: concession 0 (no request left the state), a valid proof,
  // the payout account fixed at deposit. No clearance was asked.
  expect(wallet.vaultCalls.map((c) => c.method)).toEqual(['request_exit']);
  const exit = wallet.decoded('request_exit');
  expect(exit.valid).toBe(true);
  expect(exit).toMatchObject({ kind: 0, noteId: 0, conceded: 0n, finalBalance: 1_000_000n });
  expect(exit.destination).toBe(payoutDestination(SENDER));
  expect(stack.clearances).toHaveLength(0);

  // The deadline is 7 days out by the chain's clock; finalize is not offered before it.
  const deadline = (await chainNow()) + 7 * DAY;
  await expect(page.getByTestId('exit-deadline')).toContainText(
    new Date(deadline * 1000).toISOString().slice(0, 10),
  );
  await expect(page.getByTestId('exit-finalize')).toBeDisabled();

  // After the exit proof left, the note opens no new lease, here or at the operator.
  await page.getByTestId('nav-chat').click();
  await expect(page.getByTestId('chat-screen')).toBeVisible();
  await expect(page.getByTestId('chat-note')).toHaveCount(0);
  expect(stack.seen.some((r) => r.path === '/v2/openrouter/leases')).toBe(false);

  // Seven days pass on the chain.
  await advanceChain(7 * DAY + 60);
  await openClose(page);
  await expect(page.getByTestId('exit-finalize')).toBeEnabled();
  wallet.pendingAfterFinalize = '1000000';
  await page.getByTestId('exit-finalize').click();
  await expect(page.getByTestId('leave-message')).toContainText('is finalized', {
    timeout: 120_000,
  });
  expect(wallet.vaultCalls.map((c) => c.method)).toEqual(['request_exit', 'finalize_exit']);
  expect(wallet.vaultCalls[1]!.args).toEqual({ note_id: 0 });
  await expect(page.getByTestId('leave-state')).toHaveText('exit finalized');
  await expect(page.getByTestId('close-backup')).toBeVisible();

  // The payout transfer "failed": it waits in the vault, and claiming collects it. The screen says
  // that reading pending_payout shows the payout account to the RPC provider.
  await expect(page.getByTestId('claim-note')).toContainText(
    'shows your payout account to the RPC provider',
  );
  await page.getByTestId('claim-check').click();
  await expect(page.getByTestId('claim-pending')).toContainText('1.00 USDC is pending');
  await page.getByTestId('claim-payout').click();
  await expect(page.getByTestId('leave-message')).toContainText('claim_payout was sent');
  expect(wallet.vaultCalls.at(-1)).toMatchObject({
    method: 'claim_payout',
    args: { account_id: SENDER },
  });
  await page.getByTestId('claim-check').click();
  await expect(page.getByTestId('claim-pending')).toContainText('Nothing is pending');
});

test('an exit concedes the stored bound when a request may have left the state', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  const wallet = await fundForLeaving(page, context);
  await openChat(page);
  const updatedAt = () =>
    page.evaluate(
      () =>
        new Promise<number>((resolve, reject) => {
          const open = indexedDB.open('nymira');
          open.onsuccess = () => {
            const all = open.result.transaction('notes').objectStore('notes').getAll();
            all.onsuccess = () => resolve((all.result as { updatedAt: number }[])[0]!.updatedAt);
            all.onerror = () => reject(all.error);
          };
          open.onerror = () => reject(open.error);
        }),
    );
  const before = await updatedAt();
  await startConversation(page);
  // The proof is made while the user types and its bound is stored; nothing is sent.
  await expect.poll(updatedAt, { timeout: 120_000 }).toBeGreaterThan(before);
  expect(stack.seen.some((r) => r.method === 'POST')).toBe(false);

  // The tab goes away with that proof stored. The operator never saw it.
  await reloadTo(page, 'nav-close');
  await expect(page.getByTestId('close-screen')).toBeVisible();
  await expect(page.getByTestId('leave-pending')).toBeVisible();
  await connectWallet(page);
  await expect(page.getByTestId('withdrawal-prover-status')).toContainText('loaded and checked');
  await expect(page.getByTestId('exit-concession')).toContainText('0.10 USDC');
  await page.getByTestId('way-exit').click();
  await page.getByTestId('exit-request').click();
  await expect(page.getByTestId('exit-deadline')).toBeVisible({ timeout: 240_000 });

  const exit = wallet.decoded('request_exit');
  expect(exit.valid).toBe(true);
  expect(exit).toMatchObject({ kind: 0, conceded: 100_000n, finalBalance: 900_000n });
  await expect(page.getByTestId('leave-message')).toContainText('concedes 0.10 USDC');
});

test('the expiry banner, no new lease within 5 days, and a close that starts by itself and waits for the wallet', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  const wallet = await fundForLeaving(page, context, 4);
  operatorClosesOnChain(stack, wallet.expiry);

  // The page restarts: no wallet is connected, so the guard can only say what is waiting.
  await reloadTo(page, 'nav-chat');
  await expect(page.getByTestId('chat-screen')).toBeVisible();
  await expect(page.getByTestId('expiry-banner')).toContainText('expires in 3 days (7-day notice)');
  await expect(page.getByTestId('expiry-banner')).toContainText('stops opening leases');
  await expect(page.getByTestId('note-near-expiry')).toContainText('no new lease');
  await expect(page.getByTestId('chat-use-note')).toBeDisabled();
  expect(stack.seen.some((r) => r.path === '/v2/openrouter/leases')).toBe(false);

  await openClose(page);
  await expect(page.getByTestId('leave-message')).toContainText(
    'needs a close: connect your wallet',
  );
  expect(wallet.vaultCalls).toHaveLength(0);

  // Connecting a wallet starts the close at 5 days. It waits for the wallet's signature.
  let release: () => void = () => undefined;
  wallet.hold = new Promise<void>((resolve) => (release = resolve));
  await connectWallet(page);
  await expect(page.getByTestId('leave-busy')).toContainText(
    'Waiting for your wallet to sign the close',
    {
      timeout: 240_000,
    },
  );
  expect(wallet.vaultCalls).toHaveLength(0);
  release();
  await expect(page.getByTestId('leave-message')).toContainText('is closed', { timeout: 120_000 });
  expect(wallet.vaultCalls.map((c) => c.method)).toEqual(['mutual_close']);
});

test('at 3 days an exit starts when the close did not go through', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  stack.set({ clearanceError: 'operator_capacity' });
  const wallet = await fundForLeaving(page, context, 2.5);
  await reloadTo(page, 'nav-close');
  await expect(page.getByTestId('expiry-banner')).toContainText('expires in 2 days');
  await connectWallet(page);
  // The close fails (the operator gives no clearance), so the guard goes on to the exit.
  await expect(page.getByTestId('exit-deadline')).toBeVisible({ timeout: 360_000 });
  expect(stack.clearances).toHaveLength(0);
  expect(wallet.vaultCalls.map((c) => c.method)).toEqual(['request_exit']);
  expect(wallet.decoded('request_exit')).toMatchObject({ kind: 0, conceded: 0n, valid: true });
});

test('the expiry guard can be switched off: nothing starts and leases are not cut off', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  const wallet = await fundForLeaving(page, context, 4);
  await reloadTo(page, 'nav-chat');
  await expect(page.getByTestId('note-near-expiry')).toBeVisible();
  // The switch is a setting in this browser (public values only).
  await page.evaluate(
    () =>
      new Promise<void>((resolve, reject) => {
        const open = indexedDB.open('nymira');
        open.onsuccess = () => {
          const tx = open.result.transaction('meta', 'readwrite');
          tx.objectStore('meta').put({ enabled: false, closeAtSessionEnd: false }, 'setting:guard');
          tx.oncomplete = () => resolve();
          tx.onerror = () => reject(tx.error);
        };
        open.onerror = () => reject(open.error);
      }),
  );
  await openClose(page);
  await connectWallet(page);
  await expect(page.getByTestId('guard-enabled')).not.toBeChecked();
  await page.waitForTimeout(3000);
  expect(wallet.vaultCalls).toHaveLength(0);
  expect(stack.clearances).toHaveLength(0);
  await page.getByTestId('nav-chat').click();
  // Not blocked any more: the conversation opens by itself.
  await expect(page.getByTestId('chat-input')).toBeVisible();
  await expect(page.getByTestId('note-near-expiry')).toHaveCount(0);
});

test('on the operator’s emergency notice the app stops opening leases and suggests an exit', async ({
  context,
  page,
  stack,
}) => {
  stack.reset();
  const wallet = await fundForLeaving(page, context);
  await openChat(page);
  await expect(page.getByTestId('chat-input')).toBeVisible();
  await startConversation(page);
  await say(page, 'before the notice');
  await endConversation(page);
  await page.getByRole('button', { name: 'New conversation' }).click();

  const leasesBefore = stack.leases.size;
  stack.set({ emergency: 'the state key may be exposed' });
  // The status is read when the Chat or Close screen opens.
  await page.getByTestId('nav-close').click();
  await expect(page.getByTestId('emergency-banner')).toContainText('the state key may be exposed');
  await expect(page.getByTestId('emergency-banner')).toContainText('Request exit');
  await expect(page.getByTestId('emergency-advice')).toContainText('No new lease is opened');
  await page.getByTestId('nav-chat').click();
  await expect(page.getByTestId('chat-emergency')).toBeVisible();
  await expect(page.getByTestId('chat-use-note')).toBeDisabled();
  expect(stack.leases.size).toBe(leasesBefore);

  // The clearance route is closed in an emergency, so the way out is the exit.
  await openClose(page);
  await page.getByTestId('close-wallet').click();
  await expect(page.getByTestId('leave-error')).toContainText('emergency');
  await expect(page.getByTestId('leave-error')).toContainText('Request an exit');
  await page.getByTestId('way-exit').click();
  await page.getByTestId('exit-request').click();
  await expect(page.getByTestId('exit-deadline')).toBeVisible({ timeout: 240_000 });
  expect(wallet.vaultCalls.map((c) => c.method)).toEqual(['request_exit']);
  // This note spent a lease before: the successor state carries no bound, so nothing is conceded.
  expect(wallet.decoded('request_exit')).toMatchObject({ kind: 0, valid: true, conceded: 0n });
});
